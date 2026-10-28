use async_trait::async_trait;
use blockcell_core::{Error, Result};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use crate::{Tool, ToolContext, ToolSchema};

#[derive(Clone)]
pub struct ToolSearchEntry {
    pub name: String,
    pub description: String,
}

#[derive(Default)]
struct ToolSearchState {
    catalogs: HashMap<String, Vec<ToolSearchEntry>>,
    activated: HashMap<String, Vec<String>>,
}

static TOOL_SEARCH_STATE: Lazy<Mutex<ToolSearchState>> =
    Lazy::new(|| Mutex::new(ToolSearchState::default()));

pub fn set_tool_search_catalog(session_key: &str, entries: Vec<ToolSearchEntry>) {
    if let Ok(mut state) = TOOL_SEARCH_STATE.lock() {
        state.catalogs.insert(session_key.to_string(), entries);
    }
}

pub fn activate_tool_schema(session_key: &str, name: &str) {
    if let Ok(mut state) = TOOL_SEARCH_STATE.lock() {
        let names = state.activated.entry(session_key.to_string()).or_default();
        if !names.iter().any(|current| current == name) {
            names.push(name.to_string());
            names.sort();
        }
    }
}

pub fn activated_tool_schemas(session_key: &str) -> Vec<String> {
    TOOL_SEARCH_STATE
        .lock()
        .ok()
        .and_then(|state| state.activated.get(session_key).cloned())
        .unwrap_or_default()
}

pub struct ToolSearchTool;

#[async_trait]
impl Tool for ToolSearchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "tool_search".to_string(),
            description: "Discover authorized tools by exact name or a short Chinese/English task description. Results reveal schemas, never grant permissions. If no match, inspect the bounded catalog summary; stop after one reformulated search. Do not search for alternatives after a policy or user denial.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        if params.get("query").and_then(Value::as_str).is_none() {
            return Err(Error::Validation(
                "Missing required parameter: query".to_string(),
            ));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let query = params["query"].as_str().unwrap().trim().to_lowercase();
        let state = TOOL_SEARCH_STATE
            .lock()
            .map_err(|_| Error::Tool("Tool search catalog is unavailable".to_string()))?;
        Ok(search_catalog(
            state
                .catalogs
                .get(&ctx.session_key)
                .map(Vec::as_slice)
                .unwrap_or_default(),
            &query,
        ))
    }
}

const MAX_SEARCH_RESULTS: usize = 10;

// Small task vocabulary for cross-language authoring compatibility, not intent
// routing rules. Only retrieval scores are affected; the catalog is unchanged.
const TERM_ALIASES: &[(&str, &str)] = &[
    ("读取", "read"),
    ("写入", "write"),
    ("文件", "file"),
    ("搜索", "search"),
    ("网页", "web"),
    ("网站", "web"),
    ("图片", "image"),
    ("图表", "chart"),
    ("邮件", "email"),
    ("视频", "video"),
    ("音频", "audio"),
    ("转录", "transcribe"),
    ("文本", "text"),
    ("数据", "data"),
    ("分析", "analyze"),
    ("日程", "schedule"),
    ("日历", "calendar"),
    ("任务", "task"),
    ("提醒", "reminder"),
    ("目录", "directory"),
    ("浏览", "browse"),
];

fn is_chinese(ch: char) -> bool {
    matches!(ch, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}')
}

/// Latin words and Chinese bigrams avoid requiring an external tokenizer/model.
fn lexical_terms(text: &str) -> BTreeSet<String> {
    let text = text.to_lowercase();
    let mut terms = BTreeSet::new();
    let mut latin = String::new();
    let mut chinese = Vec::new();
    let flush = |latin: &mut String, chinese: &mut Vec<char>, terms: &mut BTreeSet<String>| {
        if !latin.is_empty() {
            let word = std::mem::take(latin);
            if !matches!(
                word.as_str(),
                "a" | "an" | "the" | "to" | "of" | "in" | "and" | "for" | "into" | "please"
            ) {
                terms.insert(word);
            }
        }
        if chinese.len() == 1 {
            terms.insert(chinese[0].to_string());
        } else {
            for pair in chinese.windows(2) {
                terms.insert(pair.iter().collect());
            }
        }
        chinese.clear();
    };
    for ch in text.chars() {
        if is_chinese(ch) {
            if !latin.is_empty() {
                flush(&mut latin, &mut chinese, &mut terms);
            }
            chinese.push(ch);
        } else if ch.is_ascii_alphanumeric() {
            if !chinese.is_empty() {
                flush(&mut latin, &mut chinese, &mut terms);
            }
            latin.push(ch);
        } else {
            flush(&mut latin, &mut chinese, &mut terms);
        }
    }
    flush(&mut latin, &mut chinese, &mut terms);
    terms
}

