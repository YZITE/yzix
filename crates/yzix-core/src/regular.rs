use crate::store_utils::{mk_mapef, mk_reftime, set_perms_to_mode};
use crate::{visit_bytes as yvb, StoreError as Error};
use std::fs;
use std::path::Path;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags {
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

    pub fn write_to_path(&self, x: &Path, flags: Flags) -> Result<(), Error> {
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
