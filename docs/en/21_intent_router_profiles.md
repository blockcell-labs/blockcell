# Article 21: Multi-Agent Configuration Guide for `intentRouter`

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — Article 21

`intentRouter` is now the **configuration entry point** for agent tool capabilities in blockcell. If you want different agents to expose different capabilities, keep “which agent gets which profile” separate from “which tools each profile can expose”.

## What to remember first

- Bind each agent with `agents.list[].intentProfile`
- Define reusable tool sets under `intentRouter.profiles`
- Put shared baseline tools in `coreTools`
- Default `toolSelection: "rank"` uses the union of `coreTools` and every `intentTools` group, independently of message intent
- Remove disallowed tools with `denyTools`
- An empty `Chat` entry does not clear tools in static mode; set `toolSelection: "gate"` to retain legacy gating
- Always configure `Unknown`
- Use `enabled: false` + `loadAllTools: true` if you want to disable classification while exposing the full available tool set
- Use `intentRules` when built-in intent rules do not cover your domain vocabulary

> `allowedMcpServers` and `allowedMcpTools` are agent-level MCP visibility allowlists. The JSON field names are camelCase.

## Static capabilities and migration (2026-10-27)

`intentRouter.toolSelection` defaults to `"rank"`. The name leaves room for future ranking hints; schemas currently retain stable presentation order rather than being reordered by intent. Intent still affects interaction prompts, but cannot remove tools.

The static ceiling combines the profile union, the runtime foundation, and MCP tools authorized by the agent allowlists and registry, then removes `denyTools` and unavailable tools. The foundation retains the existing global memory/agent/skill/session/cron/discovery tools, the formerly implicit Coding tools (`read_file`, `write_file`, `edit_file`, `grep`, `shell_run`), `glob`, shell poll/input/close, and the skill-directory execution tools `exec_skill_script` / `exec_local`. Execution policy and confirmation still apply. Narrow custom profiles must deny unwanted foundation tools explicitly. Denying `shell` also denies its four action tools.

This changes behavior for configurations that relied on intent gating. Set `"intentRouter": { "toolSelection": "gate" }` to retain legacy gating and Coding exposure. Both modes enforce profile denies; the compatibility switch does not restore deny bypasses.

Static mode removes the extra Coding schema whitelist. Discoverable tools stay in the authorized catalog until searched. Discovery upgrades exposure, never permissions; searching a light tool replaces its schema immediately in the same round. Other tools reuse the existing full/light mechanism.

Skill declarations are intersected with the agent ceiling. Custom skill and memory-provider tools must be included in the profile unless they are foundation or authorized MCP tools. Ghost additions, persisted activations and legacy shell calls cannot bypass denies. A missing profile fails closed without falling back to global tools.

Tool policies continue to load from `~/.blockcell/tool_policy.yaml`, with deny > ask > allow precedence. Policy checks use hook-modified arguments and apply to both shell aliases and canonical names. Static capability checks and per-invocation policy both apply.

## Key rules

1. `agents.list[].intentProfile` has the highest priority.
2. If an agent does not set `intentProfile`, blockcell falls back to `intentRouter.agentProfiles`.
3. If that still does not resolve, blockcell falls back to `intentRouter.defaultProfile`.
4. If `agents.list` is empty, runtime falls back to an implicit `default` agent.
5. If `intentRouter` is missing, blockcell injects the built-in default router automatically.
6. If `intentRouter.enabled = false` and `loadAllTools = false`, the profile portion uses `Unknown`; static mode also retains the runtime foundation and agent-authorized MCP tools.
7. If `intentRouter.enabled = false` and `loadAllTools = true`, runtime exposes all currently available tools, then applies the active profile's `denyTools`.
8. If `intentRouter.enabled = true`, `loadAllTools` is ignored and static mode unions all profile intent groups; only `toolSelection: "gate"` resolves tools from the current intent.
9. Every profile must configure `Unknown`, otherwise validation fails.

## Disable classification but keep all tools

Some deployments do not want intent classification to filter tools, but still want the LLM to see the complete current tool set. Configure:

```json
{
  "intentRouter": {
    "enabled": false,
    "loadAllTools": true,
    "defaultProfile": "default",
    "profiles": {
      "default": {
        "coreTools": [],
        "intentTools": {
          "Unknown": []
        },
        "denyTools": ["email", "exec"]
      }
    }
  }
}
```

In this mode:

