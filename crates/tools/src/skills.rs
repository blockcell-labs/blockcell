use async_trait::async_trait;
use blockcell_core::Result;
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolSchema};

/// Tool for querying skill evolution status — what skills are being learned,
/// what skills have been learned, and the overall evolution state.
pub struct ListSkillsTool;

#[async_trait]
impl Tool for ListSkillsTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "list_skills",
            description: "Query the skill evolution system. Shows which skills are currently being learned (evolving), which have been learned (completed evolutions), and available skills. Use when the user asks about learning progress, skill status, or capabilities.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "enum": ["learning", "learned", "all", "available"],
                        "description": "What to query: 'learning' = skills currently being evolved/learned, 'learned' = skills that completed evolution, 'all' = full status, 'available' = currently loaded skills"
                    }
                },
                "required": []
            }),
        }
    }

    fn validate(&self, _params: &Value) -> Result<()> {
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let query = params.get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("all");

        // workspace = ~/.blockcell/workspace
        // skills_dir = ~/.blockcell/workspace/skills
        // evolution_records = ~/.blockcell/workspace/evolution_records
        let skills_dir = ctx.workspace.join("skills");
        let evolution_records_dir = ctx.workspace.join("evolution_records");
        let builtin_dir = ctx.builtin_skills_dir.as_deref();

        match query {
            "learning" => self.get_learning_skills(&evolution_records_dir).await,
            "learned" => self.get_learned_skills(&evolution_records_dir).await,
            "available" => self.get_available_skills(&skills_dir, builtin_dir).await,
            _ => self.get_all_skills(&skills_dir, builtin_dir, &evolution_records_dir).await,
        }
    }
}

impl ListSkillsTool {
    /// Get skills currently being evolved (Triggered, Generating, Generated, Auditing, etc.)
    async fn get_learning_skills(&self, records_dir: &std::path::Path) -> Result<Value> {
        let records = self.load_evolution_records(records_dir)?;
        let learning: Vec<Value> = records.iter()
            .filter(|r| {
                let status = r.get("status").and_then(|s| s.as_str()).unwrap_or("");
                matches!(status, "Triggered" | "Generating" | "Generated" | "Auditing"
                    | "AuditPassed" | "DryRunPassed" | "Testing" | "TestPassed" | "RollingOut")
            })
            .map(|r| {
                let status = r.get("status").and_then(|s| s.as_str()).unwrap_or("unknown");
                let status_desc = match status {
                    "Triggered" => "已触发，等待开始学习",
                    "Generating" => "正在生成改进方案",
                    "Generated" => "改进方案已生成，等待审计",
                    "Auditing" => "正在审计改进方案",
                    "AuditPassed" => "审计通过，准备测试",
                    "DryRunPassed" => "编译检查通过，准备影子测试",
                    "Testing" => "正在进行影子测试",
                    "TestPassed" => "测试通过，准备灰度发布",
                    "RollingOut" => "正在灰度发布中",
                    _ => status,
                };
                json!({
                    "skill_name": r.get("skill_name").unwrap_or(&Value::Null),
                    "evolution_id": r.get("id").unwrap_or(&Value::Null),
                    "status": status,
                    "status_description": status_desc,
                    "trigger": r.get("context").and_then(|c| c.get("trigger")),
                    "created_at": r.get("created_at"),
                })
            })
            .collect();

        Ok(json!({
            "learning_skills": learning,
            "count": learning.len(),
            "note": if learning.is_empty() {
                "目前没有正在学习的技能。当工具执行失败时，系统会自动触发学习。"
            } else {
                "以下技能正在学习改进中。"
            }
        }))
    }

    /// Get skills that completed evolution successfully
    async fn get_learned_skills(&self, records_dir: &std::path::Path) -> Result<Value> {
        let records = self.load_evolution_records(records_dir)?;
        let learned: Vec<Value> = records.iter()
            .filter(|r| {
                let status = r.get("status").and_then(|s| s.as_str()).unwrap_or("");
                status == "Completed"
            })
            .map(|r| {
                json!({
                    "skill_name": r.get("skill_name").unwrap_or(&Value::Null),
                    "evolution_id": r.get("id").unwrap_or(&Value::Null),
                    "created_at": r.get("created_at"),
                    "updated_at": r.get("updated_at"),
                    "trigger": r.get("context").and_then(|c| c.get("trigger")),
                    "patch_explanation": r.get("patch").and_then(|p| p.get("explanation")),
                })
            })
            .collect();

        Ok(json!({
            "learned_skills": learned,
            "count": learned.len(),
            "note": if learned.is_empty() {
                "目前还没有通过自进化学会的技能。系统会在工具执行失败时自动触发学习。"
            } else {
                "以下技能已通过自进化学会。"
            }
        }))
    }

