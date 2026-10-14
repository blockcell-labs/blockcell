use async_trait::async_trait;
use blockcell_core::{Error, Result};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;

pub use crate::sandbox::SandboxPolicy;
use crate::sandbox::{prepare_sandboxed, NativeSandboxProvider, SandboxProvider, SandboxRequest};
use crate::{Tool, ToolContext, ToolSchema};

/// 发起 shell 会话的 agent / 会话，随沙箱请求交给 provider。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SessionOwner<'a> {
    pub agent_id: &'a str,
    pub session_id: &'a str,
}

const MAX_SESSION_OUTPUT_BYTES: usize = 1024 * 1024;

pub struct ShellTool;

static SHELL_SESSIONS: Lazy<ShellSessionManager> = Lazy::new(ShellSessionManager::new);

#[derive(Debug, Clone)]
pub(crate) struct ShellCommandOutput {
    pub output: String,
    pub exit_code: Option<i32>,
    pub running: bool,
    pub truncated: bool,
}

struct SharedOutput {
    bytes: VecDeque<u8>,
    start_offset: u64,
    end_offset: u64,
    truncated: bool,
}

impl SharedOutput {
    fn new() -> Self {
        Self {
            bytes: VecDeque::new(),
            start_offset: 0,
            end_offset: 0,
            truncated: false,
        }
    }

    fn append(&mut self, chunk: &[u8]) {
        self.bytes.extend(chunk.iter().copied());
        self.end_offset = self.end_offset.saturating_add(chunk.len() as u64);
        while self.bytes.len() > MAX_SESSION_OUTPUT_BYTES {
            self.bytes.pop_front();
            self.start_offset = self.start_offset.saturating_add(1);
            self.truncated = true;
        }
    }
}

struct ActiveCommand {
    marker: String,
    delivered_offset: u64,
}

struct ShellSession {
    child: Child,
    stdin: ChildStdin,
    #[cfg(unix)]
    command_input: tokio::fs::File,
    output: Arc<Mutex<SharedOutput>>,
    active: Option<ActiveCommand>,
    sandbox_policy: SandboxPolicy,
    pid: Option<u32>,
}

#[derive(Clone)]
pub(crate) struct ShellSessionManager {
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<ShellSession>>>>>,
    sandbox: Arc<dyn SandboxProvider>,
}

impl ShellSessionManager {
    pub(crate) fn new() -> Self {
        Self::with_sandbox(Arc::new(NativeSandboxProvider))
    }

