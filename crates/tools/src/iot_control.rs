use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::{Tool, ToolContext, ToolSchema};

/// IoT and smart home control tool.
///
/// Supported protocols/platforms:
/// - **Home Assistant**: REST API for device control, automation, scenes, states
/// - **MQTT**: Publish/subscribe via an MQTT broker's REST bridge (e.g. mosquitto REST, EMQX API)
/// - **Generic REST**: Direct HTTP control for any IoT device with a REST API
///
/// For Home Assistant, set api_base to your HA instance URL (e.g. http://homeassistant.local:8123)
/// and api_token to a Long-Lived Access Token.
pub struct IotControlTool;

#[async_trait]
impl Tool for IotControlTool {
    fn schema(&self) -> ToolSchema {
        let str_prop = |desc: &str| -> Value { json!({"type": "string", "description": desc}) };
        let obj_prop = |desc: &str| -> Value { json!({"type": "object", "description": desc}) };
        let int_prop = |desc: &str| -> Value { json!({"type": "integer", "description": desc}) };
        let bool_prop = |desc: &str| -> Value { json!({"type": "boolean", "description": desc}) };

        let mut props = serde_json::Map::new();
        props.insert("platform".into(), json!({"type": "string", "enum": ["home_assistant", "mqtt", "generic"], "description": "IoT platform to interact with"}));
        props.insert("action".into(), str_prop("Action to perform. home_assistant: list_entities|get_state|set_state|call_service|list_services|list_automations|trigger_automation|fire_event|get_history|get_config|render_template. mqtt: publish|subscribe|list_topics|broker_status. generic: get|set|list|info."));
        props.insert("api_base".into(), str_prop("Base URL of the IoT platform API (e.g. 'http://homeassistant.local:8123' for HA, 'http://broker:18083' for EMQX)"));
        props.insert("api_token".into(), str_prop("API token / Long-Lived Access Token"));
        props.insert("entity_id".into(), str_prop("(home_assistant) Entity ID (e.g. 'light.living_room', 'climate.bedroom', 'switch.fan')"));
        props.insert("domain".into(), str_prop("(home_assistant) Service domain (e.g. 'light', 'climate', 'switch', 'automation', 'scene', 'media_player')"));
        props.insert("service".into(), str_prop("(home_assistant) Service name (e.g. 'turn_on', 'turn_off', 'toggle', 'set_temperature')"));
        props.insert("service_data".into(), obj_prop("(home_assistant) Service call data (e.g. {brightness: 255} for lights, {temperature: 22} for climate)"));
        props.insert("state".into(), str_prop("(home_assistant set_state / generic set) New state value"));
        props.insert("attributes".into(), obj_prop("(home_assistant set_state) State attributes"));
        props.insert("event_type".into(), str_prop("(home_assistant fire_event) Event type to fire"));
        props.insert("event_data".into(), obj_prop("(home_assistant fire_event) Event data payload"));
        props.insert("template".into(), str_prop("(home_assistant render_template) Jinja2 template to render"));
        props.insert("automation_id".into(), str_prop("(home_assistant) Automation entity ID"));
        props.insert("start_time".into(), str_prop("(home_assistant get_history) Start time in ISO 8601"));
        props.insert("end_time".into(), str_prop("(home_assistant get_history) End time in ISO 8601"));
        props.insert("topic".into(), str_prop("(mqtt) MQTT topic (e.g. 'home/living_room/temperature')"));
        props.insert("payload".into(), str_prop("(mqtt) Message payload to publish"));
        props.insert("qos".into(), int_prop("(mqtt) Quality of Service level (0, 1, or 2, default: 0)"));
        props.insert("retain".into(), bool_prop("(mqtt) Retain flag for published message (default: false)"));
        props.insert("device_url".into(), str_prop("(generic) Full URL of the device endpoint"));
        props.insert("method".into(), json!({"type": "string", "enum": ["GET", "POST", "PUT", "PATCH"], "description": "(generic) HTTP method"}));
        props.insert("body".into(), obj_prop("(generic) Request body for set action"));
        props.insert("headers".into(), obj_prop("(generic) Custom HTTP headers"));
        props.insert("filter_domain".into(), str_prop("(home_assistant list_entities) Filter entities by domain (e.g. 'light', 'climate', 'sensor')"));
        props.insert("filter_area".into(), str_prop("(home_assistant list_entities) Filter entities by area/room name"));

        ToolSchema {
            name: "iot_control",
            description: "Control IoT devices and smart home systems. Supports Home Assistant (devices, automations, scenes, climate, lights, switches, sensors), MQTT (publish/subscribe via REST bridge), and generic REST-based IoT devices. Requires appropriate API tokens.",
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["platform", "action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let platform = params
            .get("platform")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: platform".to_string()))?;

        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: action".to_string()))?;

        let valid_actions = match platform {
            "home_assistant" => vec![
                "list_entities", "get_state", "set_state", "call_service",
                "list_services", "list_automations", "trigger_automation",
                "fire_event", "get_history", "get_config", "render_template",
            ],
            "mqtt" => vec!["publish", "subscribe", "list_topics", "broker_status"],
            "generic" => vec!["get", "set", "list", "info"],
            _ => {
                return Err(Error::Validation(format!(
                    "Unknown platform: {}. Must be one of: home_assistant, mqtt, generic",
                    platform
                )));
            }
        };

        if !valid_actions.contains(&action) {
            return Err(Error::Validation(format!(
                "Invalid action '{}' for platform '{}'. Valid actions: {}",
                action,
                platform,
                valid_actions.join(", ")
            )));
        }

        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let platform = params["platform"].as_str().unwrap();
        let action = params["action"].as_str().unwrap();

        let api_token = self.resolve_token(&ctx, &params, platform)?;

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| Error::Tool(format!("Failed to create HTTP client: {}", e)))?;

        debug!(platform = platform, action = action, "Executing iot_control");

        match platform {
            "home_assistant" => self.execute_home_assistant(&client, &api_token, action, &params, &ctx).await,
            "mqtt" => self.execute_mqtt(&client, &api_token, action, &params).await,
            "generic" => self.execute_generic(&client, &api_token, action, &params).await,
            _ => Err(Error::Tool(format!("Unknown platform: {}", platform))),
        }
    }
}

impl IotControlTool {
    fn resolve_token(&self, ctx: &ToolContext, params: &Value, platform: &str) -> Result<String> {
        // 1. Explicit param
        if let Some(token) = params.get("api_token").and_then(|v| v.as_str()) {
            if !token.is_empty() {
                return Ok(token.to_string());
            }
        }

        // 2. Config providers section
        let config_key = platform;

        if let Some(provider_config) = ctx.config.providers.get(config_key) {
            if !provider_config.api_key.is_empty() {
                return Ok(provider_config.api_key.clone());
            }
        }

        // 3. Environment variables
        let env_key = match platform {
            "home_assistant" => "HOME_ASSISTANT_TOKEN",
            "mqtt" => "MQTT_API_TOKEN",
            _ => "",
        };

        if !env_key.is_empty() {
            if let Ok(val) = std::env::var(env_key) {
                if !val.is_empty() {
                    return Ok(val);
                }
            }
        }

        // For generic platform, token is optional
        if platform == "generic" {
            return Ok(String::new());
        }

        Err(Error::Tool(format!(
            "No API token found for platform '{}'. Provide 'api_token' parameter, set config providers.{}.api_key, or set {} environment variable.",
            platform, config_key, env_key
        )))
    }

