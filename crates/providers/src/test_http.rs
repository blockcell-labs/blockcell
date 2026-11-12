//! Local HTTP fixtures for exercising the real streaming parsers.
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) async fn serve(body: String) -> (String, tokio::task::JoinHandle<()>) {
    let (url, capture) = serve_capture(body).await;
    (
        url,
        tokio::spawn(async move {
            capture.await.unwrap();
        }),
    )
}

/// Serve one local response and return the actual JSON request sent by the provider.
pub(crate) async fn serve_capture(
    body: String,
) -> (String, tokio::task::JoinHandle<serde_json::Value>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut bytes = [0; 1024];
            let body_start = loop {
                let n = socket.read(&mut bytes).await.unwrap();
                assert!(n > 0, "client closed before sending request");
                request.extend_from_slice(&bytes[..n]);
                if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let body_len = headers.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    }).unwrap_or(0);
                    if request.len() >= end + 4 + body_len { break end + 4; }
                }
                assert!(request.len() < 64 * 1024, "fixture request too large");
            };
            let request: serde_json::Value = serde_json::from_slice(&request[body_start..]).unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
            for byte in body.bytes() {
                socket.write_all(&[b'1', b'\r', b'\n', byte, b'\r', b'\n']).await.unwrap();
            }
            socket.write_all(b"0\r\n\r\n").await.unwrap();
            request
        }).await.expect("mock HTTP server timed out")
    });
    (url, task)
}
