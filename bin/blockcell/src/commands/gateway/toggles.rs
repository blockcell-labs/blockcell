use super::*;
// ---------------------------------------------------------------------------
// Toggles: enable/disable skills and tools
// ---------------------------------------------------------------------------

static TOGGLES_WRITE_LOCK: once_cell::sync::Lazy<tokio::sync::Mutex<()>> =
    once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(()));

/// GET /v1/toggles — get all toggle states
pub(super) async fn handle_toggles_get(State(state): State<GatewayState>) -> impl IntoResponse {
    let path = state.paths.toggles_file();
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Json(serde_json::json!({ "skills": {}, "tools": {} }));
    }
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(val) => Json(val),
            Err(_) => Json(serde_json::json!({ "skills": {}, "tools": {} })),
        },
        Err(_) => Json(serde_json::json!({ "skills": {}, "tools": {} })),
    }
}

#[derive(Deserialize)]
pub(super) struct ToggleUpdateRequest {
    category: String, // "skills" or "tools"
    name: String,
    enabled: bool,
}

fn empty_toggle_store() -> serde_json::Value {
    serde_json::json!({ "skills": {}, "tools": {} })
}

fn apply_toggle_update(store: &mut serde_json::Value, category: &str, name: &str, enabled: bool) {
    if store.get(category).is_none() || !store[category].is_object() {
        store[category] = serde_json::json!({});
    }

    if enabled {
        if let Some(obj) = store[category].as_object_mut() {
            obj.remove(name);
        }
    } else {
        store[category][name] = serde_json::json!(false);
    }
}

async fn atomic_write_string(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("toggles.json");
    let tmp_path = path.with_file_name(format!(
        ".{}.{}.tmp",
        file_name,
        uuid::Uuid::new_v4().simple()
    ));

    let path = path.to_path_buf();
    let tmp_path_for_cleanup = tmp_path.clone();
    let content = content.to_string();
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::io::Write;

        let mut file = std::fs::File::create(&tmp_path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);

        std::fs::rename(&tmp_path, &path)?;
        if let Some(parent) = path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
        Ok(())
    })
    .await;

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => {
            let _ = tokio::fs::remove_file(&tmp_path_for_cleanup).await;
            Err(err)
        }
        Err(err) => {
            let _ = tokio::fs::remove_file(&tmp_path_for_cleanup).await;
            Err(std::io::Error::other(err))
        }
    }
}

/// PUT /v1/toggles — update a single toggle
pub(super) async fn handle_toggles_update(
    State(state): State<GatewayState>,
    Json(req): Json<ToggleUpdateRequest>,
) -> impl IntoResponse {
    if req.category != "skills" && req.category != "tools" {
        return Json(serde_json::json!({ "error": "category must be 'skills' or 'tools'" }));
    }

    let path = state.paths.toggles_file();
    let _guard = TOGGLES_WRITE_LOCK.lock().await;
    let lock_path = path.with_extension("lock");
    let _file_guard = match tokio::task::spawn_blocking(move || {
        blockcell_core::file_store::ExclusiveFileLock::acquire(&lock_path)
    })
    .await
    {
        Ok(Ok(guard)) => guard,
        Ok(Err(error)) => return Json(serde_json::json!({"error": error.to_string()})),
        Err(error) => return Json(serde_json::json!({"error": error.to_string()})),
    };
    let mut store: serde_json::Value = if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        tokio::fs::read_to_string(&path)
            .await
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_else(empty_toggle_store)
    } else {
        empty_toggle_store()
    };

    apply_toggle_update(&mut store, &req.category, &req.name, req.enabled);
    let content = serde_json::to_string_pretty(&store).unwrap_or_else(|_| "{}".to_string());

    match atomic_write_string(&path, &content).await {
        Ok(_) => Json(serde_json::json!({
            "status": "ok",
            "category": req.category,
            "name": req.name,
            "enabled": req.enabled,
        })),
        Err(e) => Json(serde_json::json!({ "error": format!("{}", e) })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_update_preserves_existing_values() {
        let mut store = serde_json::json!({
            "skills": {
                "alpha": false
            },
            "tools": {
                "shell": false
            }
        });

        apply_toggle_update(&mut store, "skills", "beta", false);

        assert_eq!(store["skills"]["alpha"], serde_json::json!(false));
        assert_eq!(store["skills"]["beta"], serde_json::json!(false));
        assert_eq!(store["tools"]["shell"], serde_json::json!(false));
    }

    #[test]
    fn toggle_update_removes_enabled_override() {
        let mut store = serde_json::json!({
            "skills": {
                "alpha": false
            },
            "tools": {}
        });

        apply_toggle_update(&mut store, "skills", "alpha", true);

        assert!(store["skills"].get("alpha").is_none());
    }

    #[tokio::test]
    async fn atomic_write_string_replaces_file_contents() {
        let dir = std::env::temp_dir().join(format!(
            "blockcell-toggle-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create temp dir");
        let path = dir.join("toggles.json");
        tokio::fs::write(&path, "{\"skills\":{}}")
            .await
            .expect("initial write");

        atomic_write_string(&path, "{\"tools\":{}}")
            .await
            .expect("atomic write");

        let content = tokio::fs::read_to_string(&path).await.expect("read file");
        assert_eq!(content, "{\"tools\":{}}");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
