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

pub use yzix_core::*;

mod codec;
pub use codec::*;

// maximum message length
pub type ProtoLen = u32;
pub const NULL_LEN: [u8; std::mem::size_of::<ProtoLen>()] = [0u8; std::mem::size_of::<ProtoLen>()];

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub enum Request {
    GetStorePath,
    Kill(StoreHash),
    SubmitTask { item: WorkItem, subscribe2log: bool },
    Upload(ThinTree),
    HasOutHash(StoreHash),
    Download(StoreHash),
    DownloadRegular(TaggedHash<Regular>),
}

impl fmt::Display for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
            Request::Upload(d) => write!(f, "Upload(...@ {})", StoreHash::hash_complex(d)),
            Request::HasOutHash(h) => write!(f, "HasOutHash({})", h),
            Request::Download(h) => write!(f, "Download({})", h),
            Request::DownloadRegular(h) => write!(f, "DownloadRegular({})", h.as_ref()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub enum Response {
    Ok,
    Aborted,
    False,
    LogError,
    Text(String),
    Dump(ThinTree),
    TaskBound(StoreHash, TaskBoundResponse),
    RegularBound(TaggedHash<Regular>, Result<Regular, StoreError>),
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
                | Response::RegularBound(_, Ok(_))
        )
    }
}

impl From<bool> for Response {
    fn from(x: bool) -> Response {
        if x {
            Response::Ok
        } else {
            Response::False
        }
    }
}
