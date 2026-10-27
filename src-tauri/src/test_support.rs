//! Loopback HTTP fixtures exercise the production request and streaming paths.
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
};

pub struct Response {
    pub status: u16,
    pub chunks: Vec<Vec<u8>>,
    pub after_first: Option<oneshot::Receiver<()>>,
}

impl Response {
    pub fn ok(bytes: &[u8]) -> Self {
        Self {
            status: 200,
            chunks: bytes.chunks(7).map(<[u8]>::to_vec).collect(),
            after_first: None,
        }
    }
}

pub struct Request {
    pub headers: String,
    pub body: serde_json::Value,
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

pub async fn http(responses: Vec<Response>) -> (String, JoinHandle<Vec<Request>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(8), async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut raw = Vec::new();
                let headers_end = loop {
                    let mut buffer = [0; 1024];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0, "request ended before HTTP headers");
                    raw.extend_from_slice(&buffer[..count]);
                    if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                    assert!(raw.len() < 16384);
                };
                let headers = String::from_utf8(raw[..headers_end].to_vec()).unwrap();
                let length: usize = headers
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .unwrap()
                    .1
                    .trim()
                    .parse()
                    .unwrap();
                while raw.len() < headers_end + length {
                    let mut buffer = [0; 1024];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0, "request body truncated");
                    raw.extend_from_slice(&buffer[..count]);
                }
                requests.push(Request {
                    headers,
                    body: serde_json::from_slice(&raw[headers_end..headers_end + length]).unwrap(),
                });
                socket.write_all(format!("HTTP/1.1 {} Fixture\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n", response.status).as_bytes()).await.unwrap();
                let mut gate = response.after_first;
                for chunk in response.chunks {
                    assert!(!chunk.is_empty());
                    socket.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).await.unwrap();
                    socket.write_all(&chunk).await.unwrap();
                    socket.write_all(b"\r\n").await.unwrap();
                    if let Some(gate) = gate.take() {
                        gate.await.unwrap();
                    }
                    tokio::task::yield_now().await;
                }
                socket.write_all(b"0\r\n\r\n").await.unwrap();
            }
            requests
        }).await.expect("loopback HTTP fixture timed out")
    });
    (url, task)
}
