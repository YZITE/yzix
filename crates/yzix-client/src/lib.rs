#![forbid(
    clippy::as_conversions,
    clippy::cast_ptr_alignment,
    clippy::let_underscore_drop,
    trivial_casts,
    unconditional_recursion,
    unsafe_code
)]

use futures_util::{SinkExt as _, StreamExt as _};
use std::mem::drop;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
pub use yzix_proto::*;

#[derive(Clone)]
pub struct Driver {
    wchan_s: mpsc::UnboundedSender<WorkMessage>,
}

#[derive(Debug)]
enum WorkMessage {
    GetStorePath {
        answ_chan: oneshot::Sender<Response>,
    },
    SubmitTask {
        data: WorkItem,
        answ_chan: oneshot::Sender<TaskBoundResponse>,
    },
    Upload {
        data: ThinTree,
        answ_chan: oneshot::Sender<Response>,
    },
    HasOutHash {
        data: TaggedHash<ThinTree>,
        answ_chan: oneshot::Sender<Response>,
    },
    Download {
        data: TaggedHash<ThinTree>,
        answ_chan: oneshot::Sender<Response>,
    },
    DownloadRegular {
        data: TaggedHash<Regular>,
        answ_chan: oneshot::Sender<Result<Regular, StoreError>>,
    },
}

/// represents an in-flight server request, which expects a sequential response.
#[derive(Debug)]
struct Inflight {
    orig_req: String,
    answ_chan: oneshot::Sender<Response>,
}

/// server-side active task
// NOTE: `Upload` and `Download` can also produce `TaskBound` responses,
// but still need to be handled sequentially
struct RSTask {
    answ_chans: Vec<oneshot::Sender<TaskBoundResponse>>,
}

/// server-side active regular file download
struct RSRegularDownload {
    answ_chans: Vec<oneshot::Sender<Result<Regular, StoreError>>>,
}

impl Driver {
    pub async fn new(stream: TcpStream) -> Self {
        let mut wbs: WbsClientSide<_> = WrappedByteStream::new(stream);

        let (wchan_s, mut wchan_r) = mpsc::unbounded_channel::<WorkMessage>();

        // handle I/O
        tokio::spawn(async move {
            let mut backlog = std::collections::VecDeque::new();
            let mut inflight_info: Option<Inflight> = None;
            let mut running = std::collections::HashMap::<StoreHash, RSTask>::new();
            let mut running_rdl =
                std::collections::HashMap::<TaggedHash<Regular>, RSRegularDownload>::new();
            use {Request as Req, Response as Resp, TaskBoundResponse as Tbr};
            loop {
                tokio::select!(
                    msg = wchan_r.recv() => {
                        let msg = if let Some(msg) = msg {
                            msg
                        } else {
                            break;
                        };
                        match msg {
                            WorkMessage::SubmitTask { data, answ_chan } => {
                                use std::collections::hash_map::Entry;
                                let tid = StoreHash::hash_complex(&data);
                                tracing::info!("{}: submitting", tid);
                                if let Err(e) = wbs.send(
                                    Req::SubmitTask { item: data, subscribe2log: true }
                                ).await {
                                    tracing::error!("connection error: {}", e);
                                    break;
                                }
                                tracing::info!("{}: submitted", tid);
                                match running.entry(tid) {
                                    Entry::Occupied(mut occ) => occ.get_mut().answ_chans.push(answ_chan),
                                    Entry::Vacant(vac) => {
                                        let _ = vac.insert(RSTask { answ_chans: vec![answ_chan] });
                                    },
                                }
                            },
                            WorkMessage::DownloadRegular { data: tid, answ_chan } => {
                                use std::collections::hash_map::Entry;
                                tracing::debug!("{}: schedule download", tid.as_ref());
                                if let Err(e) = wbs.send(Req::DownloadRegular(tid)).await {
                                    tracing::error!("connection error: {}", e);
                                    break;
                                }
                                tracing::debug!("{}: download scheduled", tid.as_ref());
                                match running_rdl.entry(tid) {
                                    Entry::Occupied(mut occ) => occ.get_mut().answ_chans.push(answ_chan),
                                    Entry::Vacant(vac) => {
                                        let _ = vac.insert(RSRegularDownload { answ_chans: vec![answ_chan] });
                                    },
                                }
                            },
                            _ => {
                                let (xmsg, answ_chan) = match msg {
                                    WorkMessage::GetStorePath { answ_chan } =>
                                        (Req::GetStorePath, answ_chan),
                                    WorkMessage::Upload { data, answ_chan } =>
                                        (Req::Upload(data), answ_chan),
                                    WorkMessage::HasOutHash { data, answ_chan } =>
                                        (Req::HasOutHash(data), answ_chan),
                                    WorkMessage::Download { data, answ_chan } =>
                                        (Req::Download(data), answ_chan),
                                    WorkMessage::DownloadRegular { .. }
                                    | WorkMessage::SubmitTask { .. } =>
                                        unreachable!(),
                                };
                                backlog.push_back((xmsg, answ_chan));
                            }
                        };
                    }
                    msg = wbs.next() => {
                        let msg = if let Some(msg) = msg {
                            msg
                        } else {
                            break;
                        };
                        let msg = match msg {
                            Ok(msg) => msg,
                            Err(e) => {
                                tracing::error!("{}", e);
                                continue;
                            }
                        };
                        match msg {
                            Resp::TaskBound(tid, Tbr::Log(lmsg)) => {
                                tracing::info!("{}: {}", tid, lmsg);
                            }
                            Resp::TaskBound(tid, tbr) if running.contains_key(&tid) => {
                                let rinfo = running.remove(&tid).unwrap();
                                tracing::debug!("{}: -> {:?}", tid, tbr);
                                // broadcast
                                for i in rinfo.answ_chans {
                                    drop::<Result<_, _>>(i.send(tbr.clone()));
                                }
                            }
                            Resp::TaskBound(tid, tbr) => {
                                // NOTE: has the disadvantage that this is on a best-effort basis
                                if let Some(x) = inflight_info.take() {
                                    tracing::debug!("{}: {} -> {:?}", tid, x.orig_req, tbr);
                                    drop::<Result<_, _>>(x.answ_chan.send(Resp::TaskBound(tid, tbr)));
                                } else {
                                    tracing::warn!("{}: {:?} (unhandled)", tid, tbr);
                                }
                            }
                            Resp::RegularBound(tid, tbr) => {
                                match running_rdl.remove(&tid) {
                                    Some(rinfo) => {
                                        tracing::debug!("{} downloaded", tid.as_ref());
                                        // broadcast
                                        for i in rinfo.answ_chans {
                                            drop::<Result<_, _>>(i.send(tbr.clone()));
                                        }
                                    }
                                    None => {
                                        tracing::warn!("{} downloaded (unhandled)", tid.as_ref());
                                    }
                                }
                            }
                            Resp::LogError => {
                                tracing::error!(
                                    "some log messages got lost, build queue might be corrupted => shutdown"
                                );
                                break;
                            }
                            Resp::Aborted | Resp::Ok | Resp::False | Resp::Text(_) | Resp::Dump(_) => {
                                if let Some(x) = inflight_info.take() {
                                    tracing::debug!("{} -> {:?}", x.orig_req, msg);
                                    drop::<Result<_, _>>(x.answ_chan.send(msg));
                                } else {
                                    tracing::warn!("{:?} (unhandled)", msg);
                                }
                            }
                        }
                    }
                );

                if inflight_info.is_none() {
                    if let Some((xmsg, answ_chan)) = backlog.pop_front() {
                        inflight_info = Some(Inflight {
                            orig_req: format!("{}", xmsg),
                            answ_chan,
                        });
                        tracing::debug!("send request: {}", xmsg);
                        if let Err(e) = wbs.send(xmsg).await {
                            tracing::error!("connection error: {}", e);
                            break;
                        }
                    }
                }
            }
        });

        Self { wchan_s }
    }

