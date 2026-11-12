use blockcell_core::{Error, Result};
use std::path::{Path, PathBuf};
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

struct PendingDownload {
    path: PathBuf,
    committed: bool,
}

impl Drop for PendingDownload {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(crate) async fn stream_response_to_file(
    mut response: reqwest::Response,
    path: &Path,
    limit: usize,
) -> Result<(usize, bool)> {
    let temp_path = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".blockcell-download-{}.tmp", uuid::Uuid::new_v4()));
    // Create synchronously so cancellation cannot leave an unowned file from a
    // still-running async open. create_new prevents concurrent downloads from
    // sharing a staging file; staging beside the destination permits rename.
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)?;
    let mut pending = PendingDownload {
        path: temp_path,
        committed: false,
    };
    let mut file = tokio::fs::File::from_std(file);
    let result = async {
        let mut written = 0usize;
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
                return Ok((written, true));
            }
        }
        file.flush().await?;
        file.sync_all().await?;
        Ok::<_, Error>((written, false))
    }
    .await;

    // Wait for pending writes and close the handle before cleanup or replacement.
    drop(file.into_std().await);
    let (written, truncated) = result?;
    if !truncated {
        // Keep commit synchronous: cancellation cannot run cleanup while an
        // offloaded rename might still replace the destination later.
        std::fs::rename(&pending.path, path)?;
        pending.committed = true;
    }
    Ok((written, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streamed_response(chunks: Vec<std::io::Result<Vec<u8>>>) -> reqwest::Response {
        let body = reqwest::Body::wrap_stream(futures::stream::iter(chunks));
        reqwest::Response::from(
            tokio_tungstenite::tungstenite::http::Response::builder()
                .body(body)
                .unwrap(),
        )
    }

    fn assert_directory_entries(dir: &Path, expected: &[&str]) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        let mut expected = expected.to_vec();
        expected.sort();
        assert_eq!(entries, expected, "download must not leave temporary files");
    }

    #[tokio::test]
    async fn failed_download_preserves_existing_file_on_body_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body");
        std::fs::write(&path, b"previous complete file").unwrap();
        let response = streamed_response(vec![
            Ok(b"partial replacement".to_vec()),
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "interrupted response",
            )),
        ]);

        let error = stream_response_to_file(response, &path, 64)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("Failed to read response body"));
        assert_eq!(std::fs::read(&path).unwrap(), b"previous complete file");
        assert_directory_entries(dir.path(), &["body"]);
    }

    #[tokio::test]
    async fn failed_download_does_not_leave_new_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body");
        let response = streamed_response(vec![
            Ok(b"partial".to_vec()),
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "response timed out",
            )),
        ]);

        assert!(stream_response_to_file(response, &path, 64).await.is_err());

        assert!(!path.exists());
        assert_directory_entries(dir.path(), &[]);
    }

    #[tokio::test]
    async fn failed_download_preserves_existing_file_when_oversized() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body");
        std::fs::write(&path, b"previous complete file").unwrap();
        let response = streamed_response(vec![Ok(b"abcd".to_vec()), Ok(b"e".to_vec())]);

        assert_eq!(
            stream_response_to_file(response, &path, 4).await.unwrap(),
            (4, true)
        );

        assert_eq!(std::fs::read(&path).unwrap(), b"previous complete file");
        assert_directory_entries(dir.path(), &["body"]);
    }

    #[tokio::test]
    async fn failed_download_does_not_leave_new_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body");
        let response = streamed_response(vec![Ok(b"abcde".to_vec())]);

        assert_eq!(
            stream_response_to_file(response, &path, 4).await.unwrap(),
            (4, true)
        );

        assert!(!path.exists());
        assert_directory_entries(dir.path(), &[]);
    }

    #[tokio::test]
    async fn successful_download_replaces_existing_file_at_exact_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body");
        std::fs::write(&path, b"previous complete file").unwrap();
        let response = streamed_response(vec![Ok(b"ab".to_vec()), Ok(b"cd".to_vec())]);

        assert_eq!(
            stream_response_to_file(response, &path, 4).await.unwrap(),
            (4, false)
        );

        assert_eq!(std::fs::read(&path).unwrap(), b"abcd");
        assert_directory_entries(dir.path(), &["body"]);
    }

    #[tokio::test]
    async fn successful_download_keeps_non_success_http_status_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body");
        let response = reqwest::Response::from(
            tokio_tungstenite::tungstenite::http::Response::builder()
                .status(404)
                .body("not found".to_string())
                .unwrap(),
        );

        assert_eq!(
            stream_response_to_file(response, &path, 64).await.unwrap(),
            (9, false)
        );

        assert_eq!(std::fs::read(&path).unwrap(), b"not found");
        assert_directory_entries(dir.path(), &["body"]);
    }

    #[tokio::test]
    async fn failed_download_preserves_destination_on_commit_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body");
        std::fs::write(&path, b"previous complete file").unwrap();
        let destination = path.clone();
        let body = reqwest::Body::wrap_stream(futures::stream::once(async move {
            std::fs::remove_file(&destination).unwrap();
            std::fs::create_dir(&destination).unwrap();
            std::fs::write(destination.join("sentinel"), b"keep directory").unwrap();
            Ok::<_, std::io::Error>(b"replacement".to_vec())
        }));
        let response = reqwest::Response::from(
            tokio_tungstenite::tungstenite::http::Response::builder()
                .body(body)
                .unwrap(),
        );

        assert!(stream_response_to_file(response, &path, 64).await.is_err());

        assert_eq!(
            std::fs::read(path.join("sentinel")).unwrap(),
            b"keep directory"
        );
        assert_directory_entries(dir.path(), &["body"]);
    }

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
