use async_trait::async_trait;
use blockcell_core::{Error, Result};
use serde_json::{json, Value};
use tracing::debug;

use crate::{Tool, ToolContext, ToolSchema};

/// Lightweight knowledge graph tool backed by SQLite.
///
/// Stores entities (nodes) and relations (edges) in a local SQLite database.
/// Supports:
/// - Entity CRUD with types, properties, and tags
/// - Relation CRUD with types and properties
/// - Path queries (shortest path between entities)
/// - Subgraph extraction (neighborhood of an entity)
/// - Full-text search across entities
/// - Graph statistics
/// - Export to JSON/DOT/Mermaid formats
pub struct KnowledgeGraphTool;

#[async_trait]
impl Tool for KnowledgeGraphTool {
    fn schema(&self) -> ToolSchema {
        let mut props = serde_json::Map::new();
        props.insert("action".into(), json!({"type": "string", "description": "Action: add_entity|get_entity|update_entity|delete_entity|search_entities|add_relation|get_relations|delete_relation|find_path|subgraph|stats|export|query|merge_entity"}));
        props.insert("entity_id".into(), json!({"type": "string", "description": "(most actions) Entity identifier. Auto-generated if not provided for add_entity."}));
        props.insert("entity_type".into(), json!({"type": "string", "description": "(add_entity/search_entities) Entity type (e.g. 'person', 'concept', 'project', 'skill', 'book')"}));
        props.insert("name".into(), json!({"type": "string", "description": "(add_entity/update_entity) Entity display name"}));
        props.insert("properties".into(), json!({"type": "object", "description": "(add_entity/update_entity/add_relation) Key-value properties"}));
        props.insert("tags".into(), json!({"type": "array", "items": {"type": "string"}, "description": "(add_entity/update_entity) Tags for categorization"}));
        props.insert("source_id".into(), json!({"type": "string", "description": "(add_relation/get_relations) Source entity ID"}));
        props.insert("target_id".into(), json!({"type": "string", "description": "(add_relation/get_relations) Target entity ID"}));
        props.insert("relation_type".into(), json!({"type": "string", "description": "(add_relation/get_relations) Relation type (e.g. 'knows', 'depends_on', 'part_of', 'related_to')"}));
        props.insert("relation_id".into(), json!({"type": "string", "description": "(delete_relation) Relation ID to delete"}));
        props.insert("query".into(), json!({"type": "string", "description": "(search_entities/query) Search query or Cypher-like pattern"}));
        props.insert("depth".into(), json!({"type": "integer", "description": "(subgraph/find_path) Max traversal depth. Default: 2"}));
        props.insert("max_results".into(), json!({"type": "integer", "description": "(search_entities/query) Max results. Default: 50"}));
        props.insert("format".into(), json!({"type": "string", "enum": ["json", "dot", "mermaid"], "description": "(export/subgraph) Output format. Default: json"}));
        props.insert("output_path".into(), json!({"type": "string", "description": "(export) Output file path"}));
        props.insert("graph_name".into(), json!({"type": "string", "description": "Graph database name. Default: 'default'. Allows multiple separate graphs."}));
        props.insert("direction".into(), json!({"type": "string", "enum": ["outgoing", "incoming", "both"], "description": "(get_relations/subgraph) Relation direction filter. Default: both"}));
        props.insert("bidirectional".into(), json!({"type": "boolean", "description": "(add_relation) If true, creates relation in both directions. Default: false"}));

        ToolSchema {
            name: "knowledge_graph",
            description: "Lightweight knowledge graph backed by SQLite. Store entities (nodes) with types, properties, and tags. Create typed relations (edges) between entities. Search entities with full-text search, find shortest paths, extract subgraphs, and export to JSON/DOT/Mermaid. Each graph is stored in a separate SQLite file in the workspace. Use graph_name to manage multiple graphs.",
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let valid = [
            "add_entity", "get_entity", "update_entity", "delete_entity", "search_entities",
            "add_relation", "get_relations", "delete_relation",
            "find_path", "subgraph", "stats", "export", "query", "merge_entity",
        ];
        if !valid.contains(&action) {
            return Err(Error::Tool(format!("Invalid action '{}'. Valid: {}", action, valid.join(", "))));
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap_or("");
        let graph_name = params.get("graph_name").and_then(|v| v.as_str()).unwrap_or("default");

        debug!(action = action, graph = graph_name, "knowledge_graph execute");

        // Open or create the graph database
        let db_dir = ctx.workspace.join("knowledge_graphs");
        std::fs::create_dir_all(&db_dir)
            .map_err(|e| Error::Tool(format!("Failed to create graph directory: {}", e)))?;
        let db_path = db_dir.join(format!("{}.db", graph_name));

        let db = rusqlite::Connection::open(&db_path)
            .map_err(|e| Error::Tool(format!("Failed to open graph database: {}", e)))?;

        init_schema(&db)?;

        match action {
            "add_entity" => action_add_entity(&db, &params),
            "get_entity" => action_get_entity(&db, &params),
            "update_entity" => action_update_entity(&db, &params),
            "delete_entity" => action_delete_entity(&db, &params),
            "search_entities" => action_search_entities(&db, &params),
            "merge_entity" => action_merge_entity(&db, &params),
            "add_relation" => action_add_relation(&db, &params),
            "get_relations" => action_get_relations(&db, &params),
            "delete_relation" => action_delete_relation(&db, &params),
            "find_path" => action_find_path(&db, &params),
            "subgraph" => action_subgraph(&db, &params),
            "stats" => action_stats(&db),
            "export" => action_export(&db, &params, &ctx),
            "query" => action_query(&db, &params),
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

fn init_schema(db: &rusqlite::Connection) -> Result<()> {
    db.execute_batch("
        CREATE TABLE IF NOT EXISTS entities (
            id TEXT PRIMARY KEY,
            entity_type TEXT NOT NULL DEFAULT '',
            name TEXT NOT NULL DEFAULT '',
            properties TEXT NOT NULL DEFAULT '{}',
            tags TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS relations (
            id TEXT PRIMARY KEY,
            source_id TEXT NOT NULL,
            target_id TEXT NOT NULL,
            relation_type TEXT NOT NULL DEFAULT '',
            properties TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            FOREIGN KEY (source_id) REFERENCES entities(id) ON DELETE CASCADE,
            FOREIGN KEY (target_id) REFERENCES entities(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_entities_type ON entities(entity_type);
        CREATE INDEX IF NOT EXISTS idx_entities_name ON entities(name);
        CREATE INDEX IF NOT EXISTS idx_relations_source ON relations(source_id);
        CREATE INDEX IF NOT EXISTS idx_relations_target ON relations(target_id);
        CREATE INDEX IF NOT EXISTS idx_relations_type ON relations(relation_type);

        CREATE VIRTUAL TABLE IF NOT EXISTS entities_fts USING fts5(
            id, name, entity_type, tags, properties,
            content=entities,
            content_rowid=rowid
        );

        -- Triggers to keep FTS in sync
        CREATE TRIGGER IF NOT EXISTS entities_ai AFTER INSERT ON entities BEGIN
            INSERT INTO entities_fts(rowid, id, name, entity_type, tags, properties)
            VALUES (new.rowid, new.id, new.name, new.entity_type, new.tags, new.properties);
        END;
        CREATE TRIGGER IF NOT EXISTS entities_ad AFTER DELETE ON entities BEGIN
            INSERT INTO entities_fts(entities_fts, rowid, id, name, entity_type, tags, properties)
            VALUES ('delete', old.rowid, old.id, old.name, old.entity_type, old.tags, old.properties);
        END;
        CREATE TRIGGER IF NOT EXISTS entities_au AFTER UPDATE ON entities BEGIN
            INSERT INTO entities_fts(entities_fts, rowid, id, name, entity_type, tags, properties)
            VALUES ('delete', old.rowid, old.id, old.name, old.entity_type, old.tags, old.properties);
            INSERT INTO entities_fts(rowid, id, name, entity_type, tags, properties)
            VALUES (new.rowid, new.id, new.name, new.entity_type, new.tags, new.properties);
        END;

        PRAGMA foreign_keys = ON;
    ").map_err(|e| Error::Tool(format!("Failed to initialize graph schema: {}", e)))?;
    Ok(())
}

// ─── Entity operations ──────────────────────────────────────────────────────

fn action_add_entity(db: &rusqlite::Connection, params: &Value) -> Result<Value> {
    let id = params.get("entity_id").and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let entity_type = params.get("entity_type").and_then(|v| v.as_str()).unwrap_or("");
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let default_props = json!({});
    let default_tags = json!([]);
    let properties = params.get("properties").unwrap_or(&default_props);
    let tags = params.get("tags").unwrap_or(&default_tags);

    let props_str = serde_json::to_string(properties).unwrap_or_else(|_| "{}".to_string());
    let tags_str = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());

    db.execute(
        "INSERT INTO entities (id, entity_type, name, properties, tags) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![id, entity_type, name, props_str, tags_str],
    ).map_err(|e| Error::Tool(format!("Failed to add entity: {}", e)))?;

    Ok(json!({
        "status": "created",
        "entity_id": id,
        "entity_type": entity_type,
        "name": name,
    }))
}

fn action_get_entity(db: &rusqlite::Connection, params: &Value) -> Result<Value> {
    let id = params.get("entity_id").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("entity_id is required for get_entity".into()))?;

    let mut stmt = db.prepare(
        "SELECT id, entity_type, name, properties, tags, created_at, updated_at FROM entities WHERE id = ?1"
    ).map_err(|e| Error::Tool(format!("Query error: {}", e)))?;

    let entity = stmt.query_row(rusqlite::params![id], |row| {
        let props_str: String = row.get(3)?;
        let tags_str: String = row.get(4)?;
        Ok(json!({
            "id": row.get::<_, String>(0)?,
            "entity_type": row.get::<_, String>(1)?,
            "name": row.get::<_, String>(2)?,
            "properties": serde_json::from_str::<Value>(&props_str).unwrap_or(json!({})),
            "tags": serde_json::from_str::<Value>(&tags_str).unwrap_or(json!([])),
            "created_at": row.get::<_, String>(5)?,
            "updated_at": row.get::<_, String>(6)?,
        }))
    }).map_err(|e| Error::Tool(format!("Entity not found: {}", e)))?;

    // Also get relations
    let relations = get_entity_relations(db, id, "both")?;

    Ok(json!({
        "entity": entity,
        "relations": relations,
    }))
}

fn action_update_entity(db: &rusqlite::Connection, params: &Value) -> Result<Value> {
    let id = params.get("entity_id").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("entity_id is required for update_entity".into()))?;

    // Build SET clause dynamically
    let mut sets = Vec::new();
    let mut values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
        sets.push("name = ?");
        values.push(Box::new(name.to_string()));
    }
    if let Some(entity_type) = params.get("entity_type").and_then(|v| v.as_str()) {
        sets.push("entity_type = ?");
        values.push(Box::new(entity_type.to_string()));
    }
    if let Some(properties) = params.get("properties") {
        sets.push("properties = ?");
        values.push(Box::new(serde_json::to_string(properties).unwrap_or_else(|_| "{}".to_string())));
    }
    if let Some(tags) = params.get("tags") {
        sets.push("tags = ?");
        values.push(Box::new(serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string())));
    }

    if sets.is_empty() {
        return Err(Error::Tool("No fields to update".into()));
    }

    sets.push("updated_at = datetime('now')");
    let sql = format!("UPDATE entities SET {} WHERE id = ?", sets.join(", "));
    values.push(Box::new(id.to_string()));

    let params_refs: Vec<&dyn rusqlite::types::ToSql> = values.iter().map(|v| v.as_ref()).collect();
    let affected = db.execute(&sql, params_refs.as_slice())
        .map_err(|e| Error::Tool(format!("Update failed: {}", e)))?;

    if affected == 0 {
        return Err(Error::Tool(format!("Entity '{}' not found", id)));
    }

    Ok(json!({"status": "updated", "entity_id": id, "fields_updated": sets.len() - 1}))
}

fn action_delete_entity(db: &rusqlite::Connection, params: &Value) -> Result<Value> {
    let id = params.get("entity_id").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("entity_id is required for delete_entity".into()))?;

    // Delete relations first
    let relations_deleted = db.execute(
        "DELETE FROM relations WHERE source_id = ?1 OR target_id = ?1",
        rusqlite::params![id],
    ).map_err(|e| Error::Tool(format!("Failed to delete relations: {}", e)))?;

    let affected = db.execute("DELETE FROM entities WHERE id = ?1", rusqlite::params![id])
        .map_err(|e| Error::Tool(format!("Delete failed: {}", e)))?;

    Ok(json!({
        "status": if affected > 0 { "deleted" } else { "not_found" },
        "entity_id": id,
        "relations_deleted": relations_deleted,
    }))
}

fn action_search_entities(db: &rusqlite::Connection, params: &Value) -> Result<Value> {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let entity_type = params.get("entity_type").and_then(|v| v.as_str());
    let max_results = params.get("max_results").and_then(|v| v.as_u64()).unwrap_or(50);

    let entities = if !query.is_empty() {
        // FTS search
        let fts_query = query.replace('"', "\"\"");
        let sql = if let Some(et) = entity_type {
            format!(
                "SELECT e.id, e.entity_type, e.name, e.properties, e.tags, e.created_at \
                 FROM entities_fts fts JOIN entities e ON fts.id = e.id \
                 WHERE entities_fts MATCH '\"{}\"' AND e.entity_type = '{}' \
                 LIMIT {}",
                fts_query, et, max_results
            )
        } else {
            format!(
                "SELECT e.id, e.entity_type, e.name, e.properties, e.tags, e.created_at \
                 FROM entities_fts fts JOIN entities e ON fts.id = e.id \
                 WHERE entities_fts MATCH '\"{}\"' \
                 LIMIT {}",
                fts_query, max_results
            )
        };
        query_entities(db, &sql)?
    } else if let Some(et) = entity_type {
        let sql = format!(
            "SELECT id, entity_type, name, properties, tags, created_at FROM entities WHERE entity_type = '{}' LIMIT {}",
            et, max_results
        );
        query_entities(db, &sql)?
    } else {
        let sql = format!(
            "SELECT id, entity_type, name, properties, tags, created_at FROM entities ORDER BY updated_at DESC LIMIT {}",
            max_results
        );
        query_entities(db, &sql)?
    };

    Ok(json!({"entities": entities, "count": entities.len(), "query": query}))
}

fn action_merge_entity(db: &rusqlite::Connection, params: &Value) -> Result<Value> {
    let id = params.get("entity_id").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("entity_id is required for merge_entity".into()))?;

    // Check if entity exists
    let exists: bool = db.query_row(
        "SELECT COUNT(*) FROM entities WHERE id = ?1",
        rusqlite::params![id],
        |row| row.get::<_, i64>(0),
    ).map(|c| c > 0)
    .unwrap_or(false);

    if exists {
        // Update existing
        action_update_entity(db, params)
    } else {
        // Create new
        action_add_entity(db, params)
    }
}

// ─── Relation operations ────────────────────────────────────────────────────

fn action_add_relation(db: &rusqlite::Connection, params: &Value) -> Result<Value> {
    let source_id = params.get("source_id").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("source_id is required for add_relation".into()))?;
    let target_id = params.get("target_id").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("target_id is required for add_relation".into()))?;
    let relation_type = params.get("relation_type").and_then(|v| v.as_str()).unwrap_or("related_to");
    let default_props = json!({});
    let properties = params.get("properties").unwrap_or(&default_props);
    let bidirectional = params.get("bidirectional").and_then(|v| v.as_bool()).unwrap_or(false);

