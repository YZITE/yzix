use std::collections::BTreeMap;
use std::fs;
use std::io::{Error, ErrorKind};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use tracing::{self, error, trace};
use yzix_proto::store::Hash as StoreHash;
use yzix_proto::strwrappers::OutputName;

/// tries to resolve an input realisation 'cache' entry (`*.in2` paths in the store)
/// * `target` should be the target read via `read_link`
/// * `super_dir` should be the expected parent of the target
pub fn resolve_in2_from_target(target: PathBuf, super_dir: &Path) -> Option<StoreHash> {
    let x = target;
    if x.is_relative() && x.parent() == Some(super_dir) {
        x.file_name()
            .and_then(|x| x.to_str())
            .and_then(|x| x.parse::<StoreHash>().ok())
    } else {
        None
    }
}

pub fn resolve_in2(realis_path: &Path) -> BTreeMap<OutputName, StoreHash> {
    if let Ok(rp) = std::fs::read_link(&realis_path) {
        resolve_in2_from_target(rp, Path::new(""))
            .map(|outhash| (OutputName::default(), outhash))
            .into_iter()
            .collect()
    } else if let Ok(realis_iter) = std::fs::read_dir(&realis_path) {
        let super_dir = Path::new("..");
        realis_iter
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry
                    .file_name()
                    .to_str()
                    .and_then(|filn| OutputName::new(filn.to_string()))?;
                let rp = std::fs::read_link(entry.path()).ok()?;
                Some((name, rp))
            })
            .filter_map(|(filn, rp)| {
                resolve_in2_from_target(rp, super_dir).map(|outhash| (filn, outhash))
            })
            .collect()
    } else {
        Default::default()
    }
}

pub fn create_in2_symlinks_bunch(
    inpath: &Path,
    syms: &BTreeMap<OutputName, StoreHash>,
) -> std::io::Result<()> {
    let mut prev_outlink = None;
    if let Err(e) = fs::create_dir(inpath) {
        if e.kind() != ErrorKind::AlreadyExists {
            trace!("create_dir: {}", e);
            return Err(e);
        }
        let y = inpath.symlink_metadata()?;
        if !y.is_dir() {
            let prev_outlink_ = fs::read_link(inpath)?.to_string_lossy().into_owned();
            if let Some(x) = syms.get(&OutputName::default()) {
                if prev_outlink_ != x.to_string() {
                    return Err(Error::new(
                        ErrorKind::AlreadyExists,
                        format!("previous outlink differs: {}", prev_outlink_),
                    ));
                }
                fs::remove_file(inpath)?;
            }
            prev_outlink = Some(prev_outlink_);
        }
    }

    for (k, v) in syms {
        let kpath = inpath.join(&**k);
        let v: PathBuf = v.to_string().into();
        if let Err(e) = symlink(Path::new("..").join(&v), &kpath) {
            if e.kind() != ErrorKind::AlreadyExists {
                trace!("symlink: {}", e);
                return Err(e);
            }
            let oldloc = fs::read_link(&kpath)?;
            if oldloc != v {
                return Err(Error::new(
                    ErrorKind::AlreadyExists,
                    format!(
                        "previous link to output {} differs: {}",
                        k,
                        oldloc.display()
                    ),
                ));
            }
        }
    }

    if let Some(x) = prev_outlink {
        let polpath = inpath.join(&*OutputName::default());
        if !polpath.exists() {
            trace!("recover prev outlink symlink");
            symlink(Path::new("..").join(x), polpath)?;
        }
    }
    Ok(())
}

#[tracing::instrument]
pub fn create_in2_symlinks(inpath: &Path, syms: &BTreeMap<OutputName, StoreHash>) {
    // FIXME: how to deal with conflicting hashes?

    // optimization, because the amount of subdirectories per directory
    // is really limited (e.g. ~64000 for ext4)
    // see also: https://ext4.wiki.kernel.org/index.php/Ext4_Howto#Sub_directory_scalability
    // so we omit creating a subdirectory if it would only contain
    // the 'out' (default output path) symlink

    // this is caching, if it fails,
    //    it's non-fatal for the node, but a big error for the server

    if syms.len() == 1 {
        if let Some(target) = syms.get(&OutputName::default()) {
            let t2 = target.to_string();
            match std::fs::read_link(inpath) {
                Ok(oldtrg) if oldtrg == std::path::Path::new(&t2) => {}
                Err(erl) if erl.kind() == std::io::ErrorKind::NotFound => {
                    if let Err(e) = std::os::unix::fs::symlink(&t2, inpath) {
                        error!("{}", e);
                    }
                }
                Ok(orig_target) => {
                    error!(?orig_target, "outname differs");
                }
                Err(e) => {
                    error!("blocked: {}", e);
                }
            }
            // usually you can't mark a symlink as read-only
            return;
        }
    }

    if let Err(e) = create_in2_symlinks_bunch(inpath, syms) {
        error!("{}", e);
    }
}