- `enabled: false` disables intent classification
- `loadAllTools: true` returns all registered and currently available tools
- `denyTools` still applies, which is useful when you want broad capability but still exclude high-risk tools
- if `loadAllTools` is omitted, it defaults to `false`, using `Unknown` for the profile portion while retaining the runtime foundation and authorized MCP tools

## Add custom intent rules

`intentRules` extends the built-in classifier with domain-specific vocabulary. It adds matching conditions; it does not replace built-in rules.

```json
{
  "intentRouter": {
    "enabled": true,
    "intentRules": [
      {
        "category": "Finance",
        "keywords": ["funding rate", "open interest", "net inflow"],
        "patterns": ["(?i)funding\\s+rate", "(?i)open\\s+interest"],
        "negative": ["not market data"],
        "priority": 80
      }
    ]
  }
}
```

Fields:

| Field | Meaning |
|------|---------|
| `category` | Required existing intent category, such as `Finance`, `FileOps`, or `WebSearch` |
| `keywords` | Case-insensitive keywords; any hit matches the rule |
| `patterns` | Regular expressions; invalid patterns are skipped with a warning |
| `negative` | Negative keywords; if any is present, the rule is skipped |
| `priority` | Rule priority, default `60`; currently used for rule ordering |

## Example 1: default assistant + ops assistant

This is the most common two-role split:

- `default` handles daily chat, file operations, and web search
- `ops` handles ops work, debugging, and internal maintenance

```json
{
  "agents": {
    "list": [
      {
        "id": "default",
        "enabled": true,
        "name": "Daily Assistant",
        "intentProfile": "default",
        "allowedMcpServers": ["github", "sqlite"],
        "allowedMcpTools": ["github__list_issues", "sqlite__query"]
      },
      {
        "id": "ops",
        "enabled": true,
        "name": "Ops Assistant",
        "intentProfile": "ops",
        "allowedMcpServers": ["github"],
        "allowedMcpTools": ["github__list_issues", "github__create_issue"]
      }
    ]
  },
  "intentRouter": {
    "enabled": true,
    "defaultProfile": "default",
    "profiles": {
      "default": {
        "coreTools": [
          "read_file",
          "write_file",
          "list_dir",
          "exec",
          "web_search",
          "web_fetch",
          "memory_query",
          "memory_upsert",
          "toggle_manage",
          "message",
          "agent_status"
        ],
        "intentTools": {
          "Chat": { "inheritBase": false, "tools": [] },
          "FileOps": ["edit_file", "file_ops", "data_process", "office_write"],
          "WebSearch": ["browse", "http_request"],
          "Unknown": ["edit_file", "file_ops", "office_write", "http_request", "browse"]
        }
      },
      "ops": {
        "coreTools": [
          "read_file",
          "list_dir",
          "exec",
          "web_search",
          "web_fetch",
          "message",
          "agent_status"
        ],
        "intentTools": {
          "DevOps": ["network_monitor", "encrypt", "http_request", "edit_file", "file_ops"],
          "Organization": ["cron", "memory_maintenance", "list_skills"],
          "Unknown": ["http_request", "browse"]
        },
        "denyTools": ["email"]
      }
    }
  }
}
```

### How to read this example

- In static mode, `default` keeps the same capabilities in chat and task follow-ups; `Chat.inheritBase` applies only to `gate` mode.
- `default`'s `FileOps` and `WebSearch` cover file work and web search.
- `ops`'s `DevOps` profile allows debugging, encryption, file editing, and HTTP inspection.
- `ops` also sets `denyTools: ["email"]`, so `email` is removed even if it is added elsewhere.
- `allowedMcpServers` and `allowedMcpTools` only control what MCP resources the agent can see; they do not change the profile's tool mapping.

## Example 2: support + finance + admin

This example is closer to a “front desk / finance / platform ops” split:

- `support` handles day-to-day support and message replies
- `finance` handles market data, charts, alerts, and daily reports
- `admin` handles system control and platform maintenance

