use blockcell_core::Paths;
use serde_json::Value;

/// Show knowledge graph statistics.
pub async fn stats(graph_name: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::default();
    let kg_dir = paths.workspace().join("knowledge_graphs");

    if !kg_dir.exists() {
        println!("(No knowledge graphs. Use the knowledge_graph tool via agent chat to create one.)");
        return Ok(());
    }

    let name = graph_name.as_deref().unwrap_or("default");
    let db_path = kg_dir.join(format!("{}.db", name));

    if !db_path.exists() {
        println!("Knowledge graph '{}' not found.", name);
        println!("Use `blockcell knowledge list-graphs` to see available graphs.");
        return Ok(());
    }

    let conn = rusqlite::Connection::open(&db_path)?;

    let entity_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM entities", [], |row| row.get(0)
    ).unwrap_or(0);

    let relation_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM relations", [], |row| row.get(0)
    ).unwrap_or(0);

    println!();
    println!("📊 Knowledge graph: {}", name);
    println!("  Entities: {}", entity_count);
    println!("  Relations: {}", relation_count);

    // Type distribution
    let mut stmt = conn.prepare(
        "SELECT entity_type, COUNT(*) FROM entities GROUP BY entity_type ORDER BY COUNT(*) DESC LIMIT 10"
    )?;
    let types: Vec<(String, i64)> = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?.filter_map(|r| r.ok()).collect();

    if !types.is_empty() {
        println!();
        println!("  Entity type distribution:");
        for (t, count) in &types {
            println!("    {:<20} {}", t, count);
        }
    }

    println!();
    Ok(())
}

/// Search entities in a knowledge graph.
pub async fn search(query: &str, graph_name: Option<String>, limit: usize) -> anyhow::Result<()> {
    let paths = Paths::default();
    let kg_dir = paths.workspace().join("knowledge_graphs");
    let name = graph_name.as_deref().unwrap_or("default");
    let db_path = kg_dir.join(format!("{}.db", name));

    if !db_path.exists() {
        println!("Knowledge graph '{}' not found.", name);
        return Ok(());
    }

    let conn = rusqlite::Connection::open(&db_path)?;

    // Use FTS5 search with parameter binding to prevent SQL injection
    let fts_sql = "SELECT e.id, e.name, e.entity_type, e.description \
         FROM entities_fts f JOIN entities e ON f.rowid = e.rowid \
         WHERE entities_fts MATCH ?1 LIMIT ?2";

    let like_pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
    let like_sql = "SELECT id, name, entity_type, description FROM entities \
         WHERE name LIKE ?1 ESCAPE '\\' OR description LIKE ?1 ESCAPE '\\' LIMIT ?2";

    let limit_i64 = limit as i64;
    let results: Vec<(String, String, String, String)> =
        if let Ok(mut stmt) = conn.prepare(fts_sql) {
            stmt.query_map(rusqlite::params![query, limit_i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2).unwrap_or_default(),
                    row.get::<_, String>(3).unwrap_or_default(),
                ))
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        } else {
            let mut stmt = conn.prepare(like_sql)?;
            stmt.query_map(rusqlite::params![like_pattern, limit_i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2).unwrap_or_default(),
                    row.get::<_, String>(3).unwrap_or_default(),
                ))
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
        };

    if results.is_empty() {
        println!("No entities matching '{}' found.", query);
        return Ok(());
    }

    println!();
    println!("🔍 Search results: '{}' ({} found)", query, results.len());
    println!();

    for (id, name, etype, desc) in &results {
        let short_desc: String = desc.chars().take(60).collect();
        println!("  📌 {} [{}] — {}", name, etype, id);
        if !short_desc.is_empty() {
            println!("     {}", short_desc);
        }
    }
    println!();

    Ok(())
}

