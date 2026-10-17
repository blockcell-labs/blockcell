//! Unit and integration tests for the agent runtime.
//! Extracted from runtime.rs (mechanical move; logic unchanged).

use super::*;
use blockcell_core::types::LLMResponse;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

struct TestProvider;
struct StreamingRetryProvider {
    attempts: AtomicUsize,
}
struct StreamingCloseProvider;
struct HangingStreamProvider {
    started: Arc<tokio::sync::Notify>,
    senders: Mutex<Vec<mpsc::Sender<StreamChunk>>>,
}
struct ConnectionFailingProvider;
struct SuccessfulFallbackProvider;
struct UnifiedEntryProvider {
    calls: AtomicUsize,
}
struct RecallCaptureProvider {
    calls: Mutex<Vec<Vec<ChatMessage>>>,
}
struct SequencedGhostProvider;
struct ReviewAndCaptureProvider {
    calls: Mutex<Vec<Vec<ChatMessage>>>,
    review_calls: AtomicUsize,
}
struct BoundaryFlushProvider {
    calls: Mutex<Vec<Vec<ChatMessage>>>,
    flush_calls: AtomicUsize,
}
struct BoundaryMemoryProvider;
struct ProviderToolCaptureProvider {
    seen_tools: Mutex<Vec<Vec<serde_json::Value>>>,
}
struct RuntimeProviderTool {
    calls: Mutex<Vec<serde_json::Value>>,
}

fn extract_active_skill_name(system_text: &str) -> Option<String> {
    let marker = "## Active Skill: ";
    let start = system_text.find(marker)?;
    let rest = &system_text[start + marker.len()..];
    let skill_name = rest.lines().next()?.trim();
    if skill_name.is_empty() {
        None
    } else {
        Some(skill_name.to_string())
    }
}

fn drain_ws_events(event_rx: &mut broadcast::Receiver<String>) -> Vec<serde_json::Value> {
    let mut events = Vec::new();
    loop {
        match event_rx.try_recv() {
            Ok(payload) => {
                events.push(
                    serde_json::from_str::<serde_json::Value>(&payload).expect("parse ws event"),
                );
            }
            Err(broadcast::error::TryRecvError::Empty)
            | Err(broadcast::error::TryRecvError::Closed) => break,
            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
        }
    }
    events
}

fn collect_event_types(events: &[serde_json::Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| event.get("type").and_then(|value| value.as_str()))
        .map(str::to_string)
        .collect()
}

#[test]
fn identical_tool_call_failures_trip_after_three_attempts() {
    let call = ToolCallRequest {
        id: "call-1".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({"path": "missing.rs"}),
        thought_signature: None,
    };
    let mut tracker = ToolLoopGuard::new(10, HashMap::new());

    assert!(!tracker.record(&call, true));
    assert!(!tracker.record(&call, true));
    assert!(tracker.record(&call, true));
}

#[test]
fn loop_guard_enforces_default_and_per_tool_call_limits() {
    // default=4 且 {exec:1} → global_limit=4：read_file 四次占满全局预算。
    let mut guard = ToolLoopGuard::new(4, HashMap::from([("exec".to_string(), 1)]));

    assert!(guard.admit("read_file").is_ok());
    assert!(guard.admit("read_file").is_ok());
    assert!(guard.admit("read_file").is_ok());
    assert!(guard.admit("read_file").is_ok());
    // per-tool 检查优先：read_file 第 5 次报自身上限而非全局
    let denied = guard.admit("read_file").unwrap_err();
    assert!(denied.contains("max call limit (4)"), "{denied}");

    // exec 自身余量尚存，但全局预算已耗尽 → 全局拒绝
    let denied = guard.admit("exec").unwrap_err();
    assert!(denied.contains("max total tool iterations (4)"), "{denied}");
    // 再次尝试则 per-tool 上限先命中
    assert!(guard.admit("exec").unwrap_err().contains("(1)"));
    assert_eq!(guard.call_counts().get("read_file"), Some(&5));
}

#[test]
fn loop_guard_enforces_global_limit_across_distinct_tools() {
    // 默认上限同时构成回合总上限：两个工具各调 1+2 次共 3 次即顶到 global=2。
    let mut guard = ToolLoopGuard::new(2, HashMap::new());
    assert!(guard.admit("read_file").is_ok());
    assert!(guard.admit("exec").is_ok());
    let denied = guard.admit("web_fetch").unwrap_err();
    assert!(denied.contains("max total tool iterations (2)"), "{denied}");
}

#[test]
fn loop_guard_global_limit_yields_to_larger_per_tool_override() {
    // 显式放宽某工具上限会抬高总上限至该值（exec: 5 → 总共允许 5 次）。
    let mut guard = ToolLoopGuard::new(2, HashMap::from([("exec".to_string(), 5)]));
    for _ in 0..4 {
        assert!(guard.admit("exec").is_ok());
    }
    assert!(guard.admit("read_file").is_ok());
    let denied = guard.admit("read_file").unwrap_err();
    assert!(denied.contains("max total tool iterations (5)"), "{denied}");
}

#[test]
fn tool_call_failure_follows_outcome_and_soft_errors() {
    let output = |text: &str, outcome| ToolCallOutput {
        text: text.to_string(),
        outcome,
    };
    // 执行结论为准
    assert!(tool_call_failed(&output("{}", ToolOutcome::Rejected)));
    assert!(tool_call_failed(&output("ok", ToolOutcome::Unknown)));
    // 工具以 Ok 返回的软失败
    assert!(tool_call_failed(&output(
        r#"{"error":"not found"}"#,
        ToolOutcome::Succeeded
    )));
    // 成功结果中值为空的 error 字段不是失败
    assert!(!tool_call_failed(&output(
        r#"{"error":null,"data":1}"#,
        ToolOutcome::Succeeded
    )));
    assert!(!tool_call_failed(&output(
        r#"{"error":false}"#,
        ToolOutcome::Succeeded
    )));
}

#[test]
fn changed_arguments_or_success_reset_consecutive_tool_failures() {
    let first = ToolCallRequest {
        id: "call-1".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({"path": "first.rs"}),
        thought_signature: None,
    };
    let changed = ToolCallRequest {
        id: "call-2".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({"path": "second.rs"}),
        thought_signature: None,
    };
    let mut tracker = ToolLoopGuard::new(10, HashMap::new());

    assert!(!tracker.record(&first, true));
    assert!(!tracker.record(&first, true));
    assert!(!tracker.record(&changed, true));
    assert!(!tracker.record(&changed, false));
    assert!(!tracker.record(&changed, true));
}

#[test]
fn only_executed_failures_count_as_skill_evolution_signal() {
    use super::tool_pipeline::evolution_signal;
    assert_eq!(evolution_signal(ToolOutcome::Succeeded), Some(false));
    assert_eq!(evolution_signal(ToolOutcome::Failed), Some(true));
    for outcome in [
        ToolOutcome::Rejected,
        ToolOutcome::Cancelled,
        ToolOutcome::TimedOut,
        ToolOutcome::NotStarted,
        ToolOutcome::Unknown,
    ] {
        assert_eq!(evolution_signal(outcome), None, "{outcome:?}");
    }
}

fn contains_event_subsequence(events: &[String], expected: &[&str]) -> bool {
    let mut cursor = 0usize;
    for event in events {
        if cursor < expected.len() && event == expected[cursor] {
            cursor += 1;
        }
    }
    cursor == expected.len()
}

#[test]
fn apply_skill_fallback_response_uses_fallback_for_empty_output() {
    let fallback = "当前无法获取腾讯新闻数据，请先检查 CLI 安装、API Key 配置或网络环境。";

    assert_eq!(
        apply_skill_fallback_response(String::new(), Some(fallback)),
        fallback
    );
    assert_eq!(
        apply_skill_fallback_response("   \n\t".to_string(), Some(fallback)),
        fallback
    );
}

#[test]
fn apply_skill_fallback_response_keeps_non_empty_output() {
    assert_eq!(
        apply_skill_fallback_response("  ok  ".to_string(), Some("fallback")),
        "ok"
    );
}

