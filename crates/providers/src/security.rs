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
    let kind = if error.is_timeout() || error.is_connect() || error.is_body() || error.is_request()
    {
        blockcell_core::FailureKind::ProviderUnavailable
    } else {
        blockcell_core::FailureKind::Unknown
    };
    Error::ProviderFailure(blockcell_core::ProviderFailure {
        kind,
        status: error.status().map(|status| status.as_u16()),
        code: Some("transport".into()),
        retry_after_ms: None,
        message: format!("{}: {}", context, error.without_url()),
    })
}

pub(crate) fn http_status_error(
    provider: &str,
    status: reqwest::StatusCode,
    body_len: usize,
) -> Error {
    http_status_error_with_retry_after(provider, status, body_len, None)
}

pub(crate) fn http_status_error_with_retry_after(
    provider: &str,
    status: reqwest::StatusCode,
    body_len: usize,
    retry_after: Option<&str>,
) -> Error {
    use blockcell_core::FailureKind;
    let kind = match status.as_u16() {
        429 => FailureKind::RateLimited,
        401 | 403 => FailureKind::AuthenticationFailed,
        400 => FailureKind::InvalidRequest,
        500..=599 => FailureKind::ProviderUnavailable,
        _ => FailureKind::Unknown,
    };
    let retry_after_ms = retry_after.and_then(|value| {
        value
            .parse::<u64>()
            .ok()
            .map(|seconds| seconds.saturating_mul(1000))
            .or_else(|| {
                chrono::DateTime::parse_from_rfc2822(value)
                    .ok()
                    .map(|date| {
                        date.signed_duration_since(chrono::Utc::now())
                            .num_milliseconds()
                            .max(0) as u64
                    })
            })
    });
    let message = match retry_after {
        Some(value) => format!(
            "HTTP_STATUS={} {} API request failed (body_len={}, retry_after={})",
            status.as_u16(),
            provider,
            body_len,
            value
        ),
        None => format!(
            "HTTP_STATUS={} {} API request failed (body_len={})",
            status.as_u16(),
            provider,
            body_len
        ),
    };
    Error::ProviderFailure(blockcell_core::ProviderFailure {
        kind,
        status: Some(status.as_u16()),
        code: None,
        retry_after_ms,
        message,
    })
}

// Keep normalized hints safe for callers converting seconds to milliseconds.
const MAX_RETRY_AFTER_SECS: u64 = u64::MAX / 1000;

/// Normalize provider reset hints to a minimum delay in whole seconds. A valid
/// Retry-After takes precedence; otherwise wait for the slowest resource reset.
/// Round upwards so sub-second resets never result in an early retry.
pub(crate) fn retry_after_from_headers(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis()
        .min(u64::MAX as u128) as u64;
    retry_after_from_headers_at(headers, now_ms)
}

fn retry_after_from_headers_at(
    headers: &reqwest::header::HeaderMap,
    local_now_ms: u64,
) -> Option<String> {
    let now_ms = headers
        .get("date")
        .and_then(|value| value.to_str().ok())
        .and_then(http_date_ms)
        .unwrap_or(local_now_ms);
    if let Some(value) = headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
    {
        let value = value.trim();
        if value.len() <= 128 {
            let seconds = value
                .parse::<u64>()
                .ok()
                .map(|v| v.min(MAX_RETRY_AFTER_SECS))
                .or_else(|| http_date_ms(value).map(|at| at.saturating_sub(now_ms).div_ceil(1000)));
            if let Some(seconds) = seconds {
                return Some(seconds.to_string());
            }
        }
    }
    headers
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str();
            if name != "x-ratelimit-reset" && !name.starts_with("x-ratelimit-reset-") {
                return None;
            }
            let value = value.to_str().ok()?.trim();
            if value.is_empty() || value.len() > 128 {
                return None;
            }
            if let Ok(number) = value.parse::<u64>() {
                // Generic reset headers conventionally contain epoch seconds. OpenAI
                // resource reset headers may instead contain a small duration.
                if name == "x-ratelimit-reset" || number >= 1_000_000_000 {
                    let reset_ms = if number >= 1_000_000_000_000 {
                        number
                    } else {
                        number.checked_mul(1000)?
                    };
                    return Some(reset_ms.saturating_sub(now_ms).div_ceil(1000));
                }
                return Some(number.min(MAX_RETRY_AFTER_SECS));
            }
            parse_reset_duration_seconds(value)
        })
        .max()
        .map(|seconds| seconds.to_string())
}

fn http_date_ms(value: &str) -> Option<u64> {
    u64::try_from(
        chrono::DateTime::parse_from_rfc2822(value)
            .ok()?
            .timestamp_millis(),
    )
    .ok()
}

