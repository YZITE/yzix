use yzix_client::{store::Hash as StoreHash, strwrappers::OutputName, Driver, WorkItem};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    /* === seed === */

    // $ example-fetch2store --executable --with-path busybox http://tarballs.nixos.org/stdenv-linux/i686/4907fc9e8d0d82b28b3c56e3a478a2882f1d700f/busybox
    let h_busybox = "liAXAxlPQSRlEjqQFgoewxVmQTv73rfukUCyyPZfsKI"
        .parse::<StoreHash>()
        .unwrap();

    // $ example-fetch2store http://tarballs.nixos.org/stdenv-linux/i686/c5aabb0d603e2c1ea05f5a93b3be82437f5ebf31/bootstrap-tools.tar.xz
    let h_bootstrap_tools = "V2QVvHUYOYoESuMSI89zKvlZWnYVhd4JtECfNQv+ll4"
        .parse::<StoreHash>()
        .unwrap();

    // $ example-fetch2store --executable https://raw.githubusercontent.com/NixOS/nixpkgs/5abe06c801b0d513bf55d8f5924c4dc33f8bf7b9/pkgs/stdenv/linux/bootstrap-tools/scripts/unpack-bootstrap-tools.sh
    let h_unpack_bootstrap_tools = "ow8ctEPXY74kphwpR0SAb2fIbZ7FmFr8EnxmPH80_sY"
        .parse::<StoreHash>()
        .unwrap();

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

    /* === bootstrap stage 0 === */

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
            outputs: ["out"]
                .into_iter()
                .map(|i| OutputName::new(i.to_string()).unwrap())
                .collect(),
        })
        .await;

    println!("bootstrap_tools = {:?}", bootstrap_tools);
}
