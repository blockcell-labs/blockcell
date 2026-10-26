use blockcell_core::{Error, Result};
use futures::StreamExt;
use reqwest::{blocking::Response as BlockingResponse, Response, Url};
use std::io::Read;

pub(crate) const MAX_PROVIDER_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_PROVIDER_ERROR_BYTES: usize = 64 * 1024;
pub(crate) const MAX_STREAM_BUFFER_BYTES: usize = 1024 * 1024;

pub(crate) fn redact_url(value: &str) -> String {
    let Ok(mut url) = Url::parse(value) else {
        return "<invalid-url>".to_string();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    if url.query().is_some() {
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(key, value)| {
                let sensitive = matches!(
                    key.to_ascii_lowercase().as_str(),
                    "key"
                        | "api_key"
                        | "apikey"
                        | "token"
                        | "access_token"
                        | "signature"
                        | "secret"
                );
                (
                    key.into_owned(),
                    if sensitive {
                        "***".to_string()
                    } else {
                        value.into_owned()
                    },
                )
            })
            .collect();
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    url.to_string()
}

pub(crate) fn request_error(context: &str, error: reqwest::Error) -> Error {
    Error::Provider(format!("{}: {}", context, error.without_url()))
}

pub(crate) fn http_status_error(
    provider: &str,
    status: reqwest::StatusCode,
    body_len: usize,
) -> Error {
    Error::Provider(format!(
        "HTTP_STATUS={} {} API request failed (body_len={})",
        status.as_u16(),
        provider,
        body_len
    ))
}

pub(crate) fn http_status_error_with_retry_after(
    provider: &str,
    status: reqwest::StatusCode,
    body_len: usize,
    retry_after: Option<&str>,
) -> Error {
    let Some(retry_after) = retry_after else {
        return http_status_error(provider, status, body_len);
    };
    Error::Provider(format!(
        "HTTP_STATUS={} {} API request failed (body_len={}, retry_after={})",
        status.as_u16(),
        provider,
        body_len,
        retry_after
    ))
}

pub(crate) fn append_limited(data: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) -> Result<()> {
    if data.len().saturating_add(chunk.len()) > max_bytes {
        return Err(Error::Provider(format!(
            "Provider response exceeds size limit of {} bytes",
            max_bytes
        )));
    }
    data.extend_from_slice(chunk);
    Ok(())
}

pub(crate) async fn read_response_limited(response: Response, max_bytes: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(Error::Provider(format!(
            "Provider response exceeds size limit of {} bytes",
            max_bytes
        )));
    }
    let mut data = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|error| request_error("Failed to read provider response", error))?;
        append_limited(&mut data, &chunk, max_bytes)?;
    }
    Ok(data)
}

pub(crate) async fn consume_error_body(response: Response) -> usize {
    let declared = response.content_length().map(|length| length as usize);
    match read_response_limited(response, MAX_PROVIDER_ERROR_BYTES).await {
        Ok(body) => body.len(),
        Err(_) => declared.unwrap_or(MAX_PROVIDER_ERROR_BYTES.saturating_add(1)),
    }
}

pub(crate) fn read_blocking_response_limited(
    mut response: BlockingResponse,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(Error::Provider(format!(
            "Provider response exceeds size limit of {} bytes",
            max_bytes
        )));
    }
    let mut data = Vec::new();
    response
        .by_ref()
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|error| Error::Provider(format!("Failed to read provider response: {}", error)))?;
    if data.len() > max_bytes {
        return Err(Error::Provider(format!(
            "Provider response exceeds size limit of {} bytes",
            max_bytes
        )));
    }
    Ok(data)
}

pub(crate) fn consume_blocking_error_body(response: BlockingResponse) -> usize {
    let declared = response.content_length().map(|length| length as usize);
    match read_blocking_response_limited(response, MAX_PROVIDER_ERROR_BYTES) {
        Ok(body) => body.len(),
        Err(_) => declared.unwrap_or(MAX_PROVIDER_ERROR_BYTES.saturating_add(1)),
    }
}

pub(crate) struct StreamGuard {
    max_bytes: usize,
    seen_bytes: usize,
}

impl StreamGuard {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            seen_bytes: 0,
        }
    }

    pub(crate) fn observe(&mut self, bytes: usize) -> Result<()> {
        self.seen_bytes = self.seen_bytes.saturating_add(bytes);
        if self.seen_bytes > self.max_bytes {
            return Err(Error::Provider(format!(
                "Provider stream exceeds size limit of {} bytes",
                self.max_bytes
            )));
        }
        Ok(())
    }

    pub(crate) fn check_buffer(&self, bytes: usize) -> Result<()> {
        if bytes > MAX_STREAM_BUFFER_BYTES {
            return Err(Error::Provider(format!(
                "Provider stream line exceeds size limit of {} bytes",
                MAX_STREAM_BUFFER_BYTES
            )));
        }
        Ok(())
    }

    pub(crate) fn finish(&self, completed: bool, label: &str) -> Result<()> {
        if completed {
            Ok(())
        } else {
            Err(Error::Provider(format!(
                "{} ended before its completion marker",
                label
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_url_removes_userinfo_and_sensitive_query_values() {
        let redacted = redact_url("https://alice:secret@example.com/v1?key=gemini-secret&foo=bar");
        assert!(!redacted.contains("alice"));
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("gemini-secret"));
        assert!(redacted.contains("foo=bar"));
    }

    #[test]
    fn bounded_accumulator_rejects_oversized_chunks() {
        let mut data = Vec::new();
        append_limited(&mut data, b"123", 5).unwrap();
        assert!(append_limited(&mut data, b"456", 5).is_err());
        assert_eq!(data, b"123");
    }

    #[test]
    fn stream_guard_requires_explicit_completion() {
        let mut guard = StreamGuard::new(5);
        guard.observe(3).unwrap();
        assert!(guard.observe(3).is_err());
        assert!(guard.finish(false, "OpenAI stream").is_err());
        assert!(guard.finish(true, "OpenAI stream").is_ok());
    }
}
