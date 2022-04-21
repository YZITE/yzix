use std::collections::BTreeSet;
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

fn mk_outputs(elems: Vec<&str>) -> BTreeSet<OutputName> {
    elems
        .into_iter()
        .map(|i| OutputName::new(i.to_string()).unwrap())
        .collect()
}

async fn gen_wrappers(driver: &Driver, store_path: &str, bootstrap_tools: StoreHash) -> anyhow::Result<StoreHash> {
    fn gen_wrapper(store_path: &str, bootstrap_tools: StoreHash, element: &str) -> Dump {
        Dump::Regular {
            executable: true,
            contents: format!(
                "#!{stp}/{bst}/bin/bash\nexec {stp}/{bst}/bin/{elem} $YZIX_WRAPPER_{elem}_ARGS \"$@\"\n",
                stp = store_path,
                bst = bootstrap_tools,
                elem = element,
            )
            .into_bytes(),
        }
    }

    let mut dump = Dump::Directory(
        ["gcc", "g++"]
            .into_iter()
            .map(|i| (i.to_string(), gen_wrapper(store_path, bootstrap_tools, i)))
            .collect(),
    );
    dump = Dump::Directory(std::iter::once(("bin".to_string(), dump)).collect());

    let h = StoreHash::hash_complex(&dump);

    if !driver.has_out_hash(h).await {
        let x = driver.upload(dump).await;
        if !x.is_ok() {
            anyhow::bail!("genWrappers failed: {:?}", x);
        }
    }

    Ok(h)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    /* === environment setup === */

    let store_path = "/yzixs";
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
            envs: [
                ("builder", bb.clone()),
                ("tarball", format!("{}/{}", store_path, h_bootstrap_tools)),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
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

    let wrappers = gen_wrappers(&driver, store_path, bootstrap_tools).await?;
    info!("wrappers = {:?}", bootstrap_tools);

    Ok(())
}
