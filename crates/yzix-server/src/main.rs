#![forbid(
    clippy::as_conversions,
    clippy::cast_ptr_alignment,
    clippy::let_underscore_drop,
    trivial_casts,
    unconditional_recursion,
    unsafe_code
)]

use camino::Utf8PathBuf;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::mem::drop;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::block_in_place;
use tracing::{debug, error, info, span, trace, warn, Level};
use tracing_futures::Instrument as _;
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

enum TaskAction {
    Insert(Task),
    Remove,
}

struct HandleSubmitTaskEnv {
    config: Arc<ServerConfig>,
    containerpool: Pool<String>,
    store_locks: Arc<std::sync::Mutex<HashSet<StoreHash>>>,
    tasks_s: mpsc::UnboundedSender<(StoreHash, TaskAction)>,

    inpath: Utf8PathBuf,
    item: FullWorkItem,
    subscribe: Option<clients::SubscribeChan>,
}

// this function is split out from the main loop to prevent too much right-ward drift
// and make the code easier to read.
async fn handle_submit_task(
    HandleSubmitTaskEnv {
        config,
        containerpool,
        store_locks,
        tasks_s,

        inpath,
        mut item,
        subscribe,
    }: HandleSubmitTaskEnv,
) {
    let inhash = item.inhash;
    let (logs, logr) = broadcast::channel(10000);
    let logs2 = logs.clone();
    // delay the start of the task so that `tasks` remains consistent
    let hold = Arc::new(tokio::sync::Notify::new());
    let hold2 = hold.clone();
    let tasks2_s = tasks_s.clone();
    let handle = tokio::spawn(
        (async move {
            // task should get registered
            hold.notified().await;
            drop(hold);
            trace!("determine store closure...");
            // TODO: how should we handle missing store paths?
            block_in_place(|| determine_store_closure(&config.store_path, &mut item.refs));
            let res = {
                trace!("acquire container...");
                let containername = containerpool.get().await;
                trace!("start build in container {}", *containername);
                let span =
                    span!(Level::ERROR, "handle_process", ?item.inner.args, ?item.inner.envs);
                handle_process(&*config, logs2.clone(), &*containername, item)
                    .instrument(span)
                    .await
            };
            trace!(
                "build finished ({})",
                if res.is_ok() { "successful" } else { "failed" }
            );
            let msg = match res {
                Ok(x) => {
                    tokio::task::spawn_blocking(move || {
                        let mut ret = BTreeMap::new();
                        for (outname, (outhash, dump)) in x {
                            let realhash = StoreHash::hash_complex::<Dump>(&dump);
                            let span = span!(Level::ERROR, "output", %outname, %outhash, %realhash);
                            let _guard = span.enter();
                            let realdstpath = config
                                .store_path
                                .join(&realhash.to_string())
                                .into_std_path_buf();
                            if store_locks.lock().unwrap().insert(realhash) && !realdstpath.exists()
                            {
                                debug!("dumping to store ...");
                                if let Err(e) = dump.write_to_path(
                                    &realdstpath,
                                    DumpFlags {
                                        force: true,
                                        make_readonly: true,
                                    },
                                ) {
                                    error!("dumping to store failed: {}", e);
                                    store_locks.lock().unwrap().remove(&realhash);
                                    return TaskBoundResponse::BuildError(e.into());
                                }
                                store_locks.lock().unwrap().remove(&realhash);
                            } else {
                                debug!("output already present");
                            }
                            if realhash != outhash {
                                debug!("create symlink to handle self-references");
                                let dstpath = config
                                    .store_path
                                    .join(&outhash.to_string())
                                    .into_std_path_buf();
                                // we want to create a relative symlink
                                let realdstpath = std::path::PathBuf::from(realhash.to_string());
                                // TODO: handle mismatching symlink targets
                                use std::io::{Error, ErrorKind};
                                if let Err(e) = std::os::unix::fs::symlink(&realdstpath, &dstpath) {
                                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                                        match std::fs::read_link(&dstpath) {
                                            Ok(oldtrg) if oldtrg == realdstpath => {}
                                            Ok(orig_target) => {
                                                error!(
                                                    ?orig_target,
                                                    ?realdstpath,
                                                    "self-ref CA path differs"
                                                );
                                                return TaskBoundResponse::BuildError(
                                                    Error::from(ErrorKind::AlreadyExists).into(),
                                                );
                                            }
                                            Err(e) => {
                                                error!(
                                                    "checking self-reference symlink failed: {}",
                                                    e
                                                );
                                                return TaskBoundResponse::BuildError(e.into());
                                            }
                                        }
                                    } else {
                                        error!("creating self-reference symlink failed: {}", e);
                                        return TaskBoundResponse::BuildError(e.into());
                                    }
                                }
                            }
                            // avoid unnecessary indirection by always using the CA path
                            ret.insert(outname, realhash);
                        }

                        // register realisation
                        in2_helpers::create_in2_symlinks(inpath.as_std_path(), &ret);
                        TaskBoundResponse::BuildSuccess(ret)
                    })
                    .await
                    .unwrap()
                }
                Err(e) => {
                    warn!("build failed with error {}", e);
                    TaskBoundResponse::BuildError(e)
                }
            };
            trace!("send result {:?}", msg);
            drop::<Result<_, _>>(logs2.send((inhash, Arc::new(msg))));
            trace!("schedule job cleanup");
            drop::<Result<_, _>>(tasks_s.send((inhash, TaskAction::Remove)));
        })
        .instrument(span!(Level::ERROR, "job", %inhash)),
    );
    drop::<Result<_, _>>(tasks2_s.send((inhash, TaskAction::Insert(Task { handle, logs }))));
    if let Some(x) = subscribe {
        drop(x.send(logr).await);
    }
    hold2.notify_one();
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // reset all environment variables before invoking any .await
    // this is necessary to avoid unnecessary query syscalls
    {
        use std::env;
        for (key, _) in env::vars_os() {
            if key == "RUST_LOG" {
                continue;
            }
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
        match toml::de::from_slice(
            &std::fs::read(arg).expect("unable to read supplied config file"),
        ) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("yzix-server: CONFIG ERROR: {}", e);
                std::process::exit(1);
            }
        }
    };
    let config = Arc::new(config);

    // validate config

    if config.store_path == camino::Utf8Path::new("") {
        eprintln!("yzix-server: CONFIG ERROR: store_path is invalid");
        std::process::exit(1);
    }

    if config.container_runner.is_empty() {
        eprintln!("yzix-server: CONFIG ERROR: container_runner is invalid");
        std::process::exit(1);
    }

    if config.socket_bind.is_empty() {
        eprintln!("yzix-server: CONFIG ERROR: socket_bind is invalid");
        std::process::exit(1);
    }

    if config.bearer_tokens.is_empty() {
        eprintln!("yzix-server: CONFIG ERROR: bearer_tokens is empty");
        std::process::exit(1);
    }

    // continue setup

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

    let (client_reqs, mut client_reqr) = mpsc::unbounded_channel();

    // inhash-locking, to prevent racing a workitem with itself
    let (tasks_s, mut tasks_r) = mpsc::unbounded_channel();
    let mut tasks = HashMap::<StoreHash, Task>::new();

    // outhash-locking, to prevent racing in the store
    let store_locks = Arc::new(std::sync::Mutex::new(HashSet::<StoreHash>::new()));

    trace!("ready");

    // main loop
    loop {
        tokio::select! {
            biased;

            _ = &mut shutdown_r => break,

            client = listener.accept() => {
                match client {
                    Ok((stream, addr)) => {
                        info!("new connection from {:?}", addr);
                        tokio::spawn(clients::handle_client(
                            Arc::clone(&config),
                            client_reqs.clone(),
                            stream,
                        ));
                    }
                    Err(e) => {
                        error!("listener'accept() failed: {:?}", e);
                    }
                }
            },

            Some(clients::Request { inner, resp }) = client_reqr.recv() => {
                use clients::RequestKind as Rk;
                let res = match inner {
                    Rk::Kill(tid) => {
                        use std::collections::hash_map::Entry;
                        match tasks.entry(tid) {
                            Entry::Occupied(occ) => {
                                let ent = occ.remove();
                                ent.handle.abort();
                                drop::<Result<_, _>>(ent.logs.send((tid, Arc::new(
                                    TaskBoundResponse::BuildError(yzix_proto::BuildError::KilledByClient),
                                ))));
                                Response::Ok
                            },
                            Entry::Vacant(_) => Response::False,
                        }
                    },
                    Rk::SubmitTask { item, subscribe } => {
                        let inhash = item.inhash;
                        if let Some(apt) = tasks.get(&inhash) {
                            if let Some(x) = subscribe {
                                drop(x.send(apt.logs.subscribe()).await);
                            }
                        } else {
                            let inpath = config
                                .store_path
                                .join(format!("{}{}", inhash, INPUT_REALISATION_DIR_POSTFIX));
                            let ex_outputs = in2_helpers::resolve_in2(inpath.as_std_path());
                            if !ex_outputs.is_empty() {
                                if let Some(x) = subscribe {
                                    tokio::spawn(async move {
                                        let (s, r) = broadcast::channel(2);
                                        drop::<Result<_, _>>(s.send((
                                            item.inhash,
                                            Arc::new(TaskBoundResponse::BuildSuccess(ex_outputs)),
                                        )));
                                        drop(x.send(r).await);
                                    });
                                }
                            } else {
                                handle_submit_task(HandleSubmitTaskEnv {
                                    config: config.clone(),
                                    store_locks: store_locks.clone(),
                                    containerpool: containerpool.clone(),
                                    tasks_s: tasks_s.clone(),

                                    inpath,
                                    item,
                                    subscribe,
                                }).await;
                            }
                        }
                        Response::TaskBound(
                            inhash,
                            TaskBoundResponse::Queued,
                        )
                    },
                    Rk::Upload(d) => {
                        let h = StoreHash::hash_complex::<Dump>(&d);
                        if tasks.contains_key(&h) {
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
                        }
                    },
                    Rk::HasOutHash(h) => {
                        if config.store_path.join(h.to_string()).exists() {
                            Response::Ok
                        } else {
                            Response::False
                        }
                    },
                    Rk::Download(h) => {
                        let dump = block_in_place(|| Dump::read_from_path(
                            config.store_path.join(h.to_string()).as_std_path()
                        ));
                        match dump {
                            Ok(dump) => Response::Dump(dump),
                            Err(e) => Response::TaskBound(
                                h,
                                TaskBoundResponse::BuildError(e.into())
                            ),
                        }
                    },
                };
                drop(resp.send(res).await);
            },

            Some((tid, act)) = tasks_r.recv() => {
                match act {
                    TaskAction::Remove => {
                        debug!("reaping task {}", tid);
                        tasks.remove(&tid);
                    }
                    TaskAction::Insert(tdat) => {
                        debug!("registering task {}", tid);
                        tasks.insert(tid, tdat);
                    }
                }
            },
        }
    }
}