    let props_str = serde_json::to_string(properties).unwrap_or_else(|_| "{}".to_string());

    // Verify both entities exist
    let source_exists: bool = db.query_row(
        "SELECT COUNT(*) FROM entities WHERE id = ?1", rusqlite::params![source_id],
        |row| row.get::<_, i64>(0),
    ).map(|c| c > 0).unwrap_or(false);
    if !source_exists {
        return Err(Error::Tool(format!("Source entity '{}' not found", source_id)));
    }

    let target_exists: bool = db.query_row(
        "SELECT COUNT(*) FROM entities WHERE id = ?1", rusqlite::params![target_id],
        |row| row.get::<_, i64>(0),
    ).map(|c| c > 0).unwrap_or(false);
    if !target_exists {
        return Err(Error::Tool(format!("Target entity '{}' not found", target_id)));
    }

    let id = uuid::Uuid::new_v4().to_string();
    db.execute(
        "INSERT INTO relations (id, source_id, target_id, relation_type, properties) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![id, source_id, target_id, relation_type, props_str],
    ).map_err(|e| Error::Tool(format!("Failed to add relation: {}", e)))?;

    let mut result = json!({
        "status": "created",
        "relation_id": id,
        "source_id": source_id,
        "target_id": target_id,
        "relation_type": relation_type,
    });

