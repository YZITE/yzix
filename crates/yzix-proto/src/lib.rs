pub use yzix_store as store;
pub use yzix_strwrappers as strwrappers;

use serde::{Deserialize, Serialize};

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct WorkItem {
    pub args: Vec<String>,
    pub envs: BTreeMap<String, String>,
    pub outputs: BTreeSet<strwrappers::OutputName>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct TaskId(pub u32);

// maximum message length
pub type ProtoLen = u32;

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub enum Request {
    UnsubscribeAll,
    LogSub(TaskId, bool),
    Kill(TaskId),
    SubmitTask {
        item: WorkItem,
        auto_subscribe: bool,
    },
    Upload(store::Dump),
    HasOutHash(store::Hash),
    Download(store::Hash),
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub enum Response {
    Ok,
    False,
    LogError,
    OverflowError,
    Dump(store::Dump),
    TaskBound(TaskId, TaskBoundResponse),
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub enum TaskBoundResponse {
    Queued,
    BuildSuccess(BTreeMap<strwrappers::OutputName, store::Hash>),
    Log(String),
    BuildError(BuildError),
}

impl TaskBoundResponse {
    pub fn task_finished(&self) -> bool {
        matches!(self, Self::BuildSuccess(_) | Self::BuildError(_))
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, thiserror::Error)]
pub enum BuildError {
    #[error("command returned with exit code {0}")]
    Exit(i32),

    #[error("command was killed with signal {0}")]
    Killed(i32),

    #[error("server-side I/O error with errno {0}")]
    Io(i32),

    #[error("given command is empty")]
    EmptyCommand,

    #[error("hash collision at {0}")]
    HashCollision(store::Hash),
}
