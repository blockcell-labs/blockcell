use crate::manifest::Manifest;
use crate::verification::{SignatureVerifier, Sha256Verifier, HealthChecker};
use crate::atomic::{AtomicSwitcher, MaintenanceWindow};
use blockcell_core::{Config, Error, Paths, Result};
use reqwest::Client;
use std::path::PathBuf;
use tracing::{debug, info, warn, error};


pub struct UpdateManager {
    config: Config,
    paths: Paths,
    client: Client,
    switcher: AtomicSwitcher,
}

#[derive(Debug)]
pub struct UpdateStatus {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub staging_path: Option<PathBuf>,
}

impl UpdateManager {
    pub fn new(config: Config, paths: Paths) -> Self {
        let install_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        
        let switcher = AtomicSwitcher::new(install_dir);
        
        Self {
            config,
            paths,
            client: Client::new(),
            switcher,
        }
    }

    pub async fn check(&self) -> Result<Option<Manifest>> {
        let manifest_url = &self.config.auto_upgrade.manifest_url;
        if manifest_url.is_empty() {
            return Err(Error::Config("Manifest URL not configured".to_string()));
        }

        debug!(url = %manifest_url, "Checking for updates");

        let response = self
            .client
            .get(manifest_url)
            .send()
            .await
            .map_err(|e| Error::Other(format!("Failed to fetch manifest: {}", e)))?;

        if !response.status().is_success() {
            return Err(Error::Other(format!(
                "Failed to fetch manifest: HTTP {}",
                response.status()
            )));
        }

        let manifest: Manifest = response
            .json()
            .await
            .map_err(|e| Error::Other(format!("Failed to parse manifest: {}", e)))?;

        // Check if channel matches
        if manifest.channel != self.config.auto_upgrade.channel {
            debug!(
                manifest_channel = %manifest.channel,
                config_channel = %self.config.auto_upgrade.channel,
                "Channel mismatch"
            );
            return Ok(None);
        }

        Ok(Some(manifest))
    }

    pub async fn download(&self, manifest: &Manifest) -> Result<PathBuf> {
        let (os, arch) = get_current_platform();
        
        let artifact = manifest
            .get_artifact(&os, &arch)
            .ok_or_else(|| Error::NotFound(format!("No artifact for {}/{}", os, arch)))?;

        info!(url = %artifact.url, "Downloading update");

        let response = self
            .client
            .get(&artifact.url)
            .send()
            .await
            .map_err(|e| Error::Other(format!("Download failed: {}", e)))?;

        let bytes = response
            .bytes()
            .await
            .map_err(|e| Error::Other(format!("Failed to read download: {}", e)))?;

        // Verify SHA256
        let hash = Sha256Verifier::compute(&bytes);
        if hash != artifact.sha256 {
            return Err(Error::Validation(format!(
                "SHA256 mismatch: expected {}, got {}",
                artifact.sha256, hash
            )));
        }
        info!("SHA256 verification passed");

        // Verify signature if required
        if self.config.auto_upgrade.require_signature {
            self.verify_signature(manifest, &bytes, artifact.sig.as_deref())?;
        }

        // Save to staging
        let staging_dir = self.paths.update_dir().join("staging");
        std::fs::create_dir_all(&staging_dir)?;

        let staging_path = staging_dir.join(format!("blockcell-{}", manifest.version));
        std::fs::write(&staging_path, &bytes)?;

        info!(path = %staging_path.display(), "Update downloaded and verified");

        Ok(staging_path)
    }

    pub fn status(&self) -> UpdateStatus {
        let current_version = env!("CARGO_PKG_VERSION").to_string();
        
        UpdateStatus {
            current_version,
            latest_version: None,
            update_available: false,
            staging_path: None,
        }
    }

