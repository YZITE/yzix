use crate::StoreError as Error;
use std::fs;
use std::path::Path;

#[inline]
pub(crate) fn mk_mapef(x: &Path) -> impl (Fn(std::io::Error) -> Error) + '_ {
    move |e| Error {
        real_path: x.to_path_buf(),
        kind: e.into(),
    }
}

/// one second past epoch, necessary for e.g. GNU make to recognize
/// the dumped files as "oldest"
// TODO: when https://github.com/alexcrichton/filetime/pull/75 is merged,
//   upgrade `filetime` and make this a global `const` binding.
#[inline]
pub(crate) fn mk_reftime() -> filetime::FileTime {
    filetime::FileTime::from_unix_time(1, 0)
}

pub fn set_perms_to_mode(y: &Path, mode: u32) -> Result<(), Error> {
    fs::set_permissions(y, std::os::unix::fs::PermissionsExt::from_mode(mode))
        .map_err(|e| (mk_mapef(y))(e))
}
