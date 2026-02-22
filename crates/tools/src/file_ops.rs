use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::{Tool, ToolContext, ToolSchema};

fn expand_path(path: &str, workspace: &std::path::Path) -> PathBuf {
    if path.starts_with("~/") {
        dirs::home_dir()
            .map(|h| h.join(&path[2..]))
            .unwrap_or_else(|| PathBuf::from(path))
    } else if path.starts_with('/') {
        PathBuf::from(path)
    } else {
        workspace.join(path)
    }
}

pub struct FileOpsTool;

#[async_trait]
impl Tool for FileOpsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "file_ops",
            description: "File operations: delete, rename, move, copy files/directories, and compress/decompress archives (zip/tar.gz). Also supports reading PDF files.",
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
                    return Err(Error::Validation("Missing required parameter: path".to_string()));
                }
            }
            "rename" | "move" | "copy" => {
                if params.get("path").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation("Missing required parameter: path".to_string()));
                }
                if params.get("destination").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation("Missing required parameter: destination".to_string()));
                }
            }
            "compress" => {
                let has_path = params.get("path").and_then(|v| v.as_str()).is_some();
                let has_paths = params.get("paths").and_then(|v| v.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
                if !has_path && !has_paths {
                    return Err(Error::Validation("compress requires 'path' or 'paths'".to_string()));
                }
                if params.get("destination").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation("Missing required parameter: destination (archive output path)".to_string()));
                }
            }
            "decompress" => {
                if params.get("path").and_then(|v| v.as_str()).is_none() {
                    return Err(Error::Validation("Missing required parameter: path (archive to decompress)".to_string()));
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
    let recursive = params.get("recursive").and_then(|v| v.as_bool()).unwrap_or(false);

    if !path.exists() {
        return Err(Error::NotFound(format!("Path not found: {}", path.display())));
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
        return Err(Error::NotFound(format!("Source not found: {}", src.display())));
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
            copy_dir_recursive(&src, &dst)?;
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
        return Err(Error::NotFound(format!("Source not found: {}", src.display())));
    }

    if let Some(parent) = dst.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    if src.is_dir() {
        copy_dir_recursive(&src, &dst)?;
        // Count files
        let count = count_files_recursive(&dst);
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
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn count_files_recursive(path: &PathBuf) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                count += count_files_recursive(&p);
            } else {
                count += 1;
            }
        }
    }
    count
}

fn action_compress(workspace: &Path, params: &Value) -> Result<Value> {
    let format = params.get("format").and_then(|v| v.as_str()).unwrap_or("zip");
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
            return Err(Error::NotFound(format!("Source not found: {}", src.display())));
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
                    let name = src.file_name().unwrap_or_default().to_string_lossy().to_string();
                    zip.start_file(&name, options)
                        .map_err(|e| Error::Tool(format!("Zip error: {}", e)))?;
                    let data = std::fs::read(src)?;
                    std::io::Write::write_all(&mut zip, &data)?;
                    file_count += 1;
                }
            }
            zip.finish().map_err(|e| Error::Tool(format!("Zip finish error: {}", e)))?;
        }
        "tar_gz" => {
            let file = std::fs::File::create(&dst)
                .map_err(|e| Error::Tool(format!("Failed to create archive: {}", e)))?;
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);

            for src in &sources {
                if src.is_dir() {
                    let dir_name = src.file_name().unwrap_or_default().to_string_lossy().to_string();
                    tar.append_dir_all(&dir_name, src)
                        .map_err(|e| Error::Tool(format!("Tar error: {}", e)))?;
                    file_count += count_files_recursive(src) as u64;
                } else {
                    let name = src.file_name().unwrap_or_default().to_string_lossy().to_string();
                    tar.append_path_with_name(src, &name)
                        .map_err(|e| Error::Tool(format!("Tar error: {}", e)))?;
                    file_count += 1;
                }
            }
            tar.finish().map_err(|e| Error::Tool(format!("Tar finish error: {}", e)))?;
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
    let base_name = base.file_name().unwrap_or_default().to_string_lossy().to_string();

    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(base)
            .map_err(|e| Error::Tool(format!("Path strip error: {}", e)))?;
        let archive_name = format!("{}/{}", base_name, relative.display());

        if path.is_dir() {
            zip.add_directory(format!("{}/", archive_name), options)
                .map_err(|e| Error::Tool(format!("Zip dir error: {}", e)))?;
            count += zip_add_dir(zip, base, &path, options)?;
        } else {
            zip.start_file(&archive_name, options)
                .map_err(|e| Error::Tool(format!("Zip file error: {}", e)))?;
            let data = std::fs::read(&path)?;
            std::io::Write::write_all(zip, &data)?;
            count += 1;
        }
    }
    Ok(count)
}

