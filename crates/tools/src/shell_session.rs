use async_trait::async_trait;
use blockcell_core::{Error, Result};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
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
    exit_code: Option<i32>,
    exited_at: Option<tokio::time::Instant>,
    task_manager: Option<crate::TaskManagerHandle>,
    owner_agent_id: String,
    owner_session_id: String,
}

const MAX_STDIN_BUFFER_BYTES: usize = 4096;
const EXITED_SESSION_RETENTION: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub(crate) struct ShellSessionManager {
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<ShellSession>>>>>,
    /// 会话创建时固定的沙箱策略的**同步**快照。会话策略创建后不可更改，复用会话
    /// （`action=run` + `session_id` 且不带 `sandbox_policy`）时无法从参数看出其真实
    /// 环境，这里保存一份同步索引，供 [`crate::execution_env::requests_full_access`] 在
    /// 同步的工具声明上下文中判定。
    policies: Arc<std::sync::RwLock<HashMap<String, SandboxPolicy>>>,
    /// 各会话通过 `write_stdin` 分片注入的滑动输入窗口，用于跨多次调用识别拆分的危险命令或路径。
    stdin_buffers: Arc<std::sync::RwLock<HashMap<String, String>>>,
    /// 各会话跟踪到的逻辑 cwd：Some(path) = 已知（创建时为 workspace，之后按
    /// 命令中的 cd/pushd 更新）；None = 未知（cd 目标无法静态解析或内部进入
    /// 子 shell）。shell 真实 cwd 属进程内部状态无法直接观测，只能尽力追踪；
    /// 未知状态下路径检查必须 fail-closed。
    cwds: Arc<std::sync::RwLock<HashMap<String, Option<PathBuf>>>>,
    reaper_running: Arc<std::sync::atomic::AtomicBool>,
    sandbox: Arc<std::sync::RwLock<Arc<dyn SandboxProvider>>>,
}

impl ShellSessionManager {
    pub(crate) fn new() -> Self {
        Self::with_sandbox(Arc::new(NativeSandboxProvider))
    }