    pub async fn apply(&self, staging_path: &PathBuf, version: &str) -> Result<()> {
        info!(version = %version, "Applying update");

        // 1. 检查维护窗口
        let window = MaintenanceWindow::new(self.config.auto_upgrade.maintenance_window.clone());
        if !window.is_in_window() {
            return Err(Error::Other(
                "Not in maintenance window, update postponed".to_string(),
            ));
        }

        // 2. 运行 Healthcheck（在切换前）
        let checker = HealthChecker::new(staging_path.clone());
        let health_result = checker.check(30).await?;
        
        if !health_result.passed {
            error!("Healthcheck failed before switch");
            for check in &health_result.checks {
                if !check.passed {
                    error!(check = %check.name, message = %check.message, "Failed check");
                }
            }
            return Err(Error::Validation("Healthcheck failed".to_string()));
        }
        info!("Pre-switch healthcheck passed");

        // 3. 原子切换
        self.switcher.switch_to_new(staging_path, version).await?;

        // 4. 运行 Healthcheck（切换后）
        // 注意：这里需要重启进程，所以实际上这个检查应该在重启后由外部进程执行
        // 这里我们只是验证文件已正确替换
        info!("Update applied successfully. Restart required.");

        Ok(())
    }

    pub async fn rollback(&self) -> Result<()> {
        warn!("Rolling back to previous version");
        
        self.switcher.rollback().await?;
        
        info!("Rollback completed. Restart required.");
        Ok(())
    }

    /// 验证签名
    fn verify_signature(
        &self,
        _manifest: &Manifest,
        data: &[u8],
        signature: Option<&str>,
    ) -> Result<()> {
        let sig = signature.ok_or_else(|| {
            Error::Validation("Signature required but not provided".to_string())
        })?;

        // 从环境变量或配置获取公钥
        let public_key_hex = std::env::var("BLOCKCELL_PUBLIC_KEY")
            .or_else(|_| std::env::var("BLOCKCELL_VERIFY_KEY"))
            .map_err(|_| Error::Config("Public key not configured".to_string()))?;

        let verifier = SignatureVerifier::from_hex(&public_key_hex)?;
        verifier.verify(data, sig)?;

        info!("Signature verification passed");
        Ok(())
    }

    /// 执行完整的更新流程
    pub async fn update_flow(&self) -> Result<()> {
        // 1. 检查更新
        info!("Checking for updates...");
        let manifest = match self.check().await? {
            Some(m) => m,
            None => {
                info!("No updates available");
                return Ok(());
            }
        };

        let current_version = env!("CARGO_PKG_VERSION");
        if manifest.version == current_version {
            info!("Already on latest version");
            return Ok(());
        }

        // 检查最低主机版本兼容性
        if let Some(ref min_version) = manifest.min_host_version {
            if !Self::version_satisfies(current_version, min_version) {
                return Err(Error::Validation(format!(
                    "Current version {} does not meet minimum required version {}. Manual upgrade required.",
                    current_version, min_version
                )));
            }
        }

        info!(
            current = %current_version,
            latest = %manifest.version,
            "Update available"
        );

        // 2. 下载
        let staging_path = self.download(&manifest).await?;

        // 3. 应用
        self.apply(&staging_path, &manifest.version).await?;

        Ok(())
    }

    /// 检查当前版本是否满足最低版本要求 (简单的 semver 比较)
    fn version_satisfies(current: &str, minimum: &str) -> bool {
        let parse = |v: &str| -> Vec<u64> {
            v.trim_start_matches('v')
                .split('.')
                .filter_map(|s| s.parse::<u64>().ok())
                .collect()
        };
        let cur = parse(current);
        let min = parse(minimum);
        let max_len = cur.len().max(min.len());
        for i in 0..max_len {
            let c = cur.get(i).copied().unwrap_or(0);
            let m = min.get(i).copied().unwrap_or(0);
            if c > m { return true; }
            if c < m { return false; }
        }
        true // equal
    }
}

fn get_current_platform() -> (String, String) {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    };

    (os.to_string(), arch.to_string())
}
