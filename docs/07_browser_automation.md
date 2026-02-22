# 第07篇：浏览器自动化 —— 让 AI 帮你操控网页

> 系列文章：《blockcell 开源项目深度解析》第 7/14 篇

---

## 为什么需要浏览器自动化

很多信息只能通过浏览器获取：
- 需要登录的网站
- 动态加载的内容（JavaScript 渲染）
- 需要点击操作才能看到的数据

普通的 `web_fetch` 工具只能抓取静态 HTML，对这些场景无能为力。

blockcell 的 `browse` 工具通过 **CDP（Chrome DevTools Protocol）** 协议，直接控制真实的 Chrome/Edge/Firefox 浏览器，解决了这个问题。

---

## 两种浏览器工具

blockcell 有两个浏览器相关的工具，定位不同：

| 工具 | 协议 | 模式 | 适用场景 |
|------|------|------|---------|
| `browse` | CDP WebSocket | 无头/有头 | 自动化、数据抓取 |
| `chrome_control` | AppleScript | 有头（可见） | 可视化操作、演示 |

本篇主要介绍 `browse` 工具，它是功能更强大的那个。

---

## CDP 是什么

CDP（Chrome DevTools Protocol）是 Chrome 浏览器暴露的一个调试接口。

你在 Chrome 里按 F12 打开的开发者工具，底层就是通过 CDP 工作的。

blockcell 通过 WebSocket 连接到 Chrome 的 CDP 接口，可以：
- 控制页面导航
- 读取 DOM 结构
- 模拟鼠标点击和键盘输入
- 截图
- 执行 JavaScript
- 管理 Cookie
- 拦截网络请求

---

## 核心特性：无障碍树（Accessibility Tree）

这是 blockcell 浏览器工具最聪明的设计。

传统的浏览器自动化（如 Selenium）通过 CSS 选择器或 XPath 定位元素，比如：
```
document.querySelector("#submit-button")
```

这种方式的问题是：网页改版后选择器就失效了，非常脆弱。

blockcell 使用**无障碍树**（Accessibility Tree）来描述页面结构：

```
snapshot 动作返回：
[1] RootWebArea "GitHub" focused
  [2] navigation "Global"
    [3] link "Homepage" url=https://github.com
  [4] main ""
    [5] heading "Let's build from here" level=1
    [6] textbox "Search or jump to..." focused
    [7] button "Sign in" @e7
    [8] button "Sign up" @e8
```

每个元素都有一个 `@e数字` 的引用（如 `@e7`），AI 可以直接用这个引用来操作元素：

```json
{"action": "click", "ref": "@e7"}
```

这种方式比 CSS 选择器更稳定，因为无障碍树反映的是页面的语义结构，不依赖具体的 HTML 实现。

---

## 35+ 个动作

`browse` 工具支持超过 35 个动作，覆盖了浏览器操作的方方面面：

### 导航类
```
navigate    - 打开 URL
back        - 后退
forward     - 前进
reload      - 刷新
get_url     - 获取当前 URL
```

### 内容读取类
```
snapshot    - 获取无障碍树（推荐）
get_content - 获取页面内容（转为 Markdown）
execute_js  - 执行 JavaScript
```

### 交互类
```
click       - 点击元素
fill        - 填写表单
type_text   - 模拟键盘输入
press_key   - 按键（Enter/Tab/Escape 等）
scroll      - 滚动页面
wait        - 等待（毫秒）
```

### 截图类
```
screenshot  - 截图（PNG）
pdf         - 生成 PDF
```

### 标签页管理
```
tab_list    - 列出所有标签页
tab_new     - 新建标签页
tab_close   - 关闭标签页
tab_switch  - 切换标签页
```

### Cookie 管理
```
cookies_get   - 获取 Cookie
cookies_set   - 设置 Cookie
cookies_clear - 清除 Cookie
```

### 高级功能
```
upload_file       - 上传文件
dialog_handle     - 处理弹窗（alert/confirm/prompt）
network_intercept - 拦截网络请求
network_block     - 屏蔽特定 URL
set_headers       - 设置请求头
set_viewport      - 设置视口大小
list_browsers     - 列出可用浏览器
```

---

## 实际例子

### 例子一：登录并抓取数据

```
你: 帮我登录 GitHub，查看我的通知列表
```

AI 的执行过程：
```
1. browse navigate "https://github.com/login"
2. browse snapshot → 找到用户名输入框 @e5
3. browse fill ref=@e5 value="你的用户名"
4. browse fill ref=@e6 value="你的密码"
5. browse click ref=@e7  (登录按钮)
6. browse wait 2000
7. browse navigate "https://github.com/notifications"
8. browse get_content → 返回通知列表 Markdown
```

