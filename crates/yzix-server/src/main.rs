use std::collections::HashSet;
use std::sync::Arc;
use tracing::Instrument;
use yzix_store_builder::{Utf8Path, Utf8PathBuf};

#[derive(Debug, serde::Deserialize)]
pub struct ServerConfig {
    loglevel: String,
    store_path: Utf8PathBuf,
    container_runner: String,
    socket_bind: std::net::SocketAddr,
    bearer_tokens: HashSet<String>,
}

mod route;

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
        match toml::from_str(
            &String::from_utf8(std::fs::read(arg).expect("unable to read supplied config file"))
                .expect("config contains invalid UTF-8"),
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

    if config.bearer_tokens.is_empty() {
        eprintln!("yzix-server: CONFIG ERROR: bearer_tokens is empty");
        std::process::exit(1);
    }

    // install global log subscriber configured based on RUST_LOG envvar.

    {
        use tracing_subscriber::{prelude::*, *};
        registry()
            .with(fmt::layer())
            .with(EnvFilter::new(&config.loglevel))
            .init();
    }

    let sbenv = Arc::new(
        yzix_store_builder::Env::new(
            config.store_path.clone(),
            config.container_runner.clone(),
            num_cpus::get(),
        )
        .await,
    );

    let resolver = hyper_staticfile::Resolver::new(&config.store_path);
    let sbenv2 = sbenv.clone();
    let config2 = config.clone();

    // the following is based upon https://github.com/hyperium/hyper-util/blob/df55abac42d0cc1e1577f771d8a1fc91f4bcd0dd/examples/server_graceful.rs

    let listener = match tokio::net::TcpListener::bind(&config.socket_bind).await {
        Err(e) => {
            tracing::error!("unable to bind to {} : {}", &config.socket_bind, e);
            std::process::exit(1)
        }
        Ok(x) => x,
    };
    let server = hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new());
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    let mut ctrl_c = std::pin::pin!(tokio::signal::ctrl_c());

    loop {
        tokio::select! {
            conn = listener.accept() => {
                let (stream, peer_addr) = match conn {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::warn!("accept failed with {}", e);
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let config2 = config2.clone();
                let sbenv2 = sbenv2.clone();
                let peer_addr2 = peer_addr.to_string();
                let resolver2 = resolver.clone();

                let stream = hyper_util::rt::TokioIo::new(Box::pin(stream));
                let conn = server.serve_connection(stream, hyper::service::service_fn(move |req| {
                    route::handle_client(config2.clone(), sbenv2.clone(), resolver2.clone(), req).instrument(tracing::info_span!("connection", peer_addr2))
                }));
                let conn = graceful.watch(conn);

                tokio::spawn(async move {
                    if let Err(e) = conn.await {
                        tracing::error!("connection error: {}", e);
                    }
                    tracing::warn!("connection dropped: {}", peer_addr);
                });
            },

            _ = ctrl_c.as_mut() => {
                drop(listener);
                break;
            }
        }
    }

    tokio::select! {
        _ = graceful.shutdown() => {},
        _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
            tracing::error!("waited 10 seconds for graceful shutdown, aborting...");
        }
    }
}
