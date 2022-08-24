#![cfg_attr(rustfmt, rustfmt::skip::macros(serde_json::json))]

use core::mem::drop;
use hyper::{body::HttpBody as _, header, Body, Method, Request, Response, StatusCode};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::spawn_blocking;
use yzix_core::{BuildError, TaggedHash, ThinTree};
use yzix_store_builder::{Env as SbEnv, OnObject as OnObj, TaskBoundResponse};

fn resp_aborted() -> Result<Response<Body>, hyper::http::Error> {
    let (chan, body) = Body::channel();
    chan.abort();
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(body)
}

async fn handle_store_thintree(
    sbenv: Arc<SbEnv>,
    // the incoming request
    h_method: Method,
    treeid: String,
    h_headers: header::HeaderMap,
    h_body: Body,
) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    // this needs to handle uploads, downloads, etc.
    let hash: TaggedHash<_> = match treeid.parse() {
        Ok(x) => x,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(
                    serde_json::json!({
                        "errors": [{
                            "type": "hash.parse",
                            "f": e.to_string(),
                        }],
                    })
                    .to_string()
                    .into(),
                )
                .map_err(Into::into)
        }
    };
    match h_method {
        Method::HEAD => {
            Response::builder()
                .status(if OnObj::is_present(&sbenv, hash) {
                    StatusCode::NO_CONTENT
                } else {
                    // this does not perfectly fit the HTTP standard, but I/O errors can always happen...
                    StatusCode::NOT_FOUND
                })
                .body(Body::empty())
        }
        Method::GET => match spawn_blocking(move || OnObj::download(&sbenv, hash)).await {
            Ok(Ok(dump)) => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(
                    serde_json::to_string(&dump)
                        .expect("unable to serialize thintree")
                        .into(),
                ),
            Ok(Err(e)) => Response::builder()
                .status(if e.kind.is_not_found() {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                })
                .header(header::CONTENT_TYPE, "application/json")
                .body(
                    serde_json::json!({
                        "errors": [{
                            "type": e.kind.errtype(),
                            "f": e.to_string(),
                            "path": e.real_path,
                        }],
                    })
                    .to_string()
                    .into(),
                ),
            Err(_) => resp_aborted(),
        }
        .map_err(Into::into),
        Method::PUT => {
            if h_headers
                .get(header::CONTENT_TYPE)
                .map(|x| x == "application/json")
                != Some(true)
            {
                return Response::builder()
                    .status(StatusCode::UNSUPPORTED_MEDIA_TYPE)
                    .body(Body::empty())
                    .map_err(Into::into);
            }
            let bbytes = hyper::body::to_bytes(h_body).await?;
            let data: ThinTree = match serde_json::from_slice(&bbytes[..]) {
                Ok(x) => x,
                Err(e) => {
                    return Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(
                            serde_json::json!({ "errors": [ {
                                "type": "request.invalid.format",
                                "f": format!("Request isn't valid JSON: {}", e),
                            } ] })
                            .to_string()
                            .into(),
                        )
                        .map_err(Into::into);
                }
            };
            drop(bbytes);
            match spawn_blocking(move || OnObj::upload(&sbenv, hash, data)).await {
                Ok(Ok(())) => Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(Body::empty()),
                Ok(Err(e)) => Response::builder()
                    .status(if e.kind.is_not_found() {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::INTERNAL_SERVER_ERROR
                    })
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(
                        serde_json::json!({
                            "errors": [{
                                "type": e.kind.errtype(),
                                "f": e.to_string(),
                                "path": e.real_path,
                            }],
                        })
                        .to_string()
                        .into(),
                    ),
                Err(_) => resp_aborted(),
            }
        }
        _ => Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header(header::ALLOW, "HEAD, GET, PUT")
            .body(Body::empty()),
    }
    .map_err(Into::into)
}

async fn handle_realisations(
    _h_method: Method,
    path: Vec<&str>,
    _h_headers: header::HeaderMap,
    _h_body: Body,
) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    let mut path = path.into_iter();
    let p = if let Some(x) = path.next() {
        x
    } else {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::empty())
            .map_err(Into::into);
    };
    if path.next().is_some() {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .map_err(Into::into);
    }
    drop(path);
    let _ = p;
    // TODO: implement API for realisations
    Response::builder()
        .status(StatusCode::NOT_IMPLEMENTED)
        .body(Body::empty())
        .map_err(Into::into)
    // this needs to handle uploads, downloads, etc.
    // this wasn't implemented in the previous interface, so we don't do it for now...
}

#[derive(Clone, Copy)]
enum InfoFormat {
    Json,
    Nix,
}

impl core::str::FromStr for InfoFormat {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "application/json" => InfoFormat::Json,
            "text/x-nix-cache-info" => InfoFormat::Nix,
            _ => return Err(()),
        })
    }
}

