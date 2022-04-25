//! keep in mind that this executable usually requires root privileges,
//! otherwise it would be unable to get a complete root list.

use camino::{Utf8Path, Utf8PathBuf};
use std::collections::BTreeSet;
use std::io::ErrorKind as Ek;
use tracing::{debug, error, info, span, trace, warn, Level};
use walkdir::WalkDir;
use yzix_core::{visit_bytes as yvb, StoreHash};

#[derive(Debug, clap::Parser)]
struct Args {
    #[clap(long, short = 's')]
    store_path: Utf8PathBuf,

    #[clap(long, short = 'r')]
    roots_dir: Utf8PathBuf,

    #[clap(long, short = 'n')]
    dry_run: bool,

    #[clap(long)]
    print_roots: bool,
}

struct LivePaths {
    store_path: Utf8PathBuf,
    lpi: BTreeSet<StoreHash>,
}

impl LivePaths {
    fn gather_from_gc_roots_dir(&mut self, roots_dir: &Utf8Path) -> std::io::Result<()> {
        let store_spec = yzix_store_builder::store_refs::build_store_spec(&self.store_path);

        for entry in WalkDir::new(roots_dir).min_depth(1) {
            let entry = entry?;
            let ft = entry.file_type();
            if !ft.is_symlink() {
                continue;
            }
            let entry_path = entry.path();
            let span = span!(Level::ERROR, "gc root", ?entry_path);
            let _g = span.enter();
            debug!("follow root");
            let mut trg = std::fs::read_link(entry_path)?;
            for _ in 0..10 {
                debug!("-> {:?}", trg);
                match std::fs::read_link(&trg) {
                    Ok(x) => trg = x,
                    Err(e) => match e.kind() {
                        Ek::InvalidInput => break,
                        Ek::NotFound => {
                            info!("root points to non-existing location, removing");
                            let _ = std::fs::remove_file(entry_path)?;
                            continue;
                        }
                        _ => return Err(e),
                    },
                }
            }
            let span = span!(Level::ERROR, "resolved", ?trg);
            let _g = span.enter();
            if std::fs::read_link(&trg).is_ok() {
                warn!("symlink loop resolving failed, ignoring");
                continue;
            }

            let trg = match trg.to_str() {
                Some(x) => Utf8PathBuf::from(path_clean::clean(x)),
                None => {
                    warn!("unable to convert target to UTF-8 string, ignoring");
                    continue;
                }
            };
            let bp = if let Ok(x) = trg.strip_prefix(&self.store_path) {
                use camino::Utf8Component as Comp;
                match x.components().next() {
                    None => {
                        warn!("points to store, removing");
                        let _ = std::fs::remove_file(entry_path)?;
                        continue;
                    }
                    Some(Comp::Normal(y)) => y,
                    Some(y) => {
                        // maybe temporary root
                        if trg.is_file() {
                            if let Some(Comp::Normal(y)) = x.components().last() {
                                if y.chars().all(|i| i.is_ascii_digit()) {
                                    debug!("... is temporary root");
                                    use fs2::FileExt;
                                    use yvb::Visitor;
                                    let mut f =
                                        match std::fs::OpenOptions::new().read(true).open(&trg) {
                                            Ok(f) => f,
                                            Err(e) => match e.kind() {
                                                Ek::NotFound => {
                                                    let _ = std::fs::remove_file(entry_path)?;
                                                    continue;
                                                }
                                                _ => return Err(e),
                                            },
                                        };
                                    // if we can simply aquire a lock, the root is dead
                                    match f.try_lock_exclusive() {
                                        Ok(()) => {
                                            let _ = f.unlock();
                                            info!("removing stale temporary root");
                                            std::mem::drop(f);
                                            let _ = std::fs::remove_file(entry_path)?;
                                            continue;
                                        }
                                        Err(e) => match e.kind() {
                                            Ek::WouldBlock => {}
                                            _ => return Err(e),
                                        },
                                    }
                                    use std::io::Read;
                                    let mut content = Vec::new();
                                    f.read_to_end(&mut content)?;
                                    std::mem::drop(f);
                                    let mut e = yzix_store_builder::store_refs::Extract {
                                        spec: &store_spec,
                                        refs: Default::default(),
                                    };
                                    e.visit_bytes(&content[..]);
                                    for i in &e.refs {
                                        debug!("=> {}", i);
                                    }
                                    self.lpi.extend(e.refs.into_iter());
                                    continue;
                                }
                            }
                        }
                        warn!("path starts weird: {:?}, ignoring", y);
                        continue;
                    }
                }
            } else {
                warn!("it points outside of the store, ignoring");
                continue;
            };

            let h = match bp.parse::<StoreHash>() {
                Ok(h) => h,
                Err(_) => {
                    warn!("it points to a non-store-hash path, ignoring");
                    continue;
                }
            };

            debug!("=> {}", h);
            self.lpi.insert(h);
        }

        Ok(())
    }

