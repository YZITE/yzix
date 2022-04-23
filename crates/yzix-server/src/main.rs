use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info};
use yzix_store_builder::{Utf8Path, Utf8PathBuf};

#[derive(Debug, serde::Deserialize)]
pub struct ServerConfig {
    store_path: Utf8PathBuf,
    container_runner: String,
    socket_bind: String,
    bearer_tokens: HashSet<String>,
}

mod clients;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    // reset all environment variables before invoking any .await
    // this is necessary to avoid unnecessary query syscalls
    {
        use std::env;
        for (key, _) in env::vars_os() {
            if key == "RUST_LOG" {
                continue;
            }
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
            eprintln!(
                "yzix-server: ERROR: invalid invocation (supply a config file as only argument)"
            );
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
        match toml::de::from_slice(
            &std::fs::read(arg).expect("unable to read supplied config file"),
        ) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("yzix-server: CONFIG ERROR: {}", e);
                std::process::exit(1);
            }
        }
    };
    let config = Arc::new(config);

    // validate config

    if config.store_path == Utf8Path::new("") {
        eprintln!("yzix-server: CONFIG ERROR: store_path is invalid");
        std::process::exit(1);
    }

    if config.container_runner.is_empty() {
        eprintln!("yzix-server: CONFIG ERROR: container_runner is invalid");
        std::process::exit(1);
    }

    if config.socket_bind.is_empty() {
        eprintln!("yzix-server: CONFIG ERROR: socket_bind is invalid");
        std::process::exit(1);
    }

    if config.bearer_tokens.is_empty() {
        eprintln!("yzix-server: CONFIG ERROR: bearer_tokens is empty");
        std::process::exit(1);
    }

    // continue setup

    // client acceptor

    let listener = tokio::net::TcpListener::bind(&config.socket_bind)
        .await
        .expect("unable to bind socket");

    let (client_reqs, client_reqr) = mpsc::channel(1000);
    let config2 = config.clone();
    let jh_cla = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    info!("new connection from {:?}", addr);
                    tokio::spawn(clients::handle_client(
                        config2.clone(),
                        client_reqs.clone(),
                        stream,
                    ));
                }
                Err(e) => {
                    error!("listener'accept() failed: {:?}", e);
                }
            }
        }
    });

    let jh_main = tokio::spawn(yzix_store_builder::main(yzix_store_builder::Args {
        container_runner: config.container_runner.clone(),
        parallel_job_cnt: num_cpus::get(),
        store_path: config.store_path.clone(),
        ctrl_r: client_reqr,
    }));
    tokio::signal::ctrl_c().await.unwrap();
    jh_main.abort();
    jh_cla.abort();
    let _ = jh_cla.await;
    let _ = jh_main.await;
}
