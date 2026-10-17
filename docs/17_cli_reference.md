# CLI 命令参考

blockcell 命令行工具的完整参数说明。

## 全局选项

```
blockcell [OPTIONS] <COMMAND>
```

| 选项 | 短写 | 说明 |
|------|------|------|
| `--verbose` | `-v` | 开启 debug 级别详细日志 |
| `--help` | `-h` | 显示帮助信息 |
| `--version` | `-V` | 显示版本号 |

---

## setup — 交互式配置向导（推荐）

```
blockcell setup [OPTIONS]
```

首次使用时推荐运行，通过交互式向导完成 LLM provider 和可选渠道的配置。相比 `onboard`，`setup` 提供更友好的引导流程和自动验证。

| 选项 | 说明 |
|------|------|
| `--force` | 重置现有配置为默认值后再开始设置 |
| `--provider <NAME>` | 指定 LLM provider（deepseek/openai/kimi/anthropic/gemini/zhipu/qwen/xai/mistral/minimax/groq/siliconflow/openrouter/ollama） |
| `--api-key <KEY>` | 指定 provider 的 API key |
| `--model <MODEL>` | 指定模型名（如 deepseek-v4-pro、kimi-k2.6、claude-opus-4-8） |
| `--channel <NAME>` | 可选渠道配置（telegram/feishu/wecom/dingtalk/lark/qq/napcat/none；`skip` 也兼容） |
| `--skip-provider-test` | 跳过保存后的 provider 配置验证 |

**支持的 provider:**
- `deepseek` - 推荐，性价比高
- `openai` - GPT-5.5 等模型
- `kimi` (moonshot) - 国内访问稳定
- `anthropic` (claude) - Claude 系列
- `gemini` - Google Gemini
- `zhipu` - 智谱 GLM
- `qwen` - 通义千问
- `xai` - Grok 系列
- `mistral` - Mistral
- `minimax` - MiniMax
- `groq` - Groq OpenAI-compatible
- `siliconflow` - 硅基流动
- `openrouter` - OpenRouter 聚合路由
- `ollama` - 本地模型，免费

**支持的渠道:**
- `telegram` - Telegram Bot
- `feishu` - 飞书机器人
- `wecom` - 企业微信
- `dingtalk` - 钉钉
- `lark` - Lark
- `qq` - QQ 官方机器人
- `napcat` - NapCatQQ / OneBot 11
- `none` / `skip` - 跳过渠道配置（仅使用 WebUI）

**示例：**
```bash
# 交互式向导（推荐）
blockcell setup

# 非交互式，直接指定 provider
blockcell setup --provider deepseek --api-key sk-xxx --model deepseek-v4-pro

# 同时配置渠道
blockcell setup --provider kimi --api-key sk-xxx --channel telegram

# 重置配置后重新设置
blockcell setup --force

# 跳过验证（加快设置速度）
blockcell setup --provider ollama --skip-provider-test
```

**向导流程：**
1. 选择 LLM provider（或输入 skip 跳过）
2. 输入 API key（ollama 除外）
3. 选择或确认模型名称
4. 可选：配置一个消息渠道
5. 自动验证 provider 配置（除非使用 `--skip-provider-test`）
6. 如渠道 owner 缺失，自动绑定到 `default` agent
7. 显示配置摘要和下一步操作提示

---

## onboard — 初始化配置（传统方式）

```
blockcell onboard [OPTIONS]
```

创建配置文件和工作区目录。相比 `setup`，`onboard` 更适合脚本化部署或已熟悉配置结构的用户。

| 选项 | 说明 |
|------|------|
| `--force` | 强制覆盖已有配置 |
| `--interactive` | 交互式向导模式 |
| `--provider <NAME>` | 指定 LLM provider（如 deepseek、openai、kimi、anthropic） |
| `--api-key <KEY>` | 指定 provider 的 API key |
| `--model <MODEL>` | 指定模型名（如 deepseek-v4-pro、kimi-k2.6） |
| `--channels-only` | 仅更新渠道配置，跳过 provider 设置 |

**示例：**
```bash
# 创建默认配置
blockcell onboard

# 非交互式，直接指定 provider
blockcell onboard --provider deepseek --api-key sk-xxx --model deepseek-v4-pro

# 仅重新配置渠道
blockcell onboard --channels-only
```

**注意：** 新用户推荐使用 `blockcell setup` 而非 `onboard`，前者提供更友好的引导体验。

