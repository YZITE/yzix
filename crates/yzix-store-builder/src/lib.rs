#![forbid(
    clippy::as_conversions,
    clippy::cast_ptr_alignment,
    clippy::let_underscore_drop,
    trivial_casts,
    unconditional_recursion,
    unsafe_code
)]

pub use camino::{Utf8Path, Utf8PathBuf};
use std::collections::{BTreeMap, HashSet};
use std::mem::drop;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::{block_in_place, spawn_blocking};
use tracing::{debug, error, span, trace, warn, Level};
use tracing_futures::Instrument as _;
use yzix_pool::Pool;
use yzix_proto_core::{Dump, DumpFlags, StoreError, StoreHash, TaskBoundResponse};

mod fwi;
use fwi::FullWorkItem;
pub mod in2_helpers;
pub mod store_refs;
mod utils;
use utils::*;

pub const INPUT_REALISATION_DIR_POSTFIX: &str = ".in";

pub type TaskId = StoreHash;

pub enum ControlMessage {
    Kill {
        task_id: TaskId,
        answ_chan: oneshot::Sender<bool>,
    },
    SubmitTask {
        item: yzix_proto_core::WorkItem,
        subscribe: Option<mpsc::Sender<(TaskId, Arc<TaskBoundResponse>)>>,
        answ_chan: oneshot::Sender<TaskId>,
    },
    Upload {
        dump: Dump,
        answ_chan: oneshot::Sender<Result<(), StoreError>>,
    },
    HasOutHash {
        outhash: StoreHash,
        answ_chan: oneshot::Sender<bool>,
    },
    Download {
        outhash: StoreHash,
        answ_chan: oneshot::Sender<Result<Dump, StoreError>>,
    },
}

pub struct Args {
    // container runner executable name or path
    pub container_runner: String,

    // amount of job slots
    pub parallel_job_cnt: usize,

    // path to store
    pub store_path: Utf8PathBuf,

    // primary control channel
    pub ctrl_r: mpsc::Receiver<ControlMessage>,
}

// collection of all variables shared between all tasks
struct Env {
    container_runner: String,
    store_path: Utf8PathBuf,

    // cached store references, used to speed up closure calculatioon
    cache_store_refs: std::sync::Mutex<store_refs::Cache>,

    // outhash-locking, to prevent racing in the store
    store_locks: std::sync::Mutex<HashSet<StoreHash>>,

    // inhash-locking, to prevent racing a workitem with itself
    tasks: tokio::sync::Mutex<BTreeMap<StoreHash, Task>>,
}

struct Task {
    handle: tokio::task::JoinHandle<()>,
    logs: broadcast::Sender<Arc<TaskBoundResponse>>,
}

struct HandleSubmitTaskEnv {
    parent: Arc<Env>,
    containerpool: Pool<String>,

    inpath: Utf8PathBuf,
    item: FullWorkItem,
    subscribe: Option<mpsc::Sender<(TaskId, Arc<TaskBoundResponse>)>>,
}

async fn handle_subscribe(
    task_id: TaskId,
    mut logr: broadcast::Receiver<Arc<TaskBoundResponse>>,
    subscribe: mpsc::Sender<(TaskId, Arc<TaskBoundResponse>)>,
) {
    use broadcast::error::RecvError as Rerr;
    loop {
        let x = match logr.recv().await {
            Ok(x) => x,
            Err(Rerr::Closed) => break,
            Err(Rerr::Lagged(_)) => Arc::new(TaskBoundResponse::Log(
                "*** log lagged, some messages have been lost ***".to_string(),
            )),
        };
        if subscribe.send((task_id, x)).await.is_err() {
            break;
        }
    }
}

