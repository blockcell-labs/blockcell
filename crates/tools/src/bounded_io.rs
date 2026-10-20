use blockcell_core::{Error, Result};
use std::path::Path;
use tokio::io::AsyncWriteExt;

pub(crate) fn append_limited(output: &mut Vec<u8>, chunk: &[u8], limit: usize) -> bool {
    let remaining = limit.saturating_sub(output.len());
    let take = remaining.min(chunk.len());
    output.extend_from_slice(&chunk[..take]);
    // Filling the limit exactly is valid; only bytes that cannot be retained
    // prove that the response exceeds the bound.
    take < chunk.len()
}

pub(crate) async fn read_response_limited(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<(Vec<u8>, bool)> {
    let mut output = Vec::with_capacity(limit.min(64 * 1024));
    let mut truncated = false;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| Error::Tool(format!("Failed to read response body: {e}")))?
    {
        if append_limited(&mut output, &chunk, limit) {
            truncated = true;
            break;
        }
    }
    Ok((output, truncated))
}

pub(crate) async fn read_json_limited(
    response: reqwest::Response,
    limit: usize,
) -> Result<serde_json::Value> {
    let (bytes, truncated) = read_response_limited(response, limit).await?;
    if truncated {
        return Err(Error::Tool(format!(
            "JSON response exceeded the {limit} byte limit"
        )));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::Tool(format!("Failed to parse response: {e}")))
}

pub(crate) async fn read_text_limited(response: reqwest::Response, limit: usize) -> Result<String> {
    let (bytes, _truncated) = read_response_limited(response, limit).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) async fn stream_response_to_file(
    mut response: reqwest::Response,
    path: &Path,
    limit: usize,
) -> Result<(usize, bool)> {
    let mut file = tokio::fs::File::create(path).await?;
    let mut written = 0usize;
    let mut truncated = false;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| Error::Tool(format!("Failed to read response body: {e}")))?
    {
        let remaining = limit.saturating_sub(written);
        let take = remaining.min(chunk.len());
        file.write_all(&chunk[..take]).await?;
        written += take;
        if take < chunk.len() {
            truncated = true;
            break;
        }
    }
    file.flush().await?;
    Ok((written, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn p3_json_response_accepts_exact_limit() {
        let body = r#"{"ok":true}"#;
        let response = tokio_tungstenite::tungstenite::http::Response::builder()
            .body(body.to_string())
            .unwrap();
        assert_eq!(
            read_json_limited(reqwest::Response::from(response), body.len())
                .await
                .unwrap(),
            serde_json::json!({"ok":true})
        );
    }

    #[tokio::test]
    async fn p3_file_response_accepts_exact_limit() {
        let dir = tempfile::tempdir().unwrap();
        let response = tokio_tungstenite::tungstenite::http::Response::builder()
            .body("abcd".to_string())
            .unwrap();
        assert_eq!(
            stream_response_to_file(
                reqwest::Response::from(response),
                &dir.path().join("body"),
                4
            )
            .await
            .unwrap(),
            (4, false)
        );
    }

    #[tokio::test]
    async fn p3_json_response_rejects_oversized_body() {
        let response = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(200)
            .body(String::from(r#"{"text":"ok"}"#))
            .unwrap();
        let response = reqwest::Response::from(response);
        let error = read_json_limited(response, 8).await.unwrap_err();
        assert!(error.to_string().contains("limit"), "{error}");
    }

    #[test]
    fn append_limited_stops_at_byte_limit() {
        let mut output = Vec::new();
        assert!(!append_limited(&mut output, b"abcd", 6));
        assert!(append_limited(&mut output, b"efgh", 6));
        assert_eq!(output, b"abcdef");
    }
}