    fn resolve_api_base(&self, ctx: &ToolContext, params: &Value, platform: &str) -> Result<String> {
        // 1. Explicit param
        if let Some(base) = params.get("api_base").and_then(|v| v.as_str()) {
            if !base.is_empty() {
                return Ok(base.trim_end_matches('/').to_string());
            }
        }

        // 2. Config providers section
        let config_key = platform;

        if let Some(provider_config) = ctx.config.providers.get(config_key) {
            if let Some(ref base) = provider_config.api_base {
                if !base.is_empty() {
                    return Ok(base.trim_end_matches('/').to_string());
                }
            }
        }

        // 3. Environment variables
        let env_key = match platform {
            "home_assistant" => "HOME_ASSISTANT_URL",
            "mqtt" => "MQTT_BROKER_URL",
            _ => "",
        };

        if !env_key.is_empty() {
            if let Ok(val) = std::env::var(env_key) {
                if !val.is_empty() {
                    return Ok(val.trim_end_matches('/').to_string());
                }
            }
        }

        Err(Error::Tool(format!(
            "No API base URL found for platform '{}'. Provide 'api_base' parameter or set config providers.{}.api_base.",
            platform, config_key
        )))
    }

    // ========================================================================
    // Home Assistant
    // ========================================================================

    async fn execute_home_assistant(
        &self,
        client: &Client,
        token: &str,
        action: &str,
        params: &Value,
        ctx: &ToolContext,
    ) -> Result<Value> {
        let api_base = self.resolve_api_base(ctx, params, "home_assistant")?;

        let ha_api = format!("{}/api", api_base);

        match action {
            "list_entities" => {
                let url = format!("{}/states", ha_api);

                let resp = client
                    .get(&url)
                    .bearer_auth(token)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Home Assistant API error: {}", e)))?;

                let mut result = self.parse_response(resp, "HA list_entities").await?;

                // Apply domain/area filters
                if let Some(body) = result.get("body").and_then(|v| v.as_array()) {
                    let filter_domain = params.get("filter_domain").and_then(|v| v.as_str());
                    let filter_area = params.get("filter_area").and_then(|v| v.as_str());

                    let filtered: Vec<Value> = body
                        .iter()
                        .filter(|entity| {
                            let entity_id = entity
                                .get("entity_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            // Domain filter
                            if let Some(domain) = filter_domain {
                                if !entity_id.starts_with(&format!("{}.", domain)) {
                                    return false;
                                }
                            }

                            // Area filter (check friendly_name or attributes.area)
                            if let Some(area) = filter_area {
                                let area_lower = area.to_lowercase();
                                let friendly_name = entity
                                    .get("attributes")
                                    .and_then(|a| a.get("friendly_name"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let entity_area = entity
                                    .get("attributes")
                                    .and_then(|a| a.get("area"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");

                                if !friendly_name.to_lowercase().contains(&area_lower)
                                    && !entity_area.to_lowercase().contains(&area_lower)
                                    && !entity_id.to_lowercase().contains(&area_lower)
                                {
                                    return false;
                                }
                            }

                            true
                        })
                        .cloned()
                        .collect();

                    // Return summary
                    let summary: Vec<Value> = filtered
                        .iter()
                        .map(|e| {
                            json!({
                                "entity_id": e.get("entity_id"),
                                "state": e.get("state"),
                                "friendly_name": e.get("attributes").and_then(|a| a.get("friendly_name")),
                                "last_changed": e.get("last_changed")
                            })
                        })
                        .collect();

                    result["body"] = json!(summary);
                    result["total"] = json!(summary.len());
                }

                Ok(result)
            }

            "get_state" => {
                let entity_id = params
                    .get("entity_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("get_state requires 'entity_id'".to_string()))?;

                let url = format!("{}/states/{}", ha_api, urlencoding::encode(entity_id));

                let resp = client
                    .get(&url)
                    .bearer_auth(token)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Home Assistant API error: {}", e)))?;

                self.parse_response(resp, "HA get_state").await
            }

            "set_state" => {
                let entity_id = params
                    .get("entity_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("set_state requires 'entity_id'".to_string()))?;

                let state = params
                    .get("state")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("set_state requires 'state'".to_string()))?;

                let url = format!("{}/states/{}", ha_api, urlencoding::encode(entity_id));

                let mut body = json!({"state": state});
                if let Some(attrs) = params.get("attributes") {
                    body["attributes"] = attrs.clone();
                }

                let resp = client
                    .post(&url)
                    .bearer_auth(token)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Home Assistant API error: {}", e)))?;

                self.parse_response(resp, "HA set_state").await
            }

            "call_service" => {
                let domain = params
                    .get("domain")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("call_service requires 'domain'".to_string()))?;

                let service = params
                    .get("service")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("call_service requires 'service'".to_string()))?;

                let url = format!("{}/services/{}/{}", ha_api, domain, service);

                let mut body = params.get("service_data").cloned().unwrap_or(json!({}));

                // Add entity_id to service data if provided
                if let Some(entity_id) = params.get("entity_id").and_then(|v| v.as_str()) {
                    if body.is_object() {
                        body["entity_id"] = json!(entity_id);
                    }
                }

                let resp = client
                    .post(&url)
                    .bearer_auth(token)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Home Assistant API error: {}", e)))?;

                self.parse_response(resp, "HA call_service").await
            }

            "list_services" => {
                let url = format!("{}/services", ha_api);

                let resp = client
                    .get(&url)
                    .bearer_auth(token)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Home Assistant API error: {}", e)))?;

                let mut result = self.parse_response(resp, "HA list_services").await?;

                // Filter by domain if specified
                if let Some(domain) = params.get("domain").and_then(|v| v.as_str()) {
                    if let Some(body) = result.get("body").and_then(|v| v.as_array()) {
                        let filtered: Vec<&Value> = body
                            .iter()
                            .filter(|svc| {
                                svc.get("domain")
                                    .and_then(|d| d.as_str())
                                    .map(|d| d == domain)
                                    .unwrap_or(false)
                            })
                            .collect();
                        result["body"] = json!(filtered);
                    }
                }

                Ok(result)
            }

            "list_automations" => {
                // List automation entities
                let url = format!("{}/states", ha_api);

                let resp = client
                    .get(&url)
                    .bearer_auth(token)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Home Assistant API error: {}", e)))?;

                let result = self.parse_response(resp, "HA list_automations").await?;

                if let Some(body) = result.get("body").and_then(|v| v.as_array()) {
                    let automations: Vec<Value> = body
                        .iter()
                        .filter(|e| {
                            e.get("entity_id")
                                .and_then(|v| v.as_str())
                                .map(|id| id.starts_with("automation."))
                                .unwrap_or(false)
                        })
                        .map(|e| {
                            json!({
                                "entity_id": e.get("entity_id"),
                                "state": e.get("state"),
                                "friendly_name": e.get("attributes").and_then(|a| a.get("friendly_name")),
                                "last_triggered": e.get("attributes").and_then(|a| a.get("last_triggered"))
                            })
                        })
                        .collect();

                    Ok(json!({
                        "status": 200,
                        "body": automations,
                        "total": automations.len()
                    }))
                } else {
                    Ok(result)
                }
            }

            "trigger_automation" => {
                let automation_id = params
                    .get("automation_id")
                    .or_else(|| params.get("entity_id"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Error::Validation("trigger_automation requires 'automation_id' or 'entity_id'".to_string())
                    })?;

                let url = format!("{}/services/automation/trigger", ha_api);
                let body = json!({"entity_id": automation_id});

                let resp = client
                    .post(&url)
                    .bearer_auth(token)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Home Assistant API error: {}", e)))?;

                self.parse_response(resp, "HA trigger_automation").await
            }

            "fire_event" => {
                let event_type = params
                    .get("event_type")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("fire_event requires 'event_type'".to_string()))?;

                let url = format!("{}/events/{}", ha_api, event_type);
                let body = params.get("event_data").cloned().unwrap_or(json!({}));

                let resp = client
                    .post(&url)
                    .bearer_auth(token)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Home Assistant API error: {}", e)))?;

                self.parse_response(resp, "HA fire_event").await
            }

            "get_history" => {
                let entity_id = params
                    .get("entity_id")
                    .and_then(|v| v.as_str());

                let mut url = format!("{}/history/period", ha_api);

                if let Some(start) = params.get("start_time").and_then(|v| v.as_str()) {
                    url = format!("{}/{}", url, start);
                }

                let mut query_params = Vec::new();
                if let Some(end) = params.get("end_time").and_then(|v| v.as_str()) {
                    query_params.push(("end_time", end.to_string()));
                }
                if let Some(eid) = entity_id {
                    query_params.push(("filter_entity_id", eid.to_string()));
                }

                let resp = client
                    .get(&url)
                    .bearer_auth(token)
                    .query(&query_params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Home Assistant API error: {}", e)))?;

                self.parse_response(resp, "HA get_history").await
            }

            "get_config" => {
                let url = format!("{}/config", ha_api);

                let resp = client
                    .get(&url)
                    .bearer_auth(token)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Home Assistant API error: {}", e)))?;

                self.parse_response(resp, "HA get_config").await
            }

            "render_template" => {
                let template = params
                    .get("template")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("render_template requires 'template'".to_string()))?;

                let url = format!("{}/template", ha_api);
                let body = json!({"template": template});

                let resp = client
                    .post(&url)
                    .bearer_auth(token)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Home Assistant API error: {}", e)))?;

                let status = resp.status().as_u16();
                let text = resp
                    .text()
                    .await
                    .map_err(|e| Error::Tool(format!("HA render_template: {}", e)))?;

                Ok(json!({
                    "status": status,
                    "rendered": text
                }))
            }

            _ => Err(Error::Tool(format!("Unknown home_assistant action: {}", action))),
        }
    }

    // ========================================================================
    // MQTT (via REST bridge)
    // ========================================================================

    async fn execute_mqtt(
        &self,
        client: &Client,
        token: &str,
        action: &str,
        params: &Value,
    ) -> Result<Value> {
        let api_base = params
            .get("api_base")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::Validation(
                    "MQTT platform requires 'api_base' (e.g. 'http://broker:18083' for EMQX, or 'http://homeassistant.local:8123' for HA MQTT)".to_string()
                )
            })?;
        let api_base = api_base.trim_end_matches('/');

        // Detect if this is Home Assistant's MQTT integration
        let is_ha = api_base.contains(":8123") || api_base.contains("homeassistant");

        match action {
            "publish" => {
                let topic = params
                    .get("topic")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("publish requires 'topic'".to_string()))?;

                let payload = params
                    .get("payload")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let qos = params.get("qos").and_then(|v| v.as_u64()).unwrap_or(0);
                let retain = params.get("retain").and_then(|v| v.as_bool()).unwrap_or(false);

                if is_ha {
                    // Use HA MQTT service
                    let url = format!("{}/api/services/mqtt/publish", api_base);
                    let body = json!({
                        "topic": topic,
                        "payload": payload,
                        "qos": qos,
                        "retain": retain
                    });

                    let resp = client
                        .post(&url)
                        .bearer_auth(token)
                        .json(&body)
                        .send()
                        .await
                        .map_err(|e| Error::Tool(format!("MQTT publish error: {}", e)))?;

                    self.parse_response(resp, "MQTT publish (HA)").await
                } else {
                    // EMQX REST API
                    let url = format!("{}/api/v5/publish", api_base);
                    let body = json!({
                        "topic": topic,
                        "payload": payload,
                        "qos": qos,
                        "retain": retain
                    });

                    let mut req = client.post(&url).json(&body);
                    if !token.is_empty() {
                        req = req.bearer_auth(token);
                    }

                    let resp = req
                        .send()
                        .await
                        .map_err(|e| Error::Tool(format!("MQTT publish error: {}", e)))?;

                    self.parse_response(resp, "MQTT publish (EMQX)").await
                }
            }

            "subscribe" => {
                let topic = params
                    .get("topic")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("subscribe requires 'topic'".to_string()))?;

                if is_ha {
                    // HA doesn't have a direct subscribe REST API; use retained messages or history
                    Ok(json!({
                        "note": "Home Assistant MQTT subscribe is event-driven. Use HA automations to react to MQTT messages, or use 'get_state' on MQTT sensor entities.",
                        "suggestion": "Create an HA automation with MQTT trigger, or check sensor entities that are linked to MQTT topics.",
                        "topic": topic
                    }))
                } else {
                    // EMQX: list retained messages for the topic
                    let url = format!("{}/api/v5/mqtt/retainer/message/{}", api_base, urlencoding::encode(topic));

                    let mut req = client.get(&url);
                    if !token.is_empty() {
                        req = req.bearer_auth(token);
                    }

                    let resp = req
                        .send()
                        .await
                        .map_err(|e| Error::Tool(format!("MQTT subscribe error: {}", e)))?;

                    self.parse_response(resp, "MQTT subscribe (retained)").await
                }
            }

            "list_topics" => {
                if is_ha {
                    // List MQTT sensor entities from HA
                    let url = format!("{}/api/states", api_base);

                    let resp = client
                        .get(&url)
                        .bearer_auth(token)
                        .send()
                        .await
                        .map_err(|e| Error::Tool(format!("MQTT list_topics error: {}", e)))?;

                    let result = self.parse_response(resp, "MQTT list_topics (HA)").await?;

                    if let Some(body) = result.get("body").and_then(|v| v.as_array()) {
                        let mqtt_entities: Vec<Value> = body
                            .iter()
                            .filter(|e| {
                                let eid = e.get("entity_id").and_then(|v| v.as_str()).unwrap_or("");
                                eid.contains("mqtt") || {
                                    e.get("attributes")
                                        .and_then(|a| a.get("device_class"))
                                        .and_then(|v| v.as_str())
                                        .is_some()
                                        && eid.starts_with("sensor.")
                                }
                            })
                            .map(|e| {
                                json!({
                                    "entity_id": e.get("entity_id"),
                                    "state": e.get("state"),
                                    "friendly_name": e.get("attributes").and_then(|a| a.get("friendly_name"))
                                })
                            })
                            .collect();

                        Ok(json!({
                            "status": 200,
                            "body": mqtt_entities,
                            "total": mqtt_entities.len()
                        }))
                    } else {
                        Ok(result)
                    }
                } else {
                    // EMQX: list topics
                    let url = format!("{}/api/v5/topics", api_base);

                    let mut req = client.get(&url);
                    if !token.is_empty() {
                        req = req.bearer_auth(token);
                    }

                    let resp = req
                        .send()
                        .await
                        .map_err(|e| Error::Tool(format!("MQTT list_topics error: {}", e)))?;

                    self.parse_response(resp, "MQTT list_topics (EMQX)").await
                }
            }

            "broker_status" => {
                if is_ha {
                    Ok(json!({
                        "platform": "home_assistant_mqtt",
                        "note": "Use 'get_config' on home_assistant platform to check HA status. MQTT broker status is managed by HA internally."
                    }))
                } else {
                    let url = format!("{}/api/v5/status", api_base);

                    let mut req = client.get(&url);
                    if !token.is_empty() {
                        req = req.bearer_auth(token);
                    }

                    let resp = req
                        .send()
                        .await
                        .map_err(|e| Error::Tool(format!("MQTT broker_status error: {}", e)))?;

                    self.parse_response(resp, "MQTT broker_status").await
                }
            }

            _ => Err(Error::Tool(format!("Unknown MQTT action: {}", action))),
        }
    }

