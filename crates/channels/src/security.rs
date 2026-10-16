use blockcell_core::{Error, Result};
use futures::StreamExt;
use reqwest::{Response, Url};
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};

pub(crate) const MAX_MEDIA_DOWNLOAD_BYTES: u64 = 25 * 1024 * 1024;

pub(crate) fn safe_filename(value: &str) -> Result<String> {
    let normalized = value.replace('\\', "/");
    let name = normalized.rsplit('/').next().unwrap_or("").trim();
    if name.is_empty() || name == "." || name == ".." {
        return Err(Error::Channel("Unsafe or empty media filename".to_string()));
    }
    Ok(name.to_string())
}

pub(crate) fn unique_media_filename(value: &str) -> Result<String> {
    let safe = safe_filename(value)?;
    let nonce = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    Ok(format!("{}_{}", nonce, safe))
}

pub(crate) fn safe_path_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        "unknown".to_string()
    } else {
        sanitized
    }
}

pub(crate) fn safe_relative_dir(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(Error::Channel(
            "Media download directory must stay inside the workspace".to_string(),
        ));
    }
    Ok(path.to_path_buf())
}

pub(crate) async fn read_response_limited(response: Response, max_bytes: u64) -> Result<Vec<u8>> {
    if response.content_length().is_some_and(|len| len > max_bytes) {
        return Err(Error::Channel(format!(
            "Media download exceeds size limit of {} bytes",
            max_bytes
        )));
    }

    let mut body = response.bytes_stream();
    let mut data = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| Error::Channel(format!("Failed to read response: {}", e)))?;
        append_limited(&mut data, &chunk, max_bytes)?;
    }
    Ok(data)
}

fn append_limited(data: &mut Vec<u8>, chunk: &[u8], max_bytes: u64) -> Result<()> {
    if data.len() as u64 + chunk.len() as u64 > max_bytes {
        return Err(Error::Channel(format!(
            "Media download exceeds size limit of {} bytes",
            max_bytes
        )));
    }
    data.extend_from_slice(chunk);
    Ok(())
}

fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
                || ip.octets()[0] == 0
                || ip.octets()[0] >= 224
                || ip.octets() == [100, 100, 100, 200]
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_forbidden_ip(IpAddr::V4(v4)))
        }
    }
}

pub(crate) async fn validate_public_http_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).map_err(|e| Error::Channel(format!("Invalid media URL: {}", e)))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::Channel(
            "Only HTTP(S) media URLs are allowed".to_string(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| Error::Channel("Media URL has no host".to_string()))?;
    if host.eq_ignore_ascii_case("localhost") {
        return Err(Error::Channel(
            "Private media URL target is not allowed".to_string(),
        ));
    }
    // Reject IP literals deterministically instead of relying on DNS: `host_str()`
    // keeps the brackets for IPv6, so strip them before parsing. Without this the
    // loopback/link-local verdict depends on the host resolver's behaviour.
    let literal = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = literal.parse::<IpAddr>() {
        if is_forbidden_ip(ip) {
            return Err(Error::Channel(
                "Private media URL target is not allowed".to_string(),
            ));
        }
    }
    let port = url.port_or_known_default().unwrap_or(80);
    let addresses: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| Error::Channel(format!("Failed to resolve media URL host: {}", e)))?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|addr| is_forbidden_ip(addr.ip())) {
        return Err(Error::Channel(
            "Private media URL target is not allowed".to_string(),
        ));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_discards_untrusted_path_components() {
        assert_eq!(safe_filename("../secret.txt").unwrap(), "secret.txt");
        assert_eq!(safe_filename("C:\\temp\\secret.txt").unwrap(), "secret.txt");
        assert!(safe_filename("..").is_err());
    }

    #[test]
    fn media_directory_must_be_workspace_relative() {
        assert!(safe_relative_dir("downloads/media").is_ok());
        assert!(safe_relative_dir("../outside").is_err());
        assert!(safe_relative_dir("/tmp/outside").is_err());
    }

    #[tokio::test]
    async fn ssrf_guard_rejects_local_addresses() {
        for url in [
            "http://127.0.0.1/a",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/a",
            "http://localhost/a",
        ] {
            assert!(validate_public_http_url(url).await.is_err(), "{url}");
        }
    }

    #[test]
    fn streamed_chunks_cannot_exceed_limit() {
        let mut data = Vec::new();
        append_limited(&mut data, b"123", 5).unwrap();
        assert!(append_limited(&mut data, b"456", 5).is_err());
        assert_eq!(data, b"123");
    }
}
