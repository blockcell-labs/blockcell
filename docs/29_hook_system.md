# Hook 生命周期事件系统

> 适合读者：想在工具调用、用户输入或 agent 完成时接入审计、格式化、通知、外部日志系统的用户。

---

## 这个特性解决什么问题

BlockCell 内部已经有系统事件流，用于 WebSocket、任务状态、Cron、记忆维护等模块通信。Hook 系统面向用户配置，解决的是另一类问题：在 agent 生命周期的关键节点执行你自己的 shell 命令。

常见用法：

- `exec` 执行前写入审计日志
- `write_file` 或 `edit_file` 后自动运行 formatter / linter
- 新会话开始时记录一条外部日志
- 用户提交 prompt 时同步到外部追踪系统
- agent 完成一次响应后发送桌面或 IM 通知

Hook 失败不会改变工具调用结果；它是旁路副作用，不是权限系统。权限和阻断应继续使用 `tool_policy.yaml`、路径访问策略或内置确认机制。

---

## 配置文件

Hook 配置文件位于：

```text
~/.blockcell/hooks.yaml
```

最小示例：

```yaml
version: 1

hooks:
  - event: pre_tool_use
    matcher: "exec"
    command: "echo '[AUDIT] exec: {command}' >> ~/blockcell-hook.log"
    timeout: 5

  - event: post_tool_use
    matcher: "write_file|edit_file"
    command: "echo '[WRITE] {file_path}' >> ~/blockcell-hook.log"
    timeout: 5

  - event: session_start
    command: "echo '[{session_id}] session started' >> ~/blockcell-hook.log"
    timeout: 5

  - event: agent_stop
    command: "echo '[{session_id}] agent stopped' >> ~/blockcell-hook.log"
    timeout: 5
```

如果文件不存在或解析失败，HookManager 会使用空配置，agent 正常运行。

---

## 字段说明

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `event` | string | 必填 | 生命周期事件名 |
| `command` | string | 必填 | 通过 shell 执行的命令 |
| `matcher` | string | 空 | 工具名 glob，仅对工具事件有效 |
| `timeout` | number | `30.0` | 单条 hook 超时时间，单位秒 |

`matcher` 支持 glob，也支持用 `|` 分隔多个模式：

```yaml
matcher: "write_file|edit_file|file_ops"
```

---

## 当前已触发的事件

| 事件 | 触发时机 | 常用变量 |
|------|----------|----------|
| `session_start` | 当前持久化会话历史为空，收到第一条消息时 | `{session_id}`、`{cwd}` |
| `user_prompt` | 每次用户消息进入主处理流程时 | `{session_id}`、`{cwd}`、`{result}` |
| `pre_tool_use` | 工具通过禁用、策略、路径检查后，实际执行前 | `{tool_name}`、`{command}`、`{file_path}`、`{path}` |
| `post_tool_use` | 工具执行完成并得到结果后 | `{tool_name}`、`{result}`、`{command}`、`{file_path}`、`{path}` |
| `agent_stop` | 主响应持久化和投递前 | `{session_id}`、`{cwd}`、`{result}` |

事件类型中也保留了 `session_end` 和 `compaction`，但当前 runtime 尚未提供统一触发点。不要依赖它们执行生产逻辑。

---

## 模板变量

Hook 命令支持以下模板变量：

| 变量 | 来源 |
|------|------|
| `{event}` | 当前事件名，如 `post_tool_use` |
| `{tool_name}` | 工具名，如 `exec`、`write_file` |
| `{session_id}` | 当前持久化会话 key |
| `{cwd}` | 当前 workspace 路径 |
| `{command}` | 工具参数中的 `command` 字段 |
| `{file_path}` | 工具参数中的 `file_path` 或 `path` 字段 |
| `{path}` | 工具参数中的 `path` 字段 |
| `{result}` | 工具结果或最终响应，最多 1000 个字符 |

变量会进行 shell-safe quoting。包含空格或单引号的路径、命令参数不会直接拼进 shell。

---

## 常见配置

### 1. `exec` 审计日志

```yaml
hooks:
  - event: pre_tool_use
    matcher: "exec"
    command: "printf '%s\n' '[{session_id}] {command}' >> ~/blockcell-exec-audit.log"
    timeout: 3
```

### 2. 写文件后格式化 Rust

```yaml
hooks:
  - event: post_tool_use
    matcher: "write_file|edit_file"
    command: "case {file_path} in *.rs) cargo fmt --all ;; esac"
    timeout: 20
```

### 3. Python 文件写入后运行 ruff

```yaml
hooks:
  - event: post_tool_use
    matcher: "write_file|edit_file"
    command: "case {file_path} in *.py) ruff check {file_path} --fix || true ;; esac"
    timeout: 20
```

### 4. 用户输入追踪

```yaml
hooks:
  - event: user_prompt
    command: "echo '[{session_id}] prompt received' >> ~/blockcell-prompts.log"
    timeout: 3
```

---

## 执行语义

- Hook 按配置顺序执行。
- 每条 hook 有独立超时。
- Hook 失败、超时或返回非 0 exit code，不会阻断 agent 主流程；失败通过 `hook_execution` 审计事件与
  `warn` 日志观测（见上）。
- `pre_tool_use`（`tool/pre-execute`）在策略/路径/危险确认等准入检查**之前**触发——被这些门禁拒绝的调用仍会先发出 `pre_tool_use`，但不会到达 `post_tool_use`（`tool/result`）。被禁用（toggle off）的调用不会进入准入管线，两种事件都不触发。
- `post_tool_use` 在工具结果产生后触发，因此适合做格式化、审计、通知等副作用。
- 每次 hook 执行都会写一条审计事件 `hook_execution`（字段：`session_key`、`hook_event`、`tool_name`、
  `success`、`timed_out`、`duration_ms`），失败同时打一条 `warn` 日志。
  **命令、参数与输出不写入日志**（可能含密钥），只有结果与耗时。

---

## 安全建议

Hook 命令是用户本机 shell 命令。建议：

- 不要在 hook 中执行不可信输入拼出的破坏性命令。
- 用 `timeout` 限制外部命令耗时。
- 审计类 hook 使用追加日志，不要覆盖重要文件。
- 需要阻断工具调用时使用 `tool_policy.yaml`，不要依赖 hook。

---

## 内部实现位置

| 文件 | 说明 |
|------|------|
| `crates/agent/src/hooks.rs` | Hook 类型、配置解析、匹配、模板展开、命令执行 |
| `crates/agent/src/runtime.rs` | 在用户消息、工具调用前后、agent 停止点触发 hook |
| `crates/agent/src/lib.rs` | 导出 `hooks` 模块 |

---

## 相关文档

- [工具系统](./03_tools_system.md)
- [目录访问安全策略](./22_path_access_policy.md)
- [消息处理与自进化生命周期](./13_message_processing_and_evolution.md)
