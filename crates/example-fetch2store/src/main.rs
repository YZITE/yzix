use yzix_client::{store, Driver};

#[derive(Debug, clap::Parser)]
struct Args {
    #[clap(long)]
    executable: bool,

    #[clap(long)]
    with_path: Option<String>,

    url_to_fetch: String,
}

async fn my_fetch(url: &str) -> Result<Vec<u8>, reqwest::Error> {
    Ok(reqwest::get(url).await?.bytes().await?.as_ref().to_vec())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let server_addr = std::env::var("YZIX_SERVER_ADDR").expect("YZIX_SERVER_ADDR env var not set");
    let bearer_token =
        std::env::var("YZIX_BEARER_TOKEN").expect("YZIX_BEARER_TOKEN env var not set");

    let args = <Args as clap::Parser>::parse();

    // install global log subscriber configured based on RUST_LOG envvar.
    tracing_subscriber::fmt::init();

    let mut stream = tokio::net::TcpStream::connect(server_addr)
        .await
        .expect("unable to connect to yzix server");
    yzix_client::do_auth(&mut stream, &bearer_token)
        .await
        .expect("unable to send authentication info to yzix server");
    let driver = Driver::new(stream).await;

    let mut dump = store::Dump::Regular {
        executable: args.executable,
        contents: my_fetch(&args.url_to_fetch)
            .await
            .expect("unable to fetch url"),
    };

    if let Some(path) = &args.with_path {
        for i in path.split('/') {
            dump = store::Dump::Directory(std::iter::once((i.to_string(), dump)).collect());
        }
    }

    println!("hash = {}", store::Hash::hash_complex(&dump));
    println!("res  = {:?}", driver.upload(dump).await);
}