/// Parse the bounded duration syntax used by OpenAI-compatible gateways, such as
/// `1m30s` or `250ms`, without floating-point rounding or unchecked arithmetic.
fn parse_reset_duration_seconds(mut value: &str) -> Option<u64> {
    let mut total_ns = 0u128;
    while !value.is_empty() {
        let digits = value.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let whole = value[..digits].parse::<u128>().ok()?;
        value = &value[digits..];
        let (fraction, scale) = if let Some(rest) = value.strip_prefix('.') {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 || digits > 9 {
                return None;
            }
            let fraction = rest[..digits].parse::<u128>().ok()?;
            value = &rest[digits..];
            (fraction, 10u128.pow(digits as u32))
        } else {
            (0, 1)
        };
        let (unit_ns, rest) = if let Some(rest) = value.strip_prefix("ms") {
            (1_000_000u128, rest)
        } else if let Some(rest) = value.strip_prefix('s') {
            (1_000_000_000, rest)
        } else if let Some(rest) = value.strip_prefix('m') {
            (60_000_000_000, rest)
        } else if let Some(rest) = value.strip_prefix('h') {
            (3_600_000_000_000, rest)
        } else if let Some(rest) = value.strip_prefix('d') {
            (86_400_000_000_000, rest)
        } else {
            return None;
        };
        let ns = whole
            .checked_mul(unit_ns)?
            .checked_add(fraction.checked_mul(unit_ns)?.div_ceil(scale))?;
        total_ns = total_ns.checked_add(ns)?;
        value = rest;
    }
    Some(
        total_ns
            .div_ceil(1_000_000_000)
            .min(MAX_RETRY_AFTER_SECS as u128) as u64,
    )
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

    fn rate_headers(values: &[(&str, &str)]) -> reqwest::header::HeaderMap {
        values
            .iter()
            .map(|(name, value)| {
                (
                    reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                    reqwest::header::HeaderValue::from_str(value).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn retry_after_has_priority_over_rate_limit_reset_hints() {
        let headers = rate_headers(&[
            ("retry-after", " 12 "),
            ("x-ratelimit-reset-tokens", "1m30s"),
        ]);
        assert_eq!(
            retry_after_from_headers_at(&headers, 1_700_000_000_000),
            Some("12".into())
        );
        let zero = rate_headers(&[("retry-after", "0"), ("x-ratelimit-reset", "1700000090")]);
        assert_eq!(
            retry_after_from_headers_at(&zero, 1_700_000_000_000),
            Some("0".into())
        );
    }

    #[test]
    fn rate_limit_reset_durations_round_up_and_use_longest_resource_wait() {
        for (value, seconds) in [
            ("1m30s", "90"),
            ("250ms", "1"),
            ("1.2s", "2"),
            ("1h2m3.5s", "3724"),
            ("2d", "172800"),
            ("3", "3"),
        ] {
            let headers = rate_headers(&[("x-ratelimit-reset-requests", value)]);
            assert_eq!(
                retry_after_from_headers_at(&headers, 1_700_000_000_000),
                Some(seconds.into()),
                "{value}"
            );
        }
        let headers = rate_headers(&[
            ("x-ratelimit-reset-requests", "2s"),
            ("x-ratelimit-reset-tokens", "1m30s"),
        ]);
        assert_eq!(
            retry_after_from_headers_at(&headers, 1_700_000_000_000),
            Some("90".into())
        );
    }

    #[test]
    fn rate_limit_reset_accepts_epoch_seconds_and_milliseconds() {
        for (header, value, expected) in [
            ("x-ratelimit-reset", "1700000090", "90"),
            ("x-ratelimit-reset-tokens", "1700000090250", "91"),
            ("x-ratelimit-reset-requests", "1699999990", "0"),
        ] {
            let headers = rate_headers(&[(header, value)]);
            assert_eq!(
                retry_after_from_headers_at(&headers, 1_700_000_000_000),
                Some(expected.into())
            );
        }
    }

    #[test]
    fn rate_limit_dates_use_server_date_to_avoid_clock_skew() {
        let headers = rate_headers(&[
            ("date", "Tue, 14 Nov 2023 22:13:20 GMT"),
            ("retry-after", "Tue, 14 Nov 2023 22:14:50 GMT"),
        ]);
        assert_eq!(
            retry_after_from_headers_at(&headers, 1_700_001_000_000),
            Some("90".into())
        );
        let headers = rate_headers(&[
            ("date", "Tue, 14 Nov 2023 22:13:20 GMT"),
            ("x-ratelimit-reset", "1700000090"),
        ]);
        assert_eq!(
            retry_after_from_headers_at(&headers, 1_700_001_000_000),
            Some("90".into())
        );
    }

    #[test]
    fn malformed_rate_limit_hints_are_ignored_without_overflow() {
        for value in [
            "",
            "tomorrow",
            "-1s",
            "NaN",
            "1s junk",
            "1m-2s",
            "0.0000000001s",
            "9999999999999999999999999999999999999999999999999999999999s",
        ] {
            let headers =
                rate_headers(&[("retry-after", value), ("x-ratelimit-reset-tokens", value)]);
            assert_eq!(
                retry_after_from_headers_at(&headers, 1_700_000_000_000),
                None,
                "{value}"
            );
        }
        let headers = rate_headers(&[
            ("retry-after", "bad date"),
            ("x-ratelimit-reset-tokens", "2s"),
        ]);
        assert_eq!(
            retry_after_from_headers_at(&headers, 1_700_000_000_000),
            Some("2".into())
        );
        let headers = rate_headers(&[("retry-after", "18446744073709551615")]);
        let value = retry_after_from_headers_at(&headers, 1_700_000_000_000)
            .unwrap()
            .parse::<u64>()
            .unwrap();
        assert!(value.checked_mul(1000).is_some());
        assert!(value > 60);
    }

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
