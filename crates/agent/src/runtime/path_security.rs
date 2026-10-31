//! 路径安全检查 — AgentRuntime 的文件系统访问控制方法
//!
//! 包含路径提取、安全校验、用户授权和危险操作确认。

use super::{is_path_within_base, persist_session_key_for, ConfirmRequest};
use blockcell_core::path_policy::{resolve_for_policy, PathOp, PolicyAction};
use blockcell_core::InboundMessage;
use std::path::{Component, Path, PathBuf};
use tracing::{info, warn};

/// Maximum time to wait for a user confirmation before denying the operation.
/// Without a bound, a UI that never answers the oneshot leaves the turn pending forever.
const CONFIRM_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PathAccess {
    pub(super) path: String,
    pub(super) op: PathOp,
}

fn push_path(accesses: &mut Vec<PathAccess>, args: &serde_json::Value, field: &str, op: PathOp) {
    if let Some(path) = args.get(field).and_then(|value| value.as_str()) {
        accesses.push(PathAccess {
            path: path.to_string(),
            op,
        });
    }
}

fn push_paths(accesses: &mut Vec<PathAccess>, args: &serde_json::Value, field: &str, op: PathOp) {
    if let Some(paths) = args.get(field).and_then(|value| value.as_array()) {
        for path in paths.iter().filter_map(|value| value.as_str()) {
            accesses.push(PathAccess {
                path: path.to_string(),
                op,
            });
        }
    }
}

fn sandboxed_shell_expansion(
    command: &str,
    cwd: &Path,
    workspace: &Path,
    backend: blockcell_tools::sandbox::SandboxBackend,
    policy: blockcell_tools::sandbox::SandboxPolicy,
) -> Option<(String, Vec<PathAccess>)> {
    use blockcell_tools::sandbox::{SandboxBackend, SandboxPolicy};
    if policy == SandboxPolicy::FullAccess
        || !matches!(
            backend,
            SandboxBackend::MacOsSeatbelt
                | SandboxBackend::LinuxBubblewrap
                | SandboxBackend::LinuxLandlockSeccomp
        )
        || !backend.enforces(policy)
        || !is_path_within_base(workspace, cwd)
        || command_mutates_cwd(command)
    {
        return None;
    }
    let safe_relative = |text: &str| {
        !text.is_empty()
            && !text.starts_with('/')
            && !text.starts_with('~')
            && !text.contains("..")
            && text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_./-".contains(c))
    };
    let command = command.trim();
    // Lookup results are allowed only as an executable, never as a read operand.
    let lookup =
        regex::Regex::new(r"^\$\((?:command -v|which) ([A-Za-z0-9_-]+)\)([ A-Za-z0-9_./-]*)$")
            .ok()?;
    if let Some(caps) = lookup.captures(command) {
        return Some((format!("{}{}", &caps[1], &caps[2]), Vec::new()));
    }
    let loop_re = regex::Regex::new(
        r#"^for ([A-Za-z_][A-Za-z0-9_]*) in \$\(find ([A-Za-z0-9_./-]+) -type f\); do (.+); done$"#,
    )
    .ok()?;
    if let Some(caps) = loop_re.captures(command) {
        let variable = &caps[1];
        let root = &caps[2];
        if !safe_relative(root) {
            return None;
        }
        let root = cwd.join(root);
        if !is_path_within_base(workspace, &root)
            || root.symlink_metadata().ok()?.file_type().is_symlink()
        {
            return None;
        }
        let body = &caps[3];
        let operand = format!("\"${variable}\"");
        let prefix = body.strip_suffix(&operand)?.trim_end();
        let tokens: Vec<_> = prefix.split_whitespace().collect();
        let valid = tokens == ["cat"]
            || (tokens.len() == 3
                && tokens[0] == "grep"
                && matches!(tokens[1], "-n" | "-l" | "-H")
                && tokens[2]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c)));
        if !valid {
            return None;
        }
        // Enumerate bounded read operands so explicit per-file denials still win.
        // Reject symlinks and filenames altered by shell word splitting/globbing.
        let mut pending = vec![root.clone()];
        let mut accesses = vec![PathAccess {
            path: root.to_string_lossy().into_owned(),
            op: PathOp::Read,
        }];
        let mut seen = 0;
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).ok()? {
                seen += 1;
                if seen > 2048 {
                    return None;
                }
                let entry = entry.ok()?;
                let path = entry.path();
                let relative = path.strip_prefix(cwd).ok()?.to_str()?;
                if !safe_relative(relative) || !is_path_within_base(workspace, &path) {
                    return None;
                }
                let kind = entry.file_type().ok()?;
                if kind.is_symlink() {
                    return None;
                }
                if kind.is_dir() {
                    pending.push(path.clone());
                }
                accesses.push(PathAccess {
                    path: path.to_string_lossy().into_owned(),
                    op: PathOp::Read,
                });
            }
        }
        return Some((String::new(), accesses));
    }
    let substitution = regex::Regex::new(r"\$\((pwd|dirname ([A-Za-z0-9_./-]+))\)").ok()?;
    let mut normalized = String::new();
    let mut cursor = 0;
    let mut changed = false;
    for caps in substitution.captures_iter(command) {
        let whole = caps.get(0)?;
        let path = if &caps[1] == "pwd" {
            cwd.to_path_buf()
        } else {
            let relative = caps.get(2)?.as_str();
            if !safe_relative(relative) {
                return None;
            }
            let path = cwd.join(relative);
            if !is_path_within_base(workspace, &path) {
                return None;
            }
            path.parent()?.to_path_buf()
        };
        normalized.push_str(&command[cursor..whole.start()]);
        normalized.push_str(path.to_str()?);
        cursor = whole.end();
        changed = true;
    }
    normalized.push_str(&command[cursor..]);
    if changed && !normalized.contains('$') && !normalized.contains('`') {
        Some((normalized, Vec::new()))
    } else {
        None
    }
}

