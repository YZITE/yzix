use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{Receiver, Sender};
use yzix_proto::ProtoLen;

use std::collections::HashSet;
use std::sync::Arc;

pub async fn handle_clients_initial(
    reqs: Sender<yzix_proto::Request>,
    mut resp: Receiver<yzix_proto::Response>,
    mut stream: TcpStream,
    valid_bearer_tokens: Arc<HashSet<String>>,
) {
    // auth
    let mut lenbuf = [0u8; std::mem::size_of::<ProtoLen>()];
    {
        if stream.read_exact(&mut lenbuf).await.is_err() {
            return;
        }
        let len = ProtoLen::from_le_bytes(lenbuf);
        if len >= 0x400 {
            return;
        }
        let mut buf: Vec<u8> = Vec::new();
        buf.resize(len.try_into().unwrap(), 0);
        if stream.read_exact(&mut buf[..]).await.is_err() {
            return;
        }
        let bearer_token: String = match ciborium::de::from_reader(&buf[..]) {
            Ok(x) => x,
            Err(_) => return,
        };
        if !valid_bearer_tokens.contains(&bearer_token) {
            return;
        }
    }

    // normal comm
    let (stream, mut stream2) = stream.split();

    let handle_input = async move {
        let mut buf: Vec<u8> = Vec::new();
        let mut stream = tokio::io::BufReader::new(stream);
        while stream.read_exact(&mut lenbuf).await.is_ok() {
            buf.clear();
            let len = ProtoLen::from_le_bytes(lenbuf);
            // TODO: make sure that the length isn't too big
            buf.resize(len.try_into().unwrap(), 0);
            if stream.read_exact(&mut buf[..]).await.is_err() {
                break;
            }
            let val: ciborium::value::Value = match ciborium::de::from_reader(&buf[..]) {
                Ok(x) => x,
                Err(e) => {
                    // TODO: report error to client, maybe?
                    // this can happen either when the serialization format
                    // between client and server mismatches,
                    // or when we run into a ciborium bug.
                    tracing::error!("CBOR: {}", e);
                    break;
                }
            };
            use yzix_proto::Request as Req;
            let req: Req = match val.deserialized() {
                Ok(x) => x,
                Err(e) => {
                    tracing::error!("CBOR: {}", e);
                    tracing::debug!("CBOR: {:#?}", val);
                    break;
                }
            };
            if reqs.send(req).await.is_err() {
                break;
            }
        }
    };

    let handle_output = async move {
        let mut buf = Vec::<u8>::new();
        while let Some(x) = resp.recv().await {
            buf.clear();
            if let Err(e) = ciborium::ser::into_writer(&x, &mut buf) {
                // TODO: handle error
                tracing::error!("CBOR: {}", e);
            } else {
                if stream2
                    .write_all(&ProtoLen::to_le_bytes(buf.len().try_into().unwrap()))
                    .await
                    .is_err()
                {
                    break;
                }
                if stream2.write_all(&buf[..]).await.is_err() {
                    break;
                }
                if stream2.flush().await.is_err() {
                    break;
                }
            }
        }
    };

    tokio::join!(handle_input, handle_output);
}
