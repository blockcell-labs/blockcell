use blockcell_core::{Error, Result};
use chrono::Timelike;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// 原子切换管理器
pub struct AtomicSwitcher {
    #[allow(dead_code)]
    install_dir: PathBuf,
    backup_dir: PathBuf,
}

impl AtomicSwitcher {
    pub fn new(install_dir: PathBuf) -> Self {
        let backup_dir = install_dir.join("backups");
        Self {
            install_dir,
            backup_dir,
        }
    }

    /// 原子切换到新版本
    pub async fn switch_to_new(&self, new_binary: &Path, version: &str) -> Result<()> {
        info!(version = %version, "Starting atomic switch");

        // 1. 确保备份目录存在
        std::fs::create_dir_all(&self.backup_dir)?;

        // 2. 备份当前版本
        let current_binary = self.get_current_binary_path()?;
        let backup_path = self.backup_dir.join(format!(
            "blockcell-{}-{}",
            self.get_current_version()?,
            chrono::Utc::now().timestamp()
        ));

        if current_binary.exists() {
            std::fs::copy(&current_binary, &backup_path)?;
            info!(backup = %backup_path.display(), "Current version backed up");
        }

        // 3. 验证新二进制
        self.verify_binary(new_binary)?;

        // 4. 设置可执行权限
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(new_binary)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(new_binary, perms)?;
        }

        // 5. 原子替换（使用 rename，这在大多数文件系统上是原子的）
        #[cfg(unix)]
        {
            // Unix: 直接 rename
            std::fs::rename(new_binary, &current_binary)?;
            info!("Binary replaced atomically");
        }

        #[cfg(windows)]
        {
            // Windows: 需要特殊处理，因为运行中的程序无法替换
            // 方案：重命名当前文件为 .old，然后复制新文件
            let old_path = current_binary.with_extension("old");
            if old_path.exists() {
                std::fs::remove_file(&old_path)?;
            }
            std::fs::rename(&current_binary, &old_path)?;
            std::fs::copy(new_binary, &current_binary)?;
            info!("Binary replaced (Windows mode)");
        }

        // 6. 验证替换成功
        if !current_binary.exists() {
            return Err(Error::Other("Binary replacement failed".to_string()));
        }

        // 7. 清理旧备份（保留最近 N 个）
        self.cleanup_old_backups(5)?;

