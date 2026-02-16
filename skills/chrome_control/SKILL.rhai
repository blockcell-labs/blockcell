// 浏览器自动化编排脚本 (Chrome Control Orchestration)
//
// 预注入变量:
//   user_input   — 用户原始输入文本
//   url          — 目标 URL (可选)
//   search_term  — 搜索关键词 (可选)
//   search_engine — 搜索引擎 (可选, 默认 "baidu")

// === Step 1: 解析参数 ===
let engine = if search_engine != () { search_engine } else { "baidu" };

// 搜索引擎配置
let engine_urls = #{
    baidu: "https://www.baidu.com",
    google: "https://www.google.com",
    bing: "https://www.bing.com"
};

let engine_selectors = #{
    baidu: "#kw",
    google: "textarea[name=\"q\"]",
    bing: "#sb_form_q"
};

log("🌐 [浏览器] 开始浏览器自动化");

// === Step 2: 确定目标 URL ===
let target_url = if url != () && url != "" {
    url
} else {
    let u = engine_urls[engine];
    if u != () { u } else { "https://www.baidu.com" }
};

log("🌐 [浏览器] 目标: " + target_url);

// === Step 3: 打开浏览器 ===
let open_result = call_tool("chrome_control", #{
    action: "open",
    url: target_url
});

if is_error(open_result) {
    let err = get_field(open_result, "error");
    log_warn("🌐 [浏览器] 打开失败: " + err);
    set_output(#{
        success: false,
        error: "无法打开 Chrome 浏览器: " + err,
        suggestion: "请确保已安装 Google Chrome 并授权辅助功能权限"
    });
    return;
}

// 等待页面加载
call_tool("chrome_control", #{
    action: "wait",
    amount: 1500
});

log("🌐 [浏览器] 页面已打开");

// === Step 4: 如果有搜索关键词，执行搜索 ===
if search_term != () && search_term != "" {
    log("🌐 [浏览器] 搜索: " + search_term);

    // 获取搜索框选择器
    let selector = engine_selectors[engine];
    if selector == () {
        selector = "#kw";  // 默认百度
    }

    // 点击搜索框
    let click_result = call_tool("chrome_control", #{
        action: "click",
        selector: selector
    });

    if is_error(click_result) {
        log_warn("🌐 [浏览器] 搜索框点击失败，尝试直接输入");
    }

    // 短暂等待
    call_tool("chrome_control", #{
        action: "wait",
        amount: 300
    });

    // 输入搜索关键词
    let type_result = call_tool("chrome_control", #{
        action: "type",
        text: search_term
    });

    if is_error(type_result) {
        log_warn("🌐 [浏览器] 输入失败: " + get_field(type_result, "error"));
        set_output(#{
            success: false,
            error: "无法在搜索框中输入文字",
            suggestion: "请检查辅助功能权限"
        });
        return;
    }

    // 按回车搜索
    call_tool("chrome_control", #{
        action: "press_key",
        text: "return"
    });

    // 等待搜索结果
    call_tool("chrome_control", #{
        action: "wait",
        amount: 2000
    });

    log("🌐 [浏览器] 搜索完成，读取结果");

    // 读取搜索结果
    let read_result = call_tool("chrome_control", #{
        action: "read"
    });

    let page_content = if !is_error(read_result) {
        get_field(read_result, "content")
    } else {
        #{}
    };

    set_output(#{
        success: true,
        action: "search",
        engine: engine,
        url: target_url,
        search_term: search_term,
        page_content: page_content,
        steps: [
            "打开 " + target_url,
            "输入搜索词: " + search_term,
            "执行搜索",
            "获取搜索结果"
        ]
    });
} else {
    // 仅打开页面，读取内容
    let read_result = call_tool("chrome_control", #{
        action: "read"
    });

    let page_content = if !is_error(read_result) {
        get_field(read_result, "content")
    } else {
        #{}
    };

    set_output(#{
        success: true,
        action: "open",
        url: target_url,
        page_content: page_content,
        steps: [
            "打开 " + target_url,
            "页面加载完成"
        ]
    });
}
