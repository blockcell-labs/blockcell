use async_trait::async_trait;
use blockcell_core::{Error, Result};
use reqwest::Client;
use serde_json::{json, Value};

use crate::{Tool, ToolContext, ToolSchema};

fn parse_string_map(input: &str) -> Option<serde_json::Map<String, Value>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Some(serde_json::Map::new());
    }

    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(trimmed) {
        return Some(map);
    }

    let normalized = if trimmed.starts_with('{') || trimmed.starts_with('[') {
        trimmed.to_string()
    } else {
        format!("{{{}}}", trimmed)
    };

    let mut map = serde_json::Map::new();
    for pair in normalized.split(',') {
        let pair = pair
            .trim()
            .trim_start_matches('{')
            .trim_end_matches('}')
            .trim();
        if pair.is_empty() {
            continue;
        }

        let (raw_key, raw_value) = pair.split_once("=>")?;
        let key = strip_wrapping_quotes(raw_key.trim());
        let value = parse_scalar_value(raw_value.trim());
        map.insert(key, value);
    }

    Some(map)
}

fn parse_json_like_value(value: &Value) -> Option<Value> {
    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return None;
            }

            if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                return Some(parsed);
            }

            parse_string_map(trimmed).map(Value::Object)
        }
        other => Some(other.clone()),
    }
}

