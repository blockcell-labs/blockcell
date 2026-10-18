use blockcell_core::tool_policy::{
    PolicyEvalResult, ToolCallContext, ToolPolicy, ToolPolicyCondition, ToolPolicyConfig,
    ToolPolicyDecision, ToolPolicyRule,
};
use std::collections::HashMap;
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

fn ctx<'a>(
    tool_name: &'a str,
    tool_args: &'a serde_json::Value,
    channel: &'a str,
) -> ToolCallContext<'a> {
    ToolCallContext {
        tool_name,
        tool_args,
        channel,
    }
}

fn rule(name: &str, tool: &str, decision: ToolPolicyDecision) -> ToolPolicyRule {
    ToolPolicyRule {
        name: name.to_string(),
        tool: tool.to_string(),
        decision,
        when: None,
        description: None,
        inherit_from: None,
    }
}

#[test]
fn glob_tool_rules_match_mcp_tools() {
    let policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![rule("deny-mcp", "mcp__*", ToolPolicyDecision::Deny)],
        ..Default::default()
    });

    let eval = policy.evaluate(&ctx("mcp__server__danger", &serde_json::json!({}), "cli"));

    assert_eq!(eval.decision, ToolPolicyDecision::Deny);
    assert_eq!(eval.matched_rule.as_deref(), Some("deny-mcp"));
}

#[test]
fn pipe_separated_tool_patterns_match_any_pattern() {
    let policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![rule(
            "ask-file-writes",
            "write_file|edit_file",
            ToolPolicyDecision::Ask,
        )],
        ..Default::default()
    });

    assert_eq!(
        policy
            .evaluate(&ctx("edit_file", &serde_json::json!({}), "cli"))
            .decision,
        ToolPolicyDecision::Ask
    );
}

#[test]
fn conditions_match_channel_and_path_argument_candidates() {
    let mut deny_upload = rule(
        "deny-secret-upload",
        "http_request",
        ToolPolicyDecision::Deny,
    );
    deny_upload.when = Some(ToolPolicyCondition {
        path_glob: Some("*.env*".to_string()),
        channel: Some("telegram".to_string()),
        ..Default::default()
    });
    let policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![deny_upload],
        ..Default::default()
    });

    let eval = policy.evaluate(&ctx(
        "http_request",
        &serde_json::json!({"output_path": "prod.env.local"}),
        "telegram",
    ));

    assert_eq!(eval.decision, ToolPolicyDecision::Deny);
}

#[test]
fn path_glob_expands_tilde_in_policy_and_arguments() {
    let mut deny_ssh = rule("deny-ssh", "read_file", ToolPolicyDecision::Deny);
    deny_ssh.when = Some(ToolPolicyCondition {
        path_glob: Some("~/.ssh/*".to_string()),
        ..Default::default()
    });
    let policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![deny_ssh],
        ..Default::default()
    });

    let eval = policy.evaluate(&ctx(
        "read_file",
        &serde_json::json!({"path": "~/.ssh/id_rsa"}),
        "cli",
    ));

    assert_eq!(eval.decision, ToolPolicyDecision::Deny);
}

#[test]
fn path_glob_normalizes_dotdot_and_checks_all_path_args() {
    let mut allow_work = rule("allow-work", "write_file", ToolPolicyDecision::Allow);
    allow_work.when = Some(ToolPolicyCondition {
        path_glob: Some("/tmp/work/*".to_string()),
        ..Default::default()
    });
    // 默认 deny：allow 规则不命中即 fail-closed
    let policy = ToolPolicy::from_config(ToolPolicyConfig {
        default_decision: ToolPolicyDecision::Deny,
        rules: vec![allow_work],
        ..Default::default()
    });

    // `..` 穿越出 allow 前缀 → 规范化后不命中 → deny
    let eval = policy.evaluate(&ctx(
        "write_file",
        &serde_json::json!({"path": "/tmp/work/../../etc/passwd"}),
        "cli",
    ));
    assert_eq!(eval.decision, ToolPolicyDecision::Deny);

    // 第二个路径参数越权：此前只检查第一个存在的路径参数
    let eval = policy.evaluate(&ctx(
        "write_file",
        &serde_json::json!({"path": "/tmp/work/ok.txt", "output_path": "/etc/evil"}),
        "cli",
    ));
    assert_eq!(eval.decision, ToolPolicyDecision::Deny);

    // 正常路径仍放行
    let eval = policy.evaluate(&ctx(
        "write_file",
        &serde_json::json!({"path": "/tmp/work/ok.txt"}),
        "cli",
    ));
    assert_eq!(eval.decision, ToolPolicyDecision::Allow);
}

#[test]
fn simulation_mode_allows_matching_deny_rules() {
    let policy = ToolPolicy::from_config(ToolPolicyConfig {
        simulation_mode: true,
        rules: vec![rule("deny-exec", "exec", ToolPolicyDecision::Deny)],
        ..Default::default()
    });

    let PolicyEvalResult {
        decision,
        simulated,
        matched_rule,
        ..
    } = policy.evaluate(&ctx("exec", &serde_json::json!({}), "cli"));

    assert_eq!(decision, ToolPolicyDecision::Allow);
    assert!(simulated);
    assert_eq!(matched_rule.as_deref(), Some("deny-exec"));
}

