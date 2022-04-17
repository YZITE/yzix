pub use yzix_store as store;
pub use yzix_strwrappers as strwrappers;

use serde::{Deserialize, Serialize};

use std::collections::{BTreeMap, BTreeSet};

mod codec;
pub use codec::*;

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct WorkItem {
    pub args: Vec<String>,
    pub envs: BTreeMap<String, String>,
    pub outputs: BTreeSet<strwrappers::OutputName>,
}

// maximum message length
pub type ProtoLen = u32;
pub const NULL_LEN: [u8; std::mem::size_of::<ProtoLen>()] = [0u8; std::mem::size_of::<ProtoLen>()];

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub enum Request {
    UnsubscribeAll,
    Kill(store::Hash),
    SubmitTask { item: WorkItem, subscribe2log: bool },
    Upload(store::Dump),
    HasOutHash(store::Hash),
    Download(store::Hash),
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub enum Response {
    Ok,
    False,
    LogError,
    Dump(store::Dump),
    TaskBound(store::Hash, TaskBoundResponse),
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
    #[error("command was killed by client")]
    KilledByClient,

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

    #[error("store error: {0}")]
    Store(#[from] crate::store::Error),

    #[error("unknown error: {0}")]
    Unknown(String),
}

impl From<std::io::Error> for BuildError {
    fn from(x: std::io::Error) -> Self {
        if let Some(y) = x.raw_os_error() {
            BuildError::Io(y)
        } else {
            BuildError::Unknown(x.to_string())
        }
    }
}
