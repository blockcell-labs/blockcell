use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::{expand_path, Tool, ToolContext, ToolSchema};

const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_ARCHIVE_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ARCHIVE_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ARCHIVE_COMPRESSION_RATIO: u64 = 1_000;

fn check_archive_limits(entries: usize, entry_bytes: u64, total_bytes: u64) -> Result<()> {
    if entries > MAX_ARCHIVE_ENTRIES {
        return Err(Error::Tool("Archive contains too many entries".to_string()));
    }
    if entry_bytes > MAX_ARCHIVE_ENTRY_BYTES {
        return Err(Error::Tool("Archive entry exceeds size limit".to_string()));
    }
    if total_bytes > MAX_ARCHIVE_TOTAL_BYTES {
        return Err(Error::Tool(
            "Archive exceeds total uncompressed size limit".to_string(),
        ));
    }
    Ok(())
}

fn check_compression_ratio(uncompressed: u64, compressed: u64) -> Result<()> {
    if compressed > 0 && uncompressed / compressed > MAX_ARCHIVE_COMPRESSION_RATIO {
        return Err(Error::Tool(
            "Archive compression ratio exceeds safety limit".to_string(),
        ));
    }
    Ok(())
}

fn target_is_within_source(source: &Path, target: &Path) -> bool {
    let Ok(source) = std::fs::canonicalize(source) else {
        return false;
    };
    let mut existing = target;
    while !existing.exists() {
        let Some(parent) = existing.parent() else {
            return false;
        };
        existing = parent;
    }
    std::fs::canonicalize(existing)
        .map(|path| path == source || path.starts_with(&source))
        .unwrap_or(false)
}

pub struct FileOpsTool;

#[async_trait]
impl Tool for FileOpsTool {
    fn coding_exposure(&self) -> crate::base_packs::CodingExposure {
        crate::base_packs::CodingExposure::deferred(true)
    }

    fn execution_environment(&self, _params: &Value) -> crate::ExecutionEnvironment {
        crate::ExecutionEnvironment::WorkspaceWrite
    }

