#![forbid(
    clippy::as_conversions,
    clippy::cast_ptr_alignment,
    clippy::let_underscore_drop,
    trivial_casts,
    unconditional_recursion,
    unsafe_code
)]

pub use camino::{Utf8Path, Utf8PathBuf};
use core::num::NonZeroUsize;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::trace;
use yzix_core::{BuildError, DumpFlags, Regular, StoreError, StoreErrorKind, TaggedHash, ThinTree};

mod fwi;
use fwi::FullWorkItem;
mod handle_process;
mod handle_submit_task;
pub mod in2_helpers;
mod pool;
use pool::Pool;
pub mod store_refs;

pub const INPUT_REALISATION_DIR_POSTFIX: &str = ".in";
pub const CAFILE_SUBDIR_NAME: &str = ".links";

pub type TaskId = TaggedHash<yzix_core::WorkItem>;

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum TaskBoundResponse {
    BuildSuccess(BTreeMap<yzix_core::OutputName, TaggedHash<ThinTree>>),
    Log(String),
    BuildError(BuildError),
}

impl TaskBoundResponse {
    #[inline]
    pub fn task_finished(&self) -> bool {
        matches!(self, Self::BuildSuccess(_) | Self::BuildError(_))
    }
}

impl From<BuildError> for TaskBoundResponse {
    #[inline(always)]
    fn from(x: BuildError) -> Self {
        TaskBoundResponse::BuildError(x)
    }
}

pub trait OnObject: Sized {
    fn upload(env: &Env, hash: TaggedHash<Self>, data: Self) -> Result<(), StoreError>;
    fn download(env: &Env, hash: TaggedHash<Self>) -> Result<Self, StoreError>;
    fn is_present(env: &Env, hash: TaggedHash<Self>) -> bool;
}

// collection of all variables shared between all tasks
pub struct Env {
    container_runner: String,
    store_path: Utf8PathBuf,

    container_pool: Pool<String>,

    // cached store references, used to speed up closure calculatioon
    cache_store_refs: std::sync::Mutex<store_refs::Cache>,

    // outhash-locking, to prevent racing in the store
    store_locks: std::sync::Mutex<HashSet<TaggedHash<ThinTree>>>,

    // outhash-locking for the non-tree part
    store_cafiles_locks: std::sync::Mutex<HashSet<TaggedHash<Regular>>>,

    // inhash-locking, to prevent racing a workitem with itself
    tasks: tokio::sync::Mutex<BTreeMap<TaskId, Task>>,
}

struct Task {
    handle: tokio::task::JoinHandle<()>,
    logs: broadcast::Sender<Arc<TaskBoundResponse>>,
}

fn random_name() -> String {
    use rand::prelude::*;
    let mut rng = rand::thread_rng();
    std::iter::repeat(())
        .take(20)
        .map(|()| char::from_u32(rng.gen_range(b'a'..=b'z').into()).unwrap())
        .collect::<String>()
}

fn mk_request_cafile(
    store_path: &Utf8Path,
) -> impl (Fn(TaggedHash<Regular>) -> Result<std::path::PathBuf, StoreError>) + '_ {
    move |rh: TaggedHash<Regular>| {
        let mut x = store_path.join(CAFILE_SUBDIR_NAME);
        x.push(rh.as_ref().to_string());
        Ok(x.into_std_path_buf())
    }
}

fn register_cafile(
    store_path: &Utf8Path,
    store_cafiles_locks: &std::sync::Mutex<HashSet<TaggedHash<Regular>>>,
    hash: TaggedHash<Regular>,
    regu: Regular,
) -> Result<(), yzix_core::StoreError> {
    if !store_cafiles_locks.lock().unwrap().insert(hash) {
        return Ok(());
    }

    let p = (mk_request_cafile(store_path))(hash).unwrap();
    let ret = if !p.exists() {
        regu.write_to_path(
            &p,
            yzix_core::RegularFlags {
                skip_write: false,
                make_readonly: true,
            },
        )
    } else {
        Ok(())
    };

    store_cafiles_locks.lock().unwrap().remove(&hash);
    ret
}

impl Env {
    pub async fn new(
        store_path: Utf8PathBuf,
        container_runner: String,
        parallel_job_cnt: usize,
    ) -> Self {
        assert_ne!(parallel_job_cnt, 0);

        std::fs::create_dir_all(&store_path).expect("unable to create store dir");
        std::fs::create_dir_all(&store_path.join(CAFILE_SUBDIR_NAME))
            .expect("unable to create store/.links dir");

        // setup pools
        let container_pool = Pool::<String>::default();

        for _ in 0..parallel_job_cnt {
            container_pool.push(format!("yzix-{}", random_name())).await;
        }

        let env = Env {
            store_path,
            container_runner,
            container_pool,

            cache_store_refs: std::sync::Mutex::new(store_refs::Cache::new(
                NonZeroUsize::new(1000).unwrap(),
            )),
            store_locks: Default::default(),
            store_cafiles_locks: Default::default(),
            tasks: Default::default(),
        };

        trace!("ready");
        env
    }

