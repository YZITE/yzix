use futures_util::{SinkExt as _, StreamExt as _};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
pub use yzix_proto::{store::Dump, store::Hash as StoreHash, Response, TaskBoundResponse};
use yzix_proto::{WbsClientSide, WrappedByteStream};

#[derive(Clone)]
pub struct Driver {
    wchan_s: mpsc::UnboundedSender<WorkMessage>,
}

#[derive(Debug)]
enum WorkMessage {
    SubmitTask {
        data: yzix_proto::WorkItem,
        answ_chan: oneshot::Sender<yzix_proto::TaskBoundResponse>,
    },
    Upload {
        data: yzix_proto::store::Dump,
        answ_chan: oneshot::Sender<yzix_proto::Response>,
    },
    HasOutHash {
        data: StoreHash,
        answ_chan: oneshot::Sender<yzix_proto::Response>,
    },
    Download {
        data: StoreHash,
        answ_chan: oneshot::Sender<yzix_proto::Response>,
    },
}

/// represents an in-flight server request, which expects a sequential response.
#[derive(Debug)]
struct Inflight {
    orig_req: yzix_proto::Request,
    answ_chan: oneshot::Sender<yzix_proto::Response>,
}

/// server-side active task
// NOTE: `Upload` and `Download` can also produce `TaskBound` responses,
// but still need to be handled sequentially
struct RSTask {
    answ_chans: Vec<oneshot::Sender<yzix_proto::TaskBoundResponse>>,
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
            use yzix_proto::{Request as Req, Response as Resp, TaskBoundResponse as Tbr};
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
                                tracing::info!("{}: submitted", tid);
                                if let Err(e) = wbs.send(
                                    Req::SubmitTask { item: data, subscribe2log: true }
                                ).await {
                                    tracing::error!("connection error: {}", e);
                                    break;
                                }
                                match running.entry(tid) {
                                    Entry::Occupied(mut occ) => occ.get_mut().answ_chans.push(answ_chan),
                                    Entry::Vacant(vac) => {
                                        let _ = vac.insert(RSTask { answ_chans: vec![answ_chan] });
                                    },
                                }
                            },
                            _ => {
                                let (xmsg, answ_chan) = match msg {
                                    WorkMessage::Upload { data, answ_chan } =>
                                        (Req::Upload(data), answ_chan),
                                    WorkMessage::HasOutHash { data, answ_chan } =>
                                        (Req::HasOutHash(data), answ_chan),
                                    WorkMessage::Download { data, answ_chan } =>
                                        (Req::Download(data), answ_chan),
                                    _ => unreachable!(),
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
                            Resp::TaskBound(tid, Tbr::Queued) => {
                                tracing::info!(
                                    "{}: queued{}",
                                    tid,
                                    if running.contains_key(&tid) { "" } else { " (unhandled)" }
                                );
                            }
                            Resp::TaskBound(tid, tbr) if running.contains_key(&tid) => {
                                let rinfo = running.remove(&tid).unwrap();
                                tracing::debug!("{}: -> {:?}", tid, tbr);
                                // broadcast
                                for i in rinfo.answ_chans {
                                    let _ = i.send(tbr.clone()).is_ok();
                                }
                            }
                            Resp::TaskBound(tid, tbr) => {
                                // NOTE: has the disadvantage that this is on a best-effort basis
                                if let Some(x) = inflight_info.take() {
                                    let tfmt = match x.orig_req {
                                        Req::UnsubscribeAll => unreachable!(),
                                        Req::Upload(d) => format!("Upload(...@ {})", StoreHash::hash_complex(&d)),
                                        _ => format!("{:?}", x.orig_req),
                                    };
                                    tracing::debug!("{}: {} -> {:?}", tid, tfmt, tbr);
                                    let _ = x.answ_chan.send(Resp::TaskBound(tid, tbr)).is_ok();
                                } else {
                                    tracing::warn!("{}: {:?} (unhandled)", tid, tbr);
                                }
                            }
                            Resp::LogError => {
                                tracing::error!(
                                    "some log messages got lost, build queue might be corrupted => shutdown"
                                );
                                break;
                            }
                            Resp::Ok | Resp::False | Resp::Dump(_) => {
                                if let Some(x) = inflight_info.take() {
                                    let tfmt = match x.orig_req {
                                        Req::UnsubscribeAll => unreachable!(),
                                        Req::Upload(d) => format!("Upload(...@ {})", StoreHash::hash_complex(&d)),
                                        _ => format!("{:?}", x.orig_req),
                                    };
                                    tracing::debug!("{} -> {:?}", tfmt, msg);
                                    let _ = x.answ_chan.send(msg).is_ok();
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
                            orig_req: xmsg.clone(),
                            answ_chan,
                        });
                        tracing::debug!("send request: {:?}", xmsg);
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

    pub async fn run_task(&self, data: yzix_proto::WorkItem) -> yzix_proto::TaskBoundResponse {
        let (answ_chan, answ_get) = oneshot::channel();
        self.wchan_s
            .send(WorkMessage::SubmitTask { data, answ_chan })
            .unwrap();
        answ_get.await.unwrap()
    }

    pub async fn upload(&self, data: yzix_proto::store::Dump) -> yzix_proto::Response {
        let (answ_chan, answ_get) = oneshot::channel();
        self.wchan_s
            .send(WorkMessage::Upload { data, answ_chan })
            .unwrap();
        answ_get.await.unwrap()
    }

    pub async fn has_out_hash(&self, data: StoreHash) -> yzix_proto::Response {
        let (answ_chan, answ_get) = oneshot::channel();
        self.wchan_s
            .send(WorkMessage::HasOutHash { data, answ_chan })
            .unwrap();
        answ_get.await.unwrap()
    }

    pub async fn download(&self, data: StoreHash) -> yzix_proto::Response {
        let (answ_chan, answ_get) = oneshot::channel();
        self.wchan_s
            .send(WorkMessage::Download { data, answ_chan })
            .unwrap();
        answ_get.await.unwrap()
    }
}

pub async fn do_auth(
    stream: &mut TcpStream,
    bearer_token: &str,
) -> Result<(), yzix_proto::ciborium::ser::Error<std::io::Error>> {
    use tokio::io::AsyncWriteExt;
    let mut buf = Vec::<u8>::new();
    yzix_proto::ciborium::ser::into_writer(bearer_token, &mut buf)?;
    stream
        .write_all(
            &yzix_proto::ProtoLen::try_from(buf.len())
                .unwrap()
                .to_le_bytes()[..],
        )
        .await?;
    stream.write_all(&buf[..]).await?;
    Ok(())
}
