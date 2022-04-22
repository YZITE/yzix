#![forbid(
    clippy::as_conversions,
    clippy::cast_ptr_alignment,
    clippy::let_underscore_drop,
    trivial_casts,
    unconditional_recursion,
    unsafe_code
)]

use core::fmt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub use yzix_store as store;
pub use yzix_strwrappers as strwrappers;

mod codec;
pub use codec::*;
mod wi;
pub use wi::*;

// maximum message length
pub type ProtoLen = u32;
pub const NULL_LEN: [u8; std::mem::size_of::<ProtoLen>()] = [0u8; std::mem::size_of::<ProtoLen>()];

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub enum Request {
    UnsubscribeAll,
    GetStorePath,
    Kill(store::Hash),
    SubmitTask { item: WorkItem, subscribe2log: bool },
    Upload(store::Dump),
    HasOutHash(store::Hash),
    Download(store::Hash),
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Request::UnsubscribeAll => write!(f, "UnsubscribeAll"),
            Request::GetStorePath => write!(f, "GetStorePath"),
            Request::Kill(h) => write!(f, "Kill({})", h),
            Request::SubmitTask {
                item,
                subscribe2log,
            } => write!(
                f,
                "SubmitTask{}({:?})",
                if *subscribe2log { "+log" } else { "" },
                item
            ),
            Request::Upload(d) => write!(f, "Upload(...@ {})", store::Hash::hash_complex(d)),
            Request::HasOutHash(h) => write!(f, "HasOutHash({})", h),
            Request::Download(h) => write!(f, "Download({})", h),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub enum Response {
    Ok,
    False,
    LogError,
    Text(String),
    Dump(store::Dump),
    TaskBound(store::Hash, TaskBoundResponse),
}

impl Response {
    #[inline]
    pub fn is_ok(&self) -> bool {
        matches!(
            self,
            Response::Ok
                | Response::Text(_)
                | Response::Dump(_)
                | Response::TaskBound(_, TaskBoundResponse::BuildSuccess(_))
        )
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub enum TaskBoundResponse {
    Queued,
    BuildSuccess(BTreeMap<strwrappers::OutputName, store::Hash>),
    Log(String),
    BuildError(BuildError),
}

impl TaskBoundResponse {
    #[inline]
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
