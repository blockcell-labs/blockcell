use super::*;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_skills_dir(tag: &str) -> PathBuf {
    let mut root = std::env::temp_dir();
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    root.push(format!(
        "blockcell_hybrid_prompt_{}_{}_{}",
        tag,
        std::process::id(),
        now_ns
    ));
    std::fs::create_dir_all(&root).expect("create temp skills dir");
    root
}

fn sample_hybrid_context() -> EvolutionContext {
    EvolutionContext {
        skill_name: "hybrid_demo".to_string(),
        current_version: "v1".to_string(),
        trigger: TriggerReason::ManualRequest {
            description: "build a hybrid skill".to_string(),
        },
        error_stack: None,
        source_snippet: Some("print('hello')\n".to_string()),
        source_path: Some("SKILL.py".to_string()),
        layout: SkillLayout::Hybrid,
        tool_schemas: vec![],
        timestamp: chrono::Utc::now().timestamp(),
        skill_type: SkillType::Python,
        staged: false,
        staging_skills_dir: None,
    }
}

#[test]
fn test_hybrid_generation_prompt_mentions_manual_and_entrypoint_boundary() {
    let skills_dir = temp_skills_dir("gen");
    let engine = SkillEvolution::new(skills_dir, 5);
    let prompt = engine
        .build_hybrid_generation_prompt(&sample_hybrid_context())
        .expect("build hybrid generation prompt");

    assert!(prompt.contains("## Hybrid Contract"));
    assert!(prompt.contains("SKILL.md` defines the user-facing behavior"));
    assert!(prompt.contains("## Summary {#summary}"));
    assert!(prompt.contains("Current entrypoint: `SKILL.py`"));
    assert!(prompt.contains("exec_local"));
    assert!(prompt.contains("local execution is appropriate"));
}

#[test]
fn test_hybrid_fix_prompt_mentions_manual_and_entrypoint_boundary() {
    let skills_dir = temp_skills_dir("fix");
    let engine = SkillEvolution::new(skills_dir, 5);
    let feedback = FeedbackEntry {
        attempt: 1,
        stage: "compile".to_string(),
        feedback: "entrypoint mismatch".to_string(),
        previous_code: "print('bad')\n".to_string(),
        timestamp: chrono::Utc::now().timestamp(),
    };

    let prompt = engine
        .build_hybrid_fix_prompt(&sample_hybrid_context(), &feedback, &[])
        .expect("build hybrid fix prompt");

    assert!(prompt.contains("## Hybrid Contract"));
    assert!(prompt.contains("Keep the manual and the entrypoint aligned"));
    assert!(prompt.contains("## Summary {#summary}"));
    assert!(prompt.contains("Current entrypoint: `SKILL.py`"));
    assert!(prompt.contains("exec_local"));
    assert!(prompt.contains("entrypoint mismatch"));
}