    if bidirectional {
        let reverse_id = uuid::Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO relations (id, source_id, target_id, relation_type, properties) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![reverse_id, target_id, source_id, relation_type, props_str],
        ).map_err(|e| Error::Tool(format!("Failed to add reverse relation: {}", e)))?;
        result["reverse_relation_id"] = json!(reverse_id);
        result["bidirectional"] = json!(true);
    }

    Ok(result)
}

fn action_get_relations(db: &rusqlite::Connection, params: &Value) -> Result<Value> {
    let entity_id = params.get("entity_id").or(params.get("source_id")).and_then(|v| v.as_str());
    let relation_type = params.get("relation_type").and_then(|v| v.as_str());
    let direction = params.get("direction").and_then(|v| v.as_str()).unwrap_or("both");

    let relations = if let Some(eid) = entity_id {
        get_entity_relations(db, eid, direction)?
    } else if let Some(rt) = relation_type {
        let sql = format!(
            "SELECT r.id, r.source_id, r.target_id, r.relation_type, r.properties, r.created_at, \
             s.name as source_name, t.name as target_name \
             FROM relations r \
             LEFT JOIN entities s ON r.source_id = s.id \
             LEFT JOIN entities t ON r.target_id = t.id \
             WHERE r.relation_type = '{}' LIMIT 100",
            rt
        );
        query_relations_full(db, &sql)?
    } else {
        let sql = "SELECT r.id, r.source_id, r.target_id, r.relation_type, r.properties, r.created_at, \
             s.name as source_name, t.name as target_name \
             FROM relations r \
             LEFT JOIN entities s ON r.source_id = s.id \
             LEFT JOIN entities t ON r.target_id = t.id \
             LIMIT 100";
        query_relations_full(db, sql)?
    };

    // Filter by relation_type if both entity_id and relation_type are specified
    let filtered = if let (Some(_), Some(rt)) = (entity_id, relation_type) {
        relations.into_iter().filter(|r| r.get("relation_type").and_then(|v| v.as_str()) == Some(rt)).collect()
    } else {
        relations
    };

    Ok(json!({"relations": filtered, "count": filtered.len()}))
}

