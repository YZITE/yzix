use futures_util::{SinkExt as _, StreamExt as _};
use std::sync::Arc;
use core::mem::drop;
use hyper::{header, Body, body::HttpBody as _, Method, Request, Response, StatusCode};
use tokio::io::AsyncReadExt as _;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use yzix_core::{BuildError, StoreHash, TaskBoundResponse};
use yzix_proto::{ProtoLen, WbsServerSide, WrappedByteStream};
use yzix_store_builder::ControlMessage as CtrlMsg;

async fn handle_store_thintree(
    config: Arc<crate::ServerConfig>,
    // channel for requests from client to server
    ctrl_reqs: mpsc::Sender<CtrlMsg>,
    // the incoming request
    h_method: Method,
    treeid: String,
    h_headers: header::HeaderMap,
    h_body: Body,
) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    // this needs to handle uploads, downloads, etc.
    todo!()
}

pub async fn handle_client(
    config: Arc<crate::ServerConfig>,
    // channel for requests from client to server
    ctrl_reqs: mpsc::Sender<CtrlMsg>,
    // the incoming request
    http_req: Request<Body>,
) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let (h_parts, mut h_body) = http_req.into_parts();

    // auth
    if let Method::POST | Method::PUT = &h_parts.method {
        if h_parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|x| x.to_str().ok())
            .and_then(|x| x.trim().strip_prefix("Bearer "))
            .map(|x| config.bearer_tokens.contains(x)) != Some(true) {
            return Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::empty()).map_err(|e| e.into());
        }
    }

    const MAX_ALLOWED_REQUEST_BODY: usize = u32::MAX.try_into().unwrap();

    match (h_parts.method.clone(), h_parts.uri.path()) {
        // TODO: CORS stuff

        (Method::GET, "/info") => {
            #[derive(Clone, Copy)]
            enum InfoFormat {
                Json,
                Nix,
            }
            let mut f = InfoFormat::Json;
            if let Some(x) = h_parts
                .headers
                .get(header::ACCEPT) {
                for i in x.split(',') {
                    let mut i = i.trim();
                    if let Some(pos) = i.find(';') {
                        i = i[..pos];
                    }
                    match i {
                        "application/json" => {
                            f = InfoFormat::Json;
                            break;
                        }
                        "text/x-nix-cache-info" => {
                            f = InfoFormat::Nix;
                            break;
                        }
                    }
                }
            }
            let mut inf = std::collections::BTreeMap::new();
            inf.insert("StoreDir", config.store_path.to_str());
            inf.insert("LogCompression", "zst");
            match InfoFormat {
                InfoFormat::Json => Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(serde_json::to_string_pretty(&inf).into()).map_err(|e| e.into()),
                InfoFormat::Nix => Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/x-nix-cache-info")
                    .body(inf.iter().map(|(k, v)| format!("{}: {}\n", k, v)).collect::<Vec<_>>().join("")).map_err(|e| e.into()),
            }
        }

        (Method::POST, "/task") => {
            fn mk_reqerr(t: &str) -> Result<Response<Body>, hyper::Error> {
                Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(serde_json::to_string(serde_json::json!({ "errors": [ {
                        "type": t
                    } ] })).into())
            }
            let blen = match h_body.size_hint().upper() {
                Some(len) => len,
                None => return Response::builder()
                    .status(StatusCode::LENGTH_REQUIRED)
                    .body(Body::empty()).map_err(|e| e.into()),
            };
            if blen > u32::MAX.into() {
                return Response::builder()
                    .status(StatusCode::PAYLOAD_TOO_LARGE)
                    .body(Body::empty()).map_err(|e| e.into());
            }
            if h_parts
                .headers
                .get(header::CONTENT_TYPE)
                .map(|x| x == "application/json")
                != Some(true)
            {
                return Response::builder()
                    .status(StatusCode::UNSUPPORTED_MEDIA_TYPE)
                    .body(Body::empty()).map_err(|e| e.into());
            }
            let bbytes = hyper::body::into_bytes(h_body).await?;
            let bjson: yzix_core::WorkItem = serde_json::from_bytes(bbytes) {
                Ok(x) => x,
                Err(e) => return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(serde_json::to_string(serde_json::json!({ "errors": [ {
                        "type": "request.invalid.format",
                        "f": "Request isn't valid JSON"
                    } ] })).into()).map_err(|e| e.into())

            };
            drop(bbytes);
            use futures_util::StreamExt as _;

            let (answ_chan, answ_recv) = oneshot::channel();
            let resp_s = resp_s.clone();
            let _ = tokio::spawn(async move {
                resp_s
                    .send(match answ_recv.await {
                        Ok(x) => Response::TaskBound(x, TaskBoundResponse::Queued),
                        Err(_) => Response::Aborted,
                    })
                    .await
            });

            let (log_s, mut log_r) = mpsc::channel(1000);

            if reqs.send(CtrlMsg::SubmitTask {
                    item,
                    answ_chan,
                    subscribe: Some(log_s.clone()),
                }).await.is_err() {
                return Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::empty()).map_err(|e| e.into());
            }

            let tid = match answ_recv.await {
                Ok(x) => x,
                Err(_) => return Response::builder()
                    .status(StatusCode::CONFLICT)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(serde_json::to_string(serde_json::json!({ "errors": [ {
                        "type": "request.cancelled"
                    } ] })).into()).map_err(|e| e.into()),
            };

            use serde_json::{json, to_string as sjts};
                Response::builder()
                    .status(StatusCode::ACCEPTED)
                    .header(header::CONTENT_TYPE, "text/x-yzix-task-log")
                    .body((Box::new(
                        tokio_stream::wrappers::ReceiverStream::new(log_r)
                        .map(|(_, tbr)| match tbr {
                            TaskBoundResponse::Queued => unreachable!(),
                            TaskBoundResponse::BuildSuccess(outs) => {
                                sjts(serde_json::json({
                                    "outputs": outs,
                                }))
                            }
                            TaskBoundResponse::BuildError(e) => {
                                sjts(match e {
                                    BuildError::Store(yzix_core::StoreError { real_path, kind }) => json!({
                                        "errors": [ {
                                            "type": kind.errtype(),
                                            "f": e.to_string(),
                                            "path": path.display().to_string(),
                                        } ]
                                    }),
                                    BuildError::Unknown(s) => json!({
                                        "errors": [ {
                                            "type": "unknown",
                                            "f": s,
                                        } ]
                                    }),
                                    _ => json!({
                                        "errors": [ {
                                            "type": e.errtype(),
                                            "f": e.to_string(),
                                        } ]
                                    }),
                                })
                            }
                            TaskBoundResponse::Log(s) => format!(".{}", s),
                        })
                        .map(|i| Ok(hyper::body::Bytes::copy_from_slice(i.as_bytes()))
                    ) as Box::<dyn futures::Stream>)).into()).map_err(|e| e.into())
        }

        (Method::PUT, i) => {
            let j = i.split('/').collect::<Vec<_>>();
            if j.get(0) == Some("") && j.get(1) == Some("store") && j.get(3) == Some("thintree") && j.get(4) == None {
                let j_ = j.get(1);
                drop(j);
                handle_store_thintree(
                    config,
                    ctrl_reqs,
                    h_parts.method,
                    j_,
                    h_parts.headers,
                    h_body,
                )
            } else {
                Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(Body::empty())
                    .map_err(|e| e.into())
            }
        }

        (Method::HEAD | Method::GET, i) => {
            let j = i.split('/').collect::<Vec<_>>();
            if j.get(0) == Some("") && j.get(1) == Some("store") && j.get(3) == Some("thintree") && j.get(4) == None {
                let j_ = j.get(1).unwrap().to_string();
                drop(j);
                handle_store_thintree(
                    config,
                    ctrl_reqs,
                    h_parts.method,
                    j_,
                    h_parts.headers,
                    h_body,
                )
            } else {
                // TODO: realizations
                //(Method::GET, "/realisations/{hash}") => todo!(),

                let resolved = hyper_staticfile::resolve_path(config.store_path.clone(), i).await?;

                let is_exe = match resolved {
                    hyper_staticfile::ResolveResult::Found(_, meta, _) => std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0,
                    _ => false,
                };

                let resp = hyper_staticfile::ResponseBuilder::new()
                    .request_parts(&h_parts.method, &h_parts.uri, &h_parts.headers)
                    // cache response for 7 days
                    .cache_headers(Some(604800))
                    .build(resolved)?;

                if is_exe {
                    // we need to add the `X-Executable` header if the file is executable
                    resp.headers_mut().insert("x-executable", "true");
                }
                Ok(resp)
            }
        }

        (_, i) => Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Body::empty())
                    .map_err(|e| e.into()),
    }

    /*
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
                Req::SubmitTask {
                    item,
                    subscribe2log,
                } => {
                    let (answ_chan, answ_recv) = oneshot::channel();
                    let resp_s = resp_s.clone();
                    let _ = tokio::spawn(async move {
                        resp_s
                            .send(match answ_recv.await {
                                Ok(x) => Response::TaskBound(x, TaskBoundResponse::Queued),
                                Err(_) => Response::Aborted,
                            })
                            .await
                    });
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
                    let (answ_chan, answ_recv) = oneshot::channel::<bool>();
                    let resp_s = resp_s.clone();
                    let _ = tokio::spawn(async move {
                        resp_s
                            .send(match answ_recv.await {
                                Ok(x) => x.into(),
                                Err(_) => Response::Aborted,
                            })
                            .await
                    });
                    Some(CtrlMsg::Kill { task_id, answ_chan })
                }
                Req::Upload(dump) => {
                    let (answ_chan, answ_recv) = oneshot::channel();
                    let outhash = StoreHash::hash_complex(&dump);
                    let resp_s = resp_s.clone();
                    let _ = tokio::spawn(async move {
                        resp_s
                            .send(match answ_recv.await {
                                Ok(Ok(())) => Response::Ok,
                                Ok(Err(e)) => {
                                    Response::TaskBound(outhash, BuildError::from(e).into())
                                }
                                Err(_) => Response::Aborted,
                            })
                            .await
                    });
                    Some(CtrlMsg::Upload { dump, answ_chan })
                }
                Req::HasOutHash(outhash) => {
                    let (answ_chan, answ_recv) = oneshot::channel::<bool>();
                    let resp_s = resp_s.clone();
                    let _ = tokio::spawn(async move {
                        resp_s
                            .send(match answ_recv.await {
                                Ok(x) => x.into(),
                                Err(_) => Response::Aborted,
                            })
                            .await
                    });
                    Some(CtrlMsg::HasOutHash { outhash, answ_chan })
                }
                Req::Download(outhash) => {
                    let (answ_chan, answ_recv) = oneshot::channel();
                    let resp_s = resp_s.clone();
                    let _ = tokio::spawn(async move {
                        resp_s
                            .send(match answ_recv.await {
                                Ok(Ok(dump)) => Response::Dump(dump),
                                Ok(Err(e)) => {
                                    Response::TaskBound(outhash, BuildError::from(e).into())
                                }
                                Err(_) => Response::Aborted,
                            })
                            .await
                    });
                    Some(CtrlMsg::Download { outhash, answ_chan })
                }
                Req::DownloadRegular(outhash) => {
                    let (answ_chan, answ_recv) = oneshot::channel();
                    let resp_s = resp_s.clone();
                    let _ = tokio::spawn(async move {
                        resp_s
                            .send(match answ_recv.await {
                                Ok(Ok(regu)) => Response::RegularBound(outhash, Ok(regu)),
                                Ok(Err(e)) => Response::RegularBound(outhash, Err(e)),
                                Err(_) => Response::Aborted,
                            })
                            .await
                    });
                    Some(CtrlMsg::DownloadRegular { outhash, answ_chan })
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
    */
}
