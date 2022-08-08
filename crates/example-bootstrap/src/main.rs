use indoc::indoc;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tracing::{error, info};
use yzix_client::{Driver, OutputName, Regular, StoreHash, TaggedHash, ThinTree, WorkItem};

async fn my_fetch(url: &str) -> Result<Vec<u8>, reqwest::Error> {
    Ok(reqwest::get(url).await?.bytes().await?.as_ref().to_vec())
}

async fn fetchurl_outside_store(
    url: &str,
    expect_hash: &str,
    with_path: &str,
    executable: bool,
) -> anyhow::Result<ThinTree> {
    let h = expect_hash.parse::<StoreHash>().unwrap();

    info!("fetching {} ...", url);
    let contents = my_fetch(url).await?;
    info!("fetching {} ... done", url);

    let mut dump = ThinTree::RegularInline(Regular {
        executable,
        contents,
    });

    if !with_path.is_empty() {
        for i in with_path.split('/') {
            dump = ThinTree::Directory(
                std::iter::once((i.to_string().try_into().unwrap(), dump)).collect(),
            );
        }
    }

    let mut dump2 = dump.clone();
    dump2.submit_all_inlines(&mut |_, _| Ok(())).unwrap();
    let h2 = StoreHash::hash_complex(&dump2);
    if h2 != h {
        error!(
            "fetchurl ({}): hash mismatch, expected = {}, got = {}",
            url, expect_hash, h2
        );
        anyhow::bail!("hash mismatch");
    }

    Ok(dump)
}

async fn fetchurl(
    driver: &Driver,
    url: &str,
    expect_hash: &str,
    with_path: &str,
    executable: bool,
) -> anyhow::Result<TaggedHash<ThinTree>> {
    let h = expect_hash.parse::<TaggedHash<ThinTree>>().unwrap();

    if driver.has_out_hash(h).await {
        return Ok(h);
    }

    let dump = fetchurl_outside_store(url, expect_hash, with_path, executable).await?;

    let x = driver.upload(h, &dump).await;
    info!("fetchurl ({}): {:?}", url, x);

    Ok(h)
}

async fn fetchurl_wrapped(
    driver: &Driver,
    url: &str,
    expect_hash: &str,
    with_path: &str,
    executable: bool,
) -> Result<TaggedHash<ThinTree>, ()> {
    fetchurl(driver, url, expect_hash, with_path, executable)
        .await
        .map_err(|e| {
            error!("{} => {:?}", url, e);
        })
}