    pub(crate) fn with_sandbox(sandbox: Arc<dyn SandboxProvider>) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            policies: Arc::new(std::sync::RwLock::new(HashMap::new())),
            stdin_buffers: Arc::new(std::sync::RwLock::new(HashMap::new())),
            cwds: Arc::new(std::sync::RwLock::new(HashMap::new())),
            reaper_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sandbox: Arc::new(std::sync::RwLock::new(sandbox)),
        }
    }

    pub(crate) fn set_sandbox_provider(&self, sandbox: Arc<dyn SandboxProvider>) {
        if let Ok(mut guard) = self.sandbox.write() {
            *guard = sandbox;
        }
    }

    fn current_sandbox(&self) -> Arc<dyn SandboxProvider> {
        self.sandbox
            .read()
            .map(|guard| Arc::clone(&guard))
            .unwrap_or_else(|_| Arc::new(NativeSandboxProvider))
    }

    /// 会话创建时固定的沙箱策略；会话不存在返回 `None`。同步读取，供执行环境声明使用。
    pub(crate) fn policy_snapshot(&self, session_id: &str) -> Option<SandboxPolicy> {
        self.policies.read().ok()?.get(session_id).copied()
    }

    pub(crate) fn preview_stdin(&self, session_id: &str, chunk: &str) -> String {
        if session_id.is_empty() {
            return chunk.to_string();
        }
        let existing = self
            .stdin_buffers
            .read()
            .ok()
            .and_then(|guard| guard.get(session_id).cloned())
            .unwrap_or_default();
        if existing.is_empty() {
            chunk.to_string()
        } else {
            normalize_continued_lines(&format!("{existing}{chunk}"))
        }
    }

    /// 在同一写锁临界区内完成"拼接预览 → 危险检查 → 追加缓冲"：
    /// 被拒绝的 chunk 不入账、也不清空既有缓冲——缓冲记录的是已实际写入 shell 的
    /// 前缀，清掉会让攻击者重发后半截绕过拼接检测（`rm -r` 已过检写盘，再发 `f /`）。
    pub(crate) fn check_and_record_stdin_chunk(&self, session_id: &str, chunk: &str) -> Result<()> {
        let Ok(mut guard) = self.stdin_buffers.write() else {
            return Ok(());
        };
        let buf = guard.entry(session_id.to_string()).or_default();
        let combined = if buf.is_empty() {
            chunk.to_string()
        } else {
            normalize_continued_lines(&format!("{buf}{chunk}"))
        };
        if crate::exec::is_dangerous_command(chunk) || crate::exec::is_dangerous_command(&combined)
        {
            return Err(Error::PermissionDenied(
                "Input matches dangerous pattern and is blocked".to_string(),
            ));
        }
        buf.push_str(chunk);
        if buf.len() > MAX_STDIN_BUFFER_BYTES {
            let mut cut = buf.len() - MAX_STDIN_BUFFER_BYTES;
            while cut < buf.len() && !buf.is_char_boundary(cut) {
                cut += 1;
            }
            buf.drain(..cut);
        }
        drop(guard);
        // Track logical cwd for the path checker: the chunk was accepted and is
        // about to be written to the shell, so apply any cd/pushd it contains.
        self.track_cd(session_id, &combined);
        Ok(())
    }

    pub(crate) fn clear_stdin_buffer(&self, session_id: &str) {
        if let Ok(mut guard) = self.stdin_buffers.write() {
            guard.remove(session_id);
        }
    }

    /// 按命令文本更新会话的逻辑 cwd。只在能静态解析时推进：相对 `cd` 依赖
    /// 已跟踪的 cwd；含展开/替换、`cd -`、裸 `pushd`、或 `cd` 出现在子 shell
    /// 语境（无法判断是否会改父 shell cwd）时置为未知，交由检查侧 fail-closed。
    fn track_cd(&self, session_id: &str, text: &str) {
        let Ok(mut cwds) = self.cwds.write() else {
            return;
        };
        let Some(state) = cwds.get_mut(session_id) else {
            return;
        };
        for segment in text.split(['\n', ';', '&', '|']) {
            let seg = segment.trim();
            if seg.is_empty() {
                continue;
            }
            let mut tokens = seg.split_whitespace();
            let raw_head = tokens.next().unwrap_or("");
            let head = raw_head.trim_start_matches(['!', '{', '(']);
            if head != "cd" && head != "pushd" {
                // cd/pushd 紧跟在控制流/命令委派前缀后（`if cd x`、`sudo cd x`、`eval cd x`）
                // 时可能真执行并改变或分叉 cwd；普通参数位（如 `echo cd`、`git commit -m "cd"`）
                // 不会改变 shell cwd，不应误置未知。
                const DELEGATING_HEADS: &[&str] = &[
                    "if", "then", "else", "elif", "while", "until", "do", "sudo", "env", "command",
                    "builtin", "eval", "time", "nohup",
                ];
                if DELEGATING_HEADS.contains(&head)
                    && tokens.any(|t| {
                        let stripped =
                            t.trim_matches(|c| matches!(c, '"' | '\'' | ';' | '(' | ')'));
                        stripped == "cd" || stripped == "pushd" || stripped == "popd"
                    })
                {
                    *state = None;
                }
                continue;
            }
            // `cd` 出现在含展开/子 shell 的分段中 → 目标无法静态解析或不影响
            // 父 shell cwd，两种都不该据此推进状态。
            if seg.contains('$') || seg.contains('`') || seg.contains('(') {
                *state = None;
                continue;
            }
            let rest = seg[raw_head.len()..].trim();
            let quoted = (rest.starts_with('"') && rest.ends_with('"') && rest.len() >= 2)
                || (rest.starts_with('\'') && rest.ends_with('\'') && rest.len() >= 2);
            let target = if quoted {
                rest[1..rest.len() - 1].trim()
            } else {
                rest
            };
            if !quoted && target.contains(char::is_whitespace) {
                // 多余参数/无法确定的参数列表 → shell 大概率报错不改 cwd，但保守按未知处理
                *state = None;
                continue;
            }
            match (head, target) {
                ("cd", "") => {
                    *state = dirs::home_dir();
                }
                (_, "-") | ("pushd", "") => *state = None,
                (_, t) if t == "~" || t.starts_with("~/") => {
                    *state = dirs::home_dir()
                        .map(|h| normalize_lexically(&h.join(t.strip_prefix("~/").unwrap_or(""))));
                }
                (_, t) if t.starts_with('/') => {
                    *state = Some(normalize_lexically(&PathBuf::from(t)));
                }
                (_, t) => match state.as_ref() {
                    Some(cwd) => {
                        let joined = cwd.join(t);
                        *state = Some(normalize_lexically(&joined));
                    }
                    None => {
                        *state = None;
                    }
                },
            }
        }
    }

    /// 会话当前跟踪到的逻辑 cwd：
    /// - `None`：无此会话记录（新会话将以 workspace 启动，调用方按 workspace 解析）
    /// - `Some(Some(path))`：跟踪到的 cwd
    /// - `Some(None)`：cwd 未知（解析失败/子 shell 分叉），调用方必须 fail-closed
    pub(crate) fn cwd_snapshot(&self, session_id: &str) -> Option<Option<PathBuf>> {
        self.cwds
            .read()
            .ok()
            .and_then(|guard| guard.get(session_id).cloned())
    }

    fn ensure_background_reaper(&self) {
        use std::sync::atomic::Ordering;
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        if self
            .reaper_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                manager.reap_exited_sessions().await;
                let empty = manager.sessions.lock().await.is_empty();
                if empty {
                    manager.reaper_running.store(false, Ordering::SeqCst);
                    break;
                }
            }
        });
    }

    #[cfg(test)]
    pub(crate) async fn create(
        &self,
        workspace: &Path,
        sandbox_policy: SandboxPolicy,
        owner: SessionOwner<'_>,
    ) -> Result<String> {
        self.create_with_task_manager(workspace, sandbox_policy, owner, None)
            .await
    }

    pub(crate) async fn create_with_task_manager(
        &self,
        workspace: &Path,
        sandbox_policy: SandboxPolicy,
        owner: SessionOwner<'_>,
        task_manager: Option<crate::TaskManagerHandle>,
    ) -> Result<String> {
        let request = SandboxRequest {
            agent_id: owner.agent_id.to_string(),
            session_id: owner.session_id.to_string(),
            policy: sandbox_policy,
            writable_roots: vec![workspace.to_path_buf()],
            program: String::new(),
            args: Vec::new(),
        };
        let sandbox = self.current_sandbox();
        #[cfg(unix)]
        let (mut command, command_input, child_control) =
            build_interactive_shell(sandbox.as_ref(), request).await?;
        #[cfg(windows)]
        let mut command = build_interactive_shell(sandbox.as_ref(), request).await?;
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
        if let (Some(tm), Some(pid)) = (task_manager.as_ref(), pid) {
            tm.register_shell_process(&session_id, pid);
        }
        self.reap_exited_sessions().await;
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
                exit_code: None,
                exited_at: None,
                task_manager,
                owner_agent_id: owner.agent_id.to_string(),
                owner_session_id: owner.session_id.to_string(),
            })),
        );
        if let Ok(mut policies) = self.policies.write() {
            policies.insert(session_id.clone(), sandbox_policy);
        }
        if let Ok(mut cwds) = self.cwds.write() {
            cwds.insert(session_id.clone(), Some(workspace.to_path_buf()));
        }
        self.ensure_background_reaper();
        Ok(session_id)
    }

    /// 校验调用方是否为会话的创建者，防止跨会话/跨 Agent 越权操作持久 Shell。
    /// 采用严格匹配（fail-closed）：任一身份字段不一致（含一方为空另一方非空）均拒绝。
    pub(crate) async fn verify_owner(
        &self,
        session_id: &str,
        caller: SessionOwner<'_>,
    ) -> Result<()> {
        let session = self.get(session_id).await?;
        let session = session.lock().await;
        if session.owner_session_id != caller.session_id
            || session.owner_agent_id != caller.agent_id
        {
            return Err(Error::PermissionDenied(format!(
                "Shell session {session_id} belongs to another session or agent"
            )));
        }
        Ok(())
    }

    /// 回收子进程已退出的僵尸会话：
    /// 1. 一旦子进程退出，立即回收 OS 状态并从 `task_manager` 注销进程；
    /// 2. 若输出缓冲已被读空（或已退出超过保留期），从会话表中彻底移除，避免在最后一次读取前丢输出。
    pub(crate) async fn reap_exited_sessions(&self) {
        let entries: Vec<(String, Arc<Mutex<ShellSession>>)> = {
            let guard = self.sessions.lock().await;
            guard
                .iter()
                .map(|(k, v)| (k.clone(), Arc::clone(v)))
                .collect()
        };
        let mut removable_ids = Vec::new();
        for (id, session_arc) in entries {
            if let Ok(mut session) = session_arc.try_lock() {
                if session.exited_at.is_none() {
                    if let Ok(Some(status)) = session.child.try_wait() {
                        session.exit_code = status.code();
                        session.pid = None;
                        session.exited_at = Some(tokio::time::Instant::now());
                    } else if session.pid.is_none() {
                        session.exited_at = Some(tokio::time::Instant::now());
                    }
                }
                if let Some(exited_at) = session.exited_at {
                    if let Some(tm) = session.task_manager.take() {
                        tm.unregister_shell_process(&id);
                    }
                    let has_unread = session.active.is_some()
                        || session
                            .output
                            .try_lock()
                            .map(|shared| !shared.bytes.is_empty())
                            .unwrap_or(true);
                    if !has_unread || exited_at.elapsed() >= EXITED_SESSION_RETENTION {
                        session.active = None;
                        removable_ids.push(id);
                    }
                }
            }
        }
        if !removable_ids.is_empty() {
            let mut guard = self.sessions.lock().await;
            for id in &removable_ids {
                guard.remove(id);
            }
            if let Ok(mut policies) = self.policies.write() {
                for id in &removable_ids {
                    policies.remove(id);
                }
            }
            if let Ok(mut stdin_buffers) = self.stdin_buffers.write() {
                for id in &removable_ids {
                    stdin_buffers.remove(id);
                }
            }
        }
    }

    pub(crate) async fn remove_if_exited(&self, session_id: &str) -> bool {
        let Ok(session_arc) = self.get(session_id).await else {
            return false;
        };
        let exited = {
            let mut session = session_arc.lock().await;
            if session.exited_at.is_none() {
                if let Ok(Some(status)) = session.child.try_wait() {
                    session.exit_code = status.code();
                    session.pid = None;
                    session.exited_at = Some(tokio::time::Instant::now());
                } else if session.pid.is_none() {
                    session.exited_at = Some(tokio::time::Instant::now());
                }
            }
            if session.exited_at.is_some() {
                session.pid = None;
                session.active = None;
                if let Some(tm) = session.task_manager.take() {
                    tm.unregister_shell_process(session_id);
                }
                true
            } else {
                false
            }
        };
        if exited {
            self.sessions.lock().await.remove(session_id);
            if let Ok(mut policies) = self.policies.write() {
                policies.remove(session_id);
            }
            if let Ok(mut cwds) = self.cwds.write() {
                cwds.remove(session_id);
            }
            self.clear_stdin_buffer(session_id);
        }
        exited
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
        let combined = self.preview_stdin(session_id, command);
        if crate::exec::is_dangerous_command(command)
            || crate::exec::is_dangerous_command(&combined)
        {
            return Err(Error::PermissionDenied(
                "Command matches dangerous pattern and is blocked".to_string(),
            ));
        }
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
        self.clear_stdin_buffer(session_id);
        self.track_cd(session_id, command);
        self.poll(session_id, timeout).await
    }

    pub(crate) async fn poll(
        &self,
        session_id: &str,
        timeout: Duration,
    ) -> Result<ShellCommandOutput> {
        let deadline = tokio::time::Instant::now() + timeout;
        // 一次 poll 会跨多个轮询周期，而命令输出是分批到达的：`poll_once` 每次只返回自上一轮
        // 以来新到的字节，并推进"已交付"游标。因此必须把每一轮都累积起来——只返回最后一轮会
        // 静默丢掉先到的输出（`echo a; sleep 1; echo b` 会只剩 b）。
        let mut output = String::new();
        let mut truncated = false;
        loop {
            let polled = self.poll_once(session_id).await?;
            let capped = append_capped(&mut output, &polled.output);
            truncated = truncated || capped || polled.truncated;
            if !polled.running || tokio::time::Instant::now() >= deadline {
                return Ok(ShellCommandOutput {
                    output,
                    exit_code: polled.exit_code,
                    running: polled.running,
                    truncated,
                });
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn poll_once(&self, session_id: &str) -> Result<ShellCommandOutput> {
        let session = self.get(session_id).await?;
        let mut session = session.lock().await;
        let Some(active) = session.active.as_ref() else {
            if session.exited_at.is_none() {
                if let Some(status) = session
                    .child
                    .try_wait()
                    .map_err(blockcell_core::Error::Io)?
                {
                    session.exit_code = status.code();
                    session.pid = None;
                    session.exited_at = Some(tokio::time::Instant::now());
                    if let Some(tm) = session.task_manager.take() {
                        tm.unregister_shell_process(session_id);
                    }
                }
            }
            return Ok(ShellCommandOutput {
                output: String::new(),
                exit_code: session.exit_code,
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

        if session.exited_at.is_none() {
            if let Some(status) = session
                .child
                .try_wait()
                .map_err(blockcell_core::Error::Io)?
            {
                session.exit_code = status.code();
                session.pid = None;
                session.exited_at = Some(tokio::time::Instant::now());
            }
        }
        if session.exited_at.is_some() {
            session.active = None;
            session.pid = None;
            if let Some(tm) = session.task_manager.take() {
                tm.unregister_shell_process(session_id);
            }
            let mut shared = session.output.lock().await;
            shared.bytes.clear();
            shared.bytes.shrink_to_fit();
            drop(shared);
            return Ok(ShellCommandOutput {
                output,
                exit_code: session.exit_code,
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
        self.check_and_record_stdin_chunk(session_id, input)?;
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
        if let Ok(mut policies) = self.policies.write() {
            policies.remove(session_id);
        }
        if let Ok(mut cwds) = self.cwds.write() {
            cwds.remove(session_id);
        }
        self.clear_stdin_buffer(session_id);
        let mut session = session.lock().await;
        if let Some(tm) = session.task_manager.take() {
            tm.unregister_shell_process(session_id);
        }
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
        if session.exited_at.is_none() {
            if let Some(status) = session
                .child
                .try_wait()
                .map_err(blockcell_core::Error::Io)?
            {
                session.exit_code = status.code();
                session.pid = None;
                session.exited_at = Some(tokio::time::Instant::now());
                if let Some(tm) = session.task_manager.take() {
                    tm.unregister_shell_process(session_id);
                }
            }
        }
        Ok((session.pid, session.sandbox_policy))
    }
}

/// 预览某持久 `shell` 会话若继续注入 `chunk` 后的累积 `stdin` 缓冲，用于识别跨多次
/// `write_stdin` 调用拆分的危险命令或路径。
pub fn preview_session_stdin(session_id: &str, chunk: &str) -> String {
    SHELL_SESSIONS.preview_stdin(session_id, chunk)
}

fn normalize_continued_lines(input: &str) -> String {
    input.replace("\\\r\n", "").replace("\\\n", "")
}

/// Lexically normalize `.`/`..` components (no filesystem access).
fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 会话当前跟踪到的逻辑 cwd（供路径安全检查解析相对路径）：
/// - `None`：无会话记录（新会话以 workspace 启动）
/// - `Some(Some(path))`：跟踪到的 cwd
/// - `Some(None)`：cwd 未知，调用方必须 fail-closed
pub fn session_cwd(session_id: &str) -> Option<Option<PathBuf>> {
    SHELL_SESSIONS.cwd_snapshot(session_id)
}

/// 会话创建时固定的沙箱策略（同步查询）；会话不存在返回 `None`。
///
/// 供执行环境 / 确认声明使用：复用会话（`action=run` + `session_id` 且不带 `sandbox_policy`）
/// 时，参数本身看不出会话是否以 full-access 运行，只能查这份创建时的快照。
pub fn session_sandbox_policy(session_id: &str) -> Option<SandboxPolicy> {
    SHELL_SESSIONS.policy_snapshot(session_id)
}

/// 注入全局持久 `shell` 会话使用的 [`SandboxProvider`]（如自定义容器 / 远程沙箱）。
pub fn set_shell_sandbox_provider(provider: Arc<dyn SandboxProvider>) {
    SHELL_SESSIONS.set_sandbox_provider(provider);
}

/// 仅测试用：直接写入会话策略快照，避免为声明测试拉起真实 shell。
#[cfg(test)]
pub(crate) fn insert_session_policy_for_test(session_id: &str, policy: SandboxPolicy) {
    if let Ok(mut policies) = SHELL_SESSIONS.policies.write() {
        policies.insert(session_id.to_string(), policy);
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn execution_environment(&self, params: &Value) -> crate::ExecutionEnvironment {
        if crate::execution_env::effective_sandbox_is_full_access(params) {
            crate::ExecutionEnvironment::FullAccess
        } else {
            crate::ExecutionEnvironment::Shell
        }
    }

    /// full-access 的 shell 没有命令级兜底，必须逐次确认。
    fn requires_confirmation(&self, params: &Value) -> bool {
        crate::execution_env::requests_full_access("shell", params)
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
                let session_id = params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let input = params.get("input").and_then(Value::as_str).ok_or_else(|| {
                    Error::Validation("action=write_stdin requires input".to_string())
                })?;
                let combined = preview_session_stdin(session_id, input);
                if crate::exec::is_dangerous_command(input)
                    || crate::exec::is_dangerous_command(&combined)
                {
                    return Err(Error::PermissionDenied(
                        "Input matches dangerous pattern and is blocked".to_string(),
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

        let caller_agent_id = ctx
            .agent_identity
            .as_ref()
            .map(|identity| identity.agent_id.as_str())
            .filter(|id| !id.is_empty())
            .unwrap_or("default");
        let caller_owner = SessionOwner {
            agent_id: caller_agent_id,
            session_id: &ctx.session_key,
        };

        match action {
            "run" => {
                let command = params["command"].as_str().unwrap_or_default();
                let session_id = if let Some(session_id) =
                    params.get("session_id").and_then(Value::as_str)
                {
                    SHELL_SESSIONS
                        .verify_owner(session_id, caller_owner)
                        .await?;
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
                    SHELL_SESSIONS
                        .create_with_task_manager(
                            &ctx.workspace,
                            policy,
                            caller_owner,
                            ctx.task_manager.clone(),
                        )
                        .await?
                };
                let output = SHELL_SESSIONS
                    .run_command(&session_id, command, timeout)
                    .await?;
                let (pid, policy) = SHELL_SESSIONS.session_metadata(&session_id).await?;
                if pid.is_none() {
                    SHELL_SESSIONS.remove_if_exited(&session_id).await;
                    if let Some(task_manager) = ctx.task_manager.as_ref() {
                        task_manager.unregister_shell_process(&session_id);
                    }
                }
                Ok(shell_output_json(&session_id, policy, output))
            }
            "poll" => {
                let session_id = params["session_id"].as_str().unwrap_or_default();
                SHELL_SESSIONS
                    .verify_owner(session_id, caller_owner)
                    .await?;
                let output = SHELL_SESSIONS.poll(session_id, timeout).await?;
                let (pid, policy) = SHELL_SESSIONS.session_metadata(session_id).await?;
                if pid.is_none() {
                    SHELL_SESSIONS.remove_if_exited(session_id).await;
                    if let Some(task_manager) = ctx.task_manager.as_ref() {
                        task_manager.unregister_shell_process(session_id);
                    }
                }
                Ok(shell_output_json(session_id, policy, output))
            }
            "write_stdin" => {
                let session_id = params["session_id"].as_str().unwrap_or_default();
                SHELL_SESSIONS
                    .verify_owner(session_id, caller_owner)
                    .await?;
                let input = params["input"].as_str().unwrap_or_default();
                SHELL_SESSIONS.write_stdin(session_id, input).await?;
                let output = SHELL_SESSIONS.poll(session_id, timeout).await?;
                let (pid, policy) = SHELL_SESSIONS.session_metadata(session_id).await?;
                if pid.is_none() {
                    SHELL_SESSIONS.remove_if_exited(session_id).await;
                    if let Some(task_manager) = ctx.task_manager.as_ref() {
                        task_manager.unregister_shell_process(session_id);
                    }
                }
                Ok(shell_output_json(session_id, policy, output))
            }
            "close" => {
                let session_id = params["session_id"].as_str().unwrap_or_default();
                SHELL_SESSIONS
                    .verify_owner(session_id, caller_owner)
                    .await?;
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

/// 把一轮轮询的输出追加到累积结果，并把长度限制在 `MAX_SESSION_OUTPUT_BYTES` 内。
///
/// 累积多个轮询周期后，单次调用的结果可能超过环形缓冲的容量（例如持续输出的命令）；
/// 这里保留尾部并置位截断标记，与 [`SharedOutput`] 丢弃最旧字节的行为保持一致。
/// 返回是否因超长而截断。
fn append_capped(accumulated: &mut String, chunk: &str) -> bool {
    accumulated.push_str(chunk);
    if accumulated.len() <= MAX_SESSION_OUTPUT_BYTES {
        return false;
    }
    // 按字符边界截断，避免切开 UTF-8 序列。
    let mut cut = accumulated.len() - MAX_SESSION_OUTPUT_BYTES;
    while cut < accumulated.len() && !accumulated.is_char_boundary(cut) {
        cut += 1;
    }
    accumulated.drain(..cut);
    true
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
    #[cfg(windows)]
    if let Some(pid) = pid {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(any(unix, windows)))]
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

    /// `run_command`/`write_stdin` 中的 cd 会改变持久 shell 的 cwd——路径安全检查
    /// 必须按跟踪到的 cwd 解析后续相对路径，而非固定按 workspace。
    #[tokio::test]
    async fn session_cwd_tracks_cd_and_goes_unknown_on_unresolvable() {
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

        assert_eq!(
            manager.cwd_snapshot(&session_id),
            Some(Some(workspace.path().to_path_buf())),
            "new session starts in the workspace"
        );

        manager
            .run_command(
                &session_id,
                "mkdir nested && cd nested",
                Duration::from_secs(2),
            )
            .await
            .expect("cd into subdirectory");
        assert_eq!(
            manager.cwd_snapshot(&session_id),
            Some(Some(normalize_lexically(&workspace.path().join("nested")))),
        );

        manager
            .run_command(&session_id, "cd ..", Duration::from_secs(2))
            .await
            .expect("cd back");
        assert_eq!(
            manager.cwd_snapshot(&session_id),
            Some(Some(normalize_lexically(workspace.path()))),
        );

        // 绝对 cd 总是可追踪
        manager
            .run_command(&session_id, "cd /tmp", Duration::from_secs(2))
            .await
            .expect("cd to absolute path");
        assert_eq!(
            manager.cwd_snapshot(&session_id),
            Some(Some(PathBuf::from("/tmp"))),
        );

        // 展开式目标无法静态解析 → 未知（检查侧 fail-closed）
        manager
            .run_command(&session_id, "cd $SOME_DIR", Duration::from_secs(2))
            .await
            .expect("cd to unresolvable target");
        assert_eq!(manager.cwd_snapshot(&session_id), Some(None));

        // 未知状态下绝对 cd 可恢复跟踪
        manager
            .run_command(&session_id, "cd /var", Duration::from_secs(2))
            .await
            .expect("cd recovers tracking");
        assert_eq!(
            manager.cwd_snapshot(&session_id),
            Some(Some(PathBuf::from("/var"))),
        );

        manager.close(&session_id).await.expect("close session");
        assert_eq!(manager.cwd_snapshot(&session_id), None);
    }

    #[test]
    fn track_cd_static_cases() {
        let manager = ShellSessionManager::new();
        manager
            .cwds
            .write()
            .expect("cwds lock")
            .insert("s".to_string(), Some(PathBuf::from("/ws/app")));

        manager.track_cd("s", "cd sub/dir");
        assert_eq!(
            manager.cwd_snapshot("s"),
            Some(Some(PathBuf::from("/ws/app/sub/dir")))
        );

        manager.track_cd("s", "cd ../../..");
        assert_eq!(manager.cwd_snapshot("s"), Some(Some(PathBuf::from("/ws"))));

        // 子 shell 中的 cd 不改变父 shell cwd → 未知
        manager.track_cd("s", "(cd /tmp)");
        assert_eq!(manager.cwd_snapshot("s"), Some(None));

        // 未知状态下相对 cd 仍未知
        manager.track_cd("s", "cd rel");
        assert_eq!(manager.cwd_snapshot("s"), Some(None));

        // cd - 依赖 OLDPWD → 未知
        manager.track_cd("s", "cd /x; cd -");
        assert_eq!(manager.cwd_snapshot("s"), Some(None));

        // 引号包裹的相对路径照常解析
        manager.track_cd("s", "cd '/a/b'");
        assert_eq!(manager.cwd_snapshot("s"), Some(Some(PathBuf::from("/a/b"))));

        // 普通命令参数位里的 `cd` 不改变 cwd，不应误置未知
        manager.track_cd("s", "echo cd");
        assert_eq!(manager.cwd_snapshot("s"), Some(Some(PathBuf::from("/a/b"))));
    }

    #[tokio::test]
    async fn run_command_blocks_dangerous_command_split_from_stdin_buffer() {
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

        manager
            .check_and_record_stdin_chunk(&session_id, "rm -r")
            .expect("prefix chunk allowed");
        let err = manager
            .run_command(&session_id, "f /", Duration::from_secs(1))
            .await
            .expect_err("split dangerous command across write_stdin + run_command must be blocked");
        assert!(matches!(err, Error::PermissionDenied(_)));

        manager.close(&session_id).await.expect("close session");
    }

    /// 命令输出会分批到达。轮询等待期间每一批都必须累积到最终结果里——只保留最后一批
    /// 会丢掉先到的输出（`poll_once` 每轮都会推进"已交付"游标，于是先到的字节不再被计入）。
    #[tokio::test]
    async fn shell_command_keeps_output_from_earlier_poll_iterations() {
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

        let output = manager
            .run_command(
                &session_id,
                "echo first; sleep 0.3; echo second",
                Duration::from_secs(5),
            )
            .await
            .expect("run command");

        assert!(!output.running);
        assert_eq!(output.exit_code, Some(0));
        assert!(
            output.output.contains("first"),
            "output delivered before the last poll iteration was dropped: {:?}",
            output.output
        );
        assert!(
            output.output.contains("second"),
            "final output missing: {:?}",
            output.output
        );
        manager.close(&session_id).await.expect("close session");
    }

    /// 累积结果必须保持有界（与环形缓冲同量级），且截断不能切开 UTF-8 序列。
    #[test]
    fn accumulated_output_is_capped_without_splitting_utf8() {
        let mut accumulated = String::from("head");
        let chunk = "é".repeat(MAX_SESSION_OUTPUT_BYTES);

        assert!(append_capped(&mut accumulated, &chunk));
        assert!(
            accumulated.len() <= MAX_SESSION_OUTPUT_BYTES,
            "capped length was {}",
            accumulated.len()
        );
        assert!(accumulated.chars().all(|ch| ch == 'é'));

        let mut small = String::new();
        assert!(!append_capped(&mut small, "ok"));
        assert_eq!(small, "ok");
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

    /// 无论宿主原生沙箱是否可用，以 `workspace-write` 建会话都必须"要么真的隔离、要么明确要求
    /// 审批"，绝不能静默地不受限运行。
    ///
    /// 原用例在原生沙箱可用的宿主上直接 `return`（静默跳过、什么都没断言），只有不支持的宿主才
    /// 真正执行；这里对两种宿主都断言，消除这条宿主相关的分支。
    #[tokio::test]
    async fn workspace_write_session_is_either_enforced_or_requires_approval() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let manager = ShellSessionManager::new();

        match manager
            .create(
                workspace.path(),
                SandboxPolicy::WorkspaceWrite,
                SessionOwner::default(),
            )
            .await
        {
            // 原生沙箱不可用：必须明确要求审批，而不是静默降级为不受限执行
            Err(error) => assert!(
                error.to_string().contains("approval required"),
                "unavailable sandbox must request approval, got: {error}"
            ),
            // 原生沙箱可用：会话必须真的以 workspace-write 建立
            Ok(session_id) => {
                let (_, policy) = manager
                    .session_metadata(&session_id)
                    .await
                    .expect("session metadata");
                assert_eq!(policy, SandboxPolicy::WorkspaceWrite);
                manager.close(&session_id).await.expect("close session");
            }
        }
    }

    #[tokio::test]
    async fn session_owner_verification_rejects_cross_session_access() {
        let workspace = tempfile::tempdir().expect("temp workspace");
        let manager = ShellSessionManager::new();
        let session_id = manager
            .create(
                workspace.path(),
                SandboxPolicy::FullAccess,
                SessionOwner {
                    agent_id: "agent-a",
                    session_id: "cli:session-a",
                },
            )
            .await
            .expect("create owned session");

        assert!(manager
            .verify_owner(
                &session_id,
                SessionOwner {
                    agent_id: "agent-a",
                    session_id: "cli:session-a",
                }
            )
            .await
            .is_ok());

        let err = manager
            .verify_owner(
                &session_id,
                SessionOwner {
                    agent_id: "agent-a",
                    session_id: "cli:session-b",
                },
            )
            .await
            .expect_err("cross-session access must be rejected");
        assert!(matches!(err, Error::PermissionDenied(_)), "{err}");

        // Empty caller identity fields must fail closed, never bypass owner verification
        let empty_session_err = manager
            .verify_owner(
                &session_id,
                SessionOwner {
                    agent_id: "agent-a",
                    session_id: "",
                },
            )
            .await
            .expect_err("empty caller session_id must be rejected");
        assert!(
            matches!(empty_session_err, Error::PermissionDenied(_)),
            "{empty_session_err}"
        );

        let empty_agent_err = manager
            .verify_owner(
                &session_id,
                SessionOwner {
                    agent_id: "",
                    session_id: "cli:session-a",
                },
            )
            .await
            .expect_err("empty caller agent_id must be rejected");
        assert!(
            matches!(empty_agent_err, Error::PermissionDenied(_)),
            "{empty_agent_err}"
        );

        manager.close(&session_id).await.expect("close session");
    }

    #[test]
    fn write_stdin_blocks_dangerous_commands_in_validate() {
        let err = ShellTool
            .validate(&json!({
                "action": "write_stdin",
                "session_id": "s1",
                "input": "rm -rf /\n"
            }))
            .expect_err("catastrophic rm in write_stdin must be blocked");
        assert!(matches!(err, Error::PermissionDenied(_)), "{err}");
    }

    #[tokio::test]
    async fn write_stdin_blocks_dangerous_command_split_across_chunks() {
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

        let _ = manager
            .run_command(&session_id, "cat", Duration::from_millis(20))
            .await
            .expect("start cat");

        manager
            .write_stdin(&session_id, "rm -r")
            .await
            .expect("first fragment alone is not yet dangerous");
        let err = manager
            .write_stdin(&session_id, "f /\n")
            .await
            .expect_err("second fragment completing `rm -rf /` must be blocked");
        assert!(matches!(err, Error::PermissionDenied(_)), "{err}");

        manager.close(&session_id).await.expect("close session");
    }

    #[tokio::test]
    async fn reap_preserves_unread_output_and_unregisters_task_manager() {
        #[derive(Default)]
        struct RecordingTaskManager {
            active: std::sync::Mutex<std::collections::HashMap<String, u32>>,
        }

        #[async_trait]
        impl crate::TaskManagerOps for RecordingTaskManager {
            async fn list_tasks_json(&self, _status_filter: Option<String>) -> Value {
                Value::Null
            }
            async fn get_task_json(&self, _task_id: &str) -> Option<Value> {
                None
            }
            async fn summary_json(&self) -> Value {
                Value::Null
            }
            async fn send_message(&self, _task_id: &str, _message: String) -> Result<()> {
                Ok(())
            }
            async fn is_one_shot_task(&self, _task_id: &str) -> bool {
                false
            }
            fn register_shell_process(&self, session_id: &str, pid: u32) {
                self.active
                    .lock()
                    .unwrap()
                    .insert(session_id.to_string(), pid);
            }
            fn unregister_shell_process(&self, session_id: &str) {
                self.active.lock().unwrap().remove(session_id);
            }
        }

        let workspace = tempfile::tempdir().expect("temp workspace");
        let manager = ShellSessionManager::new();
        let recorder = Arc::new(RecordingTaskManager::default());
        let task_manager: crate::TaskManagerHandle = recorder.clone();
        let session_id = manager
            .create_with_task_manager(
                workspace.path(),
                SandboxPolicy::FullAccess,
                SessionOwner::default(),
                Some(task_manager),
            )
            .await
            .expect("create shell session");

        assert_eq!(recorder.active.lock().unwrap().len(), 1);

        let timed_out = manager
            .run_command(
                &session_id,
                "sleep 0.08; echo final-words; exit 0",
                Duration::from_millis(10),
            )
            .await
            .expect("start exiting command");
        assert!(timed_out.running);

        tokio::time::sleep(Duration::from_millis(150)).await;
        // Reaping after process exit must unregister from task_manager immediately,
        // but keep the session until its unread output has been polled.
        manager.reap_exited_sessions().await;
        assert!(
            recorder.active.lock().unwrap().is_empty(),
            "exited shell must be unregistered from task_manager on reap"
        );

        let polled = manager
            .poll(&session_id, Duration::from_millis(50))
            .await
            .expect("unread output must remain pollable after reap");
        assert!(!polled.running);
        assert!(
            polled.output.contains("final-words"),
            "output was: {:?}",
            polled.output
        );

        // Once output is drained, the next reap removes the session entry.
        manager.reap_exited_sessions().await;
        assert!(manager.get(&session_id).await.is_err());
    }
}
