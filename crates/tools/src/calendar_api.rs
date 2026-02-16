use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::{Tool, ToolContext, ToolSchema};

/// Unified business API tool for calendar, project management, CRM, and ticketing systems.
///
/// Supported services:
/// - **Google Calendar**: events CRUD, free/busy query
/// - **Notion**: pages/databases query, create, update
/// - **CRM (generic)**: contacts, deals, activities via REST
/// - **Ticketing (generic)**: tickets CRUD via REST (Jira, Linear, etc.)
pub struct CalendarApiTool;

#[async_trait]
impl Tool for CalendarApiTool {
    fn schema(&self) -> ToolSchema {
        let str_prop = |desc: &str| -> Value { json!({"type": "string", "description": desc}) };
        let obj_prop = |desc: &str| -> Value { json!({"type": "object", "description": desc}) };
        let int_prop = |desc: &str| -> Value { json!({"type": "integer", "description": desc}) };
        let arr_str_prop = |desc: &str| -> Value { json!({"type": "array", "items": {"type": "string"}, "description": desc}) };
        let arr_prop = |desc: &str| -> Value { json!({"type": "array", "description": desc}) };

        let mut props = serde_json::Map::new();
        props.insert("service".into(), json!({"type": "string", "enum": ["google_calendar", "notion", "crm", "ticketing"], "description": "Target service to interact with"}));
        props.insert("action".into(), str_prop("Action to perform. google_calendar: list_events|create_event|update_event|delete_event|free_busy. notion: query_database|get_page|create_page|update_page|search. crm: list_contacts|get_contact|create_contact|update_contact|list_deals|create_deal|log_activity. ticketing: list_tickets|get_ticket|create_ticket|update_ticket|add_comment|search."));
        props.insert("api_base".into(), str_prop("Base URL for the API (required for crm/ticketing, optional for google_calendar/notion)"));
        props.insert("api_token".into(), str_prop("API token / OAuth access token"));
        props.insert("calendar_id".into(), str_prop("(google_calendar) Calendar ID, default 'primary'"));
        props.insert("database_id".into(), str_prop("(notion) Database ID for query_database"));
        props.insert("page_id".into(), str_prop("(notion) Page ID for get_page/update_page"));
        props.insert("event_id".into(), str_prop("(google_calendar) Event ID for update/delete"));
        props.insert("ticket_id".into(), str_prop("(ticketing) Ticket ID/key for get/update/comment"));
        props.insert("contact_id".into(), str_prop("(crm) Contact ID for get/update"));
        props.insert("deal_id".into(), str_prop("(crm) Deal ID for get/update"));
        props.insert("title".into(), str_prop("Title/summary (for creating events, pages, tickets, deals)"));
        props.insert("description".into(), str_prop("Description/body text"));
        props.insert("start_time".into(), str_prop("Start time in ISO 8601 format (e.g. 2025-01-15T09:00:00+08:00)"));
        props.insert("end_time".into(), str_prop("End time in ISO 8601 format"));
        props.insert("time_min".into(), str_prop("(google_calendar) Lower bound for event start time (ISO 8601)"));
        props.insert("time_max".into(), str_prop("(google_calendar) Upper bound for event start time (ISO 8601)"));
        props.insert("location".into(), str_prop("Location (for events)"));
        props.insert("attendees".into(), arr_str_prop("Attendee email addresses (for events)"));
        props.insert("recurrence".into(), arr_str_prop("Recurrence rules (RRULE format)"));
        props.insert("reminders".into(), obj_prop("Reminder overrides"));
        props.insert("timezone".into(), str_prop("Timezone (e.g. 'Asia/Shanghai', 'America/New_York')"));
        props.insert("properties".into(), obj_prop("(notion) Page properties as key-value pairs"));
        props.insert("content".into(), arr_prop("(notion) Page content blocks"));
        props.insert("filter".into(), obj_prop("(notion/crm/ticketing) Filter/query object"));
        props.insert("sort".into(), arr_prop("(notion) Sort criteria"));
        props.insert("query".into(), str_prop("Search query string (for notion search, ticketing search)"));
        props.insert("fields".into(), obj_prop("(crm/ticketing) Additional fields for create/update"));
        props.insert("status".into(), str_prop("(ticketing) Ticket status (e.g. 'open', 'in_progress', 'done')"));
        props.insert("priority".into(), str_prop("(ticketing) Ticket priority (e.g. 'high', 'medium', 'low')"));
        props.insert("assignee".into(), str_prop("(ticketing) Assignee user ID or email"));
        props.insert("labels".into(), arr_str_prop("(ticketing) Labels/tags"));
        props.insert("comment".into(), str_prop("(ticketing) Comment text for add_comment"));
        props.insert("max_results".into(), int_prop("Maximum number of results to return (default: 50)"));
        props.insert("auth_type".into(), json!({"type": "string", "enum": ["bearer", "basic", "api_key"], "description": "Authentication type (default: bearer)"}));
        props.insert("auth_header_name".into(), str_prop("Custom auth header name (for api_key auth, e.g. 'X-API-Key')"));

        ToolSchema {
            name: "calendar_api",
            description: "Interact with business APIs: Google Calendar (events, free/busy), Notion (pages, databases), CRM (contacts, deals), and ticketing systems (Jira, Linear, etc.). Requires appropriate API keys/tokens configured in the agent config or passed as parameters.",
            parameters: json!({
                "type": "object",
                "properties": Value::Object(props),
                "required": ["service", "action"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let service = params
            .get("service")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: service".to_string()))?;

        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: action".to_string()))?;

        let valid_actions = match service {
            "google_calendar" => vec![
                "list_events", "create_event", "update_event", "delete_event", "free_busy",
            ],
            "notion" => vec![
                "query_database", "get_page", "create_page", "update_page", "search",
            ],
            "crm" => vec![
                "list_contacts", "get_contact", "create_contact", "update_contact",
                "list_deals", "create_deal", "log_activity",
            ],
            "ticketing" => vec![
                "list_tickets", "get_ticket", "create_ticket", "update_ticket",
                "add_comment", "search",
            ],
            _ => {
                return Err(Error::Validation(format!(
                    "Unknown service: {}. Must be one of: google_calendar, notion, crm, ticketing",
                    service
                )));
            }
        };

        if !valid_actions.contains(&action) {
            return Err(Error::Validation(format!(
                "Invalid action '{}' for service '{}'. Valid actions: {}",
                action,
                service,
                valid_actions.join(", ")
            )));
        }

        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let service = params["service"].as_str().unwrap();
        let action = params["action"].as_str().unwrap();

        // Resolve API token from params or config
        let api_token = self.resolve_token(&ctx, &params, service)?;

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| Error::Tool(format!("Failed to create HTTP client: {}", e)))?;

        debug!(service = service, action = action, "Executing calendar_api");

        match service {
            "google_calendar" => self.execute_google_calendar(&client, &api_token, action, &params).await,
            "notion" => self.execute_notion(&client, &api_token, action, &params).await,
            "crm" => self.execute_crm(&client, &api_token, action, &params).await,
            "ticketing" => self.execute_ticketing(&client, &api_token, action, &params).await,
            _ => Err(Error::Tool(format!("Unknown service: {}", service))),
        }
    }
}

impl CalendarApiTool {
    /// Resolve API token from params, config providers, or environment variables.
    fn resolve_token(&self, ctx: &ToolContext, params: &Value, service: &str) -> Result<String> {
        // 1. Explicit param
        if let Some(token) = params.get("api_token").and_then(|v| v.as_str()) {
            if !token.is_empty() {
                return Ok(token.to_string());
            }
        }

        // 2. Config providers section
        let config_key = match service {
            "google_calendar" => "google",
            "notion" => "notion",
            "crm" => "crm",
            "ticketing" => "ticketing",
            _ => service,
        };

        if let Some(provider_config) = ctx.config.providers.get(config_key) {
            if !provider_config.api_key.is_empty() {
                return Ok(provider_config.api_key.clone());
            }
        }

        // 3. Environment variables
        let env_key = match service {
            "google_calendar" => "GOOGLE_CALENDAR_TOKEN",
            "notion" => "NOTION_API_KEY",
            "crm" => "CRM_API_TOKEN",
            "ticketing" => "TICKETING_API_TOKEN",
            _ => "",
        };

        if !env_key.is_empty() {
            if let Ok(val) = std::env::var(env_key) {
                if !val.is_empty() {
                    return Ok(val);
                }
            }
        }

        Err(Error::Tool(format!(
            "No API token found for service '{}'. Provide 'api_token' parameter, set config providers.{}.api_key, or set {} environment variable.",
            service, config_key, env_key
        )))
    }

    /// Build authenticated request with the appropriate auth method.
    fn auth_request(
        &self,
        request: reqwest::RequestBuilder,
        token: &str,
        params: &Value,
    ) -> reqwest::RequestBuilder {
        let auth_type = params
            .get("auth_type")
            .and_then(|v| v.as_str())
            .unwrap_or("bearer");

        match auth_type {
            "basic" => request.basic_auth(token, Option::<&str>::None),
            "api_key" => {
                let header_name = params
                    .get("auth_header_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("X-API-Key");
                request.header(header_name, token)
            }
            _ => request.bearer_auth(token),
        }
    }

    // ========================================================================
    // Google Calendar
    // ========================================================================

    async fn execute_google_calendar(
        &self,
        client: &Client,
        token: &str,
        action: &str,
        params: &Value,
    ) -> Result<Value> {
        let calendar_id = params
            .get("calendar_id")
            .and_then(|v| v.as_str())
            .unwrap_or("primary");

        let base = format!(
            "https://www.googleapis.com/calendar/v3/calendars/{}",
            urlencoding::encode(calendar_id)
        );

        match action {
            "list_events" => {
                let url = format!("{}/events", base);
                let max_results = params
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50);

                let mut query_params = vec![
                    ("maxResults".to_string(), max_results.to_string()),
                    ("singleEvents".to_string(), "true".to_string()),
                    ("orderBy".to_string(), "startTime".to_string()),
                ];

                if let Some(time_min) = params.get("time_min").and_then(|v| v.as_str()) {
                    query_params.push(("timeMin".to_string(), time_min.to_string()));
                }
                if let Some(time_max) = params.get("time_max").and_then(|v| v.as_str()) {
                    query_params.push(("timeMax".to_string(), time_max.to_string()));
                }
                if let Some(tz) = params.get("timezone").and_then(|v| v.as_str()) {
                    query_params.push(("timeZone".to_string(), tz.to_string()));
                }
                if let Some(q) = params.get("query").and_then(|v| v.as_str()) {
                    query_params.push(("q".to_string(), q.to_string()));
                }

                let resp = self
                    .auth_request(client.get(&url).query(&query_params), token, params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Google Calendar API error: {}", e)))?;

                self.parse_response(resp, "Google Calendar list_events").await
            }

            "create_event" => {
                let url = format!("{}/events", base);

                let mut event = json!({});

                if let Some(title) = params.get("title").and_then(|v| v.as_str()) {
                    event["summary"] = json!(title);
                }
                if let Some(desc) = params.get("description").and_then(|v| v.as_str()) {
                    event["description"] = json!(desc);
                }
                if let Some(loc) = params.get("location").and_then(|v| v.as_str()) {
                    event["location"] = json!(loc);
                }

                let tz = params
                    .get("timezone")
                    .and_then(|v| v.as_str())
                    .unwrap_or("UTC");

                if let Some(start) = params.get("start_time").and_then(|v| v.as_str()) {
                    event["start"] = json!({
                        "dateTime": start,
                        "timeZone": tz
                    });
                }
                if let Some(end) = params.get("end_time").and_then(|v| v.as_str()) {
                    event["end"] = json!({
                        "dateTime": end,
                        "timeZone": tz
                    });
                }

                if let Some(attendees) = params.get("attendees").and_then(|v| v.as_array()) {
                    let att: Vec<Value> = attendees
                        .iter()
                        .filter_map(|a| a.as_str().map(|email| json!({"email": email})))
                        .collect();
                    event["attendees"] = json!(att);
                }

                if let Some(recurrence) = params.get("recurrence") {
                    event["recurrence"] = recurrence.clone();
                }

                if let Some(reminders) = params.get("reminders") {
                    event["reminders"] = reminders.clone();
                }

                let resp = self
                    .auth_request(client.post(&url).json(&event), token, params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Google Calendar API error: {}", e)))?;

                self.parse_response(resp, "Google Calendar create_event").await
            }

            "update_event" => {
                let event_id = params
                    .get("event_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("update_event requires 'event_id'".to_string()))?;

                let url = format!("{}/events/{}", base, urlencoding::encode(event_id));

                let mut patch = json!({});
                if let Some(title) = params.get("title").and_then(|v| v.as_str()) {
                    patch["summary"] = json!(title);
                }
                if let Some(desc) = params.get("description").and_then(|v| v.as_str()) {
                    patch["description"] = json!(desc);
                }
                if let Some(loc) = params.get("location").and_then(|v| v.as_str()) {
                    patch["location"] = json!(loc);
                }
                let tz = params
                    .get("timezone")
                    .and_then(|v| v.as_str())
                    .unwrap_or("UTC");
                if let Some(start) = params.get("start_time").and_then(|v| v.as_str()) {
                    patch["start"] = json!({"dateTime": start, "timeZone": tz});
                }
                if let Some(end) = params.get("end_time").and_then(|v| v.as_str()) {
                    patch["end"] = json!({"dateTime": end, "timeZone": tz});
                }
                if let Some(attendees) = params.get("attendees").and_then(|v| v.as_array()) {
                    let att: Vec<Value> = attendees
                        .iter()
                        .filter_map(|a| a.as_str().map(|email| json!({"email": email})))
                        .collect();
                    patch["attendees"] = json!(att);
                }

                let resp = self
                    .auth_request(client.patch(&url).json(&patch), token, params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Google Calendar API error: {}", e)))?;

                self.parse_response(resp, "Google Calendar update_event").await
            }

            "delete_event" => {
                let event_id = params
                    .get("event_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("delete_event requires 'event_id'".to_string()))?;

                let url = format!("{}/events/{}", base, urlencoding::encode(event_id));

                let resp = self
                    .auth_request(client.delete(&url), token, params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Google Calendar API error: {}", e)))?;

                let status = resp.status().as_u16();
                if status == 204 || status == 200 {
                    Ok(json!({
                        "status": "deleted",
                        "event_id": event_id,
                        "calendar_id": calendar_id
                    }))
                } else {
                    self.parse_response(resp, "Google Calendar delete_event").await
                }
            }

            "free_busy" => {
                let time_min = params
                    .get("time_min")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("free_busy requires 'time_min'".to_string()))?;
                let time_max = params
                    .get("time_max")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("free_busy requires 'time_max'".to_string()))?;

                let body = json!({
                    "timeMin": time_min,
                    "timeMax": time_max,
                    "timeZone": params.get("timezone").and_then(|v| v.as_str()).unwrap_or("UTC"),
                    "items": [{"id": calendar_id}]
                });

                let resp = self
                    .auth_request(
                        client.post("https://www.googleapis.com/calendar/v3/freeBusy").json(&body),
                        token,
                        params,
                    )
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Google Calendar API error: {}", e)))?;

                self.parse_response(resp, "Google Calendar free_busy").await
            }

            _ => Err(Error::Tool(format!("Unknown google_calendar action: {}", action))),
        }
    }

    // ========================================================================
    // Notion
    // ========================================================================

    async fn execute_notion(
        &self,
        client: &Client,
        token: &str,
        action: &str,
        params: &Value,
    ) -> Result<Value> {
        let notion_version = "2022-06-28";

        match action {
            "query_database" => {
                let db_id = params
                    .get("database_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("query_database requires 'database_id'".to_string()))?;

                let url = format!("https://api.notion.com/v1/databases/{}/query", db_id);

                let mut body = json!({});
                if let Some(filter) = params.get("filter") {
                    body["filter"] = filter.clone();
                }
                if let Some(sort) = params.get("sort") {
                    body["sorts"] = sort.clone();
                }
                let page_size = params
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50)
                    .min(100);
                body["page_size"] = json!(page_size);

                let resp = client
                    .post(&url)
                    .bearer_auth(token)
                    .header("Notion-Version", notion_version)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Notion API error: {}", e)))?;

                self.parse_response(resp, "Notion query_database").await
            }

            "get_page" => {
                let page_id = params
                    .get("page_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("get_page requires 'page_id'".to_string()))?;

                let url = format!("https://api.notion.com/v1/pages/{}", page_id);

                let resp = client
                    .get(&url)
                    .bearer_auth(token)
                    .header("Notion-Version", notion_version)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Notion API error: {}", e)))?;

                self.parse_response(resp, "Notion get_page").await
            }

            "create_page" => {
                let url = "https://api.notion.com/v1/pages";

                let mut body = json!({});

                // Parent: database or page
                if let Some(db_id) = params.get("database_id").and_then(|v| v.as_str()) {
                    body["parent"] = json!({"database_id": db_id});
                } else if let Some(page_id) = params.get("page_id").and_then(|v| v.as_str()) {
                    body["parent"] = json!({"page_id": page_id});
                } else {
                    return Err(Error::Validation(
                        "create_page requires 'database_id' or 'page_id' as parent".to_string(),
                    ));
                }

                // Properties
                if let Some(props) = params.get("properties") {
                    body["properties"] = props.clone();
                } else if let Some(title) = params.get("title").and_then(|v| v.as_str()) {
                    // Simple title-only creation
                    body["properties"] = json!({
                        "title": {
                            "title": [{"text": {"content": title}}]
                        }
                    });
                }

                // Content blocks
                if let Some(content) = params.get("content") {
                    body["children"] = content.clone();
                }

                let resp = client
                    .post(url)
                    .bearer_auth(token)
                    .header("Notion-Version", notion_version)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Notion API error: {}", e)))?;

                self.parse_response(resp, "Notion create_page").await
            }

            "update_page" => {
                let page_id = params
                    .get("page_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("update_page requires 'page_id'".to_string()))?;

                let url = format!("https://api.notion.com/v1/pages/{}", page_id);

                let mut body = json!({});
                if let Some(props) = params.get("properties") {
                    body["properties"] = props.clone();
                }

                let resp = client
                    .patch(&url)
                    .bearer_auth(token)
                    .header("Notion-Version", notion_version)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Notion API error: {}", e)))?;

                self.parse_response(resp, "Notion update_page").await
            }

            "search" => {
                let url = "https://api.notion.com/v1/search";

                let mut body = json!({});
                if let Some(query) = params.get("query").and_then(|v| v.as_str()) {
                    body["query"] = json!(query);
                }
                if let Some(filter) = params.get("filter") {
                    body["filter"] = filter.clone();
                }
                if let Some(sort) = params.get("sort") {
                    body["sort"] = sort.clone();
                }
                let page_size = params
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20)
                    .min(100);
                body["page_size"] = json!(page_size);

                let resp = client
                    .post(url)
                    .bearer_auth(token)
                    .header("Notion-Version", notion_version)
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Notion API error: {}", e)))?;

                self.parse_response(resp, "Notion search").await
            }

            _ => Err(Error::Tool(format!("Unknown notion action: {}", action))),
        }
    }

    // ========================================================================
    // CRM (generic REST)
    // ========================================================================

    async fn execute_crm(
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
                    "CRM service requires 'api_base' (e.g. 'https://api.hubspot.com/crm/v3')".to_string(),
                )
            })?;

        let api_base = api_base.trim_end_matches('/');

        match action {
            "list_contacts" => {
                let url = format!("{}/contacts", api_base);
                let max_results = params
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50);

                let mut req = client.get(&url).query(&[("limit", max_results.to_string())]);
                req = self.auth_request(req, token, params);

                if let Some(filter) = params.get("filter") {
                    // Some CRMs support filter as query param or body
                    if let Some(obj) = filter.as_object() {
                        for (k, v) in obj {
                            if let Some(s) = v.as_str() {
                                req = req.query(&[(k.as_str(), s)]);
                            }
                        }
                    }
                }

                let resp = req
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("CRM API error: {}", e)))?;

                self.parse_response(resp, "CRM list_contacts").await
            }

            "get_contact" => {
                let contact_id = params
                    .get("contact_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("get_contact requires 'contact_id'".to_string()))?;

                let url = format!("{}/contacts/{}", api_base, contact_id);
                let resp = self
                    .auth_request(client.get(&url), token, params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("CRM API error: {}", e)))?;

                self.parse_response(resp, "CRM get_contact").await
            }

            "create_contact" => {
                let url = format!("{}/contacts", api_base);

                let mut body = json!({});
                if let Some(fields) = params.get("fields") {
                    body = fields.clone();
                }
                // Convenience: merge top-level title/description into fields
                if let Some(title) = params.get("title").and_then(|v| v.as_str()) {
                    body["name"] = json!(title);
                }
                if let Some(desc) = params.get("description").and_then(|v| v.as_str()) {
                    body["description"] = json!(desc);
                }

                let resp = self
                    .auth_request(client.post(&url).json(&body), token, params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("CRM API error: {}", e)))?;

                self.parse_response(resp, "CRM create_contact").await
            }

            "update_contact" => {
                let contact_id = params
                    .get("contact_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("update_contact requires 'contact_id'".to_string()))?;

                let url = format!("{}/contacts/{}", api_base, contact_id);

                let body = params.get("fields").cloned().unwrap_or(json!({}));

                let resp = self
                    .auth_request(client.patch(&url).json(&body), token, params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("CRM API error: {}", e)))?;

                self.parse_response(resp, "CRM update_contact").await
            }

            "list_deals" => {
                let url = format!("{}/deals", api_base);
                let max_results = params
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50);

                let resp = self
                    .auth_request(
                        client.get(&url).query(&[("limit", max_results.to_string())]),
                        token,
                        params,
                    )
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("CRM API error: {}", e)))?;

                self.parse_response(resp, "CRM list_deals").await
            }

