pub use camino::Utf8PathBuf;
use std::collections::BTreeMap;
use std::mem::drop;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tokio::task::block_in_place;
use tracing::{debug, error, span, trace, warn, Level};
use tracing_futures::Instrument as _;
use yzix_core::{DumpFlags, TaggedHash, ThinTree};

use crate::fwi::FullWorkItem;
use crate::handle_process::{handle_process, HandleProcessArgs};
use crate::{
    in2_helpers, store_refs, Env, Task, TaskBoundResponse, TaskId, INPUT_REALISATION_DIR_POSTFIX,
};

pub struct HandleSubmitTaskEnv {
    pub parent: Arc<Env>,
    pub inpath: Utf8PathBuf,
    pub item: FullWorkItem,
    pub subscribe: Option<mpsc::Sender<Arc<TaskBoundResponse>>>,
}

async fn handle_subscribe(
    mut logr: broadcast::Receiver<Arc<TaskBoundResponse>>,
    subscribe: mpsc::Sender<Arc<TaskBoundResponse>>,
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
        if subscribe.send(x).await.is_err() {
            break;
        }
    }
}

use crate::mk_request_cafile;

// this function is split out from the main loop to prevent too much right-ward drift
// and make the code easier to read.
pub async fn handle_submit_task(
    HandleSubmitTaskEnv {
        parent,
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
                let containername = parent.container_pool.get().await;
                trace!("start build in container {}", *containername);
                let span =
                    span!(Level::ERROR, "handle_process", ?item.inner.args, ?item.inner.envs);
                handle_process(
                    HandleProcessArgs {
                        env: &parent,
                        container_name: &containername,
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
                            let realhash = TaggedHash::<ThinTree>::hash_complex(&dump);
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
                                    &mk_request_cafile(&parent.store_path),
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
        tokio::spawn(handle_subscribe(logr, x));
    }
    hold2.notify_one();
}

impl Env {
    pub async fn submit_task(
        self: Arc<Self>,
        pre_item: yzix_core::WorkItem,
        subscribe: Option<mpsc::Sender<Arc<TaskBoundResponse>>>,
    ) -> TaskId {
        let item = block_in_place(|| FullWorkItem::new(pre_item, &self.store_path));
        let inhash = item.inhash;
        if let Some(apt) = self.tasks.lock().await.get(&inhash) {
            if let Some(x) = subscribe {
                tokio::spawn(handle_subscribe(apt.logs.subscribe(), x));
            }
            return inhash;
            // we can't use an `else` block below because if we would do that,
            // the lock taken above wouldn't be released before potentially
            // `handle_submit_task` is entered, which would then deadlock.
        }
        let inpath = self
            .store_path
            .join(format!("{}{}", inhash, INPUT_REALISATION_DIR_POSTFIX));
        let ex_outputs = in2_helpers::resolve_in2(inpath.as_std_path());
        if !ex_outputs.is_empty() {
            if let Some(x) = subscribe {
                tokio::spawn(async move {
                    let _ = x
                        .send(Arc::new(TaskBoundResponse::BuildSuccess(ex_outputs)))
                        .await
                        .is_ok();
                });
            }
        } else {
            handle_submit_task(HandleSubmitTaskEnv {
                parent: self,
                inpath,
                item,
                subscribe,
            })
            .await;
        }
        inhash
    }
}
