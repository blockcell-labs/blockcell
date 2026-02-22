# 第02篇：5分钟上手 blockcell —— 从安装到第一次对话

> 系列文章：《blockcell 开源项目深度解析》第 2/14 篇

---

## 前言

上一篇我们介绍了 blockcell 是什么。这一篇直接动手，5分钟内让它跑起来。

**你需要准备的：**
- 一台 macOS 或 Linux 电脑（Windows 也支持，但本文以 macOS 为例）
- 一个 LLM API Key（OpenAI、DeepSeek、Kimi 都行，后面会说怎么选）

---

## 5分钟最短路径（照做就能跑起来）

如果你只想最快跑通一次，按下面 5 步即可：

1. 安装：运行安装脚本
2. 初始化：`blockcell onboard`
3. 配置：编辑 `~/.blockcell/config.json`，填一个可用的 Provider（推荐 DeepSeek 或 Kimi）
4. 检查：`blockcell status`，确保全部是 ✓
5. 启动：`blockcell agent`，随便发一句话测试

后面的内容会更详细（多 Provider 选择、常用命令、FAQ、部署建议），你可以在跑通后再慢慢看。

---

## 第一步：安装

### 方式一：一键安装脚本（推荐）

```bash
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/refs/heads/main/install.sh | sh
```

安装完成后，`blockcell` 命令会出现在 `~/.local/bin/`。如果找不到命令，把这个路径加到你的 PATH：

```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
source ~/.zshrc
```

### 方式二：从源码编译

如果你想自己编译（需要 Rust 1.75+）：

```bash
git clone https://github.com/blockcell-labs/blockcell.git
cd blockcell/blockcell
cargo build --release
cp target/release/blockcell ~/.local/bin/
```

### 验证安装

```bash
blockcell --version
# blockcell 0.x.x
```

---

## 第二步：初始化

```bash
blockcell onboard
```

这个命令会：
1. 创建 `~/.blockcell/` 目录结构
2. 生成默认配置文件 `~/.blockcell/config.json`
3. 打印一个简短的引导说明

目录结构长这样：

```
~/.blockcell/
├── config.json          # 主配置文件
└── workspace/           # AI 的工作目录
    ├── memory/          # 记忆数据库
    ├── sessions/        # 会话历史
    ├── skills/          # 用户安装的技能
    ├── media/           # 截图、音频等媒体文件
    └── audit/           # 操作审计日志
```

---

## 第三步：配置 API Key

打开配置文件：

```bash
# macOS
open ~/.blockcell/config.json

# 或者用命令行编辑器
nano ~/.blockcell/config.json
```

找到 `providers` 部分，填入你的 API Key。

### 选项 A：使用 DeepSeek（最便宜，推荐新手）

DeepSeek 的 API 非常便宜，适合测试：

```json
{
  "providers": {
    "deepseek": {
      "apiKey": "sk-你的DeepSeek密钥",
      "apiBase": "https://api.deepseek.com/v1"
    }
  },
  "agents": {
    "defaults": {
      "model": "deepseek-chat"
    }
  }
}
```

### 选项 B：使用 Kimi/Moonshot（国内访问稳定）

```json
{
  "providers": {
    "kimi": {
      "apiKey": "sk-你的Kimi密钥",
      "apiBase": "https://api.moonshot.cn/v1"
    }
  },
  "agents": {
    "defaults": {
      "model": "kimi/moonshot-v1-8k"
    }
  }
}
```

### 选项 C：使用 OpenRouter（一个 Key 访问所有模型）

```json
{
  "providers": {
    "openrouter": {
      "apiKey": "sk-or-你的OpenRouter密钥",
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

### 选项 D：使用 Ollama（完全本地，免费）

如果你已经安装了 Ollama 并拉取了模型：

```json
{
  "providers": {
    "ollama": {
      "apiBase": "http://localhost:11434"
    }
  },
  "agents": {
    "defaults": {
      "model": "ollama/llama3"
    }
  }
}
```

---

## 第四步：检查状态

```bash
blockcell status
```

输出类似：

```
✓ Config loaded
✓ Provider: deepseek (deepseek-chat)
✓ Workspace: ~/.blockcell/workspace
✓ Memory: SQLite (0 items)
✓ Skills: 0 user skills, 44 builtin skills
✓ Channels: none configured
```

如果有红色的 ✗，说明配置有问题，根据提示修改。

---

## 第五步：启动对话

```bash
blockcell agent
```

你会看到欢迎界面：

```
╔══════════════════════════════════════╗
║         blockcell agent              ║
║  Type /tasks to see background tasks ║
║  Type /quit to exit                  ║
╚══════════════════════════════════════╝

