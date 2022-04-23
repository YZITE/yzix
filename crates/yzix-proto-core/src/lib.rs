#![forbid(
    clippy::as_conversions,
    clippy::cast_ptr_alignment,
    clippy::let_underscore_drop,
    trivial_casts,
    unconditional_recursion,
    unsafe_code
)]

mod strwrappers;
mod wi;
pub use strwrappers::*;
pub use wi::WorkItem;

use std::collections::BTreeMap;
pub use yzix_store::{
    visit_bytes, Dump, Error as StoreError, ErrorKind as StoreErrorKind, Flags as DumpFlags,
    Hash as StoreHash, Serialize, Update as SerUpdate,
};

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum TaskBoundResponse {
    Queued,
    BuildSuccess(BTreeMap<OutputName, StoreHash>),
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

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize, thiserror::Error)]
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
    HashCollision(StoreHash),

    #[error("store error: {0}")]
    Store(#[from] StoreError),

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
