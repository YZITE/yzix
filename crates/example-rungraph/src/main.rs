fn main() {
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

    let store_path = Arc::new(driver.store_path().await);

    /* === rest === */

    todo!();
}