fn action_delete_relation(db: &rusqlite::Connection, params: &Value) -> Result<Value> {
    let id = params.get("relation_id").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("relation_id is required for delete_relation".into()))?;

    let affected = db.execute("DELETE FROM relations WHERE id = ?1", rusqlite::params![id])
        .map_err(|e| Error::Tool(format!("Delete failed: {}", e)))?;

    Ok(json!({
        "status": if affected > 0 { "deleted" } else { "not_found" },
        "relation_id": id,
    }))
}

// ─── Graph traversal ────────────────────────────────────────────────────────

fn action_find_path(db: &rusqlite::Connection, params: &Value) -> Result<Value> {
    let source_id = params.get("source_id").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("source_id is required for find_path".into()))?;
    let target_id = params.get("target_id").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("target_id is required for find_path".into()))?;
    let max_depth = params.get("depth").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

    // BFS shortest path
    let mut visited = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    let mut parent: std::collections::HashMap<String, (String, String, String)> = std::collections::HashMap::new(); // node -> (parent, relation_id, relation_type)

    visited.insert(source_id.to_string());
    queue.push_back((source_id.to_string(), 0usize));

    let mut found = false;

    while let Some((current, depth)) = queue.pop_front() {
        if depth >= max_depth { continue; }

        // Get neighbors (both directions)
        let mut stmt = db.prepare(
            "SELECT id, source_id, target_id, relation_type FROM relations WHERE source_id = ?1 OR target_id = ?1"
        ).map_err(|e| Error::Tool(format!("Query error: {}", e)))?;

        let neighbors: Vec<(String, String, String)> = stmt.query_map(
            rusqlite::params![current],
            |row| {
                let rel_id: String = row.get(0)?;
                let src: String = row.get(1)?;
                let tgt: String = row.get(2)?;
                let rel_type: String = row.get(3)?;
                let neighbor = if src == current { tgt } else { src };
                Ok((neighbor, rel_id, rel_type))
            },
        ).map_err(|e| Error::Tool(format!("Query error: {}", e)))?
        .filter_map(|r| r.ok())
        .collect();

        for (neighbor, rel_id, rel_type) in neighbors {
            if visited.contains(&neighbor) { continue; }
            visited.insert(neighbor.clone());
            parent.insert(neighbor.clone(), (current.clone(), rel_id, rel_type));

            if neighbor == target_id {
                found = true;
                break;
            }
            queue.push_back((neighbor, depth + 1));
        }

        if found { break; }
    }

    if !found {
        return Ok(json!({
            "found": false,
            "source_id": source_id,
            "target_id": target_id,
            "max_depth": max_depth,
        }));
    }

    // Reconstruct path
    let mut path = Vec::new();
    let mut current = target_id.to_string();
    while current != source_id {
        if let Some((prev, rel_id, rel_type)) = parent.get(&current) {
            path.push(json!({
                "entity_id": current,
                "relation_id": rel_id,
                "relation_type": rel_type,
                "from": prev,
            }));
            current = prev.clone();
        } else {
            break;
        }
    }
    path.reverse();

    // Enrich with entity names
    let mut enriched_path = vec![get_entity_brief(db, source_id)];
    for step in &path {
        let eid = step["entity_id"].as_str().unwrap_or("");
        let mut node = get_entity_brief(db, eid);
        node["via_relation"] = step["relation_type"].clone();
        enriched_path.push(node);
    }

    Ok(json!({
        "found": true,
        "path": enriched_path,
        "length": path.len(),
        "source_id": source_id,
        "target_id": target_id,
    }))
}

