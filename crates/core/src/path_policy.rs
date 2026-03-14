use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use tracing::{info, warn};

fn format_policy_parse_error(policy_file: &Path, error: &json5::Error) -> String {
    format!(
        "Path access policy JSON5 parse error in {}: {}",
        policy_file.display(),
        error
    )
}

/// Which operation is being performed on a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathOp {
    Read,
    Write,
    List,
    Exec,
}

impl PathOp {
    /// Derive the operation type from a tool name.
    pub fn from_tool_name(tool_name: &str) -> Self {
        match tool_name {
            "read_file" => PathOp::Read,
            "list_dir" => PathOp::List,
            "exec" => PathOp::Exec,
            // write-class tools
            "write_file" | "edit_file" | "file_ops" | "data_process" | "audio_transcribe"
            | "chart_generate" | "office_write" | "video_process" | "health_api" | "encrypt" => {
                PathOp::Write
            }
            _ => PathOp::Read,
        }
    }
}

/// What the policy engine decides for a given path + operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAction {
    /// Access allowed immediately — no confirmation required.
    Allow,
    /// User must confirm before access is granted.
    Confirm,
    /// Access denied; cannot be overridden by confirmation.
    Deny,
}

/// A single path rule entry inside `path_access.json5`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathRule {
    /// Friendly name for the rule (for logging and documentation).
    pub name: String,
    /// The access decision when this rule matches.
    pub action: PolicyAction,
    /// Which operations this rule applies to.
    pub ops: Vec<PathOp>,
    /// Path prefixes this rule covers. `~` and `~/` prefixes are expanded.
    pub paths: Vec<String>,
}

/// The contents of the `path_access.json5` policy file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathPolicyFileConfig {
    /// Schema version — currently must be 1.
    #[serde(default = "default_version")]
    pub version: u32,

    /// What to do when no rule matches a path.
    /// Default: `confirm` (requires user approval).
    #[serde(default = "default_policy_confirm")]
    pub default_policy: PolicyAction,

    /// Whether the session-level directory cache is used to avoid re-confirming
    /// the same directory. Only applies to `confirm` outcomes.
    #[serde(default = "default_true")]
    pub cache_confirmed_dirs: bool,

    /// When `true`, a built-in list of sensitive system paths (e.g. `~/.ssh`, `/etc`)
    /// is always denied, even if no explicit rule covers them.
    #[serde(default = "default_true")]
    pub builtin_protected_paths: bool,

    /// User-defined rules, evaluated in priority order (deny > allow > confirm).
    #[serde(default)]
    pub rules: Vec<PathRule>,
}

fn default_version() -> u32 {
    1
}
fn default_policy_confirm() -> PolicyAction {
    PolicyAction::Confirm
}
fn default_true() -> bool {
    true
}

impl Default for PathPolicyFileConfig {
    fn default() -> Self {
        Self {
            version: 1,
            default_policy: PolicyAction::Confirm,
            cache_confirmed_dirs: true,
            builtin_protected_paths: true,
            rules: Vec::new(),
        }
    }
}

/// Built-in sensitive path prefixes that are always denied when
/// `builtin_protected_paths = true` (the default).
pub fn builtin_sensitive_paths() -> &'static [&'static str] {
    &[
        "~/.ssh",
        "~/.aws",
        "~/.gnupg",
        "~/.kube",
        "~/.config/gcloud",
        "~/.azure",
        "~/.netrc",
        "/etc",
        "/System",
        "/private/etc",
        "/private/var",
        "/usr/bin",
        "/usr/sbin",
        "/bin",
        "/sbin",
    ]
}

/// The runtime path-policy engine. Loaded from the policy file at startup.
#[derive(Debug, Clone)]
pub struct PathPolicy {
    config: PathPolicyFileConfig,
    /// `true` when loaded successfully from a file (rather than using safe defaults).
    pub from_file: bool,
}

impl PathPolicy {
    /// Load the policy from the given file path.
    ///
    /// If the file does not exist, or is unreadable / unparseable, falls back to
    /// safe defaults (workspace = allow, sensitive = deny, everything else = confirm).
    pub fn load(policy_file: &Path) -> Self {
        if !policy_file.exists() {
            info!(
                path = %policy_file.display(),
                "Path policy file not found — using safe defaults"
            );
            return Self::safe_default();
        }

        match std::fs::read_to_string(policy_file) {
            Ok(content) => match json5::from_str::<PathPolicyFileConfig>(&content) {
                Ok(config) => {
                    info!(
                        path = %policy_file.display(),
                        rules = config.rules.len(),
                        "Loaded path access policy"
                    );
                    Self {
                        config,
                        from_file: true,
                    }
                }
                Err(e) => {
                    warn!(
                        path = %policy_file.display(),
                        error = %format_policy_parse_error(policy_file, &e),
                        "Failed to parse path access policy file — using safe defaults"
                    );
                    Self::safe_default()
                }
            },
            Err(e) => {
                warn!(
                    path = %policy_file.display(),
                    error = %e,
                    "Failed to read path access policy file — using safe defaults"
                );
                Self::safe_default()
            }
        }
    }

