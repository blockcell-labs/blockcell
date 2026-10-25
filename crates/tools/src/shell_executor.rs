//! Shell 执行能力 seam：工具只描述"要执行什么"，由 [`ShellExecutor`] 决定在哪里执行。
//!
//! ```text
//! shell tool → ShellExecutor → SandboxPolicy → SandboxProvider → process
//! ```
//!
//! 本地实现 [`LocalShellExecutor`] 通过 [`SandboxProvider`] 启动进程；
//! 未来 Docker / Remote / SSH 只需提供新的 executor，工具逻辑保持不变。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use blockcell_core::Result;

use crate::sandbox::{
    prepare_sandboxed, NativeSandboxProvider, SandboxPolicy, SandboxProvider, SandboxRequest,
};

#[derive(Debug, Clone)]
pub struct ShellRequest {
    pub agent_id: String,
    pub session_id: String,
    pub command: String,
    pub working_dir: PathBuf,
    pub policy: SandboxPolicy,
    pub writable_roots: Vec<PathBuf>,
    pub timeout: Duration,
    /// stdout / stderr 各自保留的最大字节数。
    pub max_output_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct ShellResult {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub truncated: bool,
}

impl ShellResult {
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

#[async_trait]
pub trait ShellExecutor: Send + Sync {
    async fn execute(&self, request: ShellRequest) -> Result<ShellResult>;

    fn sandbox_provider(&self) -> Option<Arc<dyn SandboxProvider>> {
        None
    }
}

/// 在本机启动 shell，隔离由 [`SandboxProvider`] 负责。
pub struct LocalShellExecutor {
    sandbox: Arc<dyn SandboxProvider>,
}

impl LocalShellExecutor {
    pub fn new(sandbox: Arc<dyn SandboxProvider>) -> Self {
        Self { sandbox }
    }
}

impl Default for LocalShellExecutor {
    fn default() -> Self {
        Self::new(Arc::new(NativeSandboxProvider))
    }
}

#[async_trait]
impl ShellExecutor for LocalShellExecutor {
    async fn execute(&self, request: ShellRequest) -> Result<ShellResult> {
        #[cfg(windows)]
        let (program, args) = ("cmd", vec!["/C".to_string(), request.command]);
        #[cfg(not(windows))]
        let (program, args) = ("sh", vec!["-c".to_string(), request.command]);
        let mut handle = prepare_sandboxed(
            self.sandbox.as_ref(),
            SandboxRequest {
                agent_id: request.agent_id,
                session_id: request.session_id,
                policy: request.policy,
                writable_roots: request.writable_roots,
                program: program.to_string(),
                args,
            },
        )
        .await?;
        handle.command.current_dir(&request.working_dir);
        let output = crate::process::run_command_bounded(
            handle.command,
            request.timeout,
            request.max_output_bytes,
        )
        .await?;
        Ok(ShellResult {
            exit_code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
            truncated: output.truncated,
        })
    }

    fn sandbox_provider(&self) -> Option<Arc<dyn SandboxProvider>> {
        Some(Arc::clone(&self.sandbox))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(command: &str, working_dir: PathBuf) -> ShellRequest {
        ShellRequest {
            agent_id: "default".into(),
            session_id: "cli:test".into(),
            command: command.into(),
            working_dir,
            policy: SandboxPolicy::FullAccess,
            writable_roots: Vec::new(),
            timeout: Duration::from_secs(5),
            max_output_bytes: 1024,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_executor_runs_in_working_dir_and_reports_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let result = LocalShellExecutor::default()
            .execute(request("pwd; exit 3", dir.path().to_path_buf()))
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(3));
        assert!(!result.success());
        let stdout = String::from_utf8_lossy(&result.stdout);
        let expected = dir.path().canonicalize().unwrap();
        assert!(
            stdout.contains(expected.file_name().unwrap().to_str().unwrap()),
            "{stdout}"
        );
    }
}
