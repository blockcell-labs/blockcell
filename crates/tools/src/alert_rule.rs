use async_trait::async_trait;
use blockcell_core::{Error, Paths, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[allow(unused_imports)]
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{Tool, ToolContext, ToolSchema};

/// Persistent alert rule store — saved to workspace/alerts/rules.json
#[derive(Debug, Serialize, Deserialize)]
struct AlertStore {
    version: u32,
    rules: Vec<AlertRule>,
}

impl Default for AlertStore {
    fn default() -> Self {
        Self { version: 1, rules: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AlertRule {
    id: String,
    name: String,
    enabled: bool,
    /// Data source: a tool call spec that fetches the metric value.
    /// e.g. {"tool": "finance_api", "params": {"action": "stock_quote", "symbol": "AAPL"}}
    source: Value,
    /// JSON path to extract the metric value from the tool result.
    /// e.g. "price" or "data.close" or "results.0.price"
    metric_path: String,
    /// Condition operator: gt, lt, gte, lte, eq, ne, change_pct, cross_above, cross_below
    operator: String,
    /// Threshold value to compare against.
    threshold: f64,
    /// Optional second threshold for range conditions.
    threshold2: Option<f64>,
    /// Cooldown in seconds — suppress re-triggering within this window.
    cooldown_secs: u64,
    /// Check interval in seconds (how often to evaluate).
    check_interval_secs: u64,
    /// Notification config: how to alert when triggered.
    notify: AlertNotify,
    /// Action callback: tool call(s) to auto-execute when the alert triggers.
    /// Each entry is {"tool": "...", "params": {...}}.
    /// Supports template vars: {value}, {threshold}, {name}, {time}.
    #[serde(default)]
    on_trigger: Vec<AlertAction>,
    /// State tracking.
    state: AlertState,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AlertNotify {
    /// Notification channel: "desktop", "message", "webhook", "email"
    channel: String,
    /// Template for the alert message. Supports {name}, {value}, {threshold}, {operator}.
    template: Option<String>,
    /// Extra params for the notification (e.g. webhook URL, email address).
    params: Option<Value>,
}

/// An action to auto-execute when an alert triggers.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AlertAction {
    /// Tool name to call, e.g. "exchange_api", "notification", "blockchain_tx".
    tool: String,
    /// Parameters for the tool call. Supports template vars: {value}, {threshold}, {name}, {time}.
    params: Value,
    /// Optional label for logging.
    #[serde(default)]
    label: Option<String>,
    /// If true, require user confirmation before executing (default: true for write ops).
    #[serde(default = "default_confirm")]
    require_confirm: bool,
}

fn default_confirm() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AlertState {
    /// Last evaluated value.
    last_value: Option<f64>,
    /// Previous value (for change_pct / cross detection).
    prev_value: Option<f64>,
    /// Last time the rule was checked (Unix ms).
    last_check_at: Option<i64>,
    /// Last time the rule triggered (Unix ms).
    last_triggered_at: Option<i64>,
    /// Total number of times triggered.
    trigger_count: u64,
    /// Last error if evaluation failed.
    last_error: Option<String>,
}

fn load_store(paths: &Paths) -> Result<AlertStore> {
    let path = paths.workspace().join("alerts").join("rules.json");
    if !path.exists() {
        return Ok(AlertStore::default());
    }
    let content = std::fs::read_to_string(&path)?;
    let store: AlertStore = serde_json::from_str(&content)?;
    Ok(store)
}

fn save_store(paths: &Paths, store: &AlertStore) -> Result<()> {
    let dir = paths.workspace().join("alerts");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("rules.json");
    let content = serde_json::to_string_pretty(store)?;
    std::fs::write(&path, content)?;
    Ok(())
}

pub struct AlertRuleTool;

#[async_trait]
impl Tool for AlertRuleTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "alert_rule",
            description: "Create and manage conditional alert rules for monitoring metrics. \
                Rules periodically evaluate a data source (via tool call), extract a metric value, \
                compare against a threshold, and trigger notifications when conditions are met. \
                NEW: on_trigger action callbacks — auto-execute tool calls when alert fires (e.g. place_order, send notification, transfer tokens). \
                Supports operators: gt, lt, gte, lte, eq, ne, change_pct (% change from previous), \
                cross_above (value crosses above threshold), cross_below (value crosses below threshold). \
                Actions: 'create' (new rule), 'list' (all rules), 'get' (single rule), \
                'update' (modify rule), 'delete' (remove rule), 'evaluate' (manually check a rule now), \
                'history' (trigger history).",
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["create", "list", "get", "update", "delete", "evaluate", "history"],
                        "description": "Action to perform"
                    },
                    "rule_id": {
                        "type": "string",
                        "description": "(get/update/delete/evaluate/history) Rule ID"
                    },
                    "name": {
                        "type": "string",
                        "description": "(create/update) Human-readable name, e.g. 'AAPL价格超过200'"
                    },
                    "source": {
                        "type": "object",
                        "description": "(create/update) Data source tool call spec: {\"tool\": \"finance_api\", \"params\": {\"action\": \"stock_quote\", \"symbol\": \"AAPL\"}}"
                    },
                    "metric_path": {
                        "type": "string",
                        "description": "(create/update) JSON path to extract metric from tool result, e.g. 'price' or 'data.0.close'"
                    },
                    "operator": {
                        "type": "string",
                        "enum": ["gt", "lt", "gte", "lte", "eq", "ne", "change_pct", "cross_above", "cross_below"],
                        "description": "(create/update) Comparison operator"
                    },
                    "threshold": {
                        "type": "number",
                        "description": "(create/update) Threshold value. For change_pct, this is the percentage (e.g. 5 for 5%)"
                    },
                    "threshold2": {
                        "type": "number",
                        "description": "(create/update) Optional second threshold for range checks"
                    },
                    "cooldown_secs": {
                        "type": "integer",
                        "description": "(create/update) Cooldown seconds between triggers. Default: 3600 (1 hour)"
                    },
                    "check_interval_secs": {
                        "type": "integer",
                        "description": "(create/update) How often to check in seconds. Default: 300 (5 min)"
                    },
                    "notify_channel": {
                        "type": "string",
                        "enum": ["desktop", "message", "webhook", "email"],
                        "description": "(create/update) How to notify. Default: desktop"
                    },
                    "notify_template": {
                        "type": "string",
                        "description": "(create/update) Alert message template. Vars: {name}, {value}, {threshold}, {operator}, {time}"
                    },
                    "notify_params": {
                        "type": "object",
                        "description": "(create/update) Extra notification params (e.g. {\"url\": \"...\"} for webhook)"
                    },
                    "enabled": {
                        "type": "boolean",
                        "description": "(update) Enable or disable the rule"
                    },
                    "on_trigger": {
                        "type": "array",
                        "description": "(create/update) Action callbacks to auto-execute when alert triggers. Array of {tool, params, label?, require_confirm?}. Example: [{\"tool\": \"notification\", \"params\": {\"message\": \"Price alert!\"}, \"require_confirm\": false}, {\"tool\": \"exchange_api\", \"params\": {\"action\": \"place_order\", ...}}]. Template vars in params: {value}, {threshold}, {name}, {time}",
                        "items": {
                            "type": "object",
                            "properties": {
                                "tool": {"type": "string"},
                                "params": {"type": "object"},
                                "label": {"type": "string"},
                                "require_confirm": {"type": "boolean"}
                            },
                            "required": ["tool", "params"]
                        }
                    }
                },
                "required": ["action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "create" => {
                if params.get("name").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Validation("'name' is required for create".into()));
                }
                if params.get("source").is_none() {
                    return Err(Error::Validation("'source' is required for create".into()));
                }
                if params.get("metric_path").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Validation("'metric_path' is required for create".into()));
                }
                if params.get("operator").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Validation("'operator' is required for create".into()));
                }
                if params.get("threshold").is_none() {
                    return Err(Error::Validation("'threshold' is required for create".into()));
                }
            }
            "get" | "delete" | "evaluate" | "history" => {
                if params.get("rule_id").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Validation("'rule_id' is required".into()));
                }
            }
            "update" => {
                if params.get("rule_id").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
                    return Err(Error::Validation("'rule_id' is required for update".into()));
                }
            }
            "list" => {}
            _ => return Err(Error::Validation(format!("Unknown action: {}", action))),
        }
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let paths = Paths::new();
        let action = params["action"].as_str().unwrap();

        match action {
            "create" => action_create(&paths, &params),
            "list" => action_list(&paths),
            "get" => action_get(&paths, &params),
            "update" => action_update(&paths, &params),
            "delete" => action_delete(&paths, &params),
            "evaluate" => action_evaluate(&paths, &ctx, &params).await,
            "history" => action_history(&paths, &params),
            _ => Err(Error::Tool(format!("Unknown action: {}", action))),
        }
    }
}

