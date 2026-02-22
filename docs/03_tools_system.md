# 第03篇：blockcell 的工具系统 —— 让 AI 真正能干活

> 系列文章：《blockcell 开源项目深度解析》第 3/14 篇

---

## 工具是什么

在 AI 智能体领域，"工具"（Tool）是一个核心概念。

简单说：**工具是 AI 可以调用的函数。**

没有工具的 AI，只能聊天。有了工具，AI 就能：
- 读文件 → 分析内容
- 搜网页 → 获取最新信息
- 执行命令 → 操作系统
- 发消息 → 通知你

blockcell 内置了 **50+ 工具**，覆盖了日常开发和工作的大多数场景。

---

## 工具是怎么工作的

当你对 AI 说"帮我读一下 report.txt"，背后发生了什么？

```
1. 你的输入 → LLM
2. LLM 决定：需要调用 read_file 工具
3. LLM 生成调用参数：{"path": "~/Desktop/report.txt"}
4. blockcell 执行 read_file，读取文件内容
5. 文件内容返回给 LLM
6. LLM 根据内容生成回答
7. 回答显示给你
```

这个过程叫做 **Tool Calling**（工具调用），是现代 AI 智能体的基础机制。

每个工具都有一个 **JSON Schema** 描述自己的参数，LLM 根据这个 schema 生成正确的调用参数。

---

## 工具分类详解

### 📁 文件系统工具

**`read_file`** — 读取文件
```
支持格式：txt/md/json/csv/xlsx/docx/pptx/pdf
特点：Office 文件自动转为 Markdown 文本
```

**`write_file`** — 写入文件
```
支持：创建新文件、覆盖已有文件
```

**`edit_file`** — 精确编辑文件
```
支持：字符串替换（精确匹配）
适合：修改代码、配置文件
```

**`list_dir`** — 列出目录内容
```
返回：文件名、大小、修改时间
```

**`file_ops`** — 文件操作集合
```
支持：删除、重命名/移动、复制、压缩(zip/tar.gz)、解压
```

**实际例子：**
```
你: 帮我把桌面上所有的 .log 文件压缩成一个 zip
AI: 调用 list_dir → 找到所有 .log 文件 → 调用 file_ops compress
```

---

### 🌐 网络工具

**`web_search`** — 网页搜索
```
后端：DuckDuckGo（无需 API Key）
返回：标题、摘要、URL
```

**`web_fetch`** — 抓取网页内容
```
特点：自动转换为 Markdown 格式
      支持 Cloudflare "Markdown for Agents" 协议
      节省 80% token 消耗
```

**`http_request`** — 通用 HTTP 请求
```
支持：GET/POST/PUT/PATCH/DELETE
功能：自定义 Header、Bearer Token、JSON/Form 请求体
适合：调用任意 REST API
```

**`stream_subscribe`** — 实时数据流
```
支持：WebSocket、SSE（Server-Sent Events）
功能：订阅、取消订阅、读取缓冲消息
适合：实时行情、日志流
```

**实际例子：**
```
你: 帮我查一下今天比特币的价格
AI: 调用 web_search "bitcoin price today" → web_fetch 结果页面
```

---

### 💻 系统执行工具

**`exec`** — 执行命令行
```
支持：任意 shell 命令
安全：工作目录外需要用户确认
超时：默认 30 秒
```

**实际例子：**
```
你: 帮我看看哪个进程占用了 8080 端口
AI: exec "lsof -i :8080"
```

---

### 🌐 浏览器工具

**`browse`** — CDP 浏览器自动化（重量级工具，下一篇详细介绍）
```
支持：Chrome/Edge/Firefox
动作：navigate, click, fill, screenshot, execute_js, 标签管理...
特点：可获取无障碍树（Accessibility Tree），精确定位元素
```

---

### 📊 数据处理工具

**`data_process`** — CSV/JSON 数据处理
```
动作：read_csv, write_csv, query（过滤/排序）, stats（统计）, transform
适合：数据分析、报表生成
```

**`chart_generate`** — 生成图表
```
类型：折线图、柱状图、饼图、散点图、热力图...
后端：matplotlib（PNG/SVG）、plotly（交互式 HTML）
```

**实际例子：**
```
你: 帮我分析 sales.csv，画一张按月份的销售额折线图
AI: data_process read_csv → stats → chart_generate line
```

---

### 💰 金融数据工具

**`finance_api`** — 股票/基金/外汇/债券数据
```
A股/港股：东方财富（免费，无需 Key）
美股：Alpha Vantage + Yahoo Finance
加密货币：CoinGecko（免费）
债券：东方财富国债收益率
```

**`exchange_api`** — 加密货币交易所
```
支持：Binance、OKX、Bybit
功能：行情、K线、账户、下单（需 API Key）
```

**`blockchain_rpc`** — 链上数据查询
```
支持：Ethereum、Polygon、BSC、Arbitrum 等 14+ 链
功能：查余额、查交易、调合约、ABI 编解码
```

