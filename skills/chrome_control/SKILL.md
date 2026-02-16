# 浏览器自动化技能 (Chrome Control)

## 触发语句
- "打开百度搜索 kimi 官网" / "帮我搜一下 xxx" / "打开网页 xxx"
- "open chrome and search for xxx" / "go to website xxx"
- "打开浏览器" / "上网"

## 参数澄清
当用户请求浏览器操作时：
1. **目标 URL** — 如果用户说"打开百度"，URL 为 `https://www.baidu.com`；如果说"打开谷歌"，URL 为 `https://www.google.com`
2. **搜索关键词** — 如果用户说"搜索 xxx"，需要先打开搜索引擎再输入关键词
3. **具体操作** — 如果用户说"点击某个按钮"，需要确认目标元素

如果用户只说"打开百度搜索 xxx"，**不需要询问**，直接执行完整流程。

## 常用 URL 映射
- 百度 → https://www.baidu.com
- 谷歌/Google → https://www.google.com
- 必应/Bing → https://www.bing.com
- 知乎 → https://www.zhihu.com
- 微博 → https://www.weibo.com
- GitHub → https://github.com
- B站/哔哩哔哩 → https://www.bilibili.com

## 工具调用顺序

### 场景 1：打开网页并搜索
1. `chrome_control(action="open", url="https://www.baidu.com")` — 打开百度
2. `chrome_control(action="wait", amount=1500)` — 等待页面加载
3. `chrome_control(action="click", selector="#kw")` — 点击搜索框（百度搜索框 CSS 选择器）
4. `chrome_control(action="type", text="kimi官网")` — 输入搜索关键词
5. `chrome_control(action="press_key", text="return")` — 按回车搜索
6. `chrome_control(action="wait", amount=2000)` — 等待搜索结果
7. `chrome_control(action="screenshot")` — 截图记录结果（可选）
8. `chrome_control(action="read")` — 读取搜索结果页面内容

### 场景 2：仅打开网页
1. `chrome_control(action="open", url="<目标URL>")` — 打开页面
2. `chrome_control(action="wait", amount=2000)` — 等待加载
3. `chrome_control(action="read")` — 读取页面内容

### 场景 3：在当前页面操作
1. `chrome_control(action="find_element", selector="<CSS选择器>")` — 查找目标元素
2. `chrome_control(action="click", selector="<CSS选择器>")` — 点击元素
3. `chrome_control(action="type", text="<输入内容>", selector="<CSS选择器>")` — 在元素中输入

## 搜索引擎选择器参考
| 搜索引擎 | 搜索框选择器 | 搜索按钮选择器 |
|---------|------------|-------------|
| 百度 | `#kw` | `#su` |
| 谷歌 | `textarea[name="q"]` | `input[name="btnK"]` |
| 必应 | `#sb_form_q` | `#search_icon` |

## 输出格式
```markdown
🌐 浏览器操作完成！

**操作步骤**:
1. ✅ 打开百度 (https://www.baidu.com)
2. ✅ 在搜索框输入 "kimi官网"
3. ✅ 执行搜索
4. ✅ 获取搜索结果

**搜索结果摘要**:
- [结果1标题](链接)
- [结果2标题](链接)
- ...

[需要我点击某个搜索结果吗？]
```

## 失败与降级策略
1. **Chrome 未安装** → 提示用户安装 Google Chrome
2. **辅助功能权限未授权** → 提示用户在系统偏好设置 > 安全性与隐私 > 辅助功能中添加终端
3. **元素未找到** → 尝试使用 `find_element` 查找替代选择器，或使用 `execute_js` 直接操作
4. **页面未加载完成** → 增加 `wait` 时间后重试
5. **AppleScript 错误** → 降级为使用 `browse` 工具（headless 模式）

## 示例

### 示例 1：打开百度搜索 kimi 官网
**用户**: 打开百度，搜索 kimi 官网
**助手**:
1. `chrome_control(action="open", url="https://www.baidu.com")`
2. `chrome_control(action="wait", amount=1500)`
3. `chrome_control(action="click", selector="#kw")`
4. `chrome_control(action="type", text="kimi官网")`
5. `chrome_control(action="press_key", text="return")`
6. `chrome_control(action="wait", amount=2000)`
7. `chrome_control(action="read")`
8. 汇总搜索结果返回给用户

### 示例 2：打开 GitHub
**用户**: 帮我打开 GitHub
**助手**:
1. `chrome_control(action="open", url="https://github.com")`
2. `chrome_control(action="wait", amount=2000)`
3. 告知用户已打开

### 示例 3：在当前页面点击链接
**用户**: 点击第一个搜索结果
**助手**:
1. `chrome_control(action="find_element", selector=".result a, .c-container a")` — 查找搜索结果链接
2. `chrome_control(action="click", selector=".result a")` — 点击第一个结果
3. `chrome_control(action="wait", amount=2000)`
4. `chrome_control(action="read")` — 读取新页面内容