impl super::AgentRuntime {
    /// Extract filesystem paths from tool call parameters.
    pub(super) fn extract_path_accesses(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Vec<PathAccess> {
        let fallback = || extract_path_accesses(tool_name, args);
        if !matches!(tool_name, "shell" | "exec") {
            return fallback();
        }
        if tool_name == "shell"
            && args
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("run")
                != "run"
        {
            return fallback();
        }
        let Some(command) = args.get("command").and_then(serde_json::Value::as_str) else {
            return fallback();
        };
        let workspace = self.paths.workspace();
        let session = args.get("session_id").and_then(serde_json::Value::as_str);
        let cwd = if tool_name == "shell" {
            match session.and_then(blockcell_tools::shell_session::session_cwd) {
                Some(Some(cwd)) => cwd,
                Some(None) => return fallback(),
                None => workspace.clone(),
            }
        } else {
            args.get("working_dir")
                .and_then(serde_json::Value::as_str)
                .map(|path| self.resolve_path(path))
                .unwrap_or_else(|| workspace.clone())
        };
        let policy = if tool_name == "shell" {
            session
                .and_then(blockcell_tools::shell_session::session_sandbox_policy)
                .map(Ok)
                .unwrap_or_else(|| {
                    blockcell_tools::sandbox::SandboxPolicy::parse(
                        args.get("sandbox_policy")
                            .and_then(serde_json::Value::as_str)
                            .or(self.config.tools.exec.sandbox_policy.as_deref()),
                    )
                })
        } else {
            blockcell_tools::exec::ExecTool::resolve_config_policy(&self.config.tools.exec)
        };
        let Ok(policy) = policy else {
            return fallback();
        };
        let Some((command, mut extra)) = sandboxed_shell_expansion(
            command,
            &cwd,
            &workspace,
            blockcell_tools::sandbox::native_backend(policy),
            policy,
        ) else {
            return fallback();
        };
        let mut normalized = args.clone();
        normalized["command"] = serde_json::Value::String(command);
        extra.extend(extract_path_accesses(tool_name, &normalized));
        extra
    }
}

/// Free-function form of the extraction table.
///
/// The table itself needs no runtime state, so it is testable in isolation
/// (P3-022): a missed branch here silently exempts a tool's paths from the
/// path policy, which is exactly the class of bug the table exists to prevent.
pub(super) fn extract_path_accesses(tool_name: &str, args: &serde_json::Value) -> Vec<PathAccess> {
    let mut accesses = Vec::new();
    let action = args
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or("");

    match tool_name {
        "read_file" => {
            push_path(&mut accesses, args, "path", PathOp::Read);
            push_paths(&mut accesses, args, "paths", PathOp::Read);
        }
        "write_file" => {
            push_path(&mut accesses, args, "path", PathOp::Write);
            if let Some(files) = args.get("files").and_then(|value| value.as_array()) {
                for file in files {
                    push_path(&mut accesses, file, "path", PathOp::Write);
                }
            }
        }
        "edit_file" => push_path(&mut accesses, args, "path", PathOp::Write),
        "list_dir" => push_path(&mut accesses, args, "path", PathOp::List),
        "file_ops" => {
            let source_op = match action {
                "delete" | "rename" | "move" => PathOp::Write,
                _ => PathOp::Read,
            };
            push_path(&mut accesses, args, "path", source_op);
            push_paths(&mut accesses, args, "paths", PathOp::Read);
            push_path(&mut accesses, args, "destination", PathOp::Write);
        }
        "data_process" => {
            let path_op = if action == "write_csv" {
                PathOp::Write
            } else {
                PathOp::Read
            };
            push_path(&mut accesses, args, "path", path_op);
            push_path(&mut accesses, args, "output_path", PathOp::Write);
        }
        "audio_transcribe" | "ocr" | "encrypt" => {
            push_path(&mut accesses, args, "path", PathOp::Read);
            push_path(&mut accesses, args, "output_path", PathOp::Write);
        }
        "chart_generate" | "office_write" | "camera" | "tts" | "knowledge_graph" => {
            push_path(&mut accesses, args, "output_path", PathOp::Write);
        }
        "video_process" => {
            push_path(&mut accesses, args, "input", PathOp::Read);
            push_paths(&mut accesses, args, "inputs", PathOp::Read);
            push_path(&mut accesses, args, "subtitle_file", PathOp::Read);
            push_path(&mut accesses, args, "watermark_image", PathOp::Read);
            push_path(&mut accesses, args, "output_path", PathOp::Write);
        }
        "image_understand" => {
            push_path(&mut accesses, args, "path", PathOp::Read);
            push_paths(&mut accesses, args, "paths", PathOp::Read);
        }
        "termux_api" => {
            let file_op = match action {
                "share" | "media_scan" | "wallpaper" => PathOp::Read,
                _ => PathOp::Write,
            };
            push_path(&mut accesses, args, "file_path", file_op);
            push_path(&mut accesses, args, "output_path", PathOp::Write);
        }
        "email" => {
            push_paths(&mut accesses, args, "attachments", PathOp::Read);
            push_path(&mut accesses, args, "save_attachments_to", PathOp::Write);
        }
        "health_api" => {
            push_path(&mut accesses, args, "path", PathOp::Read);
            push_path(&mut accesses, args, "output_path", PathOp::Write);
        }
        "http_request" => {
            // `save_to` writes the response body to disk. The runtime only
            // recognises this name here, so omitting the branch meant the
            // write never reached the path policy.
            push_path(&mut accesses, args, "save_to", PathOp::Write);
        }
        "app_control" => {
            push_path(&mut accesses, args, "screenshot_path", PathOp::Write);
        }
        "message" => {
            push_paths(&mut accesses, args, "media", PathOp::Read);
        }
        "browse" => {
            push_path(&mut accesses, args, "output_path", PathOp::Write);
            push_path(&mut accesses, args, "file_path", PathOp::Read);
            push_paths(&mut accesses, args, "files", PathOp::Read);
        }
        "exec" => {
            push_path(&mut accesses, args, "working_dir", PathOp::Exec);
            // Also subject filesystem paths referenced inside the command
            // itself to the path policy, so built-in sensitive paths
            // (~/.ssh, /etc, ...) are enforced for `exec`, not just for
            // its working directory.
            if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                let initial_cwd = args
                    .get("working_dir")
                    .and_then(|value| value.as_str())
                    .map(|path| Some(PathBuf::from(path)));
                let cwd = infer_command_cwd(cmd, &initial_cwd).or_else(|| {
                    if command_mutates_cwd(cmd) {
                        Some(None)
                    } else {
                        initial_cwd
                    }
                });
                let cmd = expand_known_pwd(cmd, &cwd);
                accesses.extend(
                    extract_command_paths(&cmd)
                        .into_iter()
                        .map(|path| PathAccess {
                            path: resolve_shell_access_path(&path, &cwd),
                            op: PathOp::Exec,
                        }),
                );
            }
        }
        "shell" => {
            let shell_action = args.get("action").and_then(|v| v.as_str()).unwrap_or("run");
            let session_id = args.get("session_id").and_then(|v| v.as_str());
            let cmd_opt = match shell_action {
                "run" => args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                "write_stdin" => args.get("input").and_then(|v| v.as_str()).map(|input| {
                    blockcell_tools::shell_session::preview_session_stdin(
                        session_id.unwrap_or_default(),
                        input,
                    )
                }),
                _ => None,
            };
            if let Some(cmd) = cmd_opt {
                // Persistent sessions execute in their own cwd, not the
                // workspace — relative paths must resolve against the
                // session's tracked cwd. Unknown cwd (cd target could not
                // be resolved statically) fails closed via a synthetic
                // external path that policy treats as Confirm. A command
                // that itself changes directory mid-string cannot be
                // mapped to a single cwd — treat relative paths as unknown.
                let initial_cwd = session_id.and_then(blockcell_tools::shell_session::session_cwd);
                let session_cwd = infer_command_cwd(&cmd, &initial_cwd).or_else(|| {
                    if command_mutates_cwd(&cmd) {
                        Some(None)
                    } else {
                        initial_cwd
                    }
                });
                let cmd = expand_known_pwd(&cmd, &session_cwd);
                accesses.extend(
                    extract_command_paths(&cmd)
                        .into_iter()
                        .map(|path| PathAccess {
                            path: resolve_shell_access_path(&path, &session_cwd),
                            op: PathOp::Exec,
                        }),
                );
            }
        }
        _ => {}
    }
    accesses
}

impl super::AgentRuntime {
    /// Resolve a path string the same way tools do (expand ~ and relative paths).
    pub(super) fn resolve_path(&self, path_str: &str) -> PathBuf {
        if path_str.starts_with("~/") {
            dirs::home_dir()
                .map(|h| h.join(&path_str[2..]))
                .unwrap_or_else(|| PathBuf::from(path_str))
        } else if Path::new(path_str).is_absolute() {
            PathBuf::from(path_str)
        } else {
            self.paths.workspace().join(path_str)
        }
    }

    /// Check if a resolved path is inside the safe workspace directory.
    pub(super) fn is_path_safe(&self, resolved: &std::path::Path) -> bool {
        if is_path_within_base(&self.paths.workspace(), resolved) {
            return true;
        }
        let canonical = resolved
            .canonicalize()
            .unwrap_or_else(|_| resolved.to_path_buf());
        let temp_dir = std::env::temp_dir();
        let temp_dir = temp_dir.canonicalize().unwrap_or(temp_dir);
        if is_path_within_base(&temp_dir, &canonical) {
            return true;
        }
        #[cfg(unix)]
        {
            let tmp = std::path::Path::new("/tmp");
            tmp.exists() && is_path_within_base(tmp, &canonical)
        }
        #[cfg(not(unix))]
        false
    }

    /// Check whether a resolved path falls within a directory authorized in
    /// `session_key`. Authorizations are scoped to the session that granted
    /// them: the runtime is shared across sessions (and users, in gateway
    /// mode), so an entry from one session must not leak into another.
    /// Optimized (#12): walk ancestors with O(1) HashSet lookups instead of O(n) iteration.
    /// `authorized_dirs` stores already-canonicalized paths, so no re-canonicalization needed.
    pub(super) fn is_path_authorized(
        &self,
        resolved: &std::path::Path,
        op: PathOp,
        session_key: &str,
    ) -> bool {
        let authorized_dirs = self
            .authorized_dirs
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if authorized_dirs.is_empty() {
            return false;
        }
        let session_key = session_key.to_string();
        let rp = resolve_for_policy(resolved);
        let mut current = rp.as_path();
        loop {
            if authorized_dirs.contains(&(session_key.clone(), current.to_path_buf(), op)) {
                return true;
            }
            match current.parent() {
                Some(parent) if parent != current => current = parent,
                _ => return false,
            }
        }
    }