fn action_create(paths: &Paths, params: &Value) -> Result<Value> {
    let mut store = load_store(paths)?;
    let now = Utc::now().timestamp_millis();
    let rule_id = format!("alert_{}", Uuid::new_v4().to_string().split('-').next().unwrap_or("x"));

    // Parse on_trigger actions
    let on_trigger: Vec<AlertAction> = params.get("on_trigger")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter().filter_map(|item| {
                let tool = item.get("tool").and_then(|v| v.as_str())?.to_string();
                let action_params = item.get("params").cloned().unwrap_or(json!({}));
                let label = item.get("label").and_then(|v| v.as_str()).map(String::from);
                let require_confirm = item.get("require_confirm").and_then(|v| v.as_bool()).unwrap_or(true);
                Some(AlertAction { tool, params: action_params, label, require_confirm })
            }).collect()
        })
        .unwrap_or_default();

    let rule = AlertRule {
        id: rule_id.clone(),
        name: params["name"].as_str().unwrap().to_string(),
        enabled: true,
        source: params["source"].clone(),
        metric_path: params["metric_path"].as_str().unwrap().to_string(),
        operator: params["operator"].as_str().unwrap().to_string(),
        threshold: params["threshold"].as_f64().unwrap(),
        threshold2: params.get("threshold2").and_then(|v| v.as_f64()),
        cooldown_secs: params.get("cooldown_secs").and_then(|v| v.as_u64()).unwrap_or(3600),
        check_interval_secs: params.get("check_interval_secs").and_then(|v| v.as_u64()).unwrap_or(300),
        notify: AlertNotify {
            channel: params.get("notify_channel").and_then(|v| v.as_str()).unwrap_or("desktop").to_string(),
            template: params.get("notify_template").and_then(|v| v.as_str()).map(|s| s.to_string()),
            params: params.get("notify_params").cloned(),
        },
        on_trigger,
        state: AlertState::default(),
        created_at: now,
        updated_at: now,
    };

    let summary = json!({
        "rule_id": rule.id,
        "name": rule.name,
        "operator": rule.operator,
        "threshold": rule.threshold,
        "check_interval_secs": rule.check_interval_secs,
        "cooldown_secs": rule.cooldown_secs,
        "notify_channel": rule.notify.channel,
        "on_trigger_count": rule.on_trigger.len(),
        "status": "created"
    });

    store.rules.push(rule);
    save_store(paths, &store)?;

    Ok(summary)
}

