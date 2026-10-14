use super::*;

struct PendingForkProvider {
    started: Arc<tokio::sync::Notify>,
}

struct ShellToolProvider;

#[async_trait::async_trait]
impl blockcell_providers::Provider for ShellToolProvider {
    async fn chat(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<blockcell_core::types::LLMResponse> {
        Ok(blockcell_core::types::LLMResponse {
            content: None,
            reasoning_content: None,
            tool_calls: vec![blockcell_core::types::ToolCallRequest {
                id: "shell-1".to_string(),
                name: "exec".to_string(),
                arguments: json!({"command": "sleep 1; touch cancelled-marker"}),
                thought_signature: None,
            }],
            finish_reason: "tool_calls".to_string(),
            usage: serde_json::Value::Null,
        })
    }
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for PendingForkProvider {
    async fn chat(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<blockcell_core::types::LLMResponse> {
        self.started.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn forked_agent_cancels_pending_provider() {
    let started = Arc::new(tokio::sync::Notify::new());
    let provider_pool = blockcell_providers::ProviderPool::from_single_provider(
        "test-model",
        "pending-provider",
        Arc::new(PendingForkProvider {
            started: Arc::clone(&started),
        }),
    );
    let params = ForkedAgentParams::builder()
        .provider_pool(provider_pool)
        .prompt_messages(vec![ChatMessage::user("wait")])
        .cache_safe_params(CacheSafeParams::default())
        .fork_label("cancel_test")
        .max_turns(1)
        .build()
        .expect("params should build");
    let abort_token = blockcell_core::AbortToken::new();
    let task_token = abort_token.clone();
    let task = tokio::spawn(async move {
        blockcell_core::scope_abort_token(task_token, run_forked_agent(params)).await
    });

    tokio::time::timeout(Duration::from_millis(200), started.notified())
        .await
        .expect("provider call should start");
    abort_token.cancel();

    let result = tokio::time::timeout(Duration::from_millis(200), task)
        .await
        .expect("cancellation should interrupt pending provider")
        .expect("forked task should join");
    assert!(matches!(result, Err(ForkedAgentError::Aborted(_))));
}

#[cfg(unix)]
#[tokio::test]
async fn forked_agent_cancellation_kills_running_shell_tool() {
    struct PassthroughWorkspaceSandbox;

    #[async_trait::async_trait]
    impl blockcell_tools::sandbox::SandboxProvider for PassthroughWorkspaceSandbox {
        fn name(&self) -> &'static str {
            "test-passthrough-sandbox"
        }

        async fn prepare(
            &self,
            request: blockcell_tools::sandbox::SandboxRequest,
        ) -> blockcell_core::Result<blockcell_tools::sandbox::SandboxHandle> {
            let mut command = tokio::process::Command::new(&request.program);
            command.args(&request.args);
            command.kill_on_drop(true);
            Ok(blockcell_tools::sandbox::SandboxHandle {
                command,
                backend: blockcell_tools::sandbox::SandboxBackend::MacOsSeatbelt,
            })
        }
    }

    let temp = tempfile::tempdir().expect("temp dir");
    let provider_pool = blockcell_providers::ProviderPool::from_single_provider(
        "test-model",
        "shell-provider",
        Arc::new(ShellToolProvider),
    );
    let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(8);
    let params = ForkedAgentParams::builder()
        .provider_pool(provider_pool)
        .prompt_messages(vec![ChatMessage::user("run shell")])
        .cache_safe_params(CacheSafeParams::default())
        .fork_label("shell_cancel_test")
        .working_dir(temp.path().to_path_buf())
        .event_tx(event_tx)
        .max_turns(1)
        .build()
        .expect("params should build");
    let abort_token = blockcell_core::AbortToken::new();
    let task_token = abort_token.clone();
    let executor = Arc::new(blockcell_tools::shell_executor::LocalShellExecutor::new(
        Arc::new(PassthroughWorkspaceSandbox),
    ));
    let task = tokio::spawn(async move {
        super::tool_exec::with_forked_shell_executor(
            executor,
            blockcell_core::scope_abort_token(task_token, run_forked_agent(params)),
        )
        .await
    });

    tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let event = event_rx.recv().await.expect("progress event");
            if event.contains("tool_call_start") {
                break;
            }
        }
    })
    .await
    .expect("shell tool should start");
    abort_token.cancel();

    let result = tokio::time::timeout(Duration::from_millis(200), task)
        .await
        .expect("cancellation should interrupt shell tool")
        .expect("forked task should join");
    assert!(matches!(result, Err(ForkedAgentError::Aborted(_))));

    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(
        !temp.path().join("cancelled-marker").exists(),
        "cancelled shell process must not outlive its tool future"
    );
}

#[test]
fn test_normalize_path_preserves_leading_parent_dir() {
    // "../secret" → pop() on empty fails → push ".." back → "../secret"
    let result = normalize_path_lexically(Path::new("../secret"));
    assert_eq!(result, PathBuf::from("../secret"));
}

#[test]
fn test_normalize_path_preserves_unresolvable_parent_dir() {
    // "a/../../secret" → "a" then ".." pops "a" → "" then ".." fails → "../secret"
    let result = normalize_path_lexically(Path::new("a/../../secret"));
    assert_eq!(result, PathBuf::from("../secret"));
}

#[test]
fn test_normalize_path_resolves_inner_parent_dir() {
    // "src/../lib" → "src" then ".." pops "src" → "lib"
    let result = normalize_path_lexically(Path::new("src/../lib"));
    assert_eq!(result, PathBuf::from("lib"));
}

#[test]
fn test_normalize_path_no_parent_dir() {
    let result = normalize_path_lexically(Path::new("foo/bar"));
    assert_eq!(result, PathBuf::from("foo/bar"));
}

#[test]
fn test_validate_path_safety_rejects_leading_parent() {
    assert!(validate_path_safety("../secret").is_err());
    assert!(validate_path_safety("a/../../secret").is_err());
}

#[test]
fn test_validate_path_safety_allows_resolvable_parent() {
    assert!(validate_path_safety("src/../lib").is_ok());
}

#[test]
fn test_usage_metrics() {
    let mut metrics = UsageMetrics::default();
    metrics.accumulate(1000, 500, 800, 200);

    assert_eq!(metrics.input_tokens, 1000);
    assert_eq!(metrics.output_tokens, 500);
    assert_eq!(metrics.cache_read_input_tokens, 800);
    assert_eq!(metrics.cache_creation_input_tokens, 200);

    // 缓存命中率 = 800 / (1000 + 800 + 200) = 0.4
    let hit_rate = metrics.cache_hit_rate();
    assert!((hit_rate - 0.4).abs() < 0.01);
}

#[test]
fn test_usage_metrics_merge() {
    let mut m1 = UsageMetrics {
        input_tokens: 100,
        output_tokens: 50,
        cache_read_input_tokens: 80,
        cache_creation_input_tokens: 20,
    };
    let m2 = UsageMetrics {
        input_tokens: 200,
        output_tokens: 100,
        cache_read_input_tokens: 160,
        cache_creation_input_tokens: 40,
    };

    m1.merge(&m2);

    assert_eq!(m1.input_tokens, 300);
    assert_eq!(m1.output_tokens, 150);
}

fn schema_names(schemas: &[serde_json::Value]) -> Vec<String> {
    schemas
        .iter()
        .filter_map(tool_schema_name)
        .map(str::to_string)
        .collect()
}

#[test]
fn test_insert_initial_prompt_before_first_user_message() {
    let mut messages = vec![
        ChatMessage::system("system"),
        ChatMessage::user("main task"),
    ];

    insert_initial_prompt(&mut messages, "custom first instruction");

    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[1].role, "user");
    assert_eq!(
        messages[1].content.as_str(),
        Some("custom first instruction")
    );
    assert_eq!(messages[2].content.as_str(), Some("main task"));
}

