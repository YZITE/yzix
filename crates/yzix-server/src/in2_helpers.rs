use std::collections::HashMap;
use std::fs;
use std::io::{Error, ErrorKind};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use tracing::{self, trace};
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

#[tracing::instrument]
pub fn create_in2_symlinks_bunch(
    inpath: &Path,
    syms: &HashMap<String, String>,
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
            if let Some(x) = syms.get(&*OutputName::default()) {
                if &prev_outlink_ != x {
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
        let kpath = inpath.join(k);
        if let Err(e) = symlink(Path::new("..").join(v), &kpath) {
            if e.kind() != ErrorKind::AlreadyExists {
                trace!("symlink: {}", e);
                return Err(e);
            }
            let oldloc = fs::read_link(&kpath)?;
            if oldloc != Path::new(v) {
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
