//! 文件系统能力 seam：文件工具只描述"读写哪个路径"，由 [`FilesystemBackend`] 决定在哪里访问。
//!
//! ```text
//! read_file / write_file / edit_file / list_dir → FilesystemBackend → Local | Remote | ...
//! ```
//!
//! 路径解析（workspace 相对路径、`~/`）与参数校验仍由工具负责；backend 只接收绝对路径。

use std::path::Path;

use async_trait::async_trait;
use blockcell_core::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsEntryKind {
    File,
    Directory,
    Other,
}

impl FsEntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEntry {
    pub name: String,
    pub kind: FsEntryKind,
}

#[async_trait]
pub trait FilesystemBackend: Send + Sync {
    /// 路径不存在时返回 `None`。
    async fn kind(&self, path: &Path) -> Result<Option<FsEntryKind>>;

    /// 按 UTF-8 原样读取文件内容。
    async fn read_to_string(&self, path: &Path) -> Result<String>;

    /// 返回文件字节数；路径不存在或不是普通文件时返回 `None`。
    ///
    /// 供工具在读取前做大小上限判断，避免把超大文件整体读入内存。
    async fn size(&self, path: &Path) -> Result<Option<u64>>;

    /// 读取供模型阅读的文本：Office 文档（xlsx/xls/docx/pptx）转换为文本，其余同
    /// [`Self::read_to_string`]。转换结果不可写回原文件。
    async fn read_document(&self, path: &Path) -> Result<String>;

    /// 写入文件，必要时创建父目录。
    async fn write(&self, path: &Path, content: &str) -> Result<()>;

    async fn list_dir(&self, path: &Path) -> Result<Vec<FsEntry>>;
}

/// 直接访问本机文件系统。
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalFilesystem;

#[async_trait]
impl FilesystemBackend for LocalFilesystem {
    async fn kind(&self, path: &Path) -> Result<Option<FsEntryKind>> {
        match tokio::fs::metadata(path).await {
            Ok(metadata) => Ok(Some(file_type_kind(metadata.file_type()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    async fn read_to_string(&self, path: &Path) -> Result<String> {
        Ok(tokio::fs::read_to_string(path).await?)
    }

    async fn size(&self, path: &Path) -> Result<Option<u64>> {
        match tokio::fs::metadata(path).await {
            Ok(metadata) if metadata.is_file() => Ok(Some(metadata.len())),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    async fn read_document(&self, path: &Path) -> Result<String> {
        if !crate::office::is_office_file(path) {
            return self.read_to_string(path).await;
        }
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || crate::office::read_office_file(&path))
            .await
            .map_err(|e| Error::Tool(format!("Failed to read office file: {}", e)))?
    }

    async fn write(&self, path: &Path, content: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        Ok(tokio::fs::write(path, content).await?)
    }

    async fn list_dir(&self, path: &Path) -> Result<Vec<FsEntry>> {
        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(path).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            entries.push(FsEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                kind: file_type_kind(entry.file_type().await?),
            });
        }
        Ok(entries)
    }
}

fn file_type_kind(file_type: std::fs::FileType) -> FsEntryKind {
    if file_type.is_dir() {
        FsEntryKind::Directory
    } else if file_type.is_file() {
        FsEntryKind::File
    } else {
        FsEntryKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_filesystem_round_trips_and_reports_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let fs = LocalFilesystem;
        let file = dir.path().join("nested/a.txt");

        assert_eq!(fs.kind(&file).await.unwrap(), None);
        fs.write(&file, "hello").await.unwrap();
        assert_eq!(fs.kind(&file).await.unwrap(), Some(FsEntryKind::File));
        assert_eq!(fs.read_to_string(&file).await.unwrap(), "hello");
        assert_eq!(fs.read_document(&file).await.unwrap(), "hello");

        let nested = dir.path().join("nested");
        assert_eq!(
            fs.kind(&nested).await.unwrap(),
            Some(FsEntryKind::Directory)
        );
        assert_eq!(
            fs.list_dir(&nested).await.unwrap(),
            vec![FsEntry {
                name: "a.txt".into(),
                kind: FsEntryKind::File
            }]
        );
    }
}