    pub(crate) fn with_sandbox(sandbox: Arc<dyn SandboxProvider>) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            sandbox,
        }
    }

    pub(crate) async fn create(
        &self,
        workspace: &Path,
        sandbox_policy: SandboxPolicy,
        owner: SessionOwner<'_>,
    ) -> Result<String> {
        let request = SandboxRequest {
            agent_id: owner.agent_id.to_string(),
            session_id: owner.session_id.to_string(),
            policy: sandbox_policy,
            writable_roots: vec![workspace.to_path_buf()],
            program: String::new(),
            args: Vec::new(),
        };
        #[cfg(unix)]
        let (mut command, command_input, child_control) =
            build_interactive_shell(self.sandbox.as_ref(), request).await?;
        #[cfg(windows)]
        let mut command = build_interactive_shell(self.sandbox.as_ref(), request).await?;
        command
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command
            .spawn()
            .map_err(|error| Error::Tool(format!("Failed to start shell session: {error}")))?;
        #[cfg(unix)]
        drop(child_control);
        let pid = child.id();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Tool("Shell stdin pipe unavailable".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Tool("Shell stdout pipe unavailable".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Tool("Shell stderr pipe unavailable".to_string()))?;
        let output = Arc::new(Mutex::new(SharedOutput::new()));
        tokio::spawn(drain_shell_stream(stdout, Arc::clone(&output)));
        tokio::spawn(drain_shell_stream(stderr, Arc::clone(&output)));

        let session_id = uuid::Uuid::new_v4().simple().to_string();
        self.sessions.lock().await.insert(
            session_id.clone(),
            Arc::new(Mutex::new(ShellSession {
                child,
                stdin,
                #[cfg(unix)]
                command_input,
                output,
                active: None,
                sandbox_policy,
                pid,
            })),
        );
        Ok(session_id)
    }

    async fn get(&self, session_id: &str) -> Result<Arc<Mutex<ShellSession>>> {
        self.sessions
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("Shell session not found: {session_id}")))
    }

    pub(crate) async fn run_command(
        &self,
        session_id: &str,
        command: &str,
        timeout: Duration,
    ) -> Result<ShellCommandOutput> {
        let session = self.get(session_id).await?;
        {
            let mut session = session.lock().await;
            if session.active.is_some() {
                return Err(Error::Validation(format!(
                    "Shell session {session_id} already has a running command; poll it or write stdin first"
                )));
            }
            let marker = format!("__BLOCKCELL_SHELL_DONE_{}__", uuid::Uuid::new_v4().simple());
            let delivered_offset = session.output.lock().await.end_offset;
            let payload = build_command_payload(command, &marker);
            #[cfg(unix)]
            let command_writer = &mut session.command_input;
            #[cfg(windows)]
            let command_writer = &mut session.stdin;
            command_writer
                .write_all(payload.as_bytes())
                .await
                .map_err(|error| Error::Tool(format!("Failed to write shell command: {error}")))?;
            command_writer
                .flush()
                .await
                .map_err(|error| Error::Tool(format!("Failed to flush shell command: {error}")))?;
            session.active = Some(ActiveCommand {
                marker,
                delivered_offset,
            });
        }
        self.poll(session_id, timeout).await
    }

    pub(crate) async fn poll(
        &self,
        session_id: &str,
        timeout: Duration,
    ) -> Result<ShellCommandOutput> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let output = self.poll_once(session_id).await?;
            if !output.running || tokio::time::Instant::now() >= deadline {
                return Ok(output);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn poll_once(&self, session_id: &str) -> Result<ShellCommandOutput> {
        let session = self.get(session_id).await?;
        let mut session = session.lock().await;
        let Some(active) = session.active.as_ref() else {
            let exit_code = session
                .child
                .try_wait()
                .map_err(blockcell_core::Error::Io)?
                .and_then(|status| status.code());
            return Ok(ShellCommandOutput {
                output: String::new(),
                exit_code,
                running: false,
                truncated: false,
            });
        };
        let marker = active.marker.clone();
        let delivered_offset = active.delivered_offset;
        let mut shared = session.output.lock().await;
        let bytes: Vec<u8> = shared.bytes.iter().copied().collect();
        let marker_prefix = format!("{marker}:");
        let text = String::from_utf8_lossy(&bytes);
        let marker_position = text.find(&marker_prefix);
        let available_start = delivered_offset.max(shared.start_offset);
        let relative_start = available_start.saturating_sub(shared.start_offset) as usize;

        if let Some(marker_start) = marker_position {
            let status_start = marker_start + marker_prefix.len();
            if let Some(line_end) = text[status_start..].find(['\r', '\n']) {
                let status_text = &text[status_start..status_start + line_end];
                let exit_code = status_text.trim().parse::<i32>().ok();
                let output =
                    String::from_utf8_lossy(&bytes[relative_start.min(marker_start)..marker_start])
                        .trim_start_matches(['\r', '\n'])
                        .to_string();
                let truncated = shared.truncated || delivered_offset < shared.start_offset;
                shared.bytes.clear();
                shared.start_offset = shared.end_offset;
                shared.truncated = false;
                drop(shared);
                session.active = None;
                return Ok(ShellCommandOutput {
                    output,
                    exit_code,
                    running: false,
                    truncated,
                });
            }
        }

        let output = if relative_start < bytes.len() {
            String::from_utf8_lossy(&bytes[relative_start..]).to_string()
        } else {
            String::new()
        };
        let truncated = shared.truncated || delivered_offset < shared.start_offset;
        let end_offset = shared.end_offset;
        drop(shared);
        if let Some(active) = session.active.as_mut() {
            active.delivered_offset = end_offset;
        }

        if let Some(status) = session
            .child
            .try_wait()
            .map_err(blockcell_core::Error::Io)?
        {
            session.active = None;
            session.pid = None;
            return Ok(ShellCommandOutput {
                output,
                exit_code: status.code(),
                running: false,
                truncated,
            });
        }

        Ok(ShellCommandOutput {
            output,
            exit_code: None,
            running: true,
            truncated,
        })
    }

    pub(crate) async fn write_stdin(&self, session_id: &str, input: &str) -> Result<()> {
        let session = self.get(session_id).await?;
        let mut session = session.lock().await;
        session
            .stdin
            .write_all(input.as_bytes())
            .await
            .map_err(|error| Error::Tool(format!("Failed to write shell stdin: {error}")))?;
        session
            .stdin
            .flush()
            .await
            .map_err(|error| Error::Tool(format!("Failed to flush shell stdin: {error}")))
    }

    pub(crate) async fn close(&self, session_id: &str) -> Result<()> {
        let session = self
            .sessions
            .lock()
            .await
            .remove(session_id)
            .ok_or_else(|| Error::NotFound(format!("Shell session not found: {session_id}")))?;
        let mut session = session.lock().await;
        terminate_process_group(session.pid);
        let _ = session.child.start_kill();
        let _ = session.child.wait().await;
        Ok(())
    }

    pub(crate) async fn session_metadata(
        &self,
        session_id: &str,
    ) -> Result<(Option<u32>, SandboxPolicy)> {
        let session = self.get(session_id).await?;
        let mut session = session.lock().await;
        if session
            .child
            .try_wait()
            .map_err(blockcell_core::Error::Io)?
            .is_some()
        {
            session.pid = None;
        }
        Ok((session.pid, session.sandbox_policy))
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn execution_environment(&self, params: &Value) -> crate::ExecutionEnvironment {
        if crate::execution_env::requests_full_access("shell", params) {
            crate::ExecutionEnvironment::FullAccess
        } else {
            crate::ExecutionEnvironment::Shell
        }
    }

    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Required
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "shell".to_string(),
            description: "Run commands in a persistent shell session. Reuse session_id to preserve cwd, environment variables, and activated virtual environments. Timed-out commands keep running and can be polled or sent stdin.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["run", "poll", "write_stdin", "close"],
                        "description": "Action to perform. Defaults to run."
                    },
                    "command": {
                        "type": "string",
                        "description": "Command to run. Required for action=run."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Existing persistent shell session. Omit on the first run to create one."
                    },
                    "input": {
                        "type": "string",
                        "description": "Characters to send for action=write_stdin."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 3600,
                        "description": "How long to wait for new output. A timeout returns running=true and does not kill the process."
                    },
                    "sandbox_policy": {
                        "type": "string",
                        "enum": ["read-only", "workspace-write", "full-access"],
                        "description": "Sandbox policy fixed when creating a session. read-only/workspace-write use native OS isolation; unavailable platforms require approval before full-access execution."
                    }
                }
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("run");
        SandboxPolicy::parse(params.get("sandbox_policy").and_then(Value::as_str))?;
        match action {
            "run" => {
                let command = params
                    .get("command")
                    .and_then(Value::as_str)
                    .filter(|command| !command.trim().is_empty())
                    .ok_or_else(|| Error::Validation("action=run requires command".to_string()))?;
                if crate::exec::is_dangerous_command(command) {
                    return Err(Error::PermissionDenied(
                        "Command matches dangerous pattern and is blocked".to_string(),
                    ));
                }
            }
            "poll" | "close" => require_session_id(params, action)?,
            "write_stdin" => {
                require_session_id(params, action)?;
                if params.get("input").and_then(Value::as_str).is_none() {
                    return Err(Error::Validation(
                        "action=write_stdin requires input".to_string(),
                    ));
                }
            }
            other => return Err(Error::Validation(format!("Unknown shell action: {other}"))),
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("run");
        let requested_timeout = params.get("timeout_secs").and_then(Value::as_u64);
        let timeout = Duration::from_secs(
            requested_timeout
                .unwrap_or(ctx.config.tools.exec.timeout as u64)
                .min(3600),
        );

        match action {
            "run" => {
                let command = params["command"].as_str().unwrap_or_default();
                let session_id = if let Some(session_id) =
                    params.get("session_id").and_then(Value::as_str)
                {
                    if params.get("sandbox_policy").is_some() {
                        let requested = SandboxPolicy::parse(
                            params.get("sandbox_policy").and_then(Value::as_str),
                        )?;
                        let (_, existing) = SHELL_SESSIONS.session_metadata(session_id).await?;
                        if requested != existing {
                            return Err(Error::Validation(format!(
                                "Shell session {session_id} already uses sandbox_policy={} and cannot be changed",
                                existing.as_str()
                            )));
                        }
                    }
                    session_id.to_string()
                } else {
                    let policy =
                        SandboxPolicy::parse(params.get("sandbox_policy").and_then(Value::as_str))?;
                    let agent_id = ctx
                        .agent_identity
                        .as_ref()
                        .map(|identity| identity.agent_id.as_str())
                        .unwrap_or_default();
                    let session_id = SHELL_SESSIONS
                        .create(
                            &ctx.workspace,
                            policy,
                            SessionOwner {
                                agent_id,
                                session_id: &ctx.session_key,
                            },
                        )
                        .await?;
                    if let (Some(task_manager), (Some(pid), _)) = (
                        ctx.task_manager.as_ref(),
                        SHELL_SESSIONS.session_metadata(&session_id).await?,
                    ) {
                        task_manager.register_shell_process(&session_id, pid);
                    }
                    session_id
                };
                let output = SHELL_SESSIONS
                    .run_command(&session_id, command, timeout)
                    .await?;
                let (_, policy) = SHELL_SESSIONS.session_metadata(&session_id).await?;
                Ok(shell_output_json(&session_id, policy, output))
            }
            "poll" => {
                let session_id = params["session_id"].as_str().unwrap_or_default();
                let output = SHELL_SESSIONS.poll(session_id, timeout).await?;
                let (_, policy) = SHELL_SESSIONS.session_metadata(session_id).await?;
                Ok(shell_output_json(session_id, policy, output))
            }
            "write_stdin" => {
                let session_id = params["session_id"].as_str().unwrap_or_default();
                let input = params["input"].as_str().unwrap_or_default();
                SHELL_SESSIONS.write_stdin(session_id, input).await?;
                let output = SHELL_SESSIONS.poll(session_id, timeout).await?;
                let (_, policy) = SHELL_SESSIONS.session_metadata(session_id).await?;
                Ok(shell_output_json(session_id, policy, output))
            }
            "close" => {
                let session_id = params["session_id"].as_str().unwrap_or_default();
                SHELL_SESSIONS.close(session_id).await?;
                if let Some(task_manager) = ctx.task_manager.as_ref() {
                    task_manager.unregister_shell_process(session_id);
                }
                Ok(json!({"session_id": session_id, "closed": true}))
            }
            _ => unreachable!("validated action"),
        }
    }
}