    fn idempotency(&self, _params: &Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Required
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "file_ops".to_string(),
            description: "Multi-action file utility. You MUST provide `action`. action='delete': requires `path`, optional `recursive` for directories. action='rename'|'move'|'copy': requires `path` and `destination`. action='compress': requires `destination` and either `path` or `paths`, optional `format`. action='decompress': requires `path`, optional `destination`. action='read_pdf': requires `path`. action='file_info': requires `path`.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["delete", "rename", "move", "copy", "compress", "decompress", "read_pdf", "file_info"],
                        "description": "Action to perform"
                    },
                    "path": {
                        "type": "string",
                        "description": "Source path (file or directory)"
                    },
                    "destination": {
                        "type": "string",
                        "description": "(rename/move/copy/decompress) Destination path"
                    },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "(compress) List of files/directories to include in the archive"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["zip", "tar_gz"],
                        "description": "(compress) Archive format, default: zip"
                    },
                    "recursive": {
                        "type": "boolean",
                        "description": "(delete) If true, delete directories recursively. Default false."
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: action".to_string()))?;

        match action {
            "delete" | "read_pdf" | "file_info" => {
                if params.get("path").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation(
                        "Missing required parameter: path".to_string(),
                    ));
                }
            }
            "rename" | "move" | "copy" => {
                if params.get("path").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation(
                        "Missing required parameter: path".to_string(),
                    ));
                }
                if params.get("destination").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation(
                        "Missing required parameter: destination".to_string(),
                    ));
                }
            }
            "compress" => {
                let has_path = params.get("path").and_then(|v| v.as_str()).is_some();
                let has_paths = params
                    .get("paths")
                    .and_then(|v| v.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false);
                if !has_path && !has_paths {
                    return Err(Error::Validation(
                        "compress requires 'path' or 'paths'".to_string(),
                    ));
                }
                if params.get("destination").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation(
                        "Missing required parameter: destination (archive output path)".to_string(),
                    ));
                }
            }
            "decompress" => {
                if params.get("path").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation(
                        "Missing required parameter: path (archive to decompress)".to_string(),
                    ));
                }
            }
            _ => {
                return Err(Error::Validation(format!("Unknown action: {}", action)));
            }
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap();
        let workspace = ctx.workspace.clone();

        match action {
            "delete" => action_delete(&workspace, &params).await,
            "rename" | "move" => action_move(&workspace, &params).await,
            "copy" => action_copy(&workspace, &params).await,
            "compress" => {
                let ws = workspace.clone();
                let p = params.clone();
                tokio::task::spawn_blocking(move || action_compress(&ws, &p))
                    .await
                    .map_err(|e| Error::Tool(format!("Compress task failed: {}", e)))?
            }
            "decompress" => {
                let ws = workspace.clone();
                let p = params.clone();
                tokio::task::spawn_blocking(move || action_decompress(&ws, &p))
                    .await
                    .map_err(|e| Error::Tool(format!("Decompress task failed: {}", e)))?
            }
            "read_pdf" => {
                let ws = workspace.clone();
                let p = params.clone();
                tokio::task::spawn_blocking(move || action_read_pdf(&ws, &p))
                    .await
                    .map_err(|e| Error::Tool(format!("PDF read task failed: {}", e)))?
            }
            "file_info" => action_file_info(&workspace, &params).await,
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

async fn action_delete(workspace: &Path, params: &Value) -> Result<Value> {
    let path = expand_path(params["path"].as_str().unwrap(), workspace);
    let recursive = params
        .get("recursive")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if !path.exists() {
        return Err(Error::NotFound(format!(
            "Path not found: {}",
            path.display()
        )));
    }

    if path.is_dir() {
        if !recursive {
            return Err(Error::Tool(format!(
                "Cannot delete directory without recursive=true: {}",
                path.display()
            )));
        }
        tokio::fs::remove_dir_all(&path).await?;
    } else {
        tokio::fs::remove_file(&path).await?;
    }

    Ok(json!({
        "status": "deleted",
        "path": path.display().to_string()
    }))
}

async fn action_move(workspace: &Path, params: &Value) -> Result<Value> {
    let src = expand_path(params["path"].as_str().unwrap(), workspace);
    let dst = expand_path(params["destination"].as_str().unwrap(), workspace);

    if !src.exists() {
        return Err(Error::NotFound(format!(
            "Source not found: {}",
            src.display()
        )));
    }
    if std::fs::symlink_metadata(&src)?.file_type().is_symlink() {
        return Err(Error::PermissionDenied(format!(
            "Refusing to move symbolic link: {}",
            src.display()
        )));
    }

    // Create parent directories for destination
    if let Some(parent) = dst.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Try rename first (fast, same-device)
    let rename_ok = tokio::fs::rename(&src, &dst).await.is_ok();

    if !rename_ok {
        // Fallback: copy then delete (cross-device move)
        if src.is_dir() {
            let (src_c, dst_c) = (src.clone(), dst.clone());
            tokio::task::spawn_blocking(move || copy_dir_recursive(&src_c, &dst_c))
                .await
                .map_err(|e| Error::Tool(format!("Move task failed: {}", e)))??;
            tokio::fs::remove_dir_all(&src).await?;
        } else {
            tokio::fs::copy(&src, &dst).await?;
            tokio::fs::remove_file(&src).await?;
        }
    }

    Ok(json!({
        "status": "moved",
        "from": src.display().to_string(),
        "to": dst.display().to_string()
    }))
}

async fn action_copy(workspace: &Path, params: &Value) -> Result<Value> {
    let src = expand_path(params["path"].as_str().unwrap(), workspace);
    let dst = expand_path(params["destination"].as_str().unwrap(), workspace);

    if !src.exists() {
        return Err(Error::NotFound(format!(
            "Source not found: {}",
            src.display()
        )));
    }
    if std::fs::symlink_metadata(&src)?.file_type().is_symlink() {
        return Err(Error::PermissionDenied(format!(
            "Refusing to copy symbolic link: {}",
            src.display()
        )));
    }
    if target_is_within_source(&src, &dst) {
        return Err(Error::Validation(
            "Copy destination cannot be inside the source".to_string(),
        ));
    }

    if let Some(parent) = dst.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    if src.is_dir() {
        let (src_c, dst_c) = (src.clone(), dst.clone());
        // Recursive copy + count can walk large trees; keep it off the async worker.
        let count = tokio::task::spawn_blocking(move || {
            copy_dir_recursive(&src_c, &dst_c)?;
            count_files_recursive(&dst_c)
        })
        .await
        .map_err(|e| Error::Tool(format!("Copy task failed: {}", e)))??;
        Ok(json!({
            "status": "copied",
            "from": src.display().to_string(),
            "to": dst.display().to_string(),
            "type": "directory",
            "files_copied": count
        }))
    } else {
        let bytes = tokio::fs::copy(&src, &dst).await?;
        Ok(json!({
            "status": "copied",
            "from": src.display().to_string(),
            "to": dst.display().to_string(),
            "type": "file",
            "bytes_copied": bytes
        }))
    }
}

fn copy_dir_recursive(src: &PathBuf, dst: &PathBuf) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if std::fs::symlink_metadata(&src_path)?
            .file_type()
            .is_symlink()
        {
            return Err(Error::PermissionDenied(format!(
                "Refusing to copy symbolic link: {}",
                src_path.display()
            )));
        }
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn count_files_recursive(path: &PathBuf) -> Result<usize> {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries {
            let entry = entry?;
            let p = entry.path();
            if std::fs::symlink_metadata(&p)?.file_type().is_symlink() {
                return Err(Error::PermissionDenied(format!(
                    "Refusing to traverse symbolic link: {}",
                    p.display()
                )));
            }
            if p.is_dir() {
                count += count_files_recursive(&p)?;
            } else {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn action_compress(workspace: &Path, params: &Value) -> Result<Value> {
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("zip");
    let dst_str = params["destination"].as_str().unwrap();
    let dst = expand_path(dst_str, workspace);

    // Collect source paths
    let mut sources: Vec<PathBuf> = Vec::new();
    if let Some(path_str) = params.get("path").and_then(|v| v.as_str()) {
        sources.push(expand_path(path_str, workspace));
    }
    if let Some(paths_arr) = params.get("paths").and_then(|v| v.as_array()) {
        for p in paths_arr {
            if let Some(s) = p.as_str() {
                sources.push(expand_path(s, workspace));
            }
        }
    }

    // Verify all sources exist
    for src in &sources {
        if !src.exists() {
            return Err(Error::NotFound(format!(
                "Source not found: {}",
                src.display()
            )));
        }
        if std::fs::symlink_metadata(src)?.file_type().is_symlink() {
            return Err(Error::PermissionDenied(format!(
                "Refusing to archive symbolic link: {}",
                src.display()
            )));
        }
        if target_is_within_source(src, &dst) {
            return Err(Error::Validation(
                "Archive destination cannot be inside a source directory".to_string(),
            ));
        }
    }

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut file_count = 0u64;
    match format {
        "zip" => {
            let file = std::fs::File::create(&dst)
                .map_err(|e| Error::Tool(format!("Failed to create archive: {}", e)))?;
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);

            for src in &sources {
                if src.is_dir() {
                    file_count += zip_add_dir(&mut zip, src, src, options)?;
                } else {
                    let name = src
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    zip.start_file(&name, options)
                        .map_err(|e| Error::Tool(format!("Zip error: {}", e)))?;
                    let mut input = std::fs::File::open(src)?;
                    std::io::copy(&mut input, &mut zip)?;
                    file_count += 1;
                }
            }
            zip.finish()
                .map_err(|e| Error::Tool(format!("Zip finish error: {}", e)))?;
        }
        "tar_gz" => {
            let file = std::fs::File::create(&dst)
                .map_err(|e| Error::Tool(format!("Failed to create archive: {}", e)))?;
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);

            for src in &sources {
                if src.is_dir() {
                    let source_file_count = count_files_recursive(src)? as u64;
                    let dir_name = src
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    tar.append_dir_all(&dir_name, src)
                        .map_err(|e| Error::Tool(format!("Tar error: {}", e)))?;
                    file_count += source_file_count;
                } else {
                    let name = src
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    tar.append_path_with_name(src, &name)
                        .map_err(|e| Error::Tool(format!("Tar error: {}", e)))?;
                    file_count += 1;
                }
            }
            tar.finish()
                .map_err(|e| Error::Tool(format!("Tar finish error: {}", e)))?;
        }
        _ => return Err(Error::Validation(format!("Unknown format: {}", format))),
    }

    let size = std::fs::metadata(&dst).map(|m| m.len()).unwrap_or(0);

    Ok(json!({
        "status": "compressed",
        "archive": dst.display().to_string(),
        "format": format,
        "files_added": file_count,
        "archive_size": size
    }))
}

fn zip_add_dir(
    zip: &mut zip::ZipWriter<std::fs::File>,
    base: &PathBuf,
    current: &PathBuf,
    options: zip::write::SimpleFileOptions,
) -> Result<u64> {
    let mut count = 0u64;
    let base_name = base
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(Error::PermissionDenied(format!(
                "Refusing to archive symbolic link: {}",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(base)
            .map_err(|e| Error::Tool(format!("Path strip error: {}", e)))?;
        let archive_name = format!("{}/{}", base_name, relative.display());

        if path.is_dir() {
            zip.add_directory(format!("{}/", archive_name), options)
                .map_err(|e| Error::Tool(format!("Zip dir error: {}", e)))?;
            count += zip_add_dir(zip, base, &path, options)?;
        } else {
            zip.start_file(&archive_name, options)
                .map_err(|e| Error::Tool(format!("Zip file error: {}", e)))?;
            let mut input = std::fs::File::open(&path)?;
            std::io::copy(&mut input, zip)?;
            count += 1;
        }
    }
    Ok(count)
}

// Resolve only macOS's root-owned system aliases. All remaining components are
// opened without following links, including the destination directory itself.
fn zip_destination_path(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    #[cfg(target_os = "macos")]
    for alias in [Path::new("/tmp"), Path::new("/var"), Path::new("/etc")] {
        if let Ok(relative) = absolute.strip_prefix(alias) {
            return Ok(std::fs::canonicalize(alias)?.join(relative));
        }
    }
    Ok(absolute)
}

fn zip_relative_components(path: &Path) -> Result<Vec<&std::ffi::OsStr>> {
    path.components()
        .filter(|component| *component != std::path::Component::CurDir)
        .map(|component| match component {
            std::path::Component::Normal(name) => Ok(name),
            _ => Err(Error::PermissionDenied(
                "ZIP destination path contains traversal or an absolute component".to_string(),
            )),
        })
        .collect()
}

#[cfg(unix)]
mod zip_destination {
    use super::*;
    use std::ffi::{CString, OsStr};
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    pub(super) struct ZipDestination {
        directory: File,
    }

    fn c_name(name: &OsStr) -> std::io::Result<CString> {
        CString::new(name.as_bytes())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "NUL in ZIP path"))
    }

    fn open_directory(parent: &File, name: &OsStr) -> std::io::Result<File> {
        let name = c_name(name)?;
        let open = || unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        let mut fd = open();
        if fd < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
            let created = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o777) };
            if created < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
            // A concurrent creator may have installed a link; openat still
            // refuses to follow it and binds the returned handle to one inode.
            fd = open();
        }
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned a new descriptor owned by this File.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    impl ZipDestination {
        pub(super) fn open(path: &Path) -> Result<Self> {
            let absolute = zip_destination_path(path)?;
            let mut directory = File::open("/")?;
            for component in absolute.components() {
                match component {
                    std::path::Component::RootDir | std::path::Component::CurDir => {}
                    std::path::Component::Normal(name) => {
                        directory = open_directory(&directory, name)?;
                    }
                    _ => {
                        return Err(Error::PermissionDenied(
                            "ZIP destination cannot contain parent traversal".to_string(),
                        ));
                    }
                }
            }
            Ok(Self { directory })
        }

        fn directory(&self, path: &Path) -> Result<File> {
            let mut directory = self.directory.try_clone()?;
            for name in zip_relative_components(path)? {
                directory = open_directory(&directory, name)?;
            }
            Ok(directory)
        }

        pub(super) fn create_directory(&self, path: &Path) -> Result<()> {
            self.directory(path)?;
            Ok(())
        }

        pub(super) fn write_file(
            &self,
            path: &Path,
            reader: &mut impl std::io::Read,
        ) -> Result<()> {
            let directory = self.directory(path.parent().unwrap_or(Path::new("")))?;
            let name =
                c_name(path.file_name().ok_or_else(|| {
                    Error::Validation("ZIP file entry has no filename".to_string())
                })?)?;
            let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
            let status = unsafe {
                libc::fstatat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    metadata.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            let existing_mode = if status == 0 {
                // SAFETY: fstatat initialized metadata on success.
                let metadata = unsafe { metadata.assume_init() };
                if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
                    return Err(Error::PermissionDenied(
                        "ZIP destination file is a link or special file".to_string(),
                    ));
                }
                Some(metadata.st_mode & 0o777)
            } else {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(error.into());
                }
                None
            };

            let temporary = CString::new(format!(".blockcell-zip-{}.tmp", uuid::Uuid::new_v4()))
                .expect("UUID contains no NUL");
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    temporary.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o666 as libc::c_uint,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            // SAFETY: the exclusively created descriptor is owned by output.
            let mut output = unsafe { File::from_raw_fd(fd) };
            let result = (|| -> Result<()> {
                if let Some(mode) = existing_mode {
                    if unsafe { libc::fchmod(output.as_raw_fd(), mode) } < 0 {
                        return Err(std::io::Error::last_os_error().into());
                    }
                }
                std::io::copy(reader, &mut output)?;
                // Rename the entry itself rather than opening/truncating it.
                // A leaf swapped to a link during the copy is replaced safely;
                // an existing hard link also never modifies its other names.
                if unsafe {
                    libc::renameat(
                        directory.as_raw_fd(),
                        temporary.as_ptr(),
                        directory.as_raw_fd(),
                        name.as_ptr(),
                    )
                } < 0
                {
                    return Err(std::io::Error::last_os_error().into());
                }
                Ok(())
            })();
            if result.is_err() {
                unsafe { libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0) };
            }
            result
        }
    }
}

