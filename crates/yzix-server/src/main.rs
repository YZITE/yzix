use camino::Utf8PathBuf;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::block_in_place;
use tracing::{span, Level};
use yzix_pool::Pool;
use yzix_proto::{
    self, store::Dump, store::Flags as DumpFlags, store::Hash as StoreHash, Response,
    TaskBoundResponse,
};

pub mod clients;
mod fwi;
pub use fwi::FullWorkItem;
pub mod in2_helpers;
mod utils;
pub use utils::*;

pub const INPUT_REALISATION_DIR_POSTFIX: &str = ".in";

#[derive(Debug, serde::Deserialize)]
pub struct ServerConfig {
    store_path: Utf8PathBuf,
    container_runner: String,
    socket_bind: String,
    bearer_tokens: HashSet<String>,
}

pub struct Task {
    handle: tokio::task::JoinHandle<()>,
    logs: broadcast::Sender<(StoreHash, Arc<TaskBoundResponse>)>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // reset all environment variables before invoking any .await
    // this is necessary to avoid unnecessary query syscalls
    {
        use std::env;
        for (key, _) in env::vars_os() {
            env::remove_var(key);
        }
        env::set_var("LC_ALL", "C.UTF-8");
        env::set_var("TZ", "UTC");
    }

    // install global log subscriber configured based on RUST_LOG envvar.
    tracing_subscriber::fmt::init();

    let config: ServerConfig = {
        let mut args = std::env::args().skip(1);
        let inv_invoc = || -> ! {
            eprintln!(
                "yzix-server: ERROR: invalid invocation (supply a config file as only argument)"
            );
            std::process::exit(1);
        };
        let arg = if let Some(x) = args.next() {
            x
        } else {
            inv_invoc();
        };
        if args.next().is_some() || arg == "--help" {
            inv_invoc();
        }
        toml::de::from_slice(&std::fs::read(arg).expect("unable to read supplied config file"))
            .expect("unable to parse supplied config file")
    };
    let config = Arc::new(config);

    std::fs::create_dir_all(&config.store_path).expect("unable to create store dir");

    let cpucnt = num_cpus::get();

    // setup pools
    let containerpool = Pool::<String>::default();

    for _ in 0..cpucnt {
        containerpool.push(format!("yzix-{}", random_name())).await;
    }

