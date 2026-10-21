use blockcell_core::config::PromptBudgetConfig;
use blockcell_core::{Paths, Result};
use blockcell_tools::MemoryStoreHandle;
use serde_json::json;
use std::collections::HashMap;

use crate::token::estimate_tokens;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetrievalSource {
    UserProfile,
    CanonicalKnowledge,
    Session,
    ShortTerm,
    SkillIndex,
    KnowledgeGraph,
}

impl RetrievalSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::UserProfile => "user-profile",
            Self::CanonicalKnowledge => "knowledge",
            Self::Session => "session",
            Self::ShortTerm => "short-term",
            Self::SkillIndex => "skill-index",
            Self::KnowledgeGraph => "knowledge-graph",
        }
    }

    fn priority(self) -> u8 {
        match self {
            Self::UserProfile => 6,
            Self::CanonicalKnowledge => 5,
            Self::Session => 4,
            Self::ShortTerm => 3,
            Self::SkillIndex => 2,
            Self::KnowledgeGraph => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievedItem {
    pub source: RetrievalSource,
    pub content: String,
}

impl RetrievedItem {
    pub fn new(source: RetrievalSource, content: impl Into<String>) -> Self {
        Self {
            source,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PromptSections {
    pub rules: String,
    pub user_profile: String,
    pub retrieved: String,
    pub active_skill: String,
    pub session_recovery: String,
}

pub struct RetrievalOrchestrator;

impl RetrievalOrchestrator {
    pub fn deduplicate(items: Vec<RetrievedItem>) -> Vec<RetrievedItem> {
        let mut by_content: HashMap<String, RetrievedItem> = HashMap::new();
        for mut item in items {
            item.content = item
                .content
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if item.content.is_empty() {
                continue;
            }
            let key = item.content.to_lowercase();
            match by_content.get(&key) {
                Some(existing) if existing.source.priority() >= item.source.priority() => {}
                _ => {
                    by_content.insert(key, item);
                }
            }
        }
        let mut result = by_content.into_values().collect::<Vec<_>>();
        result.sort_by(|left, right| {
            right
                .source
                .priority()
                .cmp(&left.source.priority())
                .then_with(|| left.content.cmp(&right.content))
        });
        result
    }

    pub fn render(items: &[RetrievedItem]) -> String {
        Self::render_with_item_cap(items, usize::MAX)
    }

    pub fn render_with_item_cap(items: &[RetrievedItem], item_token_cap: usize) -> String {
        items
            .iter()
            .map(|item| {
                let content = truncate_to_token_budget(&item.content, item_token_cap);
                format!("- [{}] {}", item.source.label(), content.trim())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn retrieve(
        paths: &Paths,
        memory_store: Option<&MemoryStoreHandle>,
        session_key: Option<&str>,
        query: &str,
        skill_index_summary: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RetrievedItem>> {
        Self::retrieve_with_snapshot(
            paths,
            memory_store,
            session_key,
            query,
            skill_index_summary,
            None,
            limit,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn retrieve_with_snapshot(
        paths: &Paths,
        memory_store: Option<&MemoryStoreHandle>,
        session_key: Option<&str>,
        query: &str,
        skill_index_summary: Option<&str>,
        canonical_snapshot: Option<(Option<&str>, Option<&str>)>,
        limit: usize,
    ) -> Result<Vec<RetrievedItem>> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut items = Vec::new();
        if let Some((user, memory)) = canonical_snapshot {
            if let Some(user) = user {
                items.extend(
                    relevant_text_chunks(user, query)
                        .into_iter()
                        .map(|content| RetrievedItem::new(RetrievalSource::UserProfile, content)),
                );
            }
            if let Some(memory) = memory {
                items.extend(
                    relevant_text_chunks(memory, query)
                        .into_iter()
                        .map(|content| {
                            RetrievedItem::new(RetrievalSource::CanonicalKnowledge, content)
                        }),
                );
            }
        } else {
            let index = blockcell_storage::KnowledgeIndex::open(&paths.knowledge_index_db())?;
            index.rebuild_from_files(paths)?;
            for entry in index.search(query, limit.saturating_mul(4).max(limit))? {
                let source = if entry.file == "USER.md" {
                    RetrievalSource::UserProfile
                } else {
                    RetrievalSource::CanonicalKnowledge
                };
                items.push(RetrievedItem::new(source, entry.content));
            }
        }

        if let Some(session_key) = session_key {
            let session_root = paths
                .memory_dir()
                .join("sessions")
                .join(blockcell_core::stable_hash_session_key(session_key));
            for path in [session_root.join("USER.md"), session_root.join("MEMORY.md")] {
                if let Ok(content) = std::fs::read_to_string(path) {
                    items.extend(
                        relevant_text_chunks(&content, query)
                            .into_iter()
                            .map(|content| RetrievedItem::new(RetrievalSource::Session, content)),
                    );
                }
            }
        }

        if let Some(store) = memory_store {
            if let Ok(rows) = store.query_json(json!({
                "query": query,
                "session_key": session_key,
                "scope": "short_term",
                "top_k": limit.min(50),
                "include_deleted": false
            })) {
                if let Some(rows) = rows.as_array() {
                    for row in rows {
                        let item = row.get("item").unwrap_or(row);
                        if let Some(content) = item.get("content").and_then(|value| value.as_str())
                        {
                            items.push(RetrievedItem::new(RetrievalSource::ShortTerm, content));
                        }
                    }
                }
            }
        }

        if let Some(summary) = skill_index_summary.filter(|summary| !summary.trim().is_empty()) {
            let relevant = relevant_text_chunks(summary, query);
            if !relevant.is_empty() {
                items.extend(
                    relevant
                        .into_iter()
                        .map(|content| RetrievedItem::new(RetrievalSource::SkillIndex, content)),
                );
            }
        }

        let mut items = Self::deduplicate(items);
        items.truncate(limit);
        Ok(items)
    }
}

fn relevant_text_chunks(content: &str, query: &str) -> Vec<String> {
    let tokens = query
        .split(|character: char| character.is_whitespace() || character.is_ascii_punctuation())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return Vec::new();
    }
    content
        .split("\n\n")
        .map(str::trim)
        .filter(|chunk| !chunk.is_empty())
        .filter(|chunk| {
            let lower = chunk.to_lowercase();
            tokens.iter().any(|token| lower.contains(token))
        })
        .map(ToOwned::to_owned)
        .collect()
}

pub struct PromptBudgetAllocator {
    config: PromptBudgetConfig,
}

impl PromptBudgetAllocator {
    pub fn new(config: PromptBudgetConfig) -> Self {
        Self { config }
    }

    pub fn assemble(&self, sections: PromptSections) -> String {
        let mut output = String::new();
        let mut remaining = self.config.total;
        for (header, content, section_budget) in [("## Rules", sections.rules, self.config.rules)] {
            if content.trim().is_empty() || remaining == 0 || section_budget == 0 {
                continue;
            }
            let rendered = format!("{header}\n{}\n\n", content.trim());
            let allocated = section_budget.min(remaining);
            let truncated = truncate_to_token_budget(&rendered, allocated);
            let used = estimate_tokens(&truncated);
            if truncated.trim().is_empty() || used == 0 {
                continue;
            }
            output.push_str(&truncated);
            if !truncated.ends_with('\n') {
                output.push('\n');
            }
            remaining = remaining.saturating_sub(used);
        }

        if remaining > 0
            && (!sections.user_profile.trim().is_empty() || !sections.retrieved.trim().is_empty())
        {
            let header = concat!(
                "<retrieved-context>\n",
                "Relevant context from configured knowledge sources.\n",
                "Use only when directly relevant. Current user instructions override this context.\n"
            );
            let footer = "</retrieved-context>\n\n";
            let base_tokens = estimate_tokens(header) + estimate_tokens(footer);
            if base_tokens < remaining {
                let mut block = String::from(header);
                let mut block_remaining = remaining - base_tokens;
                for (content, configured_budget) in [
                    (sections.user_profile, self.config.user_profile),
                    (sections.retrieved, self.config.retrieved),
                ] {
                    if content.trim().is_empty() || block_remaining == 0 {
                        continue;
                    }
                    let budget = configured_budget.min(block_remaining);
                    let truncated = truncate_to_token_budget(content.trim(), budget);
                    let used = estimate_tokens(&truncated);
                    if used > 0 {
                        block.push_str(&truncated);
                        block.push('\n');
                        block_remaining = block_remaining.saturating_sub(used);
                    }
                }
                block.push_str(footer);
                let used = estimate_tokens(&block);
                if used <= remaining {
                    output.push_str(&block);
                    remaining -= used;
                }
            }
        }

        for (header, content, section_budget) in [
            (
                "## Active Skill",
                sections.active_skill,
                self.config.active_skill,
            ),
            (
                "## Session Recovery",
                sections.session_recovery,
                self.config.session_recovery,
            ),
        ] {
            if content.trim().is_empty() || remaining == 0 || section_budget == 0 {
                continue;
            }
            let rendered = format!("{header}\n{}\n\n", content.trim());
            let allocated = section_budget.min(remaining);
            let truncated = truncate_to_token_budget(&rendered, allocated);
            let used = estimate_tokens(&truncated);
            if truncated.trim().is_empty() || used == 0 {
                continue;
            }
            output.push_str(&truncated);
            if !truncated.ends_with('\n') {
                output.push('\n');
            }
            remaining = remaining.saturating_sub(used);
        }
        if estimate_tokens(&output) > self.config.total {
            truncate_to_token_budget(&output, self.config.total)
        } else {
            output
        }
    }
}

pub(crate) fn truncate_to_token_budget(text: &str, budget: usize) -> String {
    if budget == 0 || text.is_empty() {
        return String::new();
    }
    if estimate_tokens(text) <= budget {
        return text.to_string();
    }
    let boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .collect::<Vec<_>>();
    let mut low = 0usize;
    let mut high = boundaries.len().saturating_sub(1);
    while low < high {
        let mid = (low + high).div_ceil(2);
        if estimate_tokens(&text[..boundaries[mid]]) <= budget {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    text[..boundaries[low]].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::estimate_tokens;

    #[test]
    fn retrieval_deduplicates_semantic_content_across_sources() {
        let items = vec![
            RetrievedItem::new(RetrievalSource::UserProfile, "User prefers concise replies"),
            RetrievedItem::new(
                RetrievalSource::ShortTerm,
                " User   prefers concise replies ",
            ),
        ];

        let result = RetrievalOrchestrator::deduplicate(items);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, RetrievalSource::UserProfile);
    }

    #[test]
    fn prompt_allocator_never_exceeds_total_budget() {
        let config = blockcell_core::config::PromptBudgetConfig {
            total: 8_000,
            rules: 2_000,
            user_profile: 800,
            retrieved: 3_000,
            active_skill: 4_000,
            session_recovery: 2_000,
        };
        let oversized = "规则和上下文。".repeat(8_000);
        let sections = PromptSections {
            rules: oversized.clone(),
            user_profile: oversized.clone(),
            retrieved: oversized.clone(),
            active_skill: oversized.clone(),
            session_recovery: oversized,
        };

        let prompt = PromptBudgetAllocator::new(config).assemble(sections);

        assert!(estimate_tokens(&prompt) <= 8_000);
    }

    #[test]
    fn retrieval_render_caps_each_item_before_section_budgeting() {
        let oversized = format!("release {} RETRIEVED_ITEM_END", "detail ".repeat(2_000));
        let items = vec![RetrievedItem::new(
            RetrievalSource::CanonicalKnowledge,
            oversized,
        )];

        let rendered = RetrievalOrchestrator::render_with_item_cap(&items, 500);

        assert!(estimate_tokens(&rendered) <= 510);
        assert!(!rendered.contains("RETRIEVED_ITEM_END"));
    }
}
