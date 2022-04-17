use camino::Utf8Path;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::{marker::Unpin, path::Path, sync::Arc};
use tokio::sync::broadcast::{self, Receiver, Sender};
use tracing::Level;
use yzix_proto::{
    store::Dump, store::Hash as StoreHash, strwrappers::OutputName, TaskBoundResponse,
};

async fn handle_logging_to_intermed<T: tokio::io::AsyncRead + Unpin>(
    log: Sender<String>,
    pipe: T,
) -> std::io::Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut stream = BufReader::new(pipe).lines();
    while let Some(content) = stream.next_line().await? {
        if log.send(content).is_err() {
            break;
        }
    }
    Ok(())
}

async fn handle_logging_to_file(mut linp: Receiver<String>, loutp: &Path) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut fout = async_compression::tokio::write::ZstdEncoder::with_quality(
        tokio::fs::File::create(loutp).await?,
        async_compression::Level::Best,
    );
    while let Ok(mut content) = linp.recv().await {
        content.push('\n');
        fout.write_all(content.as_bytes()).await?;
    }
    fout.flush().await?;
    fout.shutdown().await?;
    Ok(())
}

async fn handle_logging_to_global(
    tid: StoreHash,
    mut linp: Receiver<String>,
    loutp: Sender<(StoreHash, Arc<TaskBoundResponse>)>,
) {
    while let Ok(content) = linp.recv().await {
        if loutp
            .send((tid, Arc::new(TaskBoundResponse::Log(content))))
            .is_err()
        {
            break;
        }
    }
}

async fn build_linux_ocirt_spec(
    config: &crate::ServerConfig,
    rootdir: &Path,
    args: Vec<String>,
    env: Vec<String>,
    refs: BTreeSet<crate::StoreHash>,
) -> std::io::Result<oci_spec::runtime::Spec> {
    // NOTE: windows support is harder, because we need to use a hyperv container there...
    use oci_spec::runtime as osr;
    let mut mounts = osr::get_default_mounts();
    // rootless: filter 'gid=' options in mount args
    mounts
        .iter_mut()
        .filter(|i| {
            i.options()
                .as_ref()
                .map(|i2| i2.iter().any(|j: &String| j.starts_with("gid=")))
                .unwrap_or(false)
        })
        .for_each(|i| {
            let newopts: Vec<_> = i
                .options()
                .as_ref()
                .unwrap()
                .iter()
                .filter(|j| !j.starts_with("gid="))
                .cloned()
                .collect();
            *i = osr::MountBuilder::default()
                .destination(i.destination())
                .typ(i.typ().as_ref().unwrap())
                .source(i.source().as_ref().unwrap())
                .options(newopts)
                .build()
                .unwrap();
        });
    if !refs.is_empty() {
        let fake_store = rootdir.join(&config.store_path.as_str()[1..]);
        tokio::fs::create_dir_all(&fake_store).await?;
        for i in refs {
            let ias = i.to_string();
            let spath = config.store_path.join(&ias);
            let dpath = fake_store.join(&ias);

            if spath.is_dir() {
                mounts.push(
                    osr::MountBuilder::default()
                        .destination(&spath)
                        .source(&spath)
                        .typ("none")
                        .options(vec!["bind".to_string()])
                        .build()
                        .unwrap(),
                );
                tokio::fs::create_dir(dpath).await?;
            } else {
                tokio::fs::hard_link(spath, dpath).await?;
            }
        }
    }
    let mut caps = HashSet::new();
    caps.insert(osr::Capability::BlockSuspend);
    let ropaths = osr::get_default_readonly_paths();
    let mut namespaces = osr::get_default_namespaces();
    namespaces.push(
        osr::LinuxNamespaceBuilder::default()
            .typ(osr::LinuxNamespaceType::User)
            .build()
            .unwrap(),
    );
    let spec = osr::SpecBuilder::default()
        .version("1.0.0")
        .root(
            osr::RootBuilder::default()
                .path(rootdir)
                .readonly(false)
                .build()
                .unwrap(),
        )
        .hostname("yzix")
        .mounts(mounts)
        .process(
            osr::ProcessBuilder::default()
                .terminal(false)
                .user(
                    osr::UserBuilder::default()
                        .uid(0u32)
                        .gid(0u32)
                        .build()
                        .unwrap(),
                )
                .args(args)
                .env(env)
                .cwd("/")
                .capabilities(
                    osr::LinuxCapabilitiesBuilder::default()
                        .bounding(caps.clone())
                        .effective(caps.clone())
                        .inheritable(caps.clone())
                        .permitted(caps.clone())
                        .ambient(caps)
                        .build()
                        .unwrap(),
                )
                .rlimits(vec![osr::LinuxRlimitBuilder::default()
                    .typ(osr::LinuxRlimitType::RlimitNofile)
                    .hard(4096u64)
                    .soft(1024u64)
                    .build()
                    .unwrap()])
                .no_new_privileges(true)
                .build()
                .unwrap(),
        )
        .linux(
            osr::LinuxBuilder::default()
                .namespaces(namespaces)
                .masked_paths(osr::get_default_maskedpaths())
                .readonly_paths(ropaths)
                .build()
                .unwrap(),
        )
        .build()
        .unwrap();

    Ok(spec)
}

