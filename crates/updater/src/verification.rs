use base64::Engine;
use blockcell_core::{Error, Result};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use std::path::Path;
use tracing::{debug, info};

/// 签名验证器
pub struct SignatureVerifier {
    public_key: VerifyingKey,
}

impl SignatureVerifier {
    /// 从 PEM 格式的公钥创建验证器
    pub fn from_pem(pem: &str) -> Result<Self> {
        // 解析 PEM 格式
        let decoded = Self::decode_pem(pem)?;
        // ed25519 SubjectPublicKeyInfo (PKCS#8) 格式包含 12 字节 ASN.1 头部，
        // 原始公钥在最后 32 字节；若已是裸 32 字节则直接使用
        let key_bytes: [u8; 32] = if decoded.len() == 32 {
            decoded.try_into().unwrap()
        } else if decoded.len() > 32 {
            decoded[decoded.len() - 32..].try_into().map_err(|_| {
                Error::Validation("Failed to extract key bytes from PEM".to_string())
            })?
        } else {
            return Err(Error::Validation(format!(
                "PEM decoded data too short: {} bytes (expected >= 32)",
                decoded.len()
            )));
        };

        let public_key = VerifyingKey::from_bytes(&key_bytes)
            .map_err(|e| Error::Validation(format!("Invalid public key: {}", e)))?;

        Ok(Self { public_key })
    }

    /// 从十六进制字符串创建验证器
    pub fn from_hex(hex: &str) -> Result<Self> {
        let bytes =
            hex::decode(hex).map_err(|e| Error::Validation(format!("Invalid hex: {}", e)))?;
        let key_bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| Error::Validation("Public key must be 32 bytes".to_string()))?;

        let public_key = VerifyingKey::from_bytes(&key_bytes)
            .map_err(|e| Error::Validation(format!("Invalid public key: {}", e)))?;

        Ok(Self { public_key })
    }

    /// 验证签名（自动检测 hex 或 base64 格式）
    pub fn verify(&self, message: &[u8], signature_str: &str) -> Result<()> {
        // 自动检测签名编码格式：hex 只含 0-9a-fA-F，否则尝试 base64
        let sig_bytes = if signature_str.chars().all(|c| c.is_ascii_hexdigit())
            && signature_str.len() == 128
        {
            hex::decode(signature_str)
                .map_err(|e| Error::Validation(format!("Invalid signature hex: {}", e)))?
        } else {
            base64::engine::general_purpose::STANDARD
                .decode(signature_str)
                .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(signature_str))
                .map_err(|e| {
                    Error::Validation(format!(
                        "Invalid signature encoding (not hex or base64): {}",
                        e
                    ))
                })?
        };

        let sig_array: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| Error::Validation("Signature must be 64 bytes".to_string()))?;
        let signature = Signature::from_bytes(&sig_array);

        self.public_key
            .verify(message, &signature)
            .map_err(|e| Error::Validation(format!("Signature verification failed: {}", e)))?;

        info!("Signature verified successfully");
        Ok(())
    }

    fn decode_pem(pem: &str) -> Result<Vec<u8>> {
        // 简化的 PEM 解析
        let lines: Vec<&str> = pem.lines().collect();
        let mut base64_data = String::new();

        for line in lines {
            if line.starts_with("-----") {
                continue;
            }
            base64_data.push_str(line.trim());
        }

        base64::engine::general_purpose::STANDARD
            .decode(&base64_data)
            .map_err(|e| Error::Validation(format!("Failed to decode PEM: {}", e)))
    }
}

/// SHA256 校验器
pub struct Sha256Verifier;

impl Sha256Verifier {
    /// 计算文件的 SHA256
    pub fn compute_file(path: &Path) -> Result<String> {
        let bytes = std::fs::read(path)?;
        Ok(Self::compute(&bytes))
    }

    /// 计算字节数组的 SHA256
    pub fn compute(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("{:x}", hasher.finalize())
    }

