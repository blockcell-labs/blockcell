use std::path::PathBuf;

use crate::{legacy_session_file_stem, session_file_stem, stable_hash_session_key};

/// Default content written to `~/.blockcell/BLOCKCELL.md` on first run.
///
/// This file is the global project-rules memory for the self-evolution system —
/// equivalent to `CLAUDE.md` in claude-code. It is injected into every evolution
/// prompt as high-level context, so edits here affect all skill evolutions.
const DEFAULT_BLOCKCELL_MD: &str = r#"# BLOCKCELL.md — Global Agent Rules

> This file is auto-loaded by the self-evolution system as global context.
> Edit it to record stable rules, conventions, and lessons that apply to all skills.

## Skill Development Conventions

- All user skills live in `~/.blockcell/workspace/skills/<skill_name>/`
- Every skill must have `meta.yaml` + `SKILL.md` (the runtime contract)
- `meta.yaml` required fields: `name`, `description`
- `meta.yaml` optional fields: `tools`, `requires`, `permissions`, `fallback`
- **Do NOT** add legacy routing fields (`triggers`, `capabilities`, `always`, `output_format`)
- `SKILL.md` must contain `## Shared {#shared}` and `## Prompt {#prompt}` sections

## Skill Types

| Type | Primary file | When to use |
|------|-------------|-------------|
| Prompt Tool | `SKILL.md` only | Pure LLM + blockcell built-in tools |
| Local Script | `SKILL.py` or `scripts/` | Needs Python/Shell/Node local execution |
| Hybrid | `SKILL.md` + `SKILL.py` | Both tool calls AND local script |

## Self-Evolution Principles

1. **Minimum change** — only modify what is necessary to fix the reported issue
2. **Keep SKILL.md intact** — do not remove `## Shared` or `## Prompt` sections
3. **No dangerous operations** — avoid `os.remove`, `shutil.rmtree`, `subprocess` with unsanitized input
4. **Handle errors gracefully** — every external call should have a fallback
5. **Prefer `display_text`** — if the skill produces user-facing output, return it as `display_text`

## Tool Usage Rules

- Only list tools in `meta.yaml` that the skill actually uses
- `exec_local` is provided automatically; do NOT add it to `tools`
- When using `web_fetch` or `http_request`, always handle non-200 responses

## Error Handling Pattern (Python skills)

```python
import sys, json

try:
    result = do_work()
    print(json.dumps({"display_text": result}))
except Exception as e:
    print(json.dumps({"error": str(e)}), file=sys.stderr)
    sys.exit(1)
```

## Common Fix Patterns

- **Empty response from API**: add retry logic or degrade gracefully
- **Import error**: add the package to `requires.bins` or use stdlib only
- **JSON parse error**: validate `response.status == 200` before parsing

## Self-Evolution Pipeline

The self-evolution pipeline is fixed to the full validation flow:

1. Generate patch
2. Static audit
3. LLM audit
4. Compile check
5. Contract check
6. Deploy and observe
"#;

#[derive(Debug, Clone)]
pub struct Paths {
    pub base: PathBuf,
    /// Override for workspace directory, sourced from config `agents.defaults.workspace`.
    /// When set, `workspace()` returns this path instead of `base/workspace`.
    workspace_override: Option<PathBuf>,
}

impl Paths {
    pub fn new() -> Self {
        let base = dirs::home_dir()
            .map(|h| h.join(".blockcell"))
            .unwrap_or_else(|| PathBuf::from(".blockcell"));
        Self {
            base,
            workspace_override: None,
        }
    }

    pub fn with_base(base: PathBuf) -> Self {
        Self {
            base,
            workspace_override: None,
        }
    }

    /// 从已知的 base 和 workspace 路径创建 Paths。
    /// 适用于工具执行上下文，其中 base 和 workspace 可能因配置而不同。
    pub fn with_base_and_workspace(base: PathBuf, workspace: PathBuf) -> Self {
        Self {
            base,
            workspace_override: Some(workspace),
        }
    }