        info!("Atomic switch completed successfully");
        Ok(())
    }

    /// 回滚到上一个版本
    pub async fn rollback(&self) -> Result<()> {
        warn!("Rolling back to previous version");

        // 1. 找到最新的备份
        let latest_backup = self.find_latest_backup()?;

        // 2. 获取当前二进制路径
        let current_binary = self.get_current_binary_path()?;

        // 3. 备份失败的版本
        let failed_backup = self.backup_dir.join(format!(
            "blockcell-failed-{}",
            chrono::Utc::now().timestamp()
        ));
        if current_binary.exists() {
            std::fs::copy(&current_binary, &failed_backup)?;
        }

        // 4. 恢复备份
        std::fs::copy(&latest_backup, &current_binary)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&current_binary)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&current_binary, perms)?;
        }

        info!(backup = %latest_backup.display(), "Rolled back successfully");
        Ok(())
    }

    /// 验证二进制文件
    fn verify_binary(&self, path: &Path) -> Result<()> {
        // 1. 检查文件存在
        if !path.exists() {
            return Err(Error::NotFound("Binary not found".to_string()));
        }

        // 2. 检查文件大小（至少应该有几 MB）
        let metadata = std::fs::metadata(path)?;
        if metadata.len() < 1024 * 1024 {
            return Err(Error::Validation("Binary too small".to_string()));
        }

        // 3. 检查文件头（ELF/Mach-O/PE）
        let mut file = std::fs::File::open(path)?;
        use std::io::Read;
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)?;

        #[cfg(target_os = "linux")]
        if &magic != b"\x7fELF" {
            return Err(Error::Validation("Not a valid ELF binary".to_string()));
        }

        #[cfg(target_os = "macos")]
        if &magic[0..2] != b"\xcf\xfa" && &magic[0..2] != b"\xce\xfa" {
            return Err(Error::Validation("Not a valid Mach-O binary".to_string()));
        }

        #[cfg(target_os = "windows")]
        if &magic[0..2] != b"MZ" {
            return Err(Error::Validation("Not a valid PE binary".to_string()));
        }

        debug!("Binary verification passed");
        Ok(())
    }

    fn get_current_binary_path(&self) -> Result<PathBuf> {
        // 获取当前运行的二进制路径
        let exe = std::env::current_exe()?;
        Ok(exe)
    }

    fn get_current_version(&self) -> Result<String> {
        Ok(env!("CARGO_PKG_VERSION").to_string())
    }

    fn find_latest_backup(&self) -> Result<PathBuf> {
        let mut backups: Vec<_> = std::fs::read_dir(&self.backup_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("blockcell-")
                    && !e.file_name().to_string_lossy().contains("failed")
            })
            .collect();

        if backups.is_empty() {
            return Err(Error::NotFound("No backup found".to_string()));
        }

        // 按修改时间排序
        backups.sort_by_key(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        });

        Ok(backups.last().unwrap().path())
    }

    fn cleanup_old_backups(&self, keep_count: usize) -> Result<()> {
        let mut backups: Vec<_> = std::fs::read_dir(&self.backup_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("blockcell-")
                    && !e.file_name().to_string_lossy().contains("failed")
            })
            .collect();

        if backups.len() <= keep_count {
            return Ok(());
        }

        // 按修改时间排序
        backups.sort_by_key(|e| {
            e.metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        });

        // 删除旧的备份
        let to_remove = backups.len() - keep_count;
        for backup in backups.iter().take(to_remove) {
            if let Err(e) = std::fs::remove_file(backup.path()) {
                warn!(path = %backup.path().display(), error = %e, "Failed to remove old backup");
            } else {
                debug!(path = %backup.path().display(), "Removed old backup");
            }
        }

        Ok(())
    }
}

/// 维护窗口检查器
pub struct MaintenanceWindow {
    window: String, // 格式: "HH:MM-HH:MM"
}

impl MaintenanceWindow {
    pub fn new(window: String) -> Self {
        Self { window }
    }

    /// 检查当前时间是否在维护窗口内
    pub fn is_in_window(&self) -> bool {
        if self.window.is_empty() {
            return true; // 没有配置维护窗口，任何时间都可以
        }

        let parts: Vec<&str> = self.window.split('-').collect();
        if parts.len() != 2 {
            warn!(window = %self.window, "Invalid maintenance window format");
            return false;
        }

        let start = match self.parse_time(parts[0]) {
            Some(t) => t,
            None => return false,
        };

        let end = match self.parse_time(parts[1]) {
            Some(t) => t,
            None => return false,
        };

        let now = chrono::Local::now();
        let current = (now.hour(), now.minute());

        // 处理跨午夜的情况
        if start <= end {
            current >= start && current < end
        } else {
            current >= start || current < end
        }
    }

    fn parse_time(&self, time_str: &str) -> Option<(u32, u32)> {
        let parts: Vec<&str> = time_str.split(':').collect();
        if parts.len() != 2 {
            return None;
        }

        let hour = parts[0].trim().parse::<u32>().ok()?;
        let minute = parts[1].trim().parse::<u32>().ok()?;

        if hour >= 24 || minute >= 60 {
            return None;
        }

        Some((hour, minute))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_maintenance_window() {
        let window = MaintenanceWindow::new("03:00-05:00".to_string());
        // 这个测试依赖于当前时间，所以只是验证不会 panic
        let _ = window.is_in_window();
    }

    #[test]
    fn test_parse_time() {
        let window = MaintenanceWindow::new("03:00-05:00".to_string());
        assert_eq!(window.parse_time("03:00"), Some((3, 0)));
        assert_eq!(window.parse_time("23:59"), Some((23, 59)));
        assert_eq!(window.parse_time("24:00"), None);
        assert_eq!(window.parse_time("invalid"), None);
    }
}