    /// Record a directory as authorized for `session_key` so future accesses
    /// within it are auto-approved for that session only.
    pub(super) fn authorize_directory(
        &mut self,
        resolved: &std::path::Path,
        op: PathOp,
        session_key: &str,
    ) {
        // If the path is a directory, authorize it directly.
        // If it's a file, authorize its parent directory.
        let dir = if resolved.is_dir() {
            resolved.to_path_buf()
        } else {
            resolved
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| resolved.to_path_buf())
        };
        let dir = resolve_for_policy(&dir);
        // A filesystem-root entry would exempt every path for `op` in this
        // session; the single access was already approved, just don't persist it.
        if dir.parent().is_none() {
            return;
        }
        let mut authorized_dirs = self
            .authorized_dirs
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let inserted = authorized_dirs.insert((session_key.to_string(), dir.clone(), op));
        if inserted {
            info!(dir = %dir.display(), ?op, "Directory authorized for future access");
        }
        // Entries accumulate across sessions on a shared runtime; once the set
        // exceeds the bound, keep only the current session's authorizations —
        // fail-closed, so stale entries require re-confirmation.
        const MAX_AUTHORIZED_DIRS: usize = 256;
        if authorized_dirs.len() > MAX_AUTHORIZED_DIRS {
            authorized_dirs.retain(|(key, _, _)| key == session_key);
        }
    }

    /// For tools that access the filesystem, check if any paths are outside the
    /// workspace. Applies the path-access policy first; only paths whose policy
    /// outcome is `Confirm` are forwarded to the user for interactive approval.
    ///
    /// Priority (highest → lowest):
    /// 1. Workspace-safe paths  → always allowed
    /// 2. Session-authorized dirs → allowed (cached from prior confirmation)
    /// 3. Policy `Deny`         → rejected immediately, no confirmation sent
    /// 4. Policy `Allow`        → allowed immediately, cached for this session
    /// 5. Policy `Confirm`      → user confirmation required
    #[cfg(test)]
    pub(super) async fn check_path_permission(
        &mut self,
        tool_name: &str,
        args: &serde_json::Value,
        msg: &InboundMessage,
    ) -> bool {
        self.check_path_permission_with_confirmation(tool_name, args, msg, false, false)
            .await
            .is_ok()
    }

    /// `policy_confirmed`：本次调用已经过用户确认（Policy/DangerousOperation 阶段批准），
    /// `Confirm` 级路径直接放行不再询问。
    /// `confirmed_may_cache`：仅当确认为"策略级确认"（`policy_confirmed`）时才把
    /// `Confirm` 路径写入会话授权缓存；危险命令批准（`dangerous_confirmed`）作用域
    /// 仅限本次调用，不得扩大为本会话授权。
    pub(super) async fn check_path_permission_with_confirmation(
        &mut self,
        tool_name: &str,
        args: &serde_json::Value,
        msg: &InboundMessage,
        policy_confirmed: bool,
        confirmed_may_cache: bool,
    ) -> Result<(), String> {
        if matches!(tool_name, "exec_local" | "exec_skill_script") {
            // These run scripts addressed relative to the active skill
            // directory, so the generic workspace policy doesn't apply. Still
            // enforce — at the runtime layer — that the script path is a safe
            // relative path that cannot escape the skill scope.
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if !is_safe_relative_skill_path(path) {
                warn!(
                    tool = tool_name,
                    path, "Rejecting exec script path that escapes skill scope"
                );
                return Err("script path escapes the active skill directory".to_string());
            }
            return Ok(());
        }
        let mut effective_args = args.clone();
        if matches!(tool_name, "shell" | "shell_run")
            && effective_args
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("run")
                == "run"
            && effective_args
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .is_none()
        {
            effective_args["session_id"] =
                serde_json::Value::String(format!("default-{}", msg.session_key()));
        }
        let args = &effective_args;
        let path_accesses = self.extract_path_accesses(tool_name, args);
        if path_accesses.is_empty() {
            return Ok(());
        }
        let session_key = persist_session_key_for(msg);

        // Classify each path by policy outcome
        let mut deny_paths: Vec<String> = Vec::new();
        let mut confirm_paths: Vec<PathAccess> = Vec::new();
        // Directories are only cached once the *whole* operation is approved:
        // caching inside the loop left grants behind when a later path caused
        // the call to be rejected.
        let mut cacheable: Vec<PathAccess> = Vec::new();

        for access in &path_accesses {
            let resolved = self.resolve_path(&access.path);

            // Hard path-policy denial always wins, including over workspace-internal
            // paths, prior session authorization, and ToolPolicy `ask` approval.
            let action = self.path_policy.evaluate(&resolved, access.op);
            if action == PolicyAction::Deny {
                warn!(
                    tool = tool_name,
                    path = %resolved.display(),
                    "Path access denied by policy"
                );
                deny_paths.push(access.path.clone());
                continue;
            }

            // 1. Workspace-safe → always OK (is_path_safe canonicalizes via
            // resolve_for_policy, so symlink escapes out of workspace are rejected).
            if self.is_path_safe(&resolved) {
                continue;
            }

            // 2. Already authorized by user this session → OK
            if self.is_path_authorized(&resolved, access.op, &session_key) {
                continue;
            }

            // 3. Apply the non-deny policy outcome
            match action {
                PolicyAction::Deny => unreachable!("deny handled before authorization checks"),
                PolicyAction::Allow => {
                    // Policy explicitly allows — cache for this session
                    info!(
                        tool = tool_name,
                        path = %resolved.display(),
                        "Path access allowed by policy"
                    );
                    if self.path_policy.cache_confirmed_dirs() {
                        cacheable.push(access.clone());
                    }
                }
                PolicyAction::Confirm => {
                    if policy_confirmed {
                        if confirmed_may_cache && self.path_policy.cache_confirmed_dirs() {
                            cacheable.push(access.clone());
                        }
                    } else {
                        confirm_paths.push(access.clone());
                    }
                }
            }
        }

        // Any hard-deny → reject the whole operation
        if !deny_paths.is_empty() {
            return Err(format!(
                "path access denied by policy: {}",
                deny_paths.join(", ")
            ));
        }

        // All paths were allowed (workspace / session-cache / policy-allow)
        if confirm_paths.is_empty() {
            self.cache_authorized_paths(&cacheable, &session_key);
            return Ok(());
        }

        // Need user confirmation for the remaining paths
        if let Some(confirm_tx) = &self.confirm_tx {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let request = ConfirmRequest {
                tool_name: tool_name.to_string(),
                paths: confirm_paths
                    .iter()
                    .map(|access| access.path.clone())
                    .collect(),
                response_tx,
                agent_id: self.agent_id.clone(),
                channel: msg.channel.clone(),
                account_id: msg.account_id.clone(),
                sender_id: msg.sender_id.clone(),
                chat_id: msg.chat_id.clone(),
                ws_connection_id: msg
                    .metadata
                    .get("ws_connection_id")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string()),
            };

            if !matches!(
                tokio::time::timeout(CONFIRM_REQUEST_TIMEOUT, confirm_tx.send(request)).await,
                Ok(Ok(()))
            ) {
                warn!("Failed to send confirmation request, denying access");
                return Err("path confirmation request could not be delivered".to_string());
            }

            match tokio::time::timeout(CONFIRM_REQUEST_TIMEOUT, response_rx).await {
                Ok(Ok(true)) => {
                    cacheable.extend(confirm_paths.iter().cloned());
                    self.cache_authorized_paths(&cacheable, &session_key);
                    Ok(())
                }
                // Report which paths were refused: without them the model cannot
                // tell a real escape attempt from a heuristic false positive (for
                // example zip member names inside an inline script) and gives up.
                Ok(Ok(false)) => Err(denied_paths_payload(
                    "path access was denied by the user",
                    "PATH_ACCESS_DENIED",
                    &confirm_paths,
                    &self.paths.workspace(),
                )),
                Ok(Err(_)) => {
                    warn!("Confirmation channel closed, denying access");
                    Err(denied_paths_payload(
                        "path confirmation channel closed",
                        "PATH_CONFIRMATION_UNAVAILABLE",
                        &confirm_paths,
                        &self.paths.workspace(),
                    ))
                }
                Err(_) => {
                    warn!("Confirmation request timed out, denying access");
                    Err(denied_paths_payload(
                        "path confirmation timed out",
                        "PATH_CONFIRMATION_TIMEOUT",
                        &confirm_paths,
                        &self.paths.workspace(),
                    ))
                }
            }
        } else {
            warn!(
                tool = tool_name,
                "No confirmation channel, denying access to paths outside workspace"
            );
            // Explain conservative static analysis without weakening the guard. The
            // sentinel is diagnostic metadata, never a path the model should create.
            let unresolved = confirm_paths.iter().any(|access| {
                access.path.starts_with("/__unresolved_shell_var__/")
                    || access.path.starts_with("/__unknown_shell_cwd__/")
            });
            let mut alternative = String::from(
                "Only this invocation was rejected; the tool remains available. Do not use tool_search or switch to another tool to get around path policy. For test fixtures, use literal workspace paths such as ./.tmp/src, ./.tmp/out and ./.tmp/missing (leave missing absent for negative tests). Retry the corrected invocation with the same tool. Do not retry denied access or bypass policy/user denials.",
            );
            if unresolved {
                // 这里的拒绝不是"越权"，是"解析不出来"。原文案统一劝"不要改写脚本"，
                // 对这种情形正好相反：改写命令（换成字面路径）就是预期动作。
                alternative.push_str(" This invocation contained a shell variable, substitution, or an unknown working directory that static validation cannot resolve, so it could not be proven to stay inside the workspace. The expected fix is to rewrite the command with literal absolute paths (no $VAR, no $(...), no bare ~) and retry it with the same tool. If you only need to inspect or produce files, prefer the file tools (read_file/write_file/glob) over shell interpolation.");
            }
            Err(serde_json::json!({
                "error": "path requires confirmation but no confirmation channel is available",
                "code": "NEEDS_CONFIRMATION_NONINTERACTIVE",
                "paths": confirm_paths.iter().map(|access| access.path.clone()).collect::<Vec<_>>(),
                "scope": "invocation",
                "tool_available": true,
                "workspace": self.paths.workspace().display().to_string(),
                "unresolved_expressions": unresolved,
                "alternative": alternative
            })
            .to_string())
        }
    }

    /// Cache directory grants after the whole operation has been approved.
    fn cache_authorized_paths(&mut self, accesses: &[PathAccess], session_key: &str) {
        if !self.path_policy.cache_confirmed_dirs() {
            return;
        }
        for access in accesses {
            let resolved = self.resolve_path(&access.path);
            self.authorize_directory(&resolved, access.op, session_key);
        }
    }

    pub(super) async fn confirm_dangerous_operation(
        &mut self,
        tool_name: &str,
        items: Vec<String>,
        msg: &InboundMessage,
    ) -> bool {
        if items.is_empty() {
            return true;
        }
        if let Some(confirm_tx) = &self.confirm_tx {
            let (response_tx, response_rx) = tokio::sync::oneshot::channel();
            let request = ConfirmRequest {
                tool_name: tool_name.to_string(),
                paths: items,
                response_tx,
                agent_id: self.agent_id.clone(),
                channel: msg.channel.clone(),
                account_id: msg.account_id.clone(),
                sender_id: msg.sender_id.clone(),
                chat_id: msg.chat_id.clone(),
                ws_connection_id: msg
                    .metadata
                    .get("ws_connection_id")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string()),
            };
            if !matches!(
                tokio::time::timeout(CONFIRM_REQUEST_TIMEOUT, confirm_tx.send(request)).await,
                Ok(Ok(()))
            ) {
                warn!(
                    tool = tool_name,
                    "Failed to send dangerous-operation confirmation request, denying"
                );
                return false;
            }
            match tokio::time::timeout(CONFIRM_REQUEST_TIMEOUT, response_rx).await {
                Ok(Ok(allowed)) => allowed,
                Ok(Err(_)) => {
                    warn!(
                        tool = tool_name,
                        "Dangerous-operation confirmation channel closed, denying"
                    );
                    false
                }
                Err(_) => {
                    warn!(
                        tool = tool_name,
                        "Dangerous-operation confirmation timed out, denying"
                    );
                    false
                }
            }
        } else {
            warn!(
                tool = tool_name,
                "No confirmation channel, denying dangerous operation"
            );
            false
        }
    }
}