fn action_subgraph(db: &rusqlite::Connection, params: &Value) -> Result<Value> {
    let entity_id = params.get("entity_id").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("entity_id is required for subgraph".into()))?;
    let depth = params.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
    let format = params.get("format").and_then(|v| v.as_str()).unwrap_or("json");

    // BFS to collect neighborhood
    let mut visited = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    let mut entities = Vec::new();
    let mut relations = Vec::new();

    visited.insert(entity_id.to_string());
    queue.push_back((entity_id.to_string(), 0usize));

    while let Some((current, d)) = queue.pop_front() {
        entities.push(get_entity_brief(db, &current));

        if d >= depth { continue; }

        let mut stmt = db.prepare(
            "SELECT id, source_id, target_id, relation_type, properties FROM relations WHERE source_id = ?1 OR target_id = ?1"
        ).map_err(|e| Error::Tool(format!("Query error: {}", e)))?;

        let rels: Vec<(String, String, String, String, String)> = stmt.query_map(
            rusqlite::params![current],
            |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            )),
        ).map_err(|e| Error::Tool(format!("Query error: {}", e)))?
        .filter_map(|r| r.ok())
        .collect();

        for (rel_id, src, tgt, rel_type, _props) in rels {
            let neighbor = if src == current { &tgt } else { &src };
            relations.push(json!({
                "id": rel_id,
                "source_id": src,
                "target_id": tgt,
                "relation_type": rel_type,
            }));
            if !visited.contains(neighbor) {
                visited.insert(neighbor.clone());
                queue.push_back((neighbor.clone(), d + 1));
            }
        }
    }

    // Deduplicate relations
    let mut seen_rels = std::collections::HashSet::new();
    relations.retain(|r| {
        let id = r["id"].as_str().unwrap_or("");
        seen_rels.insert(id.to_string())
    });

    match format {
        "dot" => {
            let dot = export_dot(&entities, &relations);
            Ok(json!({"format": "dot", "content": dot, "entities": entities.len(), "relations": relations.len()}))
        }
        "mermaid" => {
            let mermaid = export_mermaid(&entities, &relations);
            Ok(json!({"format": "mermaid", "content": mermaid, "entities": entities.len(), "relations": relations.len()}))
        }
        _ => {
            Ok(json!({
                "center": entity_id,
                "depth": depth,
                "entities": entities,
                "relations": relations,
                "entity_count": entities.len(),
                "relation_count": relations.len(),
            }))
        }
    }
}

// ─── Stats & Export ─────────────────────────────────────────────────────────

