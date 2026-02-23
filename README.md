# BlockCell

<div align="center">

**用 Rust 构建的自进化 AI 智能体框架**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)
[![GitHub stars](https://img.shields.io/github/stars/blockcell-labs/blockcell?style=social)](https://github.com/blockcell-labs/blockcell)

[官网](https://blockcell.dev) • [文档](https://blockcell.dev/docs) • [English](README.en.md)

</div>

---

## 🌟 BlockCell 有何不同

BlockCell 不只是一个聊天机器人 — 它是一个**真正能执行任务**的 AI 智能体。当 ChatGPT 只能告诉你该做什么时，BlockCell 可以：

- 📁 读写你系统上的文件
- 🌐 控制浏览器并自动化网页任务
- 📊 分析 Excel/PDF 文件并生成报表
- 💰 监控股票价格和加密货币市场
- 📧 跨平台发送邮件和消息
- 🔄 **自我进化** — 自动修复 bug 并部署改进

```
你："监控特斯拉股价，如果跌破 200 美元就提醒我"
BlockCell: ✓ 设置监控 → ✓ 每小时检查价格 → ✓ 发送 Telegram 提醒
```

---

## 🎯 名字由来

> *"极简的单元，极繁的整体。"*

**BlockCell** 的灵感来自《星际之门》中的**复制者（Replicators）** — 由无数微小、独立的模块块组成的机械生命体。每个模块本身很简单，但组合在一起就能形成战舰、士兵和智慧。它们瞬间适应，进化速度超过任何武器，永远无法被摧毁。

这种哲学贯穿于整个框架：

- **Block** → 不可变的 Rust 宿主：安全、稳定、确定性
- **Cell** → 可变的技能层：有生命、能自我修复、无限进化

传统软件在发布的那一刻就停止了生长。BlockCell 是**活的**。

→ [完整命名故事](https://blockcell.dev/naming-story)

---

## ✨ 核心特性

### 🛠️ 内置 50+ 工具

- **文件与系统**：读写文件、执行命令、处理 Excel/Word/PDF
- **网页与浏览器**：网页抓取、无头 Chrome 自动化（CDP）、HTTP 请求
- **金融数据**：实时股票行情（A股/港股/美股）、加密货币价格、DeFi 数据
- **通讯**：邮件（SMTP/IMAP）、Telegram、Slack、Discord、飞书
- **媒体**：截图、语音转文字（Whisper）、图表生成、Office 文件创建
- **AI 增强**：图像理解、文字转语音、OCR

### 🧬 自我进化系统

当 AI 在执行任务时反复失败，BlockCell 可以：

1. 检测错误模式
2. 使用 LLM 生成改进的代码
3. 自动审计、编译和测试
4. 通过金丝雀部署（10% → 50% → 100%）
5. 如果性能下降则自动回滚

```
检测到错误 → LLM 生成修复 → 审计 → 测试 → 金丝雀部署 → 全量发布
                                        ↓ 失败时
                                      自动回滚
```

### 🌐 多渠道支持

将 BlockCell 作为守护进程运行，连接到：

- **Telegram**（长轮询）
- **WhatsApp**（通过 mautrix 桥接）
- **飞书/Lark**（WebSocket）
- **Slack**（REST 轮询）
- **Discord**（Gateway WebSocket）
- **钉钉**（Stream SDK）
- **企业微信**（WeCom）

### 🏗️ Rust 宿主 + Rhai 技能架构

```
┌─────────────────────────────────────────────┐
│         Rust 宿主（可信核心）                │
│  消息总线 | 工具注册表 | 调度器              │
│  存储 | 审计 | 安全                          │
└─────────────────────────────────────────────┘
                     ↕
┌─────────────────────────────────────────────┐
│       Rhai 技能（可变层）                    │
│  自定义技能 | AI 生成代码                    │
│  可进化 | 沙箱隔离 | 热重载                  │
└─────────────────────────────────────────────┘
```

- **Rust 宿主**：不可变、安全、高性能的基础
- **Rhai 技能**：灵活、可进化、AI 生成的能力

---

## 🚀 快速开始

### 安装（推荐）

```bash
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/main/install.sh | sh
```

这会将 `blockcell` 安装到 `~/.local/bin`。自定义安装位置：

```bash
BLOCKCELL_INSTALL_DIR="$HOME/bin" \
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/main/install.sh | sh
```

### 从源码构建

**前置要求**：Rust 1.75+

```bash
git clone https://github.com/blockcell-labs/blockcell.git
cd blockcell
cargo build --release
```

### 首次运行

```bash
# 初始化配置
blockcell onboard

# 编辑配置并添加你的 API 密钥
# ~/.blockcell/config.json

# 启动交互模式
blockcell agent
```

### 守护进程模式（带 WebUI）

```bash
blockcell gateway
```

- **API 服务器**：`http://localhost:18790`
- **WebUI**：`http://localhost:18791`

---

## 📸 项目截图

<div align="center">

### 守护进程模式
![启动 Gateway](screenshot/start-gateway.png)

### WebUI 界面
![WebUI 对话](screenshot/webui-chat.png)

</div>

---

## ⚙️ 配置说明

最小配置示例（`~/.blockcell/config.json`）：

```json
{
  "providers": {
    "openrouter": {
      "apiKey": "YOUR_API_KEY",
      "apiBase": "https://openrouter.ai/api/v1"
    }
  },
  "agents": {
    "defaults": {
      "model": "anthropic/claude-sonnet-4-20250514"
    }
  }
}
```

### 支持的 LLM 提供商

- **OpenAI**（GPT-4o、GPT-4.1、o1、o3）
- **Anthropic**（Claude 3.5 Sonnet、Claude 4）
- **Google Gemini**（Gemini 2.0 Flash、Pro）
- **DeepSeek**（DeepSeek V3、R1）
- **Kimi/Moonshot**（月之暗面）
- **MiniMax**（[MiniMax 2.5](https://www.minimaxi.com/)）
- **智谱 AI**（[GLM-5](https://bigmodel.cn/)）
- **硅基流动**（[SiliconFlow](https://siliconflow.cn/)）
- **Ollama**（本地模型，完全离线）
- **OpenRouter**（统一访问 200+ 模型）

---

## 🔧 可选依赖

要使用完整功能，请安装这些工具：

- **图表**：Python 3 + `matplotlib` / `plotly`
- **Office**：Python 3 + `python-pptx` / `python-docx` / `openpyxl`
- **音频**：`ffmpeg` + `whisper`（或使用 API 后端）
- **浏览器**：Chrome/Chromium（用于 CDP 自动化）
- **仅 macOS**：`chrome_control`、`app_control`

---

## 📚 文档

- [快速开始指南](QUICKSTART.zh-CN.md)
- [架构深度解析](docs/01_what_is_blockcell.md)
- [工具系统](docs/03_tools_system.md)
- [技能系统](docs/04_skill_system.md)
- [记忆系统](docs/05_memory_system.md)
- [渠道配置](docs/06_channels.md)
- [自我进化](docs/09_self_evolution.md)

---

## 🏗️ 项目结构

```
blockcell/
├── bin/blockcell/          # CLI 入口
└── crates/
    ├── core/               # 配置、路径、共享类型
    ├── agent/              # Agent 运行时和安全
    ├── tools/              # 50+ 内置工具
    ├── skills/             # Rhai 引擎与进化
    ├── storage/            # SQLite 记忆与会话
    ├── channels/           # 消息适配器
    ├── providers/          # LLM 提供商客户端
    ├── scheduler/          # Cron 与心跳
    └── updater/            # 自升级系统
```

---

## 🤝 贡献

我们欢迎贡献！以下是开始的方法：

1. Fork 本仓库
2. 创建特性分支（`git checkout -b feature/amazing-feature`）
3. 提交你的更改（`git commit -m 'Add amazing feature'`）
4. 推送到分支（`git push origin feature/amazing-feature`）
5. 打开 Pull Request

详细指南请参阅 [CONTRIBUTING.md](CONTRIBUTING.md)。

---

## 🔒 安全性

- **路径安全**：自动验证文件系统访问
- **沙箱执行**：Rhai 脚本在隔离环境中运行
- **审计日志**：所有工具执行都被记录
- **网关认证**：API 访问支持 Bearer token

在交互模式下，`~/.blockcell/workspace` 外的操作需要明确确认。

---

## 📊 使用场景

### 金融自动化
```
"监控茅台股价，如果跌破 1500 就提醒我"
"分析我的 portfolio.xlsx 并建议再平衡"
```

### 数据处理
```
"读取 ~/Documents 中的所有 PDF 并创建摘要表格"
"从 data.csv 生成带图表的销售报告"
```

### 网页自动化
```
"每小时检查公司网站，如果宕机就提醒"
"用 sheet.xlsx 中的数据填写 example.com 上的表单"
```

### 通讯
```
"每天发送站会总结到 Slack #team-updates"
"将紧急邮件转发到我的 Telegram"
```

---

## 🌍 社区

- **GitHub**：[blockcell-labs/blockcell](https://github.com/blockcell-labs/blockcell)
- **官网**：[blockcell.dev](https://blockcell.dev)
- **Discord**：[加入我们的社区](https://discord.gg/blockcell)
- **Twitter**：[@blockcell_dev](https://twitter.com/@blockcell_ai)

---

## 📝 许可证

本项目采用 MIT 许可证 - 详见 [LICENSE](LICENSE) 文件。

---

## 🙏 致谢

BlockCell 站在巨人的肩膀上：

- [Rust](https://www.rust-lang.org/) - 系统编程语言
- [Rhai](https://rhai.rs/) - 嵌入式脚本引擎
- [Tokio](https://tokio.rs/) - 异步运行时
- [SQLite](https://www.sqlite.org/) - 嵌入式数据库
- [OpenClaw](https://github.com/openclaw/openclaw) - OpenClaw
- [NonaClaw](https://github.com/nonaclaw) - python版本Claw

---

<div align="center">

**如果你觉得 BlockCell 有用，请在 GitHub 上给我们一个 ⭐️！**

[⭐ 在 GitHub 上 Star](https://github.com/blockcell-labs/blockcell) • [📖 阅读文档](https://blockcell.dev/docs) • [💬 加入 Discord](https://discord.gg/blockcell)

</div>
