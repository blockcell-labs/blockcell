# Article 17: CLI Reference

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — Article 17

This article is a code-aligned reference for the current `blockcell` CLI.

For the latest machine-readable truth, use `blockcell --help` and `blockcell <subcommand> --help`. The sections below describe the current behavior implemented in `bin/blockcell/src/main.rs` and related command handlers.

## Global options

```bash
blockcell [OPTIONS] <COMMAND>
```

| Option | Short | Description |
|------|------|------|
| `--verbose` | `-v` | Enable debug-level logs |
| `--help` | `-h` | Show help |
| `--version` | `-V` | Show version |

---

## `setup` — interactive setup wizard (recommended)

```bash
blockcell setup [OPTIONS]
```

Recommended for first-time users. `setup` walks you through provider setup and optional channel setup, then validates the provider configuration unless you skip that step.

| Option | Description |
|------|------|
| `--force` | Reset existing config to defaults before starting |
| `--provider <NAME>` | Provider name (`deepseek` / `openai` / `kimi` / `anthropic` / `gemini` / `zhipu` / `qwen` / `xai` / `mistral` / `minimax` / `groq` / `siliconflow` / `openrouter` / `ollama`) |
| `--api-key <KEY>` | API key for the selected provider |
| `--model <MODEL>` | Model override |
| `--channel <NAME>` | Optional channel to configure (`telegram` / `feishu` / `wecom` / `dingtalk` / `lark` / `qq` / `napcat` / `none`; `skip` is also accepted) |
| `--skip-provider-test` | Skip provider validation after saving |

**Examples:**

```bash
blockcell setup
blockcell setup --provider deepseek --api-key sk-xxx --model deepseek-v4-pro
blockcell setup --provider kimi --api-key sk-xxx --channel telegram
blockcell setup --force
blockcell setup --provider ollama --skip-provider-test
```

Typical flow:

1. Choose a provider, or skip provider setup
2. Enter an API key when needed
3. Confirm or override the model name
4. Optionally configure one message channel
5. Validate the saved provider config unless skipped
6. Auto-bind missing channel owners to the `default` agent when appropriate
7. Print a summary and suggested next steps

---

## `onboard` — classic initialization flow

```bash
blockcell onboard [OPTIONS]
```

Creates the workspace and initial config files. Compared with `setup`, `onboard` is better for scripted deployment or users already familiar with the config layout.

| Option | Description |
|------|------|
| `--force` | Overwrite existing config |
| `--interactive` | Run the interactive wizard mode |
| `--provider <NAME>` | Provider name |
| `--api-key <KEY>` | Provider API key |
| `--model <MODEL>` | Model name |
| `--channels-only` | Only update channel config and skip provider setup |

**Examples:**

```bash
blockcell onboard
blockcell onboard --provider deepseek --api-key sk-xxx --model deepseek-v4-pro
blockcell onboard --channels-only
```

---

## `agent` — run an agent session

```bash
blockcell agent [OPTIONS]
```

Starts an agent session. If `--message` is omitted, blockcell enters interactive CLI mode.

| Option | Short | Default | Description |
|------|------|--------|------|
| `--message <TEXT>` | `-m` | — | Send one message and exit |
| `--agent <ID>` | `-a` | `default` | Target agent ID |
| `--session <ID>` | `-s` | `cli:<agent>` | Session ID |
| `--model <MODEL>` | — | — | Temporary model override |
| `--provider <NAME>` | — | — | Temporary provider override |

**Examples:**

```bash
blockcell agent
blockcell agent --agent ops
blockcell agent -a ops -m "Check BTC price"
blockcell agent -a ops -s work:finance
blockcell agent --agent ops --model gpt-6.1-sol --provider openai
```

Unified slash commands:

In interactive CLI mode, inputs starting with `/` go through the unified slash-command handler first. Gateway/WebSocket and external channels reuse the same handler. Most commands execute locally and do not spend LLM tokens; `/learn` and `/compact` are forwarded to the runtime for further processing.

| Command | Description |
|------|------|
| `/help` | Show help |
| `/tasks [status]` | List background tasks, optionally filtered by `running` / `completed` / `failed` / `cancelled` |
| `/tools` | List loaded tools |
| `/skills` | List loaded skills |
| `/learn <description>` | Ask the Agent to learn a new skill; uses the LLM |
| `/clear` | Clear current conversation history |
| `/compact` | Manually trigger conversation-history compression |
| `/clear-skills` | Clear skill learning/evolution records |
| `/forget-skill <name>` | Delete learning/evolution records for one skill |
| `/session-metrics [--json|--reset|--layer N]` | Show or reset 7-layer memory-system metrics |
| `/log status|level <LEVEL>|filter <TEXT>|console on/off|file on/off|clear` | Control the logging system at runtime |
| `/exit` or `/quit` | Exit |

