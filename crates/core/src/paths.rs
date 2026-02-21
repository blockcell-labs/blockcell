use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Paths {
    pub base: PathBuf,
}

impl Paths {
    pub fn new() -> Self {
        let base = dirs::home_dir()
            .map(|h| h.join(".blockcell"))
            .unwrap_or_else(|| PathBuf::from(".blockcell"));
        Self { base }
    }

    pub fn with_base(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn config_file(&self) -> PathBuf {
        self.base.join("config.json")
    }

    pub fn workspace(&self) -> PathBuf {
        self.base.join("workspace")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.base.join("sessions")
    }

    pub fn session_file(&self, session_key: &str) -> PathBuf {
        let safe_key = session_key.replace([':', '/', '\\'], "_");
        self.sessions_dir().join(format!("{}.jsonl", safe_key))
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

    pub fn tools_md(&self) -> PathBuf {
        self.workspace().join("TOOLS.md")
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

    pub fn daily_memory(&self, date: &str) -> PathBuf {
        self.memory_dir().join(format!("{}.md", date))
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.workspace().join("skills")
    }

    pub fn evolved_tools_dir(&self) -> PathBuf {
        self.workspace().join("evolved_tools")
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
        std::fs::create_dir_all(self.update_dir())?;
        std::fs::create_dir_all(self.bridge_dir())?;
        std::fs::create_dir_all(self.whatsapp_auth_dir())?;
        std::fs::create_dir_all(self.memory_dir())?;
        std::fs::create_dir_all(self.skills_dir())?;
        std::fs::create_dir_all(self.evolved_tools_dir())?;
        std::fs::create_dir_all(self.tool_artifacts_dir())?;
        std::fs::create_dir_all(self.tool_evolution_records_dir())?;
        Ok(())
    }
}

impl Default for Paths {
    fn default() -> Self {
        Self::new()
    }
}
