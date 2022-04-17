use yzix_client::{store, Driver};

async fn my_fetch(url: &str) -> Result<Vec<u8>, reqwest::Error> {
    Ok(reqwest::get(url).await?.bytes().await?.as_ref().to_vec())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let server_addr = std::env::var("YZIX_SERVER_ADDR").expect("YZIX_SERVER_ADDR env var not set");
    let bearer_token =
        std::env::var("YZIX_BEARER_TOKEN").expect("YZIX_BEARER_TOKEN env var not set");
    let url_to_fetch = std::env::args()
        .nth(1)
        .expect("no url given as first argument");
    let executable = std::env::args().nth(2).map(|i| i == "--executable") == Some(true);

    // install global log subscriber configured based on RUST_LOG envvar.
    tracing_subscriber::fmt::init();

    let mut stream = tokio::net::TcpStream::connect(server_addr)
        .await
        .expect("unable to connect to yzix server");
    yzix_client::do_auth(&mut stream, &bearer_token)
        .await
        .expect("unable to send authentication info to yzix server");
    let driver = Driver::new(stream).await;

    let dump = store::Dump::Regular {
        executable,
        contents: my_fetch(&url_to_fetch).await.expect("unable to fetch url"),
    };

    println!("hash = {}", store::Hash::hash_complex(&dump));
    println!("res  = {:?}", driver.upload(dump).await);
}