/// Heuristically extract filesystem-path-looking tokens from a shell command.
///
/// Used to subject paths referenced inside an `exec` / `shell` command to the
/// path policy. This is intentionally conservative: when in doubt a token is
/// treated as a path, so the policy (deny / confirm) gets a chance to run.
/// 例外：bare `$VAR` 只在"以路径为参数"的命令上下文中才按疑似路径处理，
/// 避免 `git commit -m "$MSG"` 这类纯文本变量被误报为路径。
/// Structured denial that names the refused paths.
///
/// The path guard is a conservative static heuristic, so a denial can also be a
/// false positive (a literal such as `/xl/worksheets/sheet1.xml` inside an inline
/// script). Returning the paths lets the model correct the command instead of
/// abandoning the task.
fn denied_paths_payload(
    error: &str,
    code: &str,
    paths: &[PathAccess],
    workspace: &std::path::Path,
) -> String {
    serde_json::json!({
        "error": error,
        "code": code,
        "paths": paths.iter().map(|access| access.path.clone()).collect::<Vec<_>>(),
        "scope": "invocation",
        "tool_available": true,
        "workspace": workspace.display().to_string(),
        "alternative": "Only this invocation was rejected; the tool and unrelated authorized steps remain available. Keep and use files and facts already discovered. If a listed path is not a real access target (for example a zip member name, URL path or regex inside an inline script), rewrite the command without the leading slash or move that literal out of the command, then retry the same tool. Do not bypass policy or user denials.",
    })
    .to_string()
}

fn extract_command_paths(command: &str) -> Vec<String> {
    let (command_without_heredoc_bodies, heredoc_bodies) = split_heredoc_bodies(command);
    let path_verb_present = command_has_path_consuming_verb(&command_without_heredoc_bodies);
    let mut out = Vec::new();
    for raw in command_without_heredoc_bodies.split_whitespace() {
        let single_quoted = raw.starts_with('\'');
        let pieces: Vec<&str> = if !raw.contains("://") && (raw.contains('<') || raw.contains('>'))
        {
            raw.split(['<', '>'])
                .filter(|part| !part.is_empty())
                .collect()
        } else {
            vec![raw]
        };

        for piece in pieces {
            let mut candidates = Vec::new();
            let trimmed =
                piece.trim_matches(|c: char| matches!(c, '"' | '\'' | ';' | ',' | ')' | '(' | '&'));
            if trimmed.is_empty() {
                continue;
            }

            // Handle flags carrying path values like `-d@/etc/passwd` or `--file=/etc/passwd`
            if let Some(rest) = trimmed.strip_prefix("-d@") {
                candidates.push(rest);
            } else if trimmed.starts_with('-') {
                if let Some((_, val)) = trimmed.split_once('=') {
                    candidates.push(val.trim_matches(|c| c == '"' || c == '\''));
                }
            } else if let Some((_, val)) = trimmed.split_once('=') {
                // `dd if=/etc/shadow` or `FOO=/etc/passwd`
                candidates.push(val.trim_matches(|c| c == '"' || c == '\''));
            } else {
                candidates.push(trimmed);
            }

            for cand in candidates {
                let tok = cand
                    .trim_start_matches(['@', '>', '<', '&'])
                    .trim_matches(|c: char| c == '"' || c == '\'');
                if tok.is_empty() || tok.starts_with('-') || tok.contains("://") {
                    continue;
                }
                let normalized = normalize_shell_home_vars(tok);
                let has_shell_expansion =
                    !single_quoted && (normalized.contains('$') || normalized.contains('`'));
                // 展开形式只在仍像路径时才触发确认：含 `/` 的组合路径、命令替换
                // （`$(...)`、反引号），或出现在吃路径命令中的裸 `$VAR`。
                // 纯文本变量（如 `git commit -m "$MSG"` 的 `$MSG`）不再误报。
                let expansion_looks_like_path = has_shell_expansion
                    && (normalized.contains('/')
                        || normalized.contains("$(")
                        || normalized.contains('`')
                        || (is_bare_shell_var(&normalized) && path_verb_present));
                let looks_like_path = normalized.starts_with('/')
                    || normalized.starts_with("~/")
                    || normalized == "~"
                    || normalized.starts_with("./")
                    || normalized.starts_with("../")
                    || normalized == ".."
                    || (normalized.contains('/') && !normalized.contains('='))
                    || expansion_looks_like_path;
                if looks_like_path {
                    // If an unresolved shell variable or command substitution appears in a
                    // token (e.g. `$FILE`, `$FOO/secret`, or `$(pwd)/x`), never let
                    // `resolve_path` join it under workspace and treat it as workspace-safe;
                    // anchor it as an external synthetic path so PathPolicy requires confirmation.
                    let entry = if has_shell_expansion {
                        format!(
                            "/__unresolved_shell_var__/{}",
                            normalized.trim_start_matches('/')
                        )
                    } else {
                        normalized
                    };
                    if !is_pseudo_device_path(&entry) && !out.contains(&entry) {
                        out.push(entry);
                    }
                }
            }
        }
    }

    // Also extract quoted absolute / home / escaping relative paths embedded in inline scripts
    // (e.g. `python3 -c "open('/etc/passwd').read()"` or `python3 -c "open('../secret').read()"`).
    extract_quoted_inline_paths(&command_without_heredoc_bodies, true, &mut out);
    for (body, quoted_delimiter) in heredoc_bodies {
        extract_quoted_inline_paths(&body, !quoted_delimiter, &mut out);
    }

    out
}