fn action_list(paths: &Paths) -> Result<Value> {
    let store = load_store(paths)?;
    let rules: Vec<Value> = store.rules.iter().map(|r| {
        json!({
            "rule_id": r.id,
            "name": r.name,
            "enabled": r.enabled,
            "operator": r.operator,
            "threshold": r.threshold,
            "check_interval_secs": r.check_interval_secs,
            "last_value": r.state.last_value,
            "trigger_count": r.state.trigger_count,
            "last_triggered_at": r.state.last_triggered_at,
            "last_error": r.state.last_error,
        })
    }).collect();

    Ok(json!({
        "rules": rules,
        "count": rules.len()
    }))
}

fn action_get(paths: &Paths, params: &Value) -> Result<Value> {
    let store = load_store(paths)?;
    let rule_id = params["rule_id"].as_str().unwrap();
    let rule = store.rules.iter().find(|r| r.id == rule_id)
        .ok_or_else(|| Error::Tool(format!("Rule '{}' not found", rule_id)))?;

    Ok(serde_json::to_value(rule).unwrap_or(json!({"error": "serialize failed"})))
}

fn action_update(paths: &Paths, params: &Value) -> Result<Value> {
    let mut store = load_store(paths)?;
    let rule_id = params["rule_id"].as_str().unwrap();
    let now = Utc::now().timestamp_millis();

    let rule = store.rules.iter_mut().find(|r| r.id == rule_id)
        .ok_or_else(|| Error::Tool(format!("Rule '{}' not found", rule_id)))?;

    if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
        rule.name = name.to_string();
    }
    if let Some(source) = params.get("source") {
        rule.source = source.clone();
    }
    if let Some(mp) = params.get("metric_path").and_then(|v| v.as_str()) {
        rule.metric_path = mp.to_string();
    }
    if let Some(op) = params.get("operator").and_then(|v| v.as_str()) {
        rule.operator = op.to_string();
    }
    if let Some(th) = params.get("threshold").and_then(|v| v.as_f64()) {
        rule.threshold = th;
    }
    if let Some(th2) = params.get("threshold2").and_then(|v| v.as_f64()) {
        rule.threshold2 = Some(th2);
    }
    if let Some(cd) = params.get("cooldown_secs").and_then(|v| v.as_u64()) {
        rule.cooldown_secs = cd;
    }
    if let Some(ci) = params.get("check_interval_secs").and_then(|v| v.as_u64()) {
        rule.check_interval_secs = ci;
    }
    if let Some(en) = params.get("enabled").and_then(|v| v.as_bool()) {
        rule.enabled = en;
    }
    if let Some(nc) = params.get("notify_channel").and_then(|v| v.as_str()) {
        rule.notify.channel = nc.to_string();
    }
    if let Some(nt) = params.get("notify_template").and_then(|v| v.as_str()) {
        rule.notify.template = Some(nt.to_string());
    }
    if let Some(np) = params.get("notify_params") {
        rule.notify.params = Some(np.clone());
    }
    if let Some(ot) = params.get("on_trigger").and_then(|v| v.as_array()) {
        rule.on_trigger = ot.iter().filter_map(|item| {
            let tool = item.get("tool").and_then(|v| v.as_str())?.to_string();
            let action_params = item.get("params").cloned().unwrap_or(json!({}));
            let label = item.get("label").and_then(|v| v.as_str()).map(String::from);
            let require_confirm = item.get("require_confirm").and_then(|v| v.as_bool()).unwrap_or(true);
            Some(AlertAction { tool, params: action_params, label, require_confirm })
        }).collect();
    }
    rule.updated_at = now;

    let rule_id_out = rule.id.clone();
    let rule_name = rule.name.clone();
    save_store(paths, &store)?;

    Ok(json!({
        "rule_id": rule_id_out,
        "name": rule_name,
        "status": "updated"
    }))
}

