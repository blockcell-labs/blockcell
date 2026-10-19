use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(windows)]
use tracing::warn;

/// 全局计数器，用于生成唯一的临时文件名
static ATOMIC_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 生成唯一的临时文件路径: `<original>.tmp.<pid>.<counter>`
fn unique_tmp_path(path: &Path) -> PathBuf {
    let pid = std::process::id();
    let counter = ATOMIC_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("tmp.{pid}.{counter}"))
}

/// 生成唯一的备份文件路径: `<original>.bak.<pid>.<counter>`
///
/// 使用追加方式而非 `with_extension`，确保对 `.dream_state.json` 等多扩展名文件
/// 生成 `.dream_state.json.bak.<pid>.<counter>` 而非 `.dream_state.bak.<pid>.<counter>`。
#[cfg(windows)]
fn unique_bak_path(path: &Path) -> PathBuf {
    let pid = std::process::id();
    let counter = ATOMIC_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    // 追加 .bak.<pid>.<counter> 到完整文件名，而非替换扩展名
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let bak_name = format!("{file_name}.bak.{pid}.{counter}");
    path.with_file_name(bak_name)
}

/// 生成备份文件名前缀，供 `find_latest_backup` 使用。
///
/// 与 `unique_bak_path` 共用同一命名逻辑，确保查找和生成使用相同的命名格式。
fn bak_prefix_for(path: &Path) -> String {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    format!("{file_name}.bak.")
}

/// 查找 `atomic_write` 产生的最新备份文件。
///
/// `atomic_write` 在 Windows 上使用 `<original>.bak.<pid>.<counter>` 格式备份，
/// 此函数扫描目录中所有匹配 `<original>.bak.*` 模式的文件，
/// 返回修改时间最新的那个（即最近一次 `atomic_write` 产生的备份）。
///
/// 用于崩溃恢复：当主文件不存在但存在备份时，找到最新的备份来恢复数据。
pub fn find_latest_backup(original_path: &Path) -> Option<PathBuf> {
    let dir = original_path.parent()?;

    // 使用与 unique_bak_path 相同的命名逻辑构造前缀
    // 例如 ".dream_state.json" -> ".dream_state.json.bak."
    let bak_prefix = bak_prefix_for(original_path);

    let mut latest: Option<(PathBuf, std::time::SystemTime)> = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name_str = name.to_str()?;
        if name_str.starts_with(&bak_prefix) {
            let mtime = entry.metadata().ok()?.modified().ok()?;
            if latest.as_ref().is_none_or(|(_, t)| mtime > *t) {
                latest = Some((entry.path(), mtime));
            }
        }
    }

    latest.map(|(path, _)| path)
}

/// 原子写入 `data` 到 `path`，使用临时文件 + rename 策略。
///
/// 每次调用使用唯一的 `.tmp.<pid>.<counter>` 和 `.bak.<pid>.<counter>`
/// 路径，因此并发写入（同进程或跨进程）不会共享临时/备份文件。
///
/// ## Windows 策略（backup-based）
/// 1. 写入唯一的 `.tmp` 文件
/// 2. 若目标文件已存在，将其重命名为唯一的 `.bak`
/// 3. 将 `.tmp` 重命名为目标文件
/// 4. 成功后删除 `.bak`；若步骤 3 失败，恢复 `.bak`
///
/// ## Unix 策略
/// 直接 `rename`（原子替换），无需备份。
pub fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp_path = unique_tmp_path(path);

    // 先写入临时文件
    std::fs::write(&tmp_path, data)?;

    #[cfg(windows)]
    {
        let bak_path = unique_bak_path(path);

        if path.exists() {
            // 步骤 2: 备份现有文件
            if let Err(e) = std::fs::rename(path, &bak_path) {
                // 无法备份 — 清理临时文件并返回错误
                let _ = std::fs::remove_file(&tmp_path);
                return Err(std::io::Error::other(format!(
                    "atomic_write: 备份现有文件失败: {e}"
                )));
            }
        }

        // 步骤 3: 将临时文件重命名为目标文件
        if let Err(e) = std::fs::rename(&tmp_path, path) {
            // 重命名失败，清理临时文件
            let _ = std::fs::remove_file(&tmp_path);
            // 尝试恢复备份
            if bak_path.exists() {
                if let Err(restore_err) = std::fs::rename(&bak_path, path) {
                    warn!("atomic_write: 重命名失败后恢复备份也失败: {restore_err}");
                }
            }
            return Err(e);
        }

        // 步骤 4: 清理备份文件
        if bak_path.exists() {
            let _ = std::fs::remove_file(&bak_path);
        }
    }

    #[cfg(not(windows))]
    {
        // Unix 上 rename 是原子替换；失败时清理临时文件，避免残留 .tmp
        if let Err(error) = std::fs::rename(&tmp_path, path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(error);
        }
    }

    Ok(())
}