fn action_decompress(workspace: &Path, params: &Value) -> Result<Value> {
    let src_str = params["path"].as_str().unwrap();
    let src = expand_path(src_str, workspace);

    if !src.exists() {
        return Err(Error::NotFound(format!("Archive not found: {}", src.display())));
    }

    let dst = if let Some(d) = params.get("destination").and_then(|v| v.as_str()) {
        expand_path(d, workspace)
    } else {
        // Default: extract next to the archive
        src.parent().unwrap_or(workspace).to_path_buf()
    };

    std::fs::create_dir_all(&dst)?;

    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let name = src.file_name().unwrap_or_default().to_string_lossy().to_lowercase();

    let mut file_count = 0u64;

    if ext == "zip" {
        let file = std::fs::File::open(&src)
            .map_err(|e| Error::Tool(format!("Failed to open archive: {}", e)))?;
        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| Error::Tool(format!("Failed to read zip: {}", e)))?;

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)
                .map_err(|e| Error::Tool(format!("Zip entry error: {}", e)))?;
            let out_path = dst.join(entry.mangled_name());

            if entry.is_dir() {
                std::fs::create_dir_all(&out_path)?;
            } else {
                if let Some(parent) = out_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut out_file = std::fs::File::create(&out_path)?;
                std::io::copy(&mut entry, &mut out_file)?;
                file_count += 1;
            }
        }
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") || ext == "gz" {
        let file = std::fs::File::open(&src)
            .map_err(|e| Error::Tool(format!("Failed to open archive: {}", e)))?;
        let dec = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(dec);

        for entry in archive.entries().map_err(|e| Error::Tool(format!("Tar error: {}", e)))? {
            let mut entry = entry.map_err(|e| Error::Tool(format!("Tar entry error: {}", e)))?;
            entry.unpack_in(&dst).map_err(|e| Error::Tool(format!("Tar unpack error: {}", e)))?;
            file_count += 1;
        }
    } else if ext == "tar" {
        let file = std::fs::File::open(&src)
            .map_err(|e| Error::Tool(format!("Failed to open archive: {}", e)))?;
        let mut archive = tar::Archive::new(file);

        for entry in archive.entries().map_err(|e| Error::Tool(format!("Tar error: {}", e)))? {
            let mut entry = entry.map_err(|e| Error::Tool(format!("Tar entry error: {}", e)))?;
            entry.unpack_in(&dst).map_err(|e| Error::Tool(format!("Tar unpack error: {}", e)))?;
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
        return Err(Error::NotFound(format!("File not found: {}", path.display())));
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
        return Err(Error::NotFound(format!("Path not found: {}", path.display())));
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
        let count = count_files_recursive(&path);
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

    #[test]
    fn test_schema() {
        let tool = FileOpsTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "file_ops");
    }

    #[test]
    fn test_validate_delete() {
        let tool = FileOpsTool;
        assert!(tool.validate(&json!({"action": "delete", "path": "/tmp/test"})).is_ok());
        assert!(tool.validate(&json!({"action": "delete"})).is_err());
    }

    #[test]
    fn test_validate_compress() {
        let tool = FileOpsTool;
        assert!(tool.validate(&json!({
            "action": "compress",
            "path": "/tmp/test",
            "destination": "/tmp/test.zip"
        })).is_ok());
        assert!(tool.validate(&json!({
            "action": "compress",
            "destination": "/tmp/test.zip"
        })).is_err());
    }

    #[test]
    fn test_validate_move() {
        let tool = FileOpsTool;
        assert!(tool.validate(&json!({
            "action": "move",
            "path": "/tmp/a",
            "destination": "/tmp/b"
        })).is_ok());
        assert!(tool.validate(&json!({
            "action": "move",
            "path": "/tmp/a"
        })).is_err());
    }
}