fn query_terms(query: &str) -> BTreeSet<String> {
    let mut terms = lexical_terms(query);
    for &(chinese, english) in TERM_ALIASES {
        if query.contains(chinese) {
            terms.insert(english.into());
        }
        if terms.contains(english) {
            terms.extend(lexical_terms(chinese));
        }
    }
    terms
}

/// Deterministic for a fixed query/catalog. Scores are lexical ranking weights,
/// not confidence values and must not be interpreted as measured recall.
pub fn search_entries(entries: &[ToolSearchEntry], query: &str) -> Vec<Value> {
    let query = query.trim().to_lowercase();
    let terms = query_terms(&query);
    let mut ranked: Vec<_> = entries
        .iter()
        .filter_map(|entry| {
            let name = entry.name.to_lowercase();
            let names = lexical_terms(&name);
            let description = lexical_terms(&entry.description);
            let score = if query.is_empty() {
                1
            } else if name == query {
                100_000
            } else {
                let name_hits = terms.intersection(&names).count();
                let description_hits = terms.intersection(&description).count();
                let prefix = usize::from(name.starts_with(&query));
                prefix * 10_000 + name_hits * 100 + description_hits * 30
            };
            (score > 0).then_some((score, entry))
        })
        .collect();
    ranked.sort_by(|(a_score, a), (b_score, b)| {
        b_score
            .cmp(a_score)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.description.cmp(&b.description))
    });
    let mut seen = BTreeSet::new();
    ranked
        .into_iter()
        .filter(|(_, entry)| seen.insert(entry.name.as_str()))
        .take(MAX_SEARCH_RESULTS)
        .map(|(_, entry)| json!({"name": entry.name, "description": entry.description}))
        .collect()
}

