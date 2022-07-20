#![forbid(
    clippy::cast_ptr_alignment,
    clippy::let_underscore_drop,
    trivial_casts,
    unconditional_recursion,
    unsafe_code
)]

mod strwrappers;
pub use strwrappers::*;

mod wi;
pub use wi::WorkItem;

mod dump;
pub use dump::{Dump, Flags as DumpFlags};

mod regular;
pub use regular::{Flags as RegularFlags, Regular};
pub mod store_utils;
mod thintree;
pub use thintree::ThinTree;
mod semitree;
pub use semitree::{SemiTree, SubmitError as SemiTreeSubmitError};

mod hash;
pub use hash::{Hash as StoreHash, TaggedHash};

mod ser_trait;
pub use ser_trait::{Serialize, Update as SerUpdate};

pub mod visit_bytes;

use std::collections::BTreeMap;

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

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, thiserror::Error)]
#[error("{real_path}: {kind}")]
pub struct StoreError {
    pub real_path: std::path::PathBuf,
    #[source]
    pub kind: StoreErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, thiserror::Error)]
pub enum StoreErrorKind {
    #[error("unable to convert symlink destination to UTF-8")]
    NonUtf8SymlinkTarget,

    #[error("unable to convert file name to UTF-8")]
    NonUtf8Basename,

    #[error("got unknown file type {0}")]
    UnknownFileType(String),

    #[error("got directory entry with invalid base names")]
    InvalidBasename,

    #[error("symlinks are unsupported on this system")]
    #[cfg(not(any(unix, windows)))]
    SymlinksUnsupported,

    #[error("store dump declined to overwrite file")]
    /// NOTE: this obviously gets attached to the directory name
    OverwriteDeclined,

    #[error("I/O error: {desc}")]
    // NOTE: the `desc` already contains the os error code, don't print it 2 times.
    IoMisc { errno: Option<i32>, desc: String },
}

impl From<std::io::Error> for StoreErrorKind {
    #[inline]
    fn from(e: std::io::Error) -> StoreErrorKind {
        StoreErrorKind::IoMisc {
            errno: e.raw_os_error(),
            desc: e.to_string(),
        }
    }
}