    /// Get available loaded skills
    async fn get_available_skills(&self, skills_dir: &std::path::Path, builtin_dir: Option<&std::path::Path>) -> Result<Value> {
        let mut skills = Vec::new();
        let mut seen_names = std::collections::HashSet::new();

        // Scan workspace skills first (higher priority)
        self.scan_skills_dir(skills_dir, &mut skills, &mut seen_names);

        // Scan builtin skills (lower priority, skip duplicates)
        if let Some(builtin) = builtin_dir {
            self.scan_skills_dir(builtin, &mut skills, &mut seen_names);
        }

        Ok(json!({
            "available_skills": skills,
            "count": skills.len(),
        }))
    }

    /// Scan a single directory for skill subdirectories.
    fn scan_skills_dir(&self, dir: &std::path::Path, skills: &mut Vec<Value>, seen: &mut std::collections::HashSet<String>) {
        if !dir.exists() || !dir.is_dir() {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let name = path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_string();

                    // Skip if already seen (workspace overrides builtin)
                    if !seen.insert(name.clone()) {
                        continue;
                    }

                    let meta = self.read_skill_meta(&path);
                    let has_skill_file = path.join("SKILL.rhai").exists()
                        || path.join("SKILL.md").exists();

                    if has_skill_file {
                        skills.push(json!({
                            "name": name,
                            "description": meta.get("description").unwrap_or(&Value::Null),
                            "always": meta.get("always").unwrap_or(&json!(false)),
                            "path": path.display().to_string(),
                        }));
                    }
                }
            }
        }
    }

    /// Get all skills info combined
    async fn get_all_skills(
        &self,
        skills_dir: &std::path::Path,
        builtin_dir: Option<&std::path::Path>,
        records_dir: &std::path::Path,
    ) -> Result<Value> {
        let available = self.get_available_skills(skills_dir, builtin_dir).await?;
        let learning = self.get_learning_skills(records_dir).await?;
        let learned = self.get_learned_skills(records_dir).await?;

        Ok(json!({
            "available": available,
            "learning": learning,
            "learned": learned,
        }))
    }

    /// Load all evolution records from the records directory
    fn load_evolution_records(&self, records_dir: &std::path::Path) -> Result<Vec<Value>> {
        let mut records = Vec::new();

        if !records_dir.exists() {
            return Ok(records);
        }

        if let Ok(entries) = std::fs::read_dir(records_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |e| e == "json") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(record) = serde_json::from_str::<Value>(&content) {
                            records.push(record);
                        }
                    }
                }
            }
        }

        // Sort by created_at descending
        records.sort_by(|a, b| {
            let a_ts = a.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0);
            let b_ts = b.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0);
            b_ts.cmp(&a_ts)
        });

        Ok(records)
    }

    /// Read skill meta.yaml or meta.json
    fn read_skill_meta(&self, skill_dir: &std::path::Path) -> Value {
        // Try meta.json first (simpler to parse)
        let json_path = skill_dir.join("meta.json");
        if json_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&json_path) {
                if let Ok(meta) = serde_json::from_str::<Value>(&content) {
                    return meta;
                }
            }
        }

        // Try meta.yaml (parse key: value lines manually to avoid serde_yaml dependency)
        let yaml_path = skill_dir.join("meta.yaml");
        if yaml_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&yaml_path) {
                let mut meta = serde_json::Map::new();
                for line in content.lines() {
                    if let Some((key, val)) = line.split_once(':') {
                        let key = key.trim().to_string();
                        let val = val.trim();
                        // Handle boolean
                        if val == "true" {
                            meta.insert(key, Value::Bool(true));
                        } else if val == "false" {
                            meta.insert(key, Value::Bool(false));
                        } else {
                            meta.insert(key, Value::String(val.to_string()));
                        }
                    }
                }
                if !meta.is_empty() {
                    return Value::Object(meta);
                }
            }
        }

        json!({})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_list_skills_schema() {
        let tool = ListSkillsTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "list_skills");
    }

    #[test]
    fn test_list_skills_validate() {
        let tool = ListSkillsTool;
        assert!(tool.validate(&json!({})).is_ok());
        assert!(tool.validate(&json!({"query": "learning"})).is_ok());
    }

    #[test]
    fn test_read_skill_meta_missing() {
        let tool = ListSkillsTool;
        let meta = tool.read_skill_meta(std::path::Path::new("/nonexistent"));
        assert!(meta.is_object());
        assert!(meta.as_object().unwrap().is_empty());
    }
}