fn action_stats(db: &rusqlite::Connection) -> Result<Value> {
    let entity_count: i64 = db.query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
        .unwrap_or(0);
    let relation_count: i64 = db.query_row("SELECT COUNT(*) FROM relations", [], |row| row.get(0))
        .unwrap_or(0);

    // Entity types distribution
    let mut stmt = db.prepare("SELECT entity_type, COUNT(*) FROM entities GROUP BY entity_type ORDER BY COUNT(*) DESC")
        .map_err(|e| Error::Tool(format!("Query error: {}", e)))?;
    let types: Vec<Value> = stmt.query_map([], |row| {
        Ok(json!({"type": row.get::<_, String>(0)?, "count": row.get::<_, i64>(1)?}))
    }).map_err(|e| Error::Tool(format!("Query error: {}", e)))?
    .filter_map(|r| r.ok())
    .collect();

    // Relation types distribution
    let mut stmt = db.prepare("SELECT relation_type, COUNT(*) FROM relations GROUP BY relation_type ORDER BY COUNT(*) DESC")
        .map_err(|e| Error::Tool(format!("Query error: {}", e)))?;
    let rel_types: Vec<Value> = stmt.query_map([], |row| {
        Ok(json!({"type": row.get::<_, String>(0)?, "count": row.get::<_, i64>(1)?}))
    }).map_err(|e| Error::Tool(format!("Query error: {}", e)))?
    .filter_map(|r| r.ok())
    .collect();

    // Most connected entities
    let mut stmt = db.prepare(
        "SELECT e.id, e.name, e.entity_type, \
         (SELECT COUNT(*) FROM relations WHERE source_id = e.id OR target_id = e.id) as degree \
         FROM entities e ORDER BY degree DESC LIMIT 10"
    ).map_err(|e| Error::Tool(format!("Query error: {}", e)))?;
    let top_entities: Vec<Value> = stmt.query_map([], |row| {
        Ok(json!({
            "id": row.get::<_, String>(0)?,
            "name": row.get::<_, String>(1)?,
            "type": row.get::<_, String>(2)?,
            "degree": row.get::<_, i64>(3)?,
        }))
    }).map_err(|e| Error::Tool(format!("Query error: {}", e)))?
    .filter_map(|r| r.ok())
    .collect();

    Ok(json!({
        "entity_count": entity_count,
        "relation_count": relation_count,
        "entity_types": types,
        "relation_types": rel_types,
        "most_connected": top_entities,
    }))
}

fn action_export(db: &rusqlite::Connection, params: &Value, ctx: &ToolContext) -> Result<Value> {
    let format = params.get("format").and_then(|v| v.as_str()).unwrap_or("json");
    let output_path = params.get("output_path").and_then(|v| v.as_str());

    // Get all entities and relations
    let entities = query_entities(db, "SELECT id, entity_type, name, properties, tags, created_at FROM entities")?;
    let relations = query_relations_full(db,
        "SELECT r.id, r.source_id, r.target_id, r.relation_type, r.properties, r.created_at, \
         s.name as source_name, t.name as target_name \
         FROM relations r \
         LEFT JOIN entities s ON r.source_id = s.id \
         LEFT JOIN entities t ON r.target_id = t.id"
    )?;

    let content = match format {
        "dot" => export_dot(&entities, &relations),
        "mermaid" => export_mermaid(&entities, &relations),
        _ => serde_json::to_string_pretty(&json!({"entities": entities, "relations": relations}))
            .unwrap_or_else(|_| "{}".to_string()),
    };

    if let Some(path) = output_path {
        let resolved = if path.starts_with('/') || path.starts_with("~/") {
            path.to_string()
        } else {
            ctx.workspace.join(path).to_string_lossy().to_string()
        };
        std::fs::write(&resolved, &content)
            .map_err(|e| Error::Tool(format!("Failed to write export: {}", e)))?;
        Ok(json!({"status": "exported", "path": resolved, "format": format, "entities": entities.len(), "relations": relations.len()}))
    } else {
        Ok(json!({"format": format, "content": content, "entities": entities.len(), "relations": relations.len()}))
    }
}