fn split_heredoc_bodies(command: &str) -> (String, Vec<(String, bool)>) {
    let mut command_without_bodies = String::new();
    let mut bodies = Vec::new();
    let mut pending_delimiters = Vec::new();
    let mut active_body: Option<(String, bool, String)> = None;

    for line in command.split_inclusive('\n') {
        if let Some((delimiter, quoted, mut body)) = active_body.take() {
            if line.trim() == delimiter {
                bodies.push((body, quoted));
            } else {
                body.push_str(line);
                active_body = Some((delimiter, quoted, body));
            }
            continue;
        }

        if let Some((delimiter, quoted)) = pending_delimiters.first().cloned() {
            pending_delimiters.remove(0);
            if line.trim() == delimiter {
                bodies.push((String::new(), quoted));
            } else {
                active_body = Some((delimiter, quoted, line.to_string()));
            }
            continue;
        }

        command_without_bodies.push_str(line);
        pending_delimiters.extend(heredoc_delimiters_in_line(line));
    }

    if let Some((_, quoted, body)) = active_body {
        bodies.push((body, quoted));
    }

    (command_without_bodies, bodies)
}

fn heredoc_delimiters_in_line(line: &str) -> Vec<(String, bool)> {
    let chars: Vec<char> = line.chars().collect();
    let mut delimiters = Vec::new();
    let mut index = 0;

    while index + 1 < chars.len() {
        if chars[index] != '<' || chars[index + 1] != '<' {
            index += 1;
            continue;
        }
        if chars.get(index + 2) == Some(&'<') {
            index += 3;
            continue;
        }

        index += 2;
        if chars.get(index) == Some(&'-') {
            index += 1;
        }
        while chars
            .get(index)
            .is_some_and(|character| character.is_whitespace())
        {
            index += 1;
        }

        let quote = chars
            .get(index)
            .copied()
            .filter(|character| matches!(character, '\'' | '"'));
        if quote.is_some() {
            index += 1;
        }
        let start = index;
        while let Some(character) = chars.get(index) {
            if Some(*character) == quote
                || (quote.is_none()
                    && (character.is_whitespace()
                        || matches!(character, ';' | '|' | '&' | '<' | '>')))
            {
                break;
            }
            index += 1;
        }
        if start < index {
            delimiters.push((chars[start..index].iter().collect(), quote.is_some()));
        }
        if quote.is_some() && chars.get(index) == quote.as_ref() {
            index += 1;
        }
    }

    delimiters
}

fn extract_quoted_inline_paths(text: &str, allow_shell_expansion: bool, out: &mut Vec<String>) {
    for segment in text.split(['\'', '"']) {
        let seg = normalize_shell_home_vars(segment.trim());
        let is_inline_path = seg.starts_with('/')
            || seg.starts_with("~/")
            || seg.starts_with("../")
            || seg.starts_with("./")
            || seg == "..";
        if is_inline_path && !seg.contains(' ') && !seg.contains("://") && seg.len() > 1 {
            let entry = if allow_shell_expansion && (seg.contains('$') || seg.contains('`')) {
                format!("/__unresolved_shell_var__/{}", seg.trim_start_matches('/'))
            } else {
                seg
            };
            if !is_pseudo_device_path(&entry) && !out.contains(&entry) {
                out.push(entry);
            }
        }
    }
}

fn is_pseudo_device_path(path: &str) -> bool {
    matches!(
        path,
        "/dev/null"
            | "/dev/zero"
            | "/dev/stdout"
            | "/dev/stderr"
            | "/dev/stdin"
            | "/dev/tty"
            | "/dev/fd"
    ) || path.starts_with("/dev/fd/")
}