### 例子二：自动填写表单

```
你: 帮我在这个网站上填写联系表单，
    姓名：张三，邮箱：zhangsan@example.com，
    留言：我想了解你们的产品
```

```
1. browse snapshot → 分析表单结构
2. browse fill ref=@e3 value="张三"
3. browse fill ref=@e4 value="zhangsan@example.com"
4. browse fill ref=@e5 value="我想了解你们的产品"
5. browse click ref=@e6  (提交按钮)
6. browse snapshot → 确认提交成功
```

### 例子三：截图并分析

```
你: 帮我截一张 blockcell 官网的截图，
    然后告诉我页面上有哪些主要内容
```

```
1. browse navigate "https://blockcell.dev"
2. browse screenshot → 保存为 screenshot.png
3. image_understand analyze path=screenshot.png
   → "页面包含：导航栏、Hero 区域、特性介绍..."
```

### 例子四：监控价格变化

```
你: 帮我监控京东上某款商品的价格，
    低于 500 元时截图发给我
```

AI 会创建一个定时任务：
```
每小时：
1. browse navigate "商品URL"
2. browse snapshot → 找到价格元素
3. 提取价格数字
4. 如果 < 500：
   browse screenshot
   notification send "价格降到了 {price}！"
```

---

## 多浏览器支持

blockcell 支持三种浏览器引擎：

```json
{
  "action": "navigate",
  "url": "https://example.com",
  "browser": "chrome"   // 或 "edge" / "firefox"
}
```

`list_browsers` 动作会自动检测你电脑上安装了哪些浏览器：

```
你: 帮我看看我电脑上有哪些浏览器可以用
AI: 调用 browse list_browsers
    可用浏览器：
    - chrome: /Applications/Google Chrome.app (推荐)
    - edge: /Applications/Microsoft Edge.app
```

---

## Session 管理

每次 `browse` 调用默认会复用已有的浏览器会话（Session）。

这意味着：
- 登录状态会保持
- Cookie 会保留
- 不需要每次都重新打开浏览器

```bash
# 查看当前浏览器会话
blockcell agent
你: 帮我列出当前的浏览器会话
AI: browse session_list
    活跃会话：
    - session_1: chrome, 3 个标签页
```

---

## 网络拦截：高级用法

`network_intercept` 动作可以拦截并修改网络请求，这在以下场景很有用：

- 注入自定义请求头（如 Authorization）
- 模拟特定的 API 响应
- 屏蔽广告或追踪脚本

```json
{
  "action": "network_intercept",
  "url_pattern": "*/api/v1/*"
}
```

拦截后，每个匹配的请求都会暂停，AI 可以决定是继续（`network_continue`）还是返回自定义响应。

---

## `chrome_control` vs `browse`：如何选择

**用 `browse`（CDP）：**
- 需要无头模式（后台运行）
- 需要精确的元素操作
- 需要截图、PDF 生成
- 需要网络拦截
- 自动化测试场景

**用 `chrome_control`（AppleScript）：**
- 想看到操作过程（可视化）
- 演示给别人看
- 需要操作 Chrome 扩展
- 简单的点击/输入操作

---

## 注意事项

1. **需要安装 Chrome**：`browse` 工具需要 Chrome/Edge/Firefox 之一
2. **无头模式**：默认以无头模式运行，不会弹出浏览器窗口
3. **反爬虫**：部分网站有反爬虫机制，可能需要设置 User-Agent 或使用 Cookie
4. **性能**：浏览器自动化比普通 HTTP 请求慢，适合需要 JS 渲染的场景

---

## 小结

blockcell 的浏览器自动化工具：

- **CDP 协议**：直接控制真实 Chrome，支持 JS 渲染
- **无障碍树**：语义化元素定位，比 CSS 选择器更稳定
- **35+ 动作**：覆盖导航、交互、截图、标签管理、网络拦截
- **多浏览器**：Chrome/Edge/Firefox 都支持
- **Session 复用**：登录状态持久化

下一篇，我们来看 Gateway 模式——如何把 blockcell 变成一个可以对外提供服务的 API。
---

*上一篇：[多渠道接入 —— Telegram/Slack/Discord/飞书都能用](./06_channels.md)*
*下一篇：[Gateway 模式 —— 把 AI 变成一个服务](./08_gateway_mode.md)*

*项目地址：https://github.com/blockcell-labs/blockcell*
*官网：https://blockcell.dev*
