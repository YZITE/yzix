use super::TaskBoundResponse;
use camino::Utf8Path;
use core::future::Future;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::{marker::Unpin, mem::drop, path::Path, sync::Arc};
use tokio::sync::broadcast::Sender;
use tracing::trace;
use visit_bytes::Element as _;
use yzix_core::{
    BuildError, DumpFlags, OutputName, Regular, StoreHash, TaggedHash,
    ThinTree,
};

struct LogFromPipe<T> {
    stream: tokio::io::Lines<tokio::io::BufReader<T>>,
}

impl<T: tokio::io::AsyncRead + Unpin> LogFromPipe<T> {
    #[inline]
    fn new(pipe: T) -> Self {
        use tokio::io::{AsyncBufReadExt, BufReader};
        Self {
            stream: BufReader::new(pipe).lines(),
        }
    }

    // note: this is cancel-safe
    #[inline]
    fn recv(&mut self) -> impl Future<Output = std::io::Result<Option<String>>> + '_ {
        self.stream.next_line()
    }
}

struct LogToFile<'p> {
    loutp: &'p Path,
    fout: Option<async_compression::tokio::write::ZstdEncoder<tokio::fs::File>>,
    got_any_content: bool,
}

impl<'p> LogToFile<'p> {
    async fn try_new(loutp: &'p Path) -> std::io::Result<Self> {
        let fout = async_compression::tokio::write::ZstdEncoder::with_quality(
            tokio::fs::File::create(loutp).await?,
            async_compression::Level::Best,
        );
        Ok(Self {
            loutp,
            fout: Some(fout),
            got_any_content: false,
        })
    }

    #[inline]
    fn process_line<'a>(&'a mut self, content_wnl: &'a str) -> impl Future<Output = std::io::Result<()>> + 'a {
        use tokio::io::AsyncWriteExt;
        self.got_any_content = true;
        self.fout.as_mut().unwrap().write_all(content_wnl.as_bytes())
    }

    async fn shutdown(mut self) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        if let Some(mut fout) = self.fout.take() {
            fout.flush().await?;
            fout.shutdown().await?;
        }

        if !self.got_any_content {
            trace!("log is empty -> remove");
            tokio::fs::remove_file(self.loutp).await?;
        }
        Ok(())
    }
}

struct LogForwarder<'p> {
    stdout2log: LogFromPipe<tokio::process::ChildStdout>,
    stderr2log: LogFromPipe<tokio::process::ChildStderr>,

    log2file: LogToFile<'p>,
    log2global: Sender<Arc<TaskBoundResponse>>,
}

impl<'p> LogForwarder<'p> {
    async fn try_new(child: &mut tokio::process::Child, file_loutp: &'p Path, glb_loutp: Sender<Arc<TaskBoundResponse>>) -> std::io::Result<Self> {
        let stdout2log = LogFromPipe::new(child.stdout.take().unwrap());
        let stderr2log = LogFromPipe::new(child.stderr.take().unwrap());

        let log2file = LogToFile::try_new(file_loutp).await?;

        Ok(Self {
            stdout2log,
            stderr2log,
            log2file,
            log2global: glb_loutp,
        })
    }

    async fn main_loop(self) -> std::io::Result<()> {
        let LogForwarder { mut stdout2log, mut stderr2log, mut log2file, log2global } = self;

        let res = loop {
            let content = tokio::select! {
                x = stdout2log.recv() => x,
                x = stderr2log.recv() => x,
            };
            let mut content = match content {
                Err(e) => break Err(e),
                Ok(None) => break Ok(()),
                Ok(Some(x)) => x,
            };

            content.push('\n');
            let a = log2file.process_line(&content).await;
            content.pop();
            content.shrink_to_fit();
            let b = log2global.send(Arc::new(TaskBoundResponse::Log(content)));

            if a.is_err() {
                break a;
            }

            if b.is_err() {
                break Ok(());
            }
        };
        log2file.shutdown().await?;
        res
    }
}

async fn build_linux_ocirt_spec(
    store_path: &Utf8Path,
    rootdir: &Path,
    args: Vec<String>,
    env: Vec<String>,
    refs: BTreeSet<TaggedHash<ThinTree>>,
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
    let mut ropaths = osr::get_default_readonly_paths();
    if !refs.is_empty() {
        let fake_store = rootdir.join(&store_path.as_str()[1..]);
        tokio::fs::create_dir_all(&fake_store).await?;
        for i in refs {
            let ias = i.to_string();
            let spath = store_path.join(&ias);
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
                ropaths.push(spath.to_string());
                tokio::fs::create_dir(dpath).await?;
            } else {
                // we don't use hard links (for now)
                // because that would require us to put the fake store
                // in a subdirectory of the real store,
                // and would require us to write the (to-be-rewritten)
                // output paths there, too, which is bad for SSDs...
                //
                // side note: nixpkgs builds often copy whole source trees...
                // so this probably isn't too bad.
                tokio::fs::copy(spath, dpath).await?;
            }
        }
    }
    let mut caps = HashSet::new();
    caps.insert(osr::Capability::BlockSuspend);
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
                .cwd("/build")
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

#[inline]
fn placeholder(name: &OutputName) -> StoreHash {
    StoreHash::hash_complex::<OutputName>(name)
}

pub struct HandleProcessArgs<'a> {
    pub(crate) env: &'a crate::Env,
    pub container_name: &'a str,
    pub logs: Sender<Arc<TaskBoundResponse>>,
}

