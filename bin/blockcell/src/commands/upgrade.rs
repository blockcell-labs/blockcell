use blockcell_core::{Config, Paths};
use blockcell_updater::UpdateManager;

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

pub async fn apply() -> anyhow::Result<()> {
    println!("Apply not yet implemented.");
    println!("This would:");
    println!("  1. Verify the downloaded binary");
    println!("  2. Stop the running daemon");
    println!("  3. Replace the binary atomically");
    println!("  4. Restart the daemon");
    println!("  5. Run healthcheck");
    Ok(())
}

pub async fn rollback(to: Option<String>) -> anyhow::Result<()> {
    if let Some(version) = to {
        println!("Rollback to version {} not yet implemented.", version);
    } else {
        println!("Rollback to previous version not yet implemented.");
    }
    Ok(())
}

pub async fn status() -> anyhow::Result<()> {
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;
    let manager = UpdateManager::new(config, paths);

    let status = manager.status();

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
