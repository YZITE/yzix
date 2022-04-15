use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, broadcast, mpsc};
use yzix_proto::{ProtoLen, TaskId, TaskBoundResponse, WorkItem, store};
use std::collections::HashSet;
use std::sync::Arc;

pub struct Request {
    pub inner: RequestKind,
    pub resp: mpsc::Sender<yzix_proto::Response>,
}

pub enum RequestKind {
    Kill(TaskId),
    SubmitTask {
        item: WorkItem,
        subscribe: Option<mpsc::Sender<TaskId>>,
    },
    Upload(store::Dump),
    HasOutHash(store::Hash),
    Download(store::Hash),
}

pub async fn handle_client(
    // channel for requests from client to server
    reqs: broadcast::Sender<Request>,
    // channel for task build log messages
    mut logs: broadcast::Receiver<(TaskId, Arc<TaskBoundResponse>)>,
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
    let (stream, mut stream2) = stream.split();

    let (subscribe_s, mut subscribe_r) = mpsc::channel::<TaskId>(1000);
    let (resp_s, mut resp_r) = mpsc::channel(1000);
    let logsubs = Arc::new(Mutex::new(HashSet::<TaskId>::new()));
    let logsubs2 = logsubs.clone();

    let handle_input = async move {
        let mut buf: Vec<u8> = Vec::new();
        let mut stream = tokio::io::BufReader::new(stream);
        while stream.read_exact(&mut lenbuf).await.is_ok() {
            buf.clear();
            let len = ProtoLen::from_le_bytes(lenbuf);
            // TODO: make sure that the length isn't too big
            buf.resize(len.try_into().unwrap(), 0);
            if stream.read_exact(&mut buf[..]).await.is_err() {
                break;
            }
            let val: ciborium::value::Value = match ciborium::de::from_reader(&buf[..]) {
                Ok(x) => x,
                Err(e) => {
                    // TODO: report error to client, maybe?
                    // this can happen either when the serialization format
                    // between client and server mismatches,
                    // or when we run into a ciborium bug.
                    tracing::error!("CBOR: {}", e);
                    break;
                }
            };
            use yzix_proto::Request as Req;
            let req: Req = match val.deserialized() {
                Ok(x) => x,
                Err(e) => {
                    tracing::error!("CBOR: {}", e);
                    tracing::debug!("CBOR: {:#?}", val);
                    break;
                }
            };
            let req = match req {
                Req::UnsubscribeAll => { logsubs.lock().await.clear(); None },
                Req::LogSub(tid, false) => { logsubs.lock().await.remove(&tid); None },
                Req::LogSub(tid, true) => { logsubs.lock().await.insert(tid); None },
                Req::SubmitTask { item, auto_subscribe } =>
                    Some(RequestKind::SubmitTask {
                        item,
                        subscribe: if auto_subscribe {
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
                if reqs.send(Request {
                    inner,
                    resp: resp_s.clone(),
                }).is_err() {
                    break;
                }
            }
        }
    };

    let handle_output = async move {
        let mut buf = Vec::<u8>::new();
        loop {
            use yzix_proto::Response;
            use broadcast::error::RecvError::Closed;
            let mut msg = Option::<Response>::None;
            tokio::select! {
                biased;

                v = subscribe_r.recv() => if let Some(tid) = v {
                    logsubs2.lock().await.insert(tid);
                } else {
                    break;
                },

                v = logs.recv() => match v {
                    Err(Closed) => break,
                    Err(_) => msg = Some(Response::LogError),
                    Ok((tid, tbr)) => {
                        let mut ls = logsubs2.lock().await;
                        if ls.contains(&tid) {
                            if tbr.task_finished() {
                                ls.remove(&tid);
                            }
                            msg = Some(Response::TaskBound(tid, (*tbr).clone()));
                        }
                    },
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

            if let Err(e) = ciborium::ser::into_writer(&msg, &mut buf) {
                // TODO: handle error
                tracing::error!("CBOR: {}", e);
            } else {
                if stream2
                    .write_all(&ProtoLen::to_le_bytes(buf.len().try_into().unwrap()))
                    .await
                    .is_err()
                {
                    break;
                }
                if stream2.write_all(&buf[..]).await.is_err() {
                    break;
                }
                if stream2.flush().await.is_err() {
                    break;
                }
            }
        }
    };

    tokio::join!(handle_input, handle_output);
}