#[test]
fn test_inject_preloaded_skills_appends_skill_content_to_system_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    let skills_dir = tmp.path().join("skills");
    let skill_dir = skills_dir.join("review-flow");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("SKILL.md"), "# Review Flow\nCheck the diff.").unwrap();
    let mut messages = vec![ChatMessage::system("system")];

    inject_preloaded_skills(
        &mut messages,
        &["review-flow".to_string()],
        Some(&skills_dir),
        &[],
    );

    let prompt = messages[0].content.as_str().unwrap_or_default();
    assert!(prompt.contains("## Preloaded Skills"));
    assert!(prompt.contains("# Review Flow"));
}

#[test]
fn test_filter_tool_schemas_respects_whitelist_and_blacklist() {
    let schemas = build_forked_tool_schemas(&[]);
    let allowed = vec!["read_file".to_string(), "exec".to_string()];
    let disallowed = vec!["exec".to_string()];

    let filtered = filter_tool_schemas(&schemas, Some(&allowed), &disallowed);
    let names = schema_names(&filtered);

    assert_eq!(names, vec!["read_file".to_string()]);
}

#[test]
fn test_filter_tool_schemas_wildcard_keeps_all_except_disallowed() {
    let schemas = build_forked_tool_schemas(&[]);
    let allowed = vec!["*".to_string()];
    let disallowed = vec!["exec".to_string()];

    let filtered = filter_tool_schemas(&schemas, Some(&allowed), &disallowed);
    let names = schema_names(&filtered);

    assert!(names.contains(&"read_file".to_string()));
    assert!(names.contains(&"write_file".to_string()));
    assert!(!names.contains(&"exec".to_string()));
}