---

## agent — 运行 Agent

```
blockcell agent [OPTIONS]
```

启动 Agent 会话。不带 `--message` 时进入交互模式。

| 选项 | 短写 | 默认值 | 说明 |
|------|------|--------|------|
| `--message <TEXT>` | `-m` | — | 发送单条消息后退出 |
| `--agent <ID>` | `-a` | `default` | 指定运行的 agent |
| `--session <ID>` | `-s` | `cli:<agent>` | 会话 ID |
| `--model <MODEL>` | — | — | 临时覆盖 LLM 模型 |
| `--provider <NAME>` | — | — | 临时覆盖 LLM provider |

**示例：**
```bash
# 进入交互模式
blockcell agent

# 使用指定 agent
blockcell agent --agent ops

# 发送单条消息
blockcell agent -a ops -m "帮我查询 BTC 价格"

# 指定会话 ID（便于管理多个会话）
blockcell agent -a ops -s work:finance

# 临时使用不同模型
blockcell agent --agent ops --model gpt-5.5 --provider openai
```

**统一斜杠命令：**

在 CLI 交互模式中，以 `/` 开头的输入会先进入统一斜杠命令处理器。Gateway/WebSocket 与外部渠道也复用同一套处理逻辑；大多数命令本地执行，不消耗 LLM token，只有 `/learn` 和 `/compact` 会转交 runtime 继续处理。

| 命令 | 说明 |
|------|------|
| `/help` | 显示帮助 |
| `/tasks [status]` | 查看后台任务，可按 `running` / `completed` / `failed` / `cancelled` 过滤 |
| `/tools` | 列出已加载工具 |
| `/skills` | 列出已加载技能 |
| `/learn <描述>` | 让 Agent 学习一个新技能，会调用 LLM |
| `/clear` | 清空当前会话历史 |
| `/compact` | 手动触发会话历史压缩 |
| `/clear-skills` | 清理技能学习/进化记录 |
| `/forget-skill <名称>` | 删除指定技能的学习/进化记录 |
| `/session-metrics [--json|--reset|--layer N]` | 查看或重置 7 层记忆系统指标 |
| `/log status|level <LEVEL>|filter <TEXT>|console on/off|file on/off|clear` | 动态控制日志系统 |
| `/exit` 或 `/quit` | 退出 |

`/quit` 和 `/exit` 只在 CLI 模式可用；在外部渠道里会返回不可用提示。命令名必须完整匹配，例如 `/skills now` 不会误触发 `/skills`。

---

## gateway — 启动网关守护进程

```
blockcell gateway [OPTIONS]
```

启动 HTTP/WebSocket 网关服务，同时接入所有已配置渠道（Telegram、飞书、钉钉等）。

| 选项 | 短写 | 默认值 | 说明 |
|------|------|--------|------|
| `--port <PORT>` | `-p` | 18790 | API 监听端口（覆盖配置中的 `gateway.port`） |
| `--host <HOST>` | — | `localhost` | 绑定地址（覆盖配置中的 `gateway.host`） |

**示例：**
```bash
blockcell gateway
blockcell gateway --port 8080 --host 127.0.0.1
```

**网关 API 端点：**

| 端点 | 说明 |
|------|------|
| `POST /v1/chat` | 发送消息 |
| `GET  /v1/health` | 健康检查（不需要认证） |
| `GET  /v1/tasks` | 列出后台任务 |
| `GET  /v1/ws` | WebSocket 连接 |
| `GET  /v1/channels/status` | 查看渠道连接状态 |
| `GET  /v1/channel-owners` | 查看渠道 owner 绑定 |

---

## mcp — 管理 MCP 服务器

```
blockcell mcp <SUBCOMMAND>
```

用于管理独立的 MCP 配置文件：`~/.blockcell/mcp.json` 与 `~/.blockcell/mcp.d/*.json`。

| 子命令 | 说明 |
|------|------|
| `list` | 列出全部 MCP server |
| `show <NAME>` | 查看某个 MCP server 的合并后配置 |
| `add <TEMPLATE>` | 从模板添加 MCP server，如 `github`、`sqlite`、`filesystem`、`postgres`、`puppeteer` |
| `add <NAME> --raw ...` | 直接按底层 `command/args/env/cwd` 写入配置 |
| `enable <NAME>` | 启用某个 MCP server |
| `disable <NAME>` | 禁用某个 MCP server |
| `remove <NAME>` | 删除某个 MCP server 配置 |
| `edit [NAME]` | 打开 `mcp.json` 或某个 `mcp.d/<name>.json` |