#[cfg(windows)]
mod zip_destination {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    // Ancestor handles omit FILE_SHARE_DELETE and FILE_SHARE_WRITE so Windows
    // cannot rename a checked directory or turn it into a reparse point while
    // an extraction uses its path. Existing incompatible writer handles cause
    // the pin to fail rather than leave a race after the metadata check.
    pub(super) struct ZipDestination {
        path: PathBuf,
        _directories: Vec<File>,
    }

    fn pin_directory(path: &Path) -> Result<File> {
        let directory = OpenOptions::new()
            .read(true)
            .share_mode(0x1) // FILE_SHARE_READ
            .custom_flags(0x0020_0000 | 0x0200_0000) // OPEN_REPARSE_POINT | BACKUP_SEMANTICS
            .open(path)?;
        let metadata = directory.metadata()?;
        if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
            return Err(Error::PermissionDenied(
                "ZIP destination contains a link, junction or non-directory".to_string(),
            ));
        }
        Ok(directory)
    }

    fn create_and_pin(path: &Path) -> Result<File> {
        match std::fs::create_dir(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        pin_directory(path)
    }

    impl ZipDestination {
        pub(super) fn open(path: &Path) -> Result<Self> {
            let absolute = zip_destination_path(path)?;
            let mut path = PathBuf::new();
            let mut directories = Vec::new();
            for component in absolute.components() {
                match component {
                    std::path::Component::Prefix(prefix) => path.push(prefix.as_os_str()),
                    std::path::Component::RootDir => {
                        path.push(component.as_os_str());
                        directories.push(pin_directory(&path)?);
                    }
                    std::path::Component::Normal(name) => {
                        path.push(name);
                        directories.push(create_and_pin(&path)?);
                    }
                    std::path::Component::CurDir => {}
                    _ => {
                        return Err(Error::PermissionDenied(
                            "ZIP destination cannot contain parent traversal".to_string(),
                        ));
                    }
                }
            }
            Ok(Self {
                path,
                _directories: directories,
            })
        }

        fn directory(&self, path: &Path) -> Result<Self> {
            let mut directory = Self {
                path: self.path.clone(),
                _directories: Vec::new(),
            };
            for name in zip_relative_components(path)? {
                directory.path.push(name);
                directory
                    ._directories
                    .push(create_and_pin(&directory.path)?);
            }
            Ok(directory)
        }

        pub(super) fn create_directory(&self, path: &Path) -> Result<()> {
            self.directory(path)?;
            Ok(())
        }

        pub(super) fn write_file(
            &self,
            path: &Path,
            reader: &mut impl std::io::Read,
        ) -> Result<()> {
            let directory = self.directory(path.parent().unwrap_or(Path::new("")))?;
            let name = path
                .file_name()
                .ok_or_else(|| Error::Validation("ZIP file entry has no filename".to_string()))?;
            let target = directory.path.join(name);
            match std::fs::symlink_metadata(&target) {
                Ok(metadata) if !metadata.is_file() || metadata.file_attributes() & 0x400 != 0 => {
                    return Err(Error::PermissionDenied(
                        "ZIP destination file is a link or special file".to_string(),
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let temporary = directory
                .path
                .join(format!(".blockcell-zip-{}.tmp", uuid::Uuid::new_v4()));
            let result = (|| -> Result<()> {
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)?;
                std::io::copy(reader, &mut output)?;
                drop(output);
                std::fs::rename(&temporary, &target)?;
                Ok(())
            })();
            if result.is_err() {
                let _ = std::fs::remove_file(&temporary);
            }
            result
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod zip_destination {
    use super::*;

    pub(super) struct ZipDestination;

    impl ZipDestination {
        pub(super) fn open(_path: &Path) -> Result<Self> {
            Err(Error::PermissionDenied(
                "Secure ZIP extraction is unsupported on this platform".to_string(),
            ))
        }
        pub(super) fn create_directory(&self, _path: &Path) -> Result<()> {
            unreachable!()
        }
        pub(super) fn write_file(
            &self,
            _path: &Path,
            _reader: &mut impl std::io::Read,
        ) -> Result<()> {
            unreachable!()
        }
    }
}

fn action_decompress(workspace: &Path, params: &Value) -> Result<Value> {
    let src_str = params["path"].as_str().unwrap();
    let src = expand_path(src_str, workspace);

    if !src.exists() {
        return Err(Error::NotFound(format!(
            "Archive not found: {}",
            src.display()
        )));
    }

    let dst = if let Some(d) = params.get("destination").and_then(|v| v.as_str()) {
        expand_path(d, workspace)
    } else {
        // Default: extract next to the archive
        src.parent().unwrap_or(workspace).to_path_buf()
    };

    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let name = src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();

    let mut file_count = 0u64;

    if ext == "zip" {
        let file = std::fs::File::open(&src)
            .map_err(|e| Error::Tool(format!("Failed to open archive: {}", e)))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| Error::Tool(format!("Failed to read zip: {}", e)))?;
        let destination = zip_destination::ZipDestination::open(&dst)?;

        let mut total_uncompressed = 0u64;
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| Error::Tool(format!("Zip entry error: {}", e)))?;
            total_uncompressed = total_uncompressed.saturating_add(entry.size());
            check_archive_limits(i + 1, entry.size(), total_uncompressed)?;
            check_compression_ratio(entry.size(), entry.compressed_size())?;
            if entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err(Error::PermissionDenied(
                    "Archive contains a symbolic link".to_string(),
                ));
            }
            if entry.is_dir() {
                destination.create_directory(&entry.mangled_name())?;
            } else {
                destination.write_file(&entry.mangled_name(), &mut entry)?;
                file_count += 1;
            }
        }
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") || ext == "gz" {
        std::fs::create_dir_all(&dst)?;
        let file = std::fs::File::open(&src)
            .map_err(|e| Error::Tool(format!("Failed to open archive: {}", e)))?;
        let dec = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(dec);

        let compressed_size = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
        let mut total_uncompressed = 0u64;
        for (index, entry) in archive
            .entries()
            .map_err(|e| Error::Tool(format!("Tar error: {}", e)))?
            .enumerate()
        {
            let mut entry = entry.map_err(|e| Error::Tool(format!("Tar entry error: {}", e)))?;
            let size = entry.size();
            total_uncompressed = total_uncompressed.saturating_add(size);
            check_archive_limits(index + 1, size, total_uncompressed)?;
            check_compression_ratio(total_uncompressed, compressed_size)?;
            if !entry.header().entry_type().is_file() && !entry.header().entry_type().is_dir() {
                return Err(Error::PermissionDenied(
                    "Archive contains a link or special file".to_string(),
                ));
            }
            entry
                .unpack_in(&dst)
                .map_err(|e| Error::Tool(format!("Tar unpack error: {}", e)))?;
            file_count += 1;
        }
    } else if ext == "tar" {
        std::fs::create_dir_all(&dst)?;
        let file = std::fs::File::open(&src)
            .map_err(|e| Error::Tool(format!("Failed to open archive: {}", e)))?;
        let mut archive = tar::Archive::new(file);

        let mut total_uncompressed = 0u64;
        for (index, entry) in archive
            .entries()
            .map_err(|e| Error::Tool(format!("Tar error: {}", e)))?
            .enumerate()
        {
            let mut entry = entry.map_err(|e| Error::Tool(format!("Tar entry error: {}", e)))?;
            let size = entry.size();
            total_uncompressed = total_uncompressed.saturating_add(size);
            check_archive_limits(index + 1, size, total_uncompressed)?;
            if !entry.header().entry_type().is_file() && !entry.header().entry_type().is_dir() {
                return Err(Error::PermissionDenied(
                    "Archive contains a link or special file".to_string(),
                ));
            }
            entry
                .unpack_in(&dst)
                .map_err(|e| Error::Tool(format!("Tar unpack error: {}", e)))?;
            file_count += 1;
        }
    } else {
        return Err(Error::Tool(format!(
            "Unsupported archive format: {}. Supported: .zip, .tar.gz, .tgz, .tar",
            src.display()
        )));
    }

    Ok(json!({
        "status": "decompressed",
        "archive": src.display().to_string(),
        "destination": dst.display().to_string(),
        "files_extracted": file_count
    }))
}

fn action_read_pdf(workspace: &Path, params: &Value) -> Result<Value> {
    let path = expand_path(params["path"].as_str().unwrap(), workspace);

    if !path.exists() {
        return Err(Error::NotFound(format!(
            "File not found: {}",
            path.display()
        )));
    }

    // Use pdf-extract crate to extract text
    let text = pdf_extract::extract_text(&path)
        .map_err(|e| Error::Tool(format!("Failed to extract PDF text: {}", e)))?;

    let page_count = text.matches('\u{0C}').count() + 1; // form feed = page break

    // Truncate if very long
    let max_chars = 100000;
    let truncated = text.len() > max_chars;
    let text = if truncated {
        let mut end = max_chars;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].to_string()
    } else {
        text
    };

    Ok(json!({
        "path": path.display().to_string(),
        "pages": page_count,
        "length": text.len(),
        "truncated": truncated,
        "content": text
    }))
}

async fn action_file_info(workspace: &Path, params: &Value) -> Result<Value> {
    let path = expand_path(params["path"].as_str().unwrap(), workspace);

    if !path.exists() {
        return Err(Error::NotFound(format!(
            "Path not found: {}",
            path.display()
        )));
    }

    let metadata = tokio::fs::metadata(&path).await?;
    let file_type = if metadata.is_dir() {
        "directory"
    } else if metadata.is_file() {
        "file"
    } else {
        "symlink"
    };

    let mut info = json!({
        "path": path.display().to_string(),
        "type": file_type,
        "size": metadata.len(),
        "readonly": metadata.permissions().readonly(),
    });

    if let Ok(modified) = metadata.modified() {
        if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
            info["modified_timestamp"] = json!(duration.as_secs());
        }
    }

    if metadata.is_dir() {
        let path_c = path.clone();
        let count = tokio::task::spawn_blocking(move || count_files_recursive(&path_c))
            .await
            .map_err(|e| Error::Tool(format!("File count task failed: {}", e)))??;
        info["total_files"] = json!(count);
    }

    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        info["extension"] = json!(ext);
    }

    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_test_zip(path: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Write;

        let mut archive = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, contents) in entries {
            archive.start_file(*name, options).unwrap();
            archive.write_all(contents).unwrap();
        }
        archive.finish().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn zip_decompress_rejects_existing_parent_symlink() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        let destination = root.path().join("output");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(outside.join("sentinel.txt"), b"original").unwrap();
        std::os::unix::fs::symlink(&outside, destination.join("linked")).unwrap();
        let archive = root.path().join("input.zip");
        write_test_zip(&archive, &[("linked/sentinel.txt", b"archive")]);

        let result = action_decompress(
            root.path(),
            &json!({"path": archive, "destination": destination}),
        );

        assert_eq!(
            std::fs::read(outside.join("sentinel.txt")).unwrap(),
            b"original"
        );
        assert!(result.is_err(), "destination parent links must be rejected");
    }

    #[cfg(unix)]
    #[test]
    fn zip_decompress_rejects_existing_leaf_symlink() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("sentinel.txt");
        let destination = root.path().join("output");
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(&outside, b"original").unwrap();
        std::os::unix::fs::symlink(&outside, destination.join("sentinel.txt")).unwrap();
        let archive = root.path().join("input.zip");
        write_test_zip(&archive, &[("sentinel.txt", b"archive")]);

        let result = action_decompress(
            root.path(),
            &json!({"path": archive, "destination": destination}),
        );

        assert_eq!(std::fs::read(&outside).unwrap(), b"original");
        assert!(result.is_err(), "destination leaf links must be rejected");
    }

    #[cfg(unix)]
    #[test]
    fn zip_decompress_rejects_symlink_destination_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("sentinel.txt"), b"original").unwrap();
        let destination = root.path().join("output");
        std::os::unix::fs::symlink(&outside, &destination).unwrap();
        let archive = root.path().join("input.zip");
        write_test_zip(&archive, &[("sentinel.txt", b"archive")]);

        let result = action_decompress(
            root.path(),
            &json!({"path": archive, "destination": destination}),
        );

        assert_eq!(
            std::fs::read(outside.join("sentinel.txt")).unwrap(),
            b"original"
        );
        assert!(result.is_err(), "destination root links must be rejected");
    }

    #[test]
    fn zip_decompress_keeps_existing_files_and_extracts_nested_entries() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("output");
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::write(destination.join("untouched.txt"), b"keep").unwrap();
        std::fs::write(destination.join("replace.txt"), b"old").unwrap();
        let archive = root.path().join("input.zip");
        write_test_zip(
            &archive,
            &[("replace.txt", b"new"), ("nested/data.txt", b"nested")],
        );

        let result = action_decompress(
            root.path(),
            &json!({"path": archive, "destination": destination}),
        )
        .unwrap();

        assert_eq!(result["files_extracted"], 2);
        assert_eq!(
            std::fs::read(destination.join("untouched.txt")).unwrap(),
            b"keep"
        );
        assert_eq!(
            std::fs::read(destination.join("replace.txt")).unwrap(),
            b"new"
        );
        assert_eq!(
            std::fs::read(destination.join("nested/data.txt")).unwrap(),
            b"nested"
        );
    }

    #[cfg(unix)]
    struct ReplaceOnRead<F: FnOnce()> {
        replace: Option<F>,
        contents: std::io::Cursor<Vec<u8>>,
    }

    #[cfg(unix)]
    impl<F: FnOnce()> std::io::Read for ReplaceOnRead<F> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if let Some(replace) = self.replace.take() {
                replace();
            }
            std::io::Read::read(&mut self.contents, buffer)
        }
    }

    #[cfg(unix)]
    #[test]
    fn zip_decompress_parent_replaced_during_copy_cannot_redirect_writes() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");
        let parent = output.join("parent");
        let retained = output.join("retained");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("sentinel.txt"), b"original").unwrap();
        let destination = zip_destination::ZipDestination::open(&output).unwrap();
        let mut reader = ReplaceOnRead {
            replace: Some(|| {
                std::fs::rename(&parent, &retained).unwrap();
                std::os::unix::fs::symlink(&outside, &parent).unwrap();
            }),
            contents: std::io::Cursor::new(b"archive".to_vec()),
        };

        destination
            .write_file(Path::new("parent/sentinel.txt"), &mut reader)
            .unwrap();

        assert_eq!(
            std::fs::read(outside.join("sentinel.txt")).unwrap(),
            b"original"
        );
        assert_eq!(
            std::fs::read(retained.join("sentinel.txt")).unwrap(),
            b"archive"
        );
        assert!(destination
            .write_file(Path::new("parent/another.txt"), &mut &b"bad"[..])
            .is_err());
        assert!(!outside.join("another.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn zip_decompress_leaf_replaced_during_copy_cannot_redirect_writes() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");
        let outside = root.path().join("sentinel.txt");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(&outside, b"original").unwrap();
        let leaf = output.join("sentinel.txt");
        std::fs::write(&leaf, b"old").unwrap();
        let destination = zip_destination::ZipDestination::open(&output).unwrap();
        let mut reader = ReplaceOnRead {
            replace: Some(|| {
                std::fs::remove_file(&leaf).unwrap();
                std::os::unix::fs::symlink(&outside, &leaf).unwrap();
            }),
            contents: std::io::Cursor::new(b"archive".to_vec()),
        };

        destination
            .write_file(Path::new("sentinel.txt"), &mut reader)
            .unwrap();

        assert_eq!(std::fs::read(&outside).unwrap(), b"original");
        assert_eq!(std::fs::read(&leaf).unwrap(), b"archive");
        assert!(!std::fs::symlink_metadata(&leaf)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn zip_decompress_root_replaced_during_copy_cannot_redirect_writes() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");
        let retained = root.path().join("retained");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("sentinel.txt"), b"original").unwrap();
        let destination = zip_destination::ZipDestination::open(&output).unwrap();
        let mut reader = ReplaceOnRead {
            replace: Some(|| {
                std::fs::rename(&output, &retained).unwrap();
                std::os::unix::fs::symlink(&outside, &output).unwrap();
            }),
            contents: std::io::Cursor::new(b"archive".to_vec()),
        };

        destination
            .write_file(Path::new("sentinel.txt"), &mut reader)
            .unwrap();

        assert_eq!(
            std::fs::read(outside.join("sentinel.txt")).unwrap(),
            b"original"
        );
        assert_eq!(
            std::fs::read(retained.join("sentinel.txt")).unwrap(),
            b"archive"
        );
    }

    #[cfg(unix)]
    #[test]
    fn zip_decompress_replaces_hardlink_without_changing_external_file() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("sentinel.txt");
        let output = root.path().join("output");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(&outside, b"original").unwrap();
        std::fs::hard_link(&outside, output.join("sentinel.txt")).unwrap();
        let archive = root.path().join("input.zip");
        write_test_zip(&archive, &[("sentinel.txt", b"archive")]);

        action_decompress(
            root.path(),
            &json!({"path": archive, "destination": output}),
        )
        .unwrap();

        assert_eq!(std::fs::read(&outside).unwrap(), b"original");
        assert_eq!(
            std::fs::read(output.join("sentinel.txt")).unwrap(),
            b"archive"
        );
    }

    #[test]
    fn zip_decompress_copy_error_keeps_old_file_and_removes_temporary() {
        struct FailingReader(bool);
        impl std::io::Read for FailingReader {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.0 {
                    return Err(std::io::Error::other("injected read failure"));
                }
                self.0 = true;
                buffer[0] = b'x';
                Ok(1)
            }
        }
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("output");
        std::fs::create_dir_all(&output).unwrap();
        std::fs::write(output.join("sentinel.txt"), b"original").unwrap();
        let destination = zip_destination::ZipDestination::open(&output).unwrap();

        assert!(destination
            .write_file(Path::new("sentinel.txt"), &mut FailingReader(false))
            .is_err());

        assert_eq!(
            std::fs::read(output.join("sentinel.txt")).unwrap(),
            b"original"
        );
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), 1);
    }

    #[test]
    fn zip_decompress_rejects_archive_symlink() {
        let root = tempfile::tempdir().unwrap();
        let archive = root.path().join("input.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        writer
            .add_symlink(
                "linked",
                "../outside",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.finish().unwrap();

        assert!(action_decompress(
            root.path(),
            &json!({"path": archive, "destination": root.path().join("output")})
        )
        .is_err());
        assert!(!root.path().join("output/linked").exists());
    }

    #[test]
    fn test_schema() {
        let tool = FileOpsTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "file_ops");
    }

    #[test]
    fn test_validate_delete() {
        let tool = FileOpsTool;
        assert!(tool
            .validate(&json!({"action": "delete", "path": "/tmp/test"}))
            .is_ok());
        assert!(tool.validate(&json!({"action": "delete"})).is_err());
    }

    #[test]
    fn test_validate_compress() {
        let tool = FileOpsTool;
        assert!(tool
            .validate(&json!({
                "action": "compress",
                "path": "/tmp/test",
                "destination": "/tmp/test.zip"
            }))
            .is_ok());
        assert!(tool
            .validate(&json!({
                "action": "compress",
                "destination": "/tmp/test.zip"
            }))
            .is_err());
    }

    #[test]
    fn test_validate_move() {
        let tool = FileOpsTool;
        assert!(tool
            .validate(&json!({
                "action": "move",
                "path": "/tmp/a",
                "destination": "/tmp/b"
            }))
            .is_ok());
        assert!(tool
            .validate(&json!({
                "action": "move",
                "path": "/tmp/a"
            }))
            .is_err());
    }

    #[test]
    fn test_archive_limits_reject_excessive_total_and_ratio() {
        assert!(check_archive_limits(1, MAX_ARCHIVE_ENTRY_BYTES + 1, 1).is_err());
        assert!(check_archive_limits(MAX_ARCHIVE_ENTRIES + 1, 1, 1).is_err());
        assert!(check_archive_limits(1, MAX_ARCHIVE_TOTAL_BYTES + 1, 1).is_err());
        assert!(check_compression_ratio(10_000_000, 1).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_recursive_copy_rejects_directory_symlink() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("blockcell-copy-{}", uuid::Uuid::new_v4()));
        let source = root.join("source");
        let outside = root.join("outside");
        let destination = root.join("destination");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        symlink(&outside, source.join("linked")).unwrap();

        assert!(copy_dir_recursive(&source, &destination).is_err());
        assert!(!destination.join("linked").join("secret.txt").exists());

        let _ = std::fs::remove_dir_all(&root);
    }
}