#[test]
fn default_fork_capabilities_are_structurally_read_only() {
    let disallowed = default_read_only_fork_disallowed_tools();
    for tool in [
        "exec",
        "shell",
        "edit_file",
        "file_edit",
        "write_file",
        "file_write",
    ] {
        assert!(
            disallowed.iter().any(|name| name == tool),
            "{tool} must be denied in default fork mode"
        );
    }

    let schemas = build_forked_tool_schemas(&disallowed);
    assert_eq!(
        schema_names(&schemas),
        vec![
            "read_file".to_string(),
            "list_dir".to_string(),
            "grep".to_string(),
            "glob".to_string(),
        ]
    );
}

#[test]
fn test_forked_tool_schemas_prefer_relative_paths() {
    let schemas = build_forked_tool_schemas(&[]);
    let serialized = serde_json::to_string(&schemas).unwrap();

    assert!(serialized.contains("relative path"));
    assert!(serialized.contains("working directory"));
    assert!(!serialized.contains("The absolute path to the file to read"));
    assert!(!serialized.contains("The absolute directory path to list"));
}

#[test]
fn typed_agent_schemas_expose_shell_and_keep_exec_compatibility() {
    let schemas = build_forked_tool_schemas(&[]);
    let names = schemas
        .iter()
        .filter_map(|schema| tool_schema_name(schema))
        .collect::<Vec<_>>();

    assert!(names.contains(&"shell"));
    assert!(names.contains(&"exec"));
}

#[tokio::test]
async fn test_execute_forked_write_file_creates_relative_file_in_working_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let working_dir = Some(tmp.path().to_path_buf());
    let can_use_tool: CanUseToolFn = Arc::new(|_, _| ToolPermission::Allow);

    let result = execute_forked_tool(
        "write_file",
        &json!({"file_path": "memorytest.md", "content": "temp"}),
        &can_use_tool,
        &[],
        &None,
        &None,
        &None,
        &None,
        &[],
        &None,
        &working_dir,
        &[],
    )
    .await;

    assert!(result.is_ok(), "write_file failed: {:?}", result.err());
    let written = tokio::fs::read_to_string(tmp.path().join("memorytest.md"))
        .await
        .unwrap();
    assert_eq!(written, "temp");
}