**示例：**
```bash
blockcell mcp list
blockcell mcp add github
blockcell mcp add sqlite --db-path /tmp/notes.db
blockcell mcp add custom --raw --name custom --command uvx --arg my-mcp-server
blockcell mcp disable github
```

**说明：** MCP 配置文件是独立的 `mcp.json` / `mcp.d/*.json`（当前为严格 JSON，不是 JSON5）；配置变更默认在重启 `blockcell agent` 或 `blockcell gateway` 后生效。

---

## 后台事件与主动摘要（Phase 1）

当前版本没有单独的 `system-event` CLI 子命令，但 `agent` / `gateway` 运行时已经默认启用后台事件编排：

- `TaskManager` 会为后台子任务发出结构化事件
- `CronService` 会为定时任务派发发出结构化事件
- `AgentRuntime` 的 tick 会把这些事件聚合为：
  - 即时通知（如关键失败）
  - 主会话摘要（如任务完成、定时任务成功派发）

使用要点：

- 生效入口是 `blockcell agent` / `blockcell gateway` 进程本身，不需要额外命令开关
- 升级到包含该特性的版本后，**重启进程即可生效**
- 当前只支持每个 agent 的“最近活跃主会话”
- 当前只做进程内内存聚合；进程重启后，未发出的摘要不会保留
- 当前只接入 `TaskManager` 与 `CronService`，Ghost 还没有接入这一轮事件生产

---

## status — 查看状态

```
blockcell status
```

显示当前配置状态（provider、模型、API key 是否配置、agent → intent profile、渠道 owner 等）。

---

## doctor — 环境诊断

```
blockcell doctor
```

检查运行环境，包括依赖工具（ffmpeg、chrome、python3 等）是否安装、API key 是否有效、`intentRouter` / `channelOwners` / `channelAccountOwners` 是否通过校验等。

---

## config — 管理配置

```
blockcell config <SUBCOMMAND>
```

### config show

显示当前完整配置（以 pretty-printed JSON 形式输出；配置文件本身可写为 JSON5）。

```bash
blockcell config show
```

### config schema

打印配置文件的 JSON Schema。

```bash
blockcell config schema
```

### config get

按点分隔路径读取配置项。

```bash
blockcell config get <KEY>
```

**示例：**
```bash
blockcell config get agents.defaults.model
blockcell config get providers.openai.apiKey
blockcell config get network.proxy
```

### config set

按点分隔路径设置配置项（自动识别 JSON5 / 标量类型；解析失败时回退为字符串）。

```bash
blockcell config set <KEY> <VALUE>
```

**示例：**
```bash
blockcell config set agents.defaults.model "deepseek-v4-pro"
blockcell config set network.proxy "http://127.0.0.1:7890"
blockcell config set agents.defaults.maxTokens 4096
```

### config edit

在 `$EDITOR` 中打开配置文件直接编辑。

```bash
blockcell config edit
```

### config providers

显示所有 provider 配置摘要。

```bash
blockcell config providers
```

### config reset

重置为默认配置。

```bash
blockcell config reset [--force]
```

| 选项 | 说明 |
|------|------|
| `--force` | 跳过确认提示 |

---

## tools — 管理工具

```
blockcell tools <SUBCOMMAND>
```

### tools list

```bash
blockcell tools list [--category <NAME>]
```

| 选项 | 说明 |
|------|------|
| `--category <NAME>` | 按分类过滤 |

### tools show / tools info

显示指定工具的详细信息和参数说明。

```bash
blockcell tools show <TOOL_NAME>
blockcell tools info <TOOL_NAME>
```

### tools test

直接调用工具并传入 JSON 参数，绕过 LLM。

```bash
blockcell tools test <TOOL_NAME> '<JSON_PARAMS>'
```

**示例：**
```bash
blockcell tools test finance_api '{"action":"stock_quote","symbol":"600519"}'
blockcell tools test exec '{"command":"echo hello"}'
```

### tools toggle

启用或禁用工具。

```bash
blockcell tools toggle <TOOL_NAME> --enable
blockcell tools toggle <TOOL_NAME> --disable
```

---

## run — 直接执行

```
blockcell run <SUBCOMMAND>
```

### run tool

