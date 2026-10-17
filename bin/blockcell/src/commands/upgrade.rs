use blockcell_core::{Config, Paths};
use blockcell_updater::UpdateManager;
use std::time::Duration;

const AUTO_UPGRADE_START_DELAY: Duration = Duration::from_secs(30);

pub fn spawn_auto_upgrade_if_enabled(
    config: Config,
    paths: Paths,
) -> Option<tokio::task::JoinHandle<()>> {
    if !config.auto_upgrade.enabled {
        return None;
    }

    Some(tokio::spawn(async move {
        tokio::time::sleep(AUTO_UPGRADE_START_DELAY).await;
        let manager = UpdateManager::new(config, paths);
        if let Err(error) = manager.update_flow().await {
            tracing::warn!(error = %error, "Automatic update failed");
        }
    }))
}

pub async fn check() -> anyhow::Result<()> {
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;
    let manager = UpdateManager::new(config, paths);

    println!("Checking for updates...");

    match manager.check().await {
        Ok(Some(manifest)) => {
            println!("Update available!");
            println!("  Version: {}", manifest.version);
            println!("  Channel: {}", manifest.channel);
            println!("  Published: {}", manifest.published_at);
            if !manifest.notes.is_empty() {
                println!("  Notes: {}", manifest.notes);
            }
            println!();
            println!("Run `blockcell upgrade download` to download.");
        }
        Ok(None) => {
            println!("No updates available.");
        }
        Err(e) => {
            println!("Failed to check for updates: {}", e);
        }
    }

    Ok(())
}

pub async fn download() -> anyhow::Result<()> {
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;
    let manager = UpdateManager::new(config, paths);

    println!("Checking for updates...");

    match manager.check().await {
        Ok(Some(manifest)) => {
            println!("Downloading version {}...", manifest.version);
            match manager.download(&manifest).await {
                Ok(path) => {
                    println!("Downloaded to: {}", path.display());
                    println!();
                    println!("Run `blockcell upgrade apply` to install.");
                }
                Err(e) => {
                    println!("Download failed: {}", e);
                }
            }
        }
        Ok(None) => {
            println!("No updates available.");
        }
        Err(e) => {
            println!("Failed to check for updates: {}", e);
        }
    }

    Ok(())
}

pub async fn apply(force: bool) -> anyhow::Result<()> {
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;
    let manager = UpdateManager::new(config, paths);
    let staged = manager
        .staged_update()?
        .ok_or_else(|| anyhow::anyhow!("No downloaded update is ready to apply"))?;

    println!("Applying version {}...", staged.version);
    manager
        .apply_with_options(&staged.path, &staged.version, force)
        .await?;
    println!("Update applied successfully. Restart blockcell to use the new version.");
    Ok(())
}

pub async fn rollback(to: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;
    let manager = UpdateManager::new(config, paths);

    match to.as_deref() {
        Some(version) => println!("Rolling back to version {}...", version),
        None => println!("Rolling back to the previous version..."),
    }
    manager.rollback(to.as_deref()).await?;
    println!("Rollback completed. Restart blockcell to use the restored version.");
    Ok(())
}

pub async fn status() -> anyhow::Result<()> {
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;
    let manager = UpdateManager::new(config, paths);

    let status = manager.status().await?;

    println!("Upgrade Status");
    println!("==============");
    println!();
    println!("Current version: {}", status.current_version);

    if let Some(latest) = status.latest_version {
        println!("Latest version:  {}", latest);
    }

    if status.update_available {
        println!("Update available: yes");
    }

    if let Some(staging) = status.staging_path {
        println!("Staging path:    {}", staging.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn auto_upgrade_task_respects_enabled_setting() {
        let paths = Paths::with_base(std::env::temp_dir().join(format!(
            "blockcell-auto-upgrade-test-{}",
            std::process::id()
        )));
        let mut config = Config::default();

        config.auto_upgrade.enabled = false;
        assert!(spawn_auto_upgrade_if_enabled(config.clone(), paths.clone()).is_none());

        config.auto_upgrade.enabled = true;
        let handle = spawn_auto_upgrade_if_enabled(config, paths)
            .expect("enabled auto-upgrade should start a task");
        handle.abort();
    }
}
