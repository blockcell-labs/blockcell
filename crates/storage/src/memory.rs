use blockcell_core::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

// --- submodules extracted from the original monolithic memory.rs ---
mod brief;
mod crud;
mod import_migrate;
mod maintenance;
mod query;
mod schema;
mod vector_sync;

use crate::retriever::HybridMemoryRetriever;
use crate::vector::{VectorMeta, VectorRuntime};

pub use crate::memory_contract::MemoryType;

const VECTOR_SYNC_OP_UPSERT: &str = "upsert";
const VECTOR_SYNC_OP_DELETE: &str = "delete";
pub const MAX_MEMORY_QUERY_RESULTS: usize = 50;
pub const MAX_MEMORY_CONTENT_CHARS: usize = 32_768;

/// Scope of a memory item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    ShortTerm,
    LongTerm,
}

impl MemoryScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryScope::ShortTerm => "short_term",
            MemoryScope::LongTerm => "long_term",
        }
    }
}

impl std::str::FromStr for MemoryScope {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "short_term" => Ok(MemoryScope::ShortTerm),
            "long_term" => Ok(MemoryScope::LongTerm),
            _ => Err(format!("Invalid memory scope: {}", s)),
        }
    }
}

/// A memory item stored in the database.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: String,
    pub scope: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub title: Option<String>,
    pub content: String,
    pub summary: Option<String>,
    pub tags: Vec<String>,
    pub source: String,
    pub channel: Option<String>,
    pub session_key: Option<String>,
    pub importance: f64,
    pub created_at: String,
    pub updated_at: String,
    pub last_accessed_at: Option<String>,
    pub access_count: i64,
    pub expires_at: Option<String>,
    pub deleted_at: Option<String>,
    pub dedup_key: Option<String>,
}

/// Parameters for upserting a memory item.
pub struct UpsertParams {
    pub scope: String,
    pub item_type: String,
    pub title: Option<String>,
    pub content: String,
    pub summary: Option<String>,
    pub tags: Vec<String>,
    pub source: String,
    pub channel: Option<String>,
    pub session_key: Option<String>,
    pub importance: f64,
    pub dedup_key: Option<String>,
    pub expires_at: Option<String>,
}

/// Parameters for querying memory items.
pub struct QueryParams {
    pub query: Option<String>,
    pub session_key: Option<String>,
    pub scope: Option<String>,
    pub item_type: Option<String>,
    pub tags: Option<Vec<String>>,
    pub time_range_days: Option<i64>,
    pub top_k: usize,
    pub include_deleted: bool,
}

impl Default for QueryParams {
    fn default() -> Self {
        Self {
            query: None,
            session_key: None,
            scope: None,
            item_type: None,
            tags: None,
            time_range_days: None,
            top_k: 20,
            include_deleted: false,
        }
    }
}

impl QueryParams {
    pub fn bounded_top_k(&self) -> usize {
        self.top_k.min(MAX_MEMORY_QUERY_RESULTS)
    }
}

/// A query result with score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryResult {
    pub item: MemoryItem,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VectorSyncRetryResult {
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VectorReindexResult {
    pub indexed: usize,
    pub failed: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingVectorSync {
    id: String,
    operation: String,
}

#[derive(Clone, Default)]
pub struct MemoryStoreOptions {
    pub vector: Option<Arc<VectorRuntime>>,
}

/// SQLite-backed memory store with FTS5 full-text search.
#[derive(Clone)]
pub struct MemoryStore {
    pub(crate) inner: Arc<Mutex<Connection>>,
    #[allow(dead_code)]
    db_path: PathBuf,
    pub(crate) vector: Option<Arc<VectorRuntime>>,
}

fn build_embedding_text(item: &MemoryItem) -> String {
    let mut parts = Vec::new();

    if let Some(title) = item
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("Title: {}", title));
    }

    let summary = item
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| truncate_chars(item.content.trim(), 240));
    if !summary.is_empty() {
        parts.push(format!("Summary: {}", summary));
    }

    let tags: Vec<&str> = item
        .tags
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .take(3)
        .collect();
    if !tags.is_empty() {
        parts.push(format!("Tags: {}", tags.join(", ")));
    }

    if parts.is_empty() {
        truncate_chars(item.content.trim(), 240)
    } else {
        parts.join("\n")
    }
}

