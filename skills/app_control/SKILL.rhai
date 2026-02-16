// 通用应用控制技能编排脚本
// 根据用户意图执行不同的应用控制流程

let user_input = if context.contains("user_input") { context["user_input"] } else { "" };
let target_app = if context.contains("app") { context["app"] } else { "Windsurf" };

// 判断用户意图
let intent = "screenshot"; // 默认截图

if user_input.contains("列出") || user_input.contains("什么应用") || user_input.contains("list") {
    intent = "list_apps";
} else if user_input.contains("打开文件") || user_input.contains("open file") {
    intent = "open_file";
} else if user_input.contains("截图") || user_input.contains("看看") || user_input.contains("screenshot") {
    intent = "screenshot";
} else if user_input.contains("菜单") || user_input.contains("menu") {
    intent = "menu";
} else if user_input.contains("UI") || user_input.contains("界面") || user_input.contains("read") {
    intent = "read_ui";
}

if intent == "list_apps" {
    let result = call_tool("app_control", #{
        "action": "list_apps"
    });
    if is_error(result) {
        log_warn("列出应用失败: " + to_json(result));
        set_output("❌ 无法列出运行中的应用");
    } else {
        set_output_json(result);
    }
} else if intent == "screenshot" {
    // 截图流程
    let result = call_tool("app_control", #{
        "action": "screenshot",
        "app": target_app
    });
    if is_error(result) {
        log_warn("截图失败，尝试降级: " + to_json(result));
        // 降级: 全屏截图
        let fallback = call_tool("exec", #{
            "command": "screencapture -x /tmp/app_screenshot.png"
        });
        set_output("⚠️ 窗口截图失败，已降级为全屏截图: /tmp/app_screenshot.png");
    } else {
        // 同时读取 UI 树
        let ui = call_tool("app_control", #{
            "action": "read_ui",
            "app": target_app,
            "depth": 2
        });
        let output = #{
            "screenshot": result,
            "ui_tree": ui
        };
        set_output_json(output);
    }
} else if intent == "read_ui" {
    let result = call_tool("app_control", #{
        "action": "read_ui",
        "app": target_app,
        "depth": 3
    });
    if is_error(result) {
        // 降级: 减少深度
        log_warn("UI 读取失败，降低深度重试");
        let retry = call_tool("app_control", #{
            "action": "read_ui",
            "app": target_app,
            "depth": 1
        });
        set_output_json(retry);
    } else {
        set_output_json(result);
    }
} else {
    // 默认: 截图
    let result = call_tool("app_control", #{
        "action": "screenshot",
        "app": target_app
    });
    set_output_json(result);
}
