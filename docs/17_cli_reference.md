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

## onboard — 初始化配置

```
blockcell onboard [OPTIONS]
```

首次使用时运行，创建配置文件和工作区目录。

| 选项 | 说明 |
|------|------|
| `--force` | 强制覆盖已有配置 |
| `--provider <NAME>` | 指定 LLM provider（如 deepseek、openai、kimi、anthropic） |
| `--api-key <KEY>` | 指定 provider 的 API key |
| `--model <MODEL>` | 指定模型名（如 deepseek-chat、moonshot-v1-8k） |
| `--channels-only` | 仅更新渠道配置，跳过 provider 设置 |

**示例：**
```bash
# 交互式向导（默认）
blockcell onboard

# 非交互式，直接指定 provider
blockcell onboard --provider deepseek --api-key sk-xxx --model deepseek-chat

# 仅重新配置渠道
blockcell onboard --channels-only
```

---

## agent — 运行 Agent

```
blockcell agent [OPTIONS]
```

启动 Agent 会话。不带 `--message` 时进入交互模式。

| 选项 | 短写 | 默认值 | 说明 |
|------|------|--------|------|
| `--message <TEXT>` | `-m` | — | 发送单条消息后退出 |
| `--session <ID>` | `-s` | `cli:default` | 会话 ID |
| `--model <MODEL>` | — | — | 临时覆盖 LLM 模型 |
| `--provider <NAME>` | — | — | 临时覆盖 LLM provider |

**示例：**
```bash
# 进入交互模式
blockcell agent

# 发送单条消息
blockcell agent -m "帮我查询 BTC 价格"

# 指定会话 ID（便于管理多个会话）
blockcell agent -s work:finance

# 临时使用不同模型
blockcell agent --model gpt-4o --provider openai
```

**交互模式内置命令：**

在交互模式中，以 `/` 开头的输入为内置命令：

| 命令 | 说明 |
|------|------|
| `/help` | 显示帮助 |
| `/tools` | 列出已加载工具 |
| `/skills` | 列出已加载技能 |
| `/status` | 显示系统状态 |
| `/clear` | 清空当前会话历史 |
| `/exit` 或 `/quit` | 退出 |

---

## gateway — 启动网关守护进程

```
blockcell gateway [OPTIONS]
```

启动 HTTP/WebSocket 网关服务，同时接入所有已配置渠道（Telegram、飞书、钉钉等）。

| 选项 | 短写 | 默认值 | 说明 |
|------|------|--------|------|
| `--port <PORT>` | `-p` | 18790 | API 监听端口（覆盖配置中的 `gateway.port`） |
| `--host <HOST>` | — | `0.0.0.0` | 绑定地址（覆盖配置中的 `gateway.host`） |

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

---

## status — 查看状态

```
blockcell status
```

显示当前配置状态（provider、模型、API key 是否配置、渠道状态等）。

---

## doctor — 环境诊断

```
blockcell doctor
```

检查运行环境，包括依赖工具（ffmpeg、chrome、python3 等）是否安装、API key 是否有效等。

---

## config — 管理配置

```
blockcell config <SUBCOMMAND>
```

### config show

显示当前完整配置（JSON 格式）。

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

按点分隔路径设置配置项（自动识别 JSON 类型）。

```bash
blockcell config set <KEY> <VALUE>
```

**示例：**
```bash
blockcell config set agents.defaults.model "deepseek-chat"
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

直接运行工具（与 `tools test` 等价）。

```bash
blockcell run tool <TOOL_NAME> '<JSON_PARAMS>'
```

### run msg

通过 Agent 发送消息（等同于 `agent -m`）。

```bash
blockcell run msg <MESSAGE> [--session <ID>]
```

| 选项 | 短写 | 默认值 | 说明 |
|------|------|--------|------|
| `--session <ID>` | `-s` | `cli:run` | 会话 ID |

---

## tasks — 管理后台任务

```
blockcell tasks <SUBCOMMAND>
```

| 子命令 | 说明 |
|--------|------|
| `list` | 列出所有后台任务 |
| `show <TASK_ID>` | 显示指定任务详情（支持 ID 前缀匹配） |
| `cancel <TASK_ID>` | 取消运行中的任务（支持 ID 前缀匹配） |

---

## channels — 管理渠道

```
blockcell channels <SUBCOMMAND>
```

| 子命令 | 说明 |
|--------|------|
| `status` | 显示所有渠道连接状态 |
| `login <CHANNEL>` | 登录指定渠道（如 WhatsApp 需扫码） |

---

## cron — 管理定时任务

```
blockcell cron <SUBCOMMAND>
```

### cron list

```bash
blockcell cron list [--all]
```

| 选项 | 说明 |
|------|------|
| `--all` | 显示所有任务，包括已禁用的 |

### cron add

创建定时任务。

```bash
blockcell cron add --name <NAME> --message <TEXT> [调度选项] [投递选项]
```

| 选项 | 说明 |
|------|------|
| `--name <NAME>` | 任务名称（必填） |
| `--message <TEXT>` | 要发送的消息内容（必填） |
| `--every <SECONDS>` | 每隔 N 秒执行一次 |
| `--cron <EXPR>` | Cron 表达式（如 `"0 9 * * 1-5"`） |
| `--at <ISO_TIME>` | 在指定时间执行一次（ISO 格式） |
| `--deliver` | 将输出投递到渠道 |
| `--to <CHAT_ID>` | 目标 chat ID |
| `--channel <NAME>` | 目标渠道名称 |

**示例：**
```bash
# 每天早上 9 点发送金融日报
blockcell cron add --name daily_report --message "生成今日金融日报" \
  --cron "0 9 * * 1-5" --deliver --channel telegram --to 123456789

# 每隔 60 秒检查一次
blockcell cron add --name check --message "检查系统状态" --every 60
```

### cron pause / resume

```bash
blockcell cron pause <JOB_ID>
blockcell cron resume <JOB_ID>
```

### cron enable

```bash
blockcell cron enable <JOB_ID>          # 启用
blockcell cron enable <JOB_ID> --disable  # 禁用
```

### cron run

立即运行一个定时任务。

```bash
blockcell cron run <JOB_ID> [--force]
```

| 选项 | 说明 |
|------|------|
| `--force` | 强制运行，即使任务已禁用 |

### cron remove

```bash
blockcell cron remove <JOB_ID>
```

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
| `--source <SOURCE>` | 数据源，格式 `tool:action:param`（如 `finance_api:stock_quote:600519`） |
| `--field <FIELD>` | 监控字段（如 `price`、`change_pct`） |
| `--operator <OP>` | 比较运算符：`gt` / `lt` / `gte` / `lte` / `eq` / `ne` / `change_pct` / `cross_above` / `cross_below` |
| `--threshold <VALUE>` | 阈值 |

**示例：**
```bash
blockcell alerts add --name "茅台跌停预警" \
  --source "finance_api:stock_quote:600519" \
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