/// Structured search result; only `matches` are eligible for schema promotion.
pub fn search_catalog(entries: &[ToolSearchEntry], query: &str) -> Value {
    let query = query.trim().to_lowercase();
    let matches = search_entries(entries, &query);
    if !matches.is_empty() {
        return json!({"query": query, "matches": matches, "strategy": "lexical", "status": "matched"});
    }

    // One bounded recovery surface, not a second source of authorization. Keep
    // fallback outside `matches`: runtime may promote only actual matches.
    let mut catalog: Vec<_> = entries.iter().collect();
    catalog.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.description.cmp(&b.description))
    });
    catalog.dedup_by(|a, b| a.name == b.name);
    let catalog_size = catalog.len();
    let mut remaining_chars = 1_200;
    let mut summary = Vec::new();
    for entry in catalog {
        if summary.len() >= MAX_SEARCH_RESULTS {
            break;
        }
        let name_chars = entry.name.chars().count();
        if name_chars > remaining_chars {
            continue;
        }
        remaining_chars -= name_chars;
        let description: String = entry
            .description
            .chars()
            .take(96.min(remaining_chars))
            .collect();
        remaining_chars -= description.chars().count();
        summary.push(json!({"name": entry.name, "description": description}));
    }
    json!({
        "query": query,
        "matches": [],
        "strategy": "lexical",
        "status": "empty",
        "recovery": {
            "kind": "catalog_summary",
            "catalog_summary": summary,
            "omitted_count": catalog_size.saturating_sub(summary.len()),
            "max_reformulations": 1,
            "hint": "No lexical match. Use an exact listed name or reformulate once; if still unavailable, explain the limitation or ask for clarification. Do not retry or bypass policy/user denials."
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, description: &str) -> ToolSearchEntry {
        ToolSearchEntry {
            name: name.into(),
            description: description.into(),
        }
    }

    #[test]
    fn search_ranks_multiword_english_by_relevance() {
        let entries = vec![
            entry("audio_transcribe", "Transcribe audio recordings into text"),
            entry("text_read", "Read text files"),
            entry("video_process", "Process video files"),
        ];
        let matches = search_entries(&entries, "transcribe audio into text");
        assert_eq!(matches[0]["name"], "audio_transcribe");
    }

    #[test]
    fn search_finds_chinese_requests_against_english_descriptions() {
        let entries = vec![
            entry("read_file", "Read a text file"),
            entry("web_search", "Search the web"),
        ];
        let matches = search_entries(&entries, "读取文件");
        assert_eq!(
            matches.first().map(|m| m["name"].as_str()),
            Some(Some("read_file"))
        );
    }

    #[test]
    fn search_matches_chinese_description_without_whole_query_substring() {
        let entries = vec![
            entry("quote", "查询股票行情和股价"),
            entry("calendar", "管理日历事件"),
        ];
        let matches = search_entries(&entries, "帮我查询今天股票的股价");
        assert_eq!(
            matches.first().map(|m| m["name"].as_str()),
            Some(Some("quote"))
        );
    }

    #[test]
    fn search_caps_after_ranking_and_is_catalog_order_independent() {
        let mut entries: Vec<_> = (0..20)
            .map(|i| entry(&format!("a{i:02}"), "Find video files"))
            .collect();
        entries.push(entry("video_process", "Process videos"));
        let forward = search_entries(&entries, "video_process");
        entries.reverse();
        assert_eq!(forward, search_entries(&entries, "video_process"));
        assert_eq!(forward[0]["name"], "video_process");
        let forward = search_entries(&entries, "");
        entries.reverse();
        assert_eq!(forward, search_entries(&entries, ""));
        assert_eq!(forward.len(), 10);
    }

    #[test]
    fn empty_search_returns_bounded_authorized_summary_not_matches() {
        let entries: Vec<_> = (0..30)
            .map(|i| entry(&format!("tool_{i:02}"), &"描述".repeat(200)))
            .collect();
        let result = search_catalog(&entries, "unfindable");
        assert_eq!(result["matches"], json!([]));
        let recovery = &result["recovery"];
        assert_eq!(recovery["kind"], "catalog_summary");
        let summary = recovery["catalog_summary"]
            .as_array()
            .expect("bounded summary");
        assert!(!summary.is_empty());
        assert!(summary.len() <= 10);
        let chars: usize = summary
            .iter()
            .map(|v| {
                v["name"].as_str().unwrap().chars().count()
                    + v["description"].as_str().unwrap().chars().count()
            })
            .sum();
        assert!(chars <= 1200);
        assert!(recovery["omitted_count"].as_u64().unwrap() > 0);
        for v in summary {
            assert!(entries
                .iter()
                .any(|e| e.name == v["name"].as_str().unwrap()));
        }
        let mut reversed = entries.clone();
        reversed.reverse();
        assert_eq!(result, search_catalog(&reversed, "unfindable"));
    }

    #[test]
    fn matching_search_has_no_fallback_summary() {
        let result = search_catalog(&[entry("read_file", "Read a file")], "read_file");
        assert_eq!(result["status"], "matched");
        assert!(result["recovery"].is_null());
    }

    #[test]
    fn search_never_returns_a_duplicate_tool_name() {
        let entries = [
            entry("read_file", "Read"),
            entry("read_file", "Read local file text"),
            entry("read_text", "Read a file"),
        ];
        let results = search_entries(&entries, "read file text");
        let names: BTreeSet<_> = results
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), results.len());
    }

    #[test]
    fn search_returns_sorted_allowed_tool_summaries() {
        let entries = vec![
            ToolSearchEntry {
                name: "web_fetch".to_string(),
                description: "Fetch a web page".to_string(),
            },
            ToolSearchEntry {
                name: "video_process".to_string(),
                description: "Process videos".to_string(),
            },
        ];
        let matches = search_entries(&entries, "video");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0]["name"], "video_process");
    }

    #[test]
    fn searched_tools_remain_activated_for_session() {
        activate_tool_schema("tool-search-test-session", "video_process");
        assert_eq!(
            activated_tool_schemas("tool-search-test-session"),
            vec!["video_process"]
        );
    }
}
