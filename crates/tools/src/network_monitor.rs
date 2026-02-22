use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// Network monitoring and diagnostics tool.
///
/// Actions:
/// - **ping**: ICMP ping with statistics
/// - **traceroute**: Network path tracing
/// - **port_scan**: TCP port scanning (connect scan)
/// - **ssl_check**: SSL/TLS certificate inspection
/// - **dns_lookup**: DNS record queries (A, AAAA, MX, CNAME, TXT, NS, SOA)
/// - **whois**: Domain WHOIS lookup
/// - **http_check**: HTTP endpoint health check with timing
/// - **bandwidth**: Simple bandwidth estimation via download test
pub struct NetworkMonitorTool;

#[async_trait]
impl Tool for NetworkMonitorTool {
    fn schema(&self) -> ToolSchema {
        let mut props = serde_json::Map::new();
        props.insert("action".into(), json!({"type": "string", "description": "Action: ping|traceroute|port_scan|ssl_check|dns_lookup|whois|http_check|bandwidth"}));
        props.insert("host".into(), json!({"type": "string", "description": "Target hostname or IP address"}));
        props.insert("port".into(), json!({"type": "integer", "description": "(port_scan/ssl_check) Single port number. Default for ssl_check: 443"}));
        props.insert("ports".into(), json!({"type": "string", "description": "(port_scan) Port range or list: '80,443,8080' or '1-1024' or 'common'. Default: common"}));
        props.insert("count".into(), json!({"type": "integer", "description": "(ping) Number of ping packets. Default: 4"}));
        props.insert("timeout".into(), json!({"type": "integer", "description": "Timeout in seconds. Default: 10"}));
        props.insert("record_type".into(), json!({"type": "string", "enum": ["A", "AAAA", "MX", "CNAME", "TXT", "NS", "SOA", "ANY"], "description": "(dns_lookup) DNS record type. Default: A"}));
        props.insert("dns_server".into(), json!({"type": "string", "description": "(dns_lookup) Custom DNS server (e.g. '8.8.8.8', '1.1.1.1')"}));
        props.insert("url".into(), json!({"type": "string", "description": "(http_check/bandwidth) Full URL to check"}));
        props.insert("max_hops".into(), json!({"type": "integer", "description": "(traceroute) Maximum hops. Default: 30"}));
        props.insert("concurrent".into(), json!({"type": "integer", "description": "(port_scan) Max concurrent connections. Default: 50"}));

        ToolSchema {
            name: "network_monitor",
            description: "Network monitoring and diagnostics. Ping hosts, trace routes, scan ports, check SSL certificates (expiry, issuer, chain), query DNS records, perform WHOIS lookups, and check HTTP endpoint health with timing. All operations use standard system tools (ping, traceroute, openssl, dig, whois, curl).",
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid = ["ping", "traceroute", "port_scan", "ssl_check", "dns_lookup", "whois", "http_check", "bandwidth"];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        Ok(())
    }

    async fn execute(&self, _ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap_or("");
        debug!(action = action, "network_monitor execute");

        match action {
            "ping" => action_ping(&params).await,
            "traceroute" => action_traceroute(&params).await,
            "port_scan" => action_port_scan(&params).await,
            "ssl_check" => action_ssl_check(&params).await,
            "dns_lookup" => action_dns_lookup(&params).await,
            "whois" => action_whois(&params).await,
            "http_check" => action_http_check(&params).await,
            "bandwidth" => action_bandwidth(&params).await,
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

// ─── Ping ───────────────────────────────────────────────────────────────────

async fn action_ping(params: &Value) -> Result<Value> {
    let host = params.get("host").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("host is required for ping".into()))?;
    let count = params.get("count").and_then(|v| v.as_u64()).unwrap_or(4);
    let timeout = params.get("timeout").and_then(|v| v.as_u64()).unwrap_or(10);

    let output = tokio::process::Command::new("ping")
        .args(["-c", &count.to_string(), "-W", &(timeout * 1000).to_string(), host])
        .output()
        .await
        .map_err(|e| Error::Tool(format!("ping failed: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // Parse ping statistics
    let mut result = json!({
        "host": host,
        "reachable": output.status.success(),
        "raw_output": if stdout.len() > 2000 { crate::safe_truncate(&stdout, 2000) } else { &stdout },
    });

    // Parse "X packets transmitted, Y received, Z% packet loss"
    if let Some(stats_line) = stdout.lines().find(|l| l.contains("packet loss")) {
        result["stats_line"] = json!(stats_line.trim());
        // Parse packet loss percentage
        if let Some(loss) = extract_between(stats_line, ", ", "% packet loss") {
            result["packet_loss_percent"] = json!(loss.parse::<f64>().unwrap_or(-1.0));
        }
    }

    // Parse "round-trip min/avg/max/stddev = X/Y/Z/W ms"
    if let Some(rtt_line) = stdout.lines().find(|l| l.contains("min/avg/max")) {
        if let Some(values) = extract_between(rtt_line, "= ", " ms") {
            let parts: Vec<&str> = values.split('/').collect();
            if parts.len() >= 4 {
                result["rtt_min_ms"] = json!(parts[0].parse::<f64>().unwrap_or(0.0));
                result["rtt_avg_ms"] = json!(parts[1].parse::<f64>().unwrap_or(0.0));
                result["rtt_max_ms"] = json!(parts[2].parse::<f64>().unwrap_or(0.0));
                result["rtt_stddev_ms"] = json!(parts[3].parse::<f64>().unwrap_or(0.0));
            }
        }
    }

    if !stderr.is_empty() && !output.status.success() {
        result["error"] = json!(stderr.trim());
    }

    Ok(result)
}

// ─── Traceroute ─────────────────────────────────────────────────────────────

async fn action_traceroute(params: &Value) -> Result<Value> {
    let host = params.get("host").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("host is required for traceroute".into()))?;
    let max_hops = params.get("max_hops").and_then(|v| v.as_u64()).unwrap_or(30);
    let timeout = params.get("timeout").and_then(|v| v.as_u64()).unwrap_or(5);

    let output = tokio::process::Command::new("traceroute")
        .args(["-m", &max_hops.to_string(), "-w", &timeout.to_string(), host])
        .output()
        .await
        .map_err(|e| Error::Tool(format!("traceroute failed: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    // Parse hops
    let mut hops = Vec::new();
    for line in stdout.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() { continue; }
        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        if parts.len() >= 2 {
            let hop_num = parts[0].trim();
            let rest = parts[1].trim();
            hops.push(json!({
                "hop": hop_num,
                "detail": rest,
            }));
        }
    }

    Ok(json!({
        "host": host,
        "hops": hops,
        "total_hops": hops.len(),
        "max_hops": max_hops,
    }))
}

// ─── Port Scan ──────────────────────────────────────────────────────────────

async fn action_port_scan(params: &Value) -> Result<Value> {
    let host = params.get("host").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("host is required for port_scan".into()))?;
    let timeout = params.get("timeout").and_then(|v| v.as_u64()).unwrap_or(3);

    let ports = resolve_ports(params)?;

    // Use nc (netcat) for port scanning — available on macOS by default
    let mut open_ports = Vec::new();
    let mut closed_ports = Vec::new();

    // Scan in batches to limit concurrency
    let concurrent = params.get("concurrent").and_then(|v| v.as_u64()).unwrap_or(50) as usize;

    for chunk in ports.chunks(concurrent) {
        let mut handles = Vec::new();
        for &port in chunk {
            let host = host.to_string();
            let timeout_secs = timeout;
            handles.push(tokio::spawn(async move {
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(timeout_secs),
                    tokio::net::TcpStream::connect(format!("{}:{}", host, port)),
                ).await;
                match result {
                    Ok(Ok(_)) => (port, true),
                    _ => (port, false),
                }
            }));
        }

        for handle in handles {
            if let Ok((port, is_open)) = handle.await {
                if is_open {
                    open_ports.push(json!({
                        "port": port,
                        "state": "open",
                        "service": guess_service(port),
                    }));
                } else {
                    closed_ports.push(port);
                }
            }
        }
    }

    Ok(json!({
        "host": host,
        "open_ports": open_ports,
        "open_count": open_ports.len(),
        "closed_count": closed_ports.len(),
        "total_scanned": open_ports.len() + closed_ports.len(),
        "timeout_seconds": timeout,
    }))
}

fn resolve_ports(params: &Value) -> Result<Vec<u16>> {
    // Single port
    if let Some(port) = params.get("port").and_then(|v| v.as_u64()) {
        return Ok(vec![port as u16]);
    }

    let ports_str = params.get("ports").and_then(|v| v.as_str()).unwrap_or("common");

    if ports_str == "common" {
        return Ok(vec![
            21, 22, 23, 25, 53, 80, 110, 143, 443, 465, 587, 993, 995,
            3306, 3389, 5432, 5900, 6379, 8080, 8443, 8888, 9090, 27017,
        ]);
    }

    let mut ports = Vec::new();
    for part in ports_str.split(',') {
        let part = part.trim();
        if part.contains('-') {
            let range: Vec<&str> = part.splitn(2, '-').collect();
            if range.len() == 2 {
                let start: u16 = range[0].parse().map_err(|_| Error::Tool(format!("Invalid port: {}", range[0])))?;
                let end: u16 = range[1].parse().map_err(|_| Error::Tool(format!("Invalid port: {}", range[1])))?;
                if end < start {
                    return Err(Error::Tool(format!("Invalid port range: {}-{}", start, end)));
                }
                if (end - start) > 10000 {
                    return Err(Error::Tool("Port range too large (max 10000 ports)".into()));
                }
                for p in start..=end {
                    ports.push(p);
                }
            }
        } else {
            let p: u16 = part.parse().map_err(|_| Error::Tool(format!("Invalid port: {}", part)))?;
            ports.push(p);
        }
    }

    Ok(ports)
}

fn guess_service(port: u16) -> &'static str {
    match port {
        21 => "ftp",
        22 => "ssh",
        23 => "telnet",
        25 => "smtp",
        53 => "dns",
        80 => "http",
        110 => "pop3",
        143 => "imap",
        443 => "https",
        465 => "smtps",
        587 => "submission",
        993 => "imaps",
        995 => "pop3s",
        3306 => "mysql",
        3389 => "rdp",
        5432 => "postgresql",
        5900 => "vnc",
        6379 => "redis",
        8080 => "http-alt",
        8443 => "https-alt",
        8888 => "http-alt",
        9090 => "http-alt",
        27017 => "mongodb",
        _ => "unknown",
    }
}

// ─── SSL Check ──────────────────────────────────────────────────────────────

async fn action_ssl_check(params: &Value) -> Result<Value> {
    let host = params.get("host").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("host is required for ssl_check".into()))?;
    let port = params.get("port").and_then(|v| v.as_u64()).unwrap_or(443);
    let timeout = params.get("timeout").and_then(|v| v.as_u64()).unwrap_or(10);

    // Use openssl s_client to get certificate info
    let cmd = format!(
        "echo | openssl s_client -connect {}:{} -servername {} 2>/dev/null | openssl x509 -noout -subject -issuer -dates -serial -fingerprint -ext subjectAltName 2>/dev/null",
        host, port, host
    );

    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&cmd)
        .output()
        .await
        .map_err(|e| Error::Tool(format!("openssl failed: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    if stdout.trim().is_empty() {
        return Err(Error::Tool(format!("Could not connect to {}:{} or no SSL certificate found", host, port)));
    }

    let mut result = json!({
        "host": host,
        "port": port,
        "valid": true,
    });

    for line in stdout.lines() {
        let line = line.trim();
        if line.starts_with("subject=") {
            result["subject"] = json!(line.trim_start_matches("subject=").trim());
        } else if line.starts_with("issuer=") {
            result["issuer"] = json!(line.trim_start_matches("issuer=").trim());
        } else if line.starts_with("notBefore=") {
            result["not_before"] = json!(line.trim_start_matches("notBefore=").trim());
        } else if line.starts_with("notAfter=") {
            let expiry = line.trim_start_matches("notAfter=").trim();
            result["not_after"] = json!(expiry);
            // Calculate days until expiry
            if let Ok(exp_date) = chrono::NaiveDateTime::parse_from_str(expiry, "%b %d %H:%M:%S %Y GMT")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(expiry, "%b  %d %H:%M:%S %Y GMT"))
            {
                let now = chrono::Utc::now().naive_utc();
                let days = (exp_date - now).num_days();
                result["days_until_expiry"] = json!(days);
                result["expired"] = json!(days < 0);
                if days < 30 {
                    result["warning"] = json!(format!("Certificate expires in {} days!", days));
                }
            }
        } else if line.starts_with("serial=") {
            result["serial"] = json!(line.trim_start_matches("serial=").trim());
        } else if line.contains("Fingerprint=") {
            result["fingerprint"] = json!(line.trim());
        } else if line.contains("DNS:") {
            let sans: Vec<&str> = line.split(',')
                .map(|s| s.trim().trim_start_matches("DNS:"))
                .filter(|s| !s.is_empty())
                .collect();
            result["subject_alt_names"] = json!(sans);
        }
    }

    // Also check TLS version
    let tls_cmd = format!(
        "echo | openssl s_client -connect {}:{} -servername {} 2>/dev/null | grep 'Protocol\\|Cipher'",
        host, port, host
    );
    if let Ok(tls_output) = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&tls_cmd)
        .output()
        .await
    {
        let tls_stdout = String::from_utf8_lossy(&tls_output.stdout);
        for line in tls_stdout.lines() {
            let line = line.trim();
            if line.contains("Protocol") {
                result["tls_version"] = json!(line.split(':').next_back().unwrap_or("").trim());
            } else if line.contains("Cipher") && !line.contains("Server") {
                result["cipher"] = json!(line.split(':').next_back().unwrap_or("").trim());
            }
        }
    }

    let _ = timeout; // used in concept, openssl has its own timeout
    Ok(result)
}

// ─── DNS Lookup ─────────────────────────────────────────────────────────────

async fn action_dns_lookup(params: &Value) -> Result<Value> {
    let host = params.get("host").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("host is required for dns_lookup".into()))?;
    let record_type = params.get("record_type").and_then(|v| v.as_str()).unwrap_or("A");
    let dns_server = params.get("dns_server").and_then(|v| v.as_str());

    let mut args = vec!["dig".to_string()];
    if let Some(server) = dns_server {
        args.push(format!("@{}", server));
    }
    args.push(host.to_string());
    args.push(record_type.to_string());
    args.push("+noall".to_string());
    args.push("+answer".to_string());
    args.push("+stats".to_string());

    let output = tokio::process::Command::new(&args[0])
        .args(&args[1..])
        .output()
        .await
        .map_err(|e| Error::Tool(format!("dig failed: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    // Parse dig output
    let mut records = Vec::new();
    let mut query_time = None;
    let mut server_used = None;

    for line in stdout.lines() {
        let line = line.trim();
        if line.starts_with(";;") {
            if line.contains("Query time:") {
                query_time = Some(line.trim_start_matches(";;").trim().to_string());
            } else if line.contains("SERVER:") {
                server_used = Some(line.trim_start_matches(";;").trim().to_string());
            }
            continue;
        }
        if line.is_empty() || line.starts_with(';') { continue; }

        // Parse record line: name TTL class type value
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 5 {
            records.push(json!({
                "name": parts[0],
                "ttl": parts[1],
                "class": parts[2],
                "type": parts[3],
                "value": parts[4..].join(" "),
            }));
        }
    }

    Ok(json!({
        "host": host,
        "record_type": record_type,
        "records": records,
        "count": records.len(),
        "query_time": query_time,
        "server": server_used.or_else(|| dns_server.map(|s| s.to_string())),
    }))
}

// ─── WHOIS ──────────────────────────────────────────────────────────────────

async fn action_whois(params: &Value) -> Result<Value> {
    let host = params.get("host").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("host is required for whois".into()))?;

    let output = tokio::process::Command::new("whois")
        .arg(host)
        .output()
        .await
        .map_err(|e| Error::Tool(format!("whois failed: {}", e)))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    if stdout.trim().is_empty() {
        return Err(Error::Tool(format!("No WHOIS data found for {}", host)));
    }

    // Extract key fields
    let mut result = json!({"domain": host});
    let mut raw_lines = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('%') || line.starts_with('#') { continue; }
        raw_lines.push(line.to_string());

        let lower = line.to_lowercase();
        if lower.starts_with("registrar:") || lower.starts_with("registrar name:") {
            result["registrar"] = json!(line.split_once(':').map(|x| x.1).unwrap_or("").trim());
        } else if lower.starts_with("creation date:") || lower.starts_with("registered on:") {
            result["creation_date"] = json!(line.split_once(':').map(|x| x.1).unwrap_or("").trim());
        } else if lower.starts_with("expiry date:") || lower.starts_with("registry expiry date:") || lower.starts_with("expiration date:") {
            result["expiry_date"] = json!(line.split_once(':').map(|x| x.1).unwrap_or("").trim());
        } else if lower.starts_with("updated date:") || lower.starts_with("last updated:") {
            result["updated_date"] = json!(line.split_once(':').map(|x| x.1).unwrap_or("").trim());
        } else if lower.starts_with("name server:") || lower.starts_with("nserver:") {
            let ns = line.split_once(':').map(|x| x.1).unwrap_or("").trim();
            let existing = result.get("name_servers").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let mut servers = existing;
            servers.push(json!(ns));
            result["name_servers"] = json!(servers);
        } else if lower.starts_with("domain status:") || lower.starts_with("status:") {
            let status = line.split_once(':').map(|x| x.1).unwrap_or("").trim();
            let existing = result.get("status").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let mut statuses = existing;
            statuses.push(json!(status));
            result["status"] = json!(statuses);
        } else if lower.starts_with("registrant organization:") || lower.starts_with("registrant:") {
            result["registrant"] = json!(line.split_once(':').map(|x| x.1).unwrap_or("").trim());
        }
    }

    // Include truncated raw output
    let raw_text: String = raw_lines.into_iter().take(50).collect::<Vec<_>>().join("\n");
    result["raw_excerpt"] = json!(raw_text);

    Ok(result)
}

// ─── HTTP Check ─────────────────────────────────────────────────────────────

async fn action_http_check(params: &Value) -> Result<Value> {
    let url = params.get("url").and_then(|v| v.as_str())
        .or_else(|| {
            params.get("host").and_then(|v| v.as_str()).map(|h| {
                if h.starts_with("http") { h } else { "" }
            }).filter(|s| !s.is_empty())
        })
        .ok_or_else(|| Error::Tool("url is required for http_check".into()))?;

    let timeout = params.get("timeout").and_then(|v| v.as_u64()).unwrap_or(10);

    let start = std::time::Instant::now();

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| Error::Tool(format!("HTTP client error: {}", e)))?;

    let resp = client.get(url).send().await;
    let elapsed = start.elapsed();

    match resp {
        Ok(response) => {
            let status = response.status();
            let headers: Vec<Value> = response.headers().iter()
                .take(20)
                .map(|(k, v)| json!({k.as_str(): v.to_str().unwrap_or("")}))
                .collect();
            let content_length = response.content_length();
            let content_type = response.headers().get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();

            Ok(json!({
                "url": url,
                "status_code": status.as_u16(),
                "status_text": status.canonical_reason().unwrap_or(""),
                "healthy": status.is_success(),
                "response_time_ms": elapsed.as_millis(),
                "content_type": content_type,
                "content_length": content_length,
                "headers_sample": headers,
            }))
        }
        Err(e) => {
            Ok(json!({
                "url": url,
                "healthy": false,
                "error": format!("{}", e),
                "response_time_ms": elapsed.as_millis(),
                "is_timeout": e.is_timeout(),
                "is_connect": e.is_connect(),
            }))
        }
    }
}

// ─── Bandwidth ──────────────────────────────────────────────────────────────

async fn action_bandwidth(params: &Value) -> Result<Value> {
    let url = params.get("url").and_then(|v| v.as_str())
        .unwrap_or("https://speed.cloudflare.com/__down?bytes=10000000");
    let timeout = params.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout))
        .build()
        .map_err(|e| Error::Tool(format!("HTTP client error: {}", e)))?;

    let start = std::time::Instant::now();
    let resp = client.get(url).send().await
        .map_err(|e| Error::Tool(format!("Download failed: {}", e)))?;

    let bytes = resp.bytes().await
        .map_err(|e| Error::Tool(format!("Failed to read response: {}", e)))?;
    let elapsed = start.elapsed();

    let bytes_downloaded = bytes.len() as f64;
    let seconds = elapsed.as_secs_f64();
    let mbps = if seconds > 0.0 {
        (bytes_downloaded * 8.0) / (seconds * 1_000_000.0)
    } else {
        0.0
    };

    Ok(json!({
        "bytes_downloaded": bytes.len(),
        "duration_seconds": format!("{:.2}", seconds),
        "speed_mbps": format!("{:.2}", mbps),
        "speed_mbytes_per_sec": format!("{:.2}", bytes_downloaded / (seconds * 1_000_000.0)),
        "test_url": url,
    }))
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn extract_between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let start_idx = text.find(start)? + start.len();
    let end_idx = text[start_idx..].find(end)? + start_idx;
    Some(&text[start_idx..end_idx])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_tool() -> NetworkMonitorTool { NetworkMonitorTool }

    #[test]
    fn test_schema() {
        let tool = make_tool();
        let schema = tool.schema();
        assert_eq!(schema.name, "network_monitor");
        assert!(schema.parameters["properties"]["action"].is_object());
    }

    #[test]
    fn test_validate_valid() {
        let tool = make_tool();
        assert!(tool.validate(&json!({"action": "ping"})).is_ok());
        assert!(tool.validate(&json!({"action": "ssl_check"})).is_ok());
        assert!(tool.validate(&json!({"action": "dns_lookup"})).is_ok());
    }

    #[test]
    fn test_validate_invalid() {
        let tool = make_tool();
        assert!(tool.validate(&json!({"action": "hack"})).is_err());
    }

    #[test]
    fn test_resolve_ports_common() {
        let ports = resolve_ports(&json!({"ports": "common"})).unwrap();
        assert!(ports.contains(&80));
        assert!(ports.contains(&443));
        assert!(ports.contains(&22));
    }

    #[test]
    fn test_resolve_ports_range() {
        let ports = resolve_ports(&json!({"ports": "80-85"})).unwrap();
        assert_eq!(ports, vec![80, 81, 82, 83, 84, 85]);
    }

    #[test]
    fn test_resolve_ports_list() {
        let ports = resolve_ports(&json!({"ports": "22,80,443"})).unwrap();
        assert_eq!(ports, vec![22, 80, 443]);
    }

    #[test]
    fn test_resolve_ports_single() {
        let ports = resolve_ports(&json!({"port": 8080})).unwrap();
        assert_eq!(ports, vec![8080]);
    }

    #[test]
    fn test_guess_service() {
        assert_eq!(guess_service(22), "ssh");
        assert_eq!(guess_service(80), "http");
        assert_eq!(guess_service(443), "https");
        assert_eq!(guess_service(3306), "mysql");
        assert_eq!(guess_service(12345), "unknown");
    }

    #[test]
    fn test_extract_between() {
        assert_eq!(extract_between("loss: 25% packet loss", "loss: ", "% packet loss"), Some("25"));
        assert_eq!(extract_between("no match here", "x", "y"), None);
    }

    #[test]
    fn test_validate_all_actions() {
        let tool = make_tool();
        for action in &["ping", "traceroute", "port_scan", "ssl_check", "dns_lookup", "whois", "http_check", "bandwidth"] {
            assert!(tool.validate(&json!({"action": action})).is_ok());
        }
    }
}