pub async fn handle_client(
    config: Arc<crate::ServerConfig>,
    // store-builder environment
    sbenv: Arc<yzix_store_builder::Env>,
    // the incoming request
    http_req: Request<Body>,
) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
    use serde_json::json;
    let (h_parts, h_body) = http_req.into_parts();

    // auth + length handling, to prevent trivial DDOS
    // (out-of-RAM / out-of-diskspace attacks)
    if matches!(h_parts.method, Method::POST | Method::PUT) {
        if h_parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|x| x.to_str().ok())
            .and_then(|x| x.trim().strip_prefix("Bearer "))
            .map(|x| config.bearer_tokens.contains(x))
            != Some(true)
        {
            return Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::empty())
                .map_err(Into::into);
        }
        let blen = match h_body.size_hint().upper() {
            Some(len) => len,
            None => {
                return Response::builder()
                    .status(StatusCode::LENGTH_REQUIRED)
                    .body(Body::empty())
                    .map_err(Into::into)
            }
        };
        if blen > u32::MAX.into() {
            return Response::builder()
                .status(StatusCode::PAYLOAD_TOO_LARGE)
                .body(Body::empty())
                .map_err(Into::into);
        }
    }

    match (h_parts.method.clone(), h_parts.uri.path()) {
        // TODO: CORS stuff
        // TODO: API to kill tasks
        (Method::GET, "/info") => {
            let mut ifm = InfoFormat::Json;
            if let Some(x) = h_parts
                .headers
                .get(header::ACCEPT)
                .and_then(|x| x.to_str().ok())
            {
                for i in x.split(',') {
                    let mut i = i.trim();
                    if let Some(pos) = i.find(';') {
                        i = &i[..pos];
                    }
                    if let Ok(x) = i.parse() {
                        ifm = x;
                        break;
                    }
                }
            }
            let mut inf = std::collections::BTreeMap::new();
            inf.insert("StoreDir", config.store_path.as_str());
            inf.insert("LogCompression", "zst");
            let resp = Response::builder().status(StatusCode::OK);
            match ifm {
                InfoFormat::Json => resp
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(serde_json::to_string_pretty(&inf)?.into()),
                InfoFormat::Nix => resp
                    .header(header::CONTENT_TYPE, "text/x-nix-cache-info")
                    .body(
                        inf.iter()
                            .map(|(k, v)| format!("{}: {}\n", k, v))
                            .collect::<Vec<_>>()
                            .join("")
                            .into(),
                    ),
            }
            .map_err(Into::into)
        }

        (Method::POST, "/task") => {
            if h_parts
                .headers
                .get(header::CONTENT_TYPE)
                .map(|x| x == "application/json")
                != Some(true)
            {
                return Response::builder()
                    .status(StatusCode::UNSUPPORTED_MEDIA_TYPE)
                    .body(Body::empty())
                    .map_err(Into::into);
            }
            let bbytes = hyper::body::to_bytes(h_body).await?;
            let item: yzix_core::WorkItem = match serde_json::from_slice(&bbytes[..]) {
                Ok(x) => x,
                Err(e) => {
                    return Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(
                            json!({ "errors": [ {
                                "type": "request.invalid.format",
                                "f": format!("Request isn't valid JSON: {}", e),
                            } ] })
                            .to_string()
                            .into(),
                        )
                        .map_err(Into::into)
                }
            };
            drop(bbytes);
            use futures_util::StreamExt as _;

            let (log_s, log_r) = mpsc::channel(1000);
            let _ = sbenv.clone().submit_task(item, Some(log_s.clone())).await;

            let bstream = tokio_stream::wrappers::ReceiverStream::new(log_r)
                .map(|tbr| match &*tbr {
                    TaskBoundResponse::BuildSuccess(outs) => json!({
                        "outputs": outs,
                    })
                    .to_string(),
                    TaskBoundResponse::BuildError(e) => match e {
                        BuildError::Store(yzix_core::StoreError { real_path, kind }) => json!({
                            "errors": [ {
                                "type": kind.errtype(),
                                "f": e.to_string(),
                                "path": real_path.display().to_string(),
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
                    }
                    .to_string(),
                    TaskBoundResponse::Log(s) => format!(".{}\n", s),
                })
                .map(|i| Ok(hyper::body::Bytes::copy_from_slice(i.as_bytes())));

            Response::builder()
                .status(StatusCode::ACCEPTED)
                .header(header::CONTENT_TYPE, "text/x-yzix-task-log")
                .body(
                    (Box::new(bstream)
                        as Box<
                            dyn futures_util::Stream<
                                    Item = Result<
                                        hyper::body::Bytes,
                                        Box<dyn std::error::Error + Send + Sync + 'static>,
                                    >,
                                > + Send
                                + 'static,
                        >)
                        .into(),
                )
                .map_err(Into::into)
        }

        (Method::HEAD | Method::GET | Method::PUT, i) => {
            let mut j = i.split('/').collect::<Vec<_>>();
            if j.first() != Some(&"") {
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Body::empty())
                    .map_err(Into::into);
            }
            j.remove(0);
            if j.first() == Some(&"store") && j.get(2) == Some(&"thintree") && j.get(3).is_none() {
                let j_ = j.get(1).unwrap().to_string();
                drop(j);
                handle_store_thintree(sbenv.clone(), h_parts.method, j_, h_parts.headers, h_body)
                    .await
                    .map_err(Into::into)
            } else if j.first() == Some(&"realisations") {
                j.remove(0);
                handle_realisations(h_parts.method, j, h_parts.headers, h_body).await
            } else if h_parts.method == Method::PUT {
                // TODO: upload of regular files
                Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(Body::empty())
                    .map_err(Into::into)
            } else {
                let resolved = hyper_staticfile::resolve_path(config.store_path.clone(), i).await?;

                let is_exe = match &resolved {
                    hyper_staticfile::ResolveResult::Found(_, meta, _) => {
                        std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0
                    }
                    _ => false,
                };

                let mut resp = hyper_staticfile::ResponseBuilder::new()
                    .request_parts(&h_parts.method, &h_parts.uri, &h_parts.headers)
                    // cache response for 7 days
                    .cache_headers(Some(604800))
                    .build(resolved)?;

                if is_exe {
                    // we need to add the `X-Executable` header if the file is executable
                    resp.headers_mut()
                        .insert("x-executable", header::HeaderValue::from_static("true"));
                }
                Ok(resp)
            }
        }

        (_, "/task") => Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header(header::ALLOW, "POST")
            .body(Body::empty())
            .map_err(Into::into),

        (_, _) => Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header(header::ALLOW, "HEAD, GET, PUT")
            .body(Body::empty())
            .map_err(Into::into),
    }
}
