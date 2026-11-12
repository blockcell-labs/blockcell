use super::*;

fn cron_message(kind: &str, account: Option<&str>) -> InboundMessage {
    InboundMessage {
        channel: "cron".into(),
        account_id: account.map(str::to_string),
        sender_id: "cron".into(),
        chat_id: format!("{kind}-{}", account.unwrap_or("legacy")),
        content: "scheduled answer".into(),
        media: vec![],
        metadata: serde_json::json!({
            "deliver": true,
            "deliver_channel": "telegram",
            "deliver_to": "same-chat",
            "deliver_account_id": account,
            "reminder": kind == "reminder",
            "cron_agent": kind == "agent",
        }),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
    }
}

#[test]
fn r2_cron_history_uses_delivery_account_and_legacy_key() {
    let mut keys = Vec::new();
    for account in [Some("bot-a"), Some("bot-b"), None] {
        let msg = cron_message("agent", account);
        let mut recipient = msg.clone();
        recipient.channel = "telegram".into();
        recipient.chat_id = "same-chat".into();
        let key = persist_session_key_for(&msg);
        assert_eq!(key, recipient.session_key());
        keys.push(key);
    }
    assert_ne!(keys[0], keys[1]);
    assert_eq!(keys[2], "telegram:same-chat");
}

#[tokio::test]
async fn r2_cron_reminder_and_agent_deliver_to_source_account() {
    let mut runtime = test_runtime();
    let (tx, mut rx) = mpsc::channel(16);
    runtime.set_outbound(tx);
    for kind in ["reminder", "agent"] {
        for account in [Some("bot-a"), Some("bot-b"), None] {
            let msg = cron_message(kind, account);
            let key = persist_session_key_for(&msg);
            runtime.process_message(msg).await.unwrap();
            let mut delivered = Vec::new();
            while let Ok(message) = rx.try_recv() {
                if message.channel == "telegram" {
                    delivered.push(message);
                }
            }
            assert_eq!(delivered.len(), 1, "{kind}/{account:?}");
            assert_eq!(delivered[0].chat_id, "same-chat");
            assert_eq!(delivered[0].account_id.as_deref(), account, "{kind}");
            if kind == "agent" {
                assert!(!runtime.session_store.load(&key).unwrap().is_empty());
            }
        }
    }
}