fn action_query(db: &rusqlite::Connection, params: &Value) -> Result<Value> {
    let query = params.get("query").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("query is required".into()))?;
    let max_results = params.get("max_results").and_then(|v| v.as_u64()).unwrap_or(50);

    // Simple pattern matching: "entity_type:person" or "tag:important" or free text
    if query.starts_with("type:") || query.starts_with("entity_type:") {
        let et = query.split_once(':').map(|x| x.1).unwrap_or("");
        let sql = format!(
            "SELECT id, entity_type, name, properties, tags, created_at FROM entities WHERE entity_type = '{}' LIMIT {}",
            et, max_results
        );
        let entities = query_entities(db, &sql)?;
        Ok(json!({"entities": entities, "count": entities.len()}))
    } else if query.starts_with("tag:") {
        let tag = query.split_once(':').map(|x| x.1).unwrap_or("");
        let sql = format!(
            "SELECT id, entity_type, name, properties, tags, created_at FROM entities WHERE tags LIKE '%\"{}%' LIMIT {}",
            tag, max_results
        );
        let entities = query_entities(db, &sql)?;
        Ok(json!({"entities": entities, "count": entities.len()}))
    } else if query.starts_with("relation:") {
        let rt = query.split_once(':').map(|x| x.1).unwrap_or("");
        let sql = format!(
            "SELECT r.id, r.source_id, r.target_id, r.relation_type, r.properties, r.created_at, \
             s.name as source_name, t.name as target_name \
             FROM relations r \
             LEFT JOIN entities s ON r.source_id = s.id \
             LEFT JOIN entities t ON r.target_id = t.id \
             WHERE r.relation_type = '{}' LIMIT {}",
            rt, max_results
        );
        let relations = query_relations_full(db, &sql)?;
        Ok(json!({"relations": relations, "count": relations.len()}))
    } else {
        // Full-text search
        action_search_entities(db, params)
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn query_entities(db: &rusqlite::Connection, sql: &str) -> Result<Vec<Value>> {
    let mut stmt = db.prepare(sql)
        .map_err(|e| Error::Tool(format!("Query error: {}", e)))?;
    let entities: Vec<Value> = stmt.query_map([], |row| {
        let props_str: String = row.get(3)?;
        let tags_str: String = row.get(4)?;
        Ok(json!({
            "id": row.get::<_, String>(0)?,
            "entity_type": row.get::<_, String>(1)?,
            "name": row.get::<_, String>(2)?,
            "properties": serde_json::from_str::<Value>(&props_str).unwrap_or(json!({})),
            "tags": serde_json::from_str::<Value>(&tags_str).unwrap_or(json!([])),
            "created_at": row.get::<_, String>(5)?,
        }))
    }).map_err(|e| Error::Tool(format!("Query error: {}", e)))?
    .filter_map(|r| r.ok())
    .collect();
    Ok(entities)
}

fn query_relations_full(db: &rusqlite::Connection, sql: &str) -> Result<Vec<Value>> {
    let mut stmt = db.prepare(sql)
        .map_err(|e| Error::Tool(format!("Query error: {}", e)))?;
    let relations: Vec<Value> = stmt.query_map([], |row| {
        let props_str: String = row.get(4)?;
        Ok(json!({
            "id": row.get::<_, String>(0)?,
            "source_id": row.get::<_, String>(1)?,
            "target_id": row.get::<_, String>(2)?,
            "relation_type": row.get::<_, String>(3)?,
            "properties": serde_json::from_str::<Value>(&props_str).unwrap_or(json!({})),
            "created_at": row.get::<_, String>(5)?,
            "source_name": row.get::<_, String>(6).unwrap_or_default(),
            "target_name": row.get::<_, String>(7).unwrap_or_default(),
        }))
    }).map_err(|e| Error::Tool(format!("Query error: {}", e)))?
    .filter_map(|r| r.ok())
    .collect();
    Ok(relations)
}

fn get_entity_relations(db: &rusqlite::Connection, entity_id: &str, direction: &str) -> Result<Vec<Value>> {
    let sql = match direction {
        "outgoing" => format!(
            "SELECT r.id, r.source_id, r.target_id, r.relation_type, r.properties, r.created_at, \
             s.name, t.name FROM relations r \
             LEFT JOIN entities s ON r.source_id = s.id LEFT JOIN entities t ON r.target_id = t.id \
             WHERE r.source_id = '{}'", entity_id
        ),
        "incoming" => format!(
            "SELECT r.id, r.source_id, r.target_id, r.relation_type, r.properties, r.created_at, \
             s.name, t.name FROM relations r \
             LEFT JOIN entities s ON r.source_id = s.id LEFT JOIN entities t ON r.target_id = t.id \
             WHERE r.target_id = '{}'", entity_id
        ),
        _ => format!(
            "SELECT r.id, r.source_id, r.target_id, r.relation_type, r.properties, r.created_at, \
             s.name, t.name FROM relations r \
             LEFT JOIN entities s ON r.source_id = s.id LEFT JOIN entities t ON r.target_id = t.id \
             WHERE r.source_id = '{}' OR r.target_id = '{}'", entity_id, entity_id
        ),
    };
    query_relations_full(db, &sql)
}

fn get_entity_brief(db: &rusqlite::Connection, id: &str) -> Value {
    db.query_row(
        "SELECT id, entity_type, name FROM entities WHERE id = ?1",
        rusqlite::params![id],
        |row| Ok(json!({
            "id": row.get::<_, String>(0)?,
            "entity_type": row.get::<_, String>(1)?,
            "name": row.get::<_, String>(2)?,
        })),
    ).unwrap_or(json!({"id": id, "name": "unknown"}))
}

fn export_dot(entities: &[Value], relations: &[Value]) -> String {
    let mut dot = String::from("digraph KnowledgeGraph {\n  rankdir=LR;\n  node [shape=box, style=rounded];\n\n");
    for e in entities {
        let id = e["id"].as_str().unwrap_or("");
        let name = e["name"].as_str().unwrap_or(id);
        let etype = e["entity_type"].as_str().unwrap_or("");
        let label = if etype.is_empty() { name.to_string() } else { format!("{}\\n[{}]", name, etype) };
        dot.push_str(&format!("  \"{}\" [label=\"{}\"];\n", id, label.replace('"', "\\\"")));
    }
    dot.push('\n');
    for r in relations {
        let src = r["source_id"].as_str().unwrap_or("");
        let tgt = r["target_id"].as_str().unwrap_or("");
        let rtype = r["relation_type"].as_str().unwrap_or("");
        dot.push_str(&format!("  \"{}\" -> \"{}\" [label=\"{}\"];\n", src, tgt, rtype));
    }
    dot.push_str("}\n");
    dot
}

fn export_mermaid(entities: &[Value], relations: &[Value]) -> String {
    let mut md = String::from("graph LR\n");
    for e in entities {
        let id = e["id"].as_str().unwrap_or("");
        let name = e["name"].as_str().unwrap_or(id);
        // Sanitize for mermaid (replace special chars)
        let safe_id = id.replace(['-', ' '], "_");
        md.push_str(&format!("  {}[\"{}\"]\n", safe_id, name));
    }
    for r in relations {
        let src = r["source_id"].as_str().unwrap_or("").replace(['-', ' '], "_");
        let tgt = r["target_id"].as_str().unwrap_or("").replace(['-', ' '], "_");
        let rtype = r["relation_type"].as_str().unwrap_or("");
        if rtype.is_empty() {
            md.push_str(&format!("  {} --> {}\n", src, tgt));
        } else {
            md.push_str(&format!("  {} -->|\"{}\"| {}\n", src, tgt, rtype));
        }
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_tool() -> KnowledgeGraphTool { KnowledgeGraphTool }

    #[test]
    fn test_schema() {
        let tool = make_tool();
        let schema = tool.schema();
        assert_eq!(schema.name, "knowledge_graph");
        assert!(schema.parameters["properties"]["action"].is_object());
    }

    #[test]
    fn test_validate_valid() {
        let tool = make_tool();
        assert!(tool.validate(&json!({"action": "add_entity"})).is_ok());
        assert!(tool.validate(&json!({"action": "find_path"})).is_ok());
        assert!(tool.validate(&json!({"action": "export"})).is_ok());
    }

    #[test]
    fn test_validate_invalid() {
        let tool = make_tool();
        assert!(tool.validate(&json!({"action": "destroy"})).is_err());
    }

    #[test]
    fn test_in_memory_graph_operations() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        init_schema(&db).unwrap();

        // Add entities
        let r1 = action_add_entity(&db, &json!({
            "entity_id": "alice", "entity_type": "person", "name": "Alice",
            "properties": {"age": 30}, "tags": ["engineer"]
        })).unwrap();
        assert_eq!(r1["status"], "created");

        let r2 = action_add_entity(&db, &json!({
            "entity_id": "bob", "entity_type": "person", "name": "Bob"
        })).unwrap();
        assert_eq!(r2["status"], "created");

        let r3 = action_add_entity(&db, &json!({
            "entity_id": "rust", "entity_type": "skill", "name": "Rust Programming"
        })).unwrap();
        assert_eq!(r3["status"], "created");

        // Add relations
        let rel1 = action_add_relation(&db, &json!({
            "source_id": "alice", "target_id": "bob", "relation_type": "knows"
        })).unwrap();
        assert_eq!(rel1["status"], "created");

        let rel2 = action_add_relation(&db, &json!({
            "source_id": "alice", "target_id": "rust", "relation_type": "has_skill"
        })).unwrap();
        assert_eq!(rel2["status"], "created");

        // Get entity with relations
        let entity = action_get_entity(&db, &json!({"entity_id": "alice"})).unwrap();
        assert_eq!(entity["entity"]["name"], "Alice");
        assert!(entity["relations"].as_array().unwrap().len() >= 2);

        // Search
        let search = action_search_entities(&db, &json!({"query": "Alice"})).unwrap();
        assert!(search["count"].as_u64().unwrap() >= 1);

        // Find path
        let path = action_find_path(&db, &json!({"source_id": "bob", "target_id": "rust"})).unwrap();
        assert_eq!(path["found"], true);
        assert!(path["length"].as_u64().unwrap() >= 1);

        // Stats
        let stats = action_stats(&db).unwrap();
        assert_eq!(stats["entity_count"], 3);
        assert_eq!(stats["relation_count"], 2);

        // Subgraph
        let sg = action_subgraph(&db, &json!({"entity_id": "alice", "depth": 1})).unwrap();
        assert!(sg["entity_count"].as_u64().unwrap() >= 2);

        // Export mermaid
        let mermaid = action_subgraph(&db, &json!({"entity_id": "alice", "depth": 2, "format": "mermaid"})).unwrap();
        assert!(mermaid["content"].as_str().unwrap().contains("graph LR"));

        // Update entity
        let upd = action_update_entity(&db, &json!({"entity_id": "alice", "name": "Alice Smith"})).unwrap();
        assert_eq!(upd["status"], "updated");

        // Delete relation
        let rel_id = rel1["relation_id"].as_str().unwrap();
        let del_rel = action_delete_relation(&db, &json!({"relation_id": rel_id})).unwrap();
        assert_eq!(del_rel["status"], "deleted");

        // Delete entity
        let del = action_delete_entity(&db, &json!({"entity_id": "bob"})).unwrap();
        assert_eq!(del["status"], "deleted");

        // Verify stats after deletion
        let stats2 = action_stats(&db).unwrap();
        assert_eq!(stats2["entity_count"], 2);
    }

    #[test]
    fn test_merge_entity() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        init_schema(&db).unwrap();

        // First merge creates
        let r1 = action_merge_entity(&db, &json!({"entity_id": "test1", "name": "Test", "entity_type": "thing"})).unwrap();
        assert_eq!(r1["status"], "created");

        // Second merge updates
        let r2 = action_merge_entity(&db, &json!({"entity_id": "test1", "name": "Test Updated"})).unwrap();
        assert_eq!(r2["status"], "updated");
    }

    #[test]
    fn test_export_dot() {
        let entities = vec![json!({"id": "a", "name": "A", "entity_type": "node"})];
        let relations = vec![json!({"source_id": "a", "target_id": "b", "relation_type": "links"})];
        let dot = export_dot(&entities, &relations);
        assert!(dot.contains("digraph"));
        assert!(dot.contains("\"a\""));
    }

    #[test]
    fn test_validate_all_actions() {
        let tool = make_tool();
        for action in &["add_entity", "get_entity", "update_entity", "delete_entity", "search_entities",
                        "add_relation", "get_relations", "delete_relation", "find_path", "subgraph",
                        "stats", "export", "query", "merge_entity"] {
            assert!(tool.validate(&json!({"action": action})).is_ok());
        }
    }
}