```json
{
  "agents": {
    "list": [
      {
        "id": "support",
        "enabled": true,
        "name": "Support",
        "intentProfile": "support",
        "allowedMcpServers": ["sqlite"],
        "allowedMcpTools": ["sqlite__query"]
      },
      {
        "id": "finance",
        "enabled": true,
        "name": "Finance",
        "intentProfile": "finance",
        "allowedMcpServers": ["market-data", "news"],
        "allowedMcpTools": ["market-data__quote", "news__search"]
      },
      {
        "id": "admin",
        "enabled": true,
        "name": "Admin",
        "intentProfile": "admin",
        "allowedMcpServers": ["github"],
        "allowedMcpTools": ["github__list_issues", "github__create_issue"]
      }
    ]
  },
  "intentRouter": {
    "enabled": true,
    "defaultProfile": "support",
    "profiles": {
      "support": {
        "coreTools": [
          "read_file",
          "list_dir",
          "message",
          "web_search",
          "web_fetch",
          "memory_query",
          "memory_upsert"
        ],
        "intentTools": {
          "Chat": { "inheritBase": false, "tools": [] },
          "Communication": ["email", "message", "http_request"],
          "Organization": ["list_tasks", "cron", "community_hub", "memory_maintenance"],
          "Unknown": ["message", "http_request"]
        }
      },
      "finance": {
        "coreTools": ["read_file", "web_search", "web_fetch", "message", "agent_status"],
        "intentTools": {
          "Finance": [
            "http_request",
            "data_process",
            "chart_generate",
            "alert_rule",
            "stream_subscribe",
            "knowledge_graph",
            "cron",
            "office_write",
            "browse"
          ],
          "WebSearch": ["browse", "http_request"],
          "Unknown": ["http_request", "browse"]
        },
        "denyTools": ["email"]
      },
      "admin": {
        "coreTools": ["read_file", "write_file", "list_dir", "exec", "toggle_manage", "message", "agent_status"],
        "intentTools": {
          "SystemControl": ["system_info", "app_control", "camera_capture", "browse", "image_understand", "termux_api"],
          "DevOps": ["network_monitor", "encrypt", "http_request", "edit_file", "file_ops"],
          "Unknown": ["edit_file", "file_ops", "http_request"]
        }
      }
    }
  }
}
```

### How to read this example

- `support` keeps its static profile scope during chat; exclude unwanted runtime foundation tools with `denyTools`.
- `finance` groups market data, alerts, charts, and reports together, which makes it a good finance agent.
- `admin` can use system-control and ops tools, but it does not get finance-specific capabilities.
- If you bind `finance` as the owner for a channel, messages from that channel will land on the finance profile first.
- If `intentRouter.enabled = false`, these profiles still exist, but with `loadAllTools: false` the profile portion uses `Unknown`, while the runtime foundation and authorized MCP tools remain available.

> The MCP server / tool names above are examples only. Replace them with real names from your merged `mcp.json` + `mcp.d` view.

## Resolution order

The final tool set is computed in this order:

1. Choose the profile for the current agent: first `agents.list[].intentProfile`, then `intentRouter.agentProfiles`, then `defaultProfile`
2. Merge `coreTools` and `intentTools` for the classified intents
3. Add runtime-required tools such as ghost-required tools
4. Apply `denyTools`
5. Apply runtime tool switches or visibility filters

## Compatibility behavior

- If `intentRouter` is missing, blockcell injects the built-in default router automatically
- `agents.list[].intentProfile` takes precedence over `intentRouter.agentProfiles`; the latter mainly exists for backward compatibility
- If `agents.list` is empty, runtime falls back to an implicit `default` agent
- `allowedMcpServers` and `allowedMcpTools` are agent-level MCP visibility allowlists, and their field names are camelCase

## Troubleshooting

Start with:

```bash
blockcell status
blockcell doctor
```

Check:

- which profile each agent resolves to
- whether `intentRouter` validation passes
- whether any tool names are invalid or unregistered
- whether `Unknown` is configured
- whether MCP-related names referenced by the profile actually exist in the merged `mcp.json` + `mcp.d/*.json` view

---

*Previous: [Provider Pool](./20_provider_pool.md)*

*Index: [Series directory](./00_index.md)*

## Explicit base capability packs

`profiles.<name>.basePacks` controls implicit capabilities in the default static mode. Omitting it preserves the legacy foundation; `[]` injects no packs. Supported names are `core` (global kernel, including memory writes/task/skill management), `coding` (file editing and shell lifecycle), `skills` (activation/local scripts), and `discovery` (tool discovery). Unknown pack names are configuration errors.

An explicitly file-read-only profile starts with:

```json
{"basePacks": [], "coreTools": ["read_file", "grep", "glob"], "intentTools": {"Unknown": []}}
```

Empty packs remove implicit capabilities. Explicit profile tools, host grants, scoped tools and allowlisted MCP tools can still add capabilities; a read-only deployment must restrict those grants and execution policies too. Denies always win. Legacy `gate` retains its compatibility behavior.
