use blockcell_core::{Error, Result};
use rhai::{Dynamic, Engine, Map, Scope};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

/// Result of executing a skill's Rhai script.
#[derive(Debug, Clone)]
pub struct SkillDispatchResult {
    /// The final output value from the Rhai script.
    pub output: Value,
    /// Tool calls that were made during execution, in order.
    pub tool_calls: Vec<ToolCallRecord>,
    /// Whether the skill completed successfully.
    pub success: bool,
    /// Error message if the skill failed.
    pub error: Option<String>,
}

/// Record of a tool call made by a Rhai script.
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub params: Value,
    pub result: Value,
    pub success: bool,
}

/// The SkillDispatcher executes SKILL.rhai scripts with tool-calling capabilities.
///
/// Architecture:
/// - Rhai scripts call `call_tool(name, params)` which executes tools inline
/// - The dispatcher uses a synchronous callback mechanism to execute tools
/// - Tool results are returned to the Rhai script as Dynamic values
pub struct SkillDispatcher;

impl SkillDispatcher {
    pub fn new() -> Self {
        Self
    }

    /// Execute a SKILL.rhai script with a synchronous tool executor.
    /// Tool calls are executed inline during script execution.
    pub fn execute_sync<F>(
        &self,
        script: &str,
        user_input: &str,
        context_vars: HashMap<String, Value>,
        tool_executor: F,
    ) -> Result<SkillDispatchResult>
    where
        F: Fn(&str, Value) -> Result<Value> + Send + Sync + 'static,
    {
        let tool_calls: Arc<Mutex<Vec<ToolCallRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let output: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let executor = Arc::new(tool_executor);

        let mut engine = Engine::new();
        engine.set_max_string_size(1_000_000);
        engine.set_max_array_size(10_000);
        engine.set_max_map_size(10_000);
        engine.set_max_call_levels(64);
        engine.set_max_expr_depths(64, 64);

        // Register call_tool(name, params) -> Dynamic
        {
            let tc = tool_calls.clone();
            let exec = executor.clone();
            engine.register_fn("call_tool", move |name: String, params: Map| -> Dynamic {
                let params_json = map_to_json(&params);
                debug!(tool = %name, "SKILL.rhai calling tool");
                
                match exec(&name, params_json.clone()) {
                    Ok(result) => {
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: name,
                            params: params_json,
                            result: result.clone(),
                            success: true,
                        });
                        json_to_dynamic(&result)
                    }
                    Err(e) => {
                        let err_val = serde_json::json!({"error": format!("{}", e)});
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: name,
                            params: params_json,
                            result: err_val.clone(),
                            success: false,
                        });
                        json_to_dynamic(&err_val)
                    }
                }
            });
        }

        // Register call_tool with string params (JSON string)
        {
            let tc = tool_calls.clone();
            let exec = executor.clone();
            engine.register_fn("call_tool_json", move |name: String, params_str: String| -> Dynamic {
                let params_json: Value = serde_json::from_str(&params_str)
                    .unwrap_or(Value::Object(serde_json::Map::new()));
                debug!(tool = %name, "SKILL.rhai calling tool (JSON)");
                
                match exec(&name, params_json.clone()) {
                    Ok(result) => {
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: name,
                            params: params_json,
                            result: result.clone(),
                            success: true,
                        });
                        json_to_dynamic(&result)
                    }
                    Err(e) => {
                        let err_val = serde_json::json!({"error": format!("{}", e)});
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: name,
                            params: params_json,
                            result: err_val.clone(),
                            success: false,
                        });
                        json_to_dynamic(&err_val)
                    }
                }
            });
        }

        // Register set_output(value) — sets the final output
        {
            let out = output.clone();
            engine.register_fn("set_output", move |val: Dynamic| {
                let json_val = dynamic_to_json(&val);
                *out.lock().unwrap() = Some(json_val);
            });
        }

        // Register set_output_json(json_string) — sets output from JSON string
        {
            let out = output.clone();
            engine.register_fn("set_output_json", move |json_str: String| {
                let val: Value = serde_json::from_str(&json_str)
                    .unwrap_or(Value::String(json_str));
                *out.lock().unwrap() = Some(val);
            });
        }

        // Register log(message) — debug logging from Rhai
        engine.register_fn("log", |msg: String| {
            info!(source = "SKILL.rhai", "{}", msg);
        });

        // Register log_warn(message) — warning from Rhai
        engine.register_fn("log_warn", |msg: String| {
            warn!(source = "SKILL.rhai", "{}", msg);
        });

        // Register is_error(result) — check if a tool result is an error
        engine.register_fn("is_error", |val: Map| -> bool {
            val.contains_key("error")
        });

        // Register get_field(map, key) — safely get a field from a map
        engine.register_fn("get_field", |map: Map, key: String| -> Dynamic {
            map.get(key.as_str()).cloned().unwrap_or(Dynamic::UNIT)
        });

        // Register to_json(value) — convert a Dynamic to JSON string
        engine.register_fn("to_json", |val: Dynamic| -> String {
            let json = dynamic_to_json(&val);
            serde_json::to_string(&json).unwrap_or_default()
        });

        // Register from_json(string) — parse a JSON string to Dynamic
        engine.register_fn("from_json", |s: String| -> Dynamic {
            match serde_json::from_str::<Value>(&s) {
                Ok(v) => json_to_dynamic(&v),
                Err(_) => Dynamic::UNIT,
            }
        });

        // Register sleep_ms(ms) — sleep for milliseconds (for retry delays)
        engine.register_fn("sleep_ms", |ms: i64| {
            if ms > 0 && ms <= 10_000 {
                std::thread::sleep(std::time::Duration::from_millis(ms as u64));
            }
        });

        // Register timestamp() — current Unix timestamp
        engine.register_fn("timestamp", || -> i64 {
            chrono::Utc::now().timestamp()
        });

        // Register shorthand tool functions so SKILL.rhai can call exec(cmd) instead of
        // call_tool("exec", #{command: cmd}).  These are thin wrappers around call_tool.

        // exec(command) -> Dynamic
        {
            let tc = tool_calls.clone();
            let exec = executor.clone();
            engine.register_fn("exec", move |command: String| -> Dynamic {
                let params = serde_json::json!({"command": command});
                match exec("exec", params.clone()) {
                    Ok(result) => {
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: "exec".to_string(),
                            params,
                            result: result.clone(),
                            success: true,
                        });
                        json_to_dynamic(&result)
                    }
                    Err(e) => {
                        let err_val = serde_json::json!({"error": format!("{}", e)});
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: "exec".to_string(),
                            params,
                            result: err_val.clone(),
                            success: false,
                        });
                        json_to_dynamic(&err_val)
                    }
                }
            });
        }

        // web_search(query) -> Dynamic
        {
            let tc = tool_calls.clone();
            let exec = executor.clone();
            engine.register_fn("web_search", move |query: String| -> Dynamic {
                let params = serde_json::json!({"query": query});
                match exec("web_search", params.clone()) {
                    Ok(result) => {
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: "web_search".to_string(),
                            params,
                            result: result.clone(),
                            success: true,
                        });
                        json_to_dynamic(&result)
                    }
                    Err(e) => {
                        let err_val = serde_json::json!({"error": format!("{}", e)});
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: "web_search".to_string(),
                            params,
                            result: err_val.clone(),
                            success: false,
                        });
                        json_to_dynamic(&err_val)
                    }
                }
            });
        }

        // web_fetch(url) -> Dynamic
        {
            let tc = tool_calls.clone();
            let exec = executor.clone();
            engine.register_fn("web_fetch", move |url: String| -> Dynamic {
                let params = serde_json::json!({"url": url});
                match exec("web_fetch", params.clone()) {
                    Ok(result) => {
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: "web_fetch".to_string(),
                            params,
                            result: result.clone(),
                            success: true,
                        });
                        json_to_dynamic(&result)
                    }
                    Err(e) => {
                        let err_val = serde_json::json!({"error": format!("{}", e)});
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: "web_fetch".to_string(),
                            params,
                            result: err_val.clone(),
                            success: false,
                        });
                        json_to_dynamic(&err_val)
                    }
                }
            });
        }

        // read_file(path) -> Dynamic
        {
            let tc = tool_calls.clone();
            let exec = executor.clone();
            engine.register_fn("read_file", move |path: String| -> Dynamic {
                let params = serde_json::json!({"path": path});
                match exec("read_file", params.clone()) {
                    Ok(result) => {
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: "read_file".to_string(),
                            params,
                            result: result.clone(),
                            success: true,
                        });
                        json_to_dynamic(&result)
                    }
                    Err(e) => {
                        let err_val = serde_json::json!({"error": format!("{}", e)});
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: "read_file".to_string(),
                            params,
                            result: err_val.clone(),
                            success: false,
                        });
                        json_to_dynamic(&err_val)
                    }
                }
            });
        }

        // write_file(path, content) -> Dynamic
        {
            let tc = tool_calls.clone();
            let exec = executor.clone();
            engine.register_fn("write_file", move |path: String, content: String| -> Dynamic {
                let params = serde_json::json!({"path": path, "content": content});
                match exec("write_file", params.clone()) {
                    Ok(result) => {
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: "write_file".to_string(),
                            params,
                            result: result.clone(),
                            success: true,
                        });
                        json_to_dynamic(&result)
                    }
                    Err(e) => {
                        let err_val = serde_json::json!({"error": format!("{}", e)});
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: "write_file".to_string(),
                            params,
                            result: err_val.clone(),
                            success: false,
                        });
                        json_to_dynamic(&err_val)
                    }
                }
            });
        }

        // http_request(url) -> Dynamic  (simple GET)
        {
            let tc = tool_calls.clone();
            let exec = executor.clone();
            engine.register_fn("http_request", move |url: String| -> Dynamic {
                let params = serde_json::json!({"url": url, "method": "GET"});
                match exec("http_request", params.clone()) {
                    Ok(result) => {
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: "http_request".to_string(),
                            params,
                            result: result.clone(),
                            success: true,
                        });
                        json_to_dynamic(&result)
                    }
                    Err(e) => {
                        let err_val = serde_json::json!({"error": format!("{}", e)});
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: "http_request".to_string(),
                            params,
                            result: err_val.clone(),
                            success: false,
                        });
                        json_to_dynamic(&err_val)
                    }
                }
            });
        }

        // message(content) -> Dynamic  (send outbound message)
        {
            let tc = tool_calls.clone();
            let exec = executor.clone();
            engine.register_fn("message", move |content: String| -> Dynamic {
                let params = serde_json::json!({"content": content});
                match exec("message", params.clone()) {
                    Ok(result) => {
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: "message".to_string(),
                            params,
                            result: result.clone(),
                            success: true,
                        });
                        json_to_dynamic(&result)
                    }
                    Err(e) => {
                        let err_val = serde_json::json!({"error": format!("{}", e)});
                        tc.lock().unwrap().push(ToolCallRecord {
                            tool_name: "message".to_string(),
                            params,
                            result: err_val.clone(),
                            success: false,
                        });
                        json_to_dynamic(&err_val)
                    }
                }
            });
        }

        // Compile
        let ast = engine.compile(script).map_err(|e| {
            Error::Skill(format!("SKILL.rhai compilation error: {}", e))
        })?;

        // Set up scope
        let mut scope = Scope::new();
        scope.push("user_input", user_input.to_string());
        for (key, val) in &context_vars {
            scope.push(key.as_str(), json_to_dynamic(val));
        }

        // Execute
        let result = engine.eval_ast_with_scope::<Dynamic>(&mut scope, &ast);

        let tc = tool_calls.lock().unwrap().clone();
        let out = output.lock().unwrap().clone();

        match result {
            Ok(value) => {
                let final_output = out.unwrap_or_else(|| dynamic_to_json(&value));
                Ok(SkillDispatchResult {
                    output: final_output,
                    tool_calls: tc,
                    success: true,
                    error: None,
                })
            }
            Err(e) => {
                let err_str = format!("{}", e);
                warn!(error = %err_str, "SKILL.rhai execution failed");
                Ok(SkillDispatchResult {
                    output: serde_json::json!({"error": err_str}),
                    tool_calls: tc,
                    success: false,
                    error: Some(err_str),
                })
            }
        }
    }

}