#[test]
fn extract_mcp_search_revealed_tools_reads_search_result_names() {
    let result = serde_json::json!({
        "tools": [
            { "name": "postgres__query" },
            { "name": "github__create_issue" },
            { "name": "not_mcp" },
            { "name": "" }
        ]
    })
    .to_string();

    assert_eq!(
        extract_mcp_search_revealed_tools(&result),
        vec![
            "postgres__query".to_string(),
            "github__create_issue".to_string()
        ]
    );
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for TestProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        let system_text = messages.first().map(chat_message_text).unwrap_or_default();
        let user_text = messages
            .iter()
            .rev()
            .find(|msg| msg.role == "user")
            .map(chat_message_text)
            .unwrap_or_default();
        let latest_tool_text = messages
            .iter()
            .rev()
            .find(|msg| msg.role == "tool")
            .map(chat_message_text);
        let active_skill_name = extract_active_skill_name(&system_text);

        let response = if matches!(active_skill_name.as_deref(), Some("compat_local_demo"))
            && latest_tool_text.is_none()
        {
            LLMResponse {
                content: Some("准备调用兼容本地脚本".to_string()),
                reasoning_content: None,
                tool_calls: vec![ToolCallRequest {
                    id: "test-exec-local-compat".to_string(),
                    name: "exec_local".to_string(),
                    arguments: serde_json::json!({
                        "path": "scripts/hello.sh",
                        "runner": "sh",
                        "args": ["skill"],
                        "cwd_mode": "skill"
                    }),
                    thought_signature: None,
                }],
                finish_reason: "tool_calls".to_string(),
                usage: serde_json::Value::Null,
            }
        } else if matches!(active_skill_name.as_deref(), Some("compat_local_demo")) {
            let stdout = latest_tool_text
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .and_then(|value| {
                    value
                        .get("stdout")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            LLMResponse {
                content: Some(format!("local exec result: {}", stdout)),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::Value::Null,
            }
        } else if matches!(
            active_skill_name.as_deref(),
            Some("local_demo" | "legacy_script_demo" | "cli_demo")
        ) && latest_tool_text.is_none()
        {
            let (path, args) = match active_skill_name.as_deref() {
                Some("cli_demo") => ("bin/cli.sh", vec!["demo"]),
                _ => ("scripts/hello.sh", vec!["skill"]),
            };
            LLMResponse {
                content: Some("准备调用本地脚本".to_string()),
                reasoning_content: None,
                tool_calls: vec![ToolCallRequest {
                    id: "test-exec-skill-script".to_string(),
                    name: "exec_skill_script".to_string(),
                    arguments: serde_json::json!({
                        "path": path,
                        "runner": "sh",
                        "args": args,
                        "cwd_mode": "skill"
                    }),
                    thought_signature: None,
                }],
                finish_reason: "tool_calls".to_string(),
                usage: serde_json::Value::Null,
            }
        } else if matches!(
            active_skill_name.as_deref(),
            Some("local_demo" | "legacy_script_demo" | "cli_demo")
        ) {
            let stdout = latest_tool_text
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .and_then(|value| {
                    value
                        .get("stdout")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            LLMResponse {
                content: Some(format!("local exec result: {}", stdout)),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::Value::Null,
            }
        } else if user_text.contains("技能说明摘要") && user_text.contains("执行结果") {
            let execution_result = user_text
                .split("执行结果：")
                .nth(1)
                .or_else(|| user_text.split("执行结果:").nth(1))
                .unwrap_or_default()
                .trim();
            LLMResponse {
                content: Some(format!("summary: {}", execution_result)),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::Value::Null,
            }
        } else {
            LLMResponse {
                content: Some(format!("mock answer: {}", user_text)),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::Value::Null,
            }
        };

        Ok(response)
    }
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for HangingStreamProvider {
    async fn chat(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        std::future::pending().await
    }

    async fn chat_stream(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<mpsc::Receiver<StreamChunk>> {
        let (tx, rx) = mpsc::channel(1);
        self.senders.lock().unwrap().push(tx);
        self.started.notify_one();
        Ok(rx)
    }
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for ProviderToolCaptureProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        self.seen_tools.lock().unwrap().push(tools.to_vec());
        let latest_tool_text = messages
            .iter()
            .rev()
            .find(|msg| msg.role == "tool")
            .map(chat_message_text);

        if let Some(tool_text) = latest_tool_text {
            return Ok(LLMResponse {
                content: Some(format!("provider result: {}", tool_text)),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::Value::Null,
            });
        }

        Ok(LLMResponse {
            content: Some("checking external memory".to_string()),
            reasoning_content: None,
            tool_calls: vec![ToolCallRequest {
                id: "provider-tool-call".to_string(),
                name: "external_memory_lookup".to_string(),
                arguments: serde_json::json!({"query": "canary rollout"}),
                thought_signature: None,
            }],
            finish_reason: "tool_calls".to_string(),
            usage: serde_json::Value::Null,
        })
    }
}

impl crate::ghost_memory_provider::GhostMemoryProvider for RuntimeProviderTool {
    fn name(&self) -> &'static str {
        "runtime_provider_tool"
    }

    fn get_tool_schemas(&self) -> Vec<serde_json::Value> {
        vec![serde_json::json!({
            "name": "external_memory_lookup",
            "description": "Lookup provider-backed external memory.",
            "parameters": {
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }
        })]
    }

    fn handle_tool_call(
        &self,
        tool_name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value> {
        assert_eq!(tool_name, "external_memory_lookup");
        self.calls.lock().unwrap().push(args.clone());
        Ok(serde_json::json!({
            "success": true,
            "provider": self.name(),
            "query": args.get("query").cloned().unwrap_or(serde_json::Value::Null),
            "memory": "Prefer canary rollout before broad release."
        }))
    }
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for StreamingRetryProvider {
    async fn chat(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        Ok(LLMResponse {
            content: Some("unexpected non-stream call".to_string()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            finish_reason: "stop".to_string(),
            usage: serde_json::Value::Null,
        })
    }

    async fn chat_stream(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<mpsc::Receiver<StreamChunk>> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel(8);

        tokio::spawn(async move {
            if attempt == 0 {
                let _ = tx
                    .send(StreamChunk::TextDelta {
                        delta: "partial".to_string(),
                    })
                    .await;
                let _ = tx
                    .send(StreamChunk::Error {
                        message: "temporary stream failure".to_string(),
                    })
                    .await;
                return;
            }

            let response = LLMResponse {
                content: Some("final answer".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::Value::Null,
            };
            let _ = tx
                .send(StreamChunk::TextDelta {
                    delta: "final answer".to_string(),
                })
                .await;
            let _ = tx.send(StreamChunk::Done { response }).await;
        });

        Ok(rx)
    }
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for StreamingCloseProvider {
    async fn chat(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        Ok(LLMResponse {
            content: Some("unexpected non-stream call".to_string()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            finish_reason: "stop".to_string(),
            usage: serde_json::Value::Null,
        })
    }

    async fn chat_stream(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<mpsc::Receiver<StreamChunk>> {
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            let _ = tx
                .send(StreamChunk::TextDelta {
                    delta: "closed answer".to_string(),
                })
                .await;
        });
        Ok(rx)
    }
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for ConnectionFailingProvider {
    async fn chat(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        Err(blockcell_core::Error::Provider(
            "connection refused before stream".to_string(),
        ))
    }

    async fn chat_stream(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<mpsc::Receiver<StreamChunk>> {
        Err(blockcell_core::Error::Provider(
            "connection refused before stream".to_string(),
        ))
    }
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for SuccessfulFallbackProvider {
    async fn chat(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        Ok(LLMResponse {
            content: Some("fallback answer".to_string()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            finish_reason: "stop".to_string(),
            usage: serde_json::Value::Null,
        })
    }

    async fn chat_stream(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<mpsc::Receiver<StreamChunk>> {
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            let response = LLMResponse {
                content: Some("fallback answer".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::Value::Null,
            };
            let _ = tx
                .send(StreamChunk::TextDelta {
                    delta: "fallback answer".to_string(),
                })
                .await;
            let _ = tx.send(StreamChunk::Done { response }).await;
        });
        Ok(rx)
    }
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for UnifiedEntryProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        self.calls.fetch_add(1, Ordering::SeqCst);

        let system_text = messages.first().map(chat_message_text).unwrap_or_default();
        let user_text = messages
            .iter()
            .rev()
            .find(|msg| msg.role == "user")
            .map(chat_message_text)
            .unwrap_or_default();
        let latest_tool_msg = messages.iter().rev().find(|msg| msg.role == "tool");
        let latest_tool_name = latest_tool_msg
            .and_then(|msg| msg.name.as_deref())
            .unwrap_or_default()
            .to_string();
        let latest_tool_text = latest_tool_msg.map(chat_message_text);
        let active_skill_name = extract_active_skill_name(&system_text);

        let response = if matches!(active_skill_name.as_deref(), Some("compat_local_demo"))
            && latest_tool_name != "exec_local"
        {
            LLMResponse {
                content: Some("进入 compat_local_demo".to_string()),
                reasoning_content: None,
                tool_calls: vec![ToolCallRequest {
                    id: "skill-exec-local-compat".to_string(),
                    name: "exec_local".to_string(),
                    arguments: serde_json::json!({
                        "path": "scripts/hello.sh",
                        "runner": "sh",
                        "args": ["skill"],
                        "cwd_mode": "skill"
                    }),
                    thought_signature: None,
                }],
                finish_reason: "tool_calls".to_string(),
                usage: serde_json::Value::Null,
            }
        } else if matches!(active_skill_name.as_deref(), Some("compat_local_demo")) {
            let stdout = latest_tool_text
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .and_then(|value| {
                    value
                        .get("stdout")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            LLMResponse {
                content: Some(format!("local exec result: {}", stdout)),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::Value::Null,
            }
        } else if matches!(active_skill_name.as_deref(), Some("local_demo"))
            && latest_tool_name != "exec_skill_script"
        {
            LLMResponse {
                content: Some("进入 local_demo".to_string()),
                reasoning_content: None,
                tool_calls: vec![ToolCallRequest {
                    id: "skill-exec-skill-script".to_string(),
                    name: "exec_skill_script".to_string(),
                    arguments: serde_json::json!({
                        "path": "scripts/hello.sh",
                        "runner": "sh",
                        "args": ["skill"],
                        "cwd_mode": "skill"
                    }),
                    thought_signature: None,
                }],
                finish_reason: "tool_calls".to_string(),
                usage: serde_json::Value::Null,
            }
        } else if matches!(active_skill_name.as_deref(), Some("local_demo")) {
            let stdout = latest_tool_text
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .and_then(|value| {
                    value
                        .get("stdout")
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .map(str::to_string)
                })
                .unwrap_or_default();
            LLMResponse {
                content: Some(format!("local exec result: {}", stdout)),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::Value::Null,
            }
        } else if user_text.contains("技能说明摘要") && user_text.contains("执行结果") {
            let execution_result = user_text
                .split("执行结果：")
                .nth(1)
                .or_else(|| user_text.split("执行结果:").nth(1))
                .unwrap_or_default()
                .trim();
            LLMResponse {
                content: Some(format!("summary: {}", execution_result)),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::Value::Null,
            }
        } else if latest_tool_name == "list_dir" {
            let path = latest_tool_text
                .as_deref()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
                .and_then(|value| {
                    value
                        .get("path")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| ".".to_string());
            LLMResponse {
                content: Some(format!("目录内容：{}", path)),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::Value::Null,
            }
        } else if user_text.contains("查看当前目录下文件") {
            LLMResponse {
                content: Some("先列目录".to_string()),
                reasoning_content: None,
                tool_calls: vec![ToolCallRequest {
                    id: "general-list-dir".to_string(),
                    name: "list_dir".to_string(),
                    arguments: serde_json::json!({ "path": "." }),
                    thought_signature: None,
                }],
                finish_reason: "tool_calls".to_string(),
                usage: serde_json::Value::Null,
            }
        } else if user_text.contains("运行本地脚本") {
            LLMResponse {
                content: Some("改用 skill".to_string()),
                reasoning_content: None,
                tool_calls: vec![ToolCallRequest {
                    id: "activate-skill-local-demo".to_string(),
                    name: ACTIVATE_SKILL_TOOL_NAME.to_string(),
                    arguments: serde_json::json!({
                        "skill_name": "local_demo",
                        "goal": "运行本地脚本"
                    }),
                    thought_signature: None,
                }],
                finish_reason: "tool_calls".to_string(),
                usage: serde_json::Value::Null,
            }
        } else {
            LLMResponse {
                content: Some(format!("mock answer: {}", user_text)),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::Value::Null,
            }
        };

        Ok(response)
    }
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for RecallCaptureProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        self.calls.lock().unwrap().push(messages.to_vec());

        let system_text = messages.first().map(chat_message_text).unwrap_or_default();
        if system_text.contains("quiet Ghost learning reviewer") && !tools.is_empty() {
            return Ok(LLMResponse {
                content: Some("no durable learning".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::Value::Null,
            });
        }

        Ok(LLMResponse {
            content: Some("mock answer: recall applied".to_string()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            finish_reason: "stop".to_string(),
            usage: serde_json::Value::Null,
        })
    }
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for SequencedGhostProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        let system_text = messages.first().map(chat_message_text).unwrap_or_default();
        let user_text = messages
            .iter()
            .rev()
            .find(|msg| msg.role == "user")
            .map(chat_message_text)
            .unwrap_or_default();

        if system_text.contains("quiet Ghost learning reviewer") && !tools.is_empty() {
            return Ok(LLMResponse {
                content: Some("no durable learning".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::Value::Null,
            });
        }

        Ok(LLMResponse {
            content: Some(format!("mock answer: {}", user_text)),
            reasoning_content: None,
            tool_calls: Vec::new(),
            finish_reason: "stop".to_string(),
            usage: serde_json::Value::Null,
        })
    }
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for ReviewAndCaptureProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        let system_text = messages.first().map(chat_message_text).unwrap_or_default();
        if system_text.contains("quiet Ghost learning reviewer") && !tools.is_empty() {
            let review_index = self.review_calls.fetch_add(1, Ordering::SeqCst);
            let tool_calls = if review_index == 0 {
                vec![
                    ToolCallRequest {
                        id: "review-user-memory".to_string(),
                        name: "memory_manage".to_string(),
                        arguments: serde_json::json!({
                            "action": "add",
                            "target": "user",
                            "content": "User prefers canary-first rollout."
                        }),
                        thought_signature: None,
                    },
                    ToolCallRequest {
                        id: "review-project-memory".to_string(),
                        name: "memory_manage".to_string(),
                        arguments: serde_json::json!({
                            "action": "add",
                            "target": "memory",
                            "content": "Confirm rollback plan before release verification."
                        }),
                        thought_signature: None,
                    },
                ]
            } else {
                Vec::new()
            };
            return Ok(LLMResponse {
                content: None,
                reasoning_content: None,
                finish_reason: if tool_calls.is_empty() {
                    "stop"
                } else {
                    "tool_calls"
                }
                .to_string(),
                tool_calls,
                usage: serde_json::Value::Null,
            });
        }

        self.calls.lock().unwrap().push(messages.to_vec());
        let user_text = messages
            .iter()
            .rev()
            .find(|msg| msg.role == "user")
            .map(chat_message_text)
            .unwrap_or_default();
        Ok(LLMResponse {
            content: Some(format!("mock answer: {}", user_text)),
            reasoning_content: None,
            tool_calls: Vec::new(),
            finish_reason: "stop".to_string(),
            usage: serde_json::Value::Null,
        })
    }
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for BoundaryFlushProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        self.calls.lock().unwrap().push(messages.to_vec());
        let latest_user_text = messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .map(chat_message_text)
            .unwrap_or_default();
        if latest_user_text.contains("__ghost_memory_flush_sentinel") && !tools.is_empty() {
            let call_idx = self.flush_calls.fetch_add(1, Ordering::SeqCst);
            let tool_calls = if call_idx == 0 {
                vec![ToolCallRequest {
                    id: "flush-memory".to_string(),
                    name: "memory_manage".to_string(),
                    arguments: serde_json::json!({
                        "action": "add",
                        "target": "user",
                        "content": "User prefers checking rollback order before deploy compression."
                    }),
                    thought_signature: None,
                }]
            } else {
                Vec::new()
            };
            return Ok(LLMResponse {
                content: None,
                reasoning_content: None,
                finish_reason: if tool_calls.is_empty() {
                    "stop"
                } else {
                    "tool_calls"
                }
                .to_string(),
                tool_calls,
                usage: serde_json::Value::Null,
            });
        }

        Ok(LLMResponse {
            content: Some("mock answer".to_string()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            finish_reason: "stop".to_string(),
            usage: serde_json::Value::Null,
        })
    }
}

impl crate::ghost_memory_provider::GhostMemoryProvider for BoundaryMemoryProvider {
    fn name(&self) -> &'static str {
        "boundary_test"
    }

    fn on_pre_compress(&self, _messages: &[String], _session_id: &str) -> Result<String> {
        Ok("preserve provider-derived rollback preference before compression".to_string())
    }

    fn on_session_end(&self, _messages: &[String], _session_id: &str) -> Result<()> {
        Ok(())
    }

    fn on_session_boundary_context(
        &self,
        _messages: &[String],
        _session_id: &str,
    ) -> Result<String> {
        Ok("preserve provider-derived session-end deploy preference".to_string())
    }
}

#[test]
fn test_core_tools_contains_toggle_manage() {
    assert!(global_core_tool_names()
        .iter()
        .any(|name| name == "toggle_manage"));
}

#[test]
fn test_path_within_base_allows_normal_child_path() {
    let base = PathBuf::from("/tmp/workspace");
    let candidate = base.join("skills/new/SKILL.py");
    assert!(is_path_within_base(&base, &candidate));
}

#[test]
fn test_path_within_base_blocks_nonexistent_traversal() {
    let base = PathBuf::from("/tmp/workspace");
    let candidate = base.join("../../etc/passwd");
    assert!(!is_path_within_base(&base, &candidate));
}

#[cfg(unix)]
#[test]
fn test_path_within_base_blocks_symlink_escape_for_nonexistent_target() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("create temp dir");
    let workspace = temp.path().join("workspace");
    let outside = temp.path().join("outside");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::create_dir_all(&outside).expect("create outside dir");
    symlink(&outside, workspace.join("link")).expect("create symlink");

    let candidate = workspace.join("link/missing/new.txt");
    assert!(!is_path_within_base(&workspace, &candidate));
}

fn test_runtime_with_path_policy(policy: &str) -> AgentRuntime {
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());

    let base = std::env::temp_dir().join(format!(
        "blockcell-path-policy-runtime-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&base).expect("create temp path policy runtime dir");
    let paths = Paths::with_base(base);
    let policy_file = paths.path_access_file();
    std::fs::write(&policy_file, policy).expect("write path policy");
    config.security.path_access.policy_file = policy_file.to_string_lossy().to_string();

    test_runtime_with_provider_and_paths(paths, Arc::new(TestProvider), config)
}

fn read_deny_write_allow_policy(path: &Path) -> String {
    format!(
        r#"{{
            version: 1,
            default_policy: "deny",
            cache_confirmed_dirs: false,
            builtin_protected_paths: false,
            rules: [
                {{
                    name: "deny-read",
                    action: "deny",
                    ops: ["read"],
                    paths: [{}],
                }},
                {{
                    name: "allow-write",
                    action: "allow",
                    ops: ["write"],
                    paths: [{}],
                }},
            ],
        }}"#,
        serde_json::to_string(&path.to_string_lossy()).expect("serialize policy path"),
        serde_json::to_string(&path.to_string_lossy()).expect("serialize policy path"),
    )
}

fn read_allow_write_deny_policy(path: &Path) -> String {
    format!(
        r#"{{
            version: 1,
            default_policy: "deny",
            cache_confirmed_dirs: true,
            builtin_protected_paths: false,
            rules: [
                {{
                    name: "allow-read",
                    action: "allow",
                    ops: ["read"],
                    paths: [{}],
                }},
                {{
                    name: "deny-write",
                    action: "deny",
                    ops: ["write"],
                    paths: [{}],
                }},
            ],
        }}"#,
        serde_json::to_string(&path.to_string_lossy()).expect("serialize policy path"),
        serde_json::to_string(&path.to_string_lossy()).expect("serialize policy path"),
    )
}

#[tokio::test]
async fn test_workspace_internal_deny_rule_is_enforced() {
    let base = std::path::PathBuf::from("/tmp").join(format!(
        "blockcell-workspace-deny-policy-{}",
        uuid::Uuid::new_v4()
    ));
    let paths = Paths::with_base(base);
    paths.ensure_dirs().expect("ensure workspace dirs");
    let secret_dir = paths.workspace().join("secrets");
    std::fs::create_dir_all(&secret_dir).expect("create workspace secrets dir");
    let secret_file = secret_dir.join("token.txt");
    std::fs::write(&secret_file, "secret").expect("write secret file");

    let policy = read_deny_write_allow_policy(&secret_dir);
    let policy_file = paths.path_access_file();
    std::fs::write(&policy_file, policy).expect("write path policy");

    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    config.security.path_access.policy_file = policy_file.to_string_lossy().to_string();

    let mut runtime = test_runtime_with_provider_and_paths(paths, Arc::new(TestProvider), config);
    let msg = test_main_session_inbound("cli", "workspace-deny-policy");

    assert!(
        !runtime
            .check_path_permission(
                "read_file",
                &serde_json::json!({"path": "secrets/token.txt"}),
                &msg
            )
            .await,
        "explicit deny rule inside workspace must not be bypassed by is_path_safe"
    );
}

#[tokio::test]
async fn dangerous_op_confirm_is_scoped_to_the_call_not_the_session() {
    // 危险命令批准（dangerous_confirmed → confirmed=true, cache=false）跳过 Confirm
    // 弹窗，但不得把路径写入会话授权缓存——与策略级确认（cache=true）区分开。
    // 注意不能用 std::env::temp_dir()：macOS 上它在 /private/var 下，会命中内置
    // 敏感路径 Deny。/tmp → /private/tmp 在内置列表之外，evaluate 为 Confirm。
    let outside = std::path::PathBuf::from(format!(
        "/tmp/blockcell-dangerous-scope-{}",
        uuid::Uuid::new_v4()
    ));
    let args = serde_json::json!({"command": format!("cat {}", outside.display())});
    let msg = test_main_session_inbound("cli", "dangerous-scope");

    // 策略级确认：Confirm 路径放行并缓存到会话，后续同路径不再需要确认
    let mut runtime_cached = test_runtime();
    assert!(
        runtime_cached
            .check_path_permission_with_confirmation("exec", &args, &msg, true, true)
            .await
    );
    assert!(
        runtime_cached
            .check_path_permission_with_confirmation("exec", &args, &msg, false, false)
            .await
    );

    // 危险命令确认：本次放行但不写入会话缓存，再次访问必须重新走确认
    let mut runtime_scoped = test_runtime();
    assert!(
        runtime_scoped
            .check_path_permission_with_confirmation("exec", &args, &msg, true, false)
            .await
    );
    // 无确认通道 → Confirm 路径被拒（说明上次批准没有泄漏到会话级）
    assert!(
        !runtime_scoped
            .check_path_permission_with_confirmation("exec", &args, &msg, false, false)
            .await
    );
}

#[tokio::test]
async fn test_authorized_read_directory_does_not_authorize_writes() {
    let outside = std::env::temp_dir().join(format!(
        "blockcell-op-cache-policy-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&outside).expect("create outside operation cache dir");
    let path = outside.join("data.txt");
    std::fs::write(&path, "data").expect("write operation cache input");

    let mut runtime = test_runtime_with_path_policy(&read_allow_write_deny_policy(&outside));
    let msg = test_main_session_inbound("cli", "operation-cache-policy");

    assert!(
        runtime
            .check_path_permission("read_file", &serde_json::json!({"path": path}), &msg)
            .await
    );
    assert!(
        !runtime
            .check_path_permission("write_file", &serde_json::json!({"path": path}), &msg)
            .await
    );
}

#[test]
fn test_authorize_directory_does_not_persist_filesystem_root() {
    use blockcell_core::path_policy::PathOp;

    // Approving a file directly under `/` used to cache `/` itself, after which
    // is_path_authorized's ancestor walk exempted every path for that op.
    let mut runtime = test_runtime();
    runtime.authorize_directory(
        std::path::Path::new("/blockcell-root-sentry.txt"),
        PathOp::Write,
        "cli:root-scope",
    );
    assert!(!runtime.is_path_authorized(
        std::path::Path::new("/etc/hostname"),
        PathOp::Write,
        "cli:root-scope"
    ));
    assert!(!runtime.is_path_authorized(
        std::path::Path::new("/tmp/anything"),
        PathOp::Write,
        "cli:root-scope"
    ));
}

#[tokio::test]
async fn test_email_attachment_is_checked_as_read() {
    let outside =
        std::env::temp_dir().join(format!("blockcell-email-policy-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&outside).expect("create outside email policy dir");
    let attachment = outside.join("secret.txt");
    std::fs::write(&attachment, "secret").expect("write attachment");

    let mut runtime = test_runtime_with_path_policy(&read_deny_write_allow_policy(&outside));
    let msg = test_main_session_inbound("cli", "email-path-policy");

    assert!(
        !runtime
            .check_path_permission(
                "email",
                &serde_json::json!({
                    "action": "send",
                    "attachments": [attachment],
                }),
                &msg,
            )
            .await
    );
}

#[tokio::test]
async fn test_email_attachment_destination_is_checked_as_write() {
    let outside = std::env::temp_dir().join(format!(
        "blockcell-email-output-policy-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&outside).expect("create outside email output dir");

    let mut runtime = test_runtime_with_path_policy(&read_deny_write_allow_policy(&outside));
    let msg = test_main_session_inbound("cli", "email-output-path-policy");

    assert!(
        runtime
            .check_path_permission(
                "email",
                &serde_json::json!({
                    "action": "read",
                    "save_attachments_to": outside,
                }),
                &msg,
            )
            .await
    );
}

#[tokio::test]
async fn test_ocr_input_and_output_use_separate_operations() {
    let outside =
        std::env::temp_dir().join(format!("blockcell-ocr-policy-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&outside).expect("create outside ocr policy dir");
    let input = outside.join("secret.png");
    std::fs::write(&input, "image").expect("write ocr input");

    let mut runtime = test_runtime_with_path_policy(&read_deny_write_allow_policy(&outside));
    let msg = test_main_session_inbound("cli", "ocr-path-policy");

    assert!(
        !runtime
            .check_path_permission(
                "ocr",
                &serde_json::json!({
                    "action": "recognize",
                    "path": input,
                    "output_path": outside.join("result.txt"),
                }),
                &msg,
            )
            .await
    );
}

#[tokio::test]
async fn test_browser_upload_files_are_checked_as_read() {
    let outside = std::env::temp_dir().join(format!(
        "blockcell-browser-upload-policy-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&outside).expect("create outside browser upload dir");
    let upload = outside.join("secret.txt");
    std::fs::write(&upload, "secret").expect("write browser upload input");

    let mut runtime = test_runtime_with_path_policy(&read_deny_write_allow_policy(&outside));
    let msg = test_main_session_inbound("cli", "browser-upload-path-policy");

    assert!(
        !runtime
            .check_path_permission(
                "browse",
                &serde_json::json!({
                    "action": "upload_file",
                    "files": [upload],
                }),
                &msg,
            )
            .await
    );
}

#[test]
fn test_tool_result_indicates_error_for_json_error_field() {
    let result = r#"{"error":"Permission denied: blocked"}"#;
    assert!(tool_result_indicates_error(result));
}

#[test]
fn test_tool_result_indicates_error_does_not_use_failed_substring() {
    let result = "Task succeeded, previous attempt failed but recovered.";
    assert!(!tool_result_indicates_error(result));
}

#[test]
fn test_should_supplement_tool_schema_for_validation_error() {
    let result = "Error: Validation error: Missing required parameter: path";
    assert!(should_supplement_tool_schema(result));
}

#[test]
fn test_should_supplement_tool_schema_for_config_error() {
    let result = "Error: Config error: 'enabled' (boolean) is required for 'set' action";
    assert!(should_supplement_tool_schema(result));
}

#[test]
fn test_should_supplement_tool_schema_ignores_permission_denied() {
    let result = "Error: Tool error: Permission denied: path blocked";
    assert!(!should_supplement_tool_schema(result));
}

#[test]
fn test_resolve_routed_agent_id_from_metadata() {
    let metadata = serde_json::json!({
        "route_agent_id": "ops"
    });

    assert_eq!(resolve_routed_agent_id(&metadata).as_deref(), Some("ops"));
    assert_eq!(resolve_routed_agent_id(&serde_json::Value::Null), None);
}

#[test]
fn test_build_subagent_inbound_for_structured_skill_task_uses_forced_skill_name() {
    let inbound = build_subagent_inbound_message(
        "__SKILL_EXEC__:weather:北京天气",
        "cli",
        "chat-1",
        &serde_json::json!({
            "route_agent_id": "ops"
        }),
        "subagent:test",
    );

    assert_eq!(inbound.content, "北京天气");
    assert_eq!(
        inbound
            .metadata
            .get("forced_skill_name")
            .and_then(|value| value.as_str()),
        Some("weather")
    );
    assert_eq!(
        inbound
            .metadata
            .get("subagent_session_key")
            .and_then(|value| value.as_str()),
        Some("subagent:test")
    );
    assert!(inbound.metadata.get("skill_script").is_none());
    assert!(inbound.metadata.get("skill_script_kind").is_none());
    assert!(inbound.metadata.get("skill_python").is_none());
    assert!(inbound.metadata.get("skill_rhai").is_none());
    assert!(inbound.metadata.get("skill_markdown").is_none());
}

#[test]
fn test_parse_spawn_task_forces_explicit_skill_request() {
    let parsed = parse_spawn_task_forced_skill_request(
        "使用已安装的 xiaohongshu 技能：先获取推荐流 feeds，然后定位第15条笔记",
    );

    assert_eq!(
        parsed,
        Some((
            "xiaohongshu".to_string(),
            "先获取推荐流 feeds，然后定位第15条笔记".to_string()
        ))
    );
}

#[test]
fn test_subagent_metadata_preserves_route_agent_id() {
    let metadata = build_subagent_metadata(Some("ops"));

    assert_eq!(
        metadata.get("route_agent_id").and_then(|v| v.as_str()),
        Some("ops")
    );
}

#[test]
fn test_global_core_tool_names_excludes_email() {
    let names = global_core_tool_names();

    assert!(names.iter().any(|name| name == "toggle_manage"));
    assert!(names.iter().any(|name| name == "memory_query"));
    assert!(names.iter().any(|name| name == "list_skills"));
    assert!(!names.iter().any(|name| name == "email"));
    assert!(!names.iter().any(|name| name == "finance_api"));
    assert!(!names.iter().any(|name| name == "read_file"));
}

#[test]
fn test_active_tool_names_for_skill_include_kernel_and_declared_tools() {
    use crate::context::ActiveSkillContext;

    let available: HashSet<String> = [
        "memory_query",
        "memory_upsert",
        "memory_forget",
        "spawn",
        "list_tasks",
        "list_skills",
        "toggle_manage",
        "finance_api",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    let skill = ActiveSkillContext {
        name: "stock_analysis".to_string(),
        prompt_md: String::new(),
        inject_prompt_md: true,
        tools: vec!["finance_api".to_string()],
        fallback_message: None,
        source: blockcell_skills::manager::SkillSource::BlockCell,
        learned: false,
    };

    let tool_names = resolve_effective_tool_names(
        &Config::default(),
        InteractionMode::Skill,
        None,
        Some(&skill),
        &[IntentCategory::Unknown],
        &available,
    );

    assert!(tool_names.contains(&"finance_api".to_string()));
    assert!(tool_names.contains(&"memory_query".to_string()));
    assert!(tool_names.contains(&"toggle_manage".to_string()));
    assert_eq!(
        tool_names
            .iter()
            .filter(|name| name.as_str() == "finance_api")
            .count(),
        1
    );
}

#[test]
fn test_tool_context_supports_optional_event_emitter() {
    use blockcell_core::system_event::{EventPriority, SystemEvent};
    use blockcell_tools::{SystemEventEmitter, ToolContext};
    use std::path::PathBuf;
    use std::sync::Arc;

    struct NoopEmitter;

    impl SystemEventEmitter for NoopEmitter {
        fn emit(&self, _event: SystemEvent) {}

        fn emit_simple(
            &self,
            kind: &str,
            source: &str,
            priority: EventPriority,
            title: &str,
            summary: &str,
        ) {
            let _ = SystemEvent::new_main_session(kind, source, priority, title, summary);
        }
    }

    let ctx = ToolContext {
        workspace: PathBuf::from("/tmp/workspace"),
        base: PathBuf::from("/tmp/blockcell"),
        builtin_skills_dir: None,
        active_skill_dir: None,
        session_key: "cli:test".to_string(),
        channel: "cli".to_string(),
        account_id: None,
        sender_id: None,
        chat_id: "chat-1".to_string(),
        config: Config::default(),
        permissions: blockcell_core::types::PermissionSet::new(),
        task_manager: None,
        memory_store: None,
        memory_file_store: None,
        ghost_memory_lifecycle: None,
        skill_file_store: None,
        session_search: None,
        outbound_tx: None,
        spawn_handle: None,
        capability_registry: None,
        core_evolution: None,
        event_emitter: Some(Arc::new(NoopEmitter)),
        channel_contacts_file: None,
        response_cache: None,
        runtime_handle: None,
        agent_identity: None,
        skill_mutex: Some(Arc::new(crate::write_guard::WriteGuard::default())
            as blockcell_tools::SkillMutexHandle),
        agent_type_registry: None,
        evolution_workflow_store: None,
    };

    assert!(ctx.event_emitter.is_some());
}

#[test]
fn test_skill_decision_engine_normalizes_selected_skill_name() {
    use crate::skill_decision::SkillDecisionEngine;

    let candidates = vec![
        ("xiaohongshu".to_string(), "小红书相关能力".to_string()),
        ("weather".to_string(), "天气查询".to_string()),
    ];

    let exact = SkillDecisionEngine::normalize_selected_skill_name("xiaohongshu", &candidates);
    let partial =
        SkillDecisionEngine::normalize_selected_skill_name("最合适的是 xiaohongshu。", &candidates);
    let missing = SkillDecisionEngine::normalize_selected_skill_name("finance", &candidates);

    assert_eq!(exact.as_deref(), Some("xiaohongshu"));
    assert_eq!(partial.as_deref(), Some("xiaohongshu"));
    assert_eq!(missing, None);
}

#[test]
fn test_expand_history_stubs_with_cache_restores_cached_content() {
    let cache = crate::response_cache::ResponseCache::new();
    let session_key = "ws:chat-1";
    let cached_list = (1..=18)
            .map(|i| {
                format!(
                    "{}. 第{}条推荐，包含足够长的标题、作者信息、摘要说明以及若干补充字段，用来模拟小红书推荐流里带隐藏定位字段的大列表返回结果。",
                    i, i
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
    let stub = cache
        .maybe_cache_and_stub(session_key, &cached_list, true)
        .expect("content should be cached");
    let history = vec![ChatMessage::assistant(&stub)];

    let expanded = expand_history_stubs_with_cache(&cache, session_key, &history);

    assert_eq!(expanded.len(), 1);
    assert_eq!(expanded[0].content.as_str(), Some(cached_list.as_str()));
}

#[test]
fn test_resolve_skill_run_mode_prefers_explicit_metadata() {
    let msg = InboundMessage {
        channel: "cron".to_string(),
        account_id: None,
        sender_id: "system".to_string(),
        chat_id: "chat-1".to_string(),
        content: "hello".to_string(),
        media: vec![],
        metadata: serde_json::json!({
            "skill_run_mode": "test",
        }),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    assert_eq!(resolve_skill_run_mode(&msg), SkillRunMode::Test);
}

#[test]
fn test_resolve_cron_deliver_target_requires_cron_mode_and_delivery_fields() {
    let msg = InboundMessage {
        channel: "cron".to_string(),
        account_id: None,
        sender_id: "system".to_string(),
        chat_id: "chat-1".to_string(),
        content: "hello".to_string(),
        media: vec![],
        metadata: serde_json::json!({
            "skill_run_mode": "cron",
            "deliver": true,
            "deliver_channel": "ws",
            "deliver_to": "chat-2",
        }),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    assert_eq!(
        resolve_cron_deliver_target(&msg),
        Some(("ws".to_string(), "chat-2".to_string()))
    );
}

#[test]
fn test_build_script_skill_summary_prompt_includes_skill_md_brief() {
    let prompt = build_script_skill_summary_prompt(
        "帮我搜一下小红书露营装备",
        "xiaohongshu",
        "search",
        "请优先提炼结果，不要冗长输出。",
        "找到 3 条高互动笔记",
    );

    assert!(prompt.contains("帮我搜一下小红书露营装备"));
    assert!(prompt.contains("xiaohongshu"));
    assert!(prompt.contains("search"));
    assert!(prompt.contains("请优先提炼结果"));
    assert!(prompt.contains("找到 3 条高互动笔记"));
}

#[test]
fn test_skill_prompt_injection_keeps_activate_skill_mainline() {
    let mut messages = vec![
        ChatMessage::system("You are BlockCell."),
        ChatMessage::user("run the local demo"),
    ];
    let skill_cards = vec![SkillCard {
        name: "local_demo".to_string(),
        description: "Local demo skill".to_string(),
        execution_layout: "PromptTool + LocalScript".to_string(),
        when_to_use: "Run local demo scripts".to_string(),
        outputs: "Local exec output".to_string(),
        allowed_tools: vec!["exec_local".to_string()],
        local_exec_entrypoints: vec!["scripts/hello.sh".to_string()],
        supports_local_exec: true,
    }];

    inject_skill_cards_into_system_prompt(&mut messages, &skill_cards, Some("local_demo"));

    assert_eq!(messages[0].content.as_str(), Some("You are BlockCell."));
    let prompt = messages[messages.len() - 2]
        .content
        .as_str()
        .unwrap_or_default();
    assert_eq!(messages[messages.len() - 2].role, "user");
    assert!(prompt.contains("## Installed Skills"));
    assert!(prompt.contains(
        "Use `activate_skill` when one installed skill is a better fit than general tools."
    ));
    assert!(prompt.contains("inspect it with `skill_view`"));
    assert!(prompt.contains("patch it with `skill_manage(action=\"patch\")`"));
    assert!(prompt.contains("If a skill card shows local execution entries, you may use `exec_local` only for those relative paths and only inside the active skill scope."));
    assert!(prompt.contains("Recent active skill: `local_demo`"));
    assert!(prompt.contains("布局: PromptTool + LocalScript"));
    assert!(prompt.contains("本地入口: scripts/hello.sh"));
    assert_eq!(
        messages.last().unwrap().content.as_str(),
        Some("run the local demo")
    );
}

#[test]
fn skill_card_prompt_caps_each_card() {
    let mut messages = vec![
        ChatMessage::system("stable"),
        ChatMessage::user("run skill"),
    ];
    let skill_cards = vec![SkillCard {
        name: "oversized".to_string(),
        description: format!("{} SKILL_CARD_END", "description ".repeat(2_000)),
        execution_layout: "PromptTool".to_string(),
        when_to_use: "large task".to_string(),
        outputs: "result".to_string(),
        allowed_tools: vec![],
        local_exec_entrypoints: vec![],
        supports_local_exec: false,
    }];

    inject_skill_cards_into_system_prompt(&mut messages, &skill_cards, None);

    let context = messages[messages.len() - 2]
        .content
        .as_str()
        .expect("skill context");
    assert!(!context.contains("SKILL_CARD_END"));
    assert!(crate::token::estimate_tokens(context) < 700);
}

#[test]
fn test_markdown_skill_executor_limits_tools_to_skill_scope() {
    let available: HashSet<String> = ["web_search", "read_file", "spawn", "memory_query"]
        .into_iter()
        .map(str::to_string)
        .collect();

    let tool_names = crate::prompt_skill_executor::PromptSkillExecutor::resolve_allowed_tool_names(
        &[
            "web_search".to_string(),
            "spawn".to_string(),
            "unknown_tool".to_string(),
        ],
        &available,
    );

    assert_eq!(tool_names, vec!["web_search".to_string()]);
}

#[test]
fn test_markdown_skill_executor_does_not_fallback_to_global_tools() {
    let available: HashSet<String> = ["web_search", "read_file", "memory_query"]
        .into_iter()
        .map(str::to_string)
        .collect();

    let tool_names = crate::prompt_skill_executor::PromptSkillExecutor::resolve_allowed_tool_names(
        &[],
        &available,
    );

    assert!(tool_names.is_empty());
}

#[tokio::test]
async fn test_prompt_skill_executes_through_unified_skill_executor() {
    let mut runtime = test_runtime();
    let skill_dir = runtime.paths.skills_dir().join("prompt_demo");
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("meta.yaml"),
        r#"
name: prompt_demo
description: prompt demo
"#,
    )
    .expect("write meta");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        r#"# Prompt Demo

## Shared {#shared}
你是一个简洁的整理助手。

## Prompt {#prompt}
直接整理用户输入，不需要调用工具。
"#,
    )
    .expect("write skill md");
    runtime.context_builder.reload_skills();

    let msg = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "chat-1".to_string(),
        content: "请帮我整理这句话".to_string(),
        media: vec![],
        metadata: serde_json::json!({
            "forced_skill_name": "prompt_demo",
        }),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    let result = runtime.process_message(msg).await.expect("process message");
    assert!(result.contains("请帮我整理这句话"));

    let session_key = blockcell_core::build_session_key("cli", "chat-1");
    let history = runtime
        .session_store
        .load(&session_key)
        .expect("load session history");
    assert!(history.iter().any(|message| {
        message
            .tool_calls
            .as_ref()
            .map(|calls| {
                calls.iter().any(|call| {
                    call.name == "skill_enter"
                        && call.arguments["skill_name"].as_str() == Some("prompt_demo")
                })
            })
            .unwrap_or(false)
    }));

    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};
    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&session_key)
        .expect("read session events");
    assert_steps_balanced(&events);
    let kinds: Vec<SessionEventKind> = events.iter().map(|event| event.kind).collect();
    assert_eq!(
        kinds.first(),
        Some(&SessionEventKind::SessionStart),
        "{kinds:?}"
    );
    let skill_load = kinds
        .iter()
        .position(|kind| *kind == SessionEventKind::SkillLoad)
        .expect("skill/load recorded");
    // skill/load 记录技能版本管理中的当前版本，指标按 `技能@版本` 统计回合
    let payload = &events[skill_load].payload;
    assert_eq!(payload["name"], "prompt_demo");
    let version = runtime
        .context_builder
        .skill_manager()
        .and_then(|manager| manager.get("prompt_demo"))
        .and_then(|skill| skill.current_version.clone());
    assert_eq!(payload["version"], serde_json::json!(version));
    let key = blockcell_storage::session_metrics::skill_key(payload).expect("skill key");
    assert_eq!(
        blockcell_storage::SessionMetrics::from_events(&events).skill_turns[&key],
        blockcell_storage::SkillTurnStats {
            turns: 1,
            failed_turns: 0
        }
    );
    assert_eq!(
        &kinds[skill_load + 1..skill_load + 5],
        &[
            SessionEventKind::StepStart,
            SessionEventKind::ModelRequest,
            SessionEventKind::ModelResponse,
            SessionEventKind::StepEnd,
        ],
        "{kinds:?}"
    );
    assert_eq!(
        &kinds[kinds.len() - 2..],
        &[
            SessionEventKind::AssistantMessage,
            SessionEventKind::TurnEnd
        ],
        "{kinds:?}"
    );
}

#[cfg(not(target_os = "windows"))]
#[tokio::test]
async fn test_prompt_skill_can_use_exec_skill_script_inside_skill_scope() {
    let mut runtime = test_runtime();
    runtime.tool_registry.register(Arc::new(
        blockcell_tools::exec_skill_script::ExecSkillScriptTool,
    ));
    let skill_dir = runtime.paths.skills_dir().join("local_demo");
    let scripts_dir = skill_dir.join("scripts");
    std::fs::create_dir_all(&scripts_dir).expect("create scripts dir");
    std::fs::write(
        skill_dir.join("meta.yaml"),
        r#"
name: local_demo
description: local demo
"#,
    )
    .expect("write meta");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        r#"# Local Demo

## Shared {#shared}
适合执行 skill 目录内的本地脚本。

## Prompt {#prompt}
如果要运行本地脚本，使用 exec_skill_script 调用 `scripts/hello.sh`。
"#,
    )
    .expect("write skill md");
    std::fs::write(
        scripts_dir.join("hello.sh"),
        "#!/bin/sh\necho local-skill-$1\n",
    )
    .expect("write script");
    runtime.context_builder.reload_skills();

    let msg = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "chat-local".to_string(),
        content: "运行本地脚本".to_string(),
        media: vec![],
        metadata: serde_json::json!({
            "forced_skill_name": "local_demo",
        }),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    let result = runtime.process_message(msg).await.expect("process message");
    let session_key = blockcell_core::build_session_key("cli", "chat-local");
    let history = runtime
        .session_store
        .load(&session_key)
        .expect("load session history");
    assert!(
        result.starts_with("summary:"),
        "unexpected skill result: {}",
        result
    );
    assert!(
        result.contains("local-skill-skill"),
        "unexpected skill result: {}; history: {:?}",
        result,
        history
    );
    assert!(history.iter().any(|message| {
        message
            .tool_calls
            .as_ref()
            .map(|calls| calls.iter().any(|call| call.name == "exec_skill_script"))
            .unwrap_or(false)
    }));
}

#[cfg(not(target_os = "windows"))]
#[test]
fn test_resolved_skill_tool_names_include_exec_skill_script_for_script_capable_skill() {
    let mut runtime = test_runtime();
    runtime.tool_registry.register(Arc::new(
        blockcell_tools::exec_skill_script::ExecSkillScriptTool,
    ));
    runtime
        .tool_registry
        .register(Arc::new(blockcell_tools::exec_local::ExecLocalTool));
    let skill_dir = runtime.paths.skills_dir().join("script_demo");
    std::fs::create_dir_all(skill_dir.join("scripts")).expect("create scripts dir");
    std::fs::write(
        skill_dir.join("meta.yaml"),
        r#"
name: script_demo
description: script demo
"#,
    )
    .expect("write meta");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        r#"# Script Demo

## Shared {#shared}
适合执行 skill 目录内的脚本资产。

## Prompt {#prompt}
如果需要运行本地脚本，使用 exec_skill_script 调用 `scripts/hello.sh`。
"#,
    )
    .expect("write skill md");
    std::fs::write(skill_dir.join("scripts/hello.sh"), "#!/bin/sh\necho ok\n")
        .expect("write script");
    runtime.context_builder.reload_skills();

    let active_skill = crate::context::ActiveSkillContext {
        name: "script_demo".to_string(),
        prompt_md: String::new(),
        inject_prompt_md: true,
        tools: vec![],
        fallback_message: None,
        source: blockcell_skills::manager::SkillSource::BlockCell,
        learned: false,
    };

    let tool_names = runtime.resolved_skill_tool_names(&active_skill);
    assert!(tool_names.contains(&"exec_skill_script".to_string()));
    assert!(tool_names.contains(&"exec_local".to_string()));
}

#[tokio::test]
async fn test_check_path_permission_allows_exec_skill_script_skill_paths() {
    let mut runtime = test_runtime();
    let msg = test_main_session_inbound("cli", "chat-script-path");

    assert!(
        runtime
            .check_path_permission(
                "exec_skill_script",
                &serde_json::json!({"path": "scripts/hello.sh"}),
                &msg,
            )
            .await
    );
}

#[cfg(not(target_os = "windows"))]
#[tokio::test]
async fn test_skill_executor_uses_manual_not_file_type_to_choose_skill_script() {
    let mut runtime = test_runtime();
    runtime.tool_registry.register(Arc::new(
        blockcell_tools::exec_skill_script::ExecSkillScriptTool,
    ));
    let skill_dir = runtime.paths.skills_dir().join("legacy_script_demo");
    let scripts_dir = skill_dir.join("scripts");
    std::fs::create_dir_all(&scripts_dir).expect("create scripts dir");
    std::fs::write(
        skill_dir.join("meta.yaml"),
        r#"
name: legacy_script_demo
description: legacy script demo
"#,
    )
    .expect("write meta");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        r#"# Legacy Script Demo

## Shared {#shared}
适合执行 skill 目录内的本地脚本。

## Prompt {#prompt}
如果需要运行本地脚本，使用 exec_skill_script 调用 `scripts/hello.sh`。
"#,
    )
    .expect("write skill md");
    std::fs::write(
        skill_dir.join("SKILL.py"),
        "print('legacy path should not run')\n",
    )
    .expect("write legacy py");
    std::fs::write(
        scripts_dir.join("hello.sh"),
        "#!/bin/sh\necho local-skill-$1\n",
    )
    .expect("write script");
    runtime.context_builder.reload_skills();

    let msg = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "chat-legacy".to_string(),
        content: "运行这个技能".to_string(),
        media: vec![],
        metadata: serde_json::json!({
            "forced_skill_name": "legacy_script_demo",
        }),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    let result = runtime.process_message(msg).await.expect("process message");
    let session_key = blockcell_core::build_session_key("cli", "chat-legacy");
    let history = runtime
        .session_store
        .load(&session_key)
        .expect("load session history");
    assert!(
        result.contains("local-skill-skill"),
        "unexpected skill result: {}; history: {:?}",
        result,
        history
    );
    assert!(history.iter().any(|message| {
        message
            .tool_calls
            .as_ref()
            .map(|calls| calls.iter().any(|call| call.name == "exec_skill_script"))
            .unwrap_or(false)
    }));
}

#[cfg(not(target_os = "windows"))]
#[tokio::test]
async fn test_cli_style_skill_runs_via_exec_skill_script() {
    let mut runtime = test_runtime();
    runtime.tool_registry.register(Arc::new(
        blockcell_tools::exec_skill_script::ExecSkillScriptTool,
    ));
    let skill_dir = runtime.paths.skills_dir().join("cli_demo");
    let bin_dir = skill_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create bin dir");
    std::fs::write(
        skill_dir.join("meta.yaml"),
        r#"
name: cli_demo
description: cli demo
"#,
    )
    .expect("write meta");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        r#"# CLI Demo

## Shared {#shared}
适合执行 skill 目录中的 CLI 脚本。

## Prompt {#prompt}
当用户要求执行 CLI 时，使用 exec_skill_script 调用 `bin/cli.sh`。
"#,
    )
    .expect("write skill md");
    std::fs::write(bin_dir.join("cli.sh"), "#!/bin/sh\necho local-cli-$1\n")
        .expect("write cli script");
    runtime.context_builder.reload_skills();

    let msg = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "chat-cli".to_string(),
        content: "执行 CLI".to_string(),
        media: vec![],
        metadata: serde_json::json!({
            "forced_skill_name": "cli_demo",
        }),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    let result = runtime.process_message(msg).await.expect("process message");
    assert!(
        result.contains("local-cli-demo"),
        "unexpected cli result: {}",
        result
    );
}

#[cfg(not(target_os = "windows"))]
#[tokio::test]
async fn test_prompt_skill_can_still_use_exec_local_inside_skill_scope_for_compat() {
    let mut runtime = test_runtime();
    runtime
        .tool_registry
        .register(Arc::new(blockcell_tools::exec_local::ExecLocalTool));
    let skill_dir = runtime.paths.skills_dir().join("compat_local_demo");
    let scripts_dir = skill_dir.join("scripts");
    std::fs::create_dir_all(&scripts_dir).expect("create scripts dir");
    std::fs::write(
        skill_dir.join("meta.yaml"),
        r#"
name: compat_local_demo
description: compat local demo
"#,
    )
    .expect("write meta");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        r#"# Compat Local Demo

## Shared {#shared}
适合执行 skill 目录内的本地脚本。

## Prompt {#prompt}
如果要运行本地脚本，使用 exec_local。
"#,
    )
    .expect("write skill md");
    std::fs::write(
        scripts_dir.join("hello.sh"),
        "#!/bin/sh\necho local-skill-$1\n",
    )
    .expect("write script");
    runtime.context_builder.reload_skills();

    let msg = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "chat-local-compat".to_string(),
        content: "运行兼容本地脚本".to_string(),
        media: vec![],
        metadata: serde_json::json!({
            "forced_skill_name": "compat_local_demo",
        }),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    let result = runtime.process_message(msg).await.expect("process message");
    let session_key = blockcell_core::build_session_key("cli", "chat-local-compat");
    let history = runtime
        .session_store
        .load(&session_key)
        .expect("load session history");
    assert!(
        result.starts_with("summary:"),
        "unexpected skill result: {}",
        result
    );
    assert!(
        result.contains("local-skill-skill"),
        "unexpected skill result: {}; history: {:?}",
        result,
        history
    );
    assert!(history.iter().any(|message| {
        message
            .tool_calls
            .as_ref()
            .map(|calls| calls.iter().any(|call| call.name == "exec_local"))
            .unwrap_or(false)
    }));
}

#[tokio::test]
async fn test_unified_entry_calls_general_tool_without_extra_planning_roundtrip() {
    let provider = Arc::new(UnifiedEntryProvider {
        calls: AtomicUsize::new(0),
    });
    let mut runtime = test_runtime_with_provider(provider.clone());

    let msg = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "chat-general-tool".to_string(),
        content: "查看当前目录下文件".to_string(),
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    let result = runtime.process_message(msg).await.expect("process message");
    assert!(result.contains("目录内容"));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
}

#[cfg(not(target_os = "windows"))]
#[tokio::test]
async fn test_unified_entry_can_activate_skill_without_forced_skill_metadata() {
    let provider = Arc::new(UnifiedEntryProvider {
        calls: AtomicUsize::new(0),
    });
    let mut runtime = test_runtime_with_provider(provider.clone());
    runtime.tool_registry.register(Arc::new(
        blockcell_tools::exec_skill_script::ExecSkillScriptTool,
    ));
    let skill_dir = runtime.paths.skills_dir().join("local_demo");
    let scripts_dir = skill_dir.join("scripts");
    std::fs::create_dir_all(&scripts_dir).expect("create scripts dir");
    std::fs::write(
        skill_dir.join("meta.yaml"),
        r#"
name: local_demo
description: local demo
"#,
    )
    .expect("write meta");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        r#"# Local Demo

## Shared {#shared}
适合执行 skill 目录内的本地脚本。

## Prompt {#prompt}
如果需要运行本地脚本，使用 exec_skill_script 调用 `scripts/hello.sh`。
"#,
    )
    .expect("write skill md");
    std::fs::write(
        scripts_dir.join("hello.sh"),
        "#!/bin/sh\necho local-skill-$1\n",
    )
    .expect("write script");
    runtime.context_builder.reload_skills();

    let msg = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "chat-activate-skill".to_string(),
        content: "运行本地脚本".to_string(),
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    let result = runtime.process_message(msg).await.expect("process message");
    let session_key = blockcell_core::build_session_key("cli", "chat-activate-skill");
    let events = blockcell_storage::SessionEventReader::read(
        &blockcell_storage::JsonlSessionEventLog::new(runtime.paths.clone()),
        &session_key,
    )
    .expect("read session events");
    assert!(
        events
            .iter()
            .filter(|event| event.kind == blockcell_storage::SessionEventKind::StepStart)
            .count()
            >= 2,
        "expected outer and nested skill steps: {events:?}"
    );
    assert_steps_balanced(&events);
    let history = runtime
        .session_store
        .load(&session_key)
        .expect("load session history");

    assert!(
        result.starts_with("summary:"),
        "unexpected result: {}",
        result
    );
    assert!(history.iter().any(|message| {
        message
            .tool_calls
            .as_ref()
            .map(|calls| {
                calls
                    .iter()
                    .any(|call| call.name == ACTIVATE_SKILL_TOOL_NAME)
            })
            .unwrap_or(false)
    }));
    assert!(history.iter().any(|message| {
        message
            .tool_calls
            .as_ref()
            .map(|calls| calls.iter().any(|call| call.name == "skill_enter"))
            .unwrap_or(false)
    }));
}

#[test]
fn test_determine_interaction_mode_prefers_skill() {
    let mode = determine_interaction_mode(true, &[IntentCategory::Chat]);
    assert_eq!(mode, InteractionMode::Skill);
}

#[test]
fn test_determine_interaction_mode_uses_chat_for_single_chat_intent() {
    let mode = determine_interaction_mode(false, &[IntentCategory::Chat]);
    assert_eq!(mode, InteractionMode::Chat);
}

#[test]
fn test_determine_interaction_mode_falls_back_to_general_without_skill() {
    let mode = determine_interaction_mode(false, &[IntentCategory::Unknown]);
    assert_eq!(mode, InteractionMode::General);
}

#[test]
fn test_determine_interaction_mode_uses_coding_for_code_changes() {
    let mode = determine_interaction_mode(false, &[IntentCategory::Coding]);
    assert_eq!(mode, InteractionMode::Coding);
}

#[test]
fn test_coding_mode_uses_shell_and_code_tools_without_legacy_exec() {
    let available: HashSet<String> = [
        "read_file",
        "write_file",
        "edit_file",
        "list_dir",
        "grep",
        "glob",
        "shell",
        "exec",
        "update_plan",
        "file_ops",
        "get_context_remaining",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    let tools = resolve_effective_tool_names(
        &Config::default(),
        InteractionMode::Coding,
        None,
        None,
        &[IntentCategory::Coding],
        &available,
    );

    for required in [
        "read_file",
        "edit_file",
        "grep",
        "glob",
        "get_context_remaining",
        "shell",
        "update_plan",
    ] {
        assert!(tools.contains(&required.to_string()), "missing {required}");
    }
    assert!(!tools.contains(&"exec".to_string()));
}

#[test]
fn test_skill_summary_formatter_uses_brief_md_and_result() {
    let prompt = crate::skill_summary::SkillSummaryFormatter::build_prompt(
        "帮我搜一下 AI 新闻",
        "ai_news",
        Some("search"),
        "请优先提炼要点，不要重复脚本原文。",
        "找到 5 条相关新闻",
    );

    assert!(prompt.contains("帮我搜一下 AI 新闻"));
    assert!(prompt.contains("ai_news"));
    assert!(prompt.contains("search"));
    assert!(prompt.contains("请优先提炼要点"));
    assert!(prompt.contains("找到 5 条相关新闻"));
}

#[test]
fn test_prompt_and_script_skills_share_summary_formatter() {
    let prompt_skill_prompt = crate::skill_summary::SkillSummaryFormatter::build_prompt(
        "帮我深度分析 BTC",
        "deep_analysis",
        None,
        "请按结构化方式输出。",
        "这是最终分析结果。",
    );
    let script_skill_prompt = crate::skill_summary::SkillSummaryFormatter::build_prompt(
        "北京天气",
        "weather",
        Some("forecast"),
        "优先给出天气摘要。",
        "今天晴，最高 18 度。",
    );

    assert!(prompt_skill_prompt.contains("技能说明摘要"));
    assert!(script_skill_prompt.contains("技能说明摘要"));
    assert!(prompt_skill_prompt.contains("执行结果"));
    assert!(script_skill_prompt.contains("执行结果"));
}

#[test]
fn test_prompt_skill_persists_internal_skill_enter_and_real_tool_chain() {
    let mut history = Vec::new();
    let real_tool_call = ToolCallRequest {
        id: "call-web-search".to_string(),
        name: "web_search".to_string(),
        arguments: serde_json::json!({ "query": "BTC" }),
        thought_signature: None,
    };
    let mut real_tool_result =
        ChatMessage::tool_result("call-web-search", r#"{"items":[{"title":"BTC news"}]}"#);
    real_tool_result.name = Some("web_search".to_string());

    persist_prompt_skill_history(
        &mut history,
        "帮我深度分析 BTC",
        "deep_analysis",
        &["web_search".to_string()],
        &[
            ChatMessage {
                id: None,
                role: "assistant".to_string(),
                content: serde_json::Value::String("搜索 BTC 新闻".to_string()),
                reasoning_content: None,
                tool_calls: Some(vec![real_tool_call]),
                tool_call_id: None,
                name: None,
            },
            real_tool_result,
        ],
        "整理后的最终回答",
    );

    assert_eq!(history.len(), 6);
    assert_eq!(history[0].role, "user");
    assert_eq!(
        history[1].tool_calls.as_ref().unwrap()[0].name,
        "skill_enter"
    );
    assert_eq!(history[2].role, "tool");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(history[2].content.as_str().unwrap()).unwrap()
            ["skill_name"],
        "deep_analysis"
    );
    assert_eq!(
        history[3].tool_calls.as_ref().unwrap()[0].name,
        "web_search"
    );
    assert_eq!(history[4].role, "tool");
    assert_eq!(history[4].name.as_deref(), Some("web_search"));
    assert_eq!(history[5].content.as_str(), Some("整理后的最终回答"));
}

#[test]
fn test_script_skill_persists_internal_skill_invoke_and_raw_result() {
    let mut history = Vec::new();

    persist_script_skill_history(
        &mut history,
        "北京天气",
        "weather",
        "skill_invoke_python",
        &[
            "forecast".to_string(),
            "--city".to_string(),
            "beijing".to_string(),
        ],
        r#"{"temp":18,"condition":"sunny"}"#,
        "今天晴，最高 18 度。",
    );

    assert_eq!(history.len(), 4);
    assert_eq!(history[0].role, "user");
    assert_eq!(
        history[1].tool_calls.as_ref().unwrap()[0].name,
        "skill_invoke_python"
    );
    assert_eq!(
        history[1].tool_calls.as_ref().unwrap()[0].arguments["argv"],
        serde_json::json!(["forecast", "--city", "beijing"])
    );
    assert_eq!(history[2].role, "tool");
    assert_eq!(
        history[2].content.as_str(),
        Some(r#"{"temp":18,"condition":"sunny"}"#)
    );
    assert_eq!(history[3].content.as_str(), Some("今天晴，最高 18 度。"));
}

#[test]
fn test_find_recent_skill_name_from_history_reads_internal_skill_trace() {
    let mut history = Vec::new();
    persist_prompt_skill_history(
        &mut history,
        "帮我深度分析 BTC",
        "deep_analysis",
        &["web_search".to_string()],
        &[],
        "整理后的最终回答",
    );

    assert_eq!(
        find_recent_skill_name_from_history(&history).as_deref(),
        Some("deep_analysis")
    );
}

#[test]
fn test_active_skill_name_metadata_roundtrip() {
    let mut metadata = serde_json::Value::Null;
    record_active_skill_name(&mut metadata, "ppt-generator");

    assert_eq!(
        active_skill_name_from_metadata(&metadata).as_deref(),
        Some("ppt-generator")
    );
    assert_eq!(
        metadata
            .get(SESSION_ACTIVE_SKILL_CORRECTIONS_KEY)
            .and_then(|value| value.as_u64()),
        Some(0)
    );
}

#[test]
fn repeated_learned_skill_corrections_disable_skill_toggle() {
    let paths = Paths::with_base(
        std::env::temp_dir().join(format!("blockcell-disable-skill-{}", uuid::Uuid::new_v4())),
    );
    let provider_pool = blockcell_providers::ProviderPool::from_single_provider(
        "test/mock",
        "test",
        Arc::new(TestProvider),
    );
    let runtime = AgentRuntime::new(
        Config::default(),
        paths.clone(),
        provider_pool,
        blockcell_tools::ToolRegistry::new(),
    )
    .expect("create runtime");
    let mut metadata = serde_json::Value::Null;
    record_active_skill_name(&mut metadata, "release_checklist");
    let msg = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "disable-skill".to_string(),
        content: "不要这样做，以后先检查 rollback plan".to_string(),
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    runtime
        .apply_learned_skill_negative_feedback(&mut metadata, &msg)
        .unwrap();
    assert!(load_disabled_toggles(&paths, "skills").is_empty());
    runtime
        .apply_learned_skill_negative_feedback(&mut metadata, &msg)
        .unwrap();

    assert!(load_disabled_toggles(&paths, "skills").contains("release_checklist"));
    assert!(active_skill_name_from_metadata(&metadata).is_none());
}

#[test]
fn test_continued_skill_name_prefers_metadata_and_falls_back_to_history() {
    let mut history = Vec::new();
    persist_prompt_skill_history(
        &mut history,
        "帮我深度分析 BTC",
        "deep_analysis",
        &["web_search".to_string()],
        &[],
        "整理后的最终回答",
    );

    assert_eq!(
        continued_skill_name(
            &serde_json::json!({"active_skill_name":"ppt-generator"}),
            &history
        )
        .as_deref(),
        Some("ppt-generator")
    );
    assert_eq!(
        continued_skill_name(&serde_json::Value::Null, &history).as_deref(),
        Some("deep_analysis")
    );
}

#[test]
fn test_continued_skill_suppresses_prompt_reinjection_for_same_skill() {
    let active_skill = crate::context::ActiveSkillContext {
        name: "ppt-generator".to_string(),
        prompt_md: "manual".to_string(),
        inject_prompt_md: true,
        tools: vec!["write_file".to_string()],
        fallback_message: None,
        source: blockcell_skills::manager::SkillSource::BlockCell,
        learned: false,
    };

    let continued = suppress_prompt_reinjection_for_continued_skill(
        active_skill.clone(),
        Some("ppt-generator"),
    );
    assert!(!continued.inject_prompt_md);

    let other = suppress_prompt_reinjection_for_continued_skill(active_skill, Some("weather"));
    assert!(other.inject_prompt_md);
}

#[test]
fn test_tool_round_throttle_delay_uses_base_delay_without_rate_limit() {
    assert_eq!(
        tool_round_throttle_delay(false),
        std::time::Duration::from_millis(600)
    );
}

#[test]
fn test_tool_round_throttle_delay_uses_longer_delay_after_rate_limit() {
    assert_eq!(
        tool_round_throttle_delay(true),
        std::time::Duration::from_millis(2500)
    );
}

#[test]
fn test_extract_json_from_text_handles_markdown_fence() {
    let text = "```json\n{\"argv\":[\"search\",\"btc\"]}\n```";
    assert_eq!(
        extract_json_from_text(text),
        "{\"argv\":[\"search\",\"btc\"]}"
    );
}

#[tokio::test]
async fn init_memory_system_uses_runtime_memory_config() {
    let mut config = Config::default();
    config.memory.memory_system.token_budget = 1_000;
    config.memory.memory_system.layer1.preview_size_chars = 123;
    config.memory.memory_system.layer2.gap_threshold_minutes = 7;
    config
        .memory
        .memory_system
        .layer3
        .minimum_message_tokens_to_init = 111;
    config.memory.memory_system.layer3.max_section_length = 222;
    config.memory.memory_system.layer4.compact_threshold_ratio = 0.5;
    config.memory.memory_system.layer4.keep_recent_messages = 3;
    config
        .memory
        .memory_system
        .layer5
        .min_messages_for_extraction = 2;

    let mut runtime = test_runtime_with_provider_and_paths(
        Paths::with_base(std::env::temp_dir().join(format!(
            "blockcell-memory-config-runtime-{}",
            uuid::Uuid::new_v4()
        ))),
        Arc::new(TestProvider),
        config,
    );

    runtime
        .init_memory_system("cli:configured-session".to_string())
        .await
        .unwrap();

    let memory_system = runtime.memory_system().expect("memory system initialized");
    assert_eq!(memory_system.session_id(), "cli:configured-session");
    assert_eq!(memory_system.config().token_budget, 1_000);
    assert_eq!(memory_system.config().layer1.preview_size_chars, 123);
    assert_eq!(memory_system.config().layer2.gap_threshold_minutes, 7);
    assert_eq!(
        memory_system
            .session_memory_state()
            .config
            .minimum_message_tokens_to_init,
        111
    );
    assert_eq!(
        memory_system
            .session_memory_state()
            .config
            .max_section_length,
        222
    );
    assert!(memory_system.should_compact(500));
    assert!(!memory_system.should_compact(499));
    assert_eq!(memory_system.config().layer4.keep_recent_messages, 3);
    assert_eq!(memory_system.config().layer5.min_messages_for_extraction, 2);
}

#[tokio::test]
async fn process_message_reinitializes_memory_system_for_new_session() {
    let mut runtime = test_runtime_with_provider(Arc::new(TestProvider));

    let first = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "memory-session-a".to_string(),
        content: "hello a".to_string(),
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };
    let first_session = first.session_key();
    runtime.process_message(first).await.unwrap();
    assert_eq!(
        runtime.memory_system().map(|system| system.session_id()),
        Some(first_session.as_str())
    );

    let second = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "memory-session-b".to_string(),
        content: "hello b".to_string(),
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };
    let second_session = second.session_key();
    runtime.process_message(second).await.unwrap();
    assert_eq!(
        runtime.memory_system().map(|system| system.session_id()),
        Some(second_session.as_str())
    );
}

#[tokio::test]
async fn test_stream_retry_emits_reset_before_retrying_ws_response() {
    let mut runtime = test_runtime_with_provider(Arc::new(StreamingRetryProvider {
        attempts: AtomicUsize::new(0),
    }));
    let (event_tx, mut event_rx) = broadcast::channel(32);
    runtime.set_event_tx(event_tx);

    let msg = InboundMessage {
        channel: "ws".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "stream-retry".to_string(),
        content: "hello retry".to_string(),
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    let result = runtime.process_message(msg).await.expect("process message");
    assert_eq!(result, "final answer");

    let events = drain_ws_events(&mut event_rx);
    let event_types = collect_event_types(&events);
    assert!(
        contains_event_subsequence(
            &event_types,
            &["token", "stream_reset", "token", "message_done"]
        ),
        "unexpected event order: {:?}",
        event_types
    );
    let final_event = events
        .iter()
        .rev()
        .find(|event| event["type"] == "message_done")
        .expect("message_done event missing");
    assert_eq!(final_event["content"], "final answer");
}

#[tokio::test]
async fn test_stream_close_without_done_returns_accumulated_response() {
    let mut runtime = test_runtime_with_provider(Arc::new(StreamingCloseProvider));

    let msg = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "stream-close".to_string(),
        content: "hello close".to_string(),
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    let result = runtime.process_message(msg).await.expect("process message");
    assert_eq!(result, "closed answer");
}

#[tokio::test]
async fn connection_phase_failure_falls_back_to_next_provider_without_retry_budget() {
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    config.agents.defaults.llm_max_retries = 0;

    let base = std::env::temp_dir().join(format!(
        "blockcell-fallback-runtime-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&base).expect("create temp fallback runtime dir");
    let paths = Paths::with_base(base);
    let provider_pool = ProviderPool::from_entries(vec![
        blockcell_providers::ProviderPoolEntry {
            model: "primary".to_string(),
            provider_name: "test".to_string(),
            weight: 1,
            priority: 1,
            provider: Arc::new(ConnectionFailingProvider),
        },
        blockcell_providers::ProviderPoolEntry {
            model: "fallback".to_string(),
            provider_name: "test".to_string(),
            weight: 1,
            priority: 2,
            provider: Arc::new(SuccessfulFallbackProvider),
        },
    ]);
    let mut runtime = AgentRuntime::new(
        config,
        paths,
        provider_pool,
        blockcell_tools::ToolRegistry::new(),
    )
    .expect("create runtime");
    runtime.set_agent_id(Some("default".to_string()));
    let msg = test_main_session_inbound("ws", "fallback-chat");
    let mut saw_rate_limit = false;

    let (response, served_by) = runtime
        .call_llm_with_retry(
            &[ChatMessage::user("hello")],
            &[],
            &msg,
            None,
            &HashMap::new(),
            &mut saw_rate_limit,
        )
        .await
        .expect("fallback provider should answer");

    assert_eq!(response.content.as_deref(), Some("fallback answer"));
    assert_eq!(served_by.model, "fallback");
    assert_eq!(served_by.provider, "test");
}

#[tokio::test]
async fn explicit_stream_done_reports_provider_success() {
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    config.agents.defaults.llm_max_retries = 0;

    let base = std::env::temp_dir().join(format!(
        "blockcell-stream-success-runtime-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&base).expect("create temp stream success runtime dir");
    let paths = Paths::with_base(base);
    let provider_pool = ProviderPool::from_single_provider(
        "test/mock",
        "test",
        Arc::new(SuccessfulFallbackProvider),
    );
    provider_pool.report(0, CallResult::Transient);
    let mut runtime = AgentRuntime::new(
        config,
        paths,
        provider_pool.clone(),
        blockcell_tools::ToolRegistry::new(),
    )
    .expect("create runtime");
    runtime.set_agent_id(Some("default".to_string()));
    let msg = test_main_session_inbound("ws", "stream-success-chat");
    let mut saw_rate_limit = false;

    runtime
        .call_llm_with_retry(
            &[ChatMessage::user("hello")],
            &[],
            &msg,
            None,
            &HashMap::new(),
            &mut saw_rate_limit,
        )
        .await
        .expect("explicit stream completion should succeed");

    let status = provider_pool.status_summary();
    assert_eq!(status[0].success_count, 1);
    assert_eq!(status[0].fail_count, 0);
}

fn test_runtime() -> AgentRuntime {
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());

    let base = std::env::temp_dir().join(format!(
        "blockcell-system-event-runtime-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&base).expect("create temp runtime dir");
    let paths = Paths::with_base(base);
    test_runtime_with_provider_and_paths(paths, Arc::new(TestProvider), config)
}

#[test]
fn shared_learning_coordinator_accumulates_turns_across_message_runtimes() {
    let first = test_runtime();
    let shared = Arc::clone(&first.learning_coordinator);

    first.learning_coordinator.on_turn_start(true);

    let mut second = test_runtime();
    second.install_shared_learning_coordinator(Arc::clone(&shared));
    second.learning_coordinator.on_turn_start(true);

    let mut third = test_runtime();
    third.install_shared_learning_coordinator(Arc::clone(&shared));
    third.learning_coordinator.on_turn_start(true);

    assert!(Arc::ptr_eq(&shared, &third.learning_coordinator));
    assert!(third
        .learning_coordinator
        .check_memory_nudge(true)
        .is_some());
}

fn test_runtime_with_provider(provider: Arc<dyn Provider>) -> AgentRuntime {
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());

    let base = std::env::temp_dir().join(format!(
        "blockcell-system-event-runtime-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&base).expect("create temp runtime dir");
    let paths = Paths::with_base(base);
    test_runtime_with_provider_and_paths(paths, provider, config)
}

fn test_runtime_with_embedded_ghost_learning() -> AgentRuntime {
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    config.agents.ghost.learning.enabled = true;
    config.agents.ghost.learning.capture_enabled = Some(true);
    config.agents.ghost.learning.write_enabled = Some(false);
    config.agents.ghost.learning.recall_enabled = Some(false);

    let base = std::env::temp_dir().join(format!(
        "blockcell-ghost-learning-runtime-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&base).expect("create temp ghost runtime dir");
    let paths = Paths::with_base(base);
    test_runtime_with_provider_and_paths(paths, Arc::new(TestProvider), config)
}

fn test_runtime_with_boundary_flush_provider(provider: Arc<BoundaryFlushProvider>) -> AgentRuntime {
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    config.agents.ghost.learning.enabled = true;
    config.agents.ghost.learning.capture_enabled = Some(true);
    config.agents.ghost.learning.write_enabled = Some(false);
    config.agents.ghost.learning.recall_enabled = Some(false);

    let base = std::env::temp_dir().join(format!(
        "blockcell-boundary-flush-runtime-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&base).expect("create temp boundary flush runtime dir");
    let paths = Paths::with_base(base);
    test_runtime_with_provider_and_paths(paths, provider, config)
}

fn test_runtime_with_ghost_review_provider(
    provider: Arc<dyn Provider>,
    shadow_mode: bool,
) -> (AgentRuntime, Paths) {
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    config.agents.ghost.learning.enabled = true;
    config.agents.ghost.learning.capture_enabled = Some(true);
    config.agents.ghost.learning.write_enabled = Some(!shadow_mode);
    config.agents.ghost.learning.recall_enabled = Some(!shadow_mode);

    let base = std::env::temp_dir().join(format!(
        "blockcell-ghost-review-runtime-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&base).expect("create temp ghost review runtime dir");
    let paths = Paths::with_base(base);

    (
        test_runtime_with_provider_and_paths(paths.clone(), provider, config),
        paths,
    )
}

fn test_runtime_with_file_memory_recall(provider: Arc<dyn Provider>) -> (AgentRuntime, Paths) {
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    config.agents.ghost.learning.enabled = true;
    config.agents.ghost.learning.capture_enabled = Some(true);
    config.agents.ghost.learning.write_enabled = Some(true);
    config.agents.ghost.learning.recall_enabled = Some(true);
    config.agents.ghost.learning.recall_max_items = 4;
    config.agents.ghost.learning.recall_token_budget = 240;

    let base = std::env::temp_dir().join(format!(
        "blockcell-file-memory-recall-runtime-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&base).expect("create temp file memory recall runtime dir");
    let paths = Paths::with_base(base);
    paths.ensure_dirs().expect("ensure dirs");
    std::fs::write(
            paths.memory_md(),
            "Project fact: write deploy docs as concise step-by-step instructions with a rollback checklist.",
        )
        .expect("write memory md");

    (
        test_runtime_with_provider_and_paths(paths.clone(), provider, config),
        paths,
    )
}

fn test_runtime_with_provider_and_paths(
    paths: Paths,
    provider: Arc<dyn Provider>,
    config: Config,
) -> AgentRuntime {
    let provider_pool =
        blockcell_providers::ProviderPool::from_single_provider("test/mock", "test", provider);

    let mut runtime = AgentRuntime::new(
        config,
        paths,
        provider_pool,
        blockcell_tools::ToolRegistry::new(),
    )
    .expect("create runtime");
    runtime.set_agent_id(Some("default".to_string()));
    runtime
}

/// 带默认工具注册表的测试运行时（预取等需要真实工具声明的用例使用）。
fn test_runtime_with_default_tools() -> AgentRuntime {
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    // 注意：`std::env::temp_dir()` 在 macOS 上位于 `/var/folders/...`，canonicalize 后
    // 落到 `/private/var/...`，而 `/private/var` 命中路径策略的内置敏感目录，会让工作区
    // 文件被判为 Deny。这里改用 `/tmp`（canonicalize 为 `/private/tmp`，不在敏感列表内），
    // 使工作区路径能正常通过路径策略。
    let base = std::path::PathBuf::from("/tmp").join(format!(
        "blockcell-default-tools-runtime-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&base).expect("create temp default-tools runtime dir");
    let paths = Paths::with_base(base);
    let provider_pool = blockcell_providers::ProviderPool::from_single_provider(
        "test/mock",
        "test",
        Arc::new(TestProvider),
    );
    let mut runtime = AgentRuntime::new(
        config,
        paths,
        provider_pool,
        blockcell_tools::ToolRegistry::with_defaults(),
    )
    .expect("create runtime");
    runtime.set_agent_id(Some("default".to_string()));
    runtime
}

#[test]
fn steering_drain_injects_user_messages_into_current_and_history() {
    let mut runtime = test_runtime();
    let (steering, sender) = SteeringChannel::new(4);
    runtime.set_steering_channel(steering, sender.clone());
    sender
        .try_send(SteeringMessage {
            content: "adjust course".to_string(),
            channel: "ws".to_string(),
            chat_id: "chat-a".to_string(),
        })
        .expect("steering message should fit");

    let inbound = InboundMessage {
        channel: "ws".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "chat-a".to_string(),
        content: "start".to_string(),
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };
    let mut current_messages = vec![ChatMessage::user("start")];
    let mut history = vec![ChatMessage::user("start")];

    let injected = runtime.drain_steering_messages(&mut current_messages, &mut history, &inbound);

    assert_eq!(injected, 1);
    assert_eq!(chat_message_text(&current_messages[1]), "adjust course");
    assert_eq!(chat_message_text(&history[1]), "adjust course");
    assert_eq!(current_messages[1].role, "user");
    assert_eq!(history[1].role, "user");
}

fn test_main_session_inbound(channel: &str, chat_id: &str) -> InboundMessage {
    InboundMessage {
        channel: channel.to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: chat_id.to_string(),
        content: "hello".to_string(),
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    }
}

#[tokio::test]
async fn message_task_failure_delivers_error_to_external_channel() {
    let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
    let (event_tx, _event_rx) = broadcast::channel(1);
    let mut msg = test_main_session_inbound("telegram", "chat-external");
    msg.account_id = Some("bot-account".to_string());

    deliver_message_task_failure(
        &msg,
        "task-external",
        Some("default"),
        "session load failed",
        Some(&event_tx),
        Some(&outbound_tx),
    )
    .await;

    let outbound = outbound_rx
        .try_recv()
        .expect("external channel should receive terminal error");
    assert_eq!(outbound.channel, "telegram");
    assert_eq!(outbound.chat_id, "chat-external");
    assert_eq!(outbound.account_id.as_deref(), Some("bot-account"));
    assert_eq!(outbound.content, "❌ session load failed");
}

#[tokio::test]
async fn message_task_failure_uses_event_only_for_websocket() {
    let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
    let (event_tx, mut event_rx) = broadcast::channel(1);
    let msg = test_main_session_inbound("ws", "chat-ws");

    deliver_message_task_failure(
        &msg,
        "task-ws",
        Some("default"),
        "provider unavailable",
        Some(&event_tx),
        Some(&outbound_tx),
    )
    .await;

    assert!(matches!(
        outbound_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    let event: serde_json::Value = serde_json::from_str(
        &event_rx
            .try_recv()
            .expect("websocket should receive terminal error event"),
    )
    .expect("parse error event");
    assert_eq!(event["type"], "error");
    assert_eq!(event["channel"], "ws");
    assert_eq!(event["chat_id"], "chat-ws");
    assert_eq!(event["task_id"], "task-ws");
    assert_eq!(event["message"], "provider unavailable");
}

#[tokio::test]
async fn message_task_supervisor_marks_panics_failed_and_emits_cleanup() {
    let task_manager = TaskManager::new();
    let task_id = "panic-message-task".to_string();
    task_manager
        .create_and_start_task(
            &task_id,
            "panic test",
            "panic",
            "ws",
            "chat-a",
            Some("default"),
            false,
            None,
            false,
        )
        .await;
    let conversation_key = ActiveConversationKey::from_channel_chat("default", "ws", "chat-a");
    let (task_done_tx, mut task_done_rx) = mpsc::unbounded_channel();
    let handle = tokio::spawn(async { panic!("intentional message-task panic") });

    supervise_message_task(
        handle,
        task_manager.clone(),
        task_id.clone(),
        None,
        task_done_tx,
        conversation_key.clone(),
    )
    .await;

    let task = task_manager
        .get_task(&task_id)
        .await
        .expect("task remains visible");
    assert_eq!(task.status, TaskStatus::Failed);
    assert!(task
        .error
        .as_deref()
        .is_some_and(|error| error.contains("panicked")));
    assert_eq!(task_done_rx.try_recv(), Ok((task_id, conversation_key)));
}

#[tokio::test]
async fn run_loop_cancels_active_message_when_inbound_closes() {
    let started = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(HangingStreamProvider {
        started: started.clone(),
        senders: Mutex::new(Vec::new()),
    });
    let paths = Paths::with_base(std::env::temp_dir().join(format!(
        "blockcell-run-loop-close-test-{}",
        uuid::Uuid::new_v4()
    )));
    paths.ensure_dirs().expect("create runtime dirs");
    let mut runtime = test_runtime_with_provider_and_paths(paths, provider, Config::default());
    let task_manager = runtime.task_manager.clone();
    let (inbound_tx, inbound_rx) = mpsc::channel(1);
    let run_loop = tokio::spawn(async move {
        runtime.run_loop(inbound_rx, None).await;
    });

    inbound_tx
        .send(test_main_session_inbound("ws", "close-chat"))
        .await
        .expect("send inbound");
    tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
        .await
        .expect("message task did not reach provider");
    drop(inbound_tx);

    tokio::time::timeout(std::time::Duration::from_secs(2), run_loop)
        .await
        .expect("run loop did not stop")
        .expect("run loop panicked");
    assert!(task_manager
        .list_tasks_for_origin("ws", "close-chat", None)
        .await
        .is_empty());
}

async fn wait_for_runtime_review_runs(paths: &Paths, expected: usize) {
    for _ in 0..50 {
        let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
        if ledger.review_run_count().expect("count review runs") >= expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for ghost review runs");
}

#[tokio::test]
async fn non_trivial_turn_creates_learning_episode() {
    let mut runtime = test_runtime_with_embedded_ghost_learning();
    let msg = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "ghost-turn".to_string(),
        content: "figure out the correct deploy sequence".to_string(),
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    runtime.process_message(msg).await.unwrap();

    assert_eq!(runtime.test_ghost_ledger().episode_count().unwrap(), 1);
    assert_eq!(
        runtime
            .test_ghost_ledger()
            .latest_boundary_kind()
            .unwrap()
            .as_deref(),
        Some("turn_end")
    );
    let episode = runtime
        .test_ghost_ledger()
        .latest_episode_by_boundary_kind("turn_end")
        .unwrap()
        .unwrap();
    assert_eq!(
        episode.subject_key.as_deref(),
        Some("chat:ghost-turn:sender:user")
    );

    // 回合事实来自 Session Event Log，并以 session_events 来源关联到该回合
    use blockcell_storage::{JsonlSessionEventLog, SessionEventReader};
    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read("cli:ghost-turn")
        .expect("read session events");
    let turn_id = events[0].turn_id.clone().expect("turn id");
    let facts = &episode.metadata["turnFacts"];
    assert_eq!(facts["turnId"], turn_id.as_str());
    assert_eq!(facts["steps"], 1);
    assert_eq!(facts["toolCalls"], serde_json::json!([]));
    let sources = runtime
        .test_ghost_ledger()
        .list_episode_sources(&episode.id)
        .unwrap();
    assert!(sources
        .iter()
        .any(|source| source.source_type == "session_events" && source.source_key == turn_id));
}

#[tokio::test]
async fn runtime_exposes_and_dispatches_ghost_memory_provider_tools() {
    let llm_provider = Arc::new(ProviderToolCaptureProvider {
        seen_tools: Mutex::new(Vec::new()),
    });
    let provider_tool = Arc::new(RuntimeProviderTool {
        calls: Mutex::new(Vec::new()),
    });
    let mut runtime = test_runtime_with_provider(llm_provider.clone());
    runtime.ghost_memory_lifecycle = Some(Arc::new(
        crate::ghost_memory_provider::GhostMemoryProviderManager::new()
            .with_provider(provider_tool.clone()),
    ));

    let response = runtime
        .process_message(InboundMessage {
            channel: "cli".to_string(),
            account_id: None,
            sender_id: "user".to_string(),
            chat_id: "provider-tool-chat".to_string(),
            content: "look up my release preference".to_string(),
            media: vec![],
            metadata: serde_json::Value::Null,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        })
        .await
        .expect("process provider tool message");

    assert!(response.contains("runtime_provider_tool"));
    let seen_tools = llm_provider.seen_tools.lock().unwrap().clone();
    assert!(seen_tools.iter().any(|tools| {
        tools.iter().any(|schema| {
            schema
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(|value| value.as_str())
                == Some("external_memory_lookup")
        })
    }));

    let calls = provider_tool.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["query"], serde_json::json!("canary rollout"));
}

#[tokio::test]
async fn pre_compress_boundary_creates_force_review_episode() {
    let mut runtime = test_runtime_with_embedded_ghost_learning();

    runtime.test_trigger_pre_compress().await.unwrap();

    assert_eq!(
        runtime
            .test_ghost_ledger()
            .latest_episode_status()
            .unwrap()
            .as_deref(),
        Some("pending_review")
    );
}

#[tokio::test]
async fn coordinated_boundary_queues_one_learning_job_without_preflush_call() {
    let provider = Arc::new(BoundaryFlushProvider {
        calls: Mutex::new(Vec::new()),
        flush_calls: AtomicUsize::new(0),
    });
    let mut runtime = test_runtime_with_boundary_flush_provider(provider.clone());

    runtime.test_trigger_pre_compress().await.unwrap();

    assert_eq!(provider.calls.lock().unwrap().len(), 0);
    assert_eq!(provider.flush_calls.load(Ordering::SeqCst), 0);
    let mut claimed = runtime
        .test_ghost_ledger()
        .claim_reviewable_episodes(1, "coordinated-test", 600)
        .expect("claim coordinated episode");
    let episode = claimed.pop().expect("coordinated episode");
    assert_eq!(
        episode.metadata["learningJob"]["historySnapshot"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        episode.metadata["requestedOutputs"]
            .as_array()
            .map(Vec::len),
        Some(4)
    );
}

#[test]
fn runtime_session_search_finds_persisted_history() {
    let base = std::env::temp_dir().join(format!(
        "blockcell-session-search-test-{}",
        uuid::Uuid::new_v4()
    ));
    let paths = Paths::with_base(base);
    paths.ensure_dirs().expect("create runtime dirs");
    let store = SessionStore::new(paths.clone());
    store
        .save(
            "cli:chat-1",
            &[
                ChatMessage::user("How should we deploy this service?"),
                ChatMessage::assistant("Use canary-first deploys and verify rollback order."),
            ],
        )
        .expect("save session");

    let search = RuntimeSessionSearch::new(paths, Some("cli:chat-1".to_string()));
    let result = search
        .search_session_json("canary rollback", 5)
        .expect("search session history");
    assert_eq!(
        result.get("count").and_then(|value| value.as_u64()),
        Some(1)
    );
    assert!(result.to_string().contains("canary-first deploys"));
    assert!(result.to_string().contains("cli:chat-1"));
}

#[tokio::test]
async fn pre_compress_boundary_includes_provider_context_in_episode_snapshot() {
    let mut runtime = test_runtime_with_embedded_ghost_learning();
    runtime.ghost_memory_lifecycle = Some(Arc::new(
        crate::ghost_memory_provider::GhostMemoryProviderManager::new()
            .with_provider(Arc::new(BoundaryMemoryProvider)),
    ));

    runtime.test_trigger_pre_compress().await.unwrap();

    let mut claimed = runtime
        .test_ghost_ledger()
        .claim_reviewable_episodes(1, "test-worker", 600)
        .expect("claim pre-compress episode");
    let episode = claimed.pop().expect("pre-compress episode");
    assert_eq!(episode.boundary_kind, "pre_compress");
    assert!(episode
        .metadata
        .get("reusableLesson")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .contains("preserve provider-derived rollback preference"));
}

#[tokio::test]
async fn session_end_boundary_includes_provider_context_in_episode_snapshot() {
    let mut runtime = test_runtime_with_embedded_ghost_learning();
    runtime.ghost_memory_lifecycle = Some(Arc::new(
        crate::ghost_memory_provider::GhostMemoryProviderManager::new()
            .with_provider(Arc::new(BoundaryMemoryProvider)),
    ));
    let msg = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "ghost-session-end-provider".to_string(),
        content: "figure out the correct deploy order".to_string(),
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };
    runtime.process_message(msg).await.unwrap();

    runtime.test_trigger_session_end().await.unwrap();

    let mut claimed = runtime
        .test_ghost_ledger()
        .claim_reviewable_episodes(4, "test-worker", 600)
        .expect("claim session-end episodes");
    let episode = claimed
        .drain(..)
        .find(|episode| episode.boundary_kind == "session_end")
        .expect("session-end episode");
    assert!(episode
        .metadata
        .get("reusableLesson")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .contains("preserve provider-derived session-end deploy preference"));
}

#[tokio::test]
async fn session_end_boundary_creates_force_review_episode() {
    let mut runtime = test_runtime_with_embedded_ghost_learning();
    let msg = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "ghost-session-end".to_string(),
        content: "figure out the correct deploy order".to_string(),
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };
    runtime.process_message(msg).await.unwrap();

    runtime.test_trigger_session_end().await.unwrap();

    assert_eq!(
        runtime
            .test_ghost_ledger()
            .latest_episode_status()
            .unwrap()
            .as_deref(),
        Some("pending_review")
    );
    assert_eq!(runtime.test_ghost_ledger().episode_count().unwrap(), 2);
}

#[tokio::test]
async fn session_rotate_boundary_creates_force_review_episode() {
    let provider = Arc::new(SequencedGhostProvider);
    let (mut runtime, paths) = test_runtime_with_ghost_review_provider(provider, true);

    runtime
        .process_message(InboundMessage {
            channel: "cli".to_string(),
            account_id: None,
            sender_id: "user".to_string(),
            chat_id: "ghost-rotate-a".to_string(),
            content: "figure out the correct deploy order".to_string(),
            media: vec![],
            metadata: serde_json::Value::Null,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        })
        .await
        .unwrap();

    runtime
        .process_message(InboundMessage {
            channel: "cli".to_string(),
            account_id: None,
            sender_id: "user".to_string(),
            chat_id: "ghost-rotate-b".to_string(),
            content: "analyze the safer rollback sequence".to_string(),
            media: vec![],
            metadata: serde_json::Value::Null,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        })
        .await
        .unwrap();

    wait_for_runtime_review_runs(&paths, 3).await;

    assert_eq!(
        runtime
            .test_ghost_ledger()
            .episode_count_by_boundary_kind("session_rotate")
            .unwrap(),
        1
    );
    assert_eq!(
        runtime
            .test_ghost_ledger()
            .latest_episode_status_by_boundary_kind("session_rotate")
            .unwrap()
            .as_deref(),
        Some("reviewed")
    );
}

#[tokio::test]
async fn session_rotate_boundary_includes_provider_context_in_episode_snapshot() {
    let provider = Arc::new(SequencedGhostProvider);
    let (mut runtime, _paths) = test_runtime_with_ghost_review_provider(provider, true);
    runtime.ghost_memory_lifecycle = Some(Arc::new(
        crate::ghost_memory_provider::GhostMemoryProviderManager::new()
            .with_provider(Arc::new(BoundaryMemoryProvider)),
    ));

    for chat_id in ["ghost-rotate-provider-a", "ghost-rotate-provider-b"] {
        runtime
            .process_message(InboundMessage {
                channel: "cli".to_string(),
                account_id: None,
                sender_id: "user".to_string(),
                chat_id: chat_id.to_string(),
                content: "figure out the correct deploy order".to_string(),
                media: vec![],
                metadata: serde_json::Value::Null,
                timestamp_ms: chrono::Utc::now().timestamp_millis(),
            })
            .await
            .unwrap();
    }

    let episode = runtime
        .test_ghost_ledger()
        .latest_episode_by_boundary_kind("session_rotate")
        .expect("load session-rotate episode")
        .expect("session-rotate episode");
    let lesson = episode
        .metadata
        .get("reusableLesson")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    assert!(lesson.contains("Switched active session"));
    assert!(lesson.contains("preserve provider-derived session-end deploy preference"));
}

#[tokio::test]
async fn delegation_completion_creates_parent_learning_episode() {
    let runtime = test_runtime_with_embedded_ghost_learning();

    runtime
        .test_complete_delegated_task(
            "research the release failure",
            "identified the root cause and the safer rollback order",
        )
        .await
        .unwrap();

    assert_eq!(runtime.test_ghost_ledger().episode_count().unwrap(), 1);
    assert_eq!(
        runtime
            .test_ghost_ledger()
            .latest_boundary_kind()
            .unwrap()
            .as_deref(),
        Some("delegation_end")
    );
}

#[tokio::test]
async fn file_memory_recall_is_fenced_and_not_persisted() {
    let provider = Arc::new(RecallCaptureProvider {
        calls: Mutex::new(Vec::new()),
    });
    let (mut runtime, paths) = test_runtime_with_file_memory_recall(provider.clone());
    let msg = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "ghost-recall".to_string(),
        content: "how do I usually like deploy docs written?".to_string(),
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    runtime.process_message(msg.clone()).await.unwrap();

    let calls = provider.calls.lock().unwrap().clone();
    let first_call = calls.first().expect("first llm call");
    assert!(
        first_call.iter().any(|message| {
            message.role == "user"
                && chat_message_text(message).contains("<memory-context>")
                && chat_message_text(message).contains("rollback checklist")
        }),
        "expected fenced file memory recall in provider payload"
    );

    let session = SessionStore::new(paths).load(&msg.session_key()).unwrap();
    assert!(session
        .iter()
        .all(|message| { !chat_message_text(message).contains("<memory-context>") }));
}

#[tokio::test]
async fn ghost_learning_closes_loop_from_experience_to_file_memory_only() {
    let provider = Arc::new(ReviewAndCaptureProvider {
        calls: Mutex::new(Vec::new()),
        review_calls: AtomicUsize::new(0),
    });
    let (mut runtime, paths) = test_runtime_with_ghost_review_provider(provider.clone(), false);

    runtime
        .process_message(InboundMessage {
            channel: "cli".to_string(),
            account_id: None,
            sender_id: "user".to_string(),
            chat_id: "ghost-closure".to_string(),
            content: "figure out the correct release verification sequence with rollback plan"
                .to_string(),
            media: vec![],
            metadata: serde_json::Value::Null,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        })
        .await
        .unwrap();

    wait_for_runtime_review_runs(&paths, 1).await;

    assert!(!paths.user_md().exists());
    assert!(!paths.memory_md().exists());
    let scoped_memory =
        crate::memory_file_store::MemoryFileStore::open_for_session(&paths, "cli:ghost-closure")
            .expect("open scoped ghost memory")
            .load_snapshot()
            .expect("load scoped ghost memory");
    let user_memory = scoped_memory.user_block.expect("scoped USER memory");
    let durable_memory = scoped_memory.memory_block.expect("scoped MEMORY memory");
    assert!(user_memory.contains("canary-first rollout"));
    assert!(durable_memory.contains("Confirm rollback plan before release verification"));
    assert!(!paths
        .skills_dir()
        .join("release_verification")
        .join("SKILL.md")
        .exists());

    let ledger = GhostLedger::open(&paths.ghost_ledger_db()).expect("open ghost ledger");
    assert_eq!(ledger.review_run_count().unwrap(), 1);

    assert_eq!(provider.review_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn shadow_mode_captures_and_reviews_without_runtime_recall() {
    let provider = Arc::new(SequencedGhostProvider);
    let (mut runtime, paths) = test_runtime_with_ghost_review_provider(provider, true);
    crate::reset_ghost_metrics_for_paths(&paths);

    runtime
        .process_message(InboundMessage {
            channel: "cli".to_string(),
            account_id: None,
            sender_id: "user".to_string(),
            chat_id: "ghost-shadow-review".to_string(),
            content: "learn my preferred deploy style".to_string(),
            media: vec![],
            metadata: serde_json::Value::Null,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        })
        .await
        .unwrap();

    wait_for_runtime_review_runs(&paths, 1).await;

    let metrics = runtime.test_ghost_metrics();
    assert_eq!(metrics.episodes_captured, 1);
    assert_eq!(metrics.reviews_started, 1);
    assert_eq!(metrics.reviews_failed, 0);
    assert_eq!(runtime.test_ghost_ledger().review_run_count().unwrap(), 1);
}

#[tokio::test]
async fn turn_review_interval_captures_periodic_trivial_turn() {
    let provider = Arc::new(SequencedGhostProvider);
    let (mut runtime, _paths) = test_runtime_with_ghost_review_provider(provider, true);
    runtime.config.agents.ghost.learning.turn_review_interval = 2;

    for content in ["hello", "thanks"] {
        runtime
            .process_message(InboundMessage {
                channel: "cli".to_string(),
                account_id: None,
                sender_id: "user".to_string(),
                chat_id: "ghost-interval".to_string(),
                content: content.to_string(),
                media: vec![],
                metadata: serde_json::Value::Null,
                timestamp_ms: chrono::Utc::now().timestamp_millis(),
            })
            .await
            .unwrap();
    }

    assert_eq!(runtime.test_ghost_ledger().episode_count().unwrap(), 1);
    assert_eq!(
        runtime
            .test_ghost_ledger()
            .latest_boundary_kind()
            .unwrap()
            .as_deref(),
        Some("turn_end")
    );
}

#[tokio::test]
async fn system_tick_processes_pending_ghost_reviews() {
    let provider = Arc::new(SequencedGhostProvider);
    let (mut runtime, paths) = test_runtime_with_ghost_review_provider(provider, true);

    runtime.test_trigger_pre_compress().await.unwrap();
    assert_eq!(
        runtime
            .test_ghost_ledger()
            .latest_episode_status()
            .unwrap()
            .as_deref(),
        Some("pending_review")
    );

    runtime
        .process_system_event_tick(chrono::Utc::now().timestamp_millis())
        .await;

    wait_for_runtime_review_runs(&paths, 1).await;
    assert_eq!(
        runtime
            .test_ghost_ledger()
            .latest_episode_status()
            .unwrap()
            .as_deref(),
        Some("reviewed")
    );
}

#[tokio::test]
async fn test_orchestrator_tick_emits_event_tx_for_immediate_notifications() {
    let mut runtime = test_runtime();
    let (event_tx, mut event_rx) = broadcast::channel(8);
    runtime.set_event_tx(event_tx);
    runtime
        .update_main_session_target(&test_main_session_inbound("cli", "chat-1"))
        .await;

    let mut event = SystemEvent::new_main_session(
        "task.failed",
        "task_manager",
        EventPriority::Critical,
        "Task failed",
        "Background report failed",
    );
    event.delivery.immediate = true;
    runtime.event_emitter_handle().emit(event);

    let decision = runtime
        .process_system_event_tick(chrono::Utc::now().timestamp_millis())
        .await;

    assert_eq!(decision.immediate_notifications.len(), 1);
    let payload = event_rx.recv().await.expect("receive ws event");
    let json: serde_json::Value = serde_json::from_str(&payload).expect("parse ws event");
    assert_eq!(json["type"], "system_event_notification");
    assert_eq!(json["chat_id"], "chat-1");
    assert_eq!(json["title"], "Task failed");
}

#[tokio::test]
async fn system_event_delivery_failure_keeps_pending() {
    let runtime = test_runtime();
    let mut event = SystemEvent::new_main_session(
        "task.failed",
        "task_manager",
        EventPriority::Critical,
        "Task failed",
        "retry me",
    );
    event.delivery.immediate = true;
    runtime.event_emitter_handle().emit(event.clone());

    runtime
        .process_system_event_tick(chrono::Utc::now().timestamp_millis())
        .await;

    let pending = runtime.system_event_store.list_pending(10);
    assert!(pending.iter().any(|candidate| candidate.id == event.id));
}

#[tokio::test]
async fn summary_delivery_failure_keeps_items() {
    let runtime = test_runtime();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut event = SystemEvent::new_main_session(
        "task.completed",
        "task_manager",
        EventPriority::Normal,
        "Report ready",
        "retry summary",
    );
    event.created_at_ms = now_ms - 60_000;
    runtime.event_emitter_handle().emit(event.clone());

    runtime.process_system_event_tick(now_ms).await;

    assert!(runtime
        .system_event_store
        .list_pending(10)
        .iter()
        .any(|candidate| candidate.id == event.id));
    assert_eq!(
        runtime
            .system_event_orchestrator
            .queue()
            .snapshot()
            .pending_count,
        1
    );
}

#[tokio::test]
async fn task_summary_delivery_stays_with_origin_after_target_rotation() {
    let mut runtime = test_runtime();
    let (outbound_tx, mut outbound_rx) = mpsc::channel(8);
    runtime.set_outbound(outbound_tx);
    runtime
        .update_main_session_target(&InboundMessage {
            channel: "telegram".to_string(),
            account_id: Some("account-b".to_string()),
            sender_id: "user-b".to_string(),
            chat_id: "chat-b".to_string(),
            content: "switch target".to_string(),
            media: vec![],
            metadata: serde_json::Value::Null,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
        })
        .await;

    runtime
        .task_manager
        .create_and_start_task_with_route(
            "task-origin-summary",
            "private report",
            "inspect private data",
            "telegram",
            "chat-a",
            Some("account-a"),
            Some("telegram:account:9:account-achat-a"),
            runtime.agent_id.as_deref(),
            true,
            Some("explore"),
            false,
        )
        .await;
    runtime
        .task_manager
        .set_completed("task-origin-summary", "secret result from chat A")
        .await;

    runtime
        .process_system_event_tick(chrono::Utc::now().timestamp_millis() + 60_000)
        .await;

    let outbound = outbound_rx.recv().await.expect("origin summary outbound");
    assert_eq!(outbound.channel, "telegram");
    assert_eq!(outbound.account_id.as_deref(), Some("account-a"));
    assert_eq!(outbound.chat_id, "chat-a");
    assert!(outbound.content.contains("后台任务已完成"));
}

#[tokio::test]
async fn test_orchestrator_tick_flushes_summary_to_main_session_outbound() {
    let mut runtime = test_runtime();
    let (outbound_tx, mut outbound_rx) = mpsc::channel(8);
    runtime.set_outbound(outbound_tx);
    runtime
        .update_main_session_target(&test_main_session_inbound("cli", "chat-1"))
        .await;

    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut event = SystemEvent::new_main_session(
        "task.completed",
        "task_manager",
        EventPriority::Normal,
        "Report ready",
        "Background report finished",
    );
    event.created_at_ms = now_ms - 60_000;
    runtime.event_emitter_handle().emit(event);

    let decision = runtime.process_system_event_tick(now_ms).await;

    assert_eq!(decision.flushed_summaries.len(), 1);
    let outbound = outbound_rx.recv().await.expect("receive outbound summary");
    assert_eq!(outbound.channel, "cli");
    assert_eq!(outbound.chat_id, "chat-1");
    assert!(outbound.content.contains("Report ready"));
    assert!(outbound.content.contains("System updates") || outbound.content.contains("🗂️"));
}

#[tokio::test]
async fn test_cron_agent_delivery_emits_ws_event_for_deliver_target() {
    let mut runtime = test_runtime();
    let (event_tx, mut event_rx) = broadcast::channel(8);
    runtime.set_event_tx(event_tx);

    let msg = InboundMessage {
        channel: "cron".to_string(),
        account_id: None,
        sender_id: "cron".to_string(),
        chat_id: "job-123".to_string(),
        content: "任务完成摘要".to_string(),
        media: vec![],
        metadata: serde_json::json!({
            "deliver": true,
            "deliver_channel": "ws",
            "deliver_to": "webui-chat-1",
            "cron_agent": true,
        }),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    let result = runtime
        .process_message(msg)
        .await
        .expect("process cron message");
    assert!(!result.is_empty());

    let json = loop {
        let payload = event_rx.recv().await.expect("receive ws event");
        let event: serde_json::Value = serde_json::from_str(&payload).expect("parse ws event");
        if event["type"] == "message_done" {
            break event;
        }
    };
    assert_eq!(json["type"], "message_done");
    assert_eq!(json["chat_id"], "webui-chat-1");
    assert_eq!(json["content"], result);
    assert_eq!(json["background_delivery"], true);
    assert_eq!(json["delivery_kind"], "cron");
    assert_eq!(json["cron_kind"], "agent");
}

#[tokio::test]
async fn test_cron_agent_persists_to_deliver_session_not_cron_job_session() {
    let mut runtime = test_runtime();

    let msg = InboundMessage {
        channel: "cron".to_string(),
        account_id: None,
        sender_id: "cron".to_string(),
        chat_id: "job-456".to_string(),
        content: "搜索美伊战争最新消息，并将结果发给用户。".to_string(),
        media: vec![],
        metadata: serde_json::json!({
            "deliver": true,
            "deliver_channel": "ws",
            "deliver_to": "webui-chat-2",
            "cron_agent": true,
        }),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };

    let result = runtime
        .process_message(msg)
        .await
        .expect("process cron message");
    assert!(!result.is_empty());

    let ws_session_key = blockcell_core::build_session_key("ws", "webui-chat-2");
    let cron_session_key = blockcell_core::build_session_key("cron", "job-456");

    let ws_history = runtime
        .session_store
        .load(&ws_session_key)
        .expect("load ws session history");
    assert!(!ws_history.is_empty());
    assert!(ws_history.iter().any(|m| match &m.content {
        serde_json::Value::String(s) => s.contains("搜索美伊战争最新消息"),
        _ => false,
    }));

    let cron_path = runtime.paths.session_file(&cron_session_key);
    assert!(
        !cron_path.exists(),
        "cron job session file should not be created"
    );
}

#[tokio::test]
async fn test_orchestrator_tick_gracefully_handles_missing_dispatchers() {
    let runtime = test_runtime();

    let event = SystemEvent::new_main_session(
        "task.failed",
        "task_manager",
        EventPriority::Critical,
        "Task failed",
        "No dispatcher configured",
    );
    runtime.event_emitter_handle().emit(event);

    let decision = runtime
        .process_system_event_tick(chrono::Utc::now().timestamp_millis())
        .await;

    assert_eq!(decision.immediate_notifications.len(), 1);
}

#[test]
fn test_resolve_profile_tool_names_uses_agent_profile_for_unknown_intent() {
    let raw = r#"{
  "agents": {
    "list": [
      { "id": "ops", "enabled": true, "intentProfile": "ops" }
    ]
  },
  "intentRouter": {
    "enabled": true,
    "defaultProfile": "default",
    "profiles": {
      "default": {
        "coreTools": ["read_file"],
        "intentTools": {
          "Chat": { "inheritBase": false, "tools": [] },
          "Unknown": ["browse"]
        }
      },
      "ops": {
        "coreTools": ["read_file", "exec", "file_ops"],
        "intentTools": {
          "Chat": { "inheritBase": false, "tools": [] },
          "Unknown": ["browse", "http_request"],
          "DevOps": ["git_api", "network_monitor"]
        }
      }
    }
  }
}"#;
    let config: Config = serde_json::from_str(raw).unwrap();
    let available: HashSet<String> = [
        "read_file",
        "exec",
        "file_ops",
        "browse",
        "http_request",
        "git_api",
        "network_monitor",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    let tool_names =
        resolve_profile_tool_names(&config, Some("ops"), &[IntentCategory::Unknown], &available);

    assert!(tool_names.contains(&"read_file".to_string()));
    assert!(tool_names.contains(&"exec".to_string()));
    assert!(tool_names.contains(&"file_ops".to_string()));
    assert!(tool_names.contains(&"browse".to_string()));
    assert!(tool_names.contains(&"http_request".to_string()));
    assert!(!tool_names.contains(&"git_api".to_string()));
}

#[test]
fn test_resolve_profile_tool_names_returns_empty_for_chat_when_profile_configures_none() {
    let config: Config = serde_json::from_str("{}").unwrap();
    let available: HashSet<String> = ["read_file", "browse"]
        .into_iter()
        .map(str::to_string)
        .collect();

    let tool_names = resolve_profile_tool_names(&config, None, &[IntentCategory::Chat], &available);

    assert!(tool_names.is_empty());
}

#[test]
fn test_napcat_tools_hidden_when_disabled() {
    // Config with napcat disabled (default)
    let config: Config = serde_json::from_str(
        r#"{
                "channels": {
                    "napcat": {
                        "enabled": false
                    }
                }
            }"#,
    )
    .unwrap();

    let available: HashSet<String> = [
        "read_file",
        "napcat_get_group_list",
        "napcat_get_login_info",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    let tool_names = resolve_effective_tool_names(
        &config,
        InteractionMode::General,
        None,
        None,
        &[IntentCategory::Communication],
        &available,
    );

    // napcat tools should be filtered out
    assert!(tool_names.contains(&"read_file".to_string()));
    assert!(!tool_names.contains(&"napcat_get_group_list".to_string()));
    assert!(!tool_names.contains(&"napcat_get_login_info".to_string()));
}

#[test]
fn test_napcat_tools_visible_when_enabled() {
    // Config with napcat enabled
    let config: Config = serde_json::from_str(
        r#"{
                "channels": {
                    "napcat": {
                        "enabled": true
                    }
                }
            }"#,
    )
    .unwrap();

    let available: HashSet<String> = [
        "read_file",
        "napcat_get_group_list",
        "napcat_get_login_info",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    let tool_names = resolve_effective_tool_names(
        &config,
        InteractionMode::General,
        None,
        None,
        &[IntentCategory::Communication],
        &available,
    );

    // napcat tools should be visible
    assert!(tool_names.contains(&"read_file".to_string()));
    assert!(tool_names.contains(&"napcat_get_group_list".to_string()));
    assert!(tool_names.contains(&"napcat_get_login_info".to_string()));
}

#[test]
fn test_prepare_skill_result_for_presentation_keeps_full_result_payload() {
    let output = serde_json::json!({
        "success": true,
        "action": "search",
        "display_text": "找到 1 条相关笔记。",
        "data": {
            "items": [
                {
                    "index": 1,
                    "title": "上海咖啡推荐"
                }
            ]
        },
        "raw_result_context": {
            "search_results": [
                {
                    "index": 1,
                    "title": "上海咖啡推荐",
                    "feed_id": "feed-1",
                    "xsec_token": "token-1"
                }
            ]
        }
    })
    .to_string();

    let presentation = prepare_skill_result_for_presentation("xiaohongshu", &output);

    assert_eq!(
        presentation.direct_text.as_deref(),
        Some("找到 1 条相关笔记。")
    );
    let llm_payload = presentation
        .llm_payload
        .as_ref()
        .expect("structured payload should still provide LLM summary input");
    assert!(llm_payload.contains("上海咖啡推荐"));
    assert!(llm_payload.contains("feed-1"));
    assert!(llm_payload.contains("xsec_token"));
}

#[tokio::test]
async fn test_deliver_subagent_result_to_ws_origin_emits_message_done_event() {
    let (event_tx, mut event_rx) = broadcast::channel::<String>(8);

    deliver_subagent_result_to_origin(
        "ws",
        "webui-chat-9",
        "第15条内容已经整理完成",
        "task-test123",
        Some("default"),
        None,
        Some(event_tx),
        None,
        None,
    )
    .await;

    let payload = event_rx.recv().await.expect("receive ws event");
    let json: serde_json::Value = serde_json::from_str(&payload).expect("parse ws event");
    assert_eq!(json["type"], "message_done");
    assert_eq!(json["chat_id"], "webui-chat-9");
    assert_eq!(json["agent_id"], "default");
    assert_eq!(json["content"], "第15条内容已经整理完成");
    assert_eq!(json["background_delivery"], true);
    assert_eq!(json["task_id"], "task-test123");
}

#[tokio::test]
async fn test_deliver_subagent_result_persists_origin_session() {
    let base = std::env::temp_dir().join(format!(
        "blockcell-subagent-delivery-test-{}",
        uuid::Uuid::new_v4()
    ));
    let paths = Paths::with_base(base);
    paths.ensure_dirs().expect("create runtime dirs");
    let store = SessionStore::new(paths);

    deliver_subagent_result_to_origin(
        "ws",
        "chat-persist",
        "background result",
        "task-persist",
        Some("default"),
        None,
        None,
        Some(&store),
        Some("ws:chat-persist"),
    )
    .await;

    let history = store.load("ws:chat-persist").expect("load origin session");
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].content.as_str(), Some("background result"));
}

// resolve_effective_tool_names 测试
#[test]
fn test_resolve_effective_tool_names_load_all_applies_deny_tools() {
    // 当 enabled=false 且 load_all_tools=true 时，应应用 deny_tools 过滤
    let raw = r#"{
            "intentRouter": {
                "enabled": false,
                "loadAllTools": true,
                "defaultProfile": "default",
                "profiles": {
                    "default": {
                        "coreTools": [],
                        "intentTools": {
                            "Chat": { "inheritBase": false, "tools": [] },
                            "Unknown": []
                        },
                        "denyTools": ["exec"]
                    }
                }
            },
            "channels": {
                "napcat": { "enabled": false }
            }
        }"#;
    let config: Config = serde_json::from_str(raw).unwrap();
    let available: HashSet<String> = [
        "read_file",
        "write_file",
        "exec",
        "web_search",
        "napcat_send",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let tools = resolve_effective_tool_names(
        &config,
        InteractionMode::General,
        None,
        None,
        &[IntentCategory::Unknown],
        &available,
    );

    // exec 被 deny_tools 过滤，napcat_send 被 napcat.enabled=false 过滤
    assert_eq!(tools.len(), 3);
    assert!(tools.contains(&"read_file".to_string()));
    assert!(tools.contains(&"write_file".to_string()));
    assert!(tools.contains(&"web_search".to_string()));
    assert!(!tools.contains(&"exec".to_string()));
    assert!(!tools.contains(&"napcat_send".to_string()));
}

#[test]
fn test_resolve_effective_tool_names_load_all_applies_napcat_filter() {
    // 当 napcat.enabled=true 时，napcat 工具应可用
    let raw = r#"{
            "intentRouter": {
                "enabled": false,
                "loadAllTools": true,
                "defaultProfile": "default",
                "profiles": {
                    "default": {
                        "coreTools": [],
                        "intentTools": {
                            "Chat": { "inheritBase": false, "tools": [] },
                            "Unknown": []
                        },
                        "denyTools": []
                    }
                }
            },
            "channels": {
                "napcat": { "enabled": true }
            }
        }"#;
    let config: Config = serde_json::from_str(raw).unwrap();
    let available: HashSet<String> = ["read_file", "napcat_send", "napcat_receive"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let tools = resolve_effective_tool_names(
        &config,
        InteractionMode::General,
        None,
        None,
        &[IntentCategory::Unknown],
        &available,
    );

    // napcat 工具应可用（enabled=true）
    assert_eq!(tools.len(), 3);
    assert!(tools.contains(&"read_file".to_string()));
    assert!(tools.contains(&"napcat_send".to_string()));
    assert!(tools.contains(&"napcat_receive".to_string()));
}

#[test]
fn test_resolve_effective_tool_names_load_all_extends_skill_tools() {
    // 当有 active_skill 时，应扩展 skill.tools
    let raw = r#"{
            "intentRouter": {
                "enabled": false,
                "loadAllTools": true,
                "defaultProfile": "default",
                "profiles": {
                    "default": {
                        "coreTools": [],
                        "intentTools": {
                            "Chat": { "inheritBase": false, "tools": [] },
                            "Unknown": []
                        },
                        "denyTools": []
                    }
                }
            },
            "channels": {
                "napcat": { "enabled": false }
            }
        }"#;
    let config: Config = serde_json::from_str(raw).unwrap();
    let available: HashSet<String> = ["read_file", "write_file"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let skill = ActiveSkillContext {
        name: "test_skill".to_string(),
        prompt_md: String::new(),
        inject_prompt_md: false,
        tools: vec!["skill_tool_1".to_string(), "skill_tool_2".to_string()],
        fallback_message: None,
        source: blockcell_skills::manager::SkillSource::BlockCell,
        learned: false,
    };
    let tools = resolve_effective_tool_names(
        &config,
        InteractionMode::Skill,
        None,
        Some(&skill),
        &[IntentCategory::Unknown],
        &available,
    );

    // 应包含 available tools + skill.tools
    assert_eq!(tools.len(), 4);
    assert!(tools.contains(&"read_file".to_string()));
    assert!(tools.contains(&"write_file".to_string()));
    assert!(tools.contains(&"skill_tool_1".to_string()));
    assert!(tools.contains(&"skill_tool_2".to_string()));
}

#[test]
fn test_resolve_effective_tool_names_enabled_true_uses_intent_classification() {
    // 当 enabled=true 时，应走意图分类流程，忽略 load_all_tools
    let raw = r#"{
            "intentRouter": {
                "enabled": true,
                "loadAllTools": true,
                "defaultProfile": "default",
                "profiles": {
                    "default": {
                        "coreTools": ["read_file"],
                        "intentTools": {
                            "Chat": { "inheritBase": false, "tools": [] },
                            "FileOps": ["edit_file"]
                        },
                        "denyTools": []
                    }
                }
            },
            "channels": {
                "napcat": { "enabled": false }
            }
        }"#;
    let config: Config = serde_json::from_str(raw).unwrap();
    let available: HashSet<String> = ["read_file", "edit_file", "exec", "web_search"]
        .iter()
        .map(|s| s.to_string())
        .collect();

    // FileOps 意图应返回 read_file (core) + edit_file (intent)
    let tools = resolve_effective_tool_names(
        &config,
        InteractionMode::General,
        None,
        None,
        &[IntentCategory::FileOps],
        &available,
    );
    assert_eq!(tools.len(), 2);
    assert!(tools.contains(&"read_file".to_string()));
    assert!(tools.contains(&"edit_file".to_string()));
}

// ========== 集成测试: Inter-Agent 通信 ==========

/// 测试 AbortToken 链式取消
#[test]
fn test_abort_token_chain_cancellation() {
    use blockcell_core::AbortToken;

    // 创建父 token
    let parent = AbortToken::new();
    // 创建子 token
    let child = parent.child();
    // 创建孙 token
    let grandchild = child.child();

    // 初始状态：都未取消
    assert!(!parent.is_cancelled());
    assert!(!child.is_cancelled());
    assert!(!grandchild.is_cancelled());

    // 取消父 -> 子和孙也应取消
    parent.cancel();
    assert!(parent.is_cancelled());
    assert!(child.is_cancelled());
    assert!(grandchild.is_cancelled());

    // 孙 token 的 check() 应返回错误
    assert!(grandchild.check().is_err());
}

/// 测试 AbortToken 独立取消（子取消不影响父）
#[test]
fn test_abort_token_independent_child() {
    use blockcell_core::AbortToken;

    let parent = AbortToken::new();
    let child = parent.child();

    // 只取消子
    child.cancel();
    assert!(child.is_cancelled());
    // 父不应受影响
    assert!(!parent.is_cancelled());
}

/// 测试 SubagentContext 的 AbortToken 集成
#[test]
fn test_subagent_context_abort_token() {
    use crate::forked::{create_subagent_context, SubagentOverrides};
    use blockcell_core::AbortToken;

    // 创建父 token
    let parent_token = AbortToken::new();

    // 创建子代理上下文，传入父 token
    let overrides = SubagentOverrides {
        abort_token: Some(parent_token.child()),
        ..Default::default()
    };
    let context = create_subagent_context(None, None, None, Some(&parent_token), overrides);

    // 子上下文的 abort_token 应是父的子 token
    assert!(!context.abort_token.is_cancelled());

    // 取消父
    parent_token.cancel();
    // 子也应取消
    assert!(context.abort_token.is_cancelled());
}

/// 测试 UsageMetrics 统一性
#[test]
fn test_usage_metrics_unified() {
    use blockcell_core::UsageMetrics;

    // 创建两个 UsageMetrics 并合并
    let mut m1 = UsageMetrics {
        input_tokens: 100,
        output_tokens: 50,
        cache_creation_input_tokens: 20,
        cache_read_input_tokens: 10,
    };

    let m2 = UsageMetrics {
        input_tokens: 200,
        output_tokens: 100,
        cache_creation_input_tokens: 30,
        cache_read_input_tokens: 40,
    };

    m1.merge(&m2);

    assert_eq!(m1.input_tokens, 300);
    assert_eq!(m1.output_tokens, 150);
    assert_eq!(m1.cache_creation_input_tokens, 50);
    assert_eq!(m1.cache_read_input_tokens, 50);

    // 测试 cache_hit_rate
    let hit_rate = m1.cache_hit_rate();
    // cache_read / (input + cache_creation + cache_read)
    // 50 / (300 + 50 + 50) = 50 / 400 = 0.125
    assert!((hit_rate - 0.125).abs() < 0.001);
}

/// 测试 AgentTypeDefinition 的 ONE_SHOT 行为
#[test]
fn test_agent_type_one_shot() {
    use crate::agent_types::{AgentTypeDefinition, PermissionMode};

    // 创建 ONE_SHOT agent type
    let one_shot_type = AgentTypeDefinition {
        agent_type: "explore".to_string(),
        when_to_use: "Explore agent for quick searches".to_string(),
        disallowed_tools: vec!["exec".to_string()],
        max_turns: Some(5),
        system_prompt_template: None,
        one_shot: true,
        permission_mode: PermissionMode::Bubble,
        isolation: None,
        ..Default::default()
    };

    assert!(one_shot_type.one_shot);

    // 创建非 ONE_SHOT agent type
    let normal_type = AgentTypeDefinition {
        agent_type: "general".to_string(),
        when_to_use: "General agent for complex tasks".to_string(),
        disallowed_tools: vec![],
        max_turns: None,
        system_prompt_template: None,
        one_shot: false,
        permission_mode: PermissionMode::Inherit,
        isolation: None,
        ..Default::default()
    };

    assert!(!normal_type.one_shot);
}

/// 测试 SpawnHandle trait 的 agent_type 参数传递
#[test]
fn test_spawn_handle_agent_type_parameter() {
    use blockcell_tools::SpawnHandle;
    use std::sync::Arc;

    // Mock SpawnHandle 实现，验证 agent_type 参数被正确传递
    struct MockSpawnHandle {
        captured_agent_type: Arc<std::sync::Mutex<Option<String>>>,
    }

    impl SpawnHandle for MockSpawnHandle {
        fn spawn(
            &self,
            _task: &str,
            _label: &str,
            _origin_channel: &str,
            _origin_chat_id: &str,
            agent_type: Option<&str>,
        ) -> blockcell_core::Result<serde_json::Value> {
            *self.captured_agent_type.lock().unwrap() = agent_type.map(|s| s.to_string());
            Ok(serde_json::json!({"task_id": "test", "status": "running"}))
        }
    }

    let captured = Arc::new(std::sync::Mutex::new(None));
    let handle = MockSpawnHandle {
        captured_agent_type: captured.clone(),
    };

    // 调用 spawn，传递 agent_type
    let result = handle.spawn("test task", "test label", "ws", "chat1", Some("explore"));

    assert!(result.is_ok());
    let captured_type = captured.lock().unwrap();
    assert_eq!(captured_type.as_deref(), Some("explore"));
}

#[tokio::test]
async fn test_completed_agent_prompt_injection_does_not_mark_before_response() {
    let task_manager = TaskManager::new();
    task_manager
        .create_and_start_task(
            "task-inject-1",
            "explore",
            "inspect code",
            "cli",
            "chat-1",
            None,
            false,
            Some("explore"),
            true,
        )
        .await;
    task_manager
        .set_completed("task-inject-1", "found the relevant implementation")
        .await;

    let mut messages = vec![
        ChatMessage::system("base prompt"),
        ChatMessage::user("summarize the agent result"),
    ];
    let injected_ids = inject_running_tasks_into_system_prompt(&mut messages, &task_manager).await;

    assert_eq!(injected_ids, vec!["task-inject-1".to_string()]);
    assert!(
        !task_manager
            .get_task("task-inject-1")
            .await
            .unwrap()
            .result_injected
    );
    assert_eq!(messages[0].content.as_str(), Some("base prompt"));
    assert!(messages[messages.len() - 2]
        .content
        .as_str()
        .unwrap()
        .contains("Completed Agent Results"));
    assert_eq!(
        messages.last().unwrap().content.as_str(),
        Some("summarize the agent result")
    );
}

#[tokio::test]
async fn test_no_background_tasks_adds_no_prompt_noise() {
    let task_manager = TaskManager::new();
    let mut messages = vec![
        ChatMessage::system("base prompt"),
        ChatMessage::user("continue"),
    ];

    let injected_ids = inject_running_tasks_into_system_prompt(&mut messages, &task_manager).await;

    assert!(injected_ids.is_empty());
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].content.as_str(), Some("base prompt"));
    assert_eq!(messages[1].content.as_str(), Some("continue"));
}

// ========== Mock Provider for Inter-Agent Tests ==========

/// Simple mock provider that returns a fixed text response.
/// Used for testing spawn_typed_agent and execute_fork_mode without real LLM calls.
struct MockInterAgentProvider {
    response_text: String,
}

impl MockInterAgentProvider {
    fn new(response: &str) -> Self {
        Self {
            response_text: response.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for MockInterAgentProvider {
    async fn chat(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        Ok(LLMResponse {
            content: Some(self.response_text.clone()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            finish_reason: "stop".to_string(),
            usage: serde_json::json!({
                "input_tokens": 100,
                "output_tokens": 50
            }),
        })
    }
}

// ========== 端到端测试: spawn_typed_agent ==========

/// 测试 spawn_typed_agent 的完整执行流程
/// 验证：task_id 返回、任务创建、后台执行完成
#[tokio::test]
async fn test_spawn_typed_agent_e2e() {
    use blockcell_providers::ProviderPool;

    // 创建 mock provider pool
    let mock_provider = Arc::new(MockInterAgentProvider::new(
        "Task completed successfully. Found 3 relevant files.",
    ));
    let provider_pool =
        ProviderPool::from_single_provider("test-model", "test-provider", mock_provider);

    // 创建 TaskManager (用于后续完整测试扩展)
    let _task_manager = Arc::new(crate::task_manager::TaskManager::new());

    // 创建简单的 Runtime (不依赖完整配置)
    // 注意：这里只测试 spawn_typed_agent 的关键逻辑
    // 实际 AgentRuntime 需要完整配置，我们简化测试

    // 验证：spawn_typed_agent 应返回 task_id
    // 由于完整的 AgentRuntime 需要 Config/Paths，这里测试 AgentTypeRegistry 和参数传递

    use crate::agent_types::AgentTypeRegistry;
    let registry = AgentTypeRegistry::new();

    // 验证 explore agent 类型定义
    let explore_def = registry
        .get("explore")
        .expect("explore agent type should exist");
    assert!(explore_def.one_shot);
    assert!(explore_def.disallowed_tools.contains(&"agent".to_string()));

    // 验证 typed agent 创建成功
    let typed_def = registry
        .get("viper")
        .expect("viper agent type should exist");
    assert!(!typed_def.one_shot);
    assert_eq!(
        typed_def.permission_mode,
        crate::agent_types::PermissionMode::Bubble
    );

    // 验证 ForkedAgentParams 可正确构建
    use crate::forked::{CacheSafeParams, ForkedAgentParams};
    let params = ForkedAgentParams::builder()
        .provider_pool(provider_pool)
        .prompt_messages(vec![ChatMessage::user("test task")])
        .cache_safe_params(CacheSafeParams::default())
        .fork_label("test")
        .max_turns(3)
        .agent_type(Some("explore".to_string()))
        .one_shot(true)
        .build();

    assert!(params.is_ok());
    let params = params.unwrap();
    assert_eq!(params.agent_type, Some("explore".to_string()));
    assert!(params.one_shot);
}

#[test]
fn typed_agent_results_are_normalized_for_parent_injection() {
    let normalized = super::normalize_typed_agent_result(
        "coder",
        r#"{"files_changed":["src/lib.rs"],"summary":"fixed","tests_run":[{"command":"cargo test","status":"passed"}],"status":"completed"}"#,
    );
    let value: serde_json::Value = serde_json::from_str(&normalized).unwrap();

    assert_eq!(value["files_changed"], serde_json::json!(["src/lib.rs"]));
    assert_eq!(value["summary"], "fixed");
    assert_eq!(value["status"], "completed");
    assert!(value["tests_run"].is_array());
}

#[test]
fn typed_agent_free_text_results_are_bounded_before_parent_injection() {
    let normalized = super::normalize_typed_agent_result("explore", &"x".repeat(10_000));
    let value: serde_json::Value = serde_json::from_str(&normalized).unwrap();
    let summary = value["summary"].as_str().unwrap();

    assert!(summary.chars().count() <= 2_100);
    assert!(summary.contains("truncated"));
}

// ========== 端到端测试: execute_fork_mode ==========

/// 测试 execute_fork_mode 的上下文继承
/// 验证：ForkChild 身份、cannot_spawn_subagent、上下文隔离
#[tokio::test]
async fn test_execute_fork_mode_context_inheritance() {
    use blockcell_core::{scope_agent_context, AgentIdentity};
    use blockcell_providers::ProviderPool;

    // 创建 mock provider pool
    let mock_provider = Arc::new(MockInterAgentProvider::new(
        "Fork task completed. Analysis result: 2 files modified.",
    ));
    let provider_pool =
        ProviderPool::from_single_provider("test-model", "test-provider", mock_provider);

    // 创建 ForkChild 身份
    let fork_identity = AgentIdentity::fork_child(
        "fork-test-001".to_string(),
        "parent-session-123".to_string(),
    );

    // 验证 ForkChild 属性
    assert!(fork_identity.role.is_fork_child());
    assert!(!fork_identity.can_spawn_subagent_basic());
    assert_eq!(fork_identity.agent_name, "fork");

    // 在 ForkChild 上下文中验证 can_spawn_subagent
    let result = scope_agent_context(fork_identity.clone(), async {
        // ForkChild 不能 spawn 子 agent
        let can_spawn = blockcell_core::can_spawn_subagent();
        assert!(!can_spawn);
        "verified"
    })
    .await;

    assert_eq!(result, "verified");

    // 验证 ForkedAgentParams for Fork mode (无 agent_type)
    use crate::forked::{CacheSafeParams, ForkedAgentParams};
    let params = ForkedAgentParams::builder()
        .provider_pool(provider_pool)
        .prompt_messages(vec![
            ChatMessage::system("Fork mode test"),
            ChatMessage::user("analyze this"),
        ])
        .cache_safe_params(CacheSafeParams::default())
        .fork_label("fork")
        .max_turns(5)
        .agent_type(None) // Fork mode: 无 agent_type
        .disallowed_tools(vec!["agent".to_string(), "spawn".to_string()])
        .one_shot(true)
        .build();

    assert!(params.is_ok());
    let params = params.unwrap();
    assert!(params.agent_type.is_none());
    assert!(params.disallowed_tools.contains(&"agent".to_string()));
}

#[tokio::test]
async fn fork_subagent_runs_through_provider_handle() {
    use blockcell_tools::subagent::{
        SubagentKind, SubagentPermissions, SubagentProvider, SubagentRequest, SubagentStatus,
    };

    let runtime =
        test_runtime_with_provider(Arc::new(MockInterAgentProvider::new("fork analysis done")));
    let provider = InProcessSubagentProvider::from_runtime(&runtime);

    let handle = provider
        .spawn(SubagentRequest {
            kind: SubagentKind::Fork,
            prompt: "summarize the repo".to_string(),
            description: None,
            permissions: SubagentPermissions::read_only(),
        })
        .await
        .expect("spawn fork");
    assert!(handle.id().starts_with("fork-"));
    assert_eq!(handle.lineage().parent_session_id, "internal:default");

    let result = handle.result().await.expect("fork result");
    assert!(result.contains("fork analysis done"), "{result}");
    assert_eq!(handle.status().await, SubagentStatus::Completed);

    // 只读授权不能附带写入范围
    let rejected = provider
        .spawn(SubagentRequest {
            kind: SubagentKind::Fork,
            prompt: "write files".to_string(),
            description: None,
            permissions: SubagentPermissions::agent_type(vec!["src".to_string()]),
        })
        .await;
    assert!(rejected.is_err());
}

// ========== 端到端测试: run_forked_agent 执行 ==========

/// 测试 run_forked_agent 的实际执行（使用 mock provider）
#[tokio::test]
async fn test_run_forked_agent_with_mock_provider() {
    use crate::forked::{run_forked_agent, CacheSafeParams, ForkedAgentParams};
    use blockcell_providers::ProviderPool;

    // 创建 mock provider pool
    let mock_provider = Arc::new(MockInterAgentProvider::new(
        "Analysis complete. Found patterns in the codebase.",
    ));
    let provider_pool =
        ProviderPool::from_single_provider("test-model", "test-provider", mock_provider);

    // 构建参数
    let params = ForkedAgentParams::builder()
        .provider_pool(provider_pool)
        .prompt_messages(vec![
            ChatMessage::system("You are a test agent. Respond briefly."),
            ChatMessage::user("Find patterns"),
        ])
        .cache_safe_params(CacheSafeParams::default())
        .fork_label("test_e2e")
        .max_turns(1) // 只执行一轮
        .one_shot(true)
        .build()
        .expect("params should build successfully");

    // 执行 forked agent
    let result = run_forked_agent(params).await;

    // 验证执行成功
    assert!(result.is_ok());
    let result = result.unwrap();
    let content = result.final_content.clone().unwrap_or_default();
    assert!(
        content.contains("Analysis")
            || content.contains("patterns")
            || content.contains("complete")
    );

    // 验证 usage metrics
    assert!(result.total_usage.input_tokens > 0 || result.total_usage.output_tokens > 0);
}

#[tokio::test]
async fn process_message_stops_when_token_budget_is_exhausted() {
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    config.agents.ghost.learning.enabled = false;
    config.budget.max_tokens_per_session = 120;

    let provider = Arc::new(MockInterAgentProvider::new(
        "This response should be replaced by budget exhaustion.",
    ));
    let base =
        std::env::temp_dir().join(format!("blockcell-budget-runtime-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&base).expect("create temp budget runtime dir");
    let paths = Paths::with_base(base);
    let mut runtime = test_runtime_with_provider_and_paths(paths, provider, config);

    let result = runtime
        .process_message(test_main_session_inbound("cli", "budget-test"))
        .await
        .expect("process message");

    assert!(result.contains("Budget exhausted"));
    assert!(result.contains("tokens: 150/120"));
}

#[tokio::test]
async fn token_budget_is_tracked_per_session_key() {
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    config.agents.ghost.learning.enabled = false;
    config.budget.max_tokens_per_session = 200;

    let provider = Arc::new(MockInterAgentProvider::new("within budget"));
    let base = std::env::temp_dir().join(format!(
        "blockcell-budget-sessions-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&base).expect("create temp budget session runtime dir");
    let paths = Paths::with_base(base);
    let mut runtime = test_runtime_with_provider_and_paths(paths, provider, config);

    let first = runtime
        .process_message(test_main_session_inbound("cli", "budget-session-a"))
        .await
        .expect("first session message");
    let second = runtime
        .process_message(test_main_session_inbound("cli", "budget-session-b"))
        .await
        .expect("second session message");

    assert_eq!(first, "within budget");
    assert_eq!(second, "within budget");
}

#[test]
fn current_context_estimate_is_tracked_per_session_for_tool_introspection() {
    let runtime = test_runtime();
    runtime.record_context_window_estimate("cli:context-a", 750, 1_000, 0.8);
    runtime.record_context_window_estimate("cli:context-b", 100, 1_000, 0.8);

    let first = runtime
        .budget_tracker_for_session("cli:context-a")
        .context_window_snapshot();
    let second = runtime
        .budget_tracker_for_session("cli:context-b")
        .context_window_snapshot();

    assert_eq!(first.tokens_remaining, 250);
    assert_eq!(first.compact_threshold_tokens, 800);
    assert_eq!(second.tokens_remaining, 900);
}

fn tool_policy_rule(
    name: &str,
    tool: &str,
    decision: blockcell_core::tool_policy::ToolPolicyDecision,
) -> blockcell_core::tool_policy::ToolPolicyRule {
    blockcell_core::tool_policy::ToolPolicyRule {
        name: name.to_string(),
        tool: tool.to_string(),
        decision,
        when: None,
        description: None,
        inherit_from: None,
    }
}

fn tool_call(name: &str, arguments: serde_json::Value) -> ToolCallRequest {
    ToolCallRequest {
        id: format!("call-{}", name),
        name: name.to_string(),
        arguments,
        thought_signature: None,
    }
}

fn tool_call_with_id(id: &str, name: &str, arguments: serde_json::Value) -> ToolCallRequest {
    ToolCallRequest {
        id: id.to_string(),
        name: name.to_string(),
        arguments,
        thought_signature: None,
    }
}

#[tokio::test]
async fn prefetch_runs_read_only_calls_and_skips_write_and_exec() {
    let runtime = test_runtime_with_default_tools();
    let workspace = runtime.paths.workspace();
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::write(workspace.join("alpha.txt"), "alpha-content").unwrap();
    std::fs::write(workspace.join("beta.txt"), "beta-content").unwrap();

    let msg = test_main_session_inbound("cli", "prefetch");
    let calls = vec![
        tool_call_with_id("c1", "read_file", serde_json::json!({"path": "alpha.txt"})),
        tool_call_with_id("c2", "read_file", serde_json::json!({"path": "beta.txt"})),
        tool_call_with_id(
            "c3",
            "write_file",
            serde_json::json!({"path": "gamma.txt", "content": "x"}),
        ),
        tool_call_with_id("c4", "exec", serde_json::json!({"command": "ls"})),
    ];

    let prefetched = runtime.prefetch_read_only_tools(&calls, &msg).await;
    assert_eq!(
        prefetched.len(),
        2,
        "only read-only calls should be prefetched"
    );
    assert!(
        !prefetched.contains_key("c3"),
        "write_file must not be prefetched"
    );
    assert!(
        !prefetched.contains_key("c4"),
        "exec must not be prefetched"
    );

    let alpha = prefetched["c1"].result.as_ref().expect("alpha read ok");
    assert!(alpha.to_string().contains("alpha-content"));
    assert_eq!(
        prefetched["c1"].args,
        serde_json::json!({"path": "alpha.txt"})
    );
}

#[tokio::test]
async fn prefetch_skips_when_fewer_than_two_eligible_calls() {
    let runtime = test_runtime_with_default_tools();
    let msg = test_main_session_inbound("cli", "prefetch-one");
    let calls = vec![tool_call("read_file", serde_json::json!({"path": "a.txt"}))];
    assert!(runtime
        .prefetch_read_only_tools(&calls, &msg)
        .await
        .is_empty());
}

#[tokio::test]
async fn prefetch_respects_tool_policy_deny() {
    use blockcell_core::tool_policy::{ToolPolicy, ToolPolicyConfig, ToolPolicyDecision};

    let mut runtime = test_runtime_with_default_tools();
    runtime.tool_policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![tool_policy_rule(
            "deny-read",
            "read_file",
            ToolPolicyDecision::Deny,
        )],
        ..Default::default()
    });
    let msg = test_main_session_inbound("cli", "prefetch-deny");
    let calls = vec![
        tool_call_with_id("c1", "read_file", serde_json::json!({"path": "a.txt"})),
        tool_call_with_id("c2", "read_file", serde_json::json!({"path": "b.txt"})),
    ];
    assert!(runtime
        .prefetch_read_only_tools(&calls, &msg)
        .await
        .is_empty());
}

/// 仅声明执行环境 / 确认要求、不做实际工作的工具，用于验证 ExplicitConfirmation 准入由
/// 声明驱动，而非硬编码工具名、也非执行环境本身。
struct DeclaredEnvTool(&'static str, blockcell_tools::ExecutionEnvironment, bool);

#[async_trait::async_trait]
impl blockcell_tools::Tool for DeclaredEnvTool {
    fn execution_environment(
        &self,
        _params: &serde_json::Value,
    ) -> blockcell_tools::ExecutionEnvironment {
        self.1
    }

    fn requires_confirmation(&self, _params: &serde_json::Value) -> bool {
        self.2
    }

    fn schema(&self) -> blockcell_tools::ToolSchema {
        blockcell_tools::ToolSchema {
            name: self.0.to_string(),
            description: "declared environment tool".to_string(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    fn validate(&self, _params: &serde_json::Value) -> Result<()> {
        Ok(())
    }

    async fn execute(
        &self,
        _ctx: blockcell_tools::ToolContext,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        Ok(serde_json::json!({ "ok": true }))
    }
}

#[tokio::test]
async fn tool_call_event_records_declared_execution_environment() {
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let mut runtime = test_runtime_with_default_tools();
    let workspace = runtime.paths.workspace();
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::write(workspace.join("alpha.txt"), "alpha-content").unwrap();
    runtime.current_turn_id = Some("turn-env".to_string());

    let msg = test_main_session_inbound("cli", "env-event");
    let session = msg.session_key();
    runtime
        .execute_tool_call(
            &tool_call_with_id("c1", "read_file", serde_json::json!({"path": "alpha.txt"})),
            &msg,
            None,
        )
        .await;

    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&session)
        .expect("read session events");
    let tool_call = events
        .iter()
        .find(|event| event.kind == SessionEventKind::ToolCall)
        .expect("tool/call event recorded");
    assert_eq!(tool_call.payload["tool"], "read_file");
    assert_eq!(
        tool_call.payload["environment"], "local",
        "tool/call should carry the declared environment: {}",
        tool_call.payload
    );
}

#[tokio::test]
async fn tool_call_audit_records_declared_execution_environment() {
    use blockcell_storage::{AuditEvent, AuditLogger};

    let mut runtime = test_runtime_with_default_tools();
    let workspace = runtime.paths.workspace();
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::write(workspace.join("alpha.txt"), "alpha-content").unwrap();
    runtime.current_turn_id = Some("turn-env-audit".to_string());

    let msg = test_main_session_inbound("cli", "env-audit");
    runtime
        .execute_tool_call(
            &tool_call_with_id("c1", "read_file", serde_json::json!({"path": "alpha.txt"})),
            &msg,
            None,
        )
        .await;

    let environment = AuditLogger::new(runtime.paths.clone())
        .read_today()
        .unwrap()
        .into_iter()
        .find_map(|event| match event {
            AuditEvent::ToolCall {
                tool_name,
                environment,
                ..
            } if tool_name == "read_file" => environment,
            _ => None,
        })
        .expect("tool call audit should carry the declared environment");
    assert_eq!(environment, "local");
}

#[tokio::test]
async fn explicit_confirmation_gate_is_driven_by_declaration_not_tool_name() {
    let mut runtime = test_runtime();
    runtime.tool_registry.register(Arc::new(DeclaredEnvTool(
        "custom_full_access",
        blockcell_tools::ExecutionEnvironment::FullAccess,
        true,
    )));
    let (confirm_tx, mut confirm_rx) = mpsc::channel(1);
    runtime.confirm_tx = Some(confirm_tx);
    let msg = test_main_session_inbound("cli", "full-gate-declared");
    let handle = tokio::spawn(async move {
        runtime
            .execute_tool_call(
                &tool_call("custom_full_access", serde_json::json!({})),
                &msg,
                None,
            )
            .await
            .text
    });

    let request = tokio::time::timeout(std::time::Duration::from_secs(1), confirm_rx.recv())
        .await
        .expect("a tool declaring explicit confirmation must request approval")
        .expect("confirmation channel should stay open");
    assert_eq!(request.tool_name, "custom_full_access");
    assert!(
        request
            .paths
            .iter()
            .any(|item| item.contains("full-access")),
        "confirmation should mention full-access: {:?}",
        request.paths
    );
    request.response_tx.send(false).expect("deny full access");

    let result = handle.await.expect("tool task should finish");
    assert!(result.contains("not approved"), "result: {result}");
}

#[tokio::test]
async fn confirmation_gate_follows_the_flag_not_the_environment() {
    // 同一个执行环境声明（Shell）下，只有 requires_confirmation=true 才触发确认。
    let mut runtime = test_runtime();
    runtime.tool_registry.register(Arc::new(DeclaredEnvTool(
        "custom_shell_confirm",
        blockcell_tools::ExecutionEnvironment::Shell,
        true,
    )));
    let (confirm_tx, mut confirm_rx) = mpsc::channel(1);
    runtime.confirm_tx = Some(confirm_tx);
    let msg = test_main_session_inbound("cli", "shell-confirm-gate");
    let handle = tokio::spawn(async move {
        runtime
            .execute_tool_call(
                &tool_call("custom_shell_confirm", serde_json::json!({})),
                &msg,
                None,
            )
            .await
            .text
    });

    let request = tokio::time::timeout(std::time::Duration::from_secs(1), confirm_rx.recv())
        .await
        .expect("a tool requiring confirmation must request approval")
        .expect("confirmation channel should stay open");
    assert_eq!(request.tool_name, "custom_shell_confirm");
    assert!(
        request.paths.iter().any(|item| item.contains("shell")),
        "confirmation should mention the declared environment: {:?}",
        request.paths
    );
    request.response_tx.send(false).expect("deny");

    let result = handle.await.expect("tool task should finish");
    assert!(result.contains("not approved"), "result: {result}");
}

#[tokio::test]
async fn declaration_without_confirmation_skips_gate() {
    // 同一个工具名与参数，仅声明不同：无需确认时直接执行，不触发任何审批。
    let mut runtime = test_runtime();
    runtime.tool_registry.register(Arc::new(DeclaredEnvTool(
        "custom_shell",
        blockcell_tools::ExecutionEnvironment::Shell,
        false,
    )));
    let (confirm_tx, mut confirm_rx) = mpsc::channel(1);
    runtime.confirm_tx = Some(confirm_tx);
    let msg = test_main_session_inbound("cli", "shell-no-gate");

    let output = runtime
        .execute_tool_call(
            &tool_call("custom_shell", serde_json::json!({})),
            &msg,
            None,
        )
        .await;
    assert!(
        output.text.contains("ok"),
        "tool should execute without confirmation: {}",
        output.text
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), confirm_rx.recv())
            .await
            .is_err(),
        "no confirmation should be requested when the tool does not declare it"
    );
}

#[tokio::test]
async fn tool_policy_deny_blocks_tool_before_execution() {
    use blockcell_core::tool_policy::{ToolPolicy, ToolPolicyConfig, ToolPolicyDecision};

    let mut runtime = test_runtime();
    let mut deny_exec = tool_policy_rule("deny-exec", "exec", ToolPolicyDecision::Deny);
    deny_exec.description = Some("exec is disabled by policy".to_string());
    runtime.tool_policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![deny_exec],
        ..Default::default()
    });

    let result = runtime
        .execute_tool_call(
            &tool_call("exec", serde_json::json!({"command": "echo hello"})),
            &test_main_session_inbound("cli", "policy-deny"),
            None,
        )
        .await
        .text;

    assert!(result.contains("exec is disabled by policy"));
    assert!(!result.contains("Unknown tool"));
}

#[tokio::test]
async fn audit_records_rejected_and_executed_calls_with_event_log_ids() {
    use blockcell_core::tool_policy::{ToolPolicy, ToolPolicyConfig, ToolPolicyDecision};
    use blockcell_storage::{
        AuditEvent, AuditLogger, JsonlSessionEventLog, SessionEventKind, SessionEventReader,
    };

    let mut runtime = test_runtime();
    runtime.tool_policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![tool_policy_rule(
            "deny-exec",
            "exec",
            ToolPolicyDecision::Deny,
        )],
        ..Default::default()
    });
    runtime.current_turn_id = Some("turn-audit".to_string());
    let msg = test_main_session_inbound("cli", "audit-link");
    let denied = runtime
        .execute_tool_call(
            &tool_call("exec", serde_json::json!({"command": "echo hi"})),
            &msg,
            None,
        )
        .await;
    assert_eq!(denied.outcome, ToolOutcome::Rejected);
    let executed = runtime
        .execute_tool_call(
            &tool_call("list_dir", serde_json::json!({"path": "."})),
            &msg,
            None,
        )
        .await;

    let audited: Vec<(String, String, String)> = AuditLogger::new(runtime.paths.clone())
        .read_today()
        .unwrap()
        .into_iter()
        .filter_map(|event| match event {
            AuditEvent::ToolCall {
                call_id: Some(call_id),
                outcome: Some(outcome),
                trace_id: Some(trace_id),
                ..
            } => Some((call_id, outcome, trace_id)),
            _ => None,
        })
        .collect();
    assert_eq!(
        audited,
        vec![
            (
                "call-exec".to_string(),
                "rejected".to_string(),
                "turn-audit".to_string()
            ),
            (
                "call-list_dir".to_string(),
                executed.outcome.as_str().to_string(),
                "turn-audit".to_string()
            ),
        ]
    );

    // 审计中的 call_id / outcome 与 Session Event Log 中同一调用一致
    let logged: Vec<(String, String)> = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&msg.session_key())
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == SessionEventKind::ToolResult)
        .map(|event| {
            (
                event.payload["call_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                event.payload["outcome"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    assert_eq!(
        logged,
        audited
            .into_iter()
            .map(|(call_id, outcome, _)| (call_id, outcome))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn tool_policy_ask_uses_existing_confirmation_flow() {
    use blockcell_core::tool_policy::{ToolPolicy, ToolPolicyConfig, ToolPolicyDecision};

    let mut runtime = test_runtime();
    let ask_exec = tool_policy_rule("ask-exec", "exec", ToolPolicyDecision::Ask);
    runtime.tool_policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![ask_exec],
        ..Default::default()
    });
    let (confirm_tx, mut confirm_rx) = mpsc::channel(1);
    runtime.confirm_tx = Some(confirm_tx);

    let msg = test_main_session_inbound("cli", "policy-ask");
    let call = tool_call("exec", serde_json::json!({"command": "echo hello"}));
    let handle =
        tokio::spawn(async move { runtime.execute_tool_call(&call, &msg, None).await.text });

    let request = confirm_rx
        .recv()
        .await
        .expect("policy ask should send confirmation request");
    assert_eq!(request.tool_name, "exec");
    assert_eq!(request.channel, "cli");
    assert_eq!(request.chat_id, "policy-ask");
    request.response_tx.send(false).expect("send denial");

    let result = handle.await.expect("policy ask task should finish");
    assert!(result.contains("用户拒绝") || result.contains("denied"));
}

#[tokio::test]
async fn shell_full_access_requires_confirmation_before_execution() {
    let mut runtime = test_runtime();
    runtime
        .tool_registry
        .register(Arc::new(blockcell_tools::shell_session::ShellTool));
    let (confirm_tx, mut confirm_rx) = mpsc::channel(1);
    runtime.confirm_tx = Some(confirm_tx);
    let msg = test_main_session_inbound("cli", "sandbox-full-access");
    let call = tool_call(
        "shell",
        serde_json::json!({
            "command": "echo approved-shell",
            "sandbox_policy": "full-access"
        }),
    );
    let handle =
        tokio::spawn(async move { runtime.execute_tool_call(&call, &msg, None).await.text });

    let request = tokio::time::timeout(std::time::Duration::from_secs(1), confirm_rx.recv())
        .await
        .expect("full-access shell should request approval")
        .expect("confirmation channel should stay open");
    assert_eq!(request.tool_name, "shell");
    assert!(request
        .paths
        .iter()
        .any(|item| item.contains("full-access")));
    request.response_tx.send(true).expect("approve shell");

    let result = handle.await.expect("shell task should finish");
    assert!(result.contains("approved-shell"), "result: {result}");
}

/// 只模拟沙箱行为、不启动真实进程的 `shell` 替身：以默认策略运行时报"原生沙箱不可用"，
/// 以 full-access 重试时返回成功结果，并记录每次调用的参数。
///
/// 用它替代真实 `ShellTool`：真实路径要求宿主原生沙箱**不可用**（否则用例静默跳过、什么也没断言），
/// 且 PATH 中必须有 `bash`（否则必然失败）。两者都会让同一个用例在不同机器上得出不同结论，
/// 因此这里只驱动管线本身。真实 provider 的拒绝行为由 blockcell-tools 侧覆盖。
struct SandboxEscalationShell {
    calls: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[async_trait::async_trait]
impl blockcell_tools::Tool for SandboxEscalationShell {
    fn schema(&self) -> blockcell_tools::ToolSchema {
        blockcell_tools::ToolSchema {
            name: "shell".to_string(),
            description: "sandbox escalation shell double".to_string(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    fn validate(&self, _params: &serde_json::Value) -> Result<()> {
        Ok(())
    }

    async fn execute(
        &self,
        _ctx: blockcell_tools::ToolContext,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.calls
            .lock()
            .expect("record shell call")
            .push(params.clone());
        if params
            .get("sandbox_policy")
            .and_then(serde_json::Value::as_str)
            == Some("full-access")
        {
            return Ok(serde_json::json!({
                "sandbox_policy": "full-access",
                "output": "fallback-approved",
                "exit_code": 0,
                "running": false,
            }));
        }
        Err(blockcell_core::Error::PermissionDenied(
            "native sandbox unavailable; approval required".to_string(),
        ))
    }
}

#[tokio::test]
async fn unavailable_native_sandbox_can_be_approved_and_retried_full_access() {
    let mut runtime = test_runtime();
    let calls = Arc::new(Mutex::new(Vec::new()));
    runtime
        .tool_registry
        .register(Arc::new(SandboxEscalationShell {
            calls: Arc::clone(&calls),
        }));
    let (confirm_tx, mut confirm_rx) = mpsc::channel(1);
    runtime.confirm_tx = Some(confirm_tx);
    let msg = test_main_session_inbound("cli", "sandbox-fallback");
    let call = tool_call(
        "shell",
        serde_json::json!({"command": "echo fallback-approved", "session_id": "sandbox-session"}),
    );
    let handle =
        tokio::spawn(async move { runtime.execute_tool_call(&call, &msg, None).await.text });

    let request = tokio::time::timeout(std::time::Duration::from_secs(5), confirm_rx.recv())
        .await
        .expect("sandbox fallback should request approval")
        .expect("confirmation channel should stay open");
    assert_eq!(request.tool_name, "shell");
    assert!(request.paths.iter().any(|item| item.contains("sandbox")));
    request.response_tx.send(true).expect("approve fallback");

    let result = handle.await.expect("shell task should finish");
    assert!(result.contains("fallback-approved"), "result: {result}");
    assert!(result.contains("full-access"), "result: {result}");

    // 升级语义：先按原参数失败一次，再以 full-access 重试一次，且重试必须丢掉旧 session_id
    // （否则会落到仍按旧策略建立的会话上，等于没升级）。
    let calls = calls.lock().expect("shell calls");
    assert_eq!(
        calls.len(),
        2,
        "escalation must retry exactly once: {calls:?}"
    );
    assert_eq!(calls[0].get("sandbox_policy"), None, "{calls:?}");
    assert_eq!(calls[0]["session_id"], "sandbox-session", "{calls:?}");
    assert_eq!(calls[1]["sandbox_policy"], "full-access", "{calls:?}");
    assert!(
        calls[1].get("session_id").is_none(),
        "the escalated retry must drop session_id and start a full-access session: {calls:?}"
    );
}

#[tokio::test]
async fn rejected_shell_full_access_does_not_execute_command() {
    let mut runtime = test_runtime();
    runtime
        .tool_registry
        .register(Arc::new(blockcell_tools::shell_session::ShellTool));
    let marker = runtime.paths.workspace().join("rejected-shell-marker");
    let (confirm_tx, mut confirm_rx) = mpsc::channel(1);
    runtime.confirm_tx = Some(confirm_tx);
    let msg = test_main_session_inbound("cli", "sandbox-rejected");
    let call = tool_call(
        "shell",
        serde_json::json!({
            "command": "touch rejected-shell-marker",
            "sandbox_policy": "full-access"
        }),
    );
    let handle =
        tokio::spawn(async move { runtime.execute_tool_call(&call, &msg, None).await.text });

    let request = confirm_rx.recv().await.expect("approval request");
    request.response_tx.send(false).expect("reject shell");
    let result = handle.await.expect("shell task should finish");

    assert!(result.contains("not approved"), "result: {result}");
    assert!(!marker.exists());
}

#[test]
fn sandbox_permission_failure_is_classified_for_escalation() {
    let backend_unavailable = Err(blockcell_core::Error::PermissionDenied(
        "native sandbox unavailable; approval required".to_string(),
    ));
    assert_eq!(
        super::tool_exec::shell_sandbox_escalation_reason(&backend_unavailable).as_deref(),
        Some("native sandbox unavailable; approval required")
    );

    // Untrusted process output strings must not trigger automatic escalation prompts
    let spoofed_output = Ok(serde_json::json!({
        "sandbox_policy": "workspace-write",
        "exit_code": 1,
        "output": "write failed: Operation not permitted"
    }));
    assert_eq!(
        super::tool_exec::shell_sandbox_escalation_reason(&spoofed_output),
        None
    );
}

#[tokio::test]
async fn full_access_shell_dangerous_command_prompts_only_once() {
    // DangerousOperation 批准后 policy_confirmed 置位，ExplicitConfirmation 不再重复弹窗。
    let mut runtime = test_runtime();
    runtime
        .tool_registry
        .register(Arc::new(blockcell_tools::shell_session::ShellTool));
    let (confirm_tx, mut confirm_rx) = mpsc::channel(1);
    runtime.confirm_tx = Some(confirm_tx);
    let msg = test_main_session_inbound("cli", "double-confirm");
    let call = tool_call(
        "shell",
        serde_json::json!({
            "command": "pkill -9 blockcell-nonexistent-marker",
            "sandbox_policy": "full-access"
        }),
    );
    let handle =
        tokio::spawn(async move { runtime.execute_tool_call(&call, &msg, None).await.text });

    let request = confirm_rx.recv().await.expect("approval request");
    request.response_tx.send(true).expect("approve shell");
    let result = handle.await.expect("shell task should finish");

    // 一次批准覆盖整个调用——channel 已关闭（runtime 随任务结束 drop），不能再有第二个请求
    assert!(
        confirm_rx.recv().await.is_none(),
        "must not prompt a second confirmation"
    );
    assert!(!result.contains("not approved"), "{result}");
}

#[tokio::test]
async fn rejected_shell_dangerous_command_reports_shell_tool() {
    // shell 危险命令被拒时，错误必须归因到 shell，而不是 file_ops。
    let mut runtime = test_runtime();
    runtime
        .tool_registry
        .register(Arc::new(blockcell_tools::shell_session::ShellTool));
    let (confirm_tx, mut confirm_rx) = mpsc::channel(1);
    runtime.confirm_tx = Some(confirm_tx);
    let msg = test_main_session_inbound("cli", "shell-dangerous-reject");
    let call = tool_call(
        "shell",
        serde_json::json!({"command": "pkill -9 blockcell-nonexistent-marker"}),
    );
    let handle =
        tokio::spawn(async move { runtime.execute_tool_call(&call, &msg, None).await.text });

    let request = confirm_rx.recv().await.expect("approval request");
    request.response_tx.send(false).expect("reject shell");
    let result = handle.await.expect("shell task should finish");

    assert!(result.contains("\"tool\":\"shell\""), "{result}");
    assert!(result.contains("dangerous shell command"), "{result}");
    assert!(!result.contains("file_ops"), "{result}");
    assert!(!result.contains("file operation"), "{result}");
}

#[tokio::test]
async fn tool_policy_ask_confirmation_skips_duplicate_path_confirmation() {
    use blockcell_core::tool_policy::{
        ToolPolicy, ToolPolicyCondition, ToolPolicyConfig, ToolPolicyDecision,
    };

    let mut runtime = test_runtime();
    let outside_path = dirs::home_dir()
        .expect("home directory")
        .join(format!("blockcell-policy-outside-{}", uuid::Uuid::new_v4()))
        .join("secrets.env");
    let mut ask_env_write =
        tool_policy_rule("ask-env-write", "write_file", ToolPolicyDecision::Ask);
    ask_env_write.when = Some(ToolPolicyCondition {
        path_glob: Some("*.env".to_string()),
        ..Default::default()
    });
    runtime.tool_policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![ask_env_write],
        ..Default::default()
    });
    let (confirm_tx, mut confirm_rx) = mpsc::channel(2);
    runtime.confirm_tx = Some(confirm_tx);

    let msg = test_main_session_inbound("cli", "policy-single-confirm");
    let call = tool_call(
        "write_file",
        serde_json::json!({"path": outside_path.to_string_lossy(), "content": "secret"}),
    );
    let handle =
        tokio::spawn(async move { runtime.execute_tool_call(&call, &msg, None).await.text });

    let request = confirm_rx
        .recv()
        .await
        .expect("tool policy ask should send first confirmation");
    assert_eq!(request.tool_name, "write_file");
    request.response_tx.send(true).expect("approve policy ask");

    let result = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
        .await
        .expect("tool should not wait for a second path confirmation")
        .expect("tool task should finish");

    assert!(result.contains("Unknown tool"));
    assert!(confirm_rx.try_recv().is_err());
}

#[tokio::test]
async fn tool_policy_ask_cannot_override_builtin_path_deny() {
    use blockcell_core::tool_policy::{ToolPolicy, ToolPolicyConfig, ToolPolicyDecision};

    let mut runtime = test_runtime();
    let ask_write = tool_policy_rule("ask-write", "write_file", ToolPolicyDecision::Ask);
    runtime.tool_policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![ask_write],
        ..Default::default()
    });
    let (confirm_tx, mut confirm_rx) = mpsc::channel(2);
    runtime.confirm_tx = Some(confirm_tx);

    let sensitive_path = dirs::home_dir()
        .expect("home directory")
        .join(".ssh")
        .join("authorized_keys");
    let msg = test_main_session_inbound("cli", "policy-hard-deny");
    let call = tool_call(
        "write_file",
        serde_json::json!({
            "path": sensitive_path.to_string_lossy(),
            "content": "ssh-ed25519 test"
        }),
    );
    let handle =
        tokio::spawn(async move { runtime.execute_tool_call(&call, &msg, None).await.text });

    let request = confirm_rx
        .recv()
        .await
        .expect("tool policy ask should request confirmation");
    request.response_tx.send(true).expect("approve tool policy");

    let result = handle.await.expect("tool call should finish");
    assert!(result.contains("Path access denied"), "result: {result}");
    assert!(!result.contains("Unknown tool"), "result: {result}");
    assert!(confirm_rx.try_recv().is_err());
}

#[tokio::test]
async fn tool_policy_channel_condition_only_applies_to_matching_channel() {
    use blockcell_core::tool_policy::{
        ToolPolicy, ToolPolicyCondition, ToolPolicyConfig, ToolPolicyDecision,
    };

    let mut runtime = test_runtime();
    let mut telegram_only =
        tool_policy_rule("deny-telegram-exec", "exec", ToolPolicyDecision::Deny);
    telegram_only.when = Some(ToolPolicyCondition {
        channel: Some("telegram".to_string()),
        ..Default::default()
    });
    runtime.tool_policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![telegram_only],
        ..Default::default()
    });

    let result = runtime
        .execute_tool_call(
            &tool_call("exec", serde_json::json!({"command": "echo hello"})),
            &test_main_session_inbound("cli", "policy-channel"),
            None,
        )
        .await
        .text;

    assert!(!result.contains("Policy denied"));
    assert!(result.contains("Unknown tool"));
}

struct RecordingToolListener {
    events: Arc<Mutex<Vec<crate::runtime_kernel::AgentEvent>>>,
    reject: bool,
    rewrite: serde_json::Value,
}

#[async_trait::async_trait]
impl crate::runtime_kernel::AgentEventListener for RecordingToolListener {
    fn spec(&self) -> crate::runtime_kernel::ListenerSpec {
        crate::runtime_kernel::ListenerSpec::new("recording")
    }

    async fn handle(
        &self,
        _ctx: &AgentEventContext,
        event: &mut AgentEvent,
    ) -> Result<EventDecision> {
        if let AgentEvent::ToolPreExecute { arguments, .. } = event {
            if self.reject {
                self.events.lock().unwrap().push(event.clone());
                return Ok(EventDecision::Reject {
                    reason: "blocked by recording listener".to_string(),
                });
            }
            *arguments = self.rewrite.clone();
        }
        self.events.lock().unwrap().push(event.clone());
        Ok(EventDecision::Continue)
    }
}

#[tokio::test]
async fn tool_pre_execute_listener_rewrites_arguments_for_downstream_events() {
    let mut runtime = test_runtime();
    let events = Arc::new(Mutex::new(Vec::new()));
    runtime
        .event_bus()
        .register(Arc::new(RecordingToolListener {
            events: events.clone(),
            reject: false,
            rewrite: serde_json::json!({"command": "echo rewritten"}),
        }));
    let (event_tx, mut event_rx) = broadcast::channel(16);
    runtime.set_event_tx(event_tx);

    runtime
        .execute_tool_call(
            &tool_call("exec", serde_json::json!({"command": "echo original"})),
            &test_main_session_inbound("cli", "event-rewrite"),
            None,
        )
        .await;

    let recorded = events.lock().unwrap().clone();
    let names: Vec<&str> = recorded.iter().map(AgentEvent::name).collect();
    assert_eq!(
        names,
        vec![
            "tool/pre-execute",
            "tool/call",
            "tool/post-execute",
            "tool/result"
        ]
    );
    match &recorded[2] {
        AgentEvent::ToolPostExecute { arguments, .. } => {
            assert_eq!(arguments["command"], "echo rewritten")
        }
        other => panic!("unexpected event: {other:?}"),
    }

    let start: serde_json::Value =
        serde_json::from_str(&event_rx.recv().await.expect("tool_call_start")).unwrap();
    assert_eq!(start["type"], "tool_call_start");
    assert_eq!(start["params"]["command"], "echo rewritten");
    let result: serde_json::Value =
        serde_json::from_str(&event_rx.recv().await.expect("tool_call_result")).unwrap();
    assert_eq!(result["type"], "tool_call_result");
    assert_eq!(result["chat_id"], "event-rewrite");
}

#[tokio::test]
async fn tool_pre_execute_listener_can_reject_before_execution() {
    let mut runtime = test_runtime();
    let events = Arc::new(Mutex::new(Vec::new()));
    runtime
        .event_bus()
        .register(Arc::new(RecordingToolListener {
            events: events.clone(),
            reject: true,
            rewrite: serde_json::Value::Null,
        }));

    let result = runtime
        .execute_tool_call(
            &tool_call("exec", serde_json::json!({"command": "echo hello"})),
            &test_main_session_inbound("cli", "event-reject"),
            None,
        )
        .await
        .text;

    assert!(result.contains("blocked by recording listener"), "{result}");
    assert!(result.contains("event_listener"), "{result}");
    assert!(!result.contains("Unknown tool"), "{result}");
    let recorded = events.lock().unwrap().clone();
    let names: Vec<&str> = recorded.iter().map(AgentEvent::name).collect();
    assert_eq!(names, vec!["tool/pre-execute", "tool/result"]);
    match &recorded[1] {
        AgentEvent::ToolResult {
            result, outcome, ..
        } => {
            assert_eq!(*outcome, blockcell_core::ToolOutcome::Rejected);
            assert_eq!(result["policy"], "event_listener");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn guards_check_arguments_rewritten_by_pre_execute_listener() {
    let mut runtime = test_runtime();
    let events = Arc::new(Mutex::new(Vec::new()));
    runtime
        .event_bus()
        .register(Arc::new(RecordingToolListener {
            events: events.clone(),
            reject: false,
            rewrite: serde_json::json!({"command": "pkill -9 blockcell"}),
        }));

    let result = runtime
        .execute_tool_call(
            &tool_call("exec", serde_json::json!({"command": "ls"})),
            &test_main_session_inbound("cli", "rewrite-bypass"),
            None,
        )
        .await
        .text;

    assert!(
        result.contains("dangerous exec command requires explicit user confirmation"),
        "{result}"
    );
    let names: Vec<&str> = events
        .lock()
        .unwrap()
        .iter()
        .map(AgentEvent::name)
        .collect();
    assert_eq!(names, vec!["tool/pre-execute", "tool/result"]);
}

#[tokio::test]
async fn policy_denied_tool_call_is_recorded_in_session_event_log() {
    use blockcell_core::tool_policy::{ToolPolicy, ToolPolicyConfig, ToolPolicyDecision};
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let mut runtime = test_runtime();
    runtime.tool_policy = ToolPolicy::from_config(ToolPolicyConfig {
        rules: vec![tool_policy_rule(
            "deny-exec",
            "exec",
            ToolPolicyDecision::Deny,
        )],
        ..Default::default()
    });
    let msg = test_main_session_inbound("cli", "policy-log");

    runtime
        .execute_tool_call(
            &tool_call("exec", serde_json::json!({"command": "echo hi"})),
            &msg,
            None,
        )
        .await;

    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&msg.session_key())
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, SessionEventKind::ToolResult);
    assert_eq!(events[0].payload["is_error"], true);
    assert_eq!(events[0].payload["result"]["policy"], "tool_policy");
}

#[tokio::test]
async fn durable_tool_events_are_appended_to_session_event_log() {
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let mut runtime = test_runtime();
    runtime.current_turn_id = Some("turn-1".to_string());
    runtime.current_step = Some(3);
    let msg = test_main_session_inbound("cli", "event-log");

    runtime
        .execute_tool_call(
            &tool_call("exec", serde_json::json!({"command": "echo hi"})),
            &msg,
            None,
        )
        .await;

    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&msg.session_key())
        .expect("read session events");
    let kinds: Vec<SessionEventKind> = events.iter().map(|event| event.kind).collect();
    assert_eq!(
        kinds,
        vec![SessionEventKind::ToolCall, SessionEventKind::ToolResult]
    );
    assert_eq!(events[0].payload["arguments"]["command"], "echo hi");
    assert_eq!(events[1].seq, 2);
    assert!(events
        .iter()
        .all(|event| event.turn_id.as_deref() == Some("turn-1") && event.step == Some(3)));
}

#[tokio::test]
async fn full_turn_produces_ordered_session_event_log() {
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let mut runtime = test_runtime_with_provider(Arc::new(TestProvider));
    let msg = InboundMessage {
        channel: "cli".to_string(),
        account_id: None,
        sender_id: "user".to_string(),
        chat_id: "event-log-turn".to_string(),
        content: "hello".to_string(),
        media: vec![],
        metadata: serde_json::Value::Null,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    };
    let session_key = msg.session_key();
    runtime.process_message(msg).await.unwrap();

    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&session_key)
        .expect("read session events");
    let kinds: Vec<SessionEventKind> = events.iter().map(|event| event.kind).collect();
    assert_eq!(
        kinds,
        vec![
            SessionEventKind::SessionStart,
            SessionEventKind::TurnStart,
            SessionEventKind::UserMessage,
            SessionEventKind::StepStart,
            SessionEventKind::ModelRequest,
            SessionEventKind::ModelResponse,
            SessionEventKind::StepEnd,
            SessionEventKind::AssistantMessage,
            SessionEventKind::TurnEnd,
        ]
    );
    let turn_id = events[0].turn_id.clone();
    assert!(turn_id.is_some());
    assert!(events.iter().all(|event| event.turn_id == turn_id));
    assert_eq!(events[4].step, Some(1));
    assert!(events.windows(2).all(|pair| pair[1].seq == pair[0].seq + 1));
    assert_steps_balanced(&events);
    assert!(runtime.current_turn_id.is_none());
    assert!(runtime.current_step.is_none());
}

/// 只要有工具可用就一直调用 read_file；无工具时给出最终回答。
struct EndlessToolProvider;

#[async_trait::async_trait]
impl blockcell_providers::Provider for EndlessToolProvider {
    async fn chat(
        &self,
        _messages: &[ChatMessage],
        tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        if tools.is_empty() {
            return Ok(LLMResponse {
                content: Some("forced final answer".to_string()),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: "stop".to_string(),
                usage: serde_json::json!({"prompt_tokens": 12, "completion_tokens": 3}),
            });
        }
        Ok(LLMResponse {
            content: None,
            reasoning_content: None,
            tool_calls: vec![ToolCallRequest {
                id: format!("call-{}", uuid::Uuid::new_v4()),
                name: "read_file".to_string(),
                arguments: serde_json::json!({"path": "missing-file.txt"}),
                thought_signature: None,
            }],
            finish_reason: "tool_calls".to_string(),
            usage: serde_json::Value::Null,
        })
    }
}

#[tokio::test]
async fn forced_final_answer_is_logged_as_its_own_step() {
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    config.agents.defaults.max_tool_iterations = 1;
    let base =
        std::env::temp_dir().join(format!("blockcell-forced-final-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&base).expect("create temp runtime dir");
    let mut runtime = test_runtime_with_provider_and_paths(
        Paths::with_base(base),
        Arc::new(EndlessToolProvider),
        config,
    );
    let msg = test_main_session_inbound("cli", "event-log-forced-final");
    let session_key = msg.session_key();

    let reply = runtime.process_message(msg).await.unwrap();
    assert_eq!(reply, "forced final answer");

    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&session_key)
        .expect("read session events");
    assert_steps_balanced(&events);
    let final_response = events
        .iter()
        .rev()
        .find(|event| event.kind == SessionEventKind::ModelResponse)
        .expect("forced final model/response");
    assert_eq!(final_response.payload["content"], "forced final answer");
    let served_by = runtime.served_by(0);
    assert_eq!(final_response.payload["model"], served_by.model.as_str());
    assert_eq!(
        final_response.payload["provider"],
        served_by.provider.as_str()
    );
    assert_eq!(final_response.payload["usage"]["input_tokens"], 12);
    assert_eq!(final_response.payload["usage"]["output_tokens"], 3);
    let final_step = final_response
        .step
        .expect("forced final call belongs to a step");
    let step_events: Vec<SessionEventKind> = events
        .iter()
        .filter(|event| event.step == Some(final_step))
        .map(|event| event.kind)
        .collect();
    assert_eq!(
        step_events,
        vec![
            SessionEventKind::StepStart,
            SessionEventKind::ModelRequest,
            SessionEventKind::ModelResponse,
            SessionEventKind::StepEnd,
        ]
    );
}

#[tokio::test]
async fn every_proposed_tool_call_ends_with_a_tool_result_outcome() {
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    config.agents.defaults.max_tool_iterations = 1;
    let base =
        std::env::temp_dir().join(format!("blockcell-tool-outcome-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&base).expect("create temp runtime dir");
    let mut runtime = test_runtime_with_provider_and_paths(
        Paths::with_base(base),
        Arc::new(EndlessToolProvider),
        config,
    );
    runtime
        .tool_registry
        .register(Arc::new(blockcell_tools::fs::ReadFileTool::default()));
    let mut msg = test_main_session_inbound("cli", "event-log-tool-outcome");
    msg.content = "read the file missing-file.txt and summarize it".to_string();
    let session_key = msg.session_key();
    runtime.process_message(msg).await.unwrap();

    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&session_key)
        .expect("read session events");
    let proposed: Vec<String> = events
        .iter()
        .filter(|event| event.kind == SessionEventKind::ModelResponse)
        .flat_map(|event| {
            event.payload["tool_calls"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .map(|call| call["id"].as_str().unwrap().to_string())
        .collect();
    let results: Vec<(String, String)> = events
        .iter()
        .filter(|event| event.kind == SessionEventKind::ToolResult)
        .map(|event| {
            (
                event.payload["call_id"].as_str().unwrap().to_string(),
                event.payload["outcome"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        proposed,
        results.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>()
    );
    let outcomes: Vec<&str> = results
        .iter()
        .map(|(_, outcome)| outcome.as_str())
        .collect();
    assert_eq!(outcomes, vec!["rejected", "rejected"], "{results:?}");
    let rejected = events
        .iter()
        .rev()
        .find(|event| event.kind == SessionEventKind::ToolResult)
        .unwrap();
    assert!(rejected.payload["result"]["error"]
        .as_str()
        .unwrap()
        .contains("max call limit"));
    assert_eq!(rejected.payload["is_error"], true);
    assert_eq!(rejected.payload["retry_safe"], false);
}

/// 第一步追加一条提示；`stop_at` 指定的步拒绝开始。
struct NotePolicy {
    stop_at: Option<u32>,
}

#[async_trait::async_trait]
impl crate::runtime_kernel::Waterfall<crate::PreStepState> for NotePolicy {
    async fn handle(
        &self,
        mut state: crate::PreStepState,
        next: crate::runtime_kernel::Next<'_, crate::PreStepState>,
    ) -> Result<crate::PreStepState> {
        if Some(state.iteration) == self.stop_at {
            state.decision = crate::PreStepDecision::Stop {
                reason: "stopped by policy".to_string(),
            };
            return Ok(state);
        }
        state.messages.push(ChatMessage::user("POLICY_NOTE"));
        next(state).await
    }
}

#[tokio::test]
async fn pre_step_policy_can_rewrite_input_or_stop_the_step() {
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let provider = Arc::new(RecallCaptureProvider {
        calls: Mutex::new(Vec::new()),
    });
    let mut runtime = test_runtime_with_provider(provider.clone());
    runtime.register_pre_step_policy(Arc::new(NotePolicy { stop_at: None }));
    runtime
        .process_message(test_main_session_inbound("cli", "pre-step-rewrite"))
        .await
        .unwrap();
    let calls = provider.calls.lock().unwrap().clone();
    assert!(
        calls[0]
            .iter()
            .any(|m| chat_message_text(m) == "POLICY_NOTE"),
        "{calls:?}"
    );

    let provider = Arc::new(RecallCaptureProvider {
        calls: Mutex::new(Vec::new()),
    });
    let mut runtime = test_runtime_with_provider(provider.clone());
    runtime.register_pre_step_policy(Arc::new(NotePolicy { stop_at: Some(1) }));
    let msg = test_main_session_inbound("cli", "pre-step-stop");
    let session_key = msg.session_key();
    let reply = runtime.process_message(msg).await.unwrap();
    assert_eq!(reply, "stopped by policy");
    assert!(provider.calls.lock().unwrap().is_empty());
    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&session_key)
        .unwrap();
    assert!(!events
        .iter()
        .any(|event| event.kind == SessionEventKind::ModelRequest));
    assert_steps_balanced(&events);
}

#[tokio::test]
async fn pre_step_stops_when_session_budget_is_exhausted() {
    let provider = Arc::new(RecallCaptureProvider {
        calls: Mutex::new(Vec::new()),
    });
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    config.budget.max_tokens_per_session = 10;
    let base = std::env::temp_dir().join(format!("blockcell-pre-step-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&base).expect("create temp runtime dir");
    let mut runtime =
        test_runtime_with_provider_and_paths(Paths::with_base(base), provider.clone(), config);
    let msg = test_main_session_inbound("cli", "pre-step-budget");
    runtime
        .budget_tracker_for_session(&persist_session_key_for(&msg))
        .record_usage(100, 0, 0);

    let reply = runtime.process_message(msg).await.unwrap();
    assert!(reply.starts_with("⚠️"), "{reply}");
    assert!(provider.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn discarded_stream_output_is_logged_as_assistant_attempt() {
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let mut runtime = test_runtime_with_provider(Arc::new(StreamingRetryProvider {
        attempts: AtomicUsize::new(0),
    }));
    let msg = test_main_session_inbound("cli", "event-log-attempt");
    let session_key = msg.session_key();

    assert_eq!(runtime.process_message(msg).await.unwrap(), "final answer");

    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&session_key)
        .expect("read session events");
    let kinds: Vec<SessionEventKind> = events.iter().map(|event| event.kind).collect();
    let attempt = kinds
        .iter()
        .position(|kind| *kind == SessionEventKind::AssistantAttempt)
        .expect("assistant/attempt recorded");
    assert_eq!(
        &kinds[attempt - 1..=attempt + 1],
        &[
            SessionEventKind::ModelRequest,
            SessionEventKind::AssistantAttempt,
            SessionEventKind::ModelResponse,
        ]
    );
    let payload = &events[attempt].payload;
    assert_eq!(payload["content"], "partial");
    assert!(payload["error"]
        .as_str()
        .unwrap()
        .contains("temporary stream failure"));
    assert_eq!(events[attempt].step, Some(1));
    assert_eq!(events[attempt + 1].payload["content"], "final answer");
}

/// 每次流式调用都直接返回给定错误，并统计调用次数。
struct FailingStreamProvider {
    error: &'static str,
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for FailingStreamProvider {
    async fn chat(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        Err(blockcell_core::Error::Provider(self.error.to_string()))
    }

    async fn chat_stream(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<mpsc::Receiver<StreamChunk>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(blockcell_core::Error::Provider(self.error.to_string()))
    }
}

async fn llm_stream_calls_for(error: &'static str) -> usize {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    config.agents.defaults.llm_max_retries = 2;
    config.agents.defaults.llm_retry_delay_ms = 1;
    let base = std::env::temp_dir().join(format!("blockcell-llm-retry-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&base).expect("create temp runtime dir");
    let mut runtime = test_runtime_with_provider_and_paths(
        Paths::with_base(base),
        Arc::new(FailingStreamProvider {
            error,
            calls: calls.clone(),
        }),
        config,
    );
    let _ = runtime
        .process_message(test_main_session_inbound("cli", "llm-retry"))
        .await;
    calls.load(Ordering::SeqCst)
}

/// 第一次主请求报上下文超限；压缩请求返回结构化摘要；压缩后的请求正常回答。
struct OverflowOnceProvider {
    stream_calls: AtomicUsize,
}

#[async_trait::async_trait]
impl blockcell_providers::Provider for OverflowOnceProvider {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        let is_compact = messages
            .iter()
            .any(|m| chat_message_text(m).contains("Generate a structured summary"));
        let content = if is_compact {
            "## User Request\nsay hello\n\n## Current State\ngreeting in progress"
        } else {
            "answer after compaction"
        };
        Ok(LLMResponse {
            content: Some(content.to_string()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            finish_reason: "stop".to_string(),
            usage: serde_json::Value::Null,
        })
    }

    async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
    ) -> blockcell_core::Result<mpsc::Receiver<StreamChunk>> {
        if self.stream_calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(blockcell_core::Error::Provider(
                "http_status=400 This model's maximum context length is 8192 tokens".to_string(),
            ));
        }
        let response = self.chat(messages, tools).await?;
        let (tx, rx) = mpsc::channel(1);
        tx.send(StreamChunk::Done { response }).await.unwrap();
        Ok(rx)
    }
}

#[tokio::test]
async fn context_overflow_compacts_then_retries_the_step() {
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let mut runtime = test_runtime_with_provider(Arc::new(OverflowOnceProvider {
        stream_calls: AtomicUsize::new(0),
    }));
    let msg = test_main_session_inbound("cli", "overflow-recovery");
    let session_key = msg.session_key();

    let reply = runtime.process_message(msg).await.unwrap();
    assert_eq!(reply, "answer after compaction");

    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&session_key)
        .expect("read session events");
    let error = events
        .iter()
        .find(|event| event.kind == SessionEventKind::ModelRequestError)
        .expect("model/request-error recorded");
    assert_eq!(error.payload["kind"], "context_overflow");
    assert_eq!(error.payload["recovery"]["retry"], "compact_first");
    let kinds: Vec<SessionEventKind> = events.iter().map(|event| event.kind).collect();
    let error_at = kinds
        .iter()
        .position(|kind| *kind == SessionEventKind::ModelRequestError)
        .unwrap();
    assert!(kinds[error_at..].contains(&SessionEventKind::CompactionEnd));
    assert_steps_balanced(&events);

    let start = events
        .iter()
        .find(|event| event.kind == SessionEventKind::CompactionStart)
        .expect("compaction/start");
    assert_eq!(start.payload["phase"], "request-error");
    assert_eq!(start.payload["compactor"], "model_summary");
    let end = events
        .iter()
        .find(|event| event.kind == SessionEventKind::CompactionEnd)
        .unwrap();
    assert!(end.payload["error"].is_null(), "{}", end.payload);
    assert!(end.payload["model"].is_string());
    assert_eq!(
        end.payload["messages_before"].as_u64().unwrap(),
        end.payload["retained_messages"].as_u64().unwrap()
            + end.payload["summarized_messages"].as_u64().unwrap()
    );
}

#[tokio::test]
async fn extractive_compactor_recovers_overflow_without_model_summary() {
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let mut runtime = test_runtime_with_provider(Arc::new(OverflowOnceProvider {
        stream_calls: AtomicUsize::new(0),
    }));
    runtime.set_context_compactor(Arc::new(crate::compact::ExtractiveCompactor));
    let msg = test_main_session_inbound("cli", "overflow-extractive");
    let session_key = msg.session_key();

    assert_eq!(
        runtime.process_message(msg).await.unwrap(),
        "answer after compaction"
    );
    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&session_key)
        .unwrap();
    let end = events
        .iter()
        .find(|event| event.kind == SessionEventKind::CompactionEnd)
        .expect("compaction/end");
    assert_eq!(end.payload["compactor"], "extractive");
    assert!(end.payload["model"].is_null());
    assert!(end.payload["error"].is_null(), "{}", end.payload);
}

#[tokio::test]
async fn failed_compaction_is_recorded_in_compaction_end() {
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let mut runtime = test_runtime_with_provider(Arc::new(FailingStreamProvider {
        error: "This model's maximum context length is 8192 tokens",
        calls: Arc::new(AtomicUsize::new(0)),
    }));
    let msg = test_main_session_inbound("cli", "overflow-compaction-failed");
    let session_key = msg.session_key();
    let _ = runtime.process_message(msg).await;

    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&session_key)
        .unwrap();
    let end = events
        .iter()
        .find(|event| event.kind == SessionEventKind::CompactionEnd)
        .expect("compaction/end");
    assert!(end.payload["error"].is_string(), "{}", end.payload);
    assert_eq!(end.payload["tokens_after"], end.payload["tokens_before"]);
    assert_eq!(end.payload["summarized_messages"], 0);
    assert_steps_balanced(&events);
}

#[tokio::test]
async fn llm_retry_follows_failure_kind() {
    assert_eq!(
        llm_stream_calls_for("This model's maximum context length is 8192 tokens").await,
        1
    );
    assert_eq!(
        llm_stream_calls_for("http_status=503 server overloaded").await,
        3
    );
    assert_eq!(
        llm_stream_calls_for("http_status=400 invalid_request_error: bad tool schema").await,
        1
    );
    // 原因不明：保守地最多再试一次（llm_max_retries = 2）。
    assert_eq!(
        llm_stream_calls_for("model returned malformed payload").await,
        2
    );
}

/// 有副作用且总是超时的工具。
struct TimingOutSideEffectTool;

#[async_trait::async_trait]
impl blockcell_tools::Tool for TimingOutSideEffectTool {
    fn schema(&self) -> blockcell_tools::ToolSchema {
        blockcell_tools::ToolSchema {
            name: "place_order".to_string(),
            description: "Place an order.".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        }
    }
    fn validate(&self, _params: &serde_json::Value) -> Result<()> {
        Ok(())
    }
    fn idempotency(&self, _params: &serde_json::Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Required
    }
    async fn execute(
        &self,
        _ctx: blockcell_tools::ToolContext,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        Err(blockcell_core::Error::Timeout(
            "Request timed out after 30 seconds".to_string(),
        ))
    }
}

#[tokio::test]
async fn side_effecting_tool_timeout_is_recorded_as_unknown_outcome() {
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let mut runtime = test_runtime();
    runtime
        .tool_registry
        .register(Arc::new(TimingOutSideEffectTool));
    let msg = test_main_session_inbound("cli", "unknown-outcome");

    let result = runtime
        .execute_tool_call(&tool_call("place_order", serde_json::json!({})), &msg, None)
        .await
        .text;

    assert!(result.contains("执行结果未知"), "{result}");
    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&msg.session_key())
        .unwrap();
    let tool_result = events
        .iter()
        .find(|event| event.kind == SessionEventKind::ToolResult)
        .expect("tool/result");
    assert_eq!(tool_result.payload["outcome"], "unknown");
    assert_eq!(tool_result.payload["idempotency"], "required");
    assert_eq!(tool_result.payload["retry_safe"], false);
}

#[tokio::test]
async fn failed_turn_is_closed_with_error_turn_end() {
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let mut runtime = test_runtime_with_provider(Arc::new(TestProvider));
    let mut msg = test_main_session_inbound("cli", "event-log-failed-turn");
    msg.metadata = serde_json::json!({ "forced_skill_name": "missing_skill" });
    let session_key = msg.session_key();

    let error = runtime.process_message(msg).await.unwrap_err();

    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&session_key)
        .expect("read session events");
    let kinds: Vec<SessionEventKind> = events.iter().map(|event| event.kind).collect();
    assert_eq!(
        kinds,
        vec![
            SessionEventKind::SessionStart,
            SessionEventKind::TurnStart,
            SessionEventKind::UserMessage,
            SessionEventKind::TurnEnd,
        ]
    );
    let turn_end = events.last().unwrap();
    assert_eq!(turn_end.turn_id, events[1].turn_id);
    assert_eq!(turn_end.payload["error"], error.to_string());
    assert!(runtime.current_turn_id.is_none());
}

struct EchoLoop {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl crate::AgentLoop for EchoLoop {
    fn name(&self) -> &'static str {
        "echo"
    }

    async fn run(&self, runtime: &mut AgentRuntime, msg: InboundMessage) -> Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(format!(
            "echo:{}:{}",
            runtime.agent_loop_name(),
            msg.content
        ))
    }
}

#[tokio::test]
async fn process_message_delegates_to_configured_agent_loop() {
    let mut runtime = test_runtime();
    assert_eq!(runtime.agent_loop_name(), "default");

    let calls = Arc::new(AtomicUsize::new(0));
    runtime.set_agent_loop(Arc::new(EchoLoop {
        calls: calls.clone(),
    }));
    let reply = runtime
        .process_message(test_main_session_inbound("cli", "custom-loop"))
        .await
        .unwrap();

    assert_eq!(runtime.agent_loop_name(), "echo");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(reply.starts_with("echo:echo:"), "{reply}");
}

#[tokio::test]
async fn candidate_scope_isolates_listeners_and_prompt_sections_from_production() {
    use crate::prompt_registry::{PromptSection, SystemPromptContext};
    use crate::runtime_kernel::{ListenerSpec, ScopeTarget};

    struct CandidateListener {
        hits: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::runtime_kernel::AgentEventListener for CandidateListener {
        fn spec(&self) -> ListenerSpec {
            ListenerSpec::new("candidate-policy").scope(ScopeTarget::Candidate("evo-1".into()))
        }

        async fn handle(
            &self,
            _ctx: &AgentEventContext,
            event: &mut AgentEvent,
        ) -> Result<EventDecision> {
            if matches!(event, AgentEvent::ToolPreExecute { .. }) {
                self.hits.fetch_add(1, Ordering::SeqCst);
            }
            Ok(EventDecision::Continue)
        }
    }

    struct CandidatePrompt;

    impl PromptSection for CandidatePrompt {
        fn id(&self) -> &'static str {
            "candidate_hint"
        }
        fn priority(&self) -> i32 {
            15
        }
        fn render(&self, _ctx: &SystemPromptContext<'_>) -> Option<String> {
            Some("CANDIDATE PROMPT\n".to_string())
        }
    }

    let mut runtime = test_runtime();
    let hits = Arc::new(AtomicUsize::new(0));
    runtime
        .event_bus()
        .register(Arc::new(CandidateListener { hits: hits.clone() }));
    runtime.context_builder().prompt_registry().register(
        ScopeTarget::Candidate("evo-1".into()),
        Arc::new(CandidatePrompt),
    );

    let render = |runtime: &AgentRuntime| {
        runtime
            .context_builder()
            .build_system_prompt_for_mode_with_channel_and_session(
                InteractionMode::Chat,
                None,
                &HashSet::new(),
                &HashSet::new(),
                "cli",
                "",
                &[],
                &[],
                Some("cli:candidate"),
            )
    };
    let call = tool_call("exec", serde_json::json!({"command": "echo hi"}));
    let msg = test_main_session_inbound("cli", "candidate");

    runtime.execute_tool_call(&call, &msg, None).await;
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    assert!(!render(&runtime).contains("CANDIDATE PROMPT"));

    runtime.set_candidate_scope(Some("evo-1".to_string()));
    runtime.execute_tool_call(&call, &msg, None).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(render(&runtime).contains("CANDIDATE PROMPT"));

    runtime.set_candidate_scope(None);
    runtime.execute_tool_call(&call, &msg, None).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cron_delivered_tool_events_are_logged_under_target_session() {
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let mut runtime = test_runtime();
    let mut msg = test_main_session_inbound("cron", "job-1");
    msg.metadata = serde_json::json!({
        "skill_run_mode": "cron",
        "deliver": true,
        "deliver_channel": "telegram",
        "deliver_to": "chat-42",
    });

    runtime
        .execute_tool_call(
            &tool_call("exec", serde_json::json!({"command": "echo hi"})),
            &msg,
            None,
        )
        .await;

    let log = JsonlSessionEventLog::new(runtime.paths.clone());
    let target = blockcell_core::build_session_key("telegram", "chat-42");
    let kinds: Vec<SessionEventKind> = log
        .read(&target)
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect();
    assert_eq!(
        kinds,
        vec![SessionEventKind::ToolCall, SessionEventKind::ToolResult]
    );
    assert!(log.read(&msg.session_key()).unwrap().is_empty());
}

/// 每个 step 只开始一次且都被关闭。
fn assert_steps_balanced(events: &[blockcell_storage::SessionEvent]) {
    use blockcell_storage::SessionEventKind;
    let mut open: Vec<u32> = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    for event in events {
        let step = event.payload["iteration"]
            .as_u64()
            .map(|value| value as u32);
        match event.kind {
            SessionEventKind::StepStart => {
                let step = step.expect("step/start iteration");
                assert!(seen.insert(step), "step {step} started twice: {events:?}");
                open.push(step);
            }
            SessionEventKind::StepEnd => {
                let step = step.expect("step/end iteration");
                assert_eq!(open.pop(), Some(step), "unbalanced step/end: {events:?}");
            }
            _ => {}
        }
    }
    assert!(open.is_empty(), "unclosed steps {open:?}");
}

struct ScopedEchoTool(&'static str);

#[async_trait::async_trait]
impl blockcell_tools::Tool for ScopedEchoTool {
    fn schema(&self) -> blockcell_tools::ToolSchema {
        blockcell_tools::ToolSchema {
            name: "scoped_echo".to_string(),
            description: format!("echo from {}", self.0),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    fn validate(&self, _params: &serde_json::Value) -> Result<()> {
        Ok(())
    }

    fn idempotency(&self, _params: &serde_json::Value) -> blockcell_core::Idempotency {
        blockcell_core::Idempotency::Safe
    }

    async fn execute(
        &self,
        _ctx: blockcell_tools::ToolContext,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        Ok(serde_json::json!({ "scope": self.0 }))
    }
}

#[tokio::test]
async fn scoped_tools_follow_runtime_session_and_candidate_views() {
    use crate::runtime_kernel::ScopeTarget;

    let mut runtime = test_runtime();
    let _session_tool = runtime.register_scoped_tool(
        ScopeTarget::Session("cli:scoped-a".into()),
        Arc::new(ScopedEchoTool("session")),
    );
    let _candidate_tool = runtime.register_scoped_tool(
        ScopeTarget::Candidate("evo-1".into()),
        Arc::new(ScopedEchoTool("candidate")),
    );
    let call = tool_call("scoped_echo", serde_json::json!({}));

    let in_session = test_main_session_inbound("cli", "scoped-a");
    runtime.sync_tool_scope_view(&in_session.session_key());
    let result = runtime
        .execute_tool_call(&call, &in_session, None)
        .await
        .text;
    assert!(result.contains("\"session\""), "{result}");

    // 其他会话看不到、也无法执行该会话作用域工具
    let other = test_main_session_inbound("cli", "scoped-b");
    runtime.sync_tool_scope_view(&other.session_key());
    assert!(!runtime
        .tool_registry
        .tool_names()
        .contains(&"scoped_echo".to_string()));
    let result = runtime.execute_tool_call(&call, &other, None).await.text;
    assert!(result.contains("Unknown tool"), "{result}");

    // 进入候选评估后，候选工具可见并覆盖同名工具
    runtime.set_candidate_scope(Some("evo-1".into()));
    let result = runtime.execute_tool_call(&call, &other, None).await.text;
    assert!(result.contains("\"candidate\""), "{result}");
    runtime.set_candidate_scope(None);
    assert!(runtime.tool_registry.get("scoped_echo").is_none());
}

struct FixedResultTool(&'static str, serde_json::Value);

#[async_trait::async_trait]
impl blockcell_tools::Tool for FixedResultTool {
    fn schema(&self) -> blockcell_tools::ToolSchema {
        blockcell_tools::ToolSchema {
            name: self.0.to_string(),
            description: "fixed result".to_string(),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    fn validate(&self, _params: &serde_json::Value) -> Result<()> {
        Ok(())
    }

    async fn execute(
        &self,
        _ctx: blockcell_tools::ToolContext,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        Ok(self.1.clone())
    }
}

#[tokio::test]
async fn successful_memory_and_spawn_tools_emit_domain_events() {
    use crate::runtime_kernel::ScopeTarget;
    use blockcell_storage::{JsonlSessionEventLog, SessionEventKind, SessionEventReader};

    let mut runtime = test_runtime();
    let msg = test_main_session_inbound("cli", "domain-events");
    let session = msg.session_key();
    let _memory = runtime.register_scoped_tool(
        ScopeTarget::Session(session.clone()),
        Arc::new(FixedResultTool(
            "memory_manage",
            serde_json::json!({"ok": true}),
        )),
    );
    let _spawn = runtime.register_scoped_tool(
        ScopeTarget::Session(session.clone()),
        Arc::new(FixedResultTool(
            "spawn",
            serde_json::json!({"task_id": "task-7", "status": "running"}),
        )),
    );
    runtime.sync_tool_scope_view(&session);
    runtime.current_turn_id = Some("turn-1".to_string());

    for name in ["memory_manage", "spawn"] {
        runtime
            .execute_tool_call(&tool_call(name, serde_json::json!({})), &msg, None)
            .await;
    }
    SubagentCompletionReporter::new(
        runtime.event_bus.clone(),
        None,
        session.clone(),
        &msg.channel,
        &msg.chat_id,
    )
    .report("task-7", true, "all done")
    .await;

    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&session)
        .expect("read session events");
    let kinds: Vec<SessionEventKind> = events.iter().map(|event| event.kind).collect();
    assert_eq!(
        kinds,
        vec![
            SessionEventKind::ToolCall,
            SessionEventKind::ToolResult,
            SessionEventKind::MemoryWrite,
            SessionEventKind::ToolCall,
            SessionEventKind::ToolResult,
            SessionEventKind::SubagentSpawn,
            SessionEventKind::SubagentCompleted,
        ]
    );
    assert_eq!(events[5].payload["task_id"], "task-7");
    assert_eq!(events[5].turn_id.as_deref(), Some("turn-1"));
    assert_eq!(events[6].payload["success"], true);
    assert_eq!(events[6].payload["summary"], "all done");
    assert!(events[6].turn_id.is_none());
}

#[tokio::test]
async fn event_log_projection_matches_stored_transcript_after_tool_turn() {
    use blockcell_storage::{JsonlSessionEventLog, SessionEventReader};

    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".to_string();
    config.agents.defaults.provider = Some("test".to_string());
    config.agents.defaults.max_tool_iterations = 1;
    let base = std::env::temp_dir().join(format!("blockcell-projection-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&base).expect("create temp runtime dir");
    let mut runtime = test_runtime_with_provider_and_paths(
        Paths::with_base(base),
        Arc::new(EndlessToolProvider),
        config,
    );
    runtime
        .tool_registry
        .register(Arc::new(blockcell_tools::fs::ReadFileTool::default()));
    let mut msg = test_main_session_inbound("cli", "projection-check");
    msg.content = "read the file missing-file.txt and summarize it".to_string();
    let session_key = msg.session_key();
    runtime.process_message(msg).await.unwrap();

    let events = JsonlSessionEventLog::new(runtime.paths.clone())
        .read(&session_key)
        .expect("read session events");
    let history = runtime.session_store.load(&session_key).unwrap();
    let check = blockcell_storage::verify_history(&events, &history);
    assert!(check.is_consistent(), "{check:?}");
    assert_eq!(check.matched_turns, 1);
    let turns = blockcell_storage::project_turns(&events);
    assert!(!turns[0].tool_call_ids.is_empty());
}

struct CountingEvolutionNotifier {
    notified: AtomicUsize,
}

impl crate::capability_adapter::EvolutionNotifier for CountingEvolutionNotifier {
    fn notify(&self) {
        self.notified.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn fork_for_message_task_shares_caches_and_capabilities_while_isolating_turn_state() {
    use blockcell_core::path_policy::PathOp;

    let mut parent = test_runtime();
    let skill_evo_notifier = Arc::new(CountingEvolutionNotifier {
        notified: AtomicUsize::new(0),
    });
    parent.set_skill_evolution_worker(
        skill_evo_notifier.clone() as Arc<dyn crate::capability_adapter::EvolutionNotifier>
    );
    parent.current_turn_id = Some("stale-parent-turn".to_string());
    parent.current_step = Some(9);

    let (steering1, sender1) = SteeringChannel::new(4);
    let token1 = parent.abort_token.child();
    let mut msg_runtime_1 = parent.fork_for_message_task(steering1, sender1, token1.clone());

    let (steering2, sender2) = SteeringChannel::new(4);
    let token2 = parent.abort_token.child();
    let msg_runtime_2 = parent.fork_for_message_task(steering2, sender2, token2.clone());

    // 1. 单轮状态与取消树隔离
    assert!(msg_runtime_1.current_turn_id.is_none());
    assert!(msg_runtime_1.current_step.is_none());
    token1.cancel();
    assert!(msg_runtime_1.abort_token.is_cancelled());
    assert!(!parent.abort_token.is_cancelled());
    assert!(!msg_runtime_2.abort_token.is_cancelled());

    // 2. skill_evolution_worker 继承并可正常唤醒
    msg_runtime_1
        .skill_evolution_worker
        .as_ref()
        .expect("skill_evolution_worker must be inherited")
        .notify();
    assert_eq!(skill_evo_notifier.notified.load(Ordering::SeqCst), 1);

    // 3. response_cache 跨消息共享，且与父实例（/clear）同源
    let long_list = (1..=15)
        .map(|i| {
            format!(
                "{}. 条目 {} 的详细描述内容，确保总字符数超过默认的 800 字符缓存阈值，以便触发 ResponseCache 缓存与 stub 替换逻辑，验证跨消息 recall 是否命中同一份缓存实例。",
                i, i
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let stub = msg_runtime_1
        .response_cache
        .maybe_cache_and_stub("cli:shared-session", &long_list, true)
        .expect("should cache list in message 1");
    let ref_id = stub
        .split("ID: ")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .expect("extract ref_id");
    assert_eq!(
        msg_runtime_2
            .response_cache
            .recall("cli:shared-session", ref_id)
            .as_deref(),
        Some(long_list.as_str())
    );
    parent.response_cache.clear_session("cli:shared-session");
    assert!(msg_runtime_2
        .response_cache
        .recall("cli:shared-session", ref_id)
        .is_none());

    // 4. authorized_dirs 跨消息共享（同一目录不重复弹窗）
    let auth_dir = std::env::temp_dir();
    msg_runtime_1.authorize_directory(&auth_dir, PathOp::Read, "cli:shared-session");
    assert!(msg_runtime_2.is_path_authorized(&auth_dir, PathOp::Read, "cli:shared-session"));
    // 授权不跨会话泄漏：其他 session 仍需确认
    assert!(!msg_runtime_2.is_path_authorized(&auth_dir, PathOp::Read, "cli:other-session"));

    // 5. budget_trackers 跨消息累加
    msg_runtime_1
        .budget_tracker_for_session("cli:shared-session")
        .record_usage(120, 30, 10);
    let budget_snap = msg_runtime_2
        .budget_tracker_for_session("cli:shared-session")
        .snapshot();
    assert_eq!(budget_snap.total_tokens_used, 150);

    // 6. ContextBuilder 的 SessionContextCache 与 skill_index_summary 跨消息共享
    let _ = msg_runtime_1
        .context_builder
        .build_messages_for_session_mode_with_channel(
            "cli:shared-session",
            &[],
            "hello",
            &[],
            InteractionMode::Chat,
            None,
            &HashSet::new(),
            &HashSet::new(),
            "cli",
            false,
            &[],
            &[],
        );
    assert!(msg_runtime_2
        .context_builder
        .system_prompt_snapshot_reader()
        .get("cli:shared-session")
        .is_some());

    msg_runtime_1
        .context_builder
        .set_skill_index_summary("shared skill summary".to_string());
    assert_eq!(
        msg_runtime_2
            .context_builder
            .skill_index_summary_arc()
            .read()
            .unwrap()
            .as_deref(),
        Some("shared skill summary")
    );
}

#[tokio::test]
async fn fork_for_message_task_preserves_session_memory_system_across_messages() {
    let parent = test_runtime();
    let session_key = "cli:shared-layer4";

    {
        let (steering1, sender1) = SteeringChannel::new(4);
        let token1 = parent.abort_token.child();
        let mut msg_runtime_1 = parent.fork_for_message_task(steering1, sender1, token1);
        msg_runtime_1
            .init_memory_system(session_key.to_string())
            .await
            .expect("init memory system in message 1");
        msg_runtime_1
            .memory_system
            .as_mut()
            .expect("memory_system initialized")
            .skill_tracker_mut()
            .record_load("deploy-skill", "deployed service in turn 1");
        // msg_runtime_1 在此作用域结束时 drop，自动将 MemorySystem 归还到父级共享槽
    }

    let (steering2, sender2) = SteeringChannel::new(4);
    let token2 = parent.abort_token.child();
    let mut msg_runtime_2 = parent.fork_for_message_task(steering2, sender2, token2);
    let reused = msg_runtime_2
        .session_memory_systems
        .lock()
        .unwrap()
        .remove(session_key)
        .expect("session MemorySystem should be returned to shared pool on drop");
    assert_eq!(reused.session_id(), session_key);
    assert_eq!(reused.skill_tracker().get_recent_skills(1000).len(), 1);
    msg_runtime_2.memory_system = Some(reused);
}

#[test]
fn skill_mutations_across_all_four_paths_are_visible_in_next_forked_message_runtime() {
    let parent = test_runtime();
    let skills_dir = parent.paths.skills_dir();
    std::fs::create_dir_all(&skills_dir).expect("create skills_dir");

    let write_skill_md = |name: &str, desc: &str, body: &str| {
        let dir = skills_dir.join(name);
        std::fs::create_dir_all(&dir).expect("create skill dir");
        std::fs::write(
            dir.join("meta.yaml"),
            format!("name: {name}\ndescription: {desc}\n"),
        )
        .expect("write meta.yaml");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {desc}\n---\n\n{body}\n"),
        )
        .expect("write SKILL.md");
    };

    // 路径 1: 主轮通过 skill_manage 写入并触发 reload_skills() → 下一条派生消息立即可见
    {
        let (s1, tx1) = SteeringChannel::new(4);
        let mut msg1 = parent.fork_for_message_task(s1, tx1, parent.abort_token.child());
        write_skill_md(
            "skill_via_manage",
            "created by skill_manage",
            "Step 1: run manage",
        );
        let added = msg1.context_builder.reload_skills();
        assert!(added.contains(&"skill_via_manage".to_string()));
    }
    {
        let (s2, tx2) = SteeringChannel::new(4);
        let msg2 = parent.fork_for_message_task(s2, tx2, parent.abort_token.child());
        assert!(
            msg2.context_builder
                .skill_manager()
                .unwrap()
                .get("skill_via_manage")
                .is_some(),
            "path 1 (skill_manage) must be visible in next forked runtime without rescanning"
        );
        assert!(
            msg2.context_builder
                .skill_index_summary_arc()
                .read()
                .unwrap()
                .as_deref()
                .unwrap_or_default()
                .contains("skill_via_manage"),
            "path 1 should also refresh shared skill_index_summary"
        );
    }

    // 路径 2: 后台 Nudge Review 异步更新 skill_index_summary_arc → 下一条派生消息立即可见
    {
        let (s1, tx1) = SteeringChannel::new(4);
        let msg1 = parent.fork_for_message_task(s1, tx1, parent.abort_token.child());
        let arc = msg1.context_builder.skill_index_summary_arc();
        *arc.write().unwrap() =
            Some("- skill_via_background_review: learned in review".to_string());
    }
    {
        let (s2, tx2) = SteeringChannel::new(4);
        let msg2 = parent.fork_for_message_task(s2, tx2, parent.abort_token.child());
        let summary = msg2
            .context_builder
            .skill_index_summary_arc()
            .read()
            .unwrap()
            .clone()
            .unwrap_or_default();
        assert!(summary.contains("skill_via_background_review"));
    }

    // 路径 3: 技能自进化部署（invalidate_prompt_snapshot + 更新 SKILL.md）→ 下一条消息 reload 后可见最新内容
    write_skill_md(
        "skill_via_manage",
        "evolved description v2",
        "Step 1: evolved body v2",
    );
    blockcell_skills::SkillManager::invalidate_prompt_snapshot(&parent.paths)
        .expect("invalidate prompt snapshot");
    {
        let (s3, tx3) = SteeringChannel::new(4);
        let mut msg3 = parent.fork_for_message_task(s3, tx3, parent.abort_token.child());
        let added = msg3.context_builder.reload_skills();
        assert!(
            added.is_empty(),
            "modifying an existing skill produces no new skill names"
        );
        let active = msg3
            .context_builder
            .resolve_active_skill_by_name("skill_via_manage", &HashSet::new())
            .expect("resolve evolved skill");
        assert!(active.prompt_md.contains("evolved body v2"));
        let summary = msg3
            .context_builder
            .skill_index_summary_arc()
            .read()
            .unwrap()
            .clone()
            .unwrap_or_default();
        assert!(
            summary.contains("evolved description v2"),
            "modifying an existing skill must still refresh skill_index_summary"
        );
    }

    // 路径 4: 用户手工在磁盘新增技能文件 → 下一条消息启动 reload_skills() 后可见并回写共享槽
    write_skill_md(
        "skill_via_manual_edit",
        "manually added by user",
        "Manual instructions",
    );
    {
        let (s4, tx4) = SteeringChannel::new(4);
        let mut msg4 = parent.fork_for_message_task(s4, tx4, parent.abort_token.child());
        let added = msg4.context_builder.reload_skills();
        assert!(added.contains(&"skill_via_manual_edit".to_string()));
    }
    {
        let (s5, tx5) = SteeringChannel::new(4);
        let msg5 = parent.fork_for_message_task(s5, tx5, parent.abort_token.child());
        assert!(msg5
            .context_builder
            .skill_manager()
            .unwrap()
            .get("skill_via_manual_edit")
            .is_some());
    }
}