直接运行工具（与 `tools test` 等价）。支持通过 `--agent/-a` 指定目标 agent。

```bash
blockcell run tool <TOOL_NAME> '<JSON_PARAMS>' [--agent <ID>]
```

| 选项 | 短写 | 默认值 | 说明 |
|------|------|--------|------|
| `--agent <ID>` | `-a` | `default` | 指定运行的 agent |

**示例：**
```bash
blockcell run tool read_file '{"path":"README.md"}' -a ops
```

### run msg

通过 Agent 发送消息（等同于 `agent -m`）。支持通过 `--agent/-a` 指定目标 agent。

```bash
blockcell run msg <MESSAGE> [--session <ID>] [--agent <ID>]
```

| 选项 | 短写 | 默认值 | 说明 |
|------|------|--------|------|
| `--session <ID>` | `-s` | `cli:run` | 会话 ID |
| `--agent <ID>` | `-a` | `default` | 指定运行的 agent |

**示例：**
```bash
blockcell run msg "你好" -a ops
```

---

## channels — 管理渠道

```
blockcell channels <SUBCOMMAND>
```

| 子命令 | 说明 |
|--------|------|
| `status` | 显示所有渠道连接状态 |
| `login <CHANNEL>` | 登录指定渠道（当前主要用于 WhatsApp 和 Weixin 扫码登录） |
| `owner list` | 列出渠道 fallback owner 与账号级 owner 覆盖 |
| `owner set --channel <NAME> [--account <ACCOUNT_ID>] --agent <ID>` | 设置渠道或账号 owner |
| `owner clear --channel <NAME> [--account <ACCOUNT_ID>]` | 清除渠道或账号 owner |

**示例：**
```bash
blockcell channels owner set --channel telegram --agent default
blockcell channels owner set --channel telegram --account bot2 --agent ops
blockcell channels owner clear --channel telegram --account bot2
```

---

## cron — 查看定时任务

```
blockcell cron <SUBCOMMAND>
```

### cron list

```bash
blockcell cron list [--all] [--agent <ID>]
```

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--all` | false | 显示所有任务，包括已禁用的 |
| `--agent <ID>` | `default` | 指定要查看的 agent |

> 当前 CLI 只提供查看入口；创建、暂停、恢复、删除等操作需要通过 WebUI 或对话/工具链完成。

---

## skills — 管理技能

```
blockcell skills <SUBCOMMAND>
```

可用别名：`blockcell skill`

### skills list

```bash
blockcell skills list [--all] [--enabled]
```

| 选项 | 说明 |
|------|------|
| `--all` | 显示所有记录，包括内置工具错误 |
| `--enabled` | 仅显示已启用的技能 |

### skills show

```bash
blockcell skills show <NAME>
```

### skills enable / disable

```bash
blockcell skills enable <NAME>
blockcell skills disable <NAME>
```

### skills reload

从磁盘热重载所有技能（无需重启）。

```bash
blockcell skills reload
```

### skills learn

通过描述让 Agent 学习一个新技能。

```bash
blockcell skills learn <DESCRIPTION>
```

**示例：**
```bash
blockcell skills learn "增加网页翻译功能，支持中英互译"
```

### skills install

从社区 Hub 安装技能。

```bash
blockcell skills install <NAME> [--version <VERSION>]
```

### skills test

测试单个技能目录。

```bash
blockcell skills test <PATH> [-i <INPUT>] [-v]
```

| 选项 | 短写 | 说明 |
|------|------|------|
| `--input <TEXT>` | `-i` | 注入到 `user_input` 变量的模拟输入 |
| `--verbose` | `-v` | 显示脚本日志和详细 meta.yaml 输出 |

**示例：**
```bash
blockcell skills test ./skills/stock_monitor -i "查询茅台股价" -v
```

### skills test-all

批量测试一个目录下的所有技能。

```bash
blockcell skills test-all <DIR> [-i <INPUT>] [-v]
```

### skills clear

清除所有技能进化记录。

```bash
blockcell skills clear
```

### skills forget

删除指定技能的进化记录。

```bash
blockcell skills forget <NAME>
```

---

## evolve — 技能进化

```
blockcell evolve <SUBCOMMAND>
```

### evolve run

触发一次新的技能进化。

```bash
blockcell evolve run <DESCRIPTION> [-w]
```

| 选项 | 短写 | 说明 |
|------|------|------|
| `--watch` | `-w` | 触发后持续观察进度 |

**示例：**
```bash
blockcell evolve run "增加网页翻译功能" --watch
```

### evolve trigger

手动触发指定技能的进化（`evolve run` 的别名）。

```bash
blockcell evolve trigger <SKILL_NAME> [--reason <TEXT>]
```

### evolve list

```bash
blockcell evolve list [--all] [-v]
```

| 选项 | 短写 | 说明 |
|------|------|------|
| `--all` | — | 显示所有记录，包括内置工具错误 |
| `--verbose` | `-v` | 显示详细信息（补丁内容、审计、测试结果） |

### evolve show / status

```bash
blockcell evolve show <SKILL_NAME>
blockcell evolve status [<EVOLUTION_ID>]
```

### evolve watch

实时观察进化进度。

```bash
blockcell evolve watch [<EVOLUTION_ID>]
```

不指定 ID 则观察所有进行中的进化。

### evolve rollback

回滚技能到历史版本。

```bash
blockcell evolve rollback <SKILL_NAME> [--to <VERSION>]
```

**示例：**
```bash
blockcell evolve rollback stock_monitor --to v2
```

---

## memory — 管理记忆

```
blockcell memory <SUBCOMMAND>
```

### memory list

```bash
blockcell memory list [--type <TYPE>] [--limit <N>]
```

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--type <TYPE>` | — | 按类型过滤（fact / preference / project / task / note 等） |
| `--limit <N>` | 20 | 最大返回条数 |