fn require_session_id(params: &Value, action: &str) -> Result<()> {
    params
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|session_id| !session_id.trim().is_empty())
        .map(|_| ())
        .ok_or_else(|| Error::Validation(format!("action={action} requires session_id")))
}

fn shell_output_json(session_id: &str, policy: SandboxPolicy, output: ShellCommandOutput) -> Value {
    json!({
        "session_id": session_id,
        "sandbox_policy": policy.as_str(),
        "output": output.output,
        "exit_code": output.exit_code,
        "running": output.running,
        "truncated": output.truncated,
    })
}

async fn drain_shell_stream<R>(mut reader: R, output: Arc<Mutex<SharedOutput>>)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => output.lock().await.append(&buffer[..read]),
        }
    }
}

#[cfg(unix)]
async fn build_interactive_shell(
    sandbox: &dyn SandboxProvider,
    mut request: SandboxRequest,
) -> Result<(Command, tokio::fs::File, std::fs::File)> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let mut pipe_fds = [-1; 2];
    // SAFETY: pipe initializes both entries on success; ownership is immediately
    // transferred to File values below.
    if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } == -1 {
        return Err(blockcell_core::Error::Io(std::io::Error::last_os_error()));
    }
    // SAFETY: pipe succeeded, so both descriptors are valid and uniquely owned.
    let child_control = unsafe { std::fs::File::from_raw_fd(pipe_fds[0]) };
    // SAFETY: pipe succeeded, so both descriptors are valid and uniquely owned.
    let parent_control = unsafe { std::fs::File::from_raw_fd(pipe_fds[1]) };
    for fd in [child_control.as_raw_fd(), parent_control.as_raw_fd()] {
        // SAFETY: fcntl only reads or updates descriptor flags for a valid fd.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1
        {
            return Err(blockcell_core::Error::Io(std::io::Error::last_os_error()));
        }
    }
    let command_input = tokio::fs::File::from_std(parent_control);
    let control_fd = child_control.as_raw_fd();
    request.program = "bash".to_string();
    request.args = ["--noprofile", "--norc", "/dev/fd/3"]
        .map(String::from)
        .to_vec();
    let mut command = prepare_sandboxed(sandbox, request).await?.command;
    // SAFETY: dup2 only remaps the inherited command channel in the child just
    // before exec; no allocation or shared-state access occurs in the closure.
    unsafe {
        command.pre_exec(move || {
            if control_fd != 3 && libc::dup2(control_fd, 3) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let flags = libc::fcntl(3, libc::F_GETFD);
            if flags == -1 || libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok((command, command_input, child_control))
}

#[cfg(windows)]
async fn build_interactive_shell(
    sandbox: &dyn SandboxProvider,
    mut request: SandboxRequest,
) -> Result<Command> {
    request.program = "cmd.exe".to_string();
    request.args = vec!["/Q".to_string()];
    Ok(prepare_sandboxed(sandbox, request).await?.command)
}

fn build_command_payload(command: &str, marker: &str) -> String {
    #[cfg(windows)]
    {
        format!("{command}\r\necho {marker}:%errorlevel%\r\n")
    }
    #[cfg(not(windows))]
    {
        format!(
            "{command}\n__blockcell_status=$?\nprintf '\\n{marker}:%s\\n' \"$__blockcell_status\"\n"
        )
    }
}

fn terminate_process_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        // SAFETY: the shell was created in its own process group.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn shell_tool_schema_exposes_session_actions_and_sandbox_policy() {
        let schema = ShellTool.schema();

        assert_eq!(schema.name, "shell");
        assert_eq!(
            schema.parameters["properties"]["action"]["enum"],
            serde_json::json!(["run", "poll", "write_stdin", "close"])
        );
        assert_eq!(
            schema.parameters["properties"]["sandbox_policy"]["enum"],
            serde_json::json!(["read-only", "workspace-write", "full-access"])
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn interactive_shell_uses_pipe_for_command_channel() {
        use std::os::fd::AsRawFd;

        let workspace = tempfile::tempdir().expect("temp workspace");
        let request = SandboxRequest {
            agent_id: String::new(),
            session_id: String::new(),
            policy: SandboxPolicy::FullAccess,
            writable_roots: vec![workspace.path().to_path_buf()],
            program: String::new(),
            args: Vec::new(),
        };
        let (_command, _parent_writer, child_reader) =
            build_interactive_shell(&NativeSandboxProvider, request)
                .await
                .expect("build interactive shell");
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();

        // SAFETY: fstat initializes the provided stat buffer for a valid owned fd.
        let result = unsafe { libc::fstat(child_reader.as_raw_fd(), stat.as_mut_ptr()) };
        assert_eq!(result, 0, "fstat command channel");
        // SAFETY: fstat succeeded, so the stat structure is initialized.
        let stat = unsafe { stat.assume_init() };
        assert_eq!(stat.st_mode & libc::S_IFMT, libc::S_IFIFO);
    }

    #[tokio::test]
    async fn shell_session_preserves_working_directory_between_commands() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let manager = ShellSessionManager::new();
        let session_id = manager
            .create(
                workspace.path(),
                SandboxPolicy::FullAccess,
                SessionOwner::default(),
            )
            .await
            .expect("create shell session");

        let first = manager
            .run_command(
                &session_id,
                "mkdir nested && cd nested",
                Duration::from_secs(2),
            )
            .await
            .expect("change directory");
        let second = manager
            .run_command(&session_id, "pwd", Duration::from_secs(2))
            .await
            .expect("print working directory");

        assert!(!first.running);
        assert!(second.output.contains("nested"));
        manager.close(&session_id).await.expect("close session");
    }

    #[tokio::test]
    async fn shell_timeout_keeps_process_running_for_later_poll() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let manager = ShellSessionManager::new();
        let session_id = manager
            .create(
                workspace.path(),
                SandboxPolicy::FullAccess,
                SessionOwner::default(),
            )
            .await
            .expect("create shell session");

        let timed_out = manager
            .run_command(
                &session_id,
                "sleep 0.15; echo finished",
                Duration::from_millis(20),
            )
            .await
            .expect("start long command");
        assert!(timed_out.running);

        tokio::time::sleep(Duration::from_millis(200)).await;
        let completed = manager
            .poll(&session_id, Duration::from_millis(50))
            .await
            .expect("poll command");
        assert!(!completed.running);
        assert!(completed.output.contains("finished"));
        manager.close(&session_id).await.expect("close session");
    }

    #[tokio::test]
    async fn shell_session_accepts_interactive_stdin() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let manager = ShellSessionManager::new();
        let session_id = manager
            .create(
                workspace.path(),
                SandboxPolicy::FullAccess,
                SessionOwner::default(),
            )
            .await
            .expect("create shell session");

        let waiting = manager
            .run_command(
                &session_id,
                "read answer; echo answer:$answer",
                Duration::from_millis(20),
            )
            .await
            .expect("start interactive command");
        assert!(waiting.running);
        manager
            .write_stdin(&session_id, "hello\n")
            .await
            .expect("write stdin");
        let completed = manager
            .poll(&session_id, Duration::from_secs(1))
            .await
            .expect("poll command");

        assert!(!completed.running);
        assert!(completed.output.contains("answer:hello"));
        manager.close(&session_id).await.expect("close session");
    }

    #[tokio::test]
    async fn exited_shell_is_not_reported_as_a_live_registered_process() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let manager = ShellSessionManager::new();
        let session_id = manager
            .create(
                workspace.path(),
                SandboxPolicy::FullAccess,
                SessionOwner::default(),
            )
            .await
            .expect("create shell session");

        let completed = manager
            .run_command(&session_id, "exit 7", Duration::from_secs(1))
            .await
            .expect("exit shell");
        let (pid, _) = manager
            .session_metadata(&session_id)
            .await
            .expect("session metadata");

        assert!(!completed.running);
        assert_eq!(completed.exit_code, Some(7));
        assert_eq!(pid, None);
    }

    #[tokio::test]
    async fn unavailable_native_sandbox_requests_approval_instead_of_silent_bypass() {
        if crate::sandbox::native_backend(SandboxPolicy::WorkspaceWrite)
            != crate::sandbox::SandboxBackend::ApprovalRequired
        {
            return;
        }
        let workspace = tempfile::tempdir().expect("temp workspace");
        let manager = ShellSessionManager::new();

        let error = manager
            .create(
                workspace.path(),
                SandboxPolicy::WorkspaceWrite,
                SessionOwner::default(),
            )
            .await
            .expect_err("unavailable sandbox must not silently run unrestricted");

        assert!(error.to_string().contains("approval required"));
    }
}