    // install Ctrl+C handler
    // this should be invoked as soon as we have something to clean up...
    let (shutdown_s, mut shutdown_r) = oneshot::channel();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.unwrap();
        let _ = shutdown_s.send(());
    });

    let listener = tokio::net::TcpListener::bind(&config.socket_bind)
        .await
        .expect("unable to bind socket");

    let valid_bearer_tokens = Arc::new(config.bearer_tokens.clone());
    let (client_reqs, mut client_reqr) = mpsc::channel(1000);

    // inhash-locking, to prevent racing a workitem with itself
    let (task_clup_s, mut task_clup_r) = mpsc::channel(1000);
    let mut tasks = HashMap::<StoreHash, Task>::new();

    // outhash-locking, to prevent racing in the store
    let store_locks = Arc::new(std::sync::Mutex::new(HashSet::<StoreHash>::new()));

    // main loop
    loop {
        tokio::select! {
            biased;

            _ = &mut shutdown_r => break,

            client = listener.accept() => {
                match client {
                    Ok((stream, addr)) => {
                        tracing::info!("new connection from {:?}", addr);
                        tokio::spawn(clients::handle_client(
                            client_reqs.clone(),
                            stream,
                            Arc::clone(&valid_bearer_tokens),
                        ));
                    }
                    Err(e) => {
                        tracing::error!("listener'accept() failed: {:?}", e);
                    }
                }
            },

            Some(clients::Request { inner, resp }) = client_reqr.recv() => {
                use clients::RequestKind as Rk;
                match inner {
                    Rk::Kill(tid) => {
                        use std::collections::hash_map::Entry;
                        match tasks.entry(tid) {
                            Entry::Occupied(occ) => {
                                let ent = occ.remove();
                                ent.handle.abort();
                                let _: Result<_, _> = ent.logs.send((tid, Arc::new(
                                    TaskBoundResponse::BuildError(yzix_proto::BuildError::KilledByClient),
                                )));
                                let _ = resp.send(Response::Ok).await;
                            },
                            Entry::Vacant(_) => {
                                let _ = resp.send(Response::False).await;
                            },
                        }
                    },
                    Rk::SubmitTask { mut item, subscribe } => {
                        if let Some(apt) = tasks.get(&item.inhash) {
                            if let Some(x) = subscribe {
                                let _ = x.send(apt.logs.subscribe()).await;
                            }
                            continue;
                        }
                        let inhash = item.inhash;
                        let inpath = config.store_path.join(format!("{}{}", inhash, INPUT_REALISATION_DIR_POSTFIX));
                        let ex_outputs = in2_helpers::resolve_in2(inpath.as_std_path());
                        if !ex_outputs.is_empty() {
                            if let Some(x) = subscribe {
                                tokio::spawn(async move {
                                    let (s, r) = broadcast::channel(2);
                                    let _: Result<_, _> = s.send((
                                        item.inhash,
                                        Arc::new(TaskBoundResponse::BuildSuccess(ex_outputs))
                                    ));
                                    let _ = x.send(r).await;
                                });
                            }
                            continue;
                        }
                        let (logs, logr) = broadcast::channel(10000);
                        let config2 = config.clone();
                        let logs2 = logs.clone();
                        let store_locks = store_locks.clone();
                        let task_clup_s = task_clup_s.clone();
                        let containerpool = containerpool.clone();
                        // delay the start of the task so that `tasks` remains consistent
                        let hold = Arc::new(tokio::sync::Notify::new());
                        let hold2 = hold.clone();
                        let handle = tokio::spawn(async move {
                            // task should get registered
                            hold.notified().await;
                            let _ = hold;
                            // TODO: how should we handle missing store paths?
                            block_in_place(|| determine_store_closure(&config2.store_path, &mut item.refs));
                            let res = {
                                let containername = containerpool.get().await;
                                handle_process(&*config2, &logs2, &*containername, item).await
                            };
                            let msg = match res {
                                Ok(x) => {
                                    tokio::task::spawn_blocking(move || {
                                        let span = span!(Level::ERROR, "handling outputs", %inhash);
                                        let _guard = span.enter();
                                        let mut ret = BTreeMap::new();
                                        for (outname, (outhash, dump)) in x {
                                            let span = span!(Level::ERROR, "output", %outname, %outhash);
                                            let _guard = span.enter();
                                            let dstpath = config2
                                                .store_path
                                                .join(&outhash.to_string())
                                                .into_std_path_buf();
                                            // TODO: auto-repair? we can't do that if ( outhash != hash(dump) )
                                            if store_locks.lock().unwrap().insert(outhash) && !dstpath.exists() {
                                                tracing::debug!("dumping to store ...");
                                                if let Err(e) = dump.write_to_path(
                                                    &dstpath,
                                                    DumpFlags {
                                                        force: true,
                                                        make_readonly: true,
                                                    },
                                                ) {
                                                    tracing::error!("{}", e);
                                                    store_locks.lock().unwrap().remove(&outhash);
                                                    return TaskBoundResponse::BuildError(e.into());
                                                }
                                                store_locks.lock().unwrap().remove(&outhash);
                                            } else {
                                                tracing::debug!("output already present");
                                            }
                                            ret.insert(outname, outhash);
                                        }

                                        // register realisation
                                        in2_helpers::create_in2_symlinks(inpath.as_std_path(), &ret);
                                        TaskBoundResponse::BuildSuccess(ret)
                                    }).await.unwrap()
                                },
                                Err(e) => TaskBoundResponse::BuildError(e),
                            };
                            let _: Result<_, _> = logs2.send((inhash, Arc::new(msg)));
                            let _: Result<_, _> = task_clup_s.send(inhash).await;
                        });
                        tasks.insert(inhash, Task {
                            handle,
                            logs,
                        });
                        if let Some(x) = subscribe {
                            let _ = x.send(logr).await;
                        }
                        hold2.notify_one();
                        let _ = resp.send(Response::TaskBound(
                            inhash,
                            TaskBoundResponse::Queued,
                        )).await;
                    },
                    Rk::Upload(d) => {
                        let h = StoreHash::hash_complex::<Dump>(&d);
                        let response = if tasks.contains_key(&h) {
                            Response::False
                        } else {
                            let p = config.store_path.join(h.to_string());
                            if p.exists() {
                                Response::False
                            } else if let Err(e) = block_in_place(|| d.write_to_path(
                                p.as_std_path(),
                                DumpFlags {
                                    force: false,
                                    make_readonly: true,
                                },
                            )) {
                                Response::TaskBound(h, TaskBoundResponse::BuildError(e.into()))
                            } else {
                                Response::Ok
                            }
                        };
                        let _ = resp.send(response).await;
                    },
                    Rk::HasOutHash(h) => {
                        let _ = resp.send(if config.store_path.join(h.to_string()).exists() {
                            Response::Ok
                        } else {
                            Response::False
                        }).await;
                    },
                    Rk::Download(h) => {
                        let dump = block_in_place(|| Dump::read_from_path(
                            config.store_path.join(h.to_string()).as_std_path()
                        ));
                        let _ = resp.send(match dump {
                            Ok(dump) => Response::Dump(dump),
                            Err(e) => Response::TaskBound(
                                h,
                                TaskBoundResponse::BuildError(e.into())
                            ),
                        }).await;
                    },
                }
            },

            Some(tid) = task_clup_r.recv() => {
                tracing::debug!("reaping task {}", tid);
                tasks.remove(&tid);
            },
        }
    }
}