    // ========================================================================
    // Generic REST IoT
    // ========================================================================

    async fn execute_generic(
        &self,
        client: &Client,
        token: &str,
        action: &str,
        params: &Value,
    ) -> Result<Value> {
        let device_url = params
            .get("device_url")
            .or_else(|| params.get("api_base"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                Error::Validation("Generic IoT requires 'device_url' or 'api_base'".to_string())
            })?;

        let method = params
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or(match action {
                "set" => "POST",
                _ => "GET",
            });

        let mut req = match method {
            "POST" => client.post(device_url),
            "PUT" => client.put(device_url),
            "PATCH" => client.patch(device_url),
            _ => client.get(device_url),
        };

        // Auth
        if !token.is_empty() {
            req = req.bearer_auth(token);
        }

        // Custom headers
        if let Some(headers) = params.get("headers").and_then(|v| v.as_object()) {
            for (key, value) in headers {
                if let Some(val_str) = value.as_str() {
                    req = req.header(key.as_str(), val_str);
                }
            }
        }

        // Body for set action
        if let Some(body) = params.get("body") {
            req = req.json(body);
        } else if let Some(state) = params.get("state").and_then(|v| v.as_str()) {
            req = req.json(&json!({"state": state}));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| Error::Tool(format!("IoT device error: {}", e)))?;

        self.parse_response(resp, &format!("Generic IoT {}", action)).await
    }