#[tokio::test]
async fn forked_write_rejects_paths_outside_workspace_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let working_dir = Some(tmp.path().to_path_buf());
    let can_use_tool: CanUseToolFn = Arc::new(|_, _| ToolPermission::Allow);
    let scope = vec![WorkspaceScopeEntry::directory(tmp.path().join("src"))];

    let result = execute_forked_tool(
        "write_file",
        &json!({"file_path": "tests/outside.rs", "content": "no"}),
        &can_use_tool,
        &[],
        &None,
        &None,
        &None,
        &None,
        &[],
        &None,
        &working_dir,
        &scope,
    )
    .await;

    let error = result.expect_err("out-of-scope write should be denied");
    assert!(error.to_string().contains("workspace_scope"));
    assert!(!tmp.path().join("tests/outside.rs").exists());
}

#[tokio::test]
async fn typed_shell_requires_approval_when_native_sandbox_is_unavailable() {
    struct UnavailableSandbox;

    #[async_trait::async_trait]
    impl blockcell_tools::sandbox::SandboxProvider for UnavailableSandbox {
        fn name(&self) -> &'static str {
            "test-unavailable-sandbox"
        }

        async fn prepare(
            &self,
            _request: blockcell_tools::sandbox::SandboxRequest,
        ) -> blockcell_core::Result<blockcell_tools::sandbox::SandboxHandle> {
            Err(blockcell_core::Error::PermissionDenied(
                "native sandbox unavailable; approval required".to_string(),
            ))
        }
    }

    let tmp = tempfile::tempdir().unwrap();
    let working_dir = Some(tmp.path().to_path_buf());
    let can_use_tool: CanUseToolFn = Arc::new(|_, _| ToolPermission::Allow);
    let scope = vec![WorkspaceScopeEntry::directory(tmp.path().join("src"))];
    let executor = Arc::new(blockcell_tools::shell_executor::LocalShellExecutor::new(
        Arc::new(UnavailableSandbox),
    ));

    let result = super::tool_exec::with_forked_shell_executor(
        executor,
        execute_forked_tool(
            "shell",
            &json!({"command": "echo should-not-run"}),
            &can_use_tool,
            &[],
            &None,
            &None,
            &None,
            &None,
            &[],
            &None,
            &working_dir,
            &scope,
        ),
    )
    .await;

    let error = result.expect_err("typed shell must not silently bypass a missing sandbox");
    assert!(error.to_string().contains("approval required"));
}

#[tokio::test]
async fn test_execute_forked_read_file_missing_is_recoverable() {
    let tmp = tempfile::tempdir().unwrap();
    let working_dir = Some(tmp.path().to_path_buf());
    let can_use_tool: CanUseToolFn = Arc::new(|_, _| ToolPermission::Allow);

    let result = execute_forked_tool(
        "read_file",
        &json!({"file_path": "memory.md"}),
        &can_use_tool,
        &[],
        &None,
        &None,
        &None,
        &None,
        &[],
        &None,
        &working_dir,
        &[],
    )
    .await;

    let message = result.expect("missing read_file should be a recoverable tool result");
    assert!(message.contains("File not found"));
    assert!(message.contains("memory.md"));
}

// 注意：ForkedAgentParams 不再实现 Default trait
// 必须通过 new() 或 builder() 创建，强制设置 provider_pool

// ========== 核心路径测试 ==========

#[test]
fn test_forked_agent_params_builder_missing_provider() {
    let result = ForkedAgentParams::builder()
        .prompt_messages(vec![ChatMessage::user("test")])
        .fork_label("test_fork")
        .max_turns(3)
        .build();

    // 没有 provider_pool，应该返回错误
    assert!(result.is_err());
    // 直接 matches! 检查，避免需要 Debug trait
    assert!(matches!(result, Err(ForkedAgentError::NoProviderAvailable)));
}

#[test]
fn test_forked_agent_params_validate_no_provider() {
    // 使用 builder 不设置 provider_pool，build() 应返回错误
    let result = ForkedAgentParams::builder()
        .prompt_messages(vec![ChatMessage::user("test")])
        .build();

    // 没有 provider_pool，应该返回错误
    assert!(result.is_err());
    assert!(matches!(result, Err(ForkedAgentError::NoProviderAvailable)));
}

