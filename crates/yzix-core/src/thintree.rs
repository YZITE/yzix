// this crate only supports unix for now, and this ensures that the build fails
// quickly, and the user isn't annoyed that it compiles, but doesn't work at runtime.
#![cfg(unix)]

use crate::store_utils::{mk_mapef, mk_reftime, set_perms_to_mode};
use crate::{
    visit_bytes as yvb, BaseName, Regular, RegularFlags, StoreError as Error,
    StoreErrorKind as ErrorKind, TaggedHash,
};
use camino::Utf8PathBuf;
use std::{fmt, fs};
use std::path::{Path, PathBuf};

/// sort-of emulation of NAR, but omits the contents of most non-self-referential `Regular` entries,
/// and instead just saves the hash of files, which get hard-linked on realisation.
///
/// this does intentionally not implement [`yvb::Element`], because the supposed
/// usage is unclear.
///
/// this format is supposed to be used mostly in transit, and for exposing separate metadata.
/// e.g. it should be retrievable from a store server, accompanied by some kind of narinfo metadata.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum ThinTree {
    Regular(TaggedHash<Regular>),
    RegularInline(Regular),
    SymLink { target: Utf8PathBuf },
    Directory(std::collections::BTreeMap<BaseName, ThinTree>),
}

pub enum SubmitError {
    StoreErr(Error),
    StoreLoop(Regular),
}

impl fmt::Display for ThinTree {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        crate::StoreHash::hash_complex(self).fmt(f)
    }
}

impl crate::Serialize for ThinTree {
    fn serialize<H: crate::SerUpdate>(&self, state: &mut H) {
        "(".serialize(state);
        "type".serialize(state);
        match self {
            ThinTree::Regular(h) => {
                "regular.hash".serialize(state);
                h.serialize(state);
            }
            ThinTree::RegularInline(regu) => {
                "regular".serialize(state);
                regu.serialize(state);
            }
            ThinTree::SymLink { target } => {
                "symlink".serialize(state);
                "target".serialize(state);
                target.as_str().serialize(state);
            }
            ThinTree::Directory(entries) => {
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

impl yvb::Element for ThinTree {
    fn accept<V: yvb::Visitor>(&self, visitor: &mut V) {
        match self {
            ThinTree::Regular(_) => {}
            ThinTree::RegularInline(regu) => regu.accept(visitor),
            ThinTree::SymLink { target } => target.accept(visitor),
            ThinTree::Directory(entries) => {
                entries.values().for_each(|val| val.accept(visitor));
            }
        }
    }

    fn accept_mut<V: yvb::VisitorMut>(&mut self, visitor: &mut V) {
        match self {
            ThinTree::Regular(_) => {}
            ThinTree::RegularInline(regu) => regu.accept_mut(visitor),
            ThinTree::SymLink { target } => target.accept_mut(visitor),
            ThinTree::Directory(entries) => {
                entries.values_mut().for_each(|val| val.accept_mut(visitor));
            }
        }
    }
}

impl ThinTree {
    /// read a thin tree from a path,
    /// submit all encountered regular files via `submit`
    pub fn read_from_path<Fs>(x: &Path, submit: &mut Fs) -> Result<Self, Error>
    where
        Fs: FnMut(TaggedHash<Regular>, Regular) -> Result<(), SubmitError>,
    {
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
            Self::SymLink { target }
        } else if ty.is_file() {
            let regu = Regular::read_from_path(x, &meta)?;
            let h = TaggedHash::hash_complex(&regu);
            match submit(h, regu) {
                Ok(()) => Self::Regular(h),
                Err(SubmitError::StoreLoop(regu)) => Self::RegularInline(regu),
                Err(SubmitError::StoreErr(e)) => return Err(e),
            }
        } else if ty.is_dir() {
            Self::Directory(
                std::fs::read_dir(x)
                    .map_err(&mapef)?
                    .map(|entry| {
                        let entry = entry.map_err(&mapef)?;
                        let ep = entry.path();
                        let name = BaseName::decode_from_path(&ep)?;
                        let val = ThinTree::read_from_path(&ep, submit)?;
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

    /// we reguire that the parent directory already exists,
    /// and will override the target path `x` if it already exists.
    pub fn write_to_path<Frq>(
        &self,
        x: &Path,
        flags: crate::DumpFlags,
        request: &Frq,
    ) -> Result<(), Error>
    where
        Frq: Fn(TaggedHash<Regular>) -> Result<PathBuf, Error>,
    {
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
            } else if y.is_dir() {
                if let Self::Directory(_) = self {
                    // passthrough
                } else {
                    // `x` is a directory and `self` isn't
                    fs::remove_dir_all(x).map_err(&mapef)?;
                }
            } else if let ThinTree::RegularInline(Regular { contents, .. }) = self {
                if fs::read(x)
                    .map(|curcts| &curcts == contents)
                    .unwrap_or(false)
                {
                    skip_write = true;
                } else {
                    // recreation is cheap
                    fs::remove_file(x).map_err(&mapef)?;
                }
            } else {
                // recreation is cheap
                fs::remove_file(x).map_err(&mapef)?;
            }
        }

        match self {
            Self::SymLink { target } => {
                std::os::unix::fs::symlink(&target, x).map_err(&mapef)?;
                filetime::set_symlink_file_times(x, reftime, reftime).map_err(&mapef)
            }
            Self::Regular(h) => {
                let lnk2 = request(*h)?;
                std::fs::hard_link(&lnk2, x).map_err(|e| {
                    match e.kind() {
                        IoErrorKind::NotFound /* | IoErrorKind::TooManyLinks */ => Error {
                            real_path: lnk2,
                            kind: e.into(),
                        },
                        _ => mapef(e),
                    }
                })
            }
            Self::RegularInline(regi) => regi.write_to_path(
                x,
                RegularFlags {
                    skip_write,
                    make_readonly: flags.make_readonly,
                },
            ),
            Self::Directory(contents) => {
                if let Err(e) = fs::create_dir(&x) {
                    if e.kind() != IoErrorKind::AlreadyExists {
                        return Err(mapef(e));
                    }
                    let mut already_writable = false;
                    // the check at the start of the function should have taken
                    // care of the annoying edge cases. x is this already a directory.
                    for entry in fs::read_dir(x).map_err(&mapef)? {
                        let entry = entry.map_err(&mapef)?;
                        if entry
                            .file_name()
                            .into_string()
                            .ok()
                            .map(|x| contents.contains_key(&x))
                            == Some(true)
                        {
                            // file does exist in the entry list
                            continue;
                        }
                        // file doesn't exist in the entry list
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
                            if entry
                                .file_type()
                                .map_err(|e| (mk_mapef(&entry.path()))(e))?
                                .is_dir()
                            {
                                fs::remove_dir_all(real_path)
                            } else {
                                fs::remove_file(real_path)
                            }
                            .map_err(|e| (mk_mapef(&entry.path())(e)))?;
                        }
                    }
                }

                let mut xs = x.to_path_buf();
                for (name, val) in contents {
                    xs.push(name);
                    // this call also deals with cases where the file already exists
                    val.write_to_path(&xs, flags, request)?;
                    xs.pop();
                }
                if flags.make_readonly {
                    set_perms_to_mode(x, 0o555)?;
                }
                filetime::set_symlink_file_times(x, reftime, reftime).map_err(&mapef)
            }
        }
    }
}