    /// 验证文件的 SHA256
    pub fn verify_file(path: &Path, expected: &str) -> Result<()> {
        let actual = Self::compute_file(path)?;

        if actual != expected {
            return Err(Error::Validation(format!(
                "SHA256 mismatch: expected {}, got {}",
                expected, actual
            )));
        }

        debug!(path = %path.display(), "SHA256 verified");
        Ok(())
    }
}

/// Healthcheck 执行器
pub struct HealthChecker {
    binary_path: std::path::PathBuf,
}

impl HealthChecker {
    pub fn new(binary_path: std::path::PathBuf) -> Self {
        Self { binary_path }
    }

    /// 执行健康检查
    pub async fn check(&self, timeout_secs: u64) -> Result<HealthCheckResult> {
        self.check_internal(timeout_secs, None).await
    }

    pub async fn check_expected_version(
        &self,
        timeout_secs: u64,
        expected_version: &str,
    ) -> Result<HealthCheckResult> {
        self.check_internal(timeout_secs, Some(expected_version))
            .await
    }

    async fn check_internal(
        &self,
        timeout_secs: u64,
        expected_version: Option<&str>,
    ) -> Result<HealthCheckResult> {
        info!(binary = %self.binary_path.display(), "Running healthcheck");

        // 1. 检查二进制是否可执行
        if !self.binary_path.exists() {
            return Ok(HealthCheckResult {
                passed: false,
                checks: vec![Check {
                    name: "binary_exists".to_string(),
                    passed: false,
                    message: "Binary not found".to_string(),
                }],
            });
        }

        let mut checks = vec![];

        // 2. 运行 --version 检查
        let version_check = self.check_version(timeout_secs, expected_version).await;
        checks.push(version_check.clone());

        // 3. 运行 --self-check（如果支持）
        let self_check = self.check_self_test(timeout_secs).await;
        checks.push(self_check.clone());

        // 4. 检查依赖库（动态链接）
        #[cfg(unix)]
        {
            let deps_check = self.check_dependencies().await;
            checks.push(deps_check);
        }

        let passed = checks.iter().all(|c| c.passed);

        Ok(HealthCheckResult { passed, checks })
    }

    async fn check_version(&self, timeout_secs: u64, expected_version: Option<&str>) -> Check {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            tokio::process::Command::new(&self.binary_path)
                .kill_on_drop(true)
                .arg("--version")
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) if output.status.success() => {
                let version = String::from_utf8_lossy(&output.stdout);
                let matches = expected_version
                    .map(|expected| version_output_matches(&version, expected))
                    .unwrap_or(true);
                if matches {
                    Check {
                        name: "version".to_string(),
                        passed: true,
                        message: format!("Version: {}", version.trim()),
                    }
                } else {
                    Check {
                        name: "version".to_string(),
                        passed: false,
                        message: format!(
                            "Expected version {}, got {}",
                            expected_version.unwrap_or_default(),
                            version.trim()
                        ),
                    }
                }
            }
            Ok(Ok(output)) => Check {
                name: "version".to_string(),
                passed: false,
                message: format!("Exit code: {}", output.status),
            },
            Ok(Err(e)) => Check {
                name: "version".to_string(),
                passed: false,
                message: format!("Failed to execute: {}", e),
            },
            Err(_) => Check {
                name: "version".to_string(),
                passed: false,
                message: "Timeout".to_string(),
            },
        }
    }

    async fn check_self_test(&self, timeout_secs: u64) -> Check {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            tokio::process::Command::new(&self.binary_path)
                .kill_on_drop(true)
                .arg("--self-check")
                .output(),
        )
        .await;

        match result {
            Ok(Ok(output)) if output.status.success() => Check {
                name: "self_check".to_string(),
                passed: true,
                message: "Self-check passed".to_string(),
            },
            Ok(Ok(output)) if self_check_is_unsupported(&output) => Check {
                name: "self_check".to_string(),
                passed: true,
                message: "Self-check not supported (skipped)".to_string(),
            },
            Ok(Ok(output)) => Check {
                name: "self_check".to_string(),
                passed: false,
                message: format!("Self-check failed with {}", output.status),
            },
            Ok(Err(error)) => Check {
                name: "self_check".to_string(),
                passed: false,
                message: format!("Failed to execute self-check: {}", error),
            },
            Err(_) => Check {
                name: "self_check".to_string(),
                passed: false,
                message: "Self-check Timeout".to_string(),
            },
        }
    }

    #[cfg(unix)]
    async fn check_dependencies(&self) -> Check {
        // 使用 ldd 或 otool 检查动态库依赖
        #[cfg(target_os = "macos")]
        let cmd = "otool";
        #[cfg(target_os = "macos")]
        let args = vec!["-L", self.binary_path.to_str().unwrap()];

        #[cfg(target_os = "linux")]
        let cmd = "ldd";
        #[cfg(target_os = "linux")]
        let args = vec![self.binary_path.to_str().unwrap()];

        // 对于其他 Unix 系统（如 Android Termux），尝试使用 ldd
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let (cmd, args) = ("ldd", vec![self.binary_path.to_str().unwrap_or("")]);

        let result = tokio::process::Command::new(cmd).args(&args).output().await;

        match result {
            Ok(output) if output.status.success() => {
                let deps = String::from_utf8_lossy(&output.stdout);
                let missing = deps.lines().any(|line| line.contains("not found"));

                Check {
                    name: "dependencies".to_string(),
                    passed: !missing,
                    message: if missing {
                        "Missing dependencies detected".to_string()
                    } else {
                        "All dependencies satisfied".to_string()
                    },
                }
            }
            _ => Check {
                name: "dependencies".to_string(),
                passed: true, // 不强制要求
                message: "Dependency check skipped".to_string(),
            },
        }
    }
}