// NOTE: in the returned outputs, the hash is modulo the outputs
pub async fn handle_process(
    HandleProcessArgs {
        env,
        container_name,
        logs,
    }: HandleProcessArgs<'_>,
    crate::FullWorkItem {
        inhash,
        refs,
        inner:
            yzix_core::WorkItem {
                args,
                mut envs,
                outputs,
                files,
            },
    }: crate::FullWorkItem,
) -> Result<BTreeMap<OutputName, (TaggedHash<ThinTree>, ThinTree)>, BuildError> {
    if args.is_empty() || args[0].is_empty() {
        return Err(BuildError::EmptyCommand);
    }

    let store_path = &env.store_path;
    let workdir = tempfile::tempdir()?;
    let rootdir = workdir.path().join("rootfs");
    let logoutput = store_path.join(format!("{}.log.zst", inhash));

    std::fs::create_dir_all(&rootdir)?;
    let builddir = rootdir.join("build");
    std::fs::create_dir_all(&builddir)?;
    std::fs::create_dir_all(rootdir.join("tmp"))?;

    for (key, value) in [
        ("HOME", "/homeless-shelter"),
        //("LC_ALL", "C.UTF-8"),
        ("LC_ALL", "C"),
        ("NIX_BUILD_TOP", "/build"),
        ("NIX_STORE", store_path.as_str()),
        // from nixpkgs/pkgs/stdenv/generic/setup.sh:
        /****
          Set a fallback default value for SOURCE_DATE_EPOCH, used by some build tools
          to provide a deterministic substitute for the "current" time. Note that
          315532800 = 1980-01-01 12:00:00. We use this date because python's wheel
          implementation uses zip archive and zip does not support dates going back to
          1970.
        ****/
        ("SOURCE_DATE_EPOCH", "315532800"),
        // trigger colored output in various tools
        ("TERM", "xterm-256color"),
        ("TZ", "UTC"),
    ] {
        envs.entry(key.to_string())
            .or_insert_with(|| value.to_string());
    }

    let outputs: BTreeMap<_, _> = outputs
        .into_iter()
        .map(|name| {
            // this is really unsafe, we just hope we won't have any collisions
            let plh = TaggedHash::unsafe_cast(placeholder(&name));
            (name, plh)
        })
        .collect();

    for (k, v) in &outputs {
        envs.insert(k.to_string(), store_path.join(&v.to_string()).into_string());
    }

    for (fnam, fdump) in files {
        tokio::task::block_in_place(|| {
            fdump.write_to_path(
                &builddir.join(&*fnam),
                DumpFlags {
                    force: true,
                    make_readonly: false,
                },
                &crate::mk_request_cafile(store_path),
            )
        })?;
    }

    drop(builddir);

    // generate spec
    {
        let spec = build_linux_ocirt_spec(
            store_path,
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

    trace!("environment settings serialized");

    use std::process::Stdio;
    let mut ch = tokio::process::Command::new(&env.container_runner)
        .args(vec![
            "--root".to_string(),
            store_path.join(".runc").into_string(),
            "run".to_string(),
            container_name.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(workdir.path())
        .kill_on_drop(true)
        .spawn()?;
    let logfwd = LogForwarder::try_new(&mut ch, logoutput.as_std_path(), logs).await?;
    trace!("runner + logfwd started");
    let (y, exs) = tokio::join!(logfwd.main_loop(), ch.wait());
    let (_, exs) = (y?, exs?);

    if exs.success() {
        let fake_store = rootdir.join(&store_path.as_str()[1..]);
        let stspec = crate::store_refs::build_store_spec(store_path);
        let dtctrefs: BTreeSet<_> = outputs.values().copied().collect();
        let mut outputs: BTreeMap<_, _> = outputs
            .into_iter()
            .map(|(i, plh)| {
                tokio::task::block_in_place(|| {
                    let semitree = ThinTree::read_from_path(
                        &fake_store.join(&plh.to_string()),
                        &mut |rh, regu: Regular| {
                            use yzix_core::ThinTreeSubmitError as Stse;
                            let mut dtct = crate::store_refs::Contains {
                                spec: &stspec,
                                refs: &dtctrefs,
                                cont: false,
                            };
                            regu.accept(&mut dtct);
                            if dtct.cont {
                                // regu contains "self-references" to some outputs...
                                Err(Stse::StoreLoop(regu))
                            } else {
                                // copy file directly
                                crate::register_cafile(
                                    store_path,
                                    &env.store_cafiles_locks,
                                    rh,
                                    regu,
                                )
                                .map_err(Stse::StoreErr)
                            }
                        },
                    )?;
                    let outhash = TaggedHash::<ThinTree>::hash_complex(&semitree);
                    Ok::<_, yzix_core::StoreError>((i, (plh, semitree, outhash)))
                })
            })
            .collect::<Result<_, _>>()?;
        drop(dtctrefs);
        {
            let rwtr = crate::store_refs::Rewrite {
                spec: &stspec,
                rwtab: outputs
                    .values()
                    .map(|&(plh, _, outhash)| (plh, outhash))
                    .collect(),
            };
            for (_, v, _) in &mut outputs.values_mut() {
                v.accept_mut(&mut &rwtr);
            }
        }
        outputs.values_mut().try_for_each(|(_, i, _)| {
            i.submit_all_inlines(&mut |rh, regu: Regular| {
                crate::register_cafile(store_path, &env.store_cafiles_locks, rh, regu)
            })
        })?;
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
