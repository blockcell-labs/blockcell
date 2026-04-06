use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use blockcell_core::{Config, Paths};
use blockcell_providers::create_embedder;
use blockcell_storage::rabitq_index::RabitqIndex;
use blockcell_storage::vector::VectorRuntime;
use blockcell_storage::{MemoryStore, MemoryStoreOptions};
use tracing::warn;

pub fn open_memory_store(paths: &Paths, config: &Config) -> anyhow::Result<MemoryStore> {
    let memory_db_path = paths.memory_dir().join("memory.db");
    let vector = match build_vector_runtime(paths, config) {
        Ok(vector) => vector,
        Err(error) => {
            warn!(
                agent_base = %paths.base.display(),
                error = %error,
                "Vector memory initialization failed, falling back to SQLite-only"
            );
            None
        }
    };

    MemoryStore::open_with_options(&memory_db_path, MemoryStoreOptions { vector })
        .map_err(|error| anyhow::anyhow!("Failed to open memory db: {}", error))
}

fn build_vector_runtime(
    paths: &Paths,
    config: &Config,
) -> anyhow::Result<Option<Arc<VectorRuntime>>> {
    let Some(embedder) = create_embedder(config)? else {
        return Ok(None);
    };

    let uri = resolve_vector_uri(paths, config);
    let table_name = config.memory.vector.table.trim().to_string();
    let table_name = if table_name.is_empty() {
        "memory_vectors".to_string()
    } else {
        table_name
    };

    let index = RabitqIndex::open_or_create(&uri, &table_name)
        .map_err(|error| anyhow::anyhow!("Failed to initialize RabitQ index: {}", error))
        .with_context(|| format!("vector uri={}, table={}", uri, table_name))?;

    Ok(Some(Arc::new(VectorRuntime {
        embedder,
        index: Arc::new(index),
    })))
}

fn resolve_vector_uri(paths: &Paths, config: &Config) -> String {
    if let Some(configured) = config
        .memory
        .vector
        .uri
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if configured.contains("://") {
            return configured.to_string();
        }

        let path = PathBuf::from(configured);
        if path.is_absolute() {
            return path.to_string_lossy().to_string();
        }

        return paths.memory_dir().join(path).to_string_lossy().to_string();
    }

    paths
        .memory_dir()
        .join("vectors.rabitq")
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_vector_uri_defaults_under_memory_dir() {
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell-test"));
        let config = Config::default();

        assert_eq!(
            resolve_vector_uri(&paths, &config),
            "/tmp/blockcell-test/workspace/memory/vectors.rabitq"
        );
    }

    #[test]
    fn test_resolve_vector_uri_joins_relative_paths() {
        let paths = Paths::with_base(PathBuf::from("/tmp/blockcell-test"));
        let mut config = Config::default();
        config.memory.vector.uri = Some("rabitq/index".to_string());

        assert_eq!(
            resolve_vector_uri(&paths, &config),
            "/tmp/blockcell-test/workspace/memory/rabitq/index"
        );
    }
}
