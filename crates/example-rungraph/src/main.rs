use anyhow::{bail, Context as _};
use std::collections::BTreeMap;
use yzix_client::{Driver, OutputName, StoreHash, TaskBoundResponse as Tbr, WorkItem};

mod pkgs;
use pkgs::{ItemData, parse_pkgs_file};

mod pat;
use pat::Pattern;

#[derive(Debug)]
struct Item {
    name: String,
    kind: ItemData,
}

#[tokio::main(flavor = "current_thread")]
fn main() -> anyhow::Result<()> {
    // install global log subscriber configured based on RUST_LOG envvar.
    tracing_subscriber::fmt::init();

    // USAGE: rungraph PKGS_FILE TARGET

    let mut argsit = std::env::args();
    let pkgs_file = camino::Utf8PathBuf::from(argsit.next().context("no pkgs file specified")?);
    let target = argsit.next().context("no target specified")?;
    let pkgs = parse_pkgs_file(tokio::fs::read(&pkgs_file).await.with_context(|| format!("{}: unable to read file", pkgs_file))?).with_context(|| format!("{}: unable to parse file", pkgs_file))?;

    // build dependency graph
    let mut g = petgraph::graph::Graph::<>::new();

    let mut idxs = BTreeMap::<camino::Utf8PathBuf, BTreeMap<>>::new();
    let mut unresolved = vec![(init_file.canonicalize_utf8().context("init file canonicalization failed")?, target)];

    while let Some(x) = unresolved.pop() {
        let x_name_parts = 

        let y = fdat.get(x_.name).ok_or_else(|| bail!("{} . {} : package/item not found", x_.origin, x_.name))?;

        // parse entry

        match y 
    }

    println!("{:?}");

    todo!();

    /* === dependency graph generated, start building === */

    let server_addr = std::env::var("YZIX_SERVER_ADDR").context("YZIX_SERVER_ADDR env var not set")`;
    let bearer_token =
        std::env::var("YZIX_BEARER_TOKEN").context("YZIX_BEARER_TOKEN env var not set")?;

    let mut stream = tokio::net::TcpStream::connect(server_addr)
        .await
        .context("unable to connect to yzix server")?;
    yzix_client::do_auth(&mut stream, &bearer_token)
        .await
        .context("unable to send authentication info to yzix server")?;
    let driver = Driver::new(stream).await;

    let store_path = Arc::new(driver.store_path().await);

    Ok(())
}