    /// Construct the safe default policy (no user-defined rules).
    pub fn safe_default() -> Self {
        Self {
            config: PathPolicyFileConfig::default(),
            from_file: false,
        }
    }

    /// Evaluate the policy for a given **resolved** (canonicalized / normalized) path
    /// and operation type.
    ///
    /// Evaluation priority:
    /// 1. Built-in sensitive paths → always `Deny`
    /// 2. User `deny` rules (most-specific prefix wins within the same action)
    /// 3. User `allow` rules (most-specific prefix)
    ///    — an `allow` rule that is MORE specific than a `deny` rule wins
    /// 4. User `confirm` rules
    /// 5. `default_policy`
    pub fn evaluate(&self, resolved: &Path, op: PathOp) -> PolicyAction {
        // 1. Built-in sensitive paths
        if self.config.builtin_protected_paths {
            for sensitive in builtin_sensitive_paths() {
                let expanded = expand_tilde(sensitive);
                if path_starts_with_normalized(resolved, &expanded) {
                    return PolicyAction::Deny;
                }
            }
        }

        // Find the most specific matching rule for each action type
        let deny_len = self.best_match_len(resolved, op, PolicyAction::Deny);
        let allow_len = self.best_match_len(resolved, op, PolicyAction::Allow);
        let confirm_len = self.best_match_len(resolved, op, PolicyAction::Confirm);

        // 2. Deny vs allow — more specific wins (longer prefix)
        if let Some(dl) = deny_len {
            if let Some(al) = allow_len {
                if al > dl {
                    return PolicyAction::Allow;
                }
            }
            return PolicyAction::Deny;
        }

        // 3. Allow
        if allow_len.is_some() {
            return PolicyAction::Allow;
        }

        // 4. Confirm
        if confirm_len.is_some() {
            return PolicyAction::Confirm;
        }

        // 5. Default
        self.config.default_policy
    }

    /// Returns the prefix length (in bytes) of the best-matching rule for the given
    /// action type, or `None` if no rule matches.
    fn best_match_len(&self, resolved: &Path, op: PathOp, action: PolicyAction) -> Option<usize> {
        let mut best: Option<usize> = None;
        for rule in &self.config.rules {
            if rule.action != action || !rule.ops.contains(&op) {
                continue;
            }
            for pattern in &rule.paths {
                let expanded = expand_tilde(pattern);
                if path_starts_with_normalized(resolved, &expanded) {
                    let len = expanded.as_os_str().len();
                    if best.map(|b| len > b).unwrap_or(true) {
                        best = Some(len);
                    }
                }
            }
        }
        best
    }

    /// Whether session-level confirmation caching is enabled.
    pub fn cache_confirmed_dirs(&self) -> bool {
        self.config.cache_confirmed_dirs
    }
}

impl Default for PathPolicy {
    fn default() -> Self {
        Self::safe_default()
    }
}

// ── Path helpers ─────────────────────────────────────────────────────────────

/// Expand a `~/...` or `~` path prefix to an absolute path.
pub fn expand_tilde(path_str: &str) -> PathBuf {
    if let Some(rest) = path_str.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(path_str))
    } else if path_str == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
    } else {
        PathBuf::from(path_str)
    }
}

/// Check whether `path` starts with `base`, after normalizing both sides.
/// Falls back to lexicographic normalization when `canonicalize` fails
/// (e.g. for paths that don't exist yet).
pub fn path_starts_with_normalized(path: &Path, base: &Path) -> bool {
    let path_c = canonical_or_normalize(path);
    let base_c = canonical_or_normalize(base);
    path_c.starts_with(&base_c)
}

fn canonical_or_normalize(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| normalize_path(p))
}

fn normalize_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in p.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

// ── Template ─────────────────────────────────────────────────────────────────

