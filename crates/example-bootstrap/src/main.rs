use indoc::indoc;
use std::collections::{BTreeMap, BTreeSet};
use tracing::{error, info};
use yzix_client::{
    store::Dump, store::Hash as StoreHash, strwrappers::OutputName, Driver,
    TaskBoundResponse as Tbr, WorkItem,
};

async fn fetchurl(
    driver: &Driver,
    url: &str,
    expect_hash: &str,
    with_path: &str,
    executable: bool,
) -> anyhow::Result<StoreHash> {
    async fn my_fetch(url: &str) -> Result<Vec<u8>, reqwest::Error> {
        Ok(reqwest::get(url).await?.bytes().await?.as_ref().to_vec())
    }

    let h = expect_hash.parse::<StoreHash>().unwrap();

    if driver.has_out_hash(h).await {
        return Ok(h);
    }

    info!("fetching {} ...", url);
    let contents = my_fetch(url).await?;
    info!("fetching {} ... done", url);

    let mut dump = Dump::Regular {
        executable,
        contents,
    };

    for i in with_path.split('/') {
        dump = Dump::Directory(std::iter::once((i.to_string(), dump)).collect());
    }

    let h2 = StoreHash::hash_complex(&dump);
    if h2 != h {
        error!(
            "fetchurl ({}): hash mismatch, expected = {}, got = {}",
            url, expect_hash, h2
        );
        anyhow::bail!("hash mismatch for url ({})", url);
    }

    let x = driver.upload(dump).await;
    info!("fetchurl ({}): {:?}", url, x);
    if !x.is_ok() {
        anyhow::bail!("fetchurl ({}) failed: {:?}", url, x);
    }
    Ok(h)
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

async fn smart_upload(driver: &Driver, dump: Dump, name: &str) -> anyhow::Result<StoreHash> {
    let h = StoreHash::hash_complex(&dump);

    if !driver.has_out_hash(h).await {
        let x = driver.upload(dump).await;
        if !x.is_ok() {
            anyhow::bail!("smart_upload failed @ {}: {:?}", name, x);
        }
    }

    Ok(h)
}

async fn gen_wrappers(
    driver: &Driver,
    store_path: &str,
    bootstrap_tools: StoreHash,
) -> anyhow::Result<StoreHash> {
    fn gen_wrapper(store_path: &str, bootstrap_tools: StoreHash, element: &str) -> Dump {
        Dump::Regular {
            executable: true,
            contents: format!(
                "#!{stp}/{bst}/bin/bash\nexec {stp}/{bst}/bin/{elem} $NIX_WRAPPER_{elem}_ARGS \"$@\"\n",
                stp = store_path,
                bst = bootstrap_tools,
                elem = element,
            )
            .into_bytes(),
        }
    }

    let mut dir: BTreeMap<_, _> = ["gcc", "g++"]
        .into_iter()
        .map(|i| (i.to_string(), gen_wrapper(store_path, bootstrap_tools, i)))
        .collect();

    dir.insert("cc".to_string(), dir["gcc"].clone());
    dir.insert("cpp".to_string(), dir["g++"].clone());
    dir.insert("cxx".to_string(), dir["g++"].clone());

    let mut dump = Dump::Directory(dir);
    dump = Dump::Directory(std::iter::once(("bin".to_string(), dump)).collect());
    smart_upload(driver, dump, "genWrappers").await
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    /* === environment setup === */

    let server_addr = std::env::var("YZIX_SERVER_ADDR").expect("YZIX_SERVER_ADDR env var not set");
    let bearer_token =
        std::env::var("YZIX_BEARER_TOKEN").expect("YZIX_BEARER_TOKEN env var not set");

    // install global log subscriber configured based on RUST_LOG envvar.
    tracing_subscriber::fmt::init();

    let mut stream = tokio::net::TcpStream::connect(server_addr)
        .await
        .expect("unable to connect to yzix server");
    yzix_client::do_auth(&mut stream, &bearer_token)
        .await
        .expect("unable to send authentication info to yzix server");
    let driver = Driver::new(stream).await;

    /* === seed === */

    let store_path = driver.store_path().await;

    // $ example-fetch2store --executable --with-path busybox http://tarballs.nixos.org/stdenv-linux/i686/4907fc9e8d0d82b28b3c56e3a478a2882f1d700f/busybox
    let h_busybox = fetchurl(
        &driver,
        "http://tarballs.nixos.org/stdenv-linux/i686/4907fc9e8d0d82b28b3c56e3a478a2882f1d700f/busybox",
        "liAXAxlPQSRlEjqQFgoewxVmQTv73rfukUCyyPZfsKI",
        "busybox",
        true,
    );

    // $ example-fetch2store http://tarballs.nixos.org/stdenv-linux/i686/c5aabb0d603e2c1ea05f5a93b3be82437f5ebf31/bootstrap-tools.tar.xz
    let h_bootstrap_tools = fetchurl(
        &driver,
        "http://tarballs.nixos.org/stdenv-linux/i686/c5aabb0d603e2c1ea05f5a93b3be82437f5ebf31/bootstrap-tools.tar.xz",
        "V2QVvHUYOYoESuMSI89zKvlZWnYVhd4JtECfNQv+ll4",
        "",
        false,
    );

    // $ example-fetch2store --executable
    let h_unpack_bootstrap_tools = fetchurl(
        &driver,
        "https://raw.githubusercontent.com/NixOS/nixpkgs/5abe06c801b0d513bf55d8f5924c4dc33f8bf7b9/pkgs/stdenv/linux/bootstrap-tools/scripts/unpack-bootstrap-tools.sh",
        "ow8ctEPXY74kphwpR0SAb2fIbZ7FmFr8EnxmPH80_sY",
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
                ("builder", bb.clone()),
                ("tarball", format!("{}/{}", store_path, h_bootstrap_tools)),
            ]),
            args: vec![
                bb.clone(),
                "ash".to_string(),
                "-e".to_string(),
                format!("{}/{}", store_path, h_unpack_bootstrap_tools),
            ],
            outputs: mk_outputs(vec!["out"]),
        })
        .await;

    info!("bootstrap_tools = {:?}", bootstrap_tools);

    let bootstrap_tools = match bootstrap_tools {
        Tbr::BuildSuccess(outs) => outs["out"],
        _ => anyhow::bail!("unable to build bootstrap tools"),
    };

    let wrappers = gen_wrappers(&driver, &store_path, bootstrap_tools).await?;
    info!("wrappers = {:?}", wrappers);

    // imported from from scratchix
    let buildsh = smart_upload(
        &driver,
        Dump::Regular {
            executable: true,
            contents: include_str!("mkDerivation-builder.sh")
                .replace(
                    "@bootstrapTools@",
                    &format!("{}/{}", store_path, bootstrap_tools),
                )
                .replace("@wrappers@", &format!("{}/{}", store_path, wrappers))
                .into_bytes(),
        },
        "buildsh",
    )
    .await?;

    let kernel_headers_src = fetchurl(
        &driver,
        "https://cdn.kernel.org/pub/linux/kernel/v5.x/linux-5.16.tar.xz",
        "Z6afPd9StYWqNcEaB6Ax1kXor6pZilBRHLRvKD+GWjM",
        "",
        false,
    )
    .await?;

    let kernel_headers_script = indoc! {"
        make ARCH=x86 headers
        mkdir -p $out
        cp -r usr/include $out
        find $out -type f ! -name '*.h' -delete
    "};
    let kernel_headers_buildsh = smart_upload(
        &driver,
        Dump::Regular {
            executable: true,
            contents: format!(
                "#!{stp}/{bst}/bin/bash\n{mscr}",
                stp = store_path,
                bst = bootstrap_tools,
                mscr = kernel_headers_script,
            )
            .into_bytes(),
        },
        "kernel_headers_buildsh",
    )
    .await?;

    let kernel_headers = driver
        .run_task(WorkItem {
            envs: mk_envs(vec![(
                "src",
                format!("{}/{}", store_path, kernel_headers_src),
            )]),
            args: vec![
                format!("{}/{}", store_path, buildsh),
                format!("{}/{}", store_path, kernel_headers_buildsh),
            ],
            outputs: mk_outputs(vec!["out"]),
        })
        .await;

    info!("kernel_headers = {:?}", kernel_headers);

    let kernel_headers = match kernel_headers {
        Tbr::BuildSuccess(outs) => outs["out"],
        _ => anyhow::bail!("unable to build kernel headers"),
    };

    Ok(())
}