`/quit` and `/exit` are CLI-only. In external channels they return an unavailable-command message. Command names must match exactly; for example, `/skills now` does not accidentally trigger `/skills`.

---

## `gateway` — start the long-running gateway

```bash
blockcell gateway [OPTIONS]
```

Starts the HTTP / WebSocket gateway and connects all configured channels.

| Option | Short | Default | Description |
|------|------|--------|------|
| `--port <PORT>` | `-p` | `18790` | Override `gateway.port` |
| `--host <HOST>` | — | `localhost` | Override `gateway.host` |

**Examples:**

```bash
blockcell gateway
blockcell gateway --port 8080 --host 127.0.0.1
```

Common gateway endpoints:

| Endpoint | Description |
|------|------|
| `POST /v1/chat` | Send a message |
| `GET /v1/health` | Health check |
| `GET /v1/tasks` | List background tasks |
| `GET /v1/ws` | WebSocket connection |
| `GET /v1/channels/status` | Channel connection status |
| `GET /v1/channel-owners` | Channel owner bindings |

---

## `mcp` — manage MCP servers

```bash
blockcell mcp <SUBCOMMAND>
```

This manages the standalone MCP config files:

- `~/.blockcell/mcp.json`
- `~/.blockcell/mcp.d/*.json`

| Subcommand | Description |
|------|------|
| `list` | List all MCP servers |
| `show <NAME>` | Show the merged config for one server |
| `add <TEMPLATE>` | Add a server from a template such as `github`, `sqlite`, `filesystem`, `postgres`, or `puppeteer` |
| `add <NAME> --raw ...` | Write a server directly from low-level `command` / `args` / `env` / `cwd` fields |
| `enable <NAME>` | Enable a server |
| `disable <NAME>` | Disable a server |
| `remove <NAME>` | Remove a server config |
| `edit [NAME]` | Open `mcp.json` or `mcp.d/<name>.json` |

**Examples:**

```bash
blockcell mcp list
blockcell mcp add github
blockcell mcp add sqlite --db-path /tmp/notes.db
blockcell mcp add custom --raw --name custom --command uvx --arg my-mcp-server
blockcell mcp disable github
```

Important runtime details:

- MCP config is **standalone**, not embedded under `config.json5`
- MCP files are currently **strict JSON**, not JSON5
- Changes take effect after restarting `blockcell agent` or `blockcell gateway`
- The generated GitHub template currently writes a literal `${env:GITHUB_PERSONAL_ACCESS_TOKEN}` placeholder; blockcell does **not** expand that placeholder at runtime, so replace it manually or remove the key and rely on inherited process environment variables

---

## Background events and proactive summaries (Phase 1)

There is currently **no dedicated `system-event` CLI subcommand**, but background event orchestration is already enabled inside normal `agent` and `gateway` runtime processes.

Current behavior:

- `TaskManager` emits structured events for background subtasks
- `CronService` emits structured events for scheduled work
- `AgentRuntime` aggregates those events into:
  - immediate notifications for important failures
  - summaries pushed into the main active session for noteworthy completions

Current limits:

- no extra flag is required; the behavior lives inside the runtime process itself
- upgrading to a build with this feature and **restarting the process** is enough
- summaries are currently scoped to the most recently active main session per agent
- aggregation is currently in-memory only, so unsent summaries are lost on process restart
- this round wires in `TaskManager` and `CronService`; Ghost is not yet an event producer for this pipeline

---

## `status` — show current status

```bash
blockcell status
```

Shows the current configuration and runtime-oriented status such as provider readiness, active model selection, agent-to-intent-profile mapping, and channel owner bindings.

---

## `doctor` — run environment diagnostics

```bash
blockcell doctor
```

Checks the runtime environment, external dependencies, key configuration validity, and several config-level validations such as `intentRouter`, `channelOwners`, and `channelAccountOwners`.

---

## `config` — manage configuration

```bash
blockcell config <SUBCOMMAND>
```

### `config show`

Print the current full config as pretty JSON. The file on disk itself is `config.json5`, so comments and trailing commas are supported there even though `config show` prints JSON.

```bash
blockcell config show
```

### `config schema`

Print the config JSON Schema.