/// Returns the content of a starter `path_access.json5` template.
/// Written to `~/.blockcell/path_access.json5` on first agent startup when
/// the file does not already exist.
pub fn default_policy_template() -> &'static str {
    r#"{
  // Path Access Policy — Blockcell
  // See docs/path_access_policy.md for full documentation.
  version: 1,

  // Fallback when no rule matches: "allow" | "confirm" | "deny"
  default_policy: "confirm",

  // Re-use session approval for the whole directory (reduces repeated prompts)
  cache_confirmed_dirs: true,

  // Always deny access to built-in sensitive paths (~/.ssh, /etc, etc.)
  builtin_protected_paths: true,

  rules: [
    // ── Deny sensitive credential directories (highest priority) ─────────
    {
      name: "deny-secrets",
      action: "deny",
      ops: ["read", "write", "list", "exec"],
      paths: [
        "~/.ssh",
        "~/.aws",
        "~/.gnupg",
        "~/.kube",
        "~/.config/gcloud",
        "/etc",
        "/System"
      ]
    },

    // ── Allow common development directories without confirmation ─────────
    // Uncomment and adjust to match your own workspace roots:
    // {
    //   name: "allow-dev-roots",
    //   action: "allow",
    //   ops: ["read", "list", "write"],
    //   paths: [
    //     "~/dev",
    //     "~/projects",
    //     "~/Desktop",
    //     "~/Documents"
    //   ]
    // },

    // ── Require confirmation for exec in home directory ───────────────────
    {
      name: "confirm-home-exec",
      action: "confirm",
      ops: ["exec"],
      paths: ["~"]
    }
  ]
}
"#
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn home() -> PathBuf {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/home/user"))
    }

    fn make_policy(rules: Vec<PathRule>, default: PolicyAction) -> PathPolicy {
        PathPolicy {
            config: PathPolicyFileConfig {
                version: 1,
                default_policy: default,
                cache_confirmed_dirs: true,
                builtin_protected_paths: false, // disable built-ins so unit tests are deterministic
                rules,
            },
            from_file: false,
        }
    }

    #[test]
    fn test_allow_rule_matches() {
        let dev_path = home().join("dev");
        let policy = make_policy(
            vec![PathRule {
                name: "allow-dev".to_string(),
                action: PolicyAction::Allow,
                ops: vec![PathOp::Read, PathOp::Write],
                paths: vec!["~/dev".to_string()],
            }],
            PolicyAction::Confirm,
        );
        let target = dev_path.join("project").join("main.rs");
        assert_eq!(policy.evaluate(&target, PathOp::Read), PolicyAction::Allow);
        assert_eq!(policy.evaluate(&target, PathOp::Write), PolicyAction::Allow);
        // exec not covered by this rule → default (confirm)
        assert_eq!(
            policy.evaluate(&target, PathOp::Exec),
            PolicyAction::Confirm
        );
    }

    #[test]
    fn test_deny_rule_matches() {
        let policy = make_policy(
            vec![PathRule {
                name: "deny-ssh".to_string(),
                action: PolicyAction::Deny,
                ops: vec![PathOp::Read, PathOp::Write, PathOp::List, PathOp::Exec],
                paths: vec!["~/.ssh".to_string()],
            }],
            PolicyAction::Confirm,
        );
        let ssh_key = home().join(".ssh").join("id_rsa");
        assert_eq!(policy.evaluate(&ssh_key, PathOp::Read), PolicyAction::Deny);
    }

    #[test]
    fn test_more_specific_allow_overrides_deny() {
        let policy = make_policy(
            vec![
                PathRule {
                    name: "deny-home".to_string(),
                    action: PolicyAction::Deny,
                    ops: vec![PathOp::Write],
                    paths: vec!["~".to_string()],
                },
                PathRule {
                    name: "allow-dev".to_string(),
                    action: PolicyAction::Allow,
                    ops: vec![PathOp::Write],
                    paths: vec!["~/dev".to_string()],
                },
            ],
            PolicyAction::Confirm,
        );
        let dev_file = home().join("dev").join("code.rs");
        // ~/dev is more specific than ~ → allow wins
        assert_eq!(
            policy.evaluate(&dev_file, PathOp::Write),
            PolicyAction::Allow
        );
        // ~/Documents is only covered by deny-home
        let doc_file = home().join("Documents").join("notes.txt");
        assert_eq!(
            policy.evaluate(&doc_file, PathOp::Write),
            PolicyAction::Deny
        );
    }

    #[test]
    fn test_default_policy_applied_when_no_rules_match() {
        let policy = make_policy(vec![], PolicyAction::Confirm);
        let random_path = PathBuf::from("/tmp/some/file.txt");
        assert_eq!(
            policy.evaluate(&random_path, PathOp::Read),
            PolicyAction::Confirm
        );
    }

    #[test]
    fn test_builtin_sensitive_paths_deny() {
        let policy = PathPolicy {
            config: PathPolicyFileConfig {
                builtin_protected_paths: true,
                rules: vec![],
                default_policy: PolicyAction::Confirm,
                ..Default::default()
            },
            from_file: false,
        };
        let ssh_key = home().join(".ssh").join("id_rsa");
        assert_eq!(policy.evaluate(&ssh_key, PathOp::Read), PolicyAction::Deny);
    }

    #[test]
    fn test_policy_from_template_parses() {
        let config: PathPolicyFileConfig =
            json5::from_str(default_policy_template()).expect("template should parse");
        assert_eq!(config.version, 1);
        assert!(!config.rules.is_empty());
    }

    #[test]
    fn test_path_op_from_tool_name() {
        assert_eq!(PathOp::from_tool_name("read_file"), PathOp::Read);
        assert_eq!(PathOp::from_tool_name("write_file"), PathOp::Write);
        assert_eq!(PathOp::from_tool_name("list_dir"), PathOp::List);
        assert_eq!(PathOp::from_tool_name("exec"), PathOp::Exec);
        assert_eq!(PathOp::from_tool_name("edit_file"), PathOp::Write);
    }
}