#[test]
fn test_forked_agent_params_builder_methods() {
    // 测试 builder 的方法链（不调用 build，避免需要 provider_pool）
    let builder = ForkedAgentParams::builder()
        .prompt_messages(vec![ChatMessage::user("test")])
        .fork_label("custom_label")
        .query_source("custom_source")
        .max_turns(10);

    // 验证 builder 字段设置正确
    // 由于无法直接访问 builder 的私有字段，我们通过 build 后检查错误类型
    let result = builder.build();
    assert!(result.is_err());
}

#[test]
fn test_forked_agent_params_builder_requires_provider() {
    // 测试 builder 必须设置 provider_pool
    // 不设置 provider_pool 时，build() 应返回错误
    let result = ForkedAgentParams::builder()
        .prompt_messages(vec![ChatMessage::user("test")])
        .fork_label("custom_label")
        .query_source("custom_source")
        .max_turns(10)
        .build();

    // 应该失败，因为没有 provider_pool
    assert!(result.is_err());
    assert!(matches!(result, Err(ForkedAgentError::NoProviderAvailable)));
}

#[test]
fn test_forked_agent_error_variants() {
    let err = ForkedAgentError::ProviderError("test".to_string());
    assert!(err.to_string().contains("LLM provider error"));

    let err = ForkedAgentError::MaxTurnsExceeded;
    assert!(err.to_string().contains("Max turns exceeded"));

    let err = ForkedAgentError::NoProviderAvailable;
    assert!(err.to_string().contains("No provider available"));

    let err = ForkedAgentError::ToolNotSupported("bad_tool".to_string());
    assert!(err.to_string().contains("Tool not supported"));
}

#[test]
fn test_usage_metrics_cache_hit_rate_zero() {
    let metrics = UsageMetrics::default();
    assert_eq!(metrics.cache_hit_rate(), 0.0);
}

#[test]
fn test_simple_glob_match() {
    assert!(simple_glob_match("*", "anything"));
    assert!(simple_glob_match("*.rs", "main.rs"));
    assert!(simple_glob_match("test*", "testing"));
    assert!(!simple_glob_match("*.rs", "main.txt"));
    assert!(simple_glob_match("exact", "exact"));
}

#[test]
fn test_resolve_forked_path_keeps_relative_paths_inside_worktree() {
    let base = std::env::temp_dir().join("blockcell-agent-wt");
    let worktree = Some(base.clone());
    let resolved = resolve_forked_path("src/main.rs", &worktree).unwrap();
    // 验证解析路径在 worktree 内（语义检查，兼容 Windows \\?\ UNC 前缀）
    let canonical_base = resolve_to_existing_ancestor(&base).unwrap();
    assert!(
        resolved.starts_with(&canonical_base),
        "resolved '{}' should start with canonical base '{}'",
        resolved.display(),
        canonical_base.display()
    );
    // 验证路径以预期的相对分量结尾
    assert!(
        resolved.ends_with(Path::new("src").join("main.rs")),
        "resolved '{}' should end with 'src/main.rs'",
        resolved.display()
    );
}

#[test]
fn test_resolve_forked_path_keeps_nonexistent_file_parent_as_working_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let working_dir = Some(tmp.path().to_path_buf());
    let canonical_base = std::fs::canonicalize(tmp.path()).unwrap();

    let resolved = resolve_forked_path("memorytest.md", &working_dir).unwrap();

    assert_eq!(resolved, canonical_base.join("memorytest.md"));
    assert_eq!(resolved.parent(), Some(canonical_base.as_path()));
    assert!(
        !resolved
            .as_os_str()
            .to_string_lossy()
            .ends_with(std::path::MAIN_SEPARATOR),
        "resolved path must not have a trailing separator: '{}'",
        resolved.display()
    );
}

#[test]
fn test_resolve_forked_path_rejects_absolute_path_outside_worktree() {
    let temp = std::env::temp_dir();
    let worktree = Some(temp.join("blockcell-agent-wt"));
    let outside = temp
        .join("blockcell-original-workspace")
        .join("src")
        .join("main.rs");
    let err = resolve_forked_path(&outside.to_string_lossy(), &worktree)
        .expect_err("absolute path outside worktree must be rejected");
    assert!(err
        .to_string()
        .contains("outside isolated working directory"));
}