```bash
blockcell config schema
```

### `config get`

Read a config value by dot-separated path.

```bash
blockcell config get <KEY>
```

**Examples:**

```bash
blockcell config get agents.defaults.model
blockcell config get providers.openai.apiKey
blockcell config get network.proxy
```

### `config set`

Set a config value by dot-separated path. Values are parsed as JSON5 / scalar values when possible, and fall back to a plain string if parsing fails.

```bash
blockcell config set <KEY> <VALUE>
```

**Examples:**

```bash
blockcell config set agents.defaults.model "deepseek-v4-pro"
blockcell config set network.proxy "http://127.0.0.1:7890"
blockcell config set agents.defaults.maxTokens 4096
blockcell config set network.noProxy "['localhost', '127.0.0.1']"
```

### `config edit`

Open `config.json5` in `$EDITOR`.

```bash
blockcell config edit
```

### `config providers`

Show a summary of all provider configs.

```bash
blockcell config providers
```

### `config reset`

Reset to the default config.

```bash
blockcell config reset [--force]
```

| Option | Description |
|------|------|
| `--force` | Skip confirmation |

---

## `tools` — manage tools

```bash
blockcell tools <SUBCOMMAND>
```

### `tools list`

```bash
blockcell tools list [--category <NAME>]
```

| Option | Description |
|------|------|
| `--category <NAME>` | Filter by tool category |

### `tools show` / `tools info`

Show detailed metadata and parameter info for one tool.

```bash
blockcell tools show <TOOL_NAME>
blockcell tools info <TOOL_NAME>
```

### `tools test`

Call a tool directly with JSON parameters, bypassing the LLM.

```bash
blockcell tools test <TOOL_NAME> '<JSON_PARAMS>'
```

**Examples:**

```bash
blockcell tools test finance_api '{"action":"stock_quote","symbol":"600519"}'
blockcell tools test exec '{"command":"echo hello"}'
```

### `tools toggle`

Enable or disable a tool.

```bash
blockcell tools toggle <TOOL_NAME> --enable
blockcell tools toggle <TOOL_NAME> --disable
```

---

## `run` — direct execution shortcuts

```bash
blockcell run <SUBCOMMAND>
```

### `run tool`

Run a tool directly. This is effectively the same idea as `tools test`, with optional agent selection.

```bash
blockcell run tool <TOOL_NAME> '<JSON_PARAMS>' [--agent <ID>]
```

| Option | Short | Default | Description |
|------|------|--------|------|
| `--agent <ID>` | `-a` | `default` | Target agent ID |

### `run msg`

Send a message through the agent runtime. This is a shortcut for `agent -m`.

```bash
blockcell run msg <MESSAGE> [--session <ID>] [--agent <ID>]
```

| Option | Short | Default | Description |
|------|------|--------|------|
| `--session <ID>` | `-s` | `cli:run` | Session ID |
| `--agent <ID>` | `-a` | `default` | Target agent ID |

**Example:**

```bash
blockcell run msg "Hello" -a ops
```

---

## `channels` — manage channels

```bash
blockcell channels <SUBCOMMAND>
```

| Subcommand | Description |
|------|------|
| `status` | Show channel connection status |
| `login <CHANNEL>` | Log in to a channel, currently used mainly for WhatsApp and Weixin QR login |
| `owner list` | List fallback owners and account-level owner overrides |
| `owner set --channel <NAME> [--account <ACCOUNT_ID>] --agent <ID>` | Set the owner agent for a channel or account |
| `owner clear --channel <NAME> [--account <ACCOUNT_ID>]` | Clear the owner binding |

**Examples:**

```bash
blockcell channels owner set --channel telegram --agent default
blockcell channels owner set --channel telegram --account bot2 --agent ops
blockcell channels owner clear --channel telegram --account bot2
```

---

## `cron` — view scheduled jobs

```bash
blockcell cron <SUBCOMMAND>
```

### `cron list`

```bash
blockcell cron list [--all] [--agent <ID>]
```

| Option | Description |
|------|------|
| `--all` | Include disabled jobs |
| `--agent <ID>` | Query one agent; defaults to `default` |

The current CLI is read-only for Cron. Create, pause, resume, run, or delete jobs through the WebUI or the conversational/tool workflow.

---

## `skills` — manage skills

```bash
blockcell skills <SUBCOMMAND>
```

Alias: `blockcell skill`

### `skills list`

```bash
blockcell skills list [--all] [--enabled]
```