/// Export a knowledge graph.
pub async fn export(graph_name: Option<String>, format: &str, output: Option<String>) -> anyhow::Result<()> {
    let paths = Paths::default();
    let kg_dir = paths.workspace().join("knowledge_graphs");
    let name = graph_name.as_deref().unwrap_or("default");
    let db_path = kg_dir.join(format!("{}.db", name));

    if !db_path.exists() {
        anyhow::bail!("Knowledge graph '{}' not found.", name);
    }

    let conn = rusqlite::Connection::open(&db_path)?;

    // Load entities
    let mut stmt = conn.prepare("SELECT id, name, entity_type, description, tags, properties FROM entities")?;
    let entities: Vec<Value> = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, String>(0)?,
            "name": row.get::<_, String>(1)?,
            "type": row.get::<_, String>(2).unwrap_or_default(),
            "description": row.get::<_, String>(3).unwrap_or_default(),
            "tags": row.get::<_, String>(4).unwrap_or_default(),
            "properties": row.get::<_, String>(5).unwrap_or_default(),
        }))
    })?.filter_map(|r| r.ok()).collect();

    // Load relations
    let mut stmt = conn.prepare("SELECT source_id, target_id, relation_type, properties FROM relations")?;
    let relations: Vec<Value> = stmt.query_map([], |row| {
        Ok(serde_json::json!({
            "source": row.get::<_, String>(0)?,
            "target": row.get::<_, String>(1)?,
            "type": row.get::<_, String>(2)?,
            "properties": row.get::<_, String>(3).unwrap_or_default(),
        }))
    })?.filter_map(|r| r.ok()).collect();

    let content = match format {
        "json" => {
            serde_json::to_string_pretty(&serde_json::json!({
                "graph": name,
                "entities": entities,
                "relations": relations,
            }))?
        }
        "dot" => export_dot(&entities, &relations),
        "mermaid" => export_mermaid(&entities, &relations),
        _ => anyhow::bail!("Unsupported format: {}. Options: json, dot, mermaid", format),
    };

    match output {
        Some(path) => {
            std::fs::write(&path, &content)?;
            println!("✓ Exported to: {}", path);
        }
        None => {
            println!("{}", content);
        }
    }

    Ok(())
}

/// List all knowledge graphs.
pub async fn list_graphs() -> anyhow::Result<()> {
    let paths = Paths::default();
    let kg_dir = paths.workspace().join("knowledge_graphs");

    if !kg_dir.exists() {
        println!("(No knowledge graphs)");
        return Ok(());
    }

    let mut graphs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&kg_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "db") {
                let name = path.file_stem().unwrap().to_string_lossy().to_string();
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                graphs.push((name, size));
            }
        }
    }

    if graphs.is_empty() {
        println!("(No knowledge graphs)");
        return Ok(());
    }

    graphs.sort_by(|a, b| a.0.cmp(&b.0));

    println!();
    println!("📚 Knowledge graphs ({} total)", graphs.len());
    println!();

    for (name, size) in &graphs {
        println!("  📊 {:<30} ({} KB)", name, size / 1024);
    }
    println!();

    Ok(())
}

fn export_dot(entities: &[Value], relations: &[Value]) -> String {
    let mut dot = String::from("digraph KnowledgeGraph {\n  rankdir=LR;\n  node [shape=box, style=rounded];\n\n");
    for e in entities {
        let id = e["id"].as_str().unwrap_or("");
        let name = e["name"].as_str().unwrap_or("");
        let safe_name = name.replace('"', "\\\"");
        dot.push_str(&format!("  \"{}\" [label=\"{}\"];\n", id, safe_name));
    }
    dot.push('\n');
    for r in relations {
        let src = r["source"].as_str().unwrap_or("");
        let tgt = r["target"].as_str().unwrap_or("");
        let rtype = r["type"].as_str().unwrap_or("");
        let safe_type = rtype.replace('"', "\\\"");
        dot.push_str(&format!("  \"{}\" -> \"{}\" [label=\"{}\"];\n", src, tgt, safe_type));
    }
    dot.push_str("}\n");
    dot
}

fn export_mermaid(entities: &[Value], relations: &[Value]) -> String {
    let mut md = String::from("graph LR\n");
    for e in entities {
        let id = e["id"].as_str().unwrap_or("");
        let name = e["name"].as_str().unwrap_or("");
        let safe_id = id.replace('-', "_");
        md.push_str(&format!("  {}[\"{}\"]\n", safe_id, name));
    }
    for r in relations {
        let src = r["source"].as_str().unwrap_or("").replace('-', "_");
        let tgt = r["target"].as_str().unwrap_or("").replace('-', "_");
        let rtype = r["type"].as_str().unwrap_or("");
        md.push_str(&format!("  {} -->|\"{}\"| {}\n", src, rtype, tgt));
    }
    md
}
