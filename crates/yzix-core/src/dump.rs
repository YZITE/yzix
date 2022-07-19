// this crate only supports unix for now, and this ensures that the build fails
// quickly, and the user isn't annoyed that it compiles, but doesn't work at runtime.
#![cfg(unix)]

use crate::stree::{mk_mapef, mk_reftime, set_perms_to_mode, Regular, RegularFlags};
use crate::{visit_bytes as yvb, StoreError as Error, StoreErrorKind as ErrorKind};
use camino::Utf8PathBuf;
use std::{fmt, fs, path::Path};

/// sort-of emulation of NAR
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Dump {
    Regular(Regular),
    SymLink { target: Utf8PathBuf },
    Directory(std::collections::BTreeMap<crate::BaseName, Dump>),
}

impl fmt::Display for Dump {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        crate::StoreHash::hash_complex(self).fmt(f)
    }
}

// sort-of NAR serialization impl
impl crate::Serialize for Dump {
    fn serialize<H: crate::SerUpdate>(&self, state: &mut H) {
        "(".serialize(state);
        "type".serialize(state);
        match self {
            Dump::Regular(regu) => {
                "regular".serialize(state);
                regu.serialize(state);
            }
            Dump::SymLink { target } => {
                "symlink".serialize(state);
                "target".serialize(state);
                target.as_str().serialize(state);
            }
            Dump::Directory(entries) => {
                "directory".serialize(state);
                for (k, v) in entries {
                    for i in ["entry", "(", "name"] {
                        i.serialize(state);
                    }
                    k.serialize(state);
                    "node".serialize(state);
                    v.serialize(state);
                    ")".serialize(state);
                }
            }
        }
        ")".serialize(state);
    }
}

impl yvb::Element for Dump {
    fn accept<V: yvb::Visitor>(&self, visitor: &mut V) {
        match self {
            Dump::Regular(regu) => regu.accept(visitor),
            Dump::SymLink { target } => target.accept(visitor),
            Dump::Directory(entries) => {
                entries.values().for_each(|val| val.accept(visitor));
            }
        }
    }
    fn accept_mut<V: yvb::VisitorMut>(&mut self, visitor: &mut V) {
        match self {
            Dump::Regular(regu) => regu.accept_mut(visitor),
            Dump::SymLink { target } => target.accept_mut(visitor),
            Dump::Directory(entries) => {
                entries.values_mut().for_each(|val| val.accept_mut(visitor));
            }
        }
    }
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
        let mapef = mk_mapef(x);
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
            Dump::Regular(Regular::read_from_path(x, &meta)?)
        } else if ty.is_dir() {
            Dump::Directory(
                std::fs::read_dir(x)
                    .map_err(&mapef)?
                    .map(|entry| {
                        let entry = entry.map_err(&mapef)?;
                        let ep = entry.path();
                        let name = crate::BaseName::decode_from_path(&ep)?;
                        let val = Dump::read_from_path(&ep)?;
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
        let reftime = mk_reftime();
        let mapef = mk_mapef(x);
        use std::io::ErrorKind as IoErrorKind;
        let mut skip_write = false;

        if let Ok(y) = fs::symlink_metadata(x) {
            if !flags.force {
                return Err(Error {
                    real_path: x.to_path_buf(),
                    kind: ErrorKind::OverwriteDeclined,
                });
            } else if !y.is_dir() {
                if let Dump::Regular(Regular { contents, .. }) = self {
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
            Dump::SymLink { target } => std::os::unix::fs::symlink(&target, x).map_err(&mapef)?,
            Dump::Regular(regu) => regu.write_to_path(
                x,
                RegularFlags {
                    skip_write,
                    make_readonly: flags.make_readonly,
                },
            )?,
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
                                        set_perms_to_mode(x, 0o755)?;
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
                            set_perms_to_mode(x, 0o755)?;
                        }
                    } else {
                        return Err(mapef(e));
                    }
                }
                let mut xs = x.to_path_buf();
                for (name, val) in contents {
                    xs.push(name);
                    // this call also deals with cases where the file already exists
                    Dump::write_to_path(val, &xs, flags)?;
                    xs.pop();
                }

                if flags.make_readonly {
                    set_perms_to_mode(x, 0o555)?;
                }
            }
        }
        filetime::set_symlink_file_times(x, reftime, reftime).map_err(&mapef)
    }
}