    /// 创建 Paths 并从已存在的 config.json5 读取 workspace 配置。
    /// 仅读取已存在的配置文件，不会创建默认配置。
    /// 适用于 status, doctor, cron, logs, memory 等辅助命令。
    ///
    /// 前置条件：调用方应确保 .env 已加载（main.rs 会在命令分发前统一加载），
    /// 否则 config 中的 `${ENV_VAR}` 无法正确展开。
    pub fn new_configured() -> Self {
        let mut paths = Self::new();
        let config_path = paths.config_file();
        // 仅在配置文件已存在时读取，避免触发默认配置创建
        if config_path.exists() {
            if let Ok(config) = crate::Config::load(&config_path) {
                paths.apply_workspace_config(&config.agents.defaults.workspace);
            }
        }
        paths
    }

    /// Apply workspace override from config, expanding `~` to home directory.
    /// Call this after loading config at runtime entry points.
    pub fn apply_workspace_config(&mut self, workspace: &str) {
        let expanded = if workspace.starts_with('~') {
            if let Some(home) = dirs::home_dir() {
                workspace.replacen('~', home.to_str().unwrap_or(""), 1)
            } else {
                workspace.to_string()
            }
        } else {
            workspace.to_string()
        };
        self.workspace_override = Some(PathBuf::from(expanded));
    }

    pub fn config_file(&self) -> PathBuf {
        self.base.join("config.json5")
    }

    pub fn env_file(&self) -> PathBuf {
        self.base.join(".env")
    }

    pub fn mcp_config_file(&self) -> PathBuf {
        self.base.join("mcp.json")
    }

    pub fn mcp_dir(&self) -> PathBuf {
        self.base.join("mcp.d")
    }

    pub fn mcp_state_file(&self) -> PathBuf {
        self.base.join("mcp-state.json")
    }

    pub fn for_agent(&self, agent_id: &str) -> Self {
        let agent_id = agent_id.trim();
        if agent_id.is_empty() || agent_id == "default" {
            return self.clone();
        }
        // 命名 agent 使用独立的工作空间（base/agents/<id>/workspace），
        // 不继承根级别的 workspace_override。
        let directory_name = if is_safe_agent_directory_name(agent_id) {
            agent_id.to_string()
        } else {
            format!("agent-{}", stable_hash_session_key(agent_id))
        };
        Self {
            base: self.base.join("agents").join(directory_name),
            workspace_override: None,
        }
    }

    pub fn workspace(&self) -> PathBuf {
        self.workspace_override
            .clone()
            .unwrap_or_else(|| self.base.join("workspace"))
    }

    /// Path to logs directory (under workspace)
    pub fn logs_dir(&self) -> PathBuf {
        self.workspace().join("logs")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.base.join("sessions")
    }

    pub fn session_file(&self, session_key: &str) -> PathBuf {
        let safe_key = session_file_stem(session_key);
        self.sessions_dir().join(format!("{}.jsonl", safe_key))
    }

    pub fn legacy_session_file(&self, session_key: &str) -> PathBuf {
        self.sessions_dir()
            .join(format!("{}.jsonl", legacy_session_file_stem(session_key)))
    }

    /// Append-only session event logs. Kept outside `sessions/` so transcript
    /// listings that scan `sessions/*.jsonl` never pick them up.
    pub fn session_events_dir(&self) -> PathBuf {
        self.base.join("session_events")
    }

    pub fn session_events_file(&self, session_key: &str) -> PathBuf {
        self.session_events_dir()
            .join(format!("{}.jsonl", session_file_stem(session_key)))
    }

    pub fn audit_dir(&self) -> PathBuf {
        self.base.join("audit")
    }

    pub fn cron_dir(&self) -> PathBuf {
        self.base.join("cron")
    }

    pub fn cron_jobs_file(&self) -> PathBuf {
        self.cron_dir().join("jobs.json")
    }

    pub fn cron_jobs_lock_file(&self) -> PathBuf {
        self.cron_dir().join("jobs.json.lock")
    }

    pub fn media_dir(&self) -> PathBuf {
        self.workspace().join("media")
    }