**实际例子：**
```
你: 帮我查一下茅台今天的股价和最近一个月的走势
AI: finance_api stock_quote "600519" → finance_api stock_history
```

---

### 📧 通信工具

**`email`** — 邮件收发
```
发送：SMTP，支持附件、HTML、CC
接收：IMAP，支持搜索、读取附件
```

**`notification`** — 消息通知
```
渠道：SMS（Twilio）、Push（Pushover/Bark/ntfy）、Webhook、桌面通知
```

---

### 🎵 多媒体工具

**`audio_transcribe`** — 语音转文字
```
后端：Whisper（本地）、whisper.cpp、OpenAI API
支持：mp3/wav/m4a/flac/mp4/mkv
```

**`tts`** — 文字转语音
```
后端：macOS say、Piper（本地）、Edge TTS（免费）、OpenAI TTS
```

**`ocr`** — 图片文字识别
```
后端：Tesseract（本地）、macOS Vision、OpenAI Vision
```

**`image_understand`** — 图片理解
```
后端：GPT-4o、Claude、Gemini
功能：分析、描述、比较、提取文字
```

**`video_process`** — 视频处理
```
功能：剪辑、合并、加字幕、截图、转码、提取音频、压缩
后端：ffmpeg
```

**`camera_capture`** — 摄像头拍照
```
支持：列出摄像头、拍照（jpg/png）
后端：imagecapture/ffmpeg/screencapture 三级降级
```

---

### 📄 Office 工具

**`office_write`** — 生成 Office 文件
```
支持：PPTX（演示文稿）、DOCX（Word 文档）、XLSX（Excel 表格）
后端：Python python-pptx/python-docx/openpyxl
```

**实际例子：**
```
你: 帮我把这份市场分析报告生成一个 PPT，10 页，每页有标题和要点
AI: office_write create_pptx，生成 report.pptx
```

---

### 🧠 记忆与知识工具

**`memory_query`** — 搜索记忆
```
后端：SQLite + FTS5 全文搜索
过滤：scope（短期/长期）、type、tags、时间范围
```

**`memory_upsert`** — 保存记忆
```
支持：dedup_key 去重、expires_in_days 过期时间
```

**`knowledge_graph`** — 知识图谱
```
后端：SQLite + FTS5
功能：实体管理、关系管理、路径查找、子图提取、导出（JSON/DOT/Mermaid）
```

---

### ⏰ 调度与监控工具

**`alert_rule`** — 告警规则
```
操作符：gt/lt/gte/lte/eq/ne/change_pct/cross_above/cross_below
触发动作：自动执行其他工具（如发通知）
持久化：规则保存在 workspace/alerts/rules.json
```

**`cron`** — 定时任务
```
格式：标准 cron 表达式
功能：创建、列出、删除定时任务
```

---

### 🔧 系统信息工具

**`system_info`** — 系统探针
```
检测：硬件（CPU/内存/GPU/摄像头/麦克风）
      软件（rustc/python3/node/git/docker/ffmpeg）
      网络（连通性/接口）
      当前能力状态
```

---

## 工具安全机制

blockcell 有一套路径安全系统：

```
工作目录内（~/.blockcell/workspace）：自动允许
工作目录外：弹出确认提示
已授权目录：一次授权，后续自动允许
Gateway 模式：工作目录外一律拒绝
```

这意味着 AI 不会在你不知情的情况下修改你的重要文件。

---

## 如何查看所有工具

```bash
blockcell tools
```

输出示例：
```
Available tools (52):
  read_file        - Read file contents
  write_file       - Write content to file
  edit_file        - Edit file with string replacement
  web_search       - Search the web
  web_fetch        - Fetch web page content
  exec             - Execute shell command
  browse           - Browser automation via CDP
  finance_api      - Stock/crypto/forex data
  ...
```

---

## 工具的扩展性

除了内置工具，blockcell 还支持：

1. **通过 `http_request` 调用任意 API** — 不需要专门的工具
2. **通过 `exec` 执行任意脚本** — Python/Node/Shell 都行
3. **通过技能系统（Skill）封装复杂流程** — 下一篇详细介绍

---

## 小结

blockcell 的工具系统是它的核心竞争力：

- **覆盖广**：50+ 工具，从文件到金融，从浏览器到 IoT
- **开箱即用**：大多数工具不需要额外配置
- **安全**：路径确认机制保护你的文件系统
- **可扩展**：通过 HTTP/exec/技能系统无限扩展

下一篇，我们来看技能（Skill）系统——这是 blockcell 让 AI 能力可复用、可进化的关键机制。
---

*上一篇：[5分钟上手 blockcell —— 从安装到第一次对话](./02_quickstart.md)*
*下一篇：[技能（Skill）系统 —— 用 Rhai 脚本扩展 AI 能力](./04_skill_system.md)*

*项目地址：https://github.com/blockcell-labs/blockcell*
*官网：https://blockcell.dev*
