use anyhow::Context;
use blockcell_core::Paths;
use std::fs;
use std::path::Path;
use tracing::info;

fn default_env_template() -> &'static str {
    "BLOCKCELL_API_TOKEN=\nOPENAI_API_KEY=\nANTHROPIC_API_KEY=\nGEMINI_API_KEY=\n"
}

fn parse_env_assignment(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let body = trimmed.strip_prefix("export ").unwrap_or(trimmed).trim();
    let (key, value) = body.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }

    let mut value = value.trim().to_string();
    if value.len() >= 2 {
        let quoted = (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''));
        if quoted {
            value = value[1..value.len() - 1].to_string();
        }
    }

    Some((key.to_string(), value))
}

pub fn ensure_and_load_blockcell_env(paths: &Paths) -> anyhow::Result<()> {
    paths.ensure_dirs().with_context(|| {
        format!(
            "failed to create blockcell dirs at {}",
            paths.base.display()
        )
    })?;

    let env_path = paths.env_file();
    if !env_path.exists() {
        write_private_env_file(&env_path, default_env_template()).with_context(|| {
            format!(
                "failed to create default env file at {}",
                env_path.display()
            )
        })?;
        info!(path = %env_path.display(), "Created default blockcell .env file");
    } else {
        // Older installs created this file world-readable; it holds API keys.
        restrict_env_file_permissions(&env_path);
    }

    load_env_file(&env_path)
}

/// Create the .env file owner-only: it stores API keys, and the process umask would
/// otherwise leave it at 0644 (readable by every local user).
fn write_private_env_file(path: &Path, contents: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents.as_bytes())?;
        file.flush()?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::write(path, contents)
    }
}

/// Tighten permissions on an existing .env file (best effort).
fn restrict_env_file_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn load_env_file(path: &Path) -> anyhow::Result<()> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read env file {}", path.display()))?;

    let mut loaded = 0usize;
    for line in content.lines() {
        if let Some((key, value)) = parse_env_assignment(line) {
            // An empty entry in the generated template must not clobber a real value the
            // shell already exported — that silently disabled auth (e.g. OPENAI_API_KEY).
            if value.is_empty() && std::env::var(&key).is_ok_and(|existing| !existing.is_empty()) {
                continue;
            }
            // This runs during command startup, before worker threads are spawned.
            unsafe {
                std::env::set_var(key, value);
            }
            loaded += 1;
        }
    }

    info!(path = %path.display(), loaded, "Loaded blockcell .env file");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_env_assignment;

    #[test]
    fn parse_env_assignment_handles_plain_export_and_quotes() {
        assert_eq!(
            parse_env_assignment("GBRAIN_EMBEDDING_MODEL=embedding-3"),
            Some((
                "GBRAIN_EMBEDDING_MODEL".to_string(),
                "embedding-3".to_string()
            ))
        );
        assert_eq!(
            parse_env_assignment("export GBRAIN_EMBEDDING_DIMENSIONS='2048'"),
            Some((
                "GBRAIN_EMBEDDING_DIMENSIONS".to_string(),
                "2048".to_string()
            ))
        );
    }

    #[test]
    fn parse_env_assignment_ignores_comments_and_blank_lines() {
        assert_eq!(parse_env_assignment("# GBRAIN_EMBEDDING_MODEL=x"), None);
        assert_eq!(parse_env_assignment("   "), None);
    }
}