/// token 是否就是单独一个 `$VAR` / `${VAR}`（不含其他字符）。
fn is_bare_shell_var(tok: &str) -> bool {
    let name = tok
        .strip_prefix("${")
        .and_then(|s| s.strip_suffix('}'))
        .or_else(|| tok.strip_prefix('$'));
    name.is_some_and(|n| {
        !n.is_empty()
            && !n.starts_with(|c: char| c.is_ascii_digit())
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// 命令中是否出现"以路径为参数"的常见命令名。用于区分 `cat $FILE`
/// （`$FILE` 可能是路径，需要确认）与 `git commit -m "$MSG"`（`$MSG` 只是文本）。
/// 这是启发式的防线收窄——带 `/` 或命令替换的展开仍始终按路径处理。
fn command_has_path_consuming_verb(command: &str) -> bool {
    const PATH_VERBS: &[&str] = &[
        "cat",
        "cp",
        "mv",
        "rm",
        "head",
        "tail",
        "less",
        "more",
        "dd",
        "chmod",
        "chown",
        "chgrp",
        "ln",
        "tar",
        "touch",
        "mkdir",
        "rmdir",
        "stat",
        "file",
        "readlink",
        "realpath",
        "ls",
        "cd",
        "source",
        "diff",
        "sort",
        "wc",
        "sed",
        "awk",
        "grep",
        "zip",
        "unzip",
        "gzip",
        "gunzip",
        "xz",
        "install",
        "rsync",
        "scp",
        "gpg",
        "bash",
        "sh",
        "zsh",
        "python",
        "python3",
        "node",
        "ruby",
        "perl",
        "vi",
        "vim",
        "nano",
        "jq",
        "xxd",
        "od",
        "hexdump",
        "cmp",
        "truncate",
        "find",
        "xargs",
        "md5",
        "sha256sum",
        "cksum",
    ];
    command.split_whitespace().any(|tok| {
        let base = tok
            .trim_matches(|c| c == '"' || c == '\'')
            .rsplit('/')
            .next()
            .unwrap_or_default();
        PATH_VERBS.contains(&base)
    })
}

/// Resolve a relative path extracted from a shell command against the
/// persistent session's tracked cwd. `session_cwd` is the triple state from
/// `shell_session::session_cwd`: `None` = no session record (new session runs
/// in the workspace, so the path stays relative and the caller resolves it
/// against the workspace as before); `Some(Some(cwd))` = tracked cwd;
/// `Some(None)` = unknown cwd — fail closed via a synthetic absolute path
/// outside the workspace so policy cannot treat it as an allowed workspace
/// file.
fn resolve_shell_access_path(path: &str, session_cwd: &Option<Option<PathBuf>>) -> String {
    if path.starts_with('/') || path.starts_with("~/") {
        return path.to_string();
    }
    match session_cwd {
        Some(Some(cwd)) => cwd.join(path).to_string_lossy().to_string(),
        Some(None) => {
            // 未知 cwd 下相对深度不可表达——把 `..` 中和为普通组件，
            // 否则词法归一化会让路径爬出哨兵根伪装成根目录下的文件。
            let neutralized = path.replace("..", "__up__");
            format!(
                "/__unknown_shell_cwd__/{}",
                neutralized.trim_start_matches('/')
            )
        }
        None => path.to_string(),
    }
}

pub(super) fn extract_literal_shell_read_paths(
    command: &str,
    initial_cwd: &Option<PathBuf>,
) -> Vec<String> {
    // This is evidence for the read-before-write guard, not general shell
    // analysis. Decline syntax whose execution or operands we cannot prove.
    if command
        .chars()
        .any(|c| "\n\r;&|><$`\\(){}*?[]~".contains(c))
    {
        return Vec::new();
    }
    let tokens: Vec<_> = command.split_whitespace().collect();
    let Some(program) = tokens.first() else {
        return Vec::new();
    };
    let program = program
        .strip_prefix("/usr/bin/")
        .or_else(|| program.strip_prefix("/bin/"))
        .unwrap_or(program);
    if !matches!(program, "cat" | "head" | "tail" | "nl" | "sed") {
        return Vec::new();
    }
    let mut index = 1;
    if program == "sed" {
        // Only the common printing form is recognized. Other sed scripts can
        // execute commands, read extra files, or suppress the actual input.
        if tokens.get(index) != Some(&"-n") {
            return Vec::new();
        }
        index += 1;
        let Some(expression) = tokens.get(index) else {
            return Vec::new();
        };
        let expression = expression.trim_matches(['\'', '"']);
        let Some(range) = expression.strip_suffix('p') else {
            return Vec::new();
        };
        if range.is_empty() || !range.chars().all(|c| c.is_ascii_digit() || c == ',') {
            return Vec::new();
        }
        index += 1;
    } else {
        while let Some(option) = tokens.get(index).filter(|token| token.starts_with('-')) {
            if *option == "--" {
                index += 1;
                break;
            }
            let known = match program {
                "cat" => matches!(*option, "-n" | "-b" | "-s" | "-v" | "-E" | "-T" | "-A"),
                "nl" => false,
                "head" | "tail" => matches!(*option, "-n" | "-c"),
                _ => false,
            };
            if !known {
                return Vec::new();
            }
            index += 1;
            if matches!(program, "head" | "tail") {
                let Some(count) = tokens.get(index) else {
                    return Vec::new();
                };
                if count.is_empty() || !count.chars().all(|c| c.is_ascii_digit()) {
                    return Vec::new();
                }
                index += 1;
            }
        }
    }
    let cwd = initial_cwd.clone().map(Some);
    let mut paths = Vec::new();
    for token in &tokens[index..] {
        if token.len() < 2 && matches!(*token, "\'" | "\"") {
            return Vec::new();
        }
        let token = if (token.starts_with('\'') && token.ends_with('\''))
            || (token.starts_with('"') && token.ends_with('"'))
        {
            &token[1..token.len() - 1]
        } else {
            token
        };
        if token.is_empty()
            || token.starts_with('-')
            || !token
                .chars()
                .all(|c| c.is_alphanumeric() || "_./-".contains(c))
        {
            return Vec::new();
        }
        let path = resolve_shell_access_path(token, &cwd);
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

fn infer_command_cwd(
    command: &str,
    initial_cwd: &Option<Option<PathBuf>>,
) -> Option<Option<PathBuf>> {
    let (before_and, _) = command.split_once("&&")?;
    let segment = before_and.trim();
    let rest = segment.strip_prefix("cd")?;
    if !rest.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let target = rest
        .trim()
        .trim_matches(|character| matches!(character, '"' | '\''));
    if target.is_empty()
        || target.split_whitespace().count() != 1
        || target.contains('$')
        || target.contains('`')
        || target == "-"
    {
        return None;
    }

    let target = PathBuf::from(target);
    if target.is_absolute() {
        return Some(Some(target));
    }

    match initial_cwd {
        Some(Some(cwd)) => Some(Some(cwd.join(target))),
        Some(None) => None,
        None => Some(Some(target)),
    }
}

fn expand_known_pwd(command: &str, cwd: &Option<Option<PathBuf>>) -> String {
    let Some(Some(cwd)) = cwd else {
        return command.to_string();
    };
    let cwd = if cwd.is_absolute() {
        cwd.to_string_lossy().into_owned()
    } else {
        ".".to_string()
    };
    command
        .replace("$(pwd -P)", &cwd)
        .replace("$(pwd)", &cwd)
        .replace("`pwd`", &cwd)
        .replace("${PWD}", &cwd)
        .replace("$PWD", &cwd)
}

/// Whether a command string itself contains a `cd`/`pushd`/`popd` that could
/// change the persistent session's cwd mid-command. Conservative: also treats
/// subshell/grouping contexts as mutating, since a `cd` inside them does not
/// affect the parent but its effect on ordering can't be tracked cheaply.
fn command_mutates_cwd(command: &str) -> bool {
    command.split(['\n', ';', '&', '|']).any(|segment| {
        let seg = segment.trim();
        let mut tokens = seg.split_whitespace();
        let head = tokens
            .next()
            .unwrap_or("")
            .trim_start_matches(['!', '{', '('])
            .trim_end_matches(')');
        matches!(head, "cd" | "pushd" | "popd")
            || (matches!(
                head,
                "if" | "then"
                    | "else"
                    | "elif"
                    | "while"
                    | "until"
                    | "do"
                    | "sudo"
                    | "env"
                    | "command"
                    | "builtin"
                    | "eval"
                    | "time"
                    | "nohup"
            ) && tokens.any(|t| {
                matches!(
                    t.trim_matches(|c| matches!(c, '"' | '\'' | ';' | '(' | ')' | '{' | '}')),
                    "cd" | "pushd" | "popd"
                )
            }))
    })
}

fn normalize_shell_home_vars(tok: &str) -> String {
    if tok == "$HOME" || tok == "${HOME}" {
        "~".to_string()
    } else if let Some(rest) = tok.strip_prefix("$HOME/") {
        format!("~/{rest}")
    } else if let Some(rest) = tok.strip_prefix("${HOME}/") {
        format!("~/{rest}")
    } else {
        tok.to_string()
    }
}

/// Whether `path` is a non-empty relative path that stays inside the active
/// skill directory (no absolute paths, no `..` traversal). Mirrors the
/// `exec_local`/`exec_skill_script` tools' own validation as defense in depth.
fn is_safe_relative_skill_path(path: &str) -> bool {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return false;
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return false;
    }
    !candidate.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{
        command_mutates_cwd, denied_paths_payload, extract_command_paths,
        extract_literal_shell_read_paths, extract_path_accesses, is_safe_relative_skill_path,
        resolve_shell_access_path, PathAccess,
    };
    use blockcell_core::path_policy::PathOp;
    use serde_json::json;
    use std::path::PathBuf;

    /// Table-driven coverage for the tool → path-field mapping (P3-022).
    ///
    /// A missing branch here does not fail loudly: the tool's path simply never
    /// reaches the path policy, so writes escape the workspace silently. The
    /// `http_request.save_to` case below is a real instance of that bug.
    #[test]
    fn path_extraction_table_covers_tool_field_mapping() {
        type ExtractionCase<'a> = (&'a str, serde_json::Value, Vec<(&'a str, PathOp)>);
        let cases: Vec<ExtractionCase> = vec![
            (
                "read_file",
                json!({"path": "a.txt"}),
                vec![("a.txt", PathOp::Read)],
            ),
            (
                "write_file",
                json!({"path": "~/x.txt"}),
                vec![("~/x.txt", PathOp::Write)],
            ),
            (
                "edit_file",
                json!({"path": "src/main.rs"}),
                vec![("src/main.rs", PathOp::Write)],
            ),
            ("list_dir", json!({"path": "."}), vec![(".", PathOp::List)]),
            (
                "file_ops",
                json!({"action": "copy", "path": "a", "destination": "b"}),
                vec![("a", PathOp::Read), ("b", PathOp::Write)],
            ),
            (
                "file_ops",
                json!({"action": "delete", "path": "a", "destination": "b"}),
                vec![("a", PathOp::Write), ("b", PathOp::Write)],
            ),
            (
                "data_process",
                json!({"action": "write_csv", "path": "in.csv", "output_path": "out.csv"}),
                vec![("in.csv", PathOp::Write), ("out.csv", PathOp::Write)],
            ),
            (
                "video_process",
                json!({"input": "a.mp4", "inputs": ["b.mp4"], "output_path": "o.mp4"}),
                vec![
                    ("a.mp4", PathOp::Read),
                    ("b.mp4", PathOp::Read),
                    ("o.mp4", PathOp::Write),
                ],
            ),
            (
                "http_request",
                json!({"save_to": "resp.bin"}),
                vec![("resp.bin", PathOp::Write)],
            ),
            (
                "message",
                json!({"media": ["a.png", "b.png"]}),
                vec![("a.png", PathOp::Read), ("b.png", PathOp::Read)],
            ),
            (
                "exec",
                json!({"working_dir": "/tmp"}),
                vec![("/tmp", PathOp::Exec)],
            ),
            ("totally_unknown_tool", json!({"path": "a"}), vec![]),
        ];

        for (tool, args, expected) in cases {
            let expected: Vec<PathAccess> = expected
                .into_iter()
                .map(|(path, op)| PathAccess {
                    path: path.to_string(),
                    op,
                })
                .collect();
            assert_eq!(
                extract_path_accesses(tool, &args),
                expected,
                "unexpected path accesses for `{tool}` with {args}"
            );
        }
    }

    /// A field that is present but not a string must be ignored, not treated as
    /// an empty path (which would resolve to the workspace root and authorise it).
    #[test]
    fn path_extraction_ignores_non_string_fields() {
        assert!(extract_path_accesses("read_file", &json!({"path": 42})).is_empty());
        assert_eq!(
            extract_path_accesses("read_file", &json!({"paths": ["a.txt", "b.txt"]})).len(),
            2
        );
        assert_eq!(
            extract_path_accesses(
                "write_file",
                &json!({"files":[
                    {"path":"a.txt","content":"a"},
                    {"path":"b.txt","content":"b"}
                ]})
            ),
            vec![
                PathAccess {
                    path: "a.txt".to_string(),
                    op: PathOp::Write
                },
                PathAccess {
                    path: "b.txt".to_string(),
                    op: PathOp::Write
                }
            ]
        );
        assert!(extract_path_accesses("message", &json!({"media": "not-an-array"})).is_empty());
        assert!(extract_path_accesses("message", &json!({"media": [1, 2]})).is_empty());
    }

    #[test]
    fn accepts_safe_relative_skill_paths() {
        for p in ["scripts/hello.sh", "main.py", "./run.sh", "a/b/c.py"] {
            assert!(is_safe_relative_skill_path(p), "expected `{p}` accepted");
        }
    }

    #[test]
    fn rejects_absolute_or_escaping_skill_paths() {
        for p in [
            "",
            "   ",
            "/etc/passwd",
            "../secret.sh",
            "scripts/../../etc/passwd",
        ] {
            assert!(!is_safe_relative_skill_path(p), "expected `{p}` rejected");
        }
    }

    #[test]
    fn denial_names_the_refused_paths_and_how_to_recover() {
        // A conservative heuristic can flag literals such as zip member names
        // (`/xl/worksheets/sheet1.xml`). The denial must list them so the model can
        // rewrite the command instead of abandoning the task.
        let paths = vec![PathAccess {
            path: "/xl/worksheets/sheet1.xml".to_string(),
            op: PathOp::Read,
        }];
        let payload: serde_json::Value = serde_json::from_str(&denied_paths_payload(
            "path access was denied by the user",
            "PATH_ACCESS_DENIED",
            &paths,
            std::path::Path::new("/work"),
        ))
        .expect("denial payload must be JSON");

        assert_eq!(payload["code"], "PATH_ACCESS_DENIED");
        assert_eq!(payload["paths"][0], "/xl/worksheets/sheet1.xml");
        assert_eq!(payload["workspace"], "/work");
        let alternative = payload["alternative"].as_str().unwrap();
        assert!(alternative.contains("zip member name"), "{alternative}");
        assert!(alternative.contains("rewrite the command"), "{alternative}");
    }

    #[test]
    fn extracts_absolute_and_home_paths() {
        assert_eq!(
            extract_command_paths("cat /etc/shadow"),
            vec!["/etc/shadow".to_string()]
        );
        assert_eq!(
            extract_command_paths("cat ~/.ssh/id_rsa"),
            vec!["~/.ssh/id_rsa".to_string()]
        );
        assert_eq!(
            extract_command_paths("cp 'a' \"/etc/hosts\""),
            vec!["/etc/hosts".to_string()]
        );
    }

    #[test]
    fn extracts_relative_paths_with_slash() {
        assert_eq!(
            extract_command_paths("python src/main.py"),
            vec!["src/main.py".to_string()]
        );
        assert_eq!(
            extract_command_paths("ls ./build ../out"),
            vec!["./build".to_string(), "../out".to_string()]
        );
    }

    #[test]
    fn exec_command_paths_skip_pseudo_devices() {
        let accesses = extract_path_accesses("exec", &json!({"command": "cmd 2>/dev/null"}));
        assert!(
            accesses.iter().all(|access| access.path != "/dev/null"),
            "{accesses:?}"
        );
    }

    #[test]
    fn exec_relative_command_paths_resolve_against_working_dir() {
        let accesses = extract_path_accesses(
            "exec",
            &json!({"working_dir": "/w", "command": "cat sub/a.txt"}),
        );
        assert!(accesses.iter().any(|access| access.path == "/w/sub/a.txt"));
    }

    #[test]
    fn exec_paths_after_static_cd_resolve_against_target() {
        let accesses = extract_path_accesses("exec", &json!({"command": "cd /etc && cat x/y"}));
        assert!(
            accesses.iter().any(|access| access.path == "/etc/x/y"),
            "{accesses:?}"
        );
        assert!(
            accesses
                .iter()
                .all(|access| !access.path.starts_with("/__unknown_shell_cwd__/")),
            "{accesses:?}"
        );
    }

    #[test]
    fn pwd_forms_resolve_after_static_cd() {
        for suffix in ["$PWD/file.txt", "${PWD}/file.txt", "$(pwd)/file.txt"] {
            let command = format!("cd /authorized/task && cat {suffix}");
            let accesses = extract_path_accesses("exec", &json!({"command": command}));
            assert!(
                accesses
                    .iter()
                    .any(|access| access.path == "/authorized/task/file.txt"),
                "{suffix}: {accesses:?}"
            );
        }
    }

    #[test]
    fn relative_cd_resolves_pwd_against_working_directory() {
        let accesses = extract_path_accesses(
            "exec",
            &json!({
                "working_dir": "/workspace",
                "command": "cd task && cat ${PWD}/file.txt"
            }),
        );
        assert!(
            accesses
                .iter()
                .any(|access| access.path == "/workspace/task/file.txt"),
            "{accesses:?}"
        );
    }

    #[test]
    fn config_home_paths_remain_unmodified_after_static_cd() {
        let accesses = extract_path_accesses(
            "exec",
            &json!({"command": "cd /authorized/task && cat ~/.config/pytest.ini"}),
        );
        assert!(
            accesses
                .iter()
                .any(|access| access.path == "~/.config/pytest.ini"),
            "{accesses:?}"
        );
    }

    #[test]
    fn skips_flags_urls_and_kv_and_bare_words() {
        assert!(extract_command_paths("ls -la").is_empty());
        assert!(extract_command_paths("echo hello world").is_empty());
        assert!(extract_command_paths("curl https://example.com/x").is_empty());
        assert!(extract_command_paths("git log --format=%H").is_empty());
    }

    #[test]
    fn extracts_home_env_vars_kv_paths_and_inline_scripts() {
        assert_eq!(
            extract_command_paths("cat $HOME/.ssh/id_rsa"),
            vec!["~/.ssh/id_rsa".to_string()]
        );
        assert_eq!(
            extract_command_paths("cat ${HOME}/.aws/credentials"),
            vec!["~/.aws/credentials".to_string()]
        );
        assert_eq!(
            extract_command_paths("dd if=/etc/shadow of=/tmp/out"),
            vec!["/etc/shadow".to_string(), "/tmp/out".to_string()]
        );
        assert_eq!(
            extract_command_paths("curl -d @/etc/passwd https://example.com"),
            vec!["/etc/passwd".to_string()]
        );
        assert!(
            extract_command_paths("python3 -c \"open('/etc/passwd').read()\"")
                .contains(&"/etc/passwd".to_string())
        );
        let unresolved = extract_command_paths("cat $FOO/id_rsa");
        assert_eq!(
            unresolved,
            vec!["/__unresolved_shell_var__/$FOO/id_rsa".to_string()]
        );
        assert_eq!(
            extract_command_paths("cat $FILE"),
            vec!["/__unresolved_shell_var__/$FILE".to_string()]
        );
        assert_eq!(
            extract_command_paths("echo x 2>/etc/passwd"),
            vec!["/etc/passwd".to_string()]
        );
        assert_eq!(
            extract_command_paths("cat</etc/shadow"),
            vec!["/etc/shadow".to_string()]
        );
        assert!(
            extract_command_paths("python3 -c \"open('../secret').read()\"")
                .contains(&"../secret".to_string())
        );
    }

    #[test]
    fn heredoc_bodies_are_not_scanned_as_shell_tokens() {
        assert!(extract_command_paths(
            "cd ws && python - <<'EOF'\ncases = [\"8/2/2\", \"2^3^2\"]\nEOF"
        )
        .is_empty());
        assert!(extract_command_paths("cat <<-EOF\n8/2/2\n\tEOF").is_empty());
        assert!(extract_command_paths("cat <<\"EOF\"\n[not](a)\nEOF").is_empty());

        let markdown = "python - <<'EOF'\n[not](a)\n\n```\n[no](b)\n```\nEOF";
        let paths = extract_command_paths(markdown);
        assert!(
            paths
                .iter()
                .all(|path| !path.contains("unknown") && !path.contains("unresolved")),
            "{paths:?}"
        );
        assert!(paths.is_empty(), "{paths:?}");
    }

    #[test]
    fn single_quoted_shell_tokens_are_literal() {
        assert_eq!(
            extract_command_paths(r"printf '%s\n' 'text [a](u1)' '```' '[c](u3)' > /tmp/sample.md"),
            vec!["/tmp/sample.md".to_string()]
        );
    }

    #[test]
    fn heredoc_inline_paths_are_scanned_without_expanding_quoted_delimiters() {
        assert_eq!(
            extract_command_paths("python - <<'EOF'\nopen('/etc/passwd')\nEOF"),
            vec!["/etc/passwd".to_string()]
        );
        assert!(
            extract_command_paths("python - <<'EOF'\nopen('/$FILE/id')\nEOF")
                .iter()
                .all(|path| !path.contains("unresolved")),
        );
        assert!(
            extract_command_paths("python - <<EOF\nopen('/$FILE/id')\nEOF")
                .iter()
                .any(|path| path.starts_with("/__unresolved_shell_var__/"))
        );
        assert_eq!(
            extract_command_paths("cat \"$FILE\""),
            vec!["/__unresolved_shell_var__/$FILE".to_string()]
        );
    }

    #[test]
    fn bare_shell_vars_are_gated_by_command_context() {
        // 纯文本变量不再被当作路径（修复 `git commit -m "$MSG"` 误报）
        assert!(extract_command_paths("git commit -m \"$MSG\"").is_empty());
        assert!(extract_command_paths("echo hello $USER").is_empty());
        assert!(extract_command_paths(
            "curl -H \"Authorization: Bearer $TOKEN\" https://example.com"
        )
        .is_empty());

        // 吃路径命令中的裸变量仍是疑似路径，不可解析为 workspace-safe
        assert_eq!(
            extract_command_paths("cat $FILE"),
            vec!["/__unresolved_shell_var__/$FILE".to_string()]
        );
        assert_eq!(
            extract_command_paths("sudo rm -rf $DIR"),
            vec!["/__unresolved_shell_var__/$DIR".to_string()]
        );
        assert_eq!(
            extract_command_paths("cd $TARGET"),
            vec!["/__unresolved_shell_var__/$TARGET".to_string()]
        );

        // 含 `/` 或命令替换的展开无论动词如何都按路径处理
        assert_eq!(
            extract_command_paths("echo $FOO/id_rsa"),
            vec!["/__unresolved_shell_var__/$FOO/id_rsa".to_string()]
        );
        assert!(extract_command_paths("echo $(pwd)/x")
            .iter()
            .any(|p| p.starts_with("/__unresolved_shell_var__/")));
    }

    #[test]
    fn shell_read_observation_accepts_only_literal_paths() {
        assert_eq!(
            extract_literal_shell_read_paths("cat a.py", &None),
            vec!["a.py"]
        );
        assert!(extract_literal_shell_read_paths("cat $HOME/.ssh/id_rsa", &None).is_empty());
        assert_eq!(
            extract_literal_shell_read_paths("sed -n '1,20p' src/main.rs", &None),
            vec!["src/main.rs"]
        );
    }

    #[test]
    fn shell_read_observation_preserves_initial_cwd() {
        let cwd = Some(PathBuf::from("/workspace/sub"));
        assert_eq!(
            extract_literal_shell_read_paths("cat a.py", &cwd),
            vec!["/workspace/sub/a.py"]
        );
    }

    #[test]
    fn shell_read_observation_rejects_ambiguous_execution() {
        for command in [
            "false && cat victim.py; true",
            "cat /dev/null > victim.py",
            "cat a.py | head",
            "cat a.py\ncat b.py",
            "cat prefix$FILE",
            "cat *.py",
            "cat 'a b.py'",
            "sed -n 'e cat victim.py' a.py",
            "head -c victim.py",
            "cat --help victim.py",
        ] {
            assert!(
                extract_literal_shell_read_paths(command, &None).is_empty(),
                "{command}"
            );
        }
    }

    /// 持久 shell 的 cwd 与 workspace 分叉时，相对路径必须按会话 cwd 解析；
    /// cwd 未知（`cd` 目标无法静态解析）时 fail-closed 到工作区外的哨兵路径。
    #[test]
    fn shell_paths_resolve_against_tracked_session_cwd() {
        let tracked = Some(Some(PathBuf::from("/tmp/moved")));
        assert_eq!(
            resolve_shell_access_path("../secret.txt", &tracked),
            "/tmp/moved/../secret.txt"
        );
        assert_eq!(
            resolve_shell_access_path("./out.log", &tracked),
            "/tmp/moved/./out.log"
        );
        // 绝对路径与 ~ 路径不随会话 cwd 变化
        assert_eq!(
            resolve_shell_access_path("/etc/passwd", &tracked),
            "/etc/passwd"
        );
        assert_eq!(
            resolve_shell_access_path("~/.ssh/id_rsa", &tracked),
            "~/.ssh/id_rsa"
        );

        // cwd 未知 → 合成绝对路径落在工作区外，policy 默认 Confirm 而非按 workspace 命中 Allow；
        // `..` 被中和，词法归一化后仍留在哨兵根下
        let unknown = Some(None);
        assert_eq!(
            resolve_shell_access_path("../secret.txt", &unknown),
            "/__unknown_shell_cwd__/__up__/secret.txt"
        );
        assert_eq!(
            resolve_shell_access_path("../../../../etc/passwd", &unknown),
            "/__unknown_shell_cwd__/__up__/__up__/__up__/__up__/etc/passwd"
        );

        // 无会话记录（新会话运行于 workspace）→ 保持相对路径，由 resolve_path 按 workspace 解析
        assert_eq!(
            resolve_shell_access_path("src/main.py", &None),
            "src/main.py"
        );
    }

    /// 同一命令字符串内自带 `cd`/`pushd` 时无法把相对路径映射到单一 cwd——
    /// 必须由检查侧按未知 cwd fail-closed。
    #[test]
    fn detects_cwd_mutation_inside_a_command_string() {
        for cmd in [
            "cd /tmp && cat secret",
            "pwd; cd ../other",
            "pushd /var",
            "if cd /x; then echo hi; fi",
            "(cd /tmp); ls",
        ] {
            assert!(command_mutates_cwd(cmd), "expected `{cmd}` to mutate cwd");
        }
        for cmd in [
            "cat ./config",
            "ls -la",
            "python3 -c \"open('x').read()\"",
            "echo hello world",
            "echo cd",
            "git commit -m msg",
        ] {
            assert!(
                !command_mutates_cwd(cmd),
                "expected `{cmd}` not to mutate cwd"
            );
        }
    }
}

#[cfg(test)]
mod sandbox_expansion_tests {
    use super::*;
    use blockcell_tools::sandbox::{SandboxBackend, SandboxPolicy};

    fn expand(command: &str, workspace: &Path) -> Option<(String, Vec<PathAccess>)> {
        sandboxed_shell_expansion(
            command,
            workspace,
            workspace,
            SandboxBackend::LinuxBubblewrap,
            SandboxPolicy::WorkspaceWrite,
        )
    }

    #[test]
    fn bounded_find_loop_exposes_concrete_workspace_reads() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("proj")).unwrap();
        std::fs::write(temp.path().join("proj/a.py"), "TODO").unwrap();
        let (_, accesses) = expand(
            "for f in $(find proj -type f); do grep -n TODO \"$f\"; done",
            temp.path(),
        )
        .expect("literal workspace find loop should be statically provable");
        assert!(accesses
            .iter()
            .any(|a| a.op == PathOp::Read && a.path.ends_with("/proj/a.py")));
        assert!(accesses
            .iter()
            .all(|a| is_path_within_base(temp.path(), Path::new(&a.path))));
    }

    #[test]
    fn safe_lookup_pwd_and_dirname_substitutions_have_literal_analysis() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("proj")).unwrap();
        for cmd in [
            "$(command -v python3) --version",
            "$(which python3) --version",
            "cat \"$(pwd)/a.py\"",
            "cat \"$(dirname proj/a.py)/b.py\"",
        ] {
            let (normalized, _) = expand(cmd, temp.path()).expect(cmd);
            assert!(!normalized.contains('$'), "{normalized}");
            assert!(extract_command_paths(&normalized)
                .iter()
                .all(|p| !p.contains("__unresolved")));
        }
    }

    #[test]
    fn unknown_or_mutating_expansions_remain_conservative() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("proj")).unwrap();
        for cmd in [
            "cat $HOME/.ssh/id_rsa",
            "cat \"$FOO/id_rsa\"",
            "cat $f",
            "cat $(which python3)",
            "$(command -v python3; cat /etc/passwd)",
            "cat $(dirname ../secret)/x",
            "cat $(dirname /etc/passwd)/shadow",
            "for f in $(find /etc -type f); do grep -n TODO \"$f\"; done",
            "for f in $(find ../proj -type f); do grep -n TODO \"$f\"; done",
            "for f in $(find proj -type f -delete); do grep -n TODO \"$f\"; done",
            "for f in $(find proj -type f -exec cat /etc/passwd +); do cat \"$f\"; done",
            "for f in $(find proj -type f); do f=/etc/passwd; cat \"$f\"; done",
            "for f in $(find proj -type f); do grep -f /etc/passwd \"$f\"; done",
        ] {
            assert!(expand(cmd, temp.path()).is_none(), "{cmd}");
        }
    }

    #[test]
    fn expansion_requires_known_workspace_cwd_and_os_enforcement() {
        let temp = tempfile::tempdir().unwrap();
        for (backend, policy) in [
            (
                SandboxBackend::ApprovalRequired,
                SandboxPolicy::WorkspaceWrite,
            ),
            (SandboxBackend::FullAccess, SandboxPolicy::FullAccess),
        ] {
            assert!(sandboxed_shell_expansion(
                "$(which python3) --version",
                temp.path(),
                temp.path(),
                backend,
                policy
            )
            .is_none());
        }
        assert!(sandboxed_shell_expansion(
            "$(which python3) --version",
            Path::new("/"),
            temp.path(),
            SandboxBackend::LinuxBubblewrap,
            SandboxPolicy::WorkspaceWrite
        )
        .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn find_loop_rejects_symlink_roots_and_shell_split_filenames() {
        let temp = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(other.path(), temp.path().join("escape")).unwrap();
        assert!(expand(
            "for f in $(find escape -type f); do cat \"$f\"; done",
            temp.path()
        )
        .is_none());
        std::fs::create_dir(temp.path().join("proj")).unwrap();
        std::fs::write(temp.path().join("proj/a b"), "x").unwrap();
        assert!(expand(
            "for f in $(find proj -type f); do cat \"$f\"; done",
            temp.path()
        )
        .is_none());
    }
}