impl Default for SkillDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a serde_json::Value to a Rhai Dynamic.
pub fn json_to_dynamic(val: &Value) -> Dynamic {
    match val {
        Value::Null => Dynamic::UNIT,
        Value::Bool(b) => Dynamic::from(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Dynamic::from(i)
            } else if let Some(f) = n.as_f64() {
                Dynamic::from(f)
            } else {
                Dynamic::from(n.to_string())
            }
        }
        Value::String(s) => Dynamic::from(s.clone()),
        Value::Array(arr) => {
            let rhai_arr: Vec<Dynamic> = arr.iter().map(json_to_dynamic).collect();
            Dynamic::from(rhai_arr)
        }
        Value::Object(obj) => {
            let mut map = Map::new();
            for (k, v) in obj {
                map.insert(k.clone().into(), json_to_dynamic(v));
            }
            Dynamic::from(map)
        }
    }
}

/// Convert a Rhai Dynamic to serde_json::Value.
pub fn dynamic_to_json(val: &Dynamic) -> Value {
    if val.is_unit() {
        Value::Null
    } else if val.is::<bool>() {
        Value::Bool(val.as_bool().unwrap_or(false))
    } else if val.is::<i64>() {
        Value::Number(serde_json::Number::from(val.as_int().unwrap_or(0)))
    } else if val.is::<f64>() {
        if let Ok(f) = val.as_float() {
            serde_json::Number::from_f64(f)
                .map(Value::Number)
                .unwrap_or(Value::Null)
        } else {
            Value::Null
        }
    } else if val.is::<String>() {
        Value::String(val.clone().into_string().unwrap_or_default())
    } else if val.is::<rhai::Array>() {
        let arr = val.clone().into_array().unwrap_or_default();
        Value::Array(arr.iter().map(dynamic_to_json).collect())
    } else if val.is::<Map>() {
        match val.clone().try_cast::<Map>() {
            Some(m) => {
                let mut obj = serde_json::Map::new();
                for (k, v) in m {
                    obj.insert(k.to_string(), dynamic_to_json(&v));
                }
                Value::Object(obj)
            }
            None => Value::String(format!("{}", val)),
        }
    } else {
        Value::String(format!("{}", val))
    }
}

