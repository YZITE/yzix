use yzix_proto::{store::Hash as StoreHash, TaskId};
use camino::Utf8PathBuf;
use tokio::sync::oneshot;
use std::collections::{HashMap, HashSet};

pub mod clients;
mod fwi;
pub use fwi::FullWorkItem;
pub mod in2_helpers;
mod utils;
pub use utils::*;

#[derive(Debug, serde::Deserialize)]
pub struct ServerConfig {
    store_path: Utf8PathBuf,
    container_runner: String,
    socket_bind: String,
    bearer_tokens: HashSet<String>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // reset all environment variables before invoking any .await
    // this is necessary to avoid unnecessary query syscalls
    {
        use std::env;
        for (key, _) in env::vars_os() {
            env::remove_var(key);
        }
        env::set_var("LC_ALL", "C.UTF-8");
        env::set_var("TZ", "UTC");
    }

    // install global log subscriber configured based on RUST_LOG envvar.
    tracing_subscriber::fmt::init();

    let config: ServerConfig = {
        let mut args = std::env::args().skip(1);
        let inv_invoc = || -> ! {
            eprintln!("yzix-server: ERROR: invalid invocation (supply a config file as only argument)");
            std::process::exit(1);
        };
        let arg = if let Some(x) = args.next() {
            x
        } else {
            inv_invoc();
        };
        if args.next().is_some() || arg == "--help" {
            inv_invoc();
        }
        toml::de::from_slice(&std::fs::read(arg).expect("unable to read supplied config file"))
            .expect("unable to parse supplied config file")
    };

    std::fs::create_dir_all(&config.store_path).expect("unable to create store dir");

    let cpucnt = num_cpus::get();

    // inhash-locking, to prevent racing a workitem with itself
    let mut inhash_exclusive = HashMap::<StoreHash, TaskId>::new();

    // install Ctrl+C handler
    // this should be invoked as soon as we have something to clean up...
    let (shutdown_s, shutdown_r) = oneshot::channel();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.unwrap();
        let _ = shutdown_s.send(());
    });

    // main loop
    loop {
        tokio::select! {
            biased;

            _ = shutdown_r => break,
        }
    }
}
