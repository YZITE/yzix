use camino::Utf8Path;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::{marker::Unpin, mem::drop, path::Path, sync::Arc};
use tokio::sync::broadcast::{self, Receiver, Sender};
use tracing::trace;
use yzix_core::{
    visit_bytes::Element as _, BuildError, DumpFlags, OutputName, Regular, StoreHash, TaggedHash,
    TaskBoundResponse, ThinTree,
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
    let mut got_any_content = false;
    while let Ok(mut content) = linp.recv().await {
        content.push('\n');
        got_any_content = true;
        fout.write_all(content.as_bytes()).await?;
    }
    fout.flush().await?;
    fout.shutdown().await?;

    std::mem::drop(fout);
    if !got_any_content {
        trace!("log is empty -> remove");
        tokio::fs::remove_file(loutp).await?;
    }
    Ok(())
}

async fn handle_logging_to_global(
    mut linp: Receiver<String>,
    loutp: Sender<Arc<TaskBoundResponse>>,
) {
    while let Ok(content) = linp.recv().await {
        if loutp
            .send(Arc::new(TaskBoundResponse::Log(content)))
            .is_err()
        {
            break;
        }
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
    let (logfwds, logfwdr) = broadcast::channel(1000);
    let z = handle_logging_to_global(logfwdr, logs);
    let y = handle_logging_to_file(logfwds.subscribe(), logoutput.as_std_path());
    let x = handle_logging_to_intermed(logfwds.clone(), ch.stderr.take().unwrap());
    let w = handle_logging_to_intermed(logfwds, ch.stdout.take().unwrap());
    trace!("runner + logfwd started");

    let (_, _, y, _, exs) = tokio::join!(w, x, y, z, ch.wait());
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