#[test]
fn inherit_from_expands_rule_groups_before_current_rule() {
    let mut groups = HashMap::new();
    groups.insert(
        "base-deny-set".to_string(),
        vec![rule("deny-exec-base", "exec", ToolPolicyDecision::Deny)],
    );
    let mut strict = rule("strict-profile", "*", ToolPolicyDecision::Ask);
    strict.inherit_from = Some("base-deny-set".to_string());
    let policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![strict],
        rule_groups: groups,
        ..Default::default()
    });

    let eval = policy.evaluate(&ctx("exec", &serde_json::json!({}), "cli"));

    assert_eq!(eval.decision, ToolPolicyDecision::Deny);
    assert_eq!(eval.matched_rule.as_deref(), Some("deny-exec-base"));
}

#[test]
fn invalid_or_oversized_regex_rule_fails_safe() {
    let mut invalid = rule("bad-regex", "exec", ToolPolicyDecision::Deny);
    invalid.when = Some(ToolPolicyCondition {
        command_regex: Some("(".to_string()),
        ..Default::default()
    });
    let mut oversized = rule("oversized-regex", "exec", ToolPolicyDecision::Deny);
    oversized.when = Some(ToolPolicyCondition {
        command_regex: Some("a".repeat(1025)),
        ..Default::default()
    });
    let policy = ToolPolicy::from_config(ToolPolicyConfig {
        default_decision: ToolPolicyDecision::Allow,
        rules: vec![invalid, oversized],
        ..Default::default()
    });

    let eval = policy.evaluate(&ctx(
        "exec",
        &serde_json::json!({"command": "rm -rf /"}),
        "cli",
    ));

    assert_eq!(eval.decision, ToolPolicyDecision::Deny);
    assert_eq!(eval.matched_rule, None);
}

#[test]
fn malformed_policy_file_fails_closed() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("blockcell-tool-policy-{nonce}"));
    fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("tool_policy.yaml");
    fs::write(&path, "rules: [not valid").expect("write malformed policy");

    let policy = ToolPolicy::load(&path);
    let eval = policy.evaluate(&ctx("exec", &serde_json::json!({}), "cli"));

    assert_eq!(eval.decision, ToolPolicyDecision::Deny);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn deny_and_ask_outrank_allow_regardless_of_order() {
    // Regression: first-match-wins let a broad `allow` shadow a specific `deny`
    // declared after it. Deny must win no matter where it appears.
    let policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![
            rule("allow-all", "*", ToolPolicyDecision::Allow),
            rule("deny-exec", "exec", ToolPolicyDecision::Deny),
        ],
        ..Default::default()
    });
    let eval = policy.evaluate(&ctx("exec", &serde_json::json!({}), "cli"));
    assert_eq!(eval.decision, ToolPolicyDecision::Deny);
    assert_eq!(eval.matched_rule.as_deref(), Some("deny-exec"));

    // Ask also outranks Allow.
    let policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![
            rule("allow-all", "*", ToolPolicyDecision::Allow),
            rule("ask-writes", "write_file", ToolPolicyDecision::Ask),
        ],
        ..Default::default()
    });
    let eval = policy.evaluate(&ctx("write_file", &serde_json::json!({}), "cli"));
    assert_eq!(eval.decision, ToolPolicyDecision::Ask);
    assert_eq!(eval.matched_rule.as_deref(), Some("ask-writes"));
}

#[test]
fn unknown_inherited_group_fails_closed() {
    let mut inherited = rule("strict", "*", ToolPolicyDecision::Ask);
    inherited.inherit_from = Some("missing-group".to_string());
    let policy = ToolPolicy::from_config(ToolPolicyConfig {
        default_decision: ToolPolicyDecision::Allow,
        rules: vec![inherited],
        ..Default::default()
    });

    let eval = policy.evaluate(&ctx("exec", &serde_json::json!({}), "cli"));

    assert_eq!(eval.decision, ToolPolicyDecision::Deny);
}

#[test]
fn unsupported_version_fails_closed() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("blockcell-tool-policy-ver-{nonce}"));
    fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("tool_policy.yaml");
    fs::write(
        &path,
        "version: 2\nrules:\n  - name: allow-all\n    tool: '*'\n    decision: allow\n",
    )
    .expect("write versioned policy");

    let policy = ToolPolicy::load(&path);
    let eval = policy.evaluate(&ctx("exec", &serde_json::json!({}), "cli"));

    assert_eq!(eval.decision, ToolPolicyDecision::Deny);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn simulation_mode_covers_unmatched_default_decision() {
    // 无规则命中时 default_decision=Deny 在模拟模式下同样只应报告、不应真实阻断。
    let policy = ToolPolicy::from_config(ToolPolicyConfig {
        simulation_mode: true,
        default_decision: ToolPolicyDecision::Deny,
        rules: vec![],
        ..Default::default()
    });

    let eval = policy.evaluate(&ctx("exec", &serde_json::json!({}), "cli"));

    assert_eq!(eval.decision, ToolPolicyDecision::Allow);
    assert!(eval.simulated);
    assert_eq!(eval.matched_rule, None);
}
