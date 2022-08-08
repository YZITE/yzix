#![forbid(
    clippy::as_conversions,
    clippy::cast_ptr_alignment,
    clippy::let_underscore_drop,
    trivial_casts,
    unconditional_recursion,
    unsafe_code
)]

use reqwest::{header, StatusCode};
pub use yzix_core::*;

#[derive(Clone)]
pub struct Driver {
    rcl: reqwest::Client,
    base: String,
    bearer: String,
}

impl Driver {
    pub async fn new(base: String, bearer: String) -> Self {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/json,text/x-yzix-task-log"),
        );
        Self {
            rcl: reqwest::ClientBuilder::new()
                .user_agent("yzix-client/0.2")
                .http2_prior_knowledge()
                .default_headers(headers)
                .build()
                .expect("unable to initialize HTTP client"),
            base,
            bearer,
        }
    }

    pub async fn store_path(&self) -> String {
        self.rcl
            .get(format!("{}/info", self.base))
            .send()
            .await
            .expect("failed to send request")
            .json::<std::collections::BTreeMap<String, String>>()
            .await
            .expect("failed to parse response")["StoreDir"]
            .to_string()
    }

    pub async fn run_task(
        &self,
        data: WorkItem,
    ) -> std::collections::BTreeMap<OutputName, TaggedHash<ThinTree>> {
        let tid = TaggedHash::hash_complex(&data);
        let mut resp = self
            .rcl
            .post(format!("{}/task", self.base))
            .bearer_auth(&self.bearer)
            .json(&data)
            .send()
            .await
            .expect("failed to send request");
        if !matches!(resp.status(), StatusCode::OK | StatusCode::ACCEPTED) {
            tracing::error!("{} :: {}", resp.status(), resp.text().await.unwrap());
            panic!("task submission failed");
        }
        let mut data = Vec::new();
        let mut non_dotted = String::new();
        use bytes::Buf;
        while let Some(mut chunk) = resp.chunk().await.expect("unable to read next chunk") {
            loop {
                let part = chunk.chunk();
                if part.is_empty() {
                    break;
                }
                data.extend_from_slice(part);
                chunk.advance(part.len());
                let mut pwnl: Vec<_> = data.split(|&i| i == b'\n').collect();
                if pwnl.len() > 1 {
                    let l = pwnl.pop().unwrap().to_vec();
                    for i in pwnl {
                        let s = core::str::from_utf8(i).expect("unable to decode line");
                        tracing::info!("{}: {}", tid, s);
                        if !s.starts_with('.') {
                            non_dotted = s.to_string();
                        }
                    }
                    data = l;
                }
            }
        }
        let s = core::str::from_utf8(&data[..]).expect("unable to decode line");
        tracing::info!("{}: {}", tid, s);
        if !s.starts_with('.') {
            non_dotted = s.to_string();
        }
        let v: serde_json::Value =
            serde_json::from_str(&non_dotted).expect("unable to parse non-dotted finalize line");
        serde_json::from_value(v["outputs"].clone())
            .expect("unable to retrieve outputs from finalize line")
    }

    pub async fn upload(&self, h: TaggedHash<ThinTree>, data: &ThinTree) -> TaggedHash<ThinTree> {
        let resp = self
            .rcl
            .put(format!("{}/store/{}/thintree", self.base, h))
            .bearer_auth(&self.bearer)
            .json(data)
            .send()
            .await
            .expect("failed to send request");
        if !matches!(resp.status(), StatusCode::OK | StatusCode::NO_CONTENT) {
            tracing::error!("{} :: {}", resp.status(), resp.text().await.unwrap());
            panic!("upload failed");
        }
        h
    }

    pub async fn has_out_hash(&self, h: TaggedHash<ThinTree>) -> bool {
        let resp = self
            .rcl
            .head(format!("{}/store/{}/thintree", self.base, h))
            .send()
            .await
            .expect("failed to send request");
        match resp.status() {
            StatusCode::OK | StatusCode::NO_CONTENT => true,
            StatusCode::NOT_FOUND => false,
            x => {
                tracing::warn!("{} :: {}", x, resp.text().await.unwrap());
                false
            }
        }
    }

    pub async fn download(&self, h: TaggedHash<ThinTree>) -> Option<ThinTree> {
        let resp = self
            .rcl
            .get(format!("{}/store/{}/thintree", self.base, h))
            .send()
            .await
            .expect("failed to send request");
        match resp.status() {
            StatusCode::OK => Some(resp.json().await.ok()?),
            StatusCode::NOT_FOUND => None,
            x => {
                tracing::error!("{} :: {}", x, resp.text().await.unwrap());
                None
            }
        }
    }

    pub async fn download_regular(&self, h: TaggedHash<Regular>) -> Option<Regular> {
        let resp = self
            .rcl
            .get(format!("{}/.links/{}", self.base, h))
            .send()
            .await
            .expect("failed to send request");
        match resp.status() {
            StatusCode::OK => {
                let executable =
                    resp.headers().get("x-executable").map(|i| i == "true") == Some(true);
                let dat = resp.bytes().await.ok()?;
                Some(Regular {
                    executable,
                    contents: dat.to_vec(),
                })
            }
            StatusCode::NOT_FOUND => None,
            x => {
                tracing::error!("{} :: {}", x, resp.text().await.unwrap());
                None
            }
        }
    }
}
