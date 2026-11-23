//! Optional stdin/stdout transport for Genesis hosts with broken Python networking.
//! Authentication is received through stdin and never appears in arguments/logs.
use serde::Deserialize;
use std::io::{self, Read, Write};
use std::time::Duration;

#[derive(Deserialize)]
struct Request {
    url: String,
    body: serde_json::Value,
    key: String,
    user_agent: String,
    #[serde(default)]
    addresses: Vec<std::net::SocketAddr>,
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let request: Request = serde_json::from_reader(io::stdin().lock())?;
    let mut builder = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(120));
    let url = reqwest::Url::parse(&request.url)?;
    if let Some(host) = url.host_str() {
        if !request.addresses.is_empty() {
            // Override only address resolution. TLS still validates the URL hostname.
            builder = builder.resolve_to_addrs(host, &request.addresses);
        }
    }
    let client = builder.build()?;
    let mut response = client
        .post(&request.url)
        .bearer_auth(&request.key)
        .header(reqwest::header::USER_AGENT, &request.user_agent)
        .json(&request.body)
        .send()?;
    let mut output = io::stdout().lock();
    writeln!(
        output,
        "{}",
        serde_json::json!({
            "status": response.status().as_u16(),
            "headers": {
                "Content-Type": response.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/json"),
                "Retry-After": response.headers().get("retry-after").and_then(|v| v.to_str().ok()).unwrap_or("0")
            }
        })
    )?;
    output.flush()?;
    let mut buffer = [0_u8; 8192];
    loop {
        let length = response.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        output.write_all(&buffer[..length])?;
        output.flush()?;
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        // Never print a request, URL or credential when the transport fails.
        if let Some(request_error) = error.downcast_ref::<reqwest::Error>() {
            eprintln!(
                "Genesis upstream transport failed: connect={}, timeout={}, status={:?}",
                request_error.is_connect(),
                request_error.is_timeout(),
                request_error.status()
            );
        } else {
            eprintln!("Genesis upstream transport failed");
        }
        std::process::exit(1);
    }
}