// this function is split out from the main loop to prevent too much right-ward drift
// and make the code easier to read.
async fn handle_submit_task(
    HandleSubmitTaskEnv {
        parent,
        containerpool,

        inpath,
        mut item,
        subscribe,
    }: HandleSubmitTaskEnv,
) {
    let inhash = item.inhash;
    let parent2 = parent.clone();
    let (logs, logr) = broadcast::channel::<Arc<TaskBoundResponse>>(10000);
    let logs2 = logs.clone();
    // delay the start of the task so that `tasks` remains consistent
    let hold = Arc::new(tokio::sync::Notify::new());
    let hold2 = hold.clone();
    let handle = tokio::spawn(
        (async move {
            // task should get registered
            hold.notified().await;
            drop(hold);
            trace!("determine store closure...");
            // TODO: how should we handle missing store paths?
            block_in_place(|| {
                store_refs::determine_store_closure(
                    &parent.store_path,
                    &mut parent
                        .cache_store_refs
                        .lock()
                        .expect("unable to lock store ref cache"),
                    &mut item.refs,
                )
            });
            let res = {
                trace!("acquire container...");
                let containername = containerpool.get().await;
                trace!("start build in container {}", *containername);
                let span =
                    span!(Level::ERROR, "handle_process", ?item.inner.args, ?item.inner.envs);
                handle_process(
                    HandleProcessArgs {
                        env: &*parent,
                        container_name: &*containername,
                        logs: logs2.clone(),
                    },
                    item,
                )
                .instrument(span)
                .await
            };
            trace!(
                "build finished ({})",
                if res.is_ok() { "successful" } else { "failed" }
            );
            let parent2 = parent.clone();
            let msg = match res {
                Ok(x) => {
                    tokio::task::spawn_blocking(move || {
                        let mut ret = BTreeMap::new();
                        for (outname, (outhash, dump)) in x {
                            let realhash = StoreHash::hash_complex::<Dump>(&dump);
                            let span = span!(Level::ERROR, "output", %outname, %outhash, %realhash);
                            let _guard = span.enter();
                            let realdstpath = parent
                                .store_path
                                .join(&realhash.to_string())
                                .into_std_path_buf();
                            if parent.store_locks.lock().unwrap().insert(realhash)
                                && !realdstpath.exists()
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
                                    parent.store_locks.lock().unwrap().remove(&realhash);
                                    return TaskBoundResponse::BuildError(e.into());
                                }
                                parent.store_locks.lock().unwrap().remove(&realhash);
                            } else {
                                debug!("output already present");
                            }
                            if realhash != outhash {
                                debug!("create symlink to handle self-references");
                                let dstpath = parent
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
            drop::<Result<_, _>>(logs2.send(Arc::new(msg)));
            parent2.tasks.lock().await.remove(&inhash);
        })
        .instrument(span!(Level::ERROR, "job", %inhash)),
    );
    parent2
        .tasks
        .lock()
        .await
        .insert(inhash, Task { handle, logs });
    if let Some(x) = subscribe {
        tokio::spawn(handle_subscribe(inhash, logr, x));
    }
    hold2.notify_one();
}

pub async fn main(
    Args {
        container_runner,
        mut ctrl_r,
        parallel_job_cnt: cpucnt,
        store_path,
    }: Args,
) {
    assert_ne!(cpucnt, 0);

    std::fs::create_dir_all(&store_path).expect("unable to create store dir");

    // setup pools
    let containerpool = Pool::<String>::default();

    for _ in 0..cpucnt {
        containerpool.push(format!("yzix-{}", random_name())).await;
    }

    let env = Arc::new(Env {
        store_path,
        container_runner,

        cache_store_refs: std::sync::Mutex::new(store_refs::Cache::new(1000)),
        store_locks: Default::default(),
        tasks: Default::default(),
    });

    trace!("ready");
    use ControlMessage as Cm;

    // main loop
    while let Some(req) = ctrl_r.recv().await {
        match req {
            Cm::Kill { task_id, answ_chan } => {
                use std::collections::btree_map::Entry;
                let resp = match env.tasks.lock().await.entry(task_id) {
                    Entry::Occupied(occ) => {
                        let ent = occ.remove();
                        ent.handle.abort();
                        let _ = ent
                            .logs
                            .send(Arc::new(TaskBoundResponse::BuildError(
                                yzix_proto_core::BuildError::KilledByClient,
                            )))
                            .is_err();
                        true
                    }
                    Entry::Vacant(_) => false,
                };
                let _ = answ_chan.send(resp).is_err();
            }
            Cm::SubmitTask {
                item: pre_item,
                subscribe,
                answ_chan,
            } => {
                let item = block_in_place(|| FullWorkItem::new(pre_item, &env.store_path));
                let inhash = item.inhash;
                if let Some(apt) = env.tasks.lock().await.get(&inhash) {
                    if let Some(x) = subscribe {
                        tokio::spawn(handle_subscribe(inhash, apt.logs.subscribe(), x));
                    }
                } else {
                    let inpath = env
                        .store_path
                        .join(format!("{}{}", inhash, INPUT_REALISATION_DIR_POSTFIX));
                    let ex_outputs = in2_helpers::resolve_in2(inpath.as_std_path());
                    if !ex_outputs.is_empty() {
                        if let Some(x) = subscribe {
                            tokio::spawn(async move {
                                let _ = x
                                    .send((
                                        inhash,
                                        Arc::new(TaskBoundResponse::BuildSuccess(ex_outputs)),
                                    ))
                                    .await
                                    .is_ok();
                            });
                        }
                    } else {
                        handle_submit_task(HandleSubmitTaskEnv {
                            parent: env.clone(),
                            containerpool: containerpool.clone(),

                            inpath,
                            item,
                            subscribe,
                        })
                        .await;
                    }
                }
                let _ = answ_chan.send(inhash).is_err();
            }
            Cm::Upload { dump, answ_chan } => {
                let env = env.clone();
                spawn_blocking(move || {
                    let h = StoreHash::hash_complex::<Dump>(&dump);
                    let res = if !env.store_locks.lock().unwrap().insert(h) {
                        // maybe return ResourceBusy when rust#86442 is fixed/stable
                        Ok(())
                    } else {
                        let p = env.store_path.join(h.to_string());
                        let ret = if p.exists() {
                            // TODO: auto repair
                            Ok(())
                        } else if let Err(e) = dump.write_to_path(
                            p.as_std_path(),
                            DumpFlags {
                                force: false,
                                make_readonly: true,
                            },
                        ) {
                            Err(e)
                        } else {
                            Ok(())
                        };
                        env.store_locks.lock().unwrap().remove(&h);
                        ret
                    };
                    let _ = answ_chan.send(res).is_err();
                });
            }
            Cm::HasOutHash { outhash, answ_chan } => {
                let _ = answ_chan
                    .send(env.store_path.join(outhash.to_string()).exists())
                    .is_err();
            }
            Cm::Download { outhash, answ_chan } => {
                let env = env.clone();
                spawn_blocking(move || {
                    // if something currently tries to insert the path, we can't download it yet.
                    let real_path = env.store_path.join(outhash.to_string()).into_std_path_buf();
                    let res = if !matches!(
                        env.store_locks.lock().map(|i| i.contains(&outhash)),
                        Ok(false)
                    ) {
                        Err(StoreError {
                            real_path,
                            kind: std::io::Error::new(
                                std::io::ErrorKind::WouldBlock,
                                "path is currently locked",
                            )
                            .into(),
                        })
                    } else {
                        Dump::read_from_path(&real_path)
                    };
                    let _ = answ_chan.send(res).is_err();
                });
            }
        }
    }
}