### memory show

```bash
blockcell memory show <ID>
```

### memory delete

```bash
blockcell memory delete <ID>
```

### memory stats

显示记忆库统计信息（条目数、分类分布等）。

```bash
blockcell memory stats
```

### memory search

```bash
blockcell memory search <QUERY> [--scope <SCOPE>] [--type <TYPE>] [--top <N>]
```

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--scope <SCOPE>` | — | 按范围过滤（`short_term` / `long_term`） |
| `--type <TYPE>` | — | 按类型过滤 |
| `--top <N>` | 10 | 最大返回条数 |

### memory clear

软删除记忆（可恢复）。

```bash
blockcell memory clear [--scope <SCOPE>]
```

### memory maintenance

清理过期记忆并清空回收站。

```bash
blockcell memory maintenance [--recycle-days <DAYS>]
```

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--recycle-days <DAYS>` | 30 | 软删除记录的保留天数 |

### memory retry-vector-sync

重试队列中的向量索引同步操作。

```bash
blockcell memory retry-vector-sync [--limit <N>]
```

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--limit <N>` | 100 | 单次重放的最大待处理操作数 |

### memory reindex

从当前 SQLite 活跃记录重建向量索引。

```bash
blockcell memory reindex
```

---

## alerts — 管理告警规则

```
blockcell alerts <SUBCOMMAND>
```

### alerts list

```bash
blockcell alerts list
```

### alerts add

添加一条告警规则。

```bash
blockcell alerts add --name <NAME> --source <SOURCE> --field <FIELD> \
  --operator <OP> --threshold <VALUE>
```

| 选项 | 说明 |
|------|------|
| `--name <NAME>` | 规则名称 |
| `--source <SOURCE>` | 数据源，**JSON 工具调用对象**，必须含字符串 `tool` 字段（如 `{"tool":"system_info","params":{"category":"hardware"}}`） |
| `--field <FIELD>` | 监控字段路径（如 `price`、`change_pct`） |
| `--operator <OP>` | 比较运算符：`gt` / `lt` / `gte` / `lte` / `eq` / `ne` / `change_pct` / `cross_above` / `cross_below` |
| `--threshold <VALUE>` | 阈值（必须是数字） |

**示例：**
```bash
blockcell alerts add --name "茅台跌停预警" \
  --source '{"tool":"finance_api","params":{"action":"stock_quote","symbol":"600519"}}' \
  --field "change_pct" \
  --operator lt \
  --threshold "-9.5"
```

### alerts remove

```bash
blockcell alerts remove <RULE_ID>
```

支持 ID 前缀匹配。

### alerts evaluate

立即评估所有告警规则。

```bash
blockcell alerts evaluate
```

### alerts history

```bash
blockcell alerts history [--limit <N>]
```

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--limit <N>` | 20 | 最大显示条数 |

---

## streams — 管理数据流订阅

```
blockcell streams <SUBCOMMAND>
```