    pub async fn store_path(&self) -> String {
        let (answ_chan, answ_get) = oneshot::channel();
        self.wchan_s
            .send(WorkMessage::GetStorePath { answ_chan })
            .unwrap();
        match answ_get.await.unwrap() {
            Response::Text(t) => t,
            r => panic!("invalid GetStorePath response: {:?}", r),
        }
    }

    pub async fn run_task(&self, data: WorkItem) -> TaskBoundResponse {
        let (answ_chan, answ_get) = oneshot::channel();
        self.wchan_s
            .send(WorkMessage::SubmitTask { data, answ_chan })
            .unwrap();
        answ_get.await.unwrap()
    }

    pub async fn upload(&self, data: ThinTree) -> Response {
        let (answ_chan, answ_get) = oneshot::channel();
        self.wchan_s
            .send(WorkMessage::Upload { data, answ_chan })
            .unwrap();
        answ_get.await.unwrap()
    }

    pub async fn has_out_hash(&self, data: TaggedHash<ThinTree>) -> bool {
        let (answ_chan, answ_get) = oneshot::channel();
        self.wchan_s
            .send(WorkMessage::HasOutHash { data, answ_chan })
            .unwrap();
        answ_get.await.unwrap() == Response::Ok
    }

    pub async fn download(&self, data: TaggedHash<ThinTree>) -> Response {
        let (answ_chan, answ_get) = oneshot::channel();
        self.wchan_s
            .send(WorkMessage::Download { data, answ_chan })
            .unwrap();
        answ_get.await.unwrap()
    }

    pub async fn download_regular(&self, data: TaggedHash<Regular>) -> Result<Regular, StoreError> {
        let (answ_chan, answ_get) = oneshot::channel();
        self.wchan_s
            .send(WorkMessage::DownloadRegular { data, answ_chan })
            .unwrap();
        answ_get.await.unwrap()
    }
}

pub async fn do_auth(stream: &mut TcpStream, bearer_token: &str) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let buf = bearer_token.to_string().into_bytes();
    stream
        .write_all(&ProtoLen::try_from(buf.len()).unwrap().to_le_bytes()[..])
        .await?;
    stream.write_all(&buf[..]).await?;
    Ok(())
}
