//! BrowseTool — unified browser automation tool. 浏览器驱动由注入的 [`BrowserBackend`] 负责，
//! 默认 backend 为 Chrome DevTools Protocol。

use async_trait::async_trait;
use blockcell_core::Result;
use serde_json::{json, Value};
use std::sync::Arc;

use super::backend::{BrowserBackend, BrowserRequest};
use super::cdp_backend::CdpBrowserBackend;
use super::session::BrowserEngine;
use crate::{Tool, ToolContext, ToolSchema};

pub struct BrowseTool {
    backend: Arc<dyn BrowserBackend>,
}

impl BrowseTool {
    pub fn new(backend: Arc<dyn BrowserBackend>) -> Self {
        Self { backend }
    }
}

impl Default for BrowseTool {
    fn default() -> Self {
        Self::new(Arc::new(CdpBrowserBackend))
    }
}

#[async_trait]
impl Tool for BrowseTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "browse".to_string(),
            description: "Browser automation via Chrome DevTools Protocol. Supports persistent sessions, accessibility snapshots with element refs (@e1, @e2...), click, fill, type, scroll, wait, screenshot, PDF, cookies, tabs, and more.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": [
                            "navigate", "snapshot", "click", "fill", "type_text",
                            "press_key", "scroll", "wait", "screenshot", "pdf",
                            "execute_js", "get_content", "get_url",
                            "cookies_get", "cookies_set", "cookies_clear",
                            "tab_list", "tab_new", "tab_close", "tab_switch",
                            "session_list", "session_close",
                            "set_viewport", "set_headers",
                            "back", "forward", "reload",
                            "upload_file", "dialog_handle",
                            "network_intercept", "network_continue", "network_block",
                            "list_browsers"
                        ],
                        "description": "Browser action: 'navigate'=open URL (requires url param); 'snapshot'=get accessibility tree of current page (read page structure/links/text); 'get_content'=get full page text as markdown; 'screenshot'=capture page image (requires output_path); 'click'=click element (requires ref or selector); 'fill'=fill input field (requires ref/selector + text); 'type_text'=type into focused element; 'press_key'=press keyboard key; 'scroll'=scroll page; 'wait'=wait for element or time; 'execute_js'=run JavaScript; 'get_url'=get current URL; 'tab_list'=list open tabs; 'tab_new'=open new tab; 'tab_close'=close tab; 'tab_switch'=switch tab; 'back'/'forward'/'reload'=navigation; 'cookies_get'/'cookies_set'/'cookies_clear'=cookie ops; 'session_list'/'session_close'=session management; 'upload_file'=file upload; 'dialog_handle'=handle JS dialogs; 'network_intercept'/'network_continue'/'network_block'=network control; 'pdf'=save page as PDF; 'set_viewport'=set window size; 'set_headers'=set HTTP headers; 'list_browsers'=list available browsers. ALWAYS specify action explicitly."
                    },
                    "url": {
                        "type": "string",
                        "description": "URL to navigate to (for 'navigate' action)"
                    },
                    "new_tab": {
                        "type": "boolean",
                        "description": "For 'navigate': open in a new tab and switch CDP session to it (default: false)"
                    },
                    "ref": {
                        "type": "string",
                        "description": "Element ref from snapshot (e.g. 'e1', 'e5') for click/fill/type actions"
                    },
                    "selector": {
                        "type": "string",
                        "description": "CSS selector (fallback if no ref). For click/fill/type/scroll/wait actions"
                    },
                    "text": {
                        "type": "string",
                        "description": "Text to fill/type, or JS expression for execute_js"
                    },
                    "key": {
                        "type": "string",
                        "description": "Key to press (e.g. 'Enter', 'Tab', 'Escape', 'ArrowDown')"
                    },
                    "direction": {
                        "type": "string",
                        "enum": ["up", "down", "left", "right"],
                        "description": "Scroll direction (default: 'down')"
                    },
                    "amount": {
                        "type": "integer",
                        "description": "Scroll amount in pixels (default: 400)"
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Wait timeout in ms (for 'wait' action, default: 5000)"
                    },
                    "wait_for": {
                        "type": "string",
                        "description": "CSS selector to wait for (for 'wait' action)"
                    },
                    "session": {
                        "type": "string",
                        "description": "Session name (default: 'default'). Each session is an isolated browser."
                    },
                    "headed": {
                        "type": "boolean",
                        "description": "Launch visible browser (default: false = headless)"
                    },
                    "full_page": {
                        "type": "boolean",
                        "description": "Full page screenshot (default: false)"
                    },
                    "output_path": {
                        "type": "string",
                        "description": "File path for screenshot/PDF output"
                    },
                    "compact": {
                        "type": "boolean",
                        "description": "Compact snapshot (skip empty structural nodes, default: true)"
                    },
                    "cookie_name": { "type": "string" },
                    "cookie_value": { "type": "string" },
                    "cookie_domain": { "type": "string" },
                    "width": { "type": "integer", "description": "Viewport width" },
                    "height": { "type": "integer", "description": "Viewport height" },
                    "headers": {
                        "type": "object",
                        "description": "Extra HTTP headers to set"
                    },
                    "tab_index": {
                        "type": "integer",
                        "description": "Tab index for tab_switch/tab_close"
                    },
                    "tab_id": {
                        "type": "string",
                        "description": "Target ID for tab_switch/tab_close (alternative to tab_index)"
                    },
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "File paths for upload_file action"
                    },
                    "accept": {
                        "type": "boolean",
                        "description": "Accept or dismiss dialog (for dialog_handle, default: true)"
                    },
                    "prompt_text": {
                        "type": "string",
                        "description": "Text to enter in prompt dialog"
                    },
                    "url_pattern": {
                        "type": "string",
                        "description": "URL pattern for network_intercept (glob, e.g. '*api*')"
                    },
                    "request_id": {
                        "type": "string",
                        "description": "Request ID for network_continue/network_block"
                    },
                    "response_code": {
                        "type": "integer",
                        "description": "HTTP response code for network_continue fulfill"
                    },
                    "body": {
                        "type": "string",
                        "description": "Response body for network_continue fulfill"
                    },
                    "browser": {
                        "type": "string",
                        "enum": ["chrome", "edge", "firefox"],
                        "description": "Browser engine to use (default: chrome)"
                    }
                },
                "required": []
            }),
        }
    }

    fn prompt_rule(&self, _ctx: &crate::PromptContext) -> Option<String> {
        Some(concat!(
            "- **`browse` action选择规则**: 打开网页用 `navigate`+url; 读取页面内容用 `get_content`; 查看页面结构/元素用 `snapshot`; **截图用 `screenshot`（无需指定output_path）**; 点击元素用 `click`+ref/selector; 填写表单用 `fill`; 按键用 `press_key`. **绝对禁止**调用 `browse` 时不带 `action` 参数——必须明确指定 action。\n",
            "- **`browse screenshot` 路径规则**: 截图**始终**自动保存在 workspace/media/ 下，返回结果中的 `path` 字段即为可展示的路径，直接用该路径给用户展示即可。**不要**把 `output_path` 设为桌面或其他绝对路径——那样会导致 WebUI 无法显示截图。如果用户要求把截图存到某个特定位置（如桌面），工具会自动 copy 一份过去，你无需额外操作，直接用返回的 `path` 字段展示图片。"
        ).to_string())
    }

    fn validate(&self, _params: &Value) -> Result<()> {
        Ok(())
    }

    async fn execute(&self, ctx: ToolContext, params: Value) -> Result<Value> {
        let action = params["action"].as_str().unwrap_or_else(|| {
            if params.get("url").and_then(|v| v.as_str()).is_some() {
                "navigate"
            } else {
                "snapshot"
            }
        });
        let session_name = params["session"].as_str().unwrap_or("default");
        let headed = params["headed"].as_bool().unwrap_or(false);
        let engine = params["browser"]
            .as_str()
            .and_then(|s| s.parse::<BrowserEngine>().ok())
            .unwrap_or(BrowserEngine::Chrome);

        let workspace = ctx.workspace.as_path();

        // Actions that don't need a session
        match action {
            "session_list" => {
                let sessions = self.backend.list_sessions(workspace).await?;
                Ok(json!({
                    "count": sessions.len(),
                    "sessions": sessions,
                }))
            }
            "session_close" => {
                self.backend.close_session(workspace, session_name).await?;
                Ok(json!({"status": "closed", "session": session_name}))
            }
            "list_browsers" => {
                let list: Vec<Value> = self
                    .backend
                    .available_browsers()
                    .iter()
                    .map(|(e, p)| json!({"engine": e.name(), "path": p}))
                    .collect();
                Ok(json!({"browsers": list, "count": list.len()}))
            }
            _ => {
                self.backend
                    .perform(BrowserRequest {
                        workspace,
                        session: session_name,
                        headed,
                        engine,
                        action,
                        params: &params,
                    })
                    .await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// 记录调用的假 backend，用于验证工具只经由 [`BrowserBackend`] 驱动浏览器。
    #[derive(Default)]
    struct FakeBackend(std::sync::Mutex<Vec<String>>);

    #[async_trait]
    impl BrowserBackend for FakeBackend {
        async fn list_sessions(&self, _workspace: &Path) -> Result<Vec<String>> {
            Ok(vec!["default".into()])
        }
        async fn close_session(&self, _workspace: &Path, session: &str) -> Result<()> {
            self.0.lock().unwrap().push(format!("close:{session}"));
            Ok(())
        }
        fn available_browsers(&self) -> Vec<(BrowserEngine, String)> {
            vec![(BrowserEngine::Edge, "/opt/edge".into())]
        }
        async fn perform(&self, request: BrowserRequest<'_>) -> Result<Value> {
            self.0.lock().unwrap().push(format!(
                "{}:{}:{}:{}",
                request.action,
                request.session,
                request.engine.name(),
                request.headed
            ));
            Ok(json!({"url": request.params["url"]}))
        }
    }

    fn tool_context() -> ToolContext {
        let workspace = std::env::temp_dir().join("blockcell-browse-tool-test");
        ToolContext {
            workspace: workspace.clone(),
            sandbox_writable_roots: None,
            base: workspace,
            builtin_skills_dir: None,
            active_skill_dir: None,
            session_key: "test-session".to_string(),
            channel: "cli".to_string(),
            account_id: None,
            sender_id: None,
            sender_role: None,
            chat_id: "test-chat".to_string(),
            config: blockcell_core::Config::default(),
            permissions: crate::PermissionSet::new(),
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
            event_emitter: None,
            channel_contacts_file: None,
            response_cache: None,
            runtime_handle: None,
            agent_identity: None,
            skill_mutex: None,
            agent_type_registry: None,
            evolution_workflow_store: None,
        }
    }

    #[tokio::test]
    async fn browse_tool_routes_actions_through_backend() {
        let backend = Arc::new(FakeBackend::default());
        let tool = BrowseTool::new(backend.clone());

        let navigated = tool
            .execute(
                tool_context(),
                json!({"url": "https://example.com", "session": "s1", "browser": "edge", "headed": true}),
            )
            .await
            .unwrap();
        assert_eq!(navigated["url"], "https://example.com");

        let sessions = tool
            .execute(tool_context(), json!({"action": "session_list"}))
            .await
            .unwrap();
        assert_eq!(sessions, json!({"sessions": ["default"], "count": 1}));

        let browsers = tool
            .execute(tool_context(), json!({"action": "list_browsers"}))
            .await
            .unwrap();
        assert_eq!(browsers["browsers"][0]["engine"], "edge");

        tool.execute(
            tool_context(),
            json!({"action": "session_close", "session": "s1"}),
        )
        .await
        .unwrap();

        assert_eq!(
            *backend.0.lock().unwrap(),
            vec!["navigate:s1:edge:true".to_string(), "close:s1".to_string()]
        );
    }

    #[test]
    fn test_schema() {
        let tool = BrowseTool::default();
        let schema = tool.schema();
        assert_eq!(schema.name, "browse");
        assert!(schema.description.contains("Chrome DevTools Protocol"));
    }

    #[test]
    fn test_validate() {
        let tool = BrowseTool::default();
        assert!(tool.validate(&json!({"action": "navigate"})).is_ok());
        assert!(tool.validate(&json!({})).is_ok());
    }

    #[test]
    fn test_schema_has_new_actions() {
        let tool = BrowseTool::default();
        let schema = tool.schema();
        let params = &schema.parameters;
        let actions = params["properties"]["action"]["enum"].as_array().unwrap();
        let action_strs: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();

        // Tab management
        assert!(action_strs.contains(&"tab_list"));
        assert!(action_strs.contains(&"tab_new"));
        assert!(action_strs.contains(&"tab_close"));
        assert!(action_strs.contains(&"tab_switch"));
        // File upload
        assert!(action_strs.contains(&"upload_file"));
        // Dialog
        assert!(action_strs.contains(&"dialog_handle"));
        // Network interception
        assert!(action_strs.contains(&"network_intercept"));
        assert!(action_strs.contains(&"network_continue"));
        assert!(action_strs.contains(&"network_block"));
        // Multi-browser
        assert!(action_strs.contains(&"list_browsers"));
    }

    #[test]
    fn test_schema_has_new_params() {
        let tool = BrowseTool::default();
        let schema = tool.schema();
        let props = &schema.parameters["properties"];

        assert!(props.get("tab_id").is_some());
        assert!(props.get("files").is_some());
        assert!(props.get("accept").is_some());
        assert!(props.get("prompt_text").is_some());
        assert!(props.get("url_pattern").is_some());
        assert!(props.get("request_id").is_some());
        assert!(props.get("response_code").is_some());
        assert!(props.get("body").is_some());
        assert!(props.get("browser").is_some());
    }

    #[test]
    fn test_validate_new_actions() {
        let tool = BrowseTool::default();
        assert!(tool.validate(&json!({"action": "tab_list"})).is_ok());
        assert!(tool.validate(&json!({"action": "upload_file"})).is_ok());
        assert!(tool.validate(&json!({"action": "dialog_handle"})).is_ok());
        assert!(tool
            .validate(&json!({"action": "network_intercept"}))
            .is_ok());
        assert!(tool.validate(&json!({"action": "list_browsers"})).is_ok());
    }
}