fn mk_envs(elems: Vec<(&str, String)>) -> BTreeMap<String, String> {
    elems.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn mk_outputs(elems: Vec<&str>) -> BTreeSet<OutputName> {
    elems
        .into_iter()
        .map(|i| OutputName::new(i.to_string()).unwrap())
        .collect()
}

fn mk_envfiles(elems: Vec<(&str, ThinTree)>) -> BTreeMap<yzix_client::BaseName, ThinTree> {
    elems
        .into_iter()
        .map(|(k, v)| (k.to_string().try_into().unwrap(), v))
        .collect()
}

async fn smart_upload(driver: &Driver, dump: ThinTree) -> anyhow::Result<TaggedHash<ThinTree>> {
    let mut dump2 = dump.clone();
    dump2.submit_all_inlines(&mut |_, _| Ok(())).unwrap();
    let h = TaggedHash::hash_complex(&dump2);

    if !driver.has_out_hash(h).await {
        driver.upload(h, &dump).await;
    }

    Ok(h)
}

async fn gen_wrappers(
    driver: &Driver,
    store_path: &str,
    bootstrap_tools: TaggedHash<ThinTree>,
) -> anyhow::Result<TaggedHash<ThinTree>> {
    fn gen_wrapper(
        store_path: &str,
        bootstrap_tools: TaggedHash<ThinTree>,
        element: &str,
    ) -> ThinTree {
        ThinTree::RegularInline(Regular {
            executable: true,
            contents: format!(
                "#!{stp}/{bst}/bin/bash\nexec {stp}/{bst}/bin/{elem} $NIX_WRAPPER_{elemshv}_ARGS \"$@\"\n",
                stp = store_path,
                bst = bootstrap_tools,
                elem = element,
                elemshv = element.replace("++", "xx"),
            )
            .into_bytes(),
        })
    }

    let mut dir: BTreeMap<yzix_client::BaseName, _> = ["gcc", "g++"]
        .into_iter()
        .map(|i| {
            (
                i.to_string().try_into().unwrap(),
                gen_wrapper(store_path, bootstrap_tools, i),
            )
        })
        .collect();

    //for (from, to) in [("gcc", "cc")] {
    //    dir.insert(to.to_string().try_into().unwrap(), dir[from].clone());
    //}
    dir.insert("cc".to_string().try_into().unwrap(), dir["gcc"].clone());

    let mut dump = ThinTree::Directory(dir);
    dump = ThinTree::Directory(
        std::iter::once(("bin".to_string().try_into().unwrap(), dump)).collect(),
    );
    smart_upload(driver, dump).await
}

#[derive(Clone)]
struct Runner {
    notif: tokio::sync::watch::Receiver<Result<BTreeMap<OutputName, TaggedHash<ThinTree>>, ()>>,
}

impl Runner {
    fn new_fetchu(
        driver: &Driver,
        url: &'static str,
        expect_hash: &'static str,
        with_path: &'static str,
        executable: bool,
    ) -> Self {
        let (notif_s, notif) = tokio::sync::watch::channel(Err(()));
        let driver = driver.clone();
        tokio::spawn(async move {
            let res = match fetchurl(&driver, url, expect_hash, with_path, executable).await {
                Ok(out) => Ok(core::iter::once((OutputName::default(), out)).collect()),
                Err(e) => {
                    error!("{} => {:?}", url, e);
                    Err(())
                }
            };
            let _ = notif_s.send(res).is_ok();
        });
        Self { notif }
    }

    fn new_wi(
        driver: &Driver,
        wi: impl std::future::Future<Output = Result<WorkItem, ()>> + Send + 'static,
    ) -> Self {
        let (notif_s, notif) = tokio::sync::watch::channel(Err(()));
        let driver = driver.clone();
        tokio::spawn(async move {
            let res = match wi.await {
                Ok(wi2) => Ok(driver.run_task(wi2).await),
                Err(()) => Err(()),
            };
            let _ = notif_s.send(res).is_ok();
        });
        Self { notif }
    }

    async fn want(&mut self) -> Result<BTreeMap<OutputName, TaggedHash<ThinTree>>, ()> {
        let _ = self.notif.changed().await.is_ok();
        self.notif.borrow_and_update().clone()
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    /* === environment setup === */

    let server_addr = std::env::var("YZIX_SERVER_ADDR").expect("YZIX_SERVER_ADDR env var not set");
    let bearer_token =
        std::env::var("YZIX_BEARER_TOKEN").expect("YZIX_BEARER_TOKEN env var not set");

    // install global log subscriber configured based on RUST_LOG envvar.
    tracing_subscriber::fmt::init();

    let driver = Driver::new(server_addr, bearer_token).await;

    /* === seed === */

    let store_path = Arc::new(driver.store_path().await);

    let h_busybox = fetchurl(
        &driver,
        "http://tarballs.nixos.org/stdenv-linux/i686/4907fc9e8d0d82b28b3c56e3a478a2882f1d700f/busybox",
        "+B850Bjml+k7Fh8azuXKfQR8ZxYgerOhZiBJhD+OwJ4",
        "busybox",
        true,
    );

    let h_bootstrap_tools = fetchurl(
        &driver,
        "http://tarballs.nixos.org/stdenv-linux/x86_64/c5aabb0d603e2c1ea05f5a93b3be82437f5ebf31/bootstrap-tools.tar.xz",
        "VFDhxoFe6d81bQrWP2mHR043nAtWnFAQSe9Bu9W9RaI",
        "",
        false,
    );

    let h_unpack_bootstrap_tools = fetchurl(
        &driver,
        "https://raw.githubusercontent.com/NixOS/nixpkgs/5abe06c801b0d513bf55d8f5924c4dc33f8bf7b9/pkgs/stdenv/linux/bootstrap-tools/scripts/unpack-bootstrap-tools.sh",
        "4VxGcP5b9SAX_vf7nnf0+0T7M4i+gRc2Tnttgl3DvIM",
        "",
        true,
    );

    /* === bootstrap stage 0 === */

    let (h_busybox, h_bootstrap_tools, h_unpack_bootstrap_tools) =
        tokio::join!(h_busybox, h_bootstrap_tools, h_unpack_bootstrap_tools);
    let (h_busybox, h_bootstrap_tools, h_unpack_bootstrap_tools) =
        (h_busybox?, h_bootstrap_tools?, h_unpack_bootstrap_tools?);

    let bb = format!("{}/{}/busybox", store_path, h_busybox);
    let bootstrap_tools = driver
        .run_task(WorkItem {
            envs: mk_envs(vec![
                ("builder", bb.to_string()),
                ("tarball", format!("{}/{}", store_path, h_bootstrap_tools)),
            ]),
            args: vec![
                bb.to_string(),
                "ash".to_string(),
                "-e".to_string(),
                format!("{}/{}", store_path, h_unpack_bootstrap_tools),
            ],
            outputs: mk_outputs(vec!["out"]),
            files: mk_envfiles(vec![]),
        })
        .await;

    info!("bootstrap_tools = {:?}", bootstrap_tools);
    let bootstrap_tools = bootstrap_tools["out"];

    let wrappers = gen_wrappers(&driver, &store_path, bootstrap_tools).await?;
    info!("wrappers = {:?}", wrappers);

    // imported from from scratchix
    let buildsh = smart_upload(
        &driver,
        ThinTree::RegularInline(Regular {
            executable: true,
            contents: include_str!("stage1/mkDerivation-builder.sh")
                .replace(
                    "@bootstrapTools@",
                    &format!("{}/{}", store_path, bootstrap_tools),
                )
                .replace("@wrappers@", &format!("{}/{}", store_path, wrappers))
                .into_bytes(),
        }),
    )
    .await?;

    let kernel_headers_script = indoc! {"
        make ARCH=x86 headers
        mkdir -p $out
        cp -r usr/include $out
        find $out -type f ! -name '*.h' -delete
    "};

    let driver2 = driver.clone();
    let store_path2 = store_path.clone();
    let kernel_headers = Runner::new_wi(&driver, async move {
        Ok(WorkItem {
            envs: mk_envs(vec![(
                "src",
                format!(
                    "{}/{}",
                    store_path2,
                    Runner::new_fetchu(
                        &driver2,
                        "https://cdn.kernel.org/pub/linux/kernel/v5.x/linux-5.16.tar.xz",
                        "qAUzBhalNzfuGFhgHGll_x7dEnVgOrfWZcJBjlJ4soo",
                        "",
                        false,
                    )
                    .want()
                    .await?["out"]
                ),
            )]),
            args: vec![
                format!("{}/{}", store_path2, buildsh),
                "/build/build.sh".to_string(),
            ],
            outputs: mk_outputs(vec!["out"]),
            files: mk_envfiles(vec![(
                "build.sh",
                ThinTree::RegularInline(Regular {
                    executable: true,
                    contents: kernel_headers_script.to_string().into_bytes(),
                }),
            )]),
        })
    });

    let binutils_script = indoc! {"
        set -xe
        cd ..
        mkdir build
        cd build
        \"../$sourceRoot/configure\" --prefix=\"$out\" --disable-nls --disable-werror --enable-deterministic-archives
        make
        make install
    "};

    let driver2 = driver.clone();
    let store_path2 = store_path.clone();
    let binutils = Runner::new_wi(&driver, async move {
        let src = fetchurl_wrapped(
            &driver2,
            //"http://ftp.gnu.org/gnu/binutils/binutils-2.38.tar.xz",
            //"dWdbr5ALD_NFkb2GxUznkedusXS9uMaTLdcYarsxt7M",
            "http://ftp.gnu.org/gnu/binutils/binutils-2.37.tar.xz",
            "QXRlu09CbhF2Y1HvHQzZzun7gbMd5rmV6_m3xzrbBiQ",
            "",
            false,
        );
        let asrp = fetchurl_wrapped(
            &driver2,
            "https://raw.githubusercontent.com/NixOS/nixpkgs/5abe06c801b0d513bf55d8f5924c4dc33f8bf7b9/pkgs/development/tools/misc/binutils/always-search-rpath.patch",
            "SmUZ9H1bWUuTUPGa9WpZc02EKHOiSBoM2ZdxfZ3PNvo",
            "",
            false,
        );
        Ok(WorkItem {
            envs: mk_envs(vec![("src", format!("{}/{}", store_path2, src.await?))]),
            args: vec![
                format!("{}/{}", store_path2, buildsh),
                "/build/build.sh".to_string(),
            ],
            outputs: mk_outputs(vec!["out"]),
            files: mk_envfiles(vec![
                (
                    "build.sh",
                    ThinTree::RegularInline(Regular {
                        executable: true,
                        contents: binutils_script.to_string().into_bytes(),
                    }),
                ),
                (
                    "patches",
                    ThinTree::RegularInline(Regular {
                        executable: false,
                        contents: vec![asrp.await?]
                            .into_iter()
                            .map(|i| format!("{}/{}\n", store_path2, i))
                            .collect::<Vec<_>>()
                            .join("")
                            .into_bytes(),
                    }),
                ),
            ]),
        })
    });

    let h_gnu_generic_script = smart_upload(
        &driver,
        ThinTree::RegularInline(Regular {
            executable: true,
            contents: indoc! {"
                set -xe
                cd ..
                mkdir build
                cd build
                \"../$sourceRoot/configure\" --prefix=\"$out\"
                make
                make install
            "}
            .to_string()
            .into_bytes(),
        }),
    )
    .await?;

    let driver2 = driver.clone();
    let store_path2 = store_path.clone();
    let gnum4 = Runner::new_wi(&driver, async move {
        let src = fetchurl_wrapped(
            &driver2,
            "http://ftp.gnu.org/gnu/m4/m4-1.4.19.tar.xz",
            "0a+VYnJfCDfm5uuoCDjjEiWvfO_CyBoVR6TiumW7xzU",
            "",
            false,
        );
        Ok(WorkItem {
            envs: mk_envs(vec![("src", format!("{}/{}", store_path2, src.await?))]),
            args: vec![
                format!("{}/{}", store_path2, buildsh),
                format!("{}/{}", store_path2, h_gnu_generic_script),
            ],
            outputs: mk_outputs(vec!["out"]),
            files: mk_envfiles(vec![]),
        })
    });

    let driver2 = driver.clone();
    let store_path2 = store_path.clone();
    let mut gnum4_ = gnum4.clone();
    let gmp = Runner::new_wi(&driver, async move {
        let src = fetchurl_wrapped(
            &driver2,
            "http://ftp.gnu.org/gnu/gmp/gmp-6.2.1.tar.xz",
            "2Y1NN59whhgXh9dnsLdm5q_LC1d+PtOf2vrpxgbFtsc",
            "",
            false,
        );
        Ok(WorkItem {
            envs: mk_envs(vec![
                ("src", format!("{}/{}", store_path2, src.await?)),
                (
                    "buildInputs",
                    [gnum4_.want().await?["out"]]
                        .into_iter()
                        .map(|i| format!("{}/{}", store_path2, i))
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
            ]),
            args: vec![
                format!("{}/{}", store_path2, buildsh),
                format!("{}/{}", store_path2, h_gnu_generic_script),
            ],
            outputs: mk_outputs(vec!["out"]),
            files: mk_envfiles(vec![]),
        })
    });

    let driver2 = driver.clone();
    let store_path2 = store_path.clone();
    let mut gmp_ = gmp.clone();
    let mpfr = Runner::new_wi(&driver, async move {
        let src = fetchurl_wrapped(
            &driver2,
            "http://ftp.gnu.org/gnu/mpfr/mpfr-4.1.0.tar.xz",
            "pv4Lc8hk+jbF_wfwzppfSfglI010A6aTF5p5jDSsnK0",
            "",
            false,
        );
        Ok(WorkItem {
            envs: mk_envs(vec![
                ("src", format!("{}/{}", store_path2, src.await?)),
                (
                    "buildInputs",
                    [gmp_.want().await?["out"]]
                        .into_iter()
                        .map(|i| format!("{}/{}", store_path2, i))
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
            ]),
            args: vec![
                format!("{}/{}", store_path2, buildsh),
                format!("{}/{}", store_path2, h_gnu_generic_script),
            ],
            outputs: mk_outputs(vec!["out"]),
            files: mk_envfiles(vec![]),
        })
    });

    let driver2 = driver.clone();
    let store_path2 = store_path.clone();
    let mut gmp_ = gmp.clone();
    let mut mpfr_ = mpfr.clone();
    let mpc = Runner::new_wi(&driver, async move {
        let src = fetchurl_wrapped(
            &driver2,
            "http://ftp.gnu.org/gnu/mpc/mpc-1.2.1.tar.gz",
            "qfCKCGwET3kpnFoh7wn8XMOR0cGBnXHQWdZO3Ddq0eI",
            "",
            false,
        );
        Ok(WorkItem {
            envs: mk_envs(vec![
                ("src", format!("{}/{}", store_path2, src.await?)),
                (
                    "buildInputs",
                    [gmp_.want().await?["out"], mpfr_.want().await?["out"]]
                        .into_iter()
                        .map(|i| format!("{}/{}", store_path2, i))
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
            ]),
            args: vec![
                format!("{}/{}", store_path2, buildsh),
                format!("{}/{}", store_path2, h_gnu_generic_script),
            ],
            outputs: mk_outputs(vec!["out"]),
            files: mk_envfiles(vec![]),
        })
    });

    let gcc_script = indoc! {"
        set -xe
        sed -e '/m64=/s/lib64/lib/' -i.orig gcc/config/i386/t-linux64
        cd ..
        mkdir build
        cd build
        \"../$sourceRoot/configure\" --prefix=\"$out\" \\
          --disable-libcc1 \\
          --disable-bootstrap \\
          --with-newlib \\
          --without-headers \\
          --enable-initfini-array \\
          --disable-nls \\
          --disable-shared \\
          --disable-multilib \\
          --disable-decimal-float \\
          --disable-threads \\
          --disable-libatomic \\
          --disable-libgomp \\
          --disable-libquadmath \\
          --disable-libssp \\
          --disable-libvtv \\
          --disable-libstdcxx \\
          --enable-languages=c,c++ \\

        make -j4
        make install

        cat ../$sourceRoot/gcc/limitx.h ../$sourceRoot/gcc/glimits.h ../$sourceRoot/gcc/limity.h > \\
          `dirname $($out/bin/x86_64-pc-linux-gnu-gcc -print-libgcc-file-name)`/install-tools/include/limits.h
    "};

    let driver2 = driver.clone();
    let store_path2 = store_path.clone();
    let mut binutils_ = binutils.clone();
    let mut gmp_ = gmp.clone();
    let mut mpfr_ = mpfr.clone();
    let mut mpc_ = mpc.clone();
    let gcc = Runner::new_wi(&driver, async move {
        let src = fetchurl_wrapped(
            &driver2,
            //"http://ftp.gnu.org/gnu/gcc/gcc-11.2.0/gcc-11.2.0.tar.gz",
            //"h3awRfXxv4uVCAML3UDhppwVtM2pvXqYJr9S+6PCaFA",
            "http://ftp.gnu.org/gnu/gcc/gcc-10.2.0/gcc-10.2.0.tar.gz",
            "0gaKxhSw6ofIEoRH2XLJ7d5KSAxf9fZInsB4POepXMc",
            "",
            false,
        );
        Ok(WorkItem {
            envs: mk_envs(vec![
                ("src", format!("{}/{}", store_path2, src.await?)),
                (
                    "buildInputs",
                    [
                        binutils_.want().await?["out"],
                        gmp_.want().await?["out"],
                        mpfr_.want().await?["out"],
                        mpc_.want().await?["out"],
                    ]
                    .into_iter()
                    .map(|i| format!("{}/{}", store_path2, i))
                    .collect::<Vec<_>>()
                    .join(" "),
                ),
            ]),
            args: vec![
                format!("{}/{}", store_path2, buildsh),
                "/build/build.sh".to_string(),
            ],
            outputs: mk_outputs(vec!["out"]),
            files: mk_envfiles(vec![(
                "build.sh",
                ThinTree::RegularInline(Regular {
                    executable: true,
                    contents: gcc_script.to_string().into_bytes(),
                }),
            )]),
        })
    });

    let binutils = match binutils.clone().want().await {
        Ok(outs) => outs["out"],
        _ => anyhow::bail!("unable to build binutils"),
    };

    let gcc = match gcc.clone().want().await {
        Ok(outs) => outs["out"],
        _ => anyhow::bail!("unable to build gcc"),
    };

    let perl_script = indoc! {"
        set -xe
        unset src
        sh Configure -des                                    \\
            -Dusedevel                                       \\
            -Uversiononly                                    \\
            -Dusethreads                                     \\
            -Dprefix=\"$out\"                                \\
            -Dvendorprefix=\"$out\"                          \\
            -Dprivlib=\"$out\"/lib/perl5/5.34/core_perl      \\
            -Darchlib=\"$out\"/lib/perl5/5.34/core_perl      \\
            -Dsitelib=\"$out\"/lib/perl5/5.34/site_perl      \\
            -Dsitearch=\"$out\"/lib/perl5/5.34/site_perl     \\
            -Dvendorlib=\"$out\"/lib/perl5/5.34/vendor_perl  \\
            -Dvendorarch=\"$out\"/lib/perl5/5.34/vendor_perl

        make -j4
        make install
    "};

    let driver2 = driver.clone();
    let store_path2 = store_path.clone();
    let perl = Runner::new_wi(&driver, async move {
        let src = fetchurl_wrapped(
            &driver2,
            "https://www.cpan.org/src/5.0/perl-5.34.1.tar.gz",
            "3Ls49BfrYuSiuRTubVBdBkCKAOVCGvUKuvX0XB10WQo",
            "",
            false,
        );
        Ok(WorkItem {
            envs: mk_envs(vec![
                ("src", format!("{}/{}", store_path2, src.await?)),
                (
                    "buildInputs",
                    [binutils]
                        .into_iter()
                        .map(|i| format!("{}/{}", store_path2, i))
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
            ]),
            args: vec![
                format!("{}/{}", store_path2, buildsh),
                "/build/build.sh".to_string(),
            ],
            outputs: mk_outputs(vec!["out"]),
            files: mk_envfiles(vec![(
                "build.sh",
                ThinTree::RegularInline(Regular {
                    executable: true,
                    contents: perl_script.to_string().into_bytes(),
                }),
            )]),
        })
    });

    let driver2 = driver.clone();
    let store_path2 = store_path.clone();
    let mut gnum4_ = gnum4.clone();
    let mut perl_ = perl.clone();
    let bison = Runner::new_wi(&driver, async move {
        let src = fetchurl_wrapped(
            &driver2,
            "http://ftp.gnu.org/gnu/bison/bison-3.8.2.tar.gz",
            "9yyz88o6AcVANnLDBbPo3IQGJU8Xc_Lv03NsDFy+jTA",
            "",
            false,
        );
        Ok(WorkItem {
            envs: mk_envs(vec![
                ("src", format!("{}/{}", store_path2, src.await?)),
                (
                    "buildInputs",
                    [
                        binutils,
                        gnum4_.want().await?["out"],
                        perl_.want().await?["out"],
                    ]
                    .into_iter()
                    .map(|i| format!("{}/{}", store_path2, i))
                    .collect::<Vec<_>>()
                    .join(" "),
                ),
            ]),
            args: vec![
                format!("{}/{}", store_path2, buildsh),
                "/build/build.sh".to_string(),
            ],
            outputs: mk_outputs(vec!["out"]),
            files: mk_envfiles(vec![(
                "build.sh",
                ThinTree::RegularInline(Regular {
                    executable: true,
                    contents: indoc! {"
                            set -xe
                            if ! ./configure --prefix=\"$out\"; then
                              ls -las config.log
                              echo \"BEGIN config.log\"
                              type cat
                              cat config.log || true
                              echo \"END config.log\"
                              exit 1
                            fi
                            make -j4
                            make install
                        "}
                    .to_string()
                    .into_bytes(),
                }),
            )]),
        })
    });

    let driver2 = driver.clone();
    let store_path2 = store_path.clone();
    let python3 = Runner::new_wi(&driver, async move {
        let src = fetchurl_wrapped(
            &driver2,
            "https://www.python.org/ftp/python/3.10.4/Python-3.10.4.tar.xz",
            "XKL3L81P5R+Qmuezwfju6N9ByNshvWRk0EHYY51ueAo",
            "",
            false,
        );
        let no_ldconfig = fetchurl_wrapped(
            &driver2,
            "https://raw.githubusercontent.com/NixOS/nixpkgs/9fc849704f9cb8baba7b3a30cceac00a448d9a53/pkgs/development/interpreters/python/cpython/3.10/no-ldconfig.patch",
            "0bAn_ysm2ElrQdQw9OxbFktiOk09ihtD7eZ0uWJkxiM",
            "",
            false,
        );
        Ok(WorkItem {
            envs: mk_envs(vec![
            (
                "src",
                format!("{}/{}", store_path2, src.await?),
            ),
            (
                "buildInputs",
                [
                    binutils,
                ].into_iter().map(|i| format!("{}/{}", store_path2, i)).collect::<Vec<_>>().join(" "),
            ),
            ("LIBS", "-lcrypt".to_string()),
            ("PYTHONHASHSEED", "0".to_string()),
            ("CFLAGS_NODIST", "-fno-semantic-interposition".to_string()),
            ]),
            args: vec![
                format!("{}/{}", store_path2, buildsh),
                "/build/build.sh".to_string(),
            ],
            outputs: mk_outputs(vec!["out"]),
            files: mk_envfiles(vec![
                (
                    "patches",
                    ThinTree::Directory(std::iter::once((
                        "no-ldconfig.patch".to_string().try_into().unwrap(),
                        ThinTree::SymLink {
                            target: format!("{}/{}", store_path2, no_ldconfig.await?).into(),
                        },
                    )).collect()),
                ),
                (
                    "build.sh",
                    ThinTree::RegularInline(Regular {
                        executable: true,
                        contents: indoc! {"
                            set -xe
                            NIX_WRAPPER_gcc_ARGS=\"$NIX_WRAPPER_gcc_ARGS -lgcc_s\"
                            NIX_WRAPPER_gxx_ARGS=\"$NIX_WRAPPER_gxx_ARGS -lgcc_s\"
                            export NIX_WRAPPER_gcc_ARGS NIX_WRAPPER_gxx_ARGS
                            ls -las
                            ./configure --prefix=\"$out\" \\
                                --enable-shared \\
                                --without-ensurepip \\
                                ac_cv_func_lchmod=no \\

                            make -j4
                            make install
                            # needed for some packages, especially packages that backport functionality
                            # to 2.x from 3.x
                            for item in $out/lib/${libPrefix}/test/*; do
                              if [[ \"$item\" != */test_support.py*
                                 && \"$item\" != */test/support
                                 && \"$item\" != */test/libregrtest
                                 && \"$item\" != */test/regrtest.py* ]]; then
                                rm -rf \"$item\"
                              else
                                echo $item
                              fi
                            done
                            touch $out/lib/${libPrefix}/test/__init__.py

                            ln -s ${libPrefix}m $out/include/${libPrefix}

                            # Determinism: Windows installers were not deterministic.
                            # We're also not interested in building Windows installers.
                            find \"$out\" -name 'wininst*.exe' | xargs -r rm -f

                            # Use Python3 as default python
                            ln -s idle3 \"$out/bin/idle\"
                            ln -s pydoc3 \"$out/bin/pydoc\"
                            ln -s python3 \"$out/bin/python\"
                            ln -s python3-config \"$out/bin/python-config\"
                            ln -s python3.pc \"$out/lib/pkgconfig/python.pc\"
                            # Get rid of retained dependencies on -dev packages, and remove
                            # some $TMPDIR references to improve binary reproducibility.
                            # Note that the .pyc file of _sysconfigdata.py should be regenerated!
                            for i in $out/lib/${libPrefix}/_sysconfigdata*.py $out/lib/${libPrefix}/config-3.10*/Makefile; do
                               sed -i $i -e \"s|/tmp|/no-such-path|g\"
                            done

                            strip -S $out/lib/${libPrefix}/config-*/libpython*.a || true
                            rm -fR $out/bin/python*-config $out/lib/python*/config-*
                            rm -fR $out/bin/idle* $out/lib/python*/{idlelib,turtledemo}
                            rm -fR $out/lib/python*/tkinter
                            rm -fR $out/lib/python*/test $out/lib/python*/**/test{,s}
                            find $out -type d -name __pycache__ -print0 | xargs -0 -I {} rm -rf \"{}\"
                            mkdir -p $out/share/gdb
                            sed '/^#!/d' Tools/gdb/libpython.py > $out/share/gdb/libpython.py
                        "}.to_string().replace("${libPrefix}", "python3.10").into_bytes(),
                    }),
                ),
            ]),
        })
    });

    let buildscript_glibc = smart_upload(
        &driver,
        ThinTree::RegularInline(Regular {
            executable: true,
            contents: include_str!("stage1/buildscript-glibc.sh")
                .replace(
                    "@bootstrapTools@",
                    &format!("{}/{}", store_path, bootstrap_tools),
                )
                .into_bytes(),
        }),
    )
    .await?;

    let driver2 = driver.clone();
    let store_path2 = store_path.clone();
    let mut bison_ = bison.clone();
    let mut python3_ = python3.clone();
    let mut kernel_headers_ = kernel_headers.clone();
    let glibc = Runner::new_wi(&driver, async move {
        let src = fetchurl_wrapped(
            &driver2,
            "http://ftp.gnu.org/gnu/glibc/glibc-2.34.tar.xz",
            "EQvlIPqQhG3JJjoH8VsbXcwGqFHbxJ+m1P1eSWEX9e0",
            "",
            false,
        );
        Ok(WorkItem {
            envs: mk_envs(vec![
                ("src", format!("{}/{}", store_path2, src.await?)),
                ("gcc", format!("{}/{}", store_path2, gcc)),
                (
                    "buildInputs",
                    [
                        binutils,
                        bison_.want().await?["out"],
                        python3_.want().await?["out"],
                    ]
                    .into_iter()
                    .map(|i| format!("{}/{}", store_path2, i))
                    .collect::<Vec<_>>()
                    .join(" "),
                ),
                (
                    "lnxheaders",
                    format!("{}/{}", store_path2, kernel_headers_.want().await?["out"]),
                ),
            ]),
            args: vec![format!("{}/{}", store_path2, buildscript_glibc)],
            outputs: mk_outputs(vec!["out"]),
            files: mk_envfiles(vec![]),
        })
    });

    let _kernel_headers = match kernel_headers.clone().want().await {
        Ok(outs) => outs["out"],
        _ => anyhow::bail!("unable to build kernel headers"),
    };

    let _perl = match perl.clone().want().await {
        Ok(outs) => outs["out"],
        _ => anyhow::bail!("unable to build perl"),
    };

    let _bison = match bison.clone().want().await {
        Ok(outs) => outs["out"],
        _ => anyhow::bail!("unable to build bison"),
    };

    let _python3 = match python3.clone().want().await {
        Ok(outs) => outs["out"],
        _ => anyhow::bail!("unable to build python3"),
    };

    let _glibc = match glibc.clone().want().await {
        Ok(outs) => outs["out"],
        _ => anyhow::bail!("unable to build glibc"),
    };

    Ok(())
}