mod extract_store_refs {
    use std::collections::BTreeSet;
    use std::str::FromStr;
    use store_ref_scanner::StoreSpec;
    use yzix_proto::store::{Dump, Hash as StoreHash};

    fn from_vec<'a>(spec: &'a StoreSpec, dat: &'a [u8]) -> impl Iterator<Item = StoreHash> + 'a {
        store_ref_scanner::StoreRefScanner::new(dat, spec)
            // SAFETY: we know that only ASCII chars are possible here
            .map(|x| StoreHash::from_str(std::str::from_utf8(x).unwrap()).unwrap())
    }

    pub fn from_dump(spec: &StoreSpec, dump: &Dump, refs: &mut BTreeSet<StoreHash>) {
        match dump {
            Dump::Regular { contents, .. } => {
                refs.extend(from_vec(spec, contents));
            }
            Dump::SymLink { target } => {
                refs.extend(from_vec(spec, target.as_str().as_bytes()));
            }
            Dump::Directory(dir) => {
                dir.values().for_each(|v| from_dump(spec, v, refs));
            }
        }
    }
}

mod rewrite_store_refs {
    use std::str::FromStr;
    use store_ref_scanner::StoreSpec;
    use yzix_proto::store::{Dump, Hash as StoreHash};

    type RwTab = std::collections::BTreeMap<StoreHash, StoreHash>;

    fn in_vec(spec: &StoreSpec, rwtab: &RwTab, dat: &mut [u8]) {
        for i in store_ref_scanner::StoreRefScanner::new(dat, spec) {
            // SAFETY: we know that only ASCII chars are possible here
            let oldhash = StoreHash::from_str(std::str::from_utf8(i).unwrap()).unwrap();
            if let Some(newhash) = rwtab.get(&oldhash) {
                tracing::info!(%oldhash, %newhash, "rewrote store ref");
                i.copy_from_slice(newhash.to_string().as_bytes());
            }
        }
    }

    pub fn in_dump(spec: &StoreSpec, rwtab: &RwTab, dump: &mut Dump) {
        match dump {
            Dump::Regular { contents, .. } => {
                in_vec(spec, rwtab, contents);
            }
            Dump::SymLink { target } => {
                let mut tmp_target: Vec<u8> = target.as_str().bytes().collect();
                in_vec(spec, rwtab, &mut tmp_target[..]);
                *target = String::from_utf8(tmp_target)
                    .expect("illegal hash characters used")
                    .into();
            }
            Dump::Directory(dir) => {
                dir.values_mut().for_each(|v| in_dump(spec, rwtab, v));
            }
        }
    }
}

pub fn build_store_spec(store_path: &Utf8Path) -> store_ref_scanner::StoreSpec<'_> {
    use store_ref_scanner::StoreSpec;
    StoreSpec {
        path_to_store: store_path.as_str(),
        // necessary because we don't attach names to store entries
        // and we don't want to pull in characters after the name
        // (might happen when we parse a CBOR serialized message)
        // if the name is not terminated with a slash.
        valid_restbytes: Default::default(),
        ..StoreSpec::DFL_YZIX1
    }
}

pub fn determine_store_closure(store_path: &Utf8Path, refs: &mut BTreeSet<StoreHash>) {
    let stspec = build_store_spec(store_path);
    let mut new_refs = std::mem::take(refs);

    while !new_refs.is_empty() {
        for i in std::mem::take(&mut new_refs) {
            refs.insert(i);
            if let Ok(dump) = Dump::read_from_path(store_path.join(i.to_string()).as_std_path()) {
                extract_store_refs::from_dump(&stspec, &dump, &mut new_refs);
                new_refs.retain(|r| !refs.contains(r));
            }
        }
    }
}

pub fn random_name() -> String {
    use rand::prelude::*;
    let mut rng = rand::thread_rng();
    std::iter::repeat(())
        .take(20)
        .map(|()| char::from_u32(rng.gen_range(b'a'..=b'z').into()).unwrap())
        .collect::<String>()
}

fn dfl_env_var(envs: &mut BTreeMap<String, String>, key: &str, value: &str) {
    envs.entry(key.to_string())
        .or_insert_with(|| value.to_string());
}

