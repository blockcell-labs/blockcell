use crate::memory::{sanitize_fts_query, MemoryResult, MemoryStore, QueryParams};
use crate::vector::VectorHit;
use blockcell_core::Result;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use tracing::warn;

const RRF_K: usize = 60;

pub struct HybridMemoryRetriever<'a> {
    store: &'a MemoryStore,
}

impl<'a> HybridMemoryRetriever<'a> {
    pub fn new(store: &'a MemoryStore) -> Self {
        Self { store }
    }

    pub fn search(&self, params: &QueryParams) -> Result<Vec<MemoryResult>> {
        if params.top_k == 0 {
            return Ok(Vec::new());
        }

        let query = params.query.as_deref().map(str::trim).unwrap_or_default();
        if query.is_empty() {
            return self.store.query_sqlite_raw(params);
        }

        let fts_query = sanitize_fts_query(query);
        if fts_query.trim().is_empty() || fts_query == "\"\"" {
            return self.store.query_sqlite_raw(params);
        }

        let fts_window = candidate_window(params.top_k);
        let vector_window = candidate_window(params.top_k);

        let fts_hits = self.store.search_fts_candidates(&fts_query, fts_window)?;
        let vector_hits = self.search_vector_candidates(query, vector_window);

        let mut ranks: HashMap<String, (Option<usize>, Option<usize>)> = HashMap::new();
        let mut merged_ids = Vec::new();
        let mut seen = HashSet::new();

        for (rank, (id, _score)) in fts_hits.iter().enumerate() {
            ranks.entry(id.clone()).or_default().0 = Some(rank);
            if seen.insert(id.clone()) {
                merged_ids.push(id.clone());
            }
        }

        for (rank, hit) in vector_hits.iter().enumerate() {
            ranks.entry(hit.id.clone()).or_default().1 = Some(rank);
            if seen.insert(hit.id.clone()) {
                merged_ids.push(hit.id.clone());
            }
        }

        if merged_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut results: Vec<MemoryResult> = self
            .store
            .load_items_by_ids(&merged_ids)?
            .into_iter()
            .filter(|item| self.store.item_matches_query(item, params))
            .filter_map(|item| {
                let (fts_rank, vector_rank) = ranks.get(&item.id).copied().unwrap_or_default();
                let score = rrf_score(fts_rank) + rrf_score(vector_rank);
                if score > 0.0 {
                    Some(MemoryResult { item, score })
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(compare_results);
        results.truncate(params.top_k);
        Ok(results)
    }

    fn search_vector_candidates(&self, query: &str, top_k: usize) -> Vec<VectorHit> {
        let Some(runtime) = self.store.vector.as_ref() else {
            return Vec::new();
        };

        let vector = match runtime.embedder.embed_query(query) {
            Ok(vector) => vector,
            Err(error) => {
                warn!(error = %error, "Failed to embed query for vector retrieval");
                return Vec::new();
            }
        };

        match runtime.index.search(&vector, top_k) {
            Ok(hits) => hits,
            Err(error) => {
                warn!(error = %error, "Vector search failed, falling back to FTS");
                Vec::new()
            }
        }
    }
}

fn candidate_window(top_k: usize) -> usize {
    top_k.saturating_mul(4).max(20)
}

fn rrf_score(rank: Option<usize>) -> f64 {
    rank.map(|rank| 1.0 / (RRF_K + rank + 1) as f64)
        .unwrap_or_default()
}

fn compare_results(left: &MemoryResult, right: &MemoryResult) -> Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| {
            right
                .item
                .importance
                .partial_cmp(&left.item.importance)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| right.item.updated_at.cmp(&left.item.updated_at))
}