    pub async fn kill(&self, task_id: TaskId) -> bool {
        use std::collections::btree_map::Entry;
        match self.tasks.lock().await.entry(task_id) {
            Entry::Occupied(occ) => {
                let ent = occ.remove();
                ent.handle.abort();
                let _ = ent
                    .logs
                    .send(Arc::new(TaskBoundResponse::BuildError(
                        yzix_core::BuildError::KilledByClient,
                    )))
                    .is_err();
                true
            }
            Entry::Vacant(_) => false,
        }
    }
}

impl OnObject for Regular {
    fn is_present(env: &Env, hash: TaggedHash<Self>) -> bool {
        (mk_request_cafile(&env.store_path))(hash).unwrap().exists()
    }

    fn upload(env: &Env, hash: TaggedHash<Self>, data: Self) -> Result<(), StoreError> {
        let h = TaggedHash::<Regular>::hash_complex(&data);
        let real_path = (mk_request_cafile(&env.store_path))(h).unwrap();
        if h != hash {
            return Err(StoreError {
                real_path,
                kind: StoreErrorKind::HashMismatch,
            });
        }
        if !env.store_cafiles_locks.lock().unwrap().insert(h) {
            Err(StoreError {
                real_path,
                kind: std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "path is currently locked",
                )
                .into(),
            })
        } else {
            let res = if real_path.exists() {
                // TODO: auto repair
                Ok(())
            } else {
                Regular::write_to_path(
                    &data,
                    &real_path,
                    yzix_core::RegularFlags {
                        skip_write: false,
                        make_readonly: true,
                    },
                )
            };
            env.store_cafiles_locks.lock().unwrap().remove(&h);
            res
        }
    }

    fn download(env: &Env, hash: TaggedHash<Self>) -> Result<Self, StoreError> {
        let real_path = (mk_request_cafile(&env.store_path))(hash).unwrap();
        if !matches!(
            env.store_cafiles_locks.lock().map(|i| i.contains(&hash)),
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
            std::fs::symlink_metadata(&real_path)
                .map_err(|e| StoreError {
                    real_path: real_path.clone(),
                    kind: e.into(),
                })
                .and_then(|meta| Regular::read_from_path(&real_path, &meta))
        }
    }
}

impl OnObject for ThinTree {
    fn is_present(env: &Env, hash: TaggedHash<Self>) -> bool {
        env.store_path.join(hash.to_string()).exists()
    }

    fn upload(env: &Env, hash: TaggedHash<Self>, mut data: Self) -> Result<(), StoreError> {
        data.submit_all_inlines(&mut |rh, regu| {
            register_cafile(&env.store_path, &env.store_cafiles_locks, rh, regu)
        })?;

        let h = TaggedHash::<ThinTree>::hash_complex(&data);
        let real_path = env.store_path.join(h.to_string()).into_std_path_buf();

        if h != hash {
            return Err(StoreError {
                real_path,
                kind: StoreErrorKind::HashMismatch,
            });
        }

        if !env.store_locks.lock().unwrap().insert(h) {
            // maybe return ResourceBusy when rust#86442 is fixed/stable
            return Err(StoreError {
                real_path,
                kind: std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "path is currently locked",
                )
                .into(),
            });
        }
        let ret = if real_path.exists() {
            // TODO: auto repair
            Ok(())
        } else if let Err(e) = data.write_to_path(
            &real_path,
            DumpFlags {
                force: false,
                make_readonly: true,
            },
            &mk_request_cafile(&env.store_path),
        ) {
            Err(e)
        } else {
            Ok(())
        };
        env.store_locks.lock().unwrap().remove(&h);
        ret
    }

    fn download(env: &Env, hash: TaggedHash<Self>) -> Result<Self, StoreError> {
        // if something currently tries to insert the path, we can't download it yet.
        let real_path = env.store_path.join(hash.to_string()).into_std_path_buf();
        if !matches!(env.store_locks.lock().map(|i| i.contains(&hash)), Ok(false)) {
            return Err(StoreError {
                real_path,
                kind: std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "path is currently locked",
                )
                .into(),
            });
        }
        let rqcf = mk_request_cafile(&env.store_path);
        ThinTree::read_from_path(&real_path, &mut |rh, regu| {
            if rqcf(rh).map(|i| i.exists()) == Ok(true) {
                Ok(())
            } else {
                Err(yzix_core::ThinTreeSubmitError::StoreLoop(regu))
            }
        })
    }
}
