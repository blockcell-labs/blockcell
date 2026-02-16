// 拍照技能编排脚本 (Camera Capture Orchestration)
//
// 可用函数:
//   call_tool(name, params)  — 调用工具，返回结果 Map
//   is_error(result)         — 检查结果是否为错误
//   get_field(map, key)      — 安全获取 Map 字段
//   set_output(value)        — 设置最终输出
//   log(msg)                 — 记录日志
//   log_warn(msg)            — 记录警告
//   to_json(value)           — 转为 JSON 字符串
//   timestamp()              — 当前时间戳
//
// 预注入变量:
//   user_input  — 用户原始输入文本
//   device      — 摄像头索引 (可选, 默认 "0")
//   format      — 图片格式 (可选, 默认 "jpg")
//   output_path — 输出路径 (可选, 自动生成)

// === Step 1: 确定参数 ===
let device_idx = if device != () { parse_int(device) } else { 0 };
let img_format = if format != () { format } else { "jpg" };

log("📷 [拍照] 开始拍照流程");
log("📷 [拍照] 设备: " + device_idx + ", 格式: " + img_format);

// === Step 2: 获取摄像头信息 ===
let info = call_tool("camera_capture", #{
    action: "info"
});

if is_error(info) {
    log_warn("📷 [拍照] 无法获取摄像头信息，尝试直接拍照");
}

// === Step 3: 执行拍照 ===
let capture_params = #{
    action: "capture",
    device_index: device_idx,
    format: img_format
};

// 如果用户指定了输出路径，加入参数
if output_path != () && output_path != "" {
    capture_params.output_path = output_path;
}

log("📷 [拍照] 正在拍摄...");
let result = call_tool("camera_capture", capture_params);

// === Step 4: 处理结果 ===
if is_error(result) {
    let error_msg = get_field(result, "error");
    log_warn("📷 [拍照] 拍照失败: " + error_msg);
    
    // 降级策略: 如果摄像头拍照失败，尝试截屏
    log("📷 [拍照] 尝试降级为截屏...");
    let fallback_params = #{
        action: "capture",
        device_index: 0
    };
    let fallback = call_tool("camera_capture", fallback_params);
    
    if is_error(fallback) {
        set_output(#{
            success: false,
            error: "拍照失败，所有方式均不可用",
            suggestion: "请安装 ffmpeg: brew install ffmpeg",
            original_error: error_msg
        });
    } else {
        let path = get_field(fallback, "path");
        let method = get_field(fallback, "method");
        let size = get_field(fallback, "file_size_bytes");
        let note = get_field(fallback, "note");
        
        set_output(#{
            success: true,
            path: path,
            method: method,
            file_size_bytes: size,
            degraded: true,
            note: if note != () { note } else { "使用了降级方式拍摄" }
        });
    }
} else {
    let path = get_field(result, "path");
    let method = get_field(result, "method");
    let size = get_field(result, "file_size_bytes");
    let resolution = get_field(result, "resolution");
    
    log("📷 [拍照] 拍照成功: " + path);
    
    set_output(#{
        success: true,
        path: path,
        method: method,
        file_size_bytes: size,
        resolution: if resolution != () { resolution } else { "unknown" },
        degraded: false
    });
}