| 子命令 | 说明 |
|--------|------|
| `list` | 列出所有订阅 |
| `status <SUB_ID>` | 查看指定订阅详情（支持 ID 前缀） |
| `stop <SUB_ID>` | 停止并移除指定订阅（支持 ID 前缀） |
| `unsubscribe <SUB_ID>` | `stop` 的别名 |
| `restore` | 显示可恢复的订阅列表 |

---

## knowledge — 管理知识图谱

```
blockcell knowledge <SUBCOMMAND>
```

### knowledge stats

```bash
blockcell knowledge stats [--graph <NAME>]
```

### knowledge search

```bash
blockcell knowledge search <QUERY> [--graph <NAME>] [--limit <N>]
```

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--graph <NAME>` | `default` | 图谱名称 |
| `--limit <N>` | 20 | 最大返回条数 |

### knowledge export

```bash
blockcell knowledge export [--format <FORMAT>] [--graph <NAME>] [--output <FILE>]
```

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--format <FORMAT>` | `json` | 输出格式：`json` / `dot` / `mermaid` |
| `--graph <NAME>` | `default` | 图谱名称 |
| `--output <FILE>` | — | 输出文件路径（不指定则打印到 stdout） |

### knowledge list-graphs

```bash
blockcell knowledge list-graphs
```

---

## upgrade — 升级管理

```
blockcell upgrade [--check]
blockcell upgrade <SUBCOMMAND>
```

| 子命令 | 说明 |
|--------|------|
| `check`（默认） | 检查是否有可用更新 |
| `download` | 下载可用更新 |
| `apply` | 应用已下载的更新 |
| `rollback [--to <VERSION>]` | 回滚到上一版本或指定版本 |
| `status` | 显示升级状态 |

**示例：**
```bash
blockcell upgrade --check      # 检查更新
blockcell upgrade              # 同上（默认行为）
blockcell upgrade download     # 下载
blockcell upgrade apply        # 应用
blockcell upgrade rollback     # 回滚到上一版本
blockcell upgrade rollback --to v0.9.0
```

---

## logs — 查看日志

```
blockcell logs <SUBCOMMAND>
```

### logs show

```bash
blockcell logs show [--lines <N>] [-n <N>] [--filter <KEYWORD>] [--session <ID>]
```

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--lines <N>` | 50 | 显示最近 N 行 |
| `-n <N>` | — | `--lines` 的短写 |
| `--filter <KEYWORD>` | — | 按关键词过滤（如 `evolution`、`ghost`、`tool`） |
| `--session <ID>` | — | 按会话 ID 过滤 |

### logs follow

实时追踪日志（类似 `tail -f`）。

```bash
blockcell logs follow [--filter <KEYWORD>] [--session <ID>]
```

### logs clear

```bash
blockcell logs clear [--force]
```

| 选项 | 说明 |
|------|------|
| `--force` | 跳过确认提示 |

---

## audit — 验证审计日志完整性

```
blockcell audit verify [--date <YYYY-MM-DD>] [--all]
```

审计日志写入 `~/.blockcell/audit/<date>.jsonl`，新记录使用 SHA-256 hash chain 包装。`verify` 会重算每条记录的 hash，并检查相邻记录的 `prev_hash` 链接。

| 选项 | 默认值 | 说明 |
|------|--------|------|
| `--date <YYYY-MM-DD>` | 今天 | 验证指定日期的审计日志 |
| `--all` | false | 按文件名顺序验证全部 `.jsonl` 审计日志 |

**示例：**
```bash
blockcell audit verify
blockcell audit verify --date 2026-06-20
blockcell audit verify --all
```

**边界：** 当前默认每天独立成链，可检测单日文件内记录被修改、插入或删除中间记录；删除整天文件、或截断某天文件末尾若干条，剩余链仍可能自洽。安全敏感部署应定期外发/异地留存每日末尾 hash。

---

## completions — 生成 Shell 补全脚本

```
blockcell completions <SHELL>
```

支持的 Shell：`bash`、`zsh`、`fish`、`powershell`、`elvish`

**安装示例（zsh）：**
```bash
blockcell completions zsh > ~/.zfunc/_blockcell
# 确保 ~/.zshrc 中有: fpath=(~/.zfunc $fpath) && autoload -U compinit && compinit
```

**安装示例（bash）：**
```bash
blockcell completions bash > /etc/bash_completion.d/blockcell
```
