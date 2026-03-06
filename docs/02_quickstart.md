# 第02篇：5分钟上手 blockcell —— 从安装到第一次对话

> 系列文章：《blockcell 开源项目深度解析》第 2 篇
---

## 前言

上一篇我们介绍了 blockcell 是什么。这一篇直接动手，5分钟内让它跑起来。

**你需要准备的：**
- 一台 macOS 或 Linux 电脑（Windows 也支持，但本文以 macOS 为例）
- 一个 LLM API Key（OpenAI、DeepSeek、Kimi 都行，后面会说怎么选）

---

## 5分钟最短路径（照做就能跑起来）

如果你只想最快跑通一次，按下面 4 步即可：

1. 安装：运行安装脚本
2. 配置：`blockcell setup`（交互式向导，自动完成初始化和基础校验）
3. 启动：`blockcell agent`，随便发一句话测试
4. 启动：`blockcell gateway`，浏览器打开 `http://127.0.0.1:18791` 查看 WebUI

后面的内容会更详细（多 Provider 选择、多 Agent、常用命令、FAQ、部署建议），你可以在跑通后再慢慢看。

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

## 第二步：配置(推荐使用 setup 向导)

### 方式一:交互式向导(推荐)

```bash
blockcell setup
```

这个命令会：
1. 创建 `~/.blockcell/` 目录结构
2. 引导你选择 LLM provider（DeepSeek/OpenAI/Kimi/Anthropic/Gemini/Zhipu/MiniMax/Ollama）
3. 配置 API Key 和模型
4. 可选：配置一个消息渠道（Telegram/飞书/企业微信/钉钉/Lark）
5. 自动验证配置是否有效
6. 如果你配置了外部渠道但还没绑定 owner，会自动把该渠道绑定到 `default` agent
7. 如果后续要把同一渠道下的某个账号 / 机器人单独分配给其它 agent，可继续配置 `channelAccountOwners.<channel>.<accountId>`

**支持的 provider:**
- `deepseek` - 推荐,便宜且性能好
- `openai` - GPT-4o 等
- `kimi` - 国内访问稳定
- `anthropic` - Claude 系列
- `gemini` - Google Gemini
- `zhipu` - 智谱 GLM
- `minimax` - MiniMax
- `ollama` - 本地模型,免费

**非交互式用法:**
```bash
# 直接指定 provider 和 API key
blockcell setup --provider deepseek --api-key sk-xxx --model deepseek-chat

# 同时配置渠道
blockcell setup --provider kimi --api-key sk-xxx --channel telegram

# 重置配置后重新设置
blockcell setup --force
```

### 方式二:使用 onboard(传统方式)

```bash
blockcell onboard
```

这个命令会创建目录结构和默认配置文件,但需要手动编辑 `config.json`

目录结构长这样：

```
~/.blockcell/
├── config.json          # 主配置文件
├── sessions/            # default agent 的会话历史
├── audit/               # default agent 的审计日志
├── workspace/           # default agent 的工作目录
│   ├── memory/          # 记忆数据库
│   ├── skills/          # 用户安装的技能
│   ├── media/           # 截图、音频等媒体文件
│   └── tasks.json       # 全局后台任务快照
└── agents/              # 非 default agent 的独立目录（按需创建）
    └── ops/
        ├── sessions/
        ├── audit/
        └── workspace/
```

说明：`default` agent 直接使用 `~/.blockcell/` 根目录；其它 agent 使用 `~/.blockcell/agents/<ID>/` 下的独立工作区。

---

## 第三步:手动配置 API Key(如果没用 setup 向导)

如果你使用了 `blockcell setup`,可以跳过这一步。如果使用了 `blockcell onboard`,需要手动编辑配置:

```bash
# macOS
open ~/.blockcell/config.json

# 或者用命令行编辑器
nano ~/.blockcell/config.json
```

找到 `providers` 部分,填入你的 API Key。

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
      "model": "deepseek-chat",
      "provider": "deepseek",
      "modelPool": [
        {
          "model": "deepseek-chat",
          "provider": "deepseek",
          "weight": 1,
          "priority": 1
        }
      ]
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
      "apiBase": "https://api.moonshot.ai/v1"
    }
  },
  "agents": {
    "defaults": {
      "model": "moonshot-v1-8k",
      "provider": "kimi",
      "modelPool": [
        {
          "model": "moonshot-v1-8k",
          "provider": "kimi",
          "weight": 1,
          "priority": 1
        }
      ]
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
      "model": "anthropic/claude-sonnet-4-20250514",
      "provider": "openrouter",
      "modelPool": [
        {
          "model": "anthropic/claude-sonnet-4-20250514",
          "provider": "openrouter",
          "weight": 1,
          "priority": 1
        }
      ]
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
      "apiKey": "ollama",
      "apiBase": "http://localhost:11434",
      "apiType": "ollama"
    }
  },
  "agents": {
    "defaults": {
      "model": "llama3",
      "provider": "ollama",
      "modelPool": [
        {
          "model": "llama3",
          "provider": "ollama",
          "weight": 1,
          "priority": 1
        }
      ]
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
      "apiBase": "https://api.openai.com/v1",
      "apiType": "openai"
    },
    "deepseek": {
      "apiKey": "sk-...",
      "apiBase": "https://api.deepseek.com/v1"
    }
  },
  "agents": {
    "defaults": {
      "model": "gpt-4o",
      "provider": "openai",
      "maxTokens": 8192,
      "temperature": 0.7,
      "modelPool": [
        {
          "model": "gpt-4o",
          "provider": "openai",
          "weight": 2,
          "priority": 1
        },
        {
          "model": "deepseek-chat",
          "provider": "deepseek",
          "weight": 1,
          "priority": 2
        }
      ]
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
  "channelOwners": {
    "telegram": "default"
  },
  "channelAccountOwners": {
    "telegram": {
      "bot2": "ops"
    }
  },
  "channels": {
    "telegram": {
      "enabled": true,
      "token": "你的 Bot Token",
      "allowFrom": ["你的用户ID"]
    }
  }
}
```

### modelPool 多模型高可用配置

`modelPool` 是一个可选的高级功能，用于配置多模型负载均衡和自动降级：

**字段说明：**
- `model`: 模型名称（如 "gpt-4o"、"deepseek-chat"）
- `provider`: 对应 providers 表中的 key
- `weight`: 负载均衡权重（正整数，越大越优先被选中），默认 1
- `priority`: 优先级（小数字 = 高优先级），同优先级内按 weight 加权随机，默认 1
- `inputPrice`: 可选，输入价格（USD/1M tokens）
- `outputPrice`: 可选，输出价格（USD/1M tokens）

**使用场景：**
1. **多模型负载均衡**：同 priority 下配置多个模型，按 weight 加权随机选择
2. **自动降级**：主模型失败时自动切换到备用模型（通过 priority 控制）
3. **成本优化**：配置价格信息，系统可根据成本选择模型

**注意：** 如果不配置 `modelPool`，系统将使用传统的单 `model` + `provider` 配置（向后兼容）。

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
