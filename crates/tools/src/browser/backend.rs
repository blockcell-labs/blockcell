//! 浏览器能力 seam：`browse` 工具只负责参数语义，由 [`BrowserBackend`] 决定浏览器在哪里、如何驱动。
//!
//! ```text
//! browse tool → BrowserBackend → Chrome CDP | Playwright | Remote Browser
//! ```
//!
//! Action 词汇（`navigate` / `click` / `screenshot` ...）与参数格式由工具 schema 定义，
//! 所有 backend 共享同一套契约。

use std::path::Path;

use async_trait::async_trait;
use blockcell_core::Result;
use serde_json::Value;

use super::session::BrowserEngine;

/// 一次需要浏览器会话的 action 调用。
#[derive(Debug, Clone, Copy)]
pub struct BrowserRequest<'a> {
    pub workspace: &'a Path,
    /// 会话名；同名会话在多次调用间保持（页面、cookie、元素 ref）。
    pub session: &'a str,
    pub headed: bool,
    pub engine: BrowserEngine,
    pub action: &'a str,
    /// 工具调用的原始参数，按 action 读取各自字段。
    pub params: &'a Value,
}

#[async_trait]
pub trait BrowserBackend: Send + Sync {
    async fn list_sessions(&self, workspace: &Path) -> Result<Vec<String>>;

    async fn close_session(&self, workspace: &Path, session: &str) -> Result<()>;

    /// 本 backend 可驱动的浏览器（engine, 可执行文件路径）。
    fn available_browsers(&self) -> Vec<(BrowserEngine, String)>;

    /// 在（必要时创建的）会话中执行 action；未知 action 返回错误。
    async fn perform(&self, request: BrowserRequest<'_>) -> Result<Value>;
}
