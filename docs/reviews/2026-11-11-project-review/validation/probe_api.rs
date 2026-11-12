//! Review evidence, not a test suite that certifies absence of defects.
//! All writes/reads use the runner-owned temporary directory passed as argv[1].
use blockcell_agent::skill_file_store::SkillFileStore;
use blockcell_core::{Config, Paths};
use blockcell_core::types::{ChatMessage, PermissionSet};
use blockcell_core::config::{mask_secret_values, restore_masked_secrets};
use blockcell_core::path_policy::{PathOp, PathPolicy};
use blockcell_storage::{MemoryStore, SessionStore};
use blockcell_tools::{Tool, ToolContext};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

fn emit(id: &str, observed: Value) { println!("{}", json!({"id":id,"observed":observed})); }

fn context(base: &Path) -> ToolContext {
    ToolContext {
        workspace: base.join("workspace"), base: base.into(), sandbox_writable_roots: None,
        builtin_skills_dir: None, active_skill_dir: None, session_key: "cli:review".into(),
        channel: "cli".into(), account_id: None, sender_id: None, sender_role: None,
        chat_id: "review".into(), config: Config::default(), permissions: PermissionSet::new(),
        task_manager: None, memory_store: None, memory_file_store: None,
        ghost_memory_lifecycle: None, skill_file_store: None, session_search: None,
        outbound_tx: None, spawn_handle: None, capability_registry: None, core_evolution: None,
        event_emitter: None, channel_contacts_file: None, response_cache: None,
        runtime_handle: None, agent_identity: None, skill_mutex: None,
        agent_type_registry: None, evolution_workflow_store: None,
    }
}

fn make_zip(path: &Path, member: &str, content: &str) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    zip.start_file(member, zip::write::SimpleFileOptions::default()).unwrap();
    zip.write_all(content.as_bytes()).unwrap();
    zip.finish().unwrap();
}