/// Convert a Rhai Map to serde_json::Value.
fn map_to_json(map: &Map) -> Value {
    let mut obj = serde_json::Map::new();
    for (k, v) in map {
        obj.insert(k.to_string(), dynamic_to_json(v));
    }
    Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_skill_script() {
        let dispatcher = SkillDispatcher::new();
        let result = dispatcher.execute_sync(
            r#"
            let msg = "Hello, " + user_input;
            set_output(msg);
            msg
            "#,
            "world",
            HashMap::new(),
            |_name, _params| Ok(serde_json::json!({"ok": true})),
        ).unwrap();

        assert!(result.success);
        assert_eq!(result.output, Value::String("Hello, world".to_string()));
    }

    #[test]
    fn test_tool_call_from_rhai() {
        let dispatcher = SkillDispatcher::new();
        let result = dispatcher.execute_sync(
            r#"
            let params = #{
                path: "/tmp/test.txt"
            };
            let result = call_tool("read_file", params);
            set_output(result);
            "#,
            "",
            HashMap::new(),
            |name, _params| {
                assert_eq!(name, "read_file");
                Ok(serde_json::json!({"content": "file contents here"}))
            },
        ).unwrap();

        assert!(result.success);
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].tool_name, "read_file");
        assert!(result.tool_calls[0].success);
    }

    #[test]
    fn test_tool_error_handling() {
        let dispatcher = SkillDispatcher::new();
        let result = dispatcher.execute_sync(
            r#"
            let result = call_tool("bad_tool", #{});
            if is_error(result) {
                set_output("Tool failed, using fallback");
                log_warn("Tool call failed, degrading");
            }
            "#,
            "",
            HashMap::new(),
            |_name, _params| {
                Err(Error::Tool("not found".to_string()))
            },
        ).unwrap();

        assert!(result.success);
        assert_eq!(result.output, Value::String("Tool failed, using fallback".to_string()));
        assert_eq!(result.tool_calls.len(), 1);
        assert!(!result.tool_calls[0].success);
    }

    #[test]
    fn test_context_variables() {
        let dispatcher = SkillDispatcher::new();
        let mut ctx = HashMap::new();
        ctx.insert("device".to_string(), serde_json::json!("front_camera"));
        ctx.insert("resolution".to_string(), serde_json::json!("1080p"));

        let result = dispatcher.execute_sync(
            r#"
            let msg = "Using " + device + " at " + resolution;
            set_output(msg);
            "#,
            "",
            ctx,
            |_name, _params| Ok(serde_json::json!({})),
        ).unwrap();

        assert!(result.success);
        assert_eq!(result.output, Value::String("Using front_camera at 1080p".to_string()));
    }

    #[test]
    fn test_multi_step_orchestration() {
        let dispatcher = SkillDispatcher::new();
        let result = dispatcher.execute_sync(
            r#"
            // Step 1: List devices
            let devices = call_tool("camera_list", #{});
            log("Found devices");

            // Step 2: Capture
            let capture = call_tool("camera_capture", #{
                device: "default",
                output_path: "/tmp/photo.jpg"
            });

            // Step 3: Check result
            if is_error(capture) {
                set_output(#{
                    success: false,
                    error: "Capture failed"
                });
            } else {
                set_output(#{
                    success: true,
                    path: "/tmp/photo.jpg",
                    device_count: 1
                });
            }
            "#,
            "帮我拍张照",
            HashMap::new(),
            |name, _params| {
                match name {
                    "camera_list" => Ok(serde_json::json!({"devices": ["FaceTime HD Camera"]})),
                    "camera_capture" => Ok(serde_json::json!({"path": "/tmp/photo.jpg", "success": true})),
                    _ => Err(Error::Tool(format!("Unknown tool: {}", name))),
                }
            },
        ).unwrap();

        assert!(result.success);
        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0].tool_name, "camera_list");
        assert_eq!(result.tool_calls[1].tool_name, "camera_capture");
    }
}