    // ========================================================================
    // Helpers
    // ========================================================================

    async fn parse_response(&self, resp: reqwest::Response, context: &str) -> Result<Value> {
        let status = resp.status().as_u16();
        let status_text = resp.status().canonical_reason().unwrap_or("").to_string();

        let body_bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::Tool(format!("{}: Failed to read response: {}", context, e)))?;

        let body_text = String::from_utf8_lossy(&body_bytes).to_string();

        let body_json: Value = serde_json::from_str(&body_text).unwrap_or(json!(body_text));

        if status >= 400 {
            warn!(
                context = context,
                status = status,
                "IoT API returned error status"
            );
            return Ok(json!({
                "error": true,
                "status": status,
                "status_text": status_text,
                "context": context,
                "body": body_json
            }));
        }

        // Truncate large responses
        let body_str = serde_json::to_string(&body_json).unwrap_or_default();
        let truncated = body_str.len() > 50000;
        let display_body = if truncated {
            let mut end = 50000;
            while end > 0 && !body_str.is_char_boundary(end) {
                end -= 1;
            }
            serde_json::from_str(&body_str[..end]).unwrap_or(json!(body_str[..end].to_string()))
        } else {
            body_json
        };

        Ok(json!({
            "status": status,
            "status_text": status_text,
            "body": display_body,
            "truncated": truncated
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = IotControlTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "iot_control");
        assert!(schema.description.contains("Home Assistant"));
        assert!(schema.description.contains("MQTT"));
    }

    #[test]
    fn test_validate_platforms() {
        let tool = IotControlTool;
        assert!(tool.validate(&json!({"platform": "home_assistant", "action": "list_entities"})).is_ok());
        assert!(tool.validate(&json!({"platform": "mqtt", "action": "publish"})).is_ok());
        assert!(tool.validate(&json!({"platform": "generic", "action": "get"})).is_ok());
    }

    #[test]
    fn test_validate_invalid_platform() {
        let tool = IotControlTool;
        assert!(tool.validate(&json!({"platform": "unknown", "action": "list"})).is_err());
    }

    #[test]
    fn test_validate_invalid_action() {
        let tool = IotControlTool;
        assert!(tool.validate(&json!({"platform": "home_assistant", "action": "invalid"})).is_err());
        assert!(tool.validate(&json!({"platform": "mqtt", "action": "delete"})).is_err());
    }

    #[test]
    fn test_validate_missing_params() {
        let tool = IotControlTool;
        assert!(tool.validate(&json!({})).is_err());
        assert!(tool.validate(&json!({"platform": "home_assistant"})).is_err());
    }

    #[test]
    fn test_validate_all_ha_actions() {
        let tool = IotControlTool;
        for action in &[
            "list_entities", "get_state", "set_state", "call_service",
            "list_services", "list_automations", "trigger_automation",
            "fire_event", "get_history", "get_config", "render_template",
        ] {
            assert!(
                tool.validate(&json!({"platform": "home_assistant", "action": action})).is_ok(),
                "Failed for action: {}",
                action
            );
        }
    }

    #[test]
    fn test_validate_all_mqtt_actions() {
        let tool = IotControlTool;
        for action in &["publish", "subscribe", "list_topics", "broker_status"] {
            assert!(
                tool.validate(&json!({"platform": "mqtt", "action": action})).is_ok(),
                "Failed for action: {}",
                action
            );
        }
    }

    #[test]
    fn test_validate_all_generic_actions() {
        let tool = IotControlTool;
        for action in &["get", "set", "list", "info"] {
            assert!(
                tool.validate(&json!({"platform": "generic", "action": action})).is_ok(),
                "Failed for action: {}",
                action
            );
        }
    }
}
