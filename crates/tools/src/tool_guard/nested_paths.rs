use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use blockcell_core::path_policy::{
    expand_tilde, resolve_for_policy, PathOp, PathPolicy, PolicyAction,
};
use blockcell_core::{Error, Paths, Result};
use serde_json::Value;

use crate::ToolContext;

/// Resource admission supplied by the host for each nested tool call.
/// A callback must reject operations requiring a new interactive confirmation.
pub type NestedToolAdmission = Arc<dyn Fn(&str, &Value) -> Result<()> + Send + Sync>;

tokio::task_local! {
    static NESTED_TOOL_ADMISSION: NestedToolAdmission;
}

/// Scope admission to one invocation rather than a process-wide default.
pub async fn with_nested_tool_admission<F: Future>(
    admission: NestedToolAdmission,
    future: F,
) -> F::Output {
    NESTED_TOOL_ADMISSION.scope(admission, future).await
}

/// Capture before crossing spawn_blocking: Tokio task locals do not propagate there.
pub fn current_nested_tool_admission() -> Option<NestedToolAdmission> {
    NESTED_TOOL_ADMISSION.try_with(Arc::clone).ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPathAccess {
    pub path: String,
    pub op: PathOp,
}

fn push_path(accesses: &mut Vec<ToolPathAccess>, args: &Value, field: &str, op: PathOp) {
    if let Some(path) = args.get(field).and_then(Value::as_str) {
        accesses.push(ToolPathAccess {
            path: path.into(),
            op,
        });
    }
}

fn push_paths(accesses: &mut Vec<ToolPathAccess>, args: &Value, field: &str, op: PathOp) {
    if let Some(paths) = args.get(field).and_then(Value::as_array) {
        for path in paths.iter().filter_map(Value::as_str) {
            accesses.push(ToolPathAccess {
                path: path.into(),
                op,
            });
        }
    }
}

/// File operands shared by direct runtime calls and Rhai calls. Shell commands
/// require the host's parser and are deliberately not handled by this table.
/// None means the tool's file effects are not described, not that it has none.
pub fn extract_file_tool_paths(tool_name: &str, args: &Value) -> Option<Vec<ToolPathAccess>> {
    let mut accesses = Vec::new();
    let action = args.get("action").and_then(Value::as_str).unwrap_or("");
    match tool_name {
        "read_file" => {
            push_path(&mut accesses, args, "path", PathOp::Read);
            push_paths(&mut accesses, args, "paths", PathOp::Read);
        }
        "write_file" => {
            push_path(&mut accesses, args, "path", PathOp::Write);
            if let Some(files) = args.get("files").and_then(Value::as_array) {
                for file in files {
                    push_path(&mut accesses, file, "path", PathOp::Write);
                }
            }
        }
        "edit_file" => push_path(&mut accesses, args, "path", PathOp::Write),
        "list_dir" => push_path(&mut accesses, args, "path", PathOp::List),
        "file_ops" => {
            let source_op = match action {
                "delete" | "rename" | "move" => PathOp::Write,
                _ => PathOp::Read,
            };
            push_path(&mut accesses, args, "path", source_op);
            push_paths(&mut accesses, args, "paths", PathOp::Read);
            push_path(&mut accesses, args, "destination", PathOp::Write);
            if action == "decompress" && args.get("destination").and_then(Value::as_str).is_none() {
                if let Some(source) = args.get("path").and_then(Value::as_str) {
                    let source = if source.starts_with("~/") {
                        expand_tilde(source)
                    } else {
                        PathBuf::from(source)
                    };
                    let parent = source
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .unwrap_or_else(|| Path::new("."));
                    accesses.push(ToolPathAccess {
                        path: parent.to_string_lossy().into_owned(),
                        op: PathOp::Write,
                    });
                }
            }
        }
        "data_process" => {
            let path_op = if action == "write_csv" {
                PathOp::Write
            } else {
                PathOp::Read
            };
            push_path(&mut accesses, args, "path", path_op);
            push_path(&mut accesses, args, "output_path", PathOp::Write);
        }
        "audio_transcribe" | "ocr" | "encrypt" => {
            push_path(&mut accesses, args, "path", PathOp::Read);
            push_path(&mut accesses, args, "output_path", PathOp::Write);
        }
        "chart_generate" | "office_write" | "camera" | "tts" | "knowledge_graph" => {
            push_path(&mut accesses, args, "output_path", PathOp::Write);
        }
        "video_process" => {
            push_path(&mut accesses, args, "input", PathOp::Read);
            push_paths(&mut accesses, args, "inputs", PathOp::Read);
            push_path(&mut accesses, args, "subtitle_file", PathOp::Read);
            push_path(&mut accesses, args, "watermark_image", PathOp::Read);
            push_path(&mut accesses, args, "output_path", PathOp::Write);
        }
        "image_understand" => {
            push_path(&mut accesses, args, "path", PathOp::Read);
            push_paths(&mut accesses, args, "paths", PathOp::Read);
        }
        "termux_api" => {
            let file_op = match action {
                "share" | "media_scan" | "wallpaper" => PathOp::Read,
                _ => PathOp::Write,
            };
            push_path(&mut accesses, args, "file_path", file_op);
            push_path(&mut accesses, args, "output_path", PathOp::Write);
        }
        "email" => {
            push_paths(&mut accesses, args, "attachments", PathOp::Read);
            push_path(&mut accesses, args, "save_attachments_to", PathOp::Write);
        }
        "health_api" => {
            push_path(&mut accesses, args, "path", PathOp::Read);
            push_path(&mut accesses, args, "output_path", PathOp::Write);
        }
        "http_request" => push_path(&mut accesses, args, "save_to", PathOp::Write),
        "app_control" => push_path(&mut accesses, args, "screenshot_path", PathOp::Write),
        "message" => push_paths(&mut accesses, args, "media", PathOp::Read),
        "browse" => {
            push_path(&mut accesses, args, "output_path", PathOp::Write);
            push_path(&mut accesses, args, "file_path", PathOp::Read);
            push_paths(&mut accesses, args, "files", PathOp::Read);
        }
        _ => return None,
    }
    Some(accesses)
}

/// Standalone tools have no session approval cache. Known file tools can use
/// workspace resources and explicit policy grants; other tools need a host.
pub(super) fn standalone_path_admission(ctx: &ToolContext) -> NestedToolAdmission {
    let config = &ctx.config.security.path_access;
    let policy = if config.enabled {
        let file = if config.policy_file.trim().is_empty() {
            Paths::with_base_and_workspace(ctx.base.clone(), ctx.workspace.clone())
                .path_access_file()
        } else {
            expand_tilde(config.policy_file.trim())
        };
        PathPolicy::load(&file)
    } else {
        PathPolicy::safe_default()
    };
    let workspace = resolve_for_policy(&ctx.workspace);
    let skill_dir = ctx
        .active_skill_dir
        .as_ref()
        .map(|dir| resolve_for_policy(dir));
    Arc::new(move |tool_name, args| {
        let accesses = extract_file_tool_paths(tool_name, args).ok_or_else(|| {
            Error::PermissionDenied(format!(
                "Nested tool '{tool_name}' requires host path admission; file effects are not described"
            ))
        })?;
        for access in accesses {
            let path = resolve_for_policy(&crate::expand_path(&access.path, &workspace));
            let action = policy.evaluate(&path, access.op);
            if action == PolicyAction::Deny {
                return Err(Error::PermissionDenied(format!(
                    "path access denied by policy: {}",
                    path.display()
                )));
            }
            if path.starts_with(&workspace)
                || skill_dir.as_ref().is_some_and(|dir| {
                    matches!(tool_name, "read_file" | "list_dir")
                        && matches!(access.op, PathOp::Read | PathOp::List)
                        && path.starts_with(dir)
                })
                || action == PolicyAction::Allow
            {
                continue;
            }
            return Err(Error::PermissionDenied(format!(
                "path requires user confirmation, unavailable inside skill scripts: {}",
                path.display()
            )));
        }
        Ok(())
    })
}