fn strip_wrapping_quotes(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        let first = bytes[0];
        let last = bytes[trimmed.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

fn parse_scalar_value(input: &str) -> Value {
    let trimmed = input.trim();
    let unquoted = strip_wrapping_quotes(trimmed);

    if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
        return parsed;
    }

    if unquoted.eq_ignore_ascii_case("true") {
        return Value::Bool(true);
    }
    if unquoted.eq_ignore_ascii_case("false") {
        return Value::Bool(false);
    }
    if unquoted.eq_ignore_ascii_case("null") {
        return Value::Null;
    }
    if let Ok(parsed) = unquoted.parse::<i64>() {
        return json!(parsed);
    }
    if let Ok(parsed) = unquoted.parse::<f64>() {
        return json!(parsed);
    }

    Value::String(unquoted)
}

pub struct HttpRequestTool;

#[async_trait]
impl Tool for HttpRequestTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "http_request",
            description: "Make HTTP requests to REST APIs. Supports all HTTP methods, custom headers, authentication (API key, Bearer token, Basic auth), JSON/form bodies, and file downloads.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "Request URL (must be http or https)"
                    },
                    "method": {
                        "type": "string",
                        "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"],
                        "description": "HTTP method, default GET"
                    },
                    "headers": {
                        "type": "object",
                        "description": "Custom headers as key-value pairs, e.g. {\"Content-Type\": \"application/json\", \"X-Custom\": \"value\"}"
                    },
                    "body": {
                        "type": "object",
                        "description": "JSON request body (for POST/PUT/PATCH). Automatically sets Content-Type: application/json."
                    },
                    "body_raw": {
                        "type": "string",
                        "description": "Raw string request body (for non-JSON payloads like XML, form-urlencoded, etc.)"
                    },
                    "form": {
                        "type": "object",
                        "description": "Form data as key-value pairs (application/x-www-form-urlencoded)"
                    },
                    "auth_type": {
                        "type": "string",
                        "enum": ["bearer", "basic", "api_key"],
                        "description": "Authentication type"
                    },
                    "auth_token": {
                        "type": "string",
                        "description": "(bearer) Bearer token value"
                    },
                    "auth_username": {
                        "type": "string",
                        "description": "(basic) Username for Basic auth"
                    },
                    "auth_password": {
                        "type": "string",
                        "description": "(basic) Password for Basic auth"
                    },
                    "auth_key_name": {
                        "type": "string",
                        "description": "(api_key) Header name for API key, e.g. 'X-API-Key'"
                    },
                    "auth_key_value": {
                        "type": "string",
                        "description": "(api_key) API key value"
                    },
                    "query_params": {
                        "type": "object",
                        "description": "URL query parameters as key-value pairs"
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Request timeout in seconds (default: 30, max: 120)"
                    },
                    "save_to": {
                        "type": "string",
                        "description": "Save response body to this file path (for downloading files)"
                    },
                    "follow_redirects": {
                        "type": "boolean",
                        "description": "Follow HTTP redirects, default true"
                    },
                    "max_response_chars": {
                        "type": "integer",
                        "description": "Maximum characters of response body to return (default: 50000)"
                    }
                },
                "required": ["url"]
            }),
        }
    }

    fn validate(&self, params: &Value) -> Result<()> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Validation("Missing required parameter: url".to_string()))?;

        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(Error::Validation(
                "URL must start with http:// or https://".to_string(),
            ));
        }

        if let Some(method) = params.get("method").and_then(|v| v.as_str()) {
            let valid = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
            if !valid.contains(&method) {
                return Err(Error::Validation(format!(
                    "Invalid HTTP method: {}",
                    method
                )));
            }
        }

        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let url = params["url"].as_str().unwrap();
        let method = params
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("GET");
        let timeout_secs = params
            .get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(30)
            .min(120);
        let follow_redirects = params
            .get("follow_redirects")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let max_response_chars = params
            .get("max_response_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(50000) as usize;

        // Build client
        let redirect_policy = if follow_redirects {
            reqwest::redirect::Policy::limited(10)
        } else {
            reqwest::redirect::Policy::none()
        };

        let client = Client::builder()
            .redirect(redirect_policy)
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .build()
            .map_err(|e| Error::Tool(format!("Failed to create HTTP client: {}", e)))?;

        // Build request
        let mut request = match method {
            "GET" => client.get(url),
            "POST" => client.post(url),
            "PUT" => client.put(url),
            "PATCH" => client.patch(url),
            "DELETE" => client.delete(url),
            "HEAD" => client.head(url),
            "OPTIONS" => client.request(reqwest::Method::OPTIONS, url),
            _ => return Err(Error::Validation(format!("Invalid method: {}", method))),
        };

        // User-Agent
        let user_agent = format!("blockcell/{}", env!("CARGO_PKG_VERSION"));
        request = request.header("User-Agent", user_agent);

        // Custom headers
        if let Some(headers) = params.get("headers").and_then(parse_json_like_value) {
            if let Some(headers) = headers.as_object() {
                for (key, value) in headers {
                    let val_str = match value {
                        Value::String(s) => s.clone(),
                        _ => value.to_string(),
                    };
                    request = request.header(key.as_str(), val_str);
                }
            }
        }

        // Authentication
        if let Some(auth_type) = params.get("auth_type").and_then(|v| v.as_str()) {
            match auth_type {
                "bearer" => {
                    let token = params
                        .get("auth_token")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            Error::Validation("bearer auth requires 'auth_token'".to_string())
                        })?;
                    request = request.bearer_auth(token);
                }
                "basic" => {
                    let username = params
                        .get("auth_username")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            Error::Validation("basic auth requires 'auth_username'".to_string())
                        })?;
                    let password = params
                        .get("auth_password")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    request = request.basic_auth(username, Some(password));
                }
                "api_key" => {
                    let key_name = params
                        .get("auth_key_name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            Error::Validation("api_key auth requires 'auth_key_name'".to_string())
                        })?;
                    let key_value = params
                        .get("auth_key_value")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            Error::Validation("api_key auth requires 'auth_key_value'".to_string())
                        })?;
                    request = request.header(key_name, key_value);
                }
                _ => {
                    return Err(Error::Validation(format!(
                        "Unknown auth_type: {}",
                        auth_type
                    )))
                }
            }
        }

        // Query parameters
        if let Some(query) = params.get("query_params").and_then(parse_json_like_value) {
            if let Some(query) = query.as_object() {
                let pairs: Vec<(String, String)> = query
                    .iter()
                    .map(|(k, v)| {
                        let val = match v {
                            Value::String(s) => s.clone(),
                            _ => v.to_string(),
                        };
                        (k.clone(), val)
                    })
                    .collect();
                request = request.query(&pairs);
            }
        }

        // Body
        if let Some(body) = params.get("body") {
            if let Some(parsed_body) = parse_json_like_value(body) {
                if parsed_body.is_object() || parsed_body.is_array() {
                    request = request.json(&parsed_body);
                } else if let Some(body_raw) = parsed_body.as_str() {
                    request = request.body(body_raw.to_string());
                }
            }
        } else if let Some(body_raw) = params.get("body_raw").and_then(|v| v.as_str()) {
            request = request.body(body_raw.to_string());
        } else if let Some(form) = params.get("form").and_then(parse_json_like_value) {
            if let Some(form) = form.as_object() {
                let form_data: Vec<(String, String)> = form
                    .iter()
                    .map(|(k, v)| {
                        let val = match v {
                            Value::String(s) => s.clone(),
                            _ => v.to_string(),
                        };
                        (k.clone(), val)
                    })
                    .collect();
                request = request.form(&form_data);
            }
        }

        // Send request
        let response = request.send().await.map_err(|e| {
            if e.is_timeout() {
                Error::Timeout(format!("Request timed out after {} seconds", timeout_secs))
            } else if e.is_connect() {
                Error::Tool(format!("Connection failed: {}", e))
            } else {
                Error::Tool(format!("Request failed: {}", e))
            }
        })?;

        // Collect response metadata
        let status = response.status().as_u16();
        let status_text = response
            .status()
            .canonical_reason()
            .unwrap_or("")
            .to_string();
        let final_url = response.url().to_string();

        let response_headers: Value = {
            let mut headers_map = serde_json::Map::new();
            for (key, value) in response.headers() {
                if let Ok(val_str) = value.to_str() {
                    headers_map.insert(key.as_str().to_string(), json!(val_str));
                }
            }
            Value::Object(headers_map)
        };

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        // Handle file download
        if let Some(save_path) = params.get("save_to").and_then(|v| v.as_str()) {
            let path = if save_path.starts_with("~/") {
                dirs::home_dir()
                    .map(|h| h.join(&save_path[2..]))
                    .unwrap_or_else(|| std::path::PathBuf::from(save_path))
            } else if save_path.starts_with('/') {
                std::path::PathBuf::from(save_path)
            } else {
                ctx.workspace.join(save_path)
            };

            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            let bytes = response
                .bytes()
                .await
                .map_err(|e| Error::Tool(format!("Failed to read response body: {}", e)))?;
            let size = bytes.len();
            tokio::fs::write(&path, &bytes).await?;

            return Ok(json!({
                "status": status,
                "status_text": status_text,
                "url": final_url,
                "headers": response_headers,
                "saved_to": path.display().to_string(),
                "bytes_saved": size
            }));
        }

        // Read response body
        let body_bytes = response
            .bytes()
            .await
            .map_err(|e| Error::Tool(format!("Failed to read response body: {}", e)))?;

        let body_text = String::from_utf8_lossy(&body_bytes).to_string();

        // Try to parse as JSON
        let body_json: Option<Value> =
            if content_type.contains("application/json") || content_type.contains("+json") {
                serde_json::from_str(&body_text).ok()
            } else {
                None
            };

        // Truncate if needed
        let truncated = body_text.len() > max_response_chars;
        let body_display = if truncated {
            let mut end = max_response_chars;
            while end > 0 && !body_text.is_char_boundary(end) {
                end -= 1;
            }
            body_text[..end].to_string()
        } else {
            body_text
        };

        let mut result = json!({
            "status": status,
            "status_text": status_text,
            "url": final_url,
            "content_type": content_type,
            "headers": response_headers,
            "body_length": body_bytes.len(),
            "truncated": truncated
        });

        if let Some(json_body) = body_json {
            result["body"] = json_body;
        } else {
            result["body"] = json!(body_display);
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema() {
        let tool = HttpRequestTool;
        let schema = tool.schema();
        assert_eq!(schema.name, "http_request");
    }

    #[test]
    fn test_validate() {
        let tool = HttpRequestTool;
        assert!(tool
            .validate(&json!({"url": "https://api.example.com"}))
            .is_ok());
        assert!(tool.validate(&json!({"url": "ftp://bad"})).is_err());
        assert!(tool.validate(&json!({})).is_err());
        assert!(tool
            .validate(&json!({"url": "https://api.example.com", "method": "POST"}))
            .is_ok());
        assert!(tool
            .validate(&json!({"url": "https://api.example.com", "method": "INVALID"}))
            .is_err());
    }

    #[test]
    fn test_validate_methods() {
        let tool = HttpRequestTool;
        for method in &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] {
            assert!(tool
                .validate(&json!({"url": "https://x.com", "method": method}))
                .is_ok());
        }
    }

    #[test]
    fn test_parse_string_map_json_string() {
        let parsed =
            parse_string_map(r#"{"Content-Type":"application/json","X-Test":"1"}"#).unwrap();
        assert_eq!(
            parsed.get("Content-Type").and_then(|v| v.as_str()),
            Some("application/json")
        );
        assert_eq!(parsed.get("X-Test").and_then(|v| v.as_str()), Some("1"));
    }

    #[test]
    fn test_parse_string_map_arrow_syntax() {
        let parsed = parse_string_map(r#""code"=>"w001","pageSize"=>30,"page"=>1"#).unwrap();
        assert_eq!(parsed.get("code").and_then(|v| v.as_str()), Some("w001"));
        assert_eq!(parsed.get("pageSize").and_then(|v| v.as_i64()), Some(30));
        assert_eq!(parsed.get("page").and_then(|v| v.as_i64()), Some(1));
    }

    #[test]
    fn test_parse_json_like_value_string_object() {
        let parsed = parse_json_like_value(&json!(r#"{"code":"w001","pageSize":30}"#)).unwrap();
        assert!(parsed.is_object());
        assert_eq!(parsed["code"], "w001");
        assert_eq!(parsed["pageSize"], 30);
    }

    #[test]
    fn test_parse_json_like_value_arrow_object() {
        let parsed =
            parse_json_like_value(&json!(r#""code"=>"w001","pageSize"=>30,"page"=>1"#)).unwrap();
        assert!(parsed.is_object());
        assert_eq!(parsed["code"], "w001");
        assert_eq!(parsed["pageSize"], 30);
        assert_eq!(parsed["page"], 1);
    }

    #[test]
    fn test_parse_json_like_value_query_params_string() {
        let parsed =
            parse_json_like_value(&json!(r#"page=>1,pageSize=>30,keyword=>kimi"#)).unwrap();
        assert!(parsed.is_object());
        assert_eq!(parsed["page"], 1);
        assert_eq!(parsed["pageSize"], 30);
        assert_eq!(parsed["keyword"], "kimi");
    }

    #[test]
    fn test_parse_json_like_value_form_json_string() {
        let parsed = parse_json_like_value(&json!(r#"{"code":"w001","pageSize":30}"#)).unwrap();
        assert!(parsed.is_object());
        assert_eq!(parsed["code"], "w001");
        assert_eq!(parsed["pageSize"], 30);
    }
}
