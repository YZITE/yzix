// this crate only supports unix for now, and this ensures that the build fails
// quickly, and the user isn't annoyed that it compiles, but doesn't work at runtime.
#![cfg(unix)]

use crate::{
    visit_bytes as yvb, BaseName, StoreError as Error, StoreErrorKind as ErrorKind, TaggedHash,
};
use camino::Utf8PathBuf;
use std::fs;
use std::path::{Path, PathBuf};

/// A regular file
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Regular {
    pub executable: bool,
    #[serde(with = "serde_bytes")]
    pub contents: Vec<u8>,
}

impl crate::Serialize for Regular {
    fn serialize<H: crate::SerUpdate>(&self, state: &mut H) {
        if self.executable {
            "executable".serialize(state);
            "".serialize(state);
        }
        "contents".serialize(state);
        self.contents.serialize(state);
    }
}

impl yvb::Element for Regular {
    #[inline(always)]
    fn accept<V: yvb::Visitor>(&self, visitor: &mut V) {
        visitor.visit_bytes(&self.contents[..]);
    }
    #[inline(always)]
    fn accept_mut<V: yvb::VisitorMut>(&mut self, visitor: &mut V) {
        visitor.visit_bytes(&mut self.contents[..]);
    }
}

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

pub(crate) fn set_perms_to_mode(y: &Path, mode: u32) -> Result<(), Error> {
    fs::set_permissions(y, std::os::unix::fs::PermissionsExt::from_mode(mode))
        .map_err(|e| (mk_mapef(y))(e))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegularFlags {
    pub skip_write: bool,
    pub make_readonly: bool,
}

impl Regular {
    pub fn read_from_path(x: &Path, meta: &fs::Metadata) -> Result<Self, Error> {
        let mut contents = fs::read(x).map_err(|e| (mk_mapef(x))(e))?;
        contents.shrink_to_fit();
        Ok(Regular {
            executable: std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0,
            contents,
        })
    }

    pub fn write_to_path(&self, x: &Path, flags: RegularFlags) -> Result<(), Error> {
        let mapef = mk_mapef(x);
        let reftime = mk_reftime();

        if !flags.skip_write {
            fs::write(x, &self.contents[..]).map_err(&mapef)?;
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
                set_perms_to_mode(x, if self.executable { 0o755 } else { 0o644 })?;

                for i in rem_xattrs {
                    xattr::remove(x, i).map_err(&mapef)?;
                }
            }
        }

        let mut permbits = 0o444;
        if self.executable {
            permbits |= 0o111;
        }
        if !flags.make_readonly {
            permbits |= 0o200;
        }
        set_perms_to_mode(x, permbits)?;

        filetime::set_symlink_file_times(x, reftime, reftime).map_err(&mapef)
    }
}

/// this is like [`Dump`](crate::Dump), but omits the contents of `Regular`,
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
    SymLink { target: Utf8PathBuf },
    Directory(std::collections::BTreeMap<BaseName, ThinTree>),
}

impl ThinTree {
    /// read a thin tree from a path,
    /// submit all encountered regular files via `submit`
    pub fn read_from_path<Fs>(x: &Path, submit: &Fs) -> Result<Self, Error>
    where
        Fs: Fn(TaggedHash<Regular>, Regular) -> Result<(), Error>,
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
            submit(h, regu)?;
            Self::Regular(h)
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