fn version_output_matches(output: &str, expected: &str) -> bool {
    let expected = expected.trim_start_matches('v');
    output.split_whitespace().any(|token| {
        token
            .trim_matches(|character: char| {
                !character.is_ascii_alphanumeric()
                    && character != '.'
                    && character != '-'
                    && character != '+'
            })
            .trim_start_matches('v')
            == expected
    })
}

fn self_check_is_unsupported(output: &std::process::Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    stderr.contains("--self-check")
        && (stderr.contains("unexpected argument")
            || stderr.contains("unrecognized option")
            || stderr.contains("unknown option")
            || stderr.contains("unknown argument"))
}

#[derive(Debug, Clone)]
pub struct HealthCheckResult {
    pub passed: bool,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub passed: bool,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    use tempfile::TempDir;

    fn executable_temp_dir() -> TempDir {
        let root = std::env::current_dir()
            .unwrap()
            .join("target/updater-healthcheck-tests");
        std::fs::create_dir_all(&root).unwrap();
        tempfile::Builder::new()
            .prefix("healthcheck-")
            .tempdir_in(root)
            .unwrap()
    }

    #[test]
    fn test_sha256_compute() {
        let data = b"hello world";
        let hash = Sha256Verifier::compute(data);
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[cfg(unix)]
    fn write_script(temp: &TempDir, contents: &str, executable: bool) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = temp.path().join("healthcheck-script");
        std::fs::write(&path, contents).unwrap();
        if executable {
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn self_check_timeout_is_a_failure() {
        let temp = executable_temp_dir();
        let path = write_script(&temp, "#!/bin/sh\nsleep 30\n", true);

        let check = HealthChecker::new(path).check_self_test(3).await;

        assert!(!check.passed);
        assert!(check.message.contains("Timeout"), "{}", check.message);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn self_check_spawn_error_is_a_failure() {
        let temp = executable_temp_dir();
        let path = write_script(&temp, "#!/bin/sh\nexit 0\n", false);

        let check = HealthChecker::new(path).check_self_test(5).await;

        assert!(!check.passed);
        assert!(
            check.message.contains("Failed to execute"),
            "{}",
            check.message
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn explicitly_unsupported_self_check_is_skipped() {
        let temp = executable_temp_dir();
        let path = write_script(
            &temp,
            "#!/bin/sh\necho \"error: unexpected argument '--self-check' found\" >&2\nexit 2\n",
            true,
        );

        let check = HealthChecker::new(path).check_self_test(5).await;

        assert!(check.passed);
        assert!(check.message.contains("not supported"), "{}", check.message);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn expected_version_must_appear_in_version_output() {
        let temp = executable_temp_dir();
        let path = write_script(
            &temp,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'blockcell 1.0.0'; exit 0; fi\necho \"error: unexpected argument '$1' found\" >&2\nexit 2\n",
            true,
        );

        let result = HealthChecker::new(path)
            .check_expected_version(5, "2.0.0")
            .await
            .unwrap();

        assert!(!result.passed);
        assert!(result
            .checks
            .iter()
            .any(|check| check.name == "version" && !check.passed));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timed_out_healthcheck_process_is_terminated() {
        let temp = executable_temp_dir();
        let pid_path = temp.path().join("pid");
        let script = format!("#!/bin/sh\necho $$ > '{}'\nsleep 30\n", pid_path.display());
        let path = write_script(&temp, &script, true);

        let check = HealthChecker::new(path).check_self_test(3).await;
        assert!(!check.passed);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let pid = std::fs::read_to_string(pid_path).unwrap();
        let alive = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        assert!(!alive, "timed-out healthcheck process {pid} is still alive");
    }

    // ── Signature verification (supply-chain critical path) ────────────────

    fn signing_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[42u8; 32])
    }

    fn verifier_for(key: &ed25519_dalek::SigningKey) -> SignatureVerifier {
        SignatureVerifier::from_hex(&hex::encode(key.verifying_key().to_bytes())).unwrap()
    }

    #[test]
    fn signature_verifier_accepts_a_matching_signature() {
        let key = signing_key();
        let signature = key.sign(b"blockcell update payload");

        assert!(verifier_for(&key)
            .verify(
                b"blockcell update payload",
                &hex::encode(signature.to_bytes())
            )
            .is_ok());
    }

    #[test]
    fn signature_verifier_rejects_a_tampered_message() {
        let key = signing_key();
        let signature = key.sign(b"blockcell update payload");

        let error = verifier_for(&key)
            .verify(
                b"blockcell update payloaD",
                &hex::encode(signature.to_bytes()),
            )
            .unwrap_err();

        assert!(error.to_string().contains("verification failed"), "{error}");
    }

    #[test]
    fn signature_verifier_rejects_a_signature_from_another_key() {
        let signer = signing_key();
        let attacker = ed25519_dalek::SigningKey::from_bytes(&[43u8; 32]);
        let signature = signer.sign(b"blockcell update payload");

        assert!(verifier_for(&attacker)
            .verify(
                b"blockcell update payload",
                &hex::encode(signature.to_bytes())
            )
            .is_err());
    }

    #[test]
    fn signature_verifier_accepts_base64_encoded_signatures() {
        let key = signing_key();
        let signature = key.sign(b"blockcell update payload");
        let encoded = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());

        assert!(verifier_for(&key)
            .verify(b"blockcell update payload", &encoded)
            .is_ok());
    }

    #[test]
    fn signature_verifier_rejects_malformed_signature_encodings() {
        let key = signing_key();
        let verifier = verifier_for(&key);

        let error = verifier.verify(b"payload", "!!!not-base64!!!").unwrap_err();
        assert!(
            error.to_string().contains("Invalid signature encoding"),
            "{error}"
        );

        // Valid base64 but not 64 bytes long.
        let error = verifier.verify(b"payload", "deadbeef").unwrap_err();
        assert!(error.to_string().contains("64 bytes"), "{error}");

        // Well-formed 64-byte signature that simply does not match.
        let error = verifier.verify(b"payload", &"ab".repeat(64)).unwrap_err();
        assert!(error.to_string().contains("verification failed"), "{error}");
    }

    #[test]
    fn signature_verifier_rejects_malformed_public_keys() {
        assert!(SignatureVerifier::from_hex("not-hex").is_err());
        // 31 / 33 bytes: a raw ed25519 public key must be exactly 32 bytes.
        assert!(SignatureVerifier::from_hex(&"ab".repeat(31)).is_err());
        assert!(SignatureVerifier::from_hex(&"ab".repeat(33)).is_err());
    }
}