#[tokio::test]
async fn test_compile_local_script_detects_python_shebang_for_no_extension_script() {
    let skills_dir = temp_skills_dir("no_ext_shebang");
    let engine = SkillEvolution::new(skills_dir.clone(), 5);
    let skill_path = skills_dir.join("run-me");
    std::fs::write(
        &skill_path,
        "#!/usr/bin/env python3\nprint(\"unterminated\"\n",
    )
    .expect("write shebang script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&skill_path)
            .expect("script metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&skill_path, perms).expect("set executable bit");
    }

    let (passed, error) = engine
        .compile_local_script(&skill_path)
        .await
        .expect("compile should run");

    assert!(!passed);
    let error = error.expect("should return syntax error");
    assert!(error.contains("SyntaxError") || error.contains("unterminated"));
}

// ── Deployment chain: contract check / observation window / rollback ───────

fn evo_root(tag: &str) -> PathBuf {
    let mut root = std::env::temp_dir();
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    root.push(format!(
        "blockcell_evo_{}_{}_{}",
        tag,
        std::process::id(),
        now_ns
    ));
    std::fs::create_dir_all(root.join("skills")).expect("create skills dir");
    root
}

fn record_for(
    id: &str,
    layout: SkillLayout,
    skill_type: SkillType,
    staged: bool,
) -> EvolutionRecord {
    let now = chrono::Utc::now().timestamp();
    let skill_name = format!("skill_{id}");
    EvolutionRecord {
        id: id.to_string(),
        skill_name: skill_name.clone(),
        context: EvolutionContext {
            skill_name,
            current_version: "v1".to_string(),
            trigger: TriggerReason::ManualRequest {
                description: "test".to_string(),
            },
            error_stack: None,
            source_snippet: None,
            source_path: None,
            layout,
            tool_schemas: vec![],
            timestamp: now,
            skill_type,
            staged,
            staging_skills_dir: None,
        },
        patch: None,
        audit: None,
        shadow_test: None,
        observation: None,
        observation_total_calls: 0,
        observation_error_calls: 0,
        rollout: None,
        status: EvolutionStatus::Triggered,
        attempt: 1,
        feedback_history: Vec::new(),
        created_at: now,
        updated_at: now,
    }
}

fn patch_with(diff: &str) -> GeneratedPatch {
    GeneratedPatch {
        patch_id: "patch-1".to_string(),
        skill_name: "skill".to_string(),
        diff: diff.to_string(),
        explanation: String::new(),
        generated_at: chrono::Utc::now().timestamp(),
    }
}

/// A SKILL.md that satisfies every `contract_check` requirement.
fn well_formed_skill_md() -> String {
    let mut body = String::from(
        "# Demo skill\n\n\
         ## Shared {#shared}\n\n\
         Shared preamble that both the manual and the prompt read before doing any work.\n\n\
         ## Prompt {#prompt}\n\n\
         Use the tool output verbatim; never invent values that were not returned.\n",
    );
    // Clear the 100-char floor even if the prose above is trimmed later.
    while body.len() < 100 {
        body.push('#');
    }
    body
}

fn engine_with(tag: &str, record: &EvolutionRecord) -> SkillEvolution {
    let root = evo_root(tag);
    let engine = SkillEvolution::new(root.join("skills"), 5);
    engine.save_record(record).expect("save record");
    engine
}

#[test]
fn contract_check_accepts_a_well_formed_prompt_tool_skill() {
    let id = "evo_contract_pass";
    let mut record = record_for(id, SkillLayout::PromptTool, SkillType::PromptOnly, false);
    record.patch = Some(patch_with(&well_formed_skill_md()));
    let engine = engine_with("contract_pass", &record);

    let (passed, error) = engine.contract_check(id).expect("contract check runs");

    assert!(passed, "{error:?}");
    assert!(error.is_none());
}

#[test]
fn contract_check_rejects_a_prompt_tool_skill_missing_a_required_section() {
    let id = "evo_contract_section";
    let mut record = record_for(id, SkillLayout::PromptTool, SkillType::PromptOnly, false);
    record.patch = Some(patch_with(
        &well_formed_skill_md().replace("## Prompt {#prompt}", "## Notes"),
    ));
    let engine = engine_with("contract_section", &record);

    let (passed, error) = engine.contract_check(id).expect("contract check runs");

    assert!(!passed);
    let error = error.expect("issues");
    assert!(error.contains("## Prompt"), "{error}");
}

#[test]
fn contract_check_rejects_a_prompt_tool_skill_without_a_shared_section() {
    let id = "evo_contract_shared";
    let mut record = record_for(id, SkillLayout::PromptTool, SkillType::PromptOnly, false);
    record.patch = Some(patch_with(
        &well_formed_skill_md().replace("## Shared {#shared}", "## Overview"),
    ));
    let engine = engine_with("contract_shared", &record);

    let (passed, error) = engine.contract_check(id).expect("contract check runs");

    assert!(!passed);
    assert!(error.expect("issues").contains("## Shared"));
}

#[test]
fn contract_check_rejects_a_too_short_prompt_tool_skill() {
    let id = "evo_contract_short";
    let mut record = record_for(id, SkillLayout::PromptTool, SkillType::PromptOnly, false);
    // Sections are present, only the 100-char floor is violated.
    record.patch = Some(patch_with("# s\n\n## Shared\n\n## Prompt\n"));
    let engine = engine_with("contract_short", &record);

    let (passed, error) = engine.contract_check(id).expect("contract check runs");

    assert!(!passed);
    assert!(error.expect("issues").contains("too short"));
}

#[test]
fn contract_check_requires_skill_md_for_hybrid_skills() {
    let id = "evo_contract_hybrid";
    let mut record = record_for(id, SkillLayout::Hybrid, SkillType::Python, false);
    record.patch = Some(patch_with("print('hi')\n"));
    let engine = engine_with("contract_hybrid", &record);

    let (passed, error) = engine.contract_check(id).expect("contract check runs");

    assert!(!passed);
    assert!(error.expect("issues").contains("SKILL.md: file is missing"));
}

#[test]
fn contract_check_uses_patch_content_for_hybrid_prompt_only_skills() {
    let id = "evo_contract_hybrid_prompt";
    let mut record = record_for(id, SkillLayout::Hybrid, SkillType::PromptOnly, false);
    record.patch = Some(patch_with(&well_formed_skill_md()));
    let engine = engine_with("contract_hybrid_prompt", &record);

    let (passed, error) = engine.contract_check(id).expect("contract check runs");

    // SKILL.md is not on disk yet, but patch.diff *is* the new SKILL.md.
    assert!(passed, "{error:?}");
}

/// Characterisation test: for LocalScript the SKILL.md check is a soft check —
/// a missing SKILL.md is skipped rather than reported (see review P2-4).
#[test]
fn contract_check_soft_skips_skill_md_for_local_scripts() {
    let id = "evo_contract_local";
    let mut record = record_for(id, SkillLayout::LocalScript, SkillType::LocalScript, false);
    record.patch = Some(patch_with("#!/bin/sh\necho hi\n"));
    let root = evo_root("contract_local");
    let skills_dir = root.join("skills");
    let engine = SkillEvolution::new(skills_dir.clone(), 5);
    engine.save_record(&record).expect("save record");

    let skill_dir = skills_dir.join(&record.skill_name);
    std::fs::create_dir_all(skill_dir.join("scripts")).expect("create scripts dir");
    std::fs::write(skill_dir.join("scripts/skill.sh"), "#!/bin/sh\necho hi\n")
        .expect("write primary file");

    let (passed, error) = engine.contract_check(id).expect("contract check runs");

    assert!(
        passed,
        "LocalScript SKILL.md is only a soft check today: {error:?}"
    );
}

#[test]
fn check_observation_reports_still_observing_for_a_fresh_window() {
    let id = "evo_obs_fresh";
    let mut record = record_for(id, SkillLayout::PromptTool, SkillType::PromptOnly, false);
    record.observation = Some(ObservationWindow::default());
    record.status = EvolutionStatus::Observing;
    let engine = engine_with("obs_fresh", &record);

    assert_eq!(
        engine.check_observation(id, 0.0).expect("observation"),
        None
    );
}

#[test]
fn check_observation_flags_an_error_rate_above_threshold_for_rollback() {
    let id = "evo_obs_errors";
    let mut record = record_for(id, SkillLayout::PromptTool, SkillType::PromptOnly, false);
    record.observation = Some(ObservationWindow::default());
    record.status = EvolutionStatus::Observing;
    let engine = engine_with("obs_errors", &record);

    // Default threshold is 0.1.
    assert_eq!(
        engine.check_observation(id, 0.5).expect("observation"),
        Some(false)
    );
}

#[test]
fn check_observation_completes_after_the_window_elapses() {
    let id = "evo_obs_done";
    let mut record = record_for(id, SkillLayout::PromptTool, SkillType::PromptOnly, false);
    record.observation = Some(ObservationWindow {
        duration_minutes: 60,
        error_threshold: 0.1,
        started_at: chrono::Utc::now().timestamp() - 61 * 60,
    });
    record.status = EvolutionStatus::Observing;
    let engine = engine_with("obs_done", &record);

    assert_eq!(
        engine.check_observation(id, 0.0).expect("observation"),
        Some(true)
    );
}

#[test]
fn check_observation_requires_a_window() {
    let id = "evo_obs_none";
    let record = record_for(id, SkillLayout::PromptTool, SkillType::PromptOnly, false);
    let engine = engine_with("obs_none", &record);

    let error = engine.check_observation(id, 0.0).unwrap_err();
    assert!(
        error.to_string().contains("No observation window"),
        "{error}"
    );
}

#[tokio::test]
async fn deploy_and_observe_rejects_a_record_that_has_not_compiled() {
    let id = "evo_deploy_status";
    let mut record = record_for(id, SkillLayout::PromptTool, SkillType::PromptOnly, false);
    record.status = EvolutionStatus::Generated;
    let engine = engine_with("deploy_status", &record);

    let error = engine
        .deploy_and_observe(id)
        .await
        .expect_err("must not deploy before compiling");
    assert!(
        error.to_string().contains("expected status CompilePassed"),
        "{error}"
    );
}

#[tokio::test]
async fn deploy_and_observe_requires_a_passed_audit() {
    let id = "evo_deploy_audit";
    let mut record = record_for(id, SkillLayout::PromptTool, SkillType::PromptOnly, false);
    record.status = EvolutionStatus::CompilePassed;
    record.audit = Some(AuditResult {
        passed: false,
        issues: vec![],
        audited_at: chrono::Utc::now().timestamp(),
    });
    let engine = engine_with("deploy_audit", &record);

    let error = engine.deploy_and_observe(id).await.unwrap_err();
    assert!(error.to_string().contains("Audit not passed"), "{error}");
}

#[tokio::test]
async fn deploy_and_observe_requires_an_audit_at_all() {
    let id = "evo_deploy_no_audit";
    let mut record = record_for(id, SkillLayout::PromptTool, SkillType::PromptOnly, false);
    record.status = EvolutionStatus::CompilePassed;
    let engine = engine_with("deploy_no_audit", &record);

    let error = engine.deploy_and_observe(id).await.unwrap_err();
    assert!(error.to_string().contains("Audit not passed"), "{error}");
}

#[tokio::test]
async fn rollback_of_a_staged_import_without_a_baseline_removes_the_skill_dir() {
    let id = "evo_rollback_staged";
    let mut record = record_for(id, SkillLayout::PromptTool, SkillType::PromptOnly, true);
    record.status = EvolutionStatus::Observing;
    let root = evo_root("rollback_staged");
    let skills_dir = root.join("skills");
    let engine = SkillEvolution::new(skills_dir.clone(), 5);
    engine.save_record(&record).expect("save record");

    let main_dir = skills_dir.join(&record.skill_name);
    std::fs::create_dir_all(&main_dir).expect("create staged skill dir");
    std::fs::write(main_dir.join("SKILL.md"), "# staged\n").expect("write SKILL.md");

    engine
        .rollback(id, "staged import has no baseline to restore")
        .await
        .expect("rollback runs");

    assert!(
        !main_dir.exists(),
        "a staged import without a baseline must be removed, not restored"
    );
    let reloaded = engine.load_record(id).expect("reload record");
    assert_eq!(reloaded.status, EvolutionStatus::RolledBack);
}
