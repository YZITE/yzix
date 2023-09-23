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

mod regular;
pub use regular::{Flags as RegularFlags, Regular};
pub mod store_utils;
mod thintree;
pub use thintree::{SubmitError as ThinTreeSubmitError, ThinTree};

mod hash;
pub use hash::{Hash as StoreHash, TaggedHash};

mod ser_trait;
pub use ser_trait::{Serialize, Update as SerUpdate};

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, thiserror::Error)]
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

impl BuildError {
    pub fn errtype(&self) -> String {
        match self {
            BuildError::KilledByClient => "killed.by.client".to_string(),
            BuildError::Exit(i) => format!("exit.{}", i),
            BuildError::Killed(i) => format!("killed.by.signal.{}", i),
            BuildError::Io(i) => format!("io.{}", i),
            BuildError::EmptyCommand => "empty.command".to_string(),
            BuildError::HashCollision(h) => format!("hash.collision:{}", h),
            BuildError::Store(x) => x.kind.errtype(),
            BuildError::Unknown(_) => "unknown".to_string(),
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
    #[error("hash mismatch between expected hash and given data")]
    HashMismatch,

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

impl StoreErrorKind {
    pub fn errtype(&self) -> String {
        match self {
            Self::HashMismatch => "store.hash.mismatch".to_string(),
            Self::NonUtf8SymlinkTarget => "store.non_utf8.symlink_target".to_string(),
            Self::NonUtf8Basename => "store.non_utf8.basename".to_string(),
            Self::UnknownFileType(i) => format!("store.unknown.file_type:{}", i),
            Self::InvalidBasename => "store.invalid.basename".to_string(),
            #[cfg(not(any(unix, windows)))]
            Self::SymlinksUnsupported => "store.unsupported.symlinks".to_string(),
            Self::OverwriteDeclined => "store.overwrite.declined".to_string(),
            Self::IoMisc { errno, .. } => {
                if let Some(errno) = errno {
                    format!("store.io.{}", errno)
                } else {
                    "store.io".to_string()
                }
            }
        }
    }

    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::IoMisc { errno: Some(2), .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DumpFlags {
    /// if `force` is `true`, then `write_to_path` will take additional
    /// measures which will/might overwrite stuff.
    ///
    /// if `force` is `false`, then, if the object to dump already exists,
    /// it aborts.
    /// FIXME: make this an enum and make it possible to select a third kind
    /// of behavoir: validation of an existing tree.
    pub force: bool,
    pub make_readonly: bool,
}