#[tokio::main]
async fn main() {
    let base = PathBuf::from(std::env::args().nth(1).expect("temporary data directory required"));
    std::fs::create_dir_all(base.join("workspace")).unwrap();
    let paths = Paths::with_base(base.clone());

    let store = SessionStore::new(paths.clone());
    store.append("ws:a:b", &ChatMessage::user("artificial-session-A")).unwrap();
    let b_history = store.load("ws:a_b").unwrap();
    store.append("ws:a_b", &ChatMessage::user("artificial-session-B")).unwrap();
    store.clear("ws:a_b").unwrap();
    emit("B1-01", json!({"B_first_read":b_history,"A_exists_after_clear_B":paths.session_file("ws:a:b").exists(),"B_exists_after_clear_B":paths.session_file("ws:a_b").exists()}));

    let config_path = base.join("config-map.json5");
    let mut cfg = Config::default();
    cfg.providers.insert("review-obsolete".into(), Default::default());
    cfg.save(&config_path).unwrap();
    cfg.providers.remove("review-obsolete");
    cfg.save(&config_path).unwrap();
    let reloaded = Config::load(&config_path).unwrap();
    emit("B1-02", json!({"removed_provider_restored":reloaded.providers.contains_key("review-obsolete")}));

    let bad_path = base.join("config-invalid.json5");
    let save = blockcell_core::config::write_raw_validated_config_json5(&bad_path, r#"{"defaultTimezone":"Mars/Olympus"}"#);
    emit("B1-03", json!({"save_ok":save.is_ok(),"load_error":Config::load(&bad_path).err().map(|e|e.to_string())}));

    let current = json!({"agents":{"list":[
        {"id":"ops","maxTokens":1024,"maxContextTokens":4096},
        {"id":"dev","maxTokens":8192,"maxContextTokens":32768}
    ]}});
    let mut incoming = current.clone();
    mask_secret_values(&mut incoming);
    incoming["agents"]["list"].as_array_mut().unwrap().remove(0);
    restore_masked_secrets(&current, &mut incoming);
    let parsed: Config = serde_json::from_value(incoming.clone()).unwrap();
    let dev = parsed.config_for_agent("dev").unwrap();
    emit("B1-04",json!({"after_delete_ops":incoming,"dev_effective_max_tokens":dev.agents.defaults.max_tokens,"dev_effective_context":dev.agents.defaults.max_context_tokens}));

    let memory = MemoryStore::open(&base.join("memory.db")).unwrap();
    emit("B1-05",json!({"missing_id_result":format!("{:?}",memory.get_by_id("artificial-missing-id"))}));

    let model: blockcell_core::config::ModelEntry = serde_json::from_value(json!({"model":"dummy","provider":"dummy","maxTokens":1234})).unwrap();
    emit("NEW-model-token-setting",json!({"serialized_model":serde_json::to_value(model).unwrap()}));

    let outside = base.join("outside-workspace");
    std::fs::create_dir_all(&outside).unwrap();
    let marker = outside.join("marker.txt");
    std::fs::write(&marker,"artificial-denied-marker").unwrap();
    let policy_file = base.join("path-policy.json5");
    std::fs::write(&policy_file,json!({"version":1,"default_policy":"confirm","builtin_protected_paths":false,"rules":[{"name":"review-denied-marker","action":"deny","ops":["read","write"],"paths":[outside]}]}).to_string()).unwrap();
    let policy = PathPolicy::load(&policy_file);
    let skill_dir = paths.skills_dir().join("review-rhai");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("meta.yaml"),"name: review-rhai\ndescription: Artificial review probe\ntools: [read_file]\n").unwrap();
    let script = format!("let r = call_tool(\"read_file\", #{{path: {}}}); set_output(r);",serde_json::to_string(marker.to_str().unwrap()).unwrap());
    std::fs::write(skill_dir.join("probe.rhai"),script).unwrap();
    let mut ctx = context(&base);
    ctx.active_skill_dir = Some(skill_dir);
    let rhai = blockcell_tools::exec_skill_script::ExecSkillScriptTool::default().execute(ctx,json!({"path":"probe.rhai"})).await;
    emit("B3-01",json!({"host_policy":format!("{:?}",policy.evaluate(&marker,PathOp::Read)),"requires_confirmation":blockcell_tools::exec_skill_script::ExecSkillScriptTool::default().requires_confirmation(&json!({"path":"probe.rhai"})),"tool_result":format!("{:?}",rhai)}));

    let out = paths.workspace().join("unzip");
    std::fs::create_dir_all(&out).unwrap();
    std::os::unix::fs::symlink(&outside,out.join("linked-directory")).unwrap();
    let zip = paths.workspace().join("sample.zip");
    make_zip(&zip,"linked-directory/marker.txt","artificial-zip-overwrite");
    let result = blockcell_tools::file_ops::FileOpsTool.execute(context(&base),json!({"action":"decompress","path":zip,"destination":out})).await;
    emit("B3-02",json!({"tool_ok":result.is_ok(),"outside_marker":std::fs::read_to_string(&marker).unwrap()}));

    let implicit_zip = outside.join("implicit.zip");
    make_zip(&implicit_zip,"implicit-output.txt","artificial-implicit-output");
    let result = blockcell_tools::file_ops::FileOpsTool.execute(context(&base),json!({"action":"decompress","path":implicit_zip})).await;
    emit("NEW-implicit-decompress-destination",json!({"tool_ok":result.is_ok(),"outside_output_exists":outside.join("implicit-output.txt").exists()}));

    let skill_store = SkillFileStore::open(&paths).unwrap();
    let standard = paths.skills_dir().join("csv_stats_dir");
    std::fs::create_dir_all(&standard).unwrap();
    std::fs::write(standard.join("SKILL.md"),"---\nname: csv-stats\ndescription: Artificial statistics skill\n---\nInstructions\n").unwrap();
    let before = blockcell_tools::skills::ListSkillsTool.execute(context(&base),json!({"query":"all"})).await.unwrap();
    emit("B3-03",json!({"list":before,"view_by_name_error":skill_store.view("csv-stats").err().map(|e|e.to_string()),"view_by_directory_ok":skill_store.view("csv_stats_dir").is_ok()}));
    skill_store.delete("csv_stats_dir").unwrap();
    let after = blockcell_tools::skills::ListSkillsTool.execute(context(&base),json!({"query":"all"})).await.unwrap();
    emit("B3-04",json!({"deleted_directory_exists":standard.exists(),"list_after_delete":after}));
}
