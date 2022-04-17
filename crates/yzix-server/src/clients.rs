use futures_util::{StreamExt as _, SinkExt as _};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::io::{AsyncReadExt as _};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc};
use tokio::task::block_in_place;
use yzix_proto::{store, ProtoLen, TaskBoundResponse, WrappedByteStream, WbsServerSide};

pub struct Request {
    pub inner: RequestKind,
    pub resp: mpsc::Sender<yzix_proto::Response>,
}

pub enum RequestKind {
    Kill(store::Hash),
    SubmitTask {
        item: crate::FullWorkItem,
        subscribe: Option<mpsc::Sender<broadcast::Receiver<(store::Hash, Arc<TaskBoundResponse>)>>>,
    },
    Upload(store::Dump),
    HasOutHash(store::Hash),
    Download(store::Hash),
}

pub async fn handle_client(
    // channel for requests from client to server
    reqs: mpsc::Sender<Request>,
    // the associated client tcp stream
    mut stream: TcpStream,
    // valid bearer tokens for auth
    valid_bearer_tokens: Arc<HashSet<String>>,
) {
    // auth
    let mut lenbuf = [0u8; std::mem::size_of::<ProtoLen>()];
    {
        if stream.read_exact(&mut lenbuf).await.is_err() {
            return;
        }
        let len = ProtoLen::from_le_bytes(lenbuf);
        if len >= 0x400 {
            return;
        }
        let mut buf: Vec<u8> = Vec::new();
        buf.resize(len.try_into().unwrap(), 0);
        if stream.read_exact(&mut buf[..]).await.is_err() {
            return;
        }
        let bearer_token: String = match ciborium::de::from_reader(&buf[..]) {
            Ok(x) => x,
            Err(_) => return,
        };
        if !valid_bearer_tokens.contains(&bearer_token) {
            return;
        }
    }

    // normal comm
    let wbs: WbsServerSide<_> = WrappedByteStream::new(stream);
    let (mut sink, mut stream) = wbs.split();

    let (subscribe_s, mut subscribe_r) = mpsc::channel(1000);
    let (resp_s, mut resp_r) = mpsc::channel(1000);
    let unsubscr = Arc::new(tokio::sync::Notify::new());
    let unsubscr2 = unsubscr.clone();

    let handle_input = async move {
        loop {
            use yzix_proto::Request as Req;
            let req: Req = match stream.next().await {
                Some(Ok(x)) => x,
                Some(Err(e)) => {
                    tracing::error!("client comm aborted (recv error): {}", e);
                    break;
                },
                None => break,
            };
            let req = match req {
                Req::UnsubscribeAll => {
                    unsubscr.notify_one();
                    None
                }
                Req::SubmitTask {
                    item,
                    subscribe2log,
                } => Some(RequestKind::SubmitTask {
                    item: block_in_place(|| item.into()),
                    subscribe: if subscribe2log {
                        Some(subscribe_s.clone())
                    } else {
                        None
                    },
                }),
                Req::Kill(tid) => Some(RequestKind::Kill(tid)),
                Req::Upload(d) => Some(RequestKind::Upload(d)),
                Req::HasOutHash(h) => Some(RequestKind::HasOutHash(h)),
                Req::Download(h) => Some(RequestKind::Download(h)),
            };
            if let Some(inner) = req {
                if reqs
                    .send(Request {
                        inner,
                        resp: resp_s.clone(),
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    };

    let handle_output = async move {
        let mut buf = Vec::<u8>::new();
        // we spawn a forwarder task for each subscribed task
        let (mut log_s, mut log_r) = mpsc::channel(1000);
        loop {
            use yzix_proto::Response;
            let mut msg = Option::<Response>::None;
            tokio::select! {
                biased;

                v = subscribe_r.recv() => if let Some(mut tbrchan) = v {
                    let log2_s = log_s.clone();
                    tokio::spawn(async move {
                        while let Ok(x) = tbrchan.recv().await {
                            if log2_s.send(x).await.is_err() {
                                break;
                            }
                        }
                    });
                } else {
                    break;
                },

                () = unsubscr2.notified() => {
                    let (log2_s, log2_r) = mpsc::channel(1000);
                    log_s = log2_s;
                    log_r = log2_r;
                },

                v = log_r.recv() => {
                    let (tid, tbr) = v.unwrap();
                    msg = Some(Response::TaskBound(tid, (*tbr).clone()));
                },

                v = resp_r.recv() => match v {
                    None => break,
                    Some(resp) => msg = Some(resp),
                },
            }

            buf.clear();

            let msg = if let Some(msg) = msg {
                msg
            } else {
                continue;
            };

            if let Err(e) = sink.send(msg).await {
                tracing::error!("client comm error (while sending): {}", e);
                if let ciborium::ser::Error::Io(_) = e {
                    break;
                }
            }
        }

        let _ = sink.close().await;
    };

    tokio::join!(handle_input, handle_output);
}