| Option | Description |
|------|------|
| `--all` | Include all records, including built-in tool errors |
| `--enabled` | Show enabled skills only |

### `skills show`

```bash
blockcell skills show <NAME>
```

### `skills enable` / `skills disable`

```bash
blockcell skills enable <NAME>
blockcell skills disable <NAME>
```

### `skills reload`

Hot-reload all skills from disk.

```bash
blockcell skills reload
```

### `skills learn`

Ask blockcell to learn a new skill from a plain-language description.

```bash
blockcell skills learn <DESCRIPTION>
```

**Example:**

```bash
blockcell skills learn "Add webpage translation with Chinese-English support"
```

### `skills install`

Install a skill from the Community Hub.

```bash
blockcell skills install <NAME> [--version <VERSION>]
```

### `skills test`

Test one skill directory.

```bash
blockcell skills test <PATH> [-i <INPUT>] [-v]
```

| Option | Short | Description |
|------|------|------|
| `--input <TEXT>` | `-i` | Simulated input injected into `user_input` |
| `--verbose` | `-v` | Show script logs and verbose metadata output |

### `skills test-all`

Batch-test all skills under one directory.

```bash
blockcell skills test-all <DIR> [-i <INPUT>] [-v]
```

### `skills clear`

Clear all skill evolution records.

```bash
blockcell skills clear
```

### `skills forget`

Delete records for one skill.

```bash
blockcell skills forget <NAME>
```

---

## `evolve` — skill evolution

```bash
blockcell evolve <SUBCOMMAND>
```

### `evolve run`

Trigger a new evolution.

```bash
blockcell evolve run <DESCRIPTION> [-w]
```

| Option | Short | Description |
|------|------|------|
| `--watch` | `-w` | Keep watching progress after triggering |

### `evolve trigger`

Manually trigger evolution for a named skill.

```bash
blockcell evolve trigger <SKILL_NAME> [--reason <TEXT>]
```

### `evolve list`

```bash
blockcell evolve list [--all] [-v]
```

| Option | Short | Description |
|------|------|------|
| `--all` | — | Include all records, including built-in tool errors |
| `--verbose` | `-v` | Show detailed patch, audit, and test information |

### `evolve show` / `evolve status`

```bash
blockcell evolve show <SKILL_NAME>
blockcell evolve status [<EVOLUTION_ID>]
```

### `evolve watch`

Watch evolution progress in real time.

```bash
blockcell evolve watch [<EVOLUTION_ID>]
```

If no ID is passed, blockcell watches all ongoing evolutions.

### `evolve rollback`

Rollback a skill to a previous version.

```bash
blockcell evolve rollback <SKILL_NAME> [--to <VERSION>]
```

---

## `memory` — manage memory

```bash
blockcell memory <SUBCOMMAND>
```

### `memory list`

```bash
blockcell memory list [--type <TYPE>] [--limit <N>]
```

| Option | Default | Description |
|------|--------|------|
| `--type <TYPE>` | — | Filter by memory type such as `fact`, `preference`, `project`, `task`, or `note` |
| `--limit <N>` | `20` | Max results |

### `memory show`

```bash
blockcell memory show <ID>
```

### `memory delete`

```bash
blockcell memory delete <ID>
```

### `memory stats`

```bash
blockcell memory stats
```

### `memory search`

```bash
blockcell memory search <QUERY> [--scope <SCOPE>] [--type <TYPE>] [--top <N>]
```

| Option | Default | Description |
|------|--------|------|
| `--scope <SCOPE>` | — | Filter by `short_term` or `long_term` |
| `--type <TYPE>` | — | Filter by memory type |
| `--top <N>` | `10` | Max results |

### `memory clear`

Soft-delete memory items.

```bash
blockcell memory clear [--scope <SCOPE>]
```

### `memory maintenance`

Clean expired memory and purge the recycle bin.

```bash
blockcell memory maintenance [--recycle-days <DAYS>]
```

| Option | Default | Description |
|------|--------|------|
| `--recycle-days <DAYS>` | `30` | Retention window for soft-deleted items |

### `memory retry-vector-sync`

Retry pending vector-index sync operations.

```bash
blockcell memory retry-vector-sync [--limit <N>]
```

### `memory reindex`

Rebuild the optional vector index from SQLite memory items.

```bash
blockcell memory reindex
```

---

## `alerts` — manage alert rules

```bash
blockcell alerts <SUBCOMMAND>
```

### `alerts list`

```bash
blockcell alerts list
```

### `alerts add`

Add an alert rule.