#[inline]
pub fn placeholder(name: &OutputName) -> StoreHash {
    StoreHash::hash_complex::<OutputName>(name)
}

// NOTE: in the returned outputs, the hash is modulo the outputs
pub async fn handle_process(
    config: &crate::ServerConfig,
    logs: &Sender<(StoreHash, Arc<TaskBoundResponse>)>,
    container_name: &str,
    crate::FullWorkItem {
        inhash,
        refs,
        inner:
            yzix_proto::WorkItem {
                args,
                mut envs,
                outputs,
            },
    }: crate::FullWorkItem,
) -> Result<BTreeMap<OutputName, (StoreHash, Dump)>, yzix_proto::BuildError> {
    use yzix_proto::BuildError;

    if args.is_empty() || args[0].is_empty() {
        return Err(BuildError::EmptyCommand);
    }

    let span = tracing::span!(Level::ERROR, "handle_process", %inhash, ?args, ?envs);
    let _guard = span.enter();

    let workdir = tempfile::tempdir()?;
    let rootdir = workdir.path().join("rootfs");
    let logoutput = config.store_path.join(format!("{}.log.zst", inhash));

    std::fs::create_dir_all(&rootdir)?;

    dfl_env_var(&mut envs, "LC_ALL", "C.UTF-8");
    dfl_env_var(&mut envs, "TZ", "UTC");

    let outputs: BTreeMap<_, _> = outputs
        .into_iter()
        .map(|name| {
            let plh = placeholder(&name);
            (name, plh)
        })
        .collect();

    for (k, v) in &outputs {
        envs.insert(
            k.to_string(),
            config.store_path.join(&v.to_string()).into_string(),
        );
    }

    // generate spec
    {
        let spec = build_linux_ocirt_spec(
            config,
            &rootdir,
            args,
            envs.into_iter()
                .map(|(i, j)| format!("{}={}", i, j))
                .collect(),
            refs,
        )
        .await?;
        tokio::fs::write(
            workdir.path().join("config.json"),
            serde_json::to_string(&spec).map_err(|e| BuildError::Unknown(e.to_string()))?,
        )
        .await?;
    }

    use std::process::Stdio;
    let mut ch = tokio::process::Command::new(&config.container_runner)
        .args(vec![
            "--root".to_string(),
            config.store_path.join(".runc").into_string(),
            "run".to_string(),
            container_name.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(workdir.path())
        .kill_on_drop(true)
        .spawn()?;
    let (logfwds, logfwdr) = broadcast::channel(10000);
    let w = handle_logging_to_intermed(logfwds.clone(), ch.stdout.take().unwrap());
    let x = handle_logging_to_intermed(logfwds.clone(), ch.stderr.take().unwrap());
    let y = handle_logging_to_file(logfwds.subscribe(), logoutput.as_std_path());
    let z = handle_logging_to_global(inhash, logfwdr, logs.clone());
    let _ = logfwds;

    let (_, _, y, _, exs) = tokio::join!(w, x, y, z, ch.wait());
    let (_, exs) = (y?, exs?);

    if exs.success() {
        let fake_store = rootdir.join(&config.store_path.as_str()[1..]);
        let mut outputs: BTreeMap<_, _> = outputs
            .into_iter()
            .map(|(i, plh)| {
                tokio::task::block_in_place(|| {
                    let dump = Dump::read_from_path(&fake_store.join(&plh.to_string()))?;
                    let outhash = StoreHash::hash_complex::<Dump>(&dump);
                    Ok::<_, yzix_proto::store::Error>((i, (plh, dump, outhash)))
                })
            })
            .collect::<Result<_, _>>()?;
        {
            // rewrite hashes...; this breaks our ability to verify the path, but nah...
            let rwtab: BTreeMap<_, _> = outputs
                .values()
                .map(|&(plh, _, outhash)| (plh, outhash))
                .collect();
            use store_ref_scanner::StoreSpec;
            let stspec = StoreSpec {
                path_to_store: config.store_path.as_str(),
                ..StoreSpec::DFL_YZIX1
            };
            for (_, v, _) in &mut outputs.values_mut() {
                rewrite_store_refs::in_dump(&stspec, &rwtab, v);
            }
        }
        Ok(outputs
            .into_iter()
            .map(|(k, (_, v, h))| (k, (h, v)))
            .collect())
    } else if let Some(x) = exs.code() {
        Err(BuildError::Exit(x))
    } else {
        #[cfg(unix)]
        if let Some(x) = std::os::unix::process::ExitStatusExt::signal(&exs) {
            return Err(BuildError::Killed(x));
        }

        Err(BuildError::Unknown(exs.to_string()))
    }
}
