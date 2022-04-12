// this crate only supports unix for now, and this ensures that the build fails
// quickly, and the user isn't annoyed that it compiles, but doesn't work at runtime.
#![cfg(unix)]

use super::{Error, ErrorKind};
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

/// sort-of emulation of NAR using CBOR
#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase", tag = "type")]
pub enum Dump {
    Regular { executable: bool, contents: Vec<u8> },
    SymLink { target: Utf8PathBuf },
    Directory(std::collections::BTreeMap<String, Dump>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags {
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

impl Dump {
    pub fn read_from_path(x: &Path) -> Result<Self, Error> {
        let mapef = |e: std::io::Error| Error {
            real_path: x.to_path_buf(),
            kind: e.into(),
        };
        let meta = fs::symlink_metadata(x).map_err(&mapef)?;
        let ty = meta.file_type();
        Ok(if ty.is_symlink() {
            let mut target: Utf8PathBuf =
                fs::read_link(x)
                    .map_err(&mapef)?
                    .try_into()
                    .map_err(|_| Error {
                        real_path: x.to_path_buf(),
                        kind: ErrorKind::NonUtf8SymlinkTarget,
                    })?;
            target.shrink_to_fit();
            Dump::SymLink { target }
        } else if ty.is_file() {
            let mut contents = std::fs::read(x).map_err(&mapef)?;
            contents.shrink_to_fit();
            Dump::Regular {
                executable: std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111
                    != 0,
                contents,
            }
        } else if ty.is_dir() {
            Dump::Directory(
                std::fs::read_dir(x)
                    .map_err(&mapef)?
                    .map(|entry| {
                        let entry = entry.map_err(&mapef)?;
                        let name = entry.file_name().into_string().map_err(|_| Error {
                            real_path: entry.path(),
                            kind: ErrorKind::NonUtf8Basename,
                        })?;
                        let val = Dump::read_from_path(&entry.path())?;
                        Ok((name, val))
                    })
                    .collect::<Result<_, Error>>()?,
            )
        } else {
            return Err(Error {
                real_path: x.to_path_buf(),
                kind: ErrorKind::UnknownFileType(format!("{:?}", ty)),
            });
        })
    }

    /// we require that the parent directory already exists,
    /// and will override the target path `x` if it already exists.
    pub fn write_to_path(&self, x: &Path, flags: Flags) -> Result<(), Error> {
        // one second past epoch, necessary for e.g. GNU make to recognize
        // the dumped files as "oldest"
        // TODO: when https://github.com/alexcrichton/filetime/pull/75 is merged,
        //   upgrade `filetime` and make this a global `const` binding.
        let reftime = filetime::FileTime::from_unix_time(1, 0);

        use std::io::ErrorKind as IoErrorKind;

        let mapef = |e: std::io::Error| Error {
            real_path: x.to_path_buf(),
            kind: e.into(),
        };
        let mut skip_write = false;

        if let Ok(y) = fs::symlink_metadata(x) {
            if !flags.force {
                return Err(Error {
                    real_path: x.to_path_buf(),
                    kind: ErrorKind::OverwriteDeclined,
                });
            } else if !y.is_dir() {
                if let Dump::Regular { contents, .. } = self {
                    if fs::read(x)
                        .map(|curcts| &curcts == contents)
                        .unwrap_or(false)
                    {
                        skip_write = true;
                    } else if y.file_type().is_symlink() {
                        // FIXME: maybe keep existing symlinks if self is also a symlink,
                        // and the target matches...
                        fs::remove_file(x).map_err(&mapef)?;
                    }
                } else {
                    fs::remove_file(x).map_err(&mapef)?;
                }
            } else if let Dump::Directory(_) = self {
                // passthrough
            } else {
                // `x` is a directory and `self` isn't
                fs::remove_dir_all(x).map_err(&mapef)?;
            }
        }

        match self {
            Dump::SymLink { target } => {
                std::os::unix::fs::symlink(&target, x).map_err(&mapef)?;
            }
            Dump::Regular {
                executable,
                contents,
            } => {
                if !skip_write {
                    fs::write(x, contents).map_err(&mapef)?;
                }

                // don't make stuff readonly on windows, it makes overwriting files more complex...

                if xattr::SUPPORTED_PLATFORM {
                    // delete only non-system attributes for now
                    // see also: https://github.com/NixOS/nix/pull/4765
                    // e.g. we can't delete attributes like
                    // - security.selinux
                    // - system.nfs4_acl
                    let rem_xattrs = xattr::list(x)
                        .map_err(&mapef)?
                        .flat_map(|i| i.into_string().ok())
                        .filter(|i| !i.starts_with("security.") && !i.starts_with("system."))
                        .collect::<Vec<_>>();
                    if !rem_xattrs.is_empty() {
                        // make the file temporary writable
                        fs::set_permissions(
                            x,
                            std::os::unix::fs::PermissionsExt::from_mode(if *executable {
                                0o755
                            } else {
                                0o644
                            }),
                        )
                        .map_err(&mapef)?;

                        for i in rem_xattrs {
                            xattr::remove(x, i).map_err(&mapef)?;
                        }
                    }
                }

                let mut permbits = 0o444;
                if *executable {
                    permbits |= 0o111;
                }
                if !flags.make_readonly {
                    permbits |= 0o200;
                }
                fs::set_permissions(x, std::os::unix::fs::PermissionsExt::from_mode(permbits))
                    .map_err(&mapef)?;
            }
            Dump::Directory(contents) => {
                if let Err(e) = fs::create_dir(&x) {
                    if e.kind() == IoErrorKind::AlreadyExists {
                        let mut already_writable = false;
                        // the check at the start of the function should have taken
                        // care of the annoying edge cases.
                        // x is thus already a directory
                        for entry in fs::read_dir(x).map_err(&mapef)? {
                            let entry = entry.map_err(&mapef)?;
                            if entry
                                .file_name()
                                .into_string()
                                .ok()
                                .map(|x| contents.contains_key(&x))
                                != Some(true)
                            {
                                // file does not exist in the entry list...
                                let real_path = entry.path();
                                if !flags.force {
                                    return Err(Error {
                                        real_path,
                                        kind: ErrorKind::OverwriteDeclined,
                                    });
                                } else {
                                    if !already_writable {
                                        fs::set_permissions(
                                            x,
                                            std::os::unix::fs::PermissionsExt::from_mode(0o755),
                                        )
                                        .map_err(&mapef)?;
                                        already_writable = true;
                                    }
                                    if entry.file_type().map_err(&mapef)?.is_dir() {
                                        fs::remove_dir_all(real_path)
                                    } else {
                                        fs::remove_file(real_path)
                                    }
                                    .map_err(&mapef)?;
                                }
                            }
                        }

                        if flags.force && !already_writable {
                            fs::set_permissions(
                                x,
                                std::os::unix::fs::PermissionsExt::from_mode(0o755),
                            )
                            .map_err(&mapef)?;
                        }
                    } else {
                        return Err(mapef(e));
                    }
                }
                let mut xs = x.to_path_buf();
                for (name, val) in contents {
                    if name.is_empty() {
                        return Err(Error {
                            real_path: x.to_path_buf(),
                            kind: ErrorKind::EmptyBasename,
                        });
                    }
                    xs.push(name);
                    // this call also deals with cases where the file already exists
                    Dump::write_to_path(val, &xs, flags)?;
                    xs.pop();
                }

                if flags.make_readonly {
                    fs::set_permissions(x, std::os::unix::fs::PermissionsExt::from_mode(0o555))
                        .map_err(&mapef)?;
                }
            }
        }
        filetime::set_symlink_file_times(x, reftime, reftime).map_err(&mapef)
    }
}