You:
```

现在可以开始对话了！

---

## 试试这些命令

### 基础对话

```
You: 你好，介绍一下你自己
```

### 让 AI 搜索信息

```
You: 帮我搜索一下今天有哪些 AI 相关的新闻
```

AI 会自动调用 `web_search` 工具，然后用 `web_fetch` 获取内容。

### 读取本地文件

```
You: 帮我读一下 ~/Desktop/report.txt，总结一下主要内容
```

> ⚠️ 注意：读取工作目录（`~/.blockcell/workspace`）之外的文件时，blockcell 会弹出确认提示，你需要输入 `y` 确认。这是安全机制。

### 执行命令

```
You: 帮我看看当前目录有哪些文件
```

AI 会调用 `exec` 工具执行 `ls` 命令。

### 写文件

```
You: 帮我在工作目录里创建一个 hello.txt，内容是"Hello from blockcell"
```

---

## 常用 CLI 命令一览

除了 `agent` 交互模式，blockcell 还有很多实用命令：

```bash
# 查看所有可用工具
blockcell tools

# 查看/管理记忆
blockcell memory list
blockcell memory search "股票"

# 查看/管理技能
blockcell skills list

# 查看定时任务
blockcell cron list

# 查看消息渠道状态
blockcell channels status

# 查看进化记录
blockcell evolve list

# 查看告警规则
blockcell alerts list

# 查看实时数据流
blockcell streams list

# 查看知识图谱
blockcell knowledge stats

# 查看日志
blockcell logs

# 自我诊断
blockcell doctor
```

---

## 配置文件完整说明

`~/.blockcell/config.json` 的主要字段：

```json
{
  "providers": {
    "openai": {
      "apiKey": "sk-...",
      "apiBase": "https://api.openai.com/v1"
    }
  },
  "agents": {
    "defaults": {
      "model": "gpt-4o",
      "maxTokens": 4096,
      "temperature": 0.7
    }
  },
  "tools": {
    "tickIntervalSecs": 30
  },
  "gateway": {
    "host": "0.0.0.0",
    "port": 18790,
    "webuiPort": 18791,
    "apiToken": "你的访问令牌（可选）"
  },
  "channels": {
    "telegram": {
      "botToken": "你的Bot Token",
      "allowFrom": ["你的用户ID"]
    }
  }
}
```

---

## 遇到问题？

### 问题1：命令找不到

```bash
which blockcell
# 如果没有输出，说明 PATH 没配置好
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc && source ~/.zshrc
```

### 问题2：API 调用失败

```bash
blockcell doctor
# 会检查网络连接和 API 配置
```

### 问题3：想换模型

直接修改 `config.json` 里的 `agents.defaults.model`，重启 `blockcell agent` 即可。

### 问题4：想看 AI 调用了哪些工具

在对话中，AI 每次调用工具都会显示工具名称和参数。如果想看更详细的日志：

```bash
blockcell logs --tail 50
```

---

## 小结

到这里，你已经完成了：
- ✅ 安装 blockcell
- ✅ 配置 API Key
- ✅ 启动第一次对话
- ✅ 了解基本命令

下一篇，我们深入看 blockcell 的工具系统——它内置了 50+ 工具，是 AI 真正能"干活"的核心。
---

*上一篇：[什么是 blockcell？一个会自我进化的 AI 智能体框架](./01_what_is_blockcell.md)*
*下一篇：[blockcell 的工具系统 —— 让 AI 真正能干活](./03_tools_system.md)*

*项目地址：https://github.com/blockcell-labs/blockcell*
*官网：https://blockcell.dev*
