use blockcell_agent::AgentRuntime;
use blockcell_core::message_receipt::{
    cancel_message_receipt, register_message_receipt, MESSAGE_RECEIPT_ID,
};
use blockcell_core::types::{ChatMessage, LLMResponse};
use blockcell_core::{Config, InboundMessage, Paths};
use blockcell_providers::{Provider, ProviderPool};
use blockcell_tools::ToolRegistry;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{mpsc, Notify};

struct ControlledProvider {
    calls: AtomicUsize,
    block_first: bool,
    started: Notify,
    release: Notify,
    dropped: Arc<Notify>,
    effects: AtomicUsize,
}

struct DropSignal(Arc<Notify>);
impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

impl ControlledProvider {
    fn new(block_first: bool) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            block_first,
            started: Notify::new(),
            release: Notify::new(),
            dropped: Arc::new(Notify::new()),
            effects: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl Provider for ControlledProvider {
    async fn chat(
        &self,
        _messages: &[ChatMessage],
        _tools: &[serde_json::Value],
    ) -> blockcell_core::Result<LLMResponse> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.block_first && index == 0 {
            let _drop = DropSignal(self.dropped.clone());
            self.started.notify_one();
            self.release.notified().await;
            self.effects.fetch_add(1, Ordering::SeqCst);
        }
        Ok(LLMResponse {
            content: Some("completed".into()),
            reasoning_content: None,
            tool_calls: Vec::new(),
            finish_reason: "stop".into(),
            usage: serde_json::json!({"input_tokens": 1, "output_tokens": 1}),
        })
    }
}

fn runtime(provider: Arc<ControlledProvider>) -> (tempfile::TempDir, AgentRuntime) {
    let dir = tempfile::TempDir::new().unwrap();
    let paths = Paths::with_base(dir.path().to_path_buf());
    paths.ensure_dirs().unwrap();
    let mut config = Config::default();
    config.agents.defaults.model = "test/mock".into();
    config.agents.defaults.provider = Some("test".into());
    config.intent_router = None;
    config.agents.ghost.learning.enabled = false;
    config.security.path_access.policy_file = paths.path_access_file().display().to_string();
    let pool = ProviderPool::from_single_provider("test/mock", "test", provider);
    let mut runtime = AgentRuntime::new(config, paths, pool, ToolRegistry::new()).unwrap();
    runtime.set_agent_id(Some("default".into()));
    (dir, runtime)
}

fn message(job: &str, receipt: Option<&str>) -> InboundMessage {
    let mut metadata = serde_json::json!({"job_id": job, "cron_agent": true});
    if let Some(receipt) = receipt {
        metadata[MESSAGE_RECEIPT_ID] = serde_json::json!(receipt);
    }
    InboundMessage {
        channel: "cron".into(),
        account_id: None,
        sender_id: "cron".into(),
        chat_id: job.into(),
        content: format!("perform {job}"),
        media: Vec::new(),
        metadata,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    }
}

async fn finish(tx: mpsc::Sender<InboundMessage>, run: tokio::task::JoinHandle<()>) {
    drop(tx);
    tokio::time::timeout(std::time::Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn r5_revoked_queued_receipt_never_starts_model_work() {
    let provider = Arc::new(ControlledProvider::new(false));
    let (_dir, mut runtime) = runtime(provider.clone());
    let (expired, _expired_rx) = register_message_receipt();
    assert!(cancel_message_receipt(&expired));
    let (valid, valid_rx) = register_message_receipt();
    let (tx, rx) = mpsc::channel(4);
    tx.send(message("expired", Some(&expired))).await.unwrap();
    tx.send(message("valid", Some(&valid))).await.unwrap();
    let run = tokio::spawn(async move { runtime.run_loop(rx, None).await });
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), valid_rx)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "revoked queued work still reached the provider"
    );
    finish(tx, run).await;
}

#[tokio::test]
async fn r5_receipt_cancellation_stops_a_claimed_runtime_turn_before_retry() {
    let provider = Arc::new(ControlledProvider::new(true));
    let (_dir, mut runtime) = runtime(provider.clone());
    let (old, _old_rx) = register_message_receipt();
    let (tx, rx) = mpsc::channel(4);
    let run = tokio::spawn(async move { runtime.run_loop(rx, None).await });
    tx.send(message("same-job", Some(&old))).await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        provider.started.notified(),
    )
    .await
    .unwrap();
    assert!(cancel_message_receipt(&old));
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        provider.dropped.notified(),
    )
    .await
    .expect("receipt cancellation left the old runtime turn executing");
    assert_eq!(provider.effects.load(Ordering::SeqCst), 0);
    let (retry, retry_rx) = register_message_receipt();
    tx.send(message("same-job", Some(&retry))).await.unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), retry_rx)
            .await
            .unwrap()
            .unwrap(),
        Ok(())
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    finish(tx, run).await;
}

#[tokio::test]
async fn r5_receipt_bearing_cron_work_keeps_its_own_completion() {
    let provider = Arc::new(ControlledProvider::new(true));
    let (_dir, mut runtime) = runtime(provider.clone());
    let (tx, rx) = mpsc::channel(4);
    let run = tokio::spawn(async move { runtime.run_loop(rx, None).await });
    tx.send(message("active-job", None)).await.unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        provider.started.notified(),
    )
    .await
    .unwrap();
    let (receipt, receipt_rx) = register_message_receipt();
    tx.send(message("active-job", Some(&receipt)))
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), receipt_rx)
            .await
            .expect("cron receipt was swallowed by the active turn's steering channel")
            .unwrap(),
        Ok(())
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(provider.effects.load(Ordering::SeqCst), 0);
    finish(tx, run).await;
}
