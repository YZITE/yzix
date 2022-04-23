use futures_util::{FutureExt as _, SinkExt as _, StreamExt as _};
use std::sync::Arc;
use tokio::io::AsyncReadExt as _;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use yzix_proto::{
    BuildError, Dump, ProtoLen, StoreError, StoreHash, TaskBoundResponse, WbsServerSide,
    WrappedByteStream,
};
use yzix_store_builder::ControlMessage as CtrlMsg;

pub async fn handle_client(
    config: Arc<crate::ServerConfig>,
    // channel for requests from client to server
    reqs: mpsc::Sender<CtrlMsg>,
    // the associated client tcp stream
    mut stream: TcpStream,
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
        let bearer_token = match std::str::from_utf8(&buf[..]) {
            Ok(x) => x,
            Err(_) => return,
        };
        if !config.bearer_tokens.contains(bearer_token) {
            return;
        }
    }

    // normal comm
    let wbs: WbsServerSide<_> = WrappedByteStream::new(stream);
    let (mut sink, mut stream) = wbs.split();

    let (log_s, mut log_r) = mpsc::channel(1000);
    let (resp_s, mut resp_r) = mpsc::channel(1000);

    let handle_input = async move {
        loop {
            use yzix_proto::{Request as Req, Response};
            let req: Req = match stream.next().await {
                Some(Ok(x)) => x,
                Some(Err(e)) => {
                    tracing::error!("client comm aborted (recv error): {}", e);
                    break;
                }
                None => break,
            };
            let req: Option<CtrlMsg> = match req {
                Req::GetStorePath => {
                    if resp_s
                        .send(Response::Text(config.store_path.as_str().to_string()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                    None
                }
                Req::SubmitTask {
                    item,
                    subscribe2log,
                } => {
                    let (answ_chan, answ_recv) = oneshot::channel();
                    let resp_s = resp_s.clone();
                    let _ =
                        tokio::spawn(answ_recv.then(move |x: Result<StoreHash, _>| async move {
                            resp_s
                                .send(match x {
                                    Ok(x) => Response::TaskBound(x, TaskBoundResponse::Queued),
                                    Err(_) => Response::Aborted,
                                })
                                .await
                        }));
                    Some(CtrlMsg::SubmitTask {
                        item,
                        answ_chan,
                        subscribe: if subscribe2log {
                            Some(log_s.clone())
                        } else {
                            None
                        },
                    })
                }
                Req::Kill(task_id) => {
                    let (answ_chan, answ_recv) = oneshot::channel();
                    let resp_s = resp_s.clone();
                    let _ = tokio::spawn(answ_recv.then(move |x: Result<bool, _>| async move {
                        resp_s
                            .send(match x {
                                Ok(x) => x.into(),
                                Err(_) => Response::Aborted,
                            })
                            .await
                    }));
                    Some(CtrlMsg::Kill { task_id, answ_chan })
                }
                Req::Upload(dump) => {
                    let (answ_chan, answ_recv) = oneshot::channel();
                    let outhash = StoreHash::hash_complex(&dump);
                    let resp_s = resp_s.clone();
                    let _ = tokio::spawn(answ_recv.then(
                        move |x: Result<Result<(), StoreError>, _>| async move {
                            resp_s
                                .send(match x {
                                    Ok(Ok(())) => Response::Ok,
                                    Ok(Err(e)) => {
                                        Response::TaskBound(outhash, BuildError::from(e).into())
                                    }
                                    Err(_) => Response::Aborted,
                                })
                                .await
                        },
                    ));
                    Some(CtrlMsg::Upload { dump, answ_chan })
                }
                Req::HasOutHash(outhash) => {
                    let (answ_chan, answ_recv) = oneshot::channel();
                    let resp_s = resp_s.clone();
                    let _ = tokio::spawn(answ_recv.then(move |x: Result<bool, _>| async move {
                        resp_s
                            .send(match x {
                                Ok(x) => x.into(),
                                Err(_) => Response::Aborted,
                            })
                            .await
                    }));
                    Some(CtrlMsg::HasOutHash { outhash, answ_chan })
                }
                Req::Download(outhash) => {
                    let (answ_chan, answ_recv) = oneshot::channel();
                    let resp_s = resp_s.clone();
                    let _ = tokio::spawn(answ_recv.then(
                        move |x: Result<Result<Dump, StoreError>, _>| async move {
                            resp_s
                                .send(match x {
                                    Ok(Ok(dump)) => Response::Dump(dump),
                                    Ok(Err(e)) => {
                                        Response::TaskBound(outhash, BuildError::from(e).into())
                                    }
                                    Err(_) => Response::Aborted,
                                })
                                .await
                        },
                    ));
                    Some(CtrlMsg::Download { outhash, answ_chan })
                }
            };
            if let Some(inner) = req {
                if reqs.send(inner).await.is_err() {
                    break;
                }
            }
        }
        tracing::info!("client disconnected");
    };

    let handle_output = async move {
        let mut buf = Vec::<u8>::new();
        // we spawn a forwarder task for each subscribed task
        loop {
            use yzix_proto::Response;
            let msg = tokio::select! {
                biased;

                v = log_r.recv() => {
                    let (tid, tbr): (StoreHash, Arc<TaskBoundResponse>) = v.unwrap();
                    Response::TaskBound(tid, (*tbr).clone())
                },

                v = resp_r.recv() => match v {
                    None => break,
                    Some(resp) => resp,
                },
            };

            buf.clear();

            if let Err(e) = sink.send(msg).await {
                tracing::error!("client comm error (while sending): {}", e);
                break;
            }
        }

        let _sclr = sink.close().await;
    };

    tokio::join!(handle_input, handle_output);
}
