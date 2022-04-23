#![forbid(
    clippy::cast_ptr_alignment,
    clippy::let_underscore_drop,
    trivial_casts,
    unconditional_recursion,
    unsafe_code
)]

mod dump;
// TODO: maybe rename Flags to something better...
pub use dump::{Dump, Flags};

mod hash;
pub use hash::Hash;

mod ser_trait;
pub use ser_trait::{Serialize, Update};

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, thiserror::Error)]
#[error("{real_path}: {kind}")]
pub struct Error {
    pub real_path: std::path::PathBuf,
    #[source]
    pub kind: ErrorKind,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, thiserror::Error)]
pub enum ErrorKind {
    #[error("unable to convert symlink destination to UTF-8")]
    NonUtf8SymlinkTarget,

    #[error("unable to convert file name to UTF-8")]
    NonUtf8Basename,

    #[error("got unknown file type {0}")]
    UnknownFileType(String),

    #[error("directory entries with empty names are invalid")]
    EmptyBasename,

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

impl From<std::io::Error> for ErrorKind {
    #[inline]
    fn from(e: std::io::Error) -> ErrorKind {
        ErrorKind::IoMisc {
            errno: e.raw_os_error(),
            desc: e.to_string(),
        }
    }
}
