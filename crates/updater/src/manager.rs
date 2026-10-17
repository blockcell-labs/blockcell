use crate::atomic::{AtomicSwitcher, MaintenanceWindow};
use crate::manifest::Manifest;
use crate::verification::{HealthChecker, Sha256Verifier, SignatureVerifier};
use blockcell_core::{Config, Error, Paths, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info, warn};

const MAX_ARTIFACT_SIZE: u64 = 256 * 1024 * 1024;
const MAX_MANIFEST_SIZE: usize = 1024 * 1024;
const STAGING_METADATA_FILE: &str = "current.json";
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedUpdate {
    pub version: String,
    pub path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StagedUpdateMetadata {
    version: String,
    file_name: String,
    channel: String,
    os: String,
    arch: String,
    url: String,
    sha256: String,
    #[serde(default)]
    sig: Option<String>,
    #[serde(default)]
    min_host_version: Option<String>,
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
            client: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if attempt.previous().len() >= 5 {
                        return attempt.error("too many update redirects");
                    }
                    if redirect_target_is_allowed(attempt.previous(), attempt.url()) {
                        attempt.follow()
                    } else {
                        attempt.stop()
                    }
                }))
                .build()
                .expect("static updater HTTP client configuration must be valid"),
            switcher,
        }
    }

    pub async fn check(&self) -> Result<Option<Manifest>> {
        let manifest_url = &self.config.auto_upgrade.manifest_url;
        if manifest_url.is_empty() {
            return Err(Error::Config("Manifest URL not configured".to_string()));
        }

        debug!(url = %manifest_url, "Checking for updates");
        validate_update_url(manifest_url)?;

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

        validate_update_url(response.url().as_str())?;
        let manifest_bytes = read_response_limited(response, MAX_MANIFEST_SIZE, "manifest").await?;
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
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

        let current_version = env!("CARGO_PKG_VERSION");
        if !Self::version_greater(&manifest.version, current_version) {
            debug!(
                current = %current_version,
                manifest = %manifest.version,
                "Already on latest version or manifest is not newer"
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
        validate_update_url(&artifact.url)?;

        let mut response = self
            .client
            .get(&artifact.url)
            .send()
            .await
            .map_err(|e| Error::Other(format!("Download failed: {}", e)))?;

        if !response.status().is_success() {
            return Err(Error::Other(format!(
                "Download failed: HTTP {}",
                response.status()
            )));
        }
        validate_update_url(response.url().as_str())?;

        if let Some(content_length) = response.content_length() {
            if content_length > MAX_ARTIFACT_SIZE {
                return Err(Error::Validation(format!(
                    "Update artifact is too large: {} bytes (maximum {})",
                    content_length, MAX_ARTIFACT_SIZE
                )));
            }
        }

        let version_file_name = Self::version_file_name(&manifest.version)?;
        let staging_dir = self.staging_dir();
        std::fs::create_dir_all(&staging_dir)?;
        let staging_path = staging_dir.join(&version_file_name);
        let temp_path = unique_temp_path(&staging_dir, "artifact");

        let result: Result<PathBuf> = async {
            let mut file = tokio::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp_path)
                .await?;
            let mut hasher = Sha256::new();
            let mut downloaded = 0u64;

            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| Error::Other(format!("Failed to read download: {}", e)))?
            {
                downloaded = downloaded.checked_add(chunk.len() as u64).ok_or_else(|| {
                    Error::Validation("Update artifact size overflow".to_string())
                })?;
                if downloaded > MAX_ARTIFACT_SIZE {
                    return Err(Error::Validation(format!(
                        "Update artifact is too large: more than {} bytes",
                        MAX_ARTIFACT_SIZE
                    )));
                }
                file.write_all(&chunk).await?;
                hasher.update(&chunk);
            }
            file.flush().await?;
            file.sync_all().await?;
            drop(file);

            let hash = format!("{:x}", hasher.finalize());
            if hash != artifact.sha256 {
                return Err(Error::Validation(format!(
                    "SHA256 mismatch: expected {}, got {}",
                    artifact.sha256, hash
                )));
            }
            info!("SHA256 verification passed");

            if self.config.auto_upgrade.require_signature {
                self.verify_signature(manifest, artifact)?;
            } else {
                warn!(
                    "requireSignature is disabled: installing update verified by SHA256 only. \
                     A tampered manifest could serve a malicious binary. \
                     Enable signature verification (requireSignature: true) for fail-closed safety."
                );
            }

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&temp_path)?.permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&temp_path, perms)?;
            }

            replace_staging_file(&temp_path, &staging_path)?;
            self.persist_staged_update(&StagedUpdateMetadata {
                version: manifest.version.clone(),
                file_name: version_file_name,
                channel: manifest.channel.clone(),
                os: artifact.os.clone(),
                arch: artifact.arch.clone(),
                url: artifact.url.clone(),
                sha256: artifact.sha256.clone(),
                sig: artifact.sig.clone(),
                min_host_version: manifest.min_host_version.clone(),
            })?;

            Ok(staging_path.clone())
        }
        .await;

        if result.is_err() {
            let _ = std::fs::remove_file(&temp_path);
        }
        let staging_path = result?;

        info!(path = %staging_path.display(), "Update downloaded and verified");

        Ok(staging_path)
    }

    pub fn staged_update(&self) -> Result<Option<StagedUpdate>> {
        let metadata_path = self.staging_dir().join(STAGING_METADATA_FILE);
        let bytes = match std::fs::read(&metadata_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata: StagedUpdateMetadata = serde_json::from_slice(&bytes)?;
        let file_name = Path::new(&metadata.file_name);
        if file_name.components().count() != 1
            || !matches!(file_name.components().next(), Some(Component::Normal(_)))
        {
            return Err(Error::Validation(
                "Invalid staged update file name".to_string(),
            ));
        }
        let path = self.staging_dir().join(file_name);
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(StagedUpdate {
            version: metadata.version,
            path,
        }))
    }

    fn staging_dir(&self) -> PathBuf {
        self.paths.update_dir().join("staging")
    }

    fn version_file_name(version: &str) -> Result<String> {
        if version.is_empty()
            || !version
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || ".-_+".contains(character))
        {
            return Err(Error::Validation(format!(
                "Invalid update version for staging: {}",
                version
            )));
        }
        Ok(format!("blockcell-{}", version))
    }

    fn persist_staged_update(&self, metadata: &StagedUpdateMetadata) -> Result<()> {
        use std::io::Write;

        let staging_dir = self.staging_dir();
        std::fs::create_dir_all(&staging_dir)?;
        let metadata_path = staging_dir.join(STAGING_METADATA_FILE);
        let temp_path = unique_temp_path(&staging_dir, "metadata");
        let result: Result<()> = (|| {
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp_path)?;
            serde_json::to_writer(&mut file, metadata)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            replace_staging_file(&temp_path, &metadata_path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp_path);
        }
        result
    }

    fn clear_staged_update_metadata(&self) -> Result<()> {
        match std::fs::remove_file(self.staging_dir().join(STAGING_METADATA_FILE)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn load_staged_update_metadata(&self) -> Result<StagedUpdateMetadata> {
        let metadata_path = self.staging_dir().join(STAGING_METADATA_FILE);
        let metadata: StagedUpdateMetadata =
            serde_json::from_slice(&std::fs::read(metadata_path)?)?;
        let file_name = Path::new(&metadata.file_name);
        if file_name.components().count() != 1
            || !matches!(file_name.components().next(), Some(Component::Normal(_)))
        {
            return Err(Error::Validation(
                "Invalid staged update file name".to_string(),
            ));
        }
        Ok(metadata)
    }

    pub async fn status(&self) -> Result<UpdateStatus> {
        let current_version = env!("CARGO_PKG_VERSION").to_string();
        let staged = self.staged_update()?;
        let remote = self.check().await?;
        let latest_version = remote
            .as_ref()
            .map(|manifest| manifest.version.clone())
            .or_else(|| staged.as_ref().map(|update| update.version.clone()));
        let update_available = remote.is_some() || staged.is_some();
        let staging_path = staged.map(|update| update.path);

        Ok(UpdateStatus {
            current_version,
            latest_version,
            update_available,
            staging_path,
        })
    }

    pub async fn apply(&self, staging_path: &std::path::Path, version: &str) -> Result<()> {
        self.apply_with_options(staging_path, version, false).await
    }

    pub async fn apply_with_options(
        &self,
        staging_path: &std::path::Path,
        version: &str,
        allow_downgrade: bool,
    ) -> Result<()> {
        info!(version = %version, allow_downgrade = allow_downgrade, "Applying update");

        // 暂存文件可能在 download 与 apply 之间被破坏或替换；安装前必须重新建立
        // 文件、元数据和发布签名之间的完整信任链。
        let metadata = self.load_staged_update_metadata()?;
        let expected_path = self.staging_dir().join(&metadata.file_name);
        if staging_path != expected_path || version != metadata.version {
            return Err(Error::Validation(
                "Staged update does not match persisted verification metadata".to_string(),
            ));
        }

        let current_version = env!("CARGO_PKG_VERSION");
        if !allow_downgrade && !Self::version_greater(version, current_version) {
            return Err(Error::Validation(format!(
                "Refusing to apply version {} over current version {} (not an upgrade; pass --force to allow downgrade/reinstall)",
                version, current_version
            )));
        }
        if let Some(ref min_version) = metadata.min_host_version {
            if !allow_downgrade && !Self::version_satisfies(current_version, min_version) {
                return Err(Error::Validation(format!(
                    "Current version {} does not meet minimum required host version {} for update {}",
                    current_version, min_version, version
                )));
            }
        }

        Sha256Verifier::verify_file(staging_path, &metadata.sha256)?;
        if self.config.auto_upgrade.require_signature {
            let manifest = metadata.as_manifest();
            self.verify_signature(&manifest, &manifest.artifacts[0])?;
        }

        // 1. 检查维护窗口
        let window = MaintenanceWindow::new(self.config.auto_upgrade.maintenance_window.clone());
        if !window.is_in_window() {
            return Err(Error::Other(
                "Not in maintenance window, update postponed".to_string(),
            ));
        }

        // 2. 运行 Healthcheck（在切换前）
        let checker = HealthChecker::new(staging_path.to_path_buf());
        let health_result = checker.check_expected_version(30, version).await?;

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

        // 4. 对实际替换后的路径再次检查；若复制/权限/替换结果不可运行，立即恢复备份。
        let post_switch = HealthChecker::new(self.switcher.current_binary_path().to_path_buf())
            .check_expected_version(30, version)
            .await?;
        if !post_switch.passed {
            error!("Healthcheck failed after switch; rolling back");
            self.switcher.rollback(None).await?;
            return Err(Error::Validation(
                "Post-switch healthcheck failed; previous version restored".to_string(),
            ));
        }

        self.clear_staged_update_metadata()?;
        info!("Update applied successfully. Restart required.");

        Ok(())
    }

    pub async fn rollback(&self, version: Option<&str>) -> Result<()> {
        warn!("Rolling back to previous version");

        self.switcher.rollback(version).await?;

        info!("Rollback completed. Restart required.");
        Ok(())
    }

    /// 验证签名
    fn verify_signature(
        &self,
        manifest: &Manifest,
        artifact: &crate::manifest::Artifact,
    ) -> Result<()> {
        let sig = artifact
            .sig
            .as_deref()
            .ok_or_else(|| Error::Validation("Signature required but not provided".to_string()))?;

        // 从环境变量或配置获取公钥
        let public_key_hex = self
            .config
            .auto_upgrade
            .public_key
            .clone()
            .or_else(|| std::env::var("BLOCKCELL_PUBLIC_KEY").ok())
            .or_else(|| std::env::var("BLOCKCELL_VERIFY_KEY").ok())
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| Error::Config("Public key not configured".to_string()))?;

        let verifier = SignatureVerifier::from_hex(&public_key_hex)?;
        verifier.verify(&manifest.signature_payload(artifact), sig)?;

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
        // 使用语义版本比较：若 manifest 版本不高于当前版本，无需更新
        if !Self::version_greater(&manifest.version, current_version) {
            info!(
                current = %current_version,
                manifest = %manifest.version,
                "Already on latest version or manifest is older"
            );
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

    /// 检查当前版本是否满足最低版本要求 (semver 比较，正确处理 pre-release)
    fn version_satisfies(current: &str, minimum: &str) -> bool {
        let cur = current.trim_start_matches('v');
        let min = minimum.trim_start_matches('v');
        // 优先使用 semver 语义比较，正确处理 pre-release 标签
        if let (Ok(cv), Ok(mv)) = (semver::Version::parse(cur), semver::Version::parse(min)) {
            return cv >= mv;
        }
        // 回退到旧的数字比较
        Self::parse_numeric(cur) >= Self::parse_numeric(min)
    }

    /// 检查 candidate 版本是否严格大于 base 版本 (semver 比较，正确处理 pre-release)
    fn version_greater(candidate: &str, base: &str) -> bool {
        let c = candidate.trim_start_matches('v');
        let b = base.trim_start_matches('v');
        // 优先使用 semver 语义比较，正确处理 pre-release 标签
        // 例如: "1.0.0" > "1.0.0-beta.1" 为 true（正式版大于预发布版）
        if let (Ok(cv), Ok(bv)) = (semver::Version::parse(c), semver::Version::parse(b)) {
            return cv > bv;
        }
        // 回退到旧的数字比较
        Self::parse_numeric(c) > Self::parse_numeric(b)
    }

    /// 旧的数字回退解析: 逐段取数字部分，用于非标准 semver 格式
    fn parse_numeric(v: &str) -> Vec<u64> {
        v.split('.')
            .filter_map(|s| {
                s.split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
            })
            .collect()
    }
}

impl StagedUpdateMetadata {
    fn as_manifest(&self) -> Manifest {
        Manifest {
            channel: self.channel.clone(),
            version: self.version.clone(),
            published_at: String::new(),
            artifacts: vec![crate::manifest::Artifact {
                os: self.os.clone(),
                arch: self.arch.clone(),
                url: self.url.clone(),
                sha256: self.sha256.clone(),
                sig: self.sig.clone(),
            }],
            min_host_version: self.min_host_version.clone(),
            notes: String::new(),
        }
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

fn validate_update_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value)
        .map_err(|error| Error::Validation(format!("Invalid update URL: {error}")))?;
    if url.scheme() == "https" {
        return Ok(());
    }
    let is_loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if url.scheme() == "http" && is_loopback {
        return Ok(());
    }
    Err(Error::Validation(
        "Remote update URLs must use HTTPS".to_string(),
    ))
}

fn redirect_target_is_allowed(previous: &[reqwest::Url], target: &reqwest::Url) -> bool {
    !(previous.iter().any(|url| url.scheme() == "https") && target.scheme() != "https")
}

async fn read_response_limited(
    mut response: reqwest::Response,
    maximum: usize,
    label: &str,
) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum as u64)
    {
        return Err(Error::Validation(format!(
            "Update {label} is too large (maximum {maximum} bytes)"
        )));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| Error::Other(format!("Failed to read update {label}: {error}")))?
    {
        if bytes.len().saturating_add(chunk.len()) > maximum {
            return Err(Error::Validation(format!(
                "Update {label} is too large (maximum {maximum} bytes)"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn unique_temp_path(directory: &Path, label: &str) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    directory.join(format!(
        ".blockcell-{}-{}-{}-{}.tmp",
        label,
        std::process::id(),
        timestamp,
        counter
    ))
}

fn replace_staging_file(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(windows)]
    if destination.exists() {
        std::fs::remove_file(destination)?;
    }
    std::fs::rename(source, destination)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Artifact;
    use ed25519_dalek::Signer;
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn serve_once(status: &str, headers: &[(&str, String)], body: &[u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let status = status.to_string();
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect();
        let body = body.to_vec();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 4096];
            let _ = socket.read(&mut request).await;

            let mut response = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
            for (name, value) in headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            response.push_str("\r\n");
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        });

        format!("http://{addr}/artifact")
    }

    fn manager(temp: &TempDir) -> UpdateManager {
        let mut config = Config::default();
        config.auto_upgrade.require_signature = false;
        UpdateManager::new(config, Paths::with_base(temp.path().to_path_buf()))
    }

    fn manager_with_manifest_url(temp: &TempDir, manifest_url: String) -> UpdateManager {
        let mut config = Config::default();
        config.auto_upgrade.require_signature = false;
        config.auto_upgrade.manifest_url = manifest_url;
        UpdateManager::new(config, Paths::with_base(temp.path().to_path_buf()))
    }

    fn manifest(url: String, body: &[u8]) -> Manifest {
        let (os, arch) = get_current_platform();
        Manifest {
            channel: "stable".to_string(),
            version: "9.9.9".to_string(),
            published_at: "2026-07-21T00:00:00Z".to_string(),
            artifacts: vec![Artifact {
                os,
                arch,
                url,
                sha256: format!("{:x}", Sha256::digest(body)),
                sig: None,
            }],
            min_host_version: None,
            notes: String::new(),
        }
    }

    #[tokio::test]
    async fn download_rejects_non_success_http_status() {
        let temp = TempDir::new().unwrap();
        let body = b"not found";
        let url = serve_once(
            "404 Not Found",
            &[("Content-Length", body.len().to_string())],
            body,
        )
        .await;

        let error = manager(&temp)
            .download(&manifest(url, body))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("HTTP 404"), "{error}");
    }

    #[tokio::test]
    async fn download_rejects_artifact_larger_than_limit() {
        let temp = TempDir::new().unwrap();
        let url = serve_once(
            "200 OK",
            &[("Content-Length", (MAX_ARTIFACT_SIZE + 1).to_string())],
            &[],
        )
        .await;

        let error = manager(&temp)
            .download(&manifest(url, &[]))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("too large"), "{error}");
    }

    #[tokio::test]
    async fn successful_download_persists_staging_metadata() {
        let temp = TempDir::new().unwrap();
        let body = b"signed update bytes";
        let url = serve_once(
            "200 OK",
            &[("Content-Length", body.len().to_string())],
            body,
        )
        .await;
        let manager = manager(&temp);

        let staged_path = manager.download(&manifest(url, body)).await.unwrap();

        assert_eq!(std::fs::read(staged_path).unwrap(), body);
        let metadata_path = temp.path().join("update/staging/current.json");
        let metadata: serde_json::Value =
            serde_json::from_slice(&std::fs::read(metadata_path).unwrap()).unwrap();
        assert_eq!(metadata["version"], "9.9.9");
        assert_eq!(metadata["fileName"], "blockcell-9.9.9");
    }

    #[tokio::test]
    async fn status_reports_remote_update() {
        let temp = TempDir::new().unwrap();
        let remote_manifest = manifest("http://127.0.0.1/unused".to_string(), b"");
        let body = serde_json::to_vec(&remote_manifest).unwrap();
        let url = serve_once(
            "200 OK",
            &[
                ("Content-Length", body.len().to_string()),
                ("Content-Type", "application/json".to_string()),
            ],
            &body,
        )
        .await;

        let status = manager_with_manifest_url(&temp, url)
            .status()
            .await
            .unwrap();

        assert_eq!(status.latest_version.as_deref(), Some("9.9.9"));
        assert!(status.update_available);
        assert_eq!(status.staging_path, None);
    }

    #[tokio::test]
    async fn status_reports_locally_staged_update() {
        let temp = TempDir::new().unwrap();
        let mut remote_manifest = manifest("http://127.0.0.1/unused".to_string(), b"");
        remote_manifest.channel = "beta".to_string();
        let body = serde_json::to_vec(&remote_manifest).unwrap();
        let url = serve_once(
            "200 OK",
            &[
                ("Content-Length", body.len().to_string()),
                ("Content-Type", "application/json".to_string()),
            ],
            &body,
        )
        .await;
        let manager = manager_with_manifest_url(&temp, url);
        let staging_dir = temp.path().join("update/staging");
        std::fs::create_dir_all(&staging_dir).unwrap();
        let staged_path = staging_dir.join("blockcell-9.9.9");
        std::fs::write(&staged_path, b"update").unwrap();
        manager
            .persist_staged_update(&StagedUpdateMetadata {
                version: "9.9.9".to_string(),
                file_name: "blockcell-9.9.9".to_string(),
                channel: "stable".to_string(),
                os: get_current_platform().0,
                arch: get_current_platform().1,
                url: "https://example.com/blockcell".to_string(),
                sha256: format!("{:x}", Sha256::digest(b"update")),
                sig: None,
                min_host_version: None,
            })
            .unwrap();

        let status = manager.status().await.unwrap();

        assert_eq!(status.latest_version.as_deref(), Some("9.9.9"));
        assert!(status.update_available);
        assert_eq!(status.staging_path.as_deref(), Some(staged_path.as_path()));
    }

    #[tokio::test]
    async fn apply_rejects_a_staged_file_modified_after_download() {
        let temp = TempDir::new().unwrap();
        let manager = manager(&temp);
        let staging_dir = temp.path().join("update/staging");
        std::fs::create_dir_all(&staging_dir).unwrap();
        let staged_path = staging_dir.join("blockcell-9.9.9");
        std::fs::write(&staged_path, b"modified after verification").unwrap();
        manager
            .persist_staged_update(&StagedUpdateMetadata {
                version: "9.9.9".to_string(),
                file_name: "blockcell-9.9.9".to_string(),
                channel: "stable".to_string(),
                os: get_current_platform().0,
                arch: get_current_platform().1,
                url: "https://example.com/blockcell".to_string(),
                sha256: format!("{:x}", Sha256::digest(b"original verified bytes")),
                sig: None,
                min_host_version: None,
            })
            .unwrap();

        let error = manager.apply(&staged_path, "9.9.9").await.unwrap_err();

        assert!(error.to_string().contains("SHA256 mismatch"), "{error}");
    }

    #[tokio::test]
    async fn apply_rejects_downgrade_or_unsatisfied_min_host_unless_forced() {
        let temp = TempDir::new().unwrap();
        let manager = manager(&temp);
        let staging_dir = temp.path().join("update/staging");
        std::fs::create_dir_all(&staging_dir).unwrap();

        let old_path = staging_dir.join("blockcell-0.0.1");
        std::fs::write(&old_path, b"old binary").unwrap();
        manager
            .persist_staged_update(&StagedUpdateMetadata {
                version: "0.0.1".to_string(),
                file_name: "blockcell-0.0.1".to_string(),
                channel: "stable".to_string(),
                os: get_current_platform().0,
                arch: get_current_platform().1,
                url: "https://example.com/blockcell".to_string(),
                sha256: format!("{:x}", Sha256::digest(b"old binary")),
                sig: None,
                min_host_version: None,
            })
            .unwrap();

        let downgrade_err = manager.apply(&old_path, "0.0.1").await.unwrap_err();
        assert!(
            downgrade_err.to_string().contains("Refusing to apply"),
            "{downgrade_err}"
        );

        let future_path = staging_dir.join("blockcell-9.9.9");
        std::fs::write(&future_path, b"future binary").unwrap();
        manager
            .persist_staged_update(&StagedUpdateMetadata {
                version: "9.9.9".to_string(),
                file_name: "blockcell-9.9.9".to_string(),
                channel: "stable".to_string(),
                os: get_current_platform().0,
                arch: get_current_platform().1,
                url: "https://example.com/blockcell".to_string(),
                sha256: format!("{:x}", Sha256::digest(b"future binary")),
                sig: None,
                min_host_version: Some("99.0.0".to_string()),
            })
            .unwrap();

        let min_host_err = manager.apply(&future_path, "9.9.9").await.unwrap_err();
        assert!(
            min_host_err
                .to_string()
                .contains("minimum required host version"),
            "{min_host_err}"
        );
    }

    #[test]
    fn remote_update_urls_must_use_https() {
        let error = validate_update_url("http://example.com/manifest.json").unwrap_err();
        assert!(error.to_string().contains("HTTPS"), "{error}");

        validate_update_url("http://127.0.0.1:8080/manifest.json").unwrap();
        validate_update_url("https://example.com/manifest.json").unwrap();
    }

    #[test]
    fn redirects_cannot_downgrade_https_to_http() {
        let previous = reqwest::Url::parse("https://example.com/manifest.json").unwrap();
        let insecure = reqwest::Url::parse("http://example.com/manifest.json").unwrap();
        let secure = reqwest::Url::parse("https://cdn.example.com/manifest.json").unwrap();

        assert!(!redirect_target_is_allowed(
            std::slice::from_ref(&previous),
            &insecure
        ));
        assert!(redirect_target_is_allowed(&[previous], &secure));
    }

    #[tokio::test]
    async fn check_rejects_manifest_larger_than_limit() {
        let temp = TempDir::new().unwrap();
        let body = vec![b' '; MAX_MANIFEST_SIZE + 1];
        let url = serve_once(
            "200 OK",
            &[
                ("Content-Length", body.len().to_string()),
                ("Content-Type", "application/json".to_string()),
            ],
            &body,
        )
        .await;

        let error = manager_with_manifest_url(&temp, url)
            .check()
            .await
            .unwrap_err();

        assert!(
            error.to_string().contains("manifest is too large"),
            "{error}"
        );
    }

    // ── Signature gate on the download/apply path ──────────────────────────

    fn signing_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[9u8; 32])
    }

    fn signed_manifest(key: &ed25519_dalek::SigningKey, url: &str) -> Manifest {
        let mut manifest = manifest(url.to_string(), b"artifact bytes");
        let payload = manifest.signature_payload(&manifest.artifacts[0]);
        let signature = key.sign(&payload);
        manifest.artifacts[0].sig = Some(hex::encode(signature.to_bytes()));
        manifest
    }

    fn manager_with_public_key(temp: &TempDir, key: &ed25519_dalek::SigningKey) -> UpdateManager {
        let mut config = Config::default();
        config.auto_upgrade.require_signature = true;
        config.auto_upgrade.public_key = Some(hex::encode(key.verifying_key().to_bytes()));
        UpdateManager::new(config, Paths::with_base(temp.path().to_path_buf()))
    }

    #[test]
    fn verify_signature_accepts_a_valid_signature() {
        let temp = TempDir::new().unwrap();
        let key = signing_key();
        let manifest = signed_manifest(&key, "https://example.invalid/blockcell");

        assert!(manager_with_public_key(&temp, &key)
            .verify_signature(&manifest, &manifest.artifacts[0])
            .is_ok());
    }

    #[test]
    fn verify_signature_rejects_a_rewritten_download_url() {
        let temp = TempDir::new().unwrap();
        let key = signing_key();
        let mut manifest = signed_manifest(&key, "https://example.invalid/blockcell");
        // A MITM repoints the artifact at another host after the signature was made.
        manifest.artifacts[0].url = "https://evil.invalid/blockcell".to_string();

        let error = manager_with_public_key(&temp, &key)
            .verify_signature(&manifest, &manifest.artifacts[0])
            .unwrap_err();

        assert!(error.to_string().contains("verification failed"), "{error}");
    }

    #[test]
    fn verify_signature_rejects_a_rewritten_version() {
        let temp = TempDir::new().unwrap();
        let key = signing_key();
        let mut manifest = signed_manifest(&key, "https://example.invalid/blockcell");
        // Replaying an old signature under a new version number must fail.
        manifest.version = "999.0.0".to_string();

        assert!(manager_with_public_key(&temp, &key)
            .verify_signature(&manifest, &manifest.artifacts[0])
            .is_err());
    }

    #[test]
    fn verify_signature_rejects_a_signature_from_another_key() {
        let temp = TempDir::new().unwrap();
        let publisher = signing_key();
        let attacker = ed25519_dalek::SigningKey::from_bytes(&[10u8; 32]);
        let manifest = signed_manifest(&publisher, "https://example.invalid/blockcell");

        // The verifier trusts the attacker's key; the publisher's signature must not pass.
        assert!(manager_with_public_key(&temp, &attacker)
            .verify_signature(&manifest, &manifest.artifacts[0])
            .is_err());
    }

    #[test]
    fn verify_signature_requires_a_signature() {
        let temp = TempDir::new().unwrap();
        let key = signing_key();
        let mut manifest = signed_manifest(&key, "https://example.invalid/blockcell");
        manifest.artifacts[0].sig = None;

        let error = manager_with_public_key(&temp, &key)
            .verify_signature(&manifest, &manifest.artifacts[0])
            .unwrap_err();

        assert!(error.to_string().contains("Signature required"), "{error}");
    }

    // ── Version comparison (upgrade / downgrade decision) ──────────────────

    #[test]
    fn version_comparison_is_semantic_not_lexicographic() {
        assert!(UpdateManager::version_greater("1.2.10", "1.2.9"));
        assert!(!UpdateManager::version_greater("1.2.9", "1.2.10"));
        assert!(!UpdateManager::version_greater("2.0.0", "10.0.0"));
    }

    #[test]
    fn version_greater_handles_v_prefix_and_prereleases() {
        assert!(UpdateManager::version_greater("v1.2.4", "1.2.3"));
        assert!(!UpdateManager::version_greater("1.2.3", "1.2.3"));
        // A release outranks its own pre-release, in both directions.
        assert!(UpdateManager::version_greater("1.0.0", "1.0.0-beta.1"));
        assert!(!UpdateManager::version_greater("1.0.0-beta.1", "1.0.0"));
    }

    #[test]
    fn version_satisfies_minimum_host_version() {
        assert!(UpdateManager::version_satisfies("1.2.3", "1.2.3"));
        assert!(UpdateManager::version_satisfies("v1.3.0", "1.2.3"));
        assert!(!UpdateManager::version_satisfies("1.2.2", "1.2.3"));
    }

    #[test]
    fn version_comparison_falls_back_to_numeric_segments() {
        // "1.2" is not valid semver, so the numeric fallback must kick in.
        assert!(UpdateManager::version_greater("1.2", "1.1"));
        assert!(UpdateManager::version_greater("1.10", "1.2"));
        assert!(!UpdateManager::version_greater("1.1", "1.2"));
    }
}
