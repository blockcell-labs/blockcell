# 通用应用控制技能 (app_control)

## 触发短语
- 控制应用/软件、截图应用、看看屏幕/IDE、操作IDE、读取界面/UI、点击菜单
- windsurf、vscode、control app、app screenshot、what's on screen

## 核心能力
通过 AppleScript + System Events 控制任何 macOS 应用程序。

## 常用应用名称映射
| 用户说的 | app 参数 |
|---------|---------|
| Windsurf / windsurf | Windsurf |
| VS Code / vscode | Code |
| 终端 / Terminal | Terminal |
| Finder / 访达 | Finder |
| Safari | Safari |
| Chrome | Google Chrome |
| 微信 | WeChat |
| 飞书 | Lark |

## 工具调用顺序

### 场景 1: 查看某个应用正在做什么
1. `app_control` action=screenshot, app=目标应用 → 截图
2. 将截图路径作为 media 发送给多模态模型分析
3. 可选: `app_control` action=read_ui, app=目标应用, depth=3 → 读取 UI 树补充信息

### 场景 2: 在应用中执行操作
1. `app_control` action=activate, app=目标应用 → 激活应用
2. `app_control` action=press_key/type/click_menu → 执行操作
3. `app_control` action=screenshot → 确认结果

### 场景 3: 了解当前环境
1. `app_control` action=list_apps → 列出所有运行中的应用
2. `app_control` action=get_frontmost → 获取当前最前面的应用

### 场景 4: 操作 IDE (Windsurf/VS Code)
常用快捷键:
- `cmd+p` — 快速打开文件
- `cmd+shift+p` — 命令面板
- `cmd+s` — 保存
- `cmd+shift+f` — 全局搜索
- `cmd+b` — 切换侧边栏
- `cmd+j` — 切换终端面板
- `cmd+,` — 打开设置

## 输出格式
```
📱 应用: {app_name}
🎯 操作: {action_description}
✅ 结果: {result_summary}
```

## 降级策略
1. 如果 read_ui 超时或返回空 → 降低 depth 重试 (depth=2 或 1)
2. 如果 screenshot 失败 → 尝试 screencapture 全屏截图作为降级
3. 如果进程名解析失败 → 用 list_apps 查找正确的进程名
4. 如果 click_menu 失败 → 尝试用 press_key 快捷键替代

## 示例

### 示例 1: 看看 Windsurf 在做什么
用户: "看看 Windsurf 在干什么"
```
call_tool("app_control", {"action": "screenshot", "app": "Windsurf"})
call_tool("app_control", {"action": "read_ui", "app": "Windsurf", "depth": 2})
```
→ 截图 + UI 树分析，告诉用户当前打开的文件、编辑器状态等

### 示例 2: 在 Windsurf 中打开文件
用户: "在 Windsurf 里打开 main.rs"
```
call_tool("app_control", {"action": "activate", "app": "Windsurf"})
call_tool("app_control", {"action": "press_key", "app": "Windsurf", "text": "cmd+p"})
call_tool("app_control", {"action": "type", "app": "Windsurf", "text": "main.rs"})
call_tool("app_control", {"action": "press_key", "app": "Windsurf", "text": "return"})
```

### 示例 3: 列出所有运行的应用
用户: "现在电脑上开了什么应用"
```
call_tool("app_control", {"action": "list_apps"})
```