fn action_delete(paths: &Paths, params: &Value) -> Result<Value> {
    let mut store = load_store(paths)?;
    let rule_id = params["rule_id"].as_str().unwrap();
    let before = store.rules.len();
    store.rules.retain(|r| r.id != rule_id);
    let removed = before - store.rules.len();
    if removed > 0 {
        save_store(paths, &store)?;
    }
    Ok(json!({
        "rule_id": rule_id,
        "removed": removed > 0
    }))
}

/// Evaluate a single rule: fetch data, extract metric, compare, return result.
/// This does NOT send notifications — the agent runtime tick handles that.
async fn action_evaluate(paths: &Paths, ctx: &ToolContext, params: &Value) -> Result<Value> {
    let mut store = load_store(paths)?;
    let rule_id = params["rule_id"].as_str().unwrap();
    let now = Utc::now().timestamp_millis();

    let rule_idx = store.rules.iter().position(|r| r.id == rule_id)
        .ok_or_else(|| Error::Tool(format!("Rule '{}' not found", rule_id)))?;

    // Extract what we need before mutable borrow
    let tool_name = store.rules[rule_idx].source.get("tool").and_then(|v| v.as_str())
        .ok_or_else(|| Error::Tool("source.tool is required".into()))?
        .to_string();
    let tool_params = store.rules[rule_idx].source.get("params").cloned().unwrap_or(json!({}));
    let metric_path = store.rules[rule_idx].metric_path.clone();
    let operator = store.rules[rule_idx].operator.clone();
    let threshold = store.rules[rule_idx].threshold;
    let threshold2 = store.rules[rule_idx].threshold2;
    let prev_value = store.rules[rule_idx].state.prev_value;
    let last_value = store.rules[rule_idx].state.last_value;
    let cooldown_secs = store.rules[rule_idx].cooldown_secs;
    let last_triggered_at = store.rules[rule_idx].state.last_triggered_at;

    // Execute the source tool call
    let tool_registry = crate::ToolRegistry::with_defaults();
    let result = tool_registry.execute(&tool_name, ctx.clone(), tool_params).await;

    let result_val = match result {
        Ok(v) => v,
        Err(e) => {
            store.rules[rule_idx].state.last_error = Some(format!("{}", e));
            store.rules[rule_idx].state.last_check_at = Some(now);
            save_store(paths, &store)?;
            return Ok(json!({
                "rule_id": rule_id,
                "error": format!("Source tool failed: {}", e),
                "triggered": false
            }));
        }
    };

    // Extract metric value via path
    let metric_value = extract_json_path(&result_val, &metric_path);
    let current_value = match metric_value {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(s)) => s.parse::<f64>().unwrap_or(0.0),
        _ => {
            store.rules[rule_idx].state.last_error = Some(format!("metric_path '{}' not found or not numeric", metric_path));
            store.rules[rule_idx].state.last_check_at = Some(now);
            save_store(paths, &store)?;
            return Ok(json!({
                "rule_id": rule_id,
                "error": format!("metric_path '{}' not found in result", metric_path),
                "triggered": false,
                "raw_result": result_val
            }));
        }
    };

    // Evaluate condition
    let triggered = evaluate_condition(
        &operator,
        current_value,
        threshold,
        threshold2,
        prev_value,
    );

    // Check cooldown
    let in_cooldown = last_triggered_at
        .map(|t| (now - t) < (cooldown_secs as i64 * 1000))
        .unwrap_or(false);

    let actually_triggered = triggered && !in_cooldown;

    // Update state
    let rule = &mut store.rules[rule_idx];
    rule.state.prev_value = last_value;
    rule.state.last_value = Some(current_value);
    rule.state.last_check_at = Some(now);
    rule.state.last_error = None;
    if actually_triggered {
        rule.state.last_triggered_at = Some(now);
        rule.state.trigger_count += 1;
    }

    // Build alert message
    let time_str = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
    let value_str = format!("{:.4}", current_value);
    let threshold_str = format!("{:.4}", threshold);
    let op_desc = match operator.as_str() {
        "gt" => ">",
        "lt" => "<",
        "gte" => ">=",
        "lte" => "<=",
        "eq" => "==",
        "ne" => "!=",
        "change_pct" => "变化%",
        "cross_above" => "上穿",
        "cross_below" => "下穿",
        _ => &operator,
    };

    let alert_message = if actually_triggered {
        let template = rule.notify.template.clone().unwrap_or_else(|| {
            "⚠️ 预警: {name} — 当前值 {value} {operator} 阈值 {threshold}".to_string()
        });
        Some(template
            .replace("{name}", &rule.name)
            .replace("{value}", &value_str)
            .replace("{threshold}", &threshold_str)
            .replace("{operator}", op_desc)
            .replace("{time}", &time_str))
    } else {
        None
    };

    // Execute on_trigger action callbacks
    let on_trigger_actions = rule.on_trigger.clone();
    let rule_name_for_actions = rule.name.clone();

    let rule_id_out = rule.id.clone();
    let rule_name = rule.name.clone();
    let notify_channel = rule.notify.channel.clone();
    let on_trigger_count = on_trigger_actions.len();
    let _ = rule;
    save_store(paths, &store)?;

    let mut action_results = Vec::new();
    if actually_triggered && !on_trigger_actions.is_empty() {
        let tool_registry = crate::ToolRegistry::with_defaults();
        for action in &on_trigger_actions {
            // Substitute template vars in params
            let params_str = serde_json::to_string(&action.params).unwrap_or_default();
            let substituted = params_str
                .replace("{value}", &value_str)
                .replace("{threshold}", &threshold_str)
                .replace("{name}", &rule_name_for_actions)
                .replace("{time}", &time_str);
            let action_params: Value = serde_json::from_str(&substituted).unwrap_or(action.params.clone());

            let label = action.label.as_deref().unwrap_or(&action.tool);

            if action.require_confirm {
                // For actions requiring confirmation, just report them — don't auto-execute
                action_results.push(json!({
                    "tool": action.tool,
                    "label": label,
                    "status": "pending_confirmation",
                    "params": action_params,
                    "note": "This action requires user confirmation before execution"
                }));
            } else {
                // Auto-execute non-confirm actions (e.g. notifications, logging)
                match tool_registry.execute(&action.tool, ctx.clone(), action_params.clone()).await {
                    Ok(result) => {
                        action_results.push(json!({
                            "tool": action.tool,
                            "label": label,
                            "status": "executed",
                            "result": result
                        }));
                    }
                    Err(e) => {
                        action_results.push(json!({
                            "tool": action.tool,
                            "label": label,
                            "status": "error",
                            "error": format!("{}", e)
                        }));
                    }
                }
            }
        }
    }

    Ok(json!({
        "rule_id": rule_id_out,
        "name": rule_name,
        "current_value": current_value,
        "threshold": threshold,
        "triggered": actually_triggered,
        "condition_met": triggered,
        "in_cooldown": in_cooldown,
        "alert_message": alert_message,
        "notify_channel": notify_channel,
        "on_trigger_count": on_trigger_count,
        "action_results": action_results,
    }))
}