    fn gather_from_runtime_per_pid(&mut self, epath: &std::path::Path) -> std::io::Result<()> {
        use yvb::Visitor;
        use yzix_store_builder::store_refs::Extract;

        fn read_proc_link(
            extractor: &mut Extract<'_>,
            xpath: &std::path::Path,
        ) -> std::io::Result<()> {
            let x = std::fs::read_link(xpath)?;
            if let Ok(y) = x.into_os_string().into_string() {
                extractor.visit_bytes(y.as_bytes());
            }
            Ok(())
        }

        let store_spec = yzix_store_builder::store_refs::build_store_spec(&self.store_path);
        let mut e = Extract {
            spec: &store_spec,
            refs: Default::default(),
        };
        read_proc_link(&mut e, &epath.join("exe"))?;
        read_proc_link(&mut e, &epath.join("cwd"))?;

        for entry in std::fs::read_dir(epath.join("fd"))? {
            let entry = entry?;
            read_proc_link(&mut e, &entry.path())?;
        }

        e.visit_bytes(&std::fs::read(epath.join("maps"))?[..]);
        e.visit_bytes(&std::fs::read(epath.join("environ"))?[..]);

        for h in &e.refs {
            debug!("=> {}", h);
        }

        self.lpi.extend(e.refs.into_iter());
        Ok(())
    }

    fn gather_from_runtime(&mut self) -> std::io::Result<()> {
        for entry in std::fs::read_dir("/proc")? {
            let entry = entry?;
            let bn = match entry.file_name().into_string() {
                Ok(x) => x,
                Err(_) => continue,
            };
            if !bn.chars().all(|i| i.is_ascii_digit()) {
                // not a process identifier, ignoring
                continue;
            }
            let bn: &str = &bn;
            let span = span!(Level::ERROR, "runtime root", bn);
            let _g = span.enter();
            trace!("follow root");
            match self.gather_from_runtime_per_pid(&entry.path()) {
                Ok(()) => {}
                Err(e) => {
                    // TODO: improve? this is flaky
                    if e.raw_os_error() == Some(3) || e.kind() == Ek::NotFound {
                        trace!("{:?}", e);
                        // ESRCH
                        continue;
                    } else if e.kind() == Ek::PermissionDenied {
                        // EACCESS
                        warn!("{:?}", e);
                        continue;
                    } else {
                        error!("{:?}", e);
                        return Err(e);
                    }
                }
            }
        }
        Ok(())
    }
}

fn main() -> std::io::Result<()> {
    let Args {
        store_path,
        roots_dir,
        dry_run,
        print_roots,
    } = <Args as clap::Parser>::parse();
    tracing_subscriber::fmt::init();

    let mut lp = LivePaths {
        store_path,
        lpi: Default::default(),
    };

    info!("gather from roots directory ...");
    lp.gather_from_gc_roots_dir(&roots_dir)?;

    info!("gather from running processes ...");
    lp.gather_from_runtime()?;

    info!("determine closure ...");
    {
        use yzix_store_builder::store_refs;
        let mut cache = store_refs::Cache::new(10000);
        store_refs::determine_store_closure(&lp.store_path, &mut cache, &mut lp.lpi);
    }

    if print_roots {
        for h in &lp.lpi {
            println!("{}", h);
        }
    }

    let trash = lp.store_path.join("trash");
    if dry_run {
        info!("unselected paths:");
    } else {
        info!("move all unselected paths to trash ...");
        std::fs::create_dir(trash.as_std_path())?;
    }

    macro_rules! handlerr {
        ($inp:expr, $e:pat, $ebody:expr) => {{
            match $inp {
                Ok(x) => x,
                Err($e) => {
                    $ebody;
                    continue;
                }
            }
        }};
    }

    for entry in std::fs::read_dir(lp.store_path.as_std_path())? {
        let entry = handlerr!(entry, e, error!("during traversal: {:?}", e));

        let bn = handlerr!(
            entry.file_name().into_string(),
            e,
            error!("got invalid store path: {:?}", e)
        );

        // don't warn about this, e.g. all realisations are catched here...
        let h = handlerr!(bn.parse::<StoreHash>(), _, {});

        if lp.lpi.contains(&h) {
            continue;
        }

        info!("... {}", h);
        if !dry_run {
            std::fs::rename(entry.path(), trash.join(h.to_string()))?;
        }
    }

    if dry_run {
        return Ok(());
    } else {
        info!("removing stale realisations ...");
    }

    for entry in std::fs::read_dir(lp.store_path.as_std_path())? {
        let entry = handlerr!(entry, e, error!("during traversal: {:?}", e));

        // we probably printed the error already in the previous crawl,
        // don't repeat it.
        let bn = handlerr!(entry.file_name().into_string(), _, {});

        let hpart = match bn.strip_suffix(".in") {
            Some(x) => x,
            None => continue,
        };

        let h = handlerr!(hpart.parse::<StoreHash>(), _, {});

        let content = yzix_store_builder::in2_helpers::resolve_in2(&entry.path());

        // if any build result was moved, remove the realisation
        // TODO: make it possible to mark realisations as incomplete,
        // so that they get rebuilt only if necessary
        if !content.values().any(|i| !lp.lpi.contains(i)) {
            continue;
        }
        info!("... {}", h);
        if !dry_run {
            std::fs::rename(entry.path(), trash.join(format!("{}.in", h)))?;
        }
    }

    if !dry_run {
        info!("removing trash");
        std::fs::remove_dir_all(trash.as_std_path())?;
    }

    Ok(())
}
