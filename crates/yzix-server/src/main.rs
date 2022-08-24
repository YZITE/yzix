use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;
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

    use hyper::service::{make_service_fn, service_fn};

    let (client_reqs, client_reqr) = mpsc::channel(1000);
    let sbenv = Arc::new(
        yzix_store_builder::Env::new(
            config.store_path.clone(),
            config.container_runner.clone(),
            num_cpus::get(),
        )
        .await,
    );
    let sbenv2 = sbenv.clone();
    let config2 = config.clone();

    let make_service = make_service_fn(move |_conn| {
        let config2 = config2.clone();
        let client_reqs = client_reqs.clone();
        let sbenv2 = sbenv2.clone();
        async move {
            Ok::<_, core::convert::Infallible>(service_fn(move |req| {
                route::handle_client(config2.clone(), client_reqs.clone(), sbenv2.clone(), req)
            }))
        }
    });

    let jh_httpserver = hyper::Server::bind(&config.socket_bind)
        .serve(make_service)
        .with_graceful_shutdown(async { tokio::signal::ctrl_c().await.unwrap() });

    let jh_main = tokio::spawn(yzix_store_builder::main(sbenv, client_reqr));

    if let Err(e) = jh_httpserver.await {
        eprintln!("yzix-server/HTTP: {}", e);
    }
    jh_main.abort();
    let _ = jh_main.await;
}