    pub fn update_dir(&self) -> PathBuf {
        self.base.join("update")
    }

    pub fn bridge_dir(&self) -> PathBuf {
        self.base.join("bridge")
    }

    pub fn whatsapp_auth_dir(&self) -> PathBuf {
        self.base.join("whatsapp-auth")
    }

    // Workspace files
    pub fn agents_md(&self) -> PathBuf {
        self.workspace().join("AGENTS.md")
    }

    pub fn soul_md(&self) -> PathBuf {
        self.workspace().join("SOUL.md")
    }

    pub fn user_md(&self) -> PathBuf {
        self.workspace().join("USER.md")
    }

    pub fn heartbeat_md(&self) -> PathBuf {
        self.workspace().join("HEARTBEAT.md")
    }

    pub fn memory_dir(&self) -> PathBuf {
        self.workspace().join("memory")
    }

    pub fn memory_md(&self) -> PathBuf {
        self.memory_dir().join("MEMORY.md")
    }

    pub fn knowledge_index_db(&self) -> PathBuf {
        self.memory_dir().join("knowledge_index.db")
    }

    pub fn daily_memory(&self, date: &str) -> PathBuf {
        self.memory_dir().join(format!("{}.md", date))
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.workspace().join("skills")
    }

    /// Path to `BLOCKCELL.md` — the project-level global rules file.
    /// Used by the self-evolution system to inject project-wide context into prompts.
    pub fn blockcell_md(&self) -> PathBuf {
        self.base.join("BLOCKCELL.md")
    }

    pub fn import_staging_skills_dir(&self) -> PathBuf {
        self.workspace().join("import_staging").join("skills")
    }

    pub fn evolved_tools_dir(&self) -> PathBuf {
        self.workspace().join("evolved_tools")
    }

    pub fn channel_contacts_file(&self) -> PathBuf {
        self.base.join("channel_contacts.json")
    }

    pub fn path_access_file(&self) -> PathBuf {
        self.base.join("path_access.json5")
    }

    pub fn toggles_file(&self) -> PathBuf {
        self.workspace().join("toggles.json")
    }

    pub fn tool_artifacts_dir(&self) -> PathBuf {
        self.workspace().join("tool_artifacts")
    }

    pub fn tool_evolution_records_dir(&self) -> PathBuf {
        self.workspace().join("tool_evolution_records")
    }

    pub fn ghost_dir(&self) -> PathBuf {
        self.workspace().join("ghost")
    }

    pub fn ghost_ledger_db(&self) -> PathBuf {
        self.ghost_dir().join("ghost_ledger.db")
    }

    pub fn ghost_reports_dir(&self) -> PathBuf {
        self.ghost_dir().join("reports")
    }

    pub fn builtin_skills_dir(&self) -> PathBuf {
        // Try multiple candidate paths, return the first that exists on disk.
        // 1. exe/../skills  (installed layout: bin/blockcell + skills/)
        // 2. exe/../../skills (cargo layout: target/debug/blockcell + skills/)
        // 3. ./skills (CWD-relative, for running from project root)
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                for relative in &["../skills", "../../skills"] {
                    let candidate = exe_dir.join(relative);
                    if candidate.is_dir() {
                        return candidate;
                    }
                }
            }
        }
        PathBuf::from("./skills")
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.base)?;
        std::fs::create_dir_all(self.workspace())?;
        std::fs::create_dir_all(self.sessions_dir())?;
        std::fs::create_dir_all(self.audit_dir())?;
        std::fs::create_dir_all(self.cron_dir())?;
        std::fs::create_dir_all(self.workspace().join("media"))?;
        std::fs::create_dir_all(self.workspace().join("downloads"))?;
        std::fs::create_dir_all(self.update_dir())?;
        std::fs::create_dir_all(self.bridge_dir())?;
        std::fs::create_dir_all(self.whatsapp_auth_dir())?;
        std::fs::create_dir_all(self.mcp_dir())?;
        std::fs::create_dir_all(self.memory_dir())?;
        std::fs::create_dir_all(self.skills_dir())?;
        std::fs::create_dir_all(self.import_staging_skills_dir())?;
        std::fs::create_dir_all(self.evolved_tools_dir())?;
        std::fs::create_dir_all(self.tool_artifacts_dir())?;
        std::fs::create_dir_all(self.tool_evolution_records_dir())?;
        std::fs::create_dir_all(self.ghost_dir())?;
        std::fs::create_dir_all(self.ghost_reports_dir())?;
        std::fs::create_dir_all(self.logs_dir())?;
        // Bootstrap BLOCKCELL.md with default template if it does not exist
        let blockcell_md = self.blockcell_md();
        if !blockcell_md.exists() {
            let _ = std::fs::write(&blockcell_md, DEFAULT_BLOCKCELL_MD);
        }
        Ok(())
    }
}