```bash
blockcell alerts add --name <NAME> --source <SOURCE> --field <FIELD> \
  --operator <OP> --threshold <VALUE>
```

| Option | Description |
|------|------|
| `--name <NAME>` | Rule name |
| `--source <SOURCE>` | Data source string |
| `--field <FIELD>` | Field to monitor |
| `--operator <OP>` | `gt` / `lt` / `gte` / `lte` / `eq` / `ne` / `change_pct` / `cross_above` / `cross_below` |
| `--threshold <VALUE>` | Threshold value |

### `alerts remove`

```bash
blockcell alerts remove <RULE_ID>
```

Prefix matching is supported.

### `alerts evaluate`

```bash
blockcell alerts evaluate
```

### `alerts history`

```bash
blockcell alerts history [--limit <N>]
```

| Option | Default | Description |
|------|--------|------|
| `--limit <N>` | `20` | Max entries |

---

## `streams` — manage real-time subscriptions

```bash
blockcell streams <SUBCOMMAND>
```

| Subcommand | Description |
|------|------|
| `list` | List subscriptions |
| `status <SUB_ID>` | Show one subscription by ID prefix |
| `stop <SUB_ID>` | Stop and remove a subscription |
| `unsubscribe <SUB_ID>` | Alias for `stop` |
| `restore` | Show restorable subscriptions |

---

## `knowledge` — manage knowledge graphs

```bash
blockcell knowledge <SUBCOMMAND>
```

### `knowledge stats`

```bash
blockcell knowledge stats [--graph <NAME>]
```

### `knowledge search`

```bash
blockcell knowledge search <QUERY> [--graph <NAME>] [--limit <N>]
```

| Option | Default | Description |
|------|--------|------|
| `--graph <NAME>` | `default` | Graph name |
| `--limit <N>` | `20` | Max results |

### `knowledge export`

```bash
blockcell knowledge export [--format <FORMAT>] [--graph <NAME>] [--output <FILE>]
```

| Option | Default | Description |
|------|--------|------|
| `--format <FORMAT>` | `json` | `json` / `dot` / `mermaid` |
| `--graph <NAME>` | `default` | Graph name |
| `--output <FILE>` | — | Output file path; prints to stdout if omitted |

### `knowledge list-graphs`

```bash
blockcell knowledge list-graphs
```

---

## `upgrade` — update management

```bash
blockcell upgrade [--check]
blockcell upgrade <SUBCOMMAND>
```

| Subcommand | Description |
|------|------|
| `check` | Check for updates |
| `download` | Download an available update |
| `apply` | Apply a downloaded update |
| `rollback [--to <VERSION>]` | Roll back to the previous version or a specific version |
| `status` | Show upgrade status |

**Examples:**

```bash
blockcell upgrade --check
blockcell upgrade
blockcell upgrade download
blockcell upgrade apply
blockcell upgrade rollback
blockcell upgrade rollback --to v0.9.0
```

---

## `logs` — view logs

```bash
blockcell logs <SUBCOMMAND>
```

### `logs show`

```bash
blockcell logs show [--lines <N>] [-n <N>] [--filter <KEYWORD>] [--session <ID>]
```

| Option | Default | Description |
|------|--------|------|
| `--lines <N>` | `50` | Show the most recent N lines |
| `-n <N>` | — | Alias for `--lines` |
| `--filter <KEYWORD>` | — | Filter by keyword such as `evolution`, `ghost`, or `tool` |
| `--session <ID>` | — | Filter by session ID |

### `logs follow`

```bash
blockcell logs follow [--filter <KEYWORD>] [--session <ID>]
```

### `logs clear`

```bash
blockcell logs clear [--force]
```

| Option | Description |
|------|------|
| `--force` | Skip confirmation |

---

## `completions` — shell completion scripts

```bash
blockcell completions <SHELL>
```

Supported shells: `bash`, `zsh`, `fish`, `powershell`, `elvish`

**Example (`zsh`):**

```bash
blockcell completions zsh > ~/.zfunc/_blockcell
# Ensure ~/.zshrc contains:
# fpath=(~/.zfunc $fpath) && autoload -U compinit && compinit
```

**Example (`bash`):**

```bash
blockcell completions bash > /etc/bash_completion.d/blockcell
```

---

*Previous: [Agent2Agent Community (Blockcell Hub)](./16_hub_community.md)*

*Next: [Proxy and Provider Configuration](./18_proxy_and_provider_config.md)*

*Index: [Series directory](./00_index.md)*
