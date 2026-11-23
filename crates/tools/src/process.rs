use blockcell_core::{Error, Result};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

pub(crate) struct BoundedOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}

async fn drain_limited<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        let take = remaining.min(read);
        output.extend_from_slice(&buffer[..take]);
        if take < read {
            truncated = true;
        }
    }
    Ok((output, truncated))
}

pub(crate) async fn run_command_bounded(
    mut command: Command,
    timeout_duration: Duration,
    max_output_bytes: usize,
) -> Result<BoundedOutput> {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command
        .spawn()
        .map_err(|e| Error::Tool(format!("Failed to execute command: {e}")))?;
    #[cfg(unix)]
    let pid = child.id();
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Tool("Command stdout pipe unavailable".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::Tool("Command stderr pipe unavailable".to_string()))?;
    let stdout_task = tokio::spawn(drain_limited(stdout, max_output_bytes));
    let stderr_task = tokio::spawn(drain_limited(stderr, max_output_bytes));
    let started = tokio::time::Instant::now();

    let status = match tokio::time::timeout(timeout_duration, child.wait()).await {
        Ok(status) => status.map_err(blockcell_core::Error::Io)?,
        Err(_) => {
            #[cfg(unix)]
            if let Some(pid) = pid {
                // SAFETY: negative PID targets only the process group created above.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(Error::Timeout(format!(
                "Command timed out after {} seconds",
                timeout_duration.as_secs_f64()
            )));
        }
    };

    let remaining = timeout_duration.saturating_sub(started.elapsed());
    let read_output = async {
        let (stdout, stdout_truncated) = stdout_task
            .await
            .map_err(|e| Error::Tool(format!("stdout reader failed: {e}")))?
            .map_err(blockcell_core::Error::Io)?;
        let (stderr, stderr_truncated) = stderr_task
            .await
            .map_err(|e| Error::Tool(format!("stderr reader failed: {e}")))?
            .map_err(blockcell_core::Error::Io)?;
        Ok::<_, blockcell_core::Error>(BoundedOutput {
            status,
            stdout,
            stderr,
            truncated: stdout_truncated || stderr_truncated,
        })
    };

    match tokio::time::timeout(remaining, read_output).await {
        Ok(result) => result,
        Err(_) => {
            #[cfg(unix)]
            if let Some(pid) = pid {
                // SAFETY: negative PID targets only the process group created above.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            Err(Error::Timeout(format!(
                "Command output pipes did not close after {} seconds",
                timeout_duration.as_secs_f64()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_background_process_group() {
        let marker =
            std::env::temp_dir().join(format!("blockcell-process-marker-{}", uuid::Uuid::new_v4()));
        let script = format!("(sleep 1; echo leaked > '{}') & wait", marker.display());
        let mut command = tokio::process::Command::new("sh");
        command.arg("-c").arg(script);

        let result =
            run_command_bounded(command, std::time::Duration::from_millis(100), 1024).await;
        assert!(result.is_err());
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
        assert!(!marker.exists());
    }
}