fn action_history(paths: &Paths, params: &Value) -> Result<Value> {
    let store = load_store(paths)?;
    let rule_id = params["rule_id"].as_str().unwrap();
    let rule = store.rules.iter().find(|r| r.id == rule_id)
        .ok_or_else(|| Error::Tool(format!("Rule '{}' not found", rule_id)))?;

    Ok(json!({
        "rule_id": rule.id,
        "name": rule.name,
        "trigger_count": rule.state.trigger_count,
        "last_triggered_at": rule.state.last_triggered_at,
        "last_value": rule.state.last_value,
        "prev_value": rule.state.prev_value,
        "last_check_at": rule.state.last_check_at,
        "last_error": rule.state.last_error,
    }))
}

/// Evaluate a condition given operator, current value, threshold, and optional previous value.
fn evaluate_condition(
    operator: &str,
    current: f64,
    threshold: f64,
    _threshold2: Option<f64>,
    prev_value: Option<f64>,
) -> bool {
    match operator {
        "gt" => current > threshold,
        "lt" => current < threshold,
        "gte" => current >= threshold,
        "lte" => current <= threshold,
        "eq" => (current - threshold).abs() < f64::EPSILON,
        "ne" => (current - threshold).abs() >= f64::EPSILON,
        "change_pct" => {
            if let Some(prev) = prev_value {
                if prev.abs() < f64::EPSILON {
                    false
                } else {
                    let pct_change = ((current - prev) / prev).abs() * 100.0;
                    pct_change >= threshold
                }
            } else {
                false
            }
        }
        "cross_above" => {
            if let Some(prev) = prev_value {
                prev < threshold && current >= threshold
            } else {
                false
            }
        }
        "cross_below" => {
            if let Some(prev) = prev_value {
                prev > threshold && current <= threshold
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Extract a value from JSON using a dot-separated path.
/// Supports numeric indices: "data.0.price" → data[0].price
fn extract_json_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = value;
    for part in parts {
        if let Ok(idx) = part.parse::<usize>() {
            current = current.get(idx)?;
        } else {
            current = current.get(part)?;
        }
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = AlertRuleTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "alert_rule");
    }

    #[test]
    fn test_validate_create() {
        let tool = AlertRuleTool;
        let params = json!({
            "action": "create",
            "name": "AAPL > 200",
            "source": {"tool": "finance_api", "params": {"action": "stock_quote", "symbol": "AAPL"}},
            "metric_path": "price",
            "operator": "gt",
            "threshold": 200.0
        });
        assert!(tool.validate(&params).is_ok());
    }

    #[test]
    fn test_validate_create_missing_name() {
        let tool = AlertRuleTool;
        let params = json!({"action": "create", "source": {}, "metric_path": "x", "operator": "gt", "threshold": 1});
        assert!(tool.validate(&params).is_err());
    }

    #[test]
    fn test_evaluate_condition_gt() {
        assert!(evaluate_condition("gt", 201.0, 200.0, None, None));
        assert!(!evaluate_condition("gt", 199.0, 200.0, None, None));
    }

    #[test]
    fn test_evaluate_condition_lt() {
        assert!(evaluate_condition("lt", 199.0, 200.0, None, None));
        assert!(!evaluate_condition("lt", 201.0, 200.0, None, None));
    }

    #[test]
    fn test_evaluate_condition_change_pct() {
        assert!(evaluate_condition("change_pct", 110.0, 5.0, None, Some(100.0)));
        assert!(!evaluate_condition("change_pct", 103.0, 5.0, None, Some(100.0)));
        // No previous value → false
        assert!(!evaluate_condition("change_pct", 110.0, 5.0, None, None));
    }

    #[test]
    fn test_evaluate_condition_cross_above() {
        assert!(evaluate_condition("cross_above", 201.0, 200.0, None, Some(199.0)));
        assert!(!evaluate_condition("cross_above", 201.0, 200.0, None, Some(200.5)));
        assert!(!evaluate_condition("cross_above", 199.0, 200.0, None, Some(198.0)));
    }

    #[test]
    fn test_evaluate_condition_cross_below() {
        assert!(evaluate_condition("cross_below", 199.0, 200.0, None, Some(201.0)));
        assert!(!evaluate_condition("cross_below", 199.0, 200.0, None, Some(198.0)));
    }

    #[test]
    fn test_extract_json_path() {
        let data = json!({"data": {"price": 150.5, "items": [{"name": "a"}, {"name": "b"}]}});
        assert_eq!(extract_json_path(&data, "data.price"), Some(&json!(150.5)));
        assert_eq!(extract_json_path(&data, "data.items.0.name"), Some(&json!("a")));
        assert_eq!(extract_json_path(&data, "data.items.1.name"), Some(&json!("b")));
        assert_eq!(extract_json_path(&data, "nonexistent"), None);
    }

    #[test]
    fn test_validate_create_with_on_trigger() {
        let tool = AlertRuleTool;
        let params = json!({
            "action": "create",
            "name": "BTC > 100000 auto-sell",
            "source": {"tool": "finance_api", "params": {"action": "crypto_price", "symbol": "bitcoin"}},
            "metric_path": "bitcoin.usd",
            "operator": "gt",
            "threshold": 100000.0,
            "on_trigger": [
                {
                    "tool": "notification",
                    "params": {"message": "BTC hit {value}! Threshold: {threshold}"},
                    "label": "price_alert",
                    "require_confirm": false
                },
                {
                    "tool": "exchange_api",
                    "params": {"action": "place_order", "exchange": "binance", "symbol": "BTCUSDT", "side": "sell", "amount": "0.1"},
                    "label": "auto_sell",
                    "require_confirm": true
                }
            ]
        });
        assert!(tool.validate(&params).is_ok());
    }

    #[test]
    fn test_alert_action_serde() {
        let action = AlertAction {
            tool: "notification".to_string(),
            params: json!({"message": "Price is {value}"}),
            label: Some("test".to_string()),
            require_confirm: false,
        };
        let serialized = serde_json::to_string(&action).unwrap();
        let deserialized: AlertAction = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.tool, "notification");
        assert_eq!(deserialized.require_confirm, false);
        assert_eq!(deserialized.label, Some("test".to_string()));
    }

    #[test]
    fn test_evaluate_condition_eq_ne() {
        assert!(evaluate_condition("eq", 200.0, 200.0, None, None));
        assert!(!evaluate_condition("eq", 200.1, 200.0, None, None));
        assert!(evaluate_condition("ne", 200.1, 200.0, None, None));
        assert!(!evaluate_condition("ne", 200.0, 200.0, None, None));
    }
}