fn is_safe_agent_directory_name(agent_id: &str) -> bool {
    agent_id != "."
        && agent_id != ".."
        && agent_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

impl Default for Paths {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_for_agent_reuses_root_layout_for_default() {
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell"));
        let default_paths = paths.for_agent("default");

        assert_eq!(default_paths.base, PathBuf::from("/tmp/blockcell"));
        assert_eq!(
            default_paths.workspace(),
            PathBuf::from("/tmp/blockcell/workspace")
        );
        assert_eq!(
            default_paths.sessions_dir(),
            PathBuf::from("/tmp/blockcell/sessions")
        );
        assert_eq!(
            default_paths.audit_dir(),
            PathBuf::from("/tmp/blockcell/audit")
        );
    }

    #[test]
    fn test_config_file_uses_json5_extension() {
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell"));
        assert_eq!(
            paths.config_file(),
            PathBuf::from("/tmp/blockcell/config.json5")
        );
    }

    #[test]
    fn test_for_agent_scopes_non_default_under_agents_dir() {
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell"));
        let ops_paths = paths.for_agent("ops");

        assert_eq!(ops_paths.base, PathBuf::from("/tmp/blockcell/agents/ops"));
        assert_eq!(
            ops_paths.workspace(),
            PathBuf::from("/tmp/blockcell/agents/ops/workspace")
        );
        assert_eq!(
            ops_paths.sessions_dir(),
            PathBuf::from("/tmp/blockcell/agents/ops/sessions")
        );
        assert_eq!(
            ops_paths.audit_dir(),
            PathBuf::from("/tmp/blockcell/agents/ops/audit")
        );
    }

    #[test]
    fn test_for_agent_keeps_unsafe_id_under_agents_dir() {
        use std::path::Component;

        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell"));
        let escaped = paths.for_agent("../../outside");

        assert!(escaped.base.starts_with("/tmp/blockcell/agents"));
        assert!(!escaped
            .base
            .components()
            .any(|component| component == Component::ParentDir));
    }

    #[test]
    fn test_logs_dir_returns_expected_path() {
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell"));
        assert_eq!(
            paths.logs_dir(),
            PathBuf::from("/tmp/blockcell/workspace/logs")
        );
    }

    #[test]
    fn test_ensure_dirs_creates_media_cleanup_targets() {
        let base = std::env::temp_dir().join(format!("blockcell-paths-{}", uuid::Uuid::new_v4()));
        let paths = Paths::with_base(base.clone());

        paths.ensure_dirs().expect("ensure dirs");

        let media_exists = paths.media_dir().is_dir();
        let downloads_exists = paths.workspace().join("downloads").is_dir();

        std::fs::remove_dir_all(base).expect("remove temp dirs");

        assert!(media_exists);
        assert!(downloads_exists);
    }

    #[test]
    fn test_ghost_learning_paths_live_under_workspace_ghost() {
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell-test"));
        assert_eq!(
            paths.ghost_dir(),
            PathBuf::from("/tmp/blockcell-test/workspace/ghost")
        );
        assert_eq!(
            paths.ghost_ledger_db(),
            PathBuf::from("/tmp/blockcell-test/workspace/ghost/ghost_ledger.db")
        );
    }
}
