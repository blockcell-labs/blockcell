use blockcell_agent::AgentRuntime;
use blockcell_core::types::{ChatMessage, LLMResponse};
use blockcell_core::{Config, InboundMessage, Paths};
use blockcell_providers::{Provider, ProviderPool};
use blockcell_tools::ToolRegistry;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

struct EmptyContinuation(AtomicUsize);
#[async_trait::async_trait]
impl Provider for EmptyContinuation {
    async fn chat(
        &self,
        messages: &[ChatMessage],
        _: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        let call = self.0.fetch_add(1, Ordering::SeqCst);
        if call > 0 {
            assert!(
                messages
                    .iter()
                    .any(|m| m.content.to_string().contains("required.json")),
                "missing continuation prompt"
            );
        }
        Ok(LLMResponse {
            content: if call == 0 {
                Some(
                    "Original findings: unable to create required.json; report this limitation."
                        .into(),
                )
            } else {
                None
            },
            reasoning_content: None,
            tool_calls: vec![],
            finish_reason: "stop".into(),
            usage: serde_json::Value::Null,
        })
    }
}

#[tokio::test]
async fn forced_skill_empty_continuation_must_deliver_an_explanation() {
    let dir = tempfile::TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());
    paths.ensure_dirs().unwrap();
    let skill = paths.skills_dir().join("review_demo");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("meta.yaml"),
        "name: review_demo\ndescription: review fixture\n",
    )
    .unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "# Review Demo\n\n## Prompt {#prompt}\nExplain your findings.\n",
    )
    .unwrap();
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".into();
    config.agents.defaults.provider = Some("test".into());
    config.intent_router = None;
    config.agents.ghost.learning.enabled = false;
    config.security.path_access.policy_file = paths.path_access_file().display().to_string();
    let provider = Arc::new(EmptyContinuation(AtomicUsize::new(0)));
    let pool = ProviderPool::from_single_provider("test/mock", "test", provider.clone());
    let mut runtime = AgentRuntime::new(config, paths, pool, ToolRegistry::new()).unwrap();
    let reply = runtime.process_message(InboundMessage {
        channel: "cli".into(), account_id: None, sender_id: "user".into(), chat_id: "review".into(),
        content: "Explain findings and produce required.json".into(), media: vec![],
        metadata: serde_json::json!({"forced_skill_name":"review_demo", "task_contract":{"deliverables":[{"path":"required.json"}]}}),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    }).await.unwrap();
    assert_eq!(
        provider.0.load(Ordering::SeqCst),
        2,
        "must exercise continuation"
    );
    assert!(
        !reply.trim().is_empty(),
        "forced skill discarded its original answer and returned an empty final response"
    );
    assert!(
        reply.contains("Original findings:"),
        "valid original explanation was lost: {reply}"
    );
    assert!(
        reply.contains("required.json"),
        "missing deliverable was not explained: {reply}"
    );
}