            "create_deal" => {
                let url = format!("{}/deals", api_base);

                let mut body = params.get("fields").cloned().unwrap_or(json!({}));
                if let Some(title) = params.get("title").and_then(|v| v.as_str()) {
                    body["name"] = json!(title);
                }
                if let Some(desc) = params.get("description").and_then(|v| v.as_str()) {
                    body["description"] = json!(desc);
                }

                let resp = self
                    .auth_request(client.post(&url).json(&body), token, params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("CRM API error: {}", e)))?;

                self.parse_response(resp, "CRM create_deal").await
            }

            "log_activity" => {
                let url = format!("{}/activities", api_base);

                let mut body = params.get("fields").cloned().unwrap_or(json!({}));
                if let Some(title) = params.get("title").and_then(|v| v.as_str()) {
                    body["subject"] = json!(title);
                }
                if let Some(desc) = params.get("description").and_then(|v| v.as_str()) {
                    body["body"] = json!(desc);
                }
                if let Some(contact_id) = params.get("contact_id").and_then(|v| v.as_str()) {
                    body["contact_id"] = json!(contact_id);
                }
                if let Some(deal_id) = params.get("deal_id").and_then(|v| v.as_str()) {
                    body["deal_id"] = json!(deal_id);
                }

                let resp = self
                    .auth_request(client.post(&url).json(&body), token, params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("CRM API error: {}", e)))?;

                self.parse_response(resp, "CRM log_activity").await
            }

            _ => Err(Error::Tool(format!("Unknown CRM action: {}", action))),
        }
    }

    // ========================================================================
    // Ticketing (generic REST — Jira, Linear, etc.)
    // ========================================================================

    async fn execute_ticketing(
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
                    "Ticketing service requires 'api_base' (e.g. 'https://your-domain.atlassian.net/rest/api/3')".to_string(),
                )
            })?;

        let api_base = api_base.trim_end_matches('/');

        match action {
            "list_tickets" => {
                let url = format!("{}/issues", api_base);
                let max_results = params
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50);

                let mut req = client.get(&url).query(&[("maxResults", max_results.to_string())]);
                req = self.auth_request(req, token, params);

                if let Some(status) = params.get("status").and_then(|v| v.as_str()) {
                    req = req.query(&[("status", status)]);
                }
                if let Some(assignee) = params.get("assignee").and_then(|v| v.as_str()) {
                    req = req.query(&[("assignee", assignee)]);
                }
                if let Some(filter) = params.get("filter").and_then(|v| v.as_object()) {
                    for (k, v) in filter {
                        if let Some(s) = v.as_str() {
                            req = req.query(&[(k.as_str(), s)]);
                        }
                    }
                }

                let resp = req
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Ticketing API error: {}", e)))?;

                self.parse_response(resp, "Ticketing list_tickets").await
            }

            "get_ticket" => {
                let ticket_id = params
                    .get("ticket_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("get_ticket requires 'ticket_id'".to_string()))?;

                let url = format!("{}/issues/{}", api_base, urlencoding::encode(ticket_id));

                let resp = self
                    .auth_request(client.get(&url), token, params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Ticketing API error: {}", e)))?;

                self.parse_response(resp, "Ticketing get_ticket").await
            }

            "create_ticket" => {
                let url = format!("{}/issues", api_base);

                let mut body = params.get("fields").cloned().unwrap_or(json!({}));

                if let Some(title) = params.get("title").and_then(|v| v.as_str()) {
                    body["summary"] = json!(title);
                }
                if let Some(desc) = params.get("description").and_then(|v| v.as_str()) {
                    body["description"] = json!(desc);
                }
                if let Some(status) = params.get("status").and_then(|v| v.as_str()) {
                    body["status"] = json!(status);
                }
                if let Some(priority) = params.get("priority").and_then(|v| v.as_str()) {
                    body["priority"] = json!(priority);
                }
                if let Some(assignee) = params.get("assignee").and_then(|v| v.as_str()) {
                    body["assignee"] = json!(assignee);
                }
                if let Some(labels) = params.get("labels") {
                    body["labels"] = labels.clone();
                }

                // Wrap in Jira-style "fields" envelope if api_base looks like Jira
                let final_body = if api_base.contains("atlassian.net") || api_base.contains("/rest/api/") {
                    json!({"fields": body})
                } else {
                    body
                };

                let resp = self
                    .auth_request(client.post(&url).json(&final_body), token, params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Ticketing API error: {}", e)))?;

                self.parse_response(resp, "Ticketing create_ticket").await
            }

            "update_ticket" => {
                let ticket_id = params
                    .get("ticket_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("update_ticket requires 'ticket_id'".to_string()))?;

                let url = format!("{}/issues/{}", api_base, urlencoding::encode(ticket_id));

                let mut body = params.get("fields").cloned().unwrap_or(json!({}));

                if let Some(title) = params.get("title").and_then(|v| v.as_str()) {
                    body["summary"] = json!(title);
                }
                if let Some(desc) = params.get("description").and_then(|v| v.as_str()) {
                    body["description"] = json!(desc);
                }
                if let Some(status) = params.get("status").and_then(|v| v.as_str()) {
                    body["status"] = json!(status);
                }
                if let Some(priority) = params.get("priority").and_then(|v| v.as_str()) {
                    body["priority"] = json!(priority);
                }
                if let Some(assignee) = params.get("assignee").and_then(|v| v.as_str()) {
                    body["assignee"] = json!(assignee);
                }
                if let Some(labels) = params.get("labels") {
                    body["labels"] = labels.clone();
                }

                let final_body = if api_base.contains("atlassian.net") || api_base.contains("/rest/api/") {
                    json!({"fields": body})
                } else {
                    body
                };

                let resp = self
                    .auth_request(client.put(&url).json(&final_body), token, params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Ticketing API error: {}", e)))?;

                self.parse_response(resp, "Ticketing update_ticket").await
            }

            "add_comment" => {
                let ticket_id = params
                    .get("ticket_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("add_comment requires 'ticket_id'".to_string()))?;

                let comment_text = params
                    .get("comment")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("add_comment requires 'comment'".to_string()))?;

                let url = format!(
                    "{}/issues/{}/comments",
                    api_base,
                    urlencoding::encode(ticket_id)
                );

                // Jira-style ADF body vs simple body
                let body = if api_base.contains("atlassian.net") || api_base.contains("/rest/api/") {
                    json!({
                        "body": {
                            "type": "doc",
                            "version": 1,
                            "content": [{
                                "type": "paragraph",
                                "content": [{
                                    "type": "text",
                                    "text": comment_text
                                }]
                            }]
                        }
                    })
                } else {
                    json!({"body": comment_text})
                };

                let resp = self
                    .auth_request(client.post(&url).json(&body), token, params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Ticketing API error: {}", e)))?;

                self.parse_response(resp, "Ticketing add_comment").await
            }

            "search" => {
                let query = params
                    .get("query")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| Error::Validation("search requires 'query'".to_string()))?;

                let max_results = params
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50);

                // Jira uses JQL via /search endpoint
                let (url, body) = if api_base.contains("atlassian.net") || api_base.contains("/rest/api/") {
                    let search_url = api_base
                        .replace("/issue", "/search")
                        .replace("/issues", "/search");
                    let search_url = if search_url.ends_with("/search") {
                        search_url
                    } else {
                        format!("{}/search", api_base)
                    };
                    (
                        search_url,
                        json!({
                            "jql": query,
                            "maxResults": max_results
                        }),
                    )
                } else {
                    (
                        format!("{}/search", api_base),
                        json!({
                            "query": query,
                            "limit": max_results
                        }),
                    )
                };

                let resp = self
                    .auth_request(client.post(&url).json(&body), token, params)
                    .send()
                    .await
                    .map_err(|e| Error::Tool(format!("Ticketing API error: {}", e)))?;

                self.parse_response(resp, "Ticketing search").await
            }

            _ => Err(Error::Tool(format!("Unknown ticketing action: {}", action))),
        }
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

        // Try to parse as JSON
        let body_json: Value = serde_json::from_str(&body_text).unwrap_or(json!(body_text));

        if status >= 400 {
            warn!(
                context = context,
                status = status,
                "API returned error status"
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
        let tool = CalendarApiTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "calendar_api");
        assert!(schema.description.contains("Google Calendar"));
        assert!(schema.description.contains("Notion"));
        assert!(schema.description.contains("CRM"));
    }

    #[test]
    fn test_validate_services() {
        let tool = CalendarApiTool;
        assert!(tool.validate(&json!({"service": "google_calendar", "action": "list_events"})).is_ok());
        assert!(tool.validate(&json!({"service": "notion", "action": "search"})).is_ok());
        assert!(tool.validate(&json!({"service": "crm", "action": "list_contacts"})).is_ok());
        assert!(tool.validate(&json!({"service": "ticketing", "action": "create_ticket"})).is_ok());
    }

    #[test]
    fn test_validate_invalid_service() {
        let tool = CalendarApiTool;
        assert!(tool.validate(&json!({"service": "unknown", "action": "list"})).is_err());
    }

    #[test]
    fn test_validate_invalid_action() {
        let tool = CalendarApiTool;
        assert!(tool.validate(&json!({"service": "google_calendar", "action": "invalid"})).is_err());
        assert!(tool.validate(&json!({"service": "notion", "action": "delete_all"})).is_err());
    }

    #[test]
    fn test_validate_missing_params() {
        let tool = CalendarApiTool;
        assert!(tool.validate(&json!({})).is_err());
        assert!(tool.validate(&json!({"service": "google_calendar"})).is_err());
    }

    #[test]
    fn test_validate_all_google_calendar_actions() {
        let tool = CalendarApiTool;
        for action in &["list_events", "create_event", "update_event", "delete_event", "free_busy"] {
            assert!(
                tool.validate(&json!({"service": "google_calendar", "action": action})).is_ok(),
                "Failed for action: {}",
                action
            );
        }
    }

    #[test]
    fn test_validate_all_notion_actions() {
        let tool = CalendarApiTool;
        for action in &["query_database", "get_page", "create_page", "update_page", "search"] {
            assert!(
                tool.validate(&json!({"service": "notion", "action": action})).is_ok(),
                "Failed for action: {}",
                action
            );
        }
    }

    #[test]
    fn test_validate_all_crm_actions() {
        let tool = CalendarApiTool;
        for action in &["list_contacts", "get_contact", "create_contact", "update_contact", "list_deals", "create_deal", "log_activity"] {
            assert!(
                tool.validate(&json!({"service": "crm", "action": action})).is_ok(),
                "Failed for action: {}",
                action
            );
        }
    }

    #[test]
    fn test_validate_all_ticketing_actions() {
        let tool = CalendarApiTool;
        for action in &["list_tickets", "get_ticket", "create_ticket", "update_ticket", "add_comment", "search"] {
            assert!(
                tool.validate(&json!({"service": "ticketing", "action": action})).is_ok(),
                "Failed for action: {}",
                action
            );
        }
    }
}