fn is_item_active_for_vector(item: &MemoryItem) -> bool {
    if item.deleted_at.is_some() {
        return false;
    }

    match item.expires_at.as_deref() {
        Some(expires_at) => match DateTime::parse_from_rfc3339(expires_at) {
            Ok(value) => value.with_timezone(&Utc) > Utc::now(),
            Err(_) => false,
        },
        None => true,
    }
}

fn format_relevant_brief_item(item: &MemoryItem) -> String {
    let display = item
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            item.title
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|title| {
                    let first_line = item.content.lines().next().unwrap_or("").trim();
                    if first_line.is_empty() {
                        title.to_string()
                    } else {
                        format!("{}: {}", title, truncate_chars(first_line, 100))
                    }
                })
        })
        .unwrap_or_else(|| truncate_chars(item.content.trim(), 120));
    let scope_tag = if item.scope == "long_term" {
        "LT"
    } else {
        "ST"
    };
    format!("- [{}|{}] {}", item.item_type, scope_tag, display)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let truncated: String = value.chars().take(max_chars).collect();
    if value.chars().count() > max_chars {
        format!("{}...", truncated)
    } else {
        truncated
    }
}

/// Sanitize a user query for FTS5 (escape special characters, use implicit AND).
pub(crate) fn sanitize_fts_query(query: &str) -> String {
    crate::fts::build_fts_query(query)
}

/// Parse markdown content into (heading, body) sections.
/// 识别 ## 和 ### 级别的标题（# 为文档标题，跳过）。
fn parse_markdown_sections(content: &str) -> Vec<(String, String)> {
    let mut sections = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_body = String::new();

    for line in content.lines() {
        if line.starts_with("## ") || line.starts_with("### ") {
            // 保存上一个 section
            if let Some(heading) = current_heading.take() {
                sections.push((heading, current_body.clone()));
            }
            // 剥离前缀 # 字符和空格
            let heading_text = line.trim_start_matches('#').trim().to_string();
            current_heading = Some(heading_text);
            current_body.clear();
        } else if line.starts_with("# ") && current_heading.is_none() {
            // 顶级标题为文档标题，跳过
            continue;
        } else if current_heading.is_some() {
            current_body.push_str(line);
            current_body.push('\n');
        }
    }

    // 保存最后一个 section
    if let Some(heading) = current_heading {
        sections.push((heading, current_body));
    }

    sections
}

/// Classify a section heading into a memory type.
fn classify_section(heading: &str) -> String {
    let h = heading.to_lowercase();
    if h.contains("preference") || h.contains("偏好") {
        "preference".to_string()
    } else if h.contains("project") || h.contains("项目") {
        "project".to_string()
    } else if h.contains("user") || h.contains("用户") || h.contains("info") {
        "fact".to_string()
    } else if h.contains("task") || h.contains("todo") || h.contains("任务") {
        "task".to_string()
    } else if h.contains("policy") || h.contains("rule") || h.contains("规则") {
        "policy".to_string()
    } else if h.contains("contact") || h.contains("联系") {
        "contact".to_string()
    } else {
        "note".to_string()
    }
}

/// Compute an expiry date for a daily note: date + days.
fn compute_daily_expiry(date_str: &str, days: i64) -> Option<String> {
    chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
        .ok()
        .map(|d| {
            let expiry = d + chrono::Duration::days(days);
            let dt: DateTime<Utc> =
                DateTime::from_naive_utc_and_offset(expiry.and_hms_opt(0, 0, 0).unwrap(), Utc);
            dt.to_rfc3339()
        })
}

#[cfg(test)]
mod tests;
