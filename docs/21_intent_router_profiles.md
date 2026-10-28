# intentRouter 多智能体配置指南

`intentRouter` 是 blockcell 中 **agent 工具能力范围的配置入口**。如果你想让不同的 agent 拥有不同的能力范围，推荐把“agent 归属”和“工具编排”拆开配置：

- 在 `agents.list[].intentProfile` 上绑定每个 agent 使用的 profile
- 在 `intentRouter.profiles` 中定义可复用工具集
- 用 `coreTools` 放共享基础工具
- 默认 `toolSelection: "rank"`：`coreTools` 与全部 `intentTools` 条目共同组成静态能力集，消息意图不再裁剪工具
- 用 `denyTools` 从最终结果里移除不允许暴露的工具
- `Chat: { "inheritBase": false, "tools": [] }` 在默认静态模式下不清空工具；依赖旧行为时显式设置 `toolSelection: "gate"`
- 始终配置 `Unknown`
- 如果只是想临时关闭意图分类但保留全量工具，可用 `enabled: false` + `loadAllTools: true`
- 如果内置意图规则不能覆盖你的业务词汇，可用 `intentRules` 追加关键词或正则

> `allowedMcpServers` / `allowedMcpTools` 是 agent 级别的 MCP 可见性白名单，字段名是 camelCase。

## 静态能力与兼容迁移（2026-10-27）

默认 `intentRouter.toolSelection` 为 `"rank"`。名称为兼容未来排序提示保留；当前工具 schema 按既有展示优先级和名称稳定排序，不随本轮意图重排。意图继续决定交互提示，不决定工具准入。

能力范围由 profile 工具并集、运行时基础工具、agent 的 MCP 白名单与注册表共同决定，最后统一扣除 `denyTools` 和未注册工具。运行时基础工具包括既有全局内核（memory、agent/spawn、skill、session、cron、web_fetch、tool_search、mcp_search_tools 等），以及此前 Coding 隐式补充的 `read_file`、`write_file`、`edit_file`、`grep`、`shell_run`；同时保留 `glob`、shell 的 poll/input/close 和仅在 skill 目录中执行的 `exec_skill_script` / `exec_local`。这些工具仍受执行策略和确认机制约束。自定义窄 profile 也应通过 `denyTools` 排除不需要的基础工具；`denyTools: ["shell"]` 会同时排除 shell 的四个单用途工具。

升级会扩大此前依赖意图裁剪的工具范围。要保留旧的逐轮裁剪和 Coding 曝光行为，配置 `"intentRouter": { "toolSelection": "gate" }`。两种模式都强制执行 `denyTools`；兼容开关不能恢复绕过拒绝规则的行为。

静态模式下，Coding 不再额外隐藏工具 schema。展示为 `discoverable` 的工具先进入授权发现目录，经 `tool_search` 或 MCP 搜索后补全完整 schema；发现只能改变曝光，不能扩权。普通工具继续复用既有 full/light schema 机制，light 工具搜索后在同轮替换为完整 schema。

skill 只能从 agent 能力范围中选择工具，不能靠声明扩大范围。自定义 skill 或 memory provider 使用的工具名也需要出现在 profile 中（内置基础工具和已授权 MCP 工具除外）。ghost 的工具追加、持久化发现记录和旧 `shell` 调用同样不能绕过 `denyTools`。profile 不存在时静态模式关闭工具能力，不回退到全局内核。

`ToolPolicy` 仍从 `~/.blockcell/tool_policy.yaml` 加载，按参数、路径和通道求值，优先级为 deny > ask > allow。hook 改写后的参数必须经过 policy；shell 别名与规范名称同时受最严格的匹配结果约束。它与 profile 的静态能力限制同时生效。

## 先理解这几个规则

1. `agents.list[].intentProfile` 的优先级最高。
2. 如果 agent 没写 `intentProfile`，则回退到 `intentRouter.agentProfiles`。
3. 如果还是找不到，就回退到 `intentRouter.defaultProfile`。
4. `agents.list` 为空时，runtime 会回退到一个隐式的 `default` agent。
5. `intentRouter` 缺失时，blockcell 会自动注入内置默认 router。
6. 如果 `intentRouter.enabled = false` 且 `loadAllTools = false`，profile 部分使用 `Unknown` 工具集；静态模式仍保留运行时基础工具和 agent 已授权的 MCP 工具。
7. 如果 `intentRouter.enabled = false` 且 `loadAllTools = true`，runtime 会暴露当前可用的全部工具，再应用当前 profile 的 `denyTools`。
8. 如果 `intentRouter.enabled = true`，`loadAllTools` 会被忽略，默认取 profile 全部意图组的并集；仅 `toolSelection: "gate"` 按本轮意图解析工具。
9. 每个 profile 都必须提供 `Unknown`，否则配置校验会失败。

## 关闭分类但保留全量工具

有些场景下你可能不想让意图分类器筛工具，而是希望 LLM 看到当前可用的完整工具集。可以这样配置：

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

这种模式下：

- `enabled: false` 表示不走意图分类
- `loadAllTools: true` 表示返回所有当前注册且可用的工具
- `denyTools` 仍然会生效，适合保留全量能力但排除高风险工具
- 如果不设置 `loadAllTools`，默认是 `false`，profile 部分会回到 `Unknown`，运行时基础工具和已授权 MCP 工具仍保留

## 追加自定义意图规则

`intentRules` 用来补充内置分类规则，适合把业务词汇映射到已有意图类别。它不会覆盖内置规则，只会追加命中条件。

```json
{
  "intentRouter": {
    "enabled": true,
    "intentRules": [
      {
        "category": "Finance",
        "keywords": ["资金费率", "合约持仓", "链上净流入"],
        "patterns": ["(?i)funding\\s+rate", "(?i)open\\s+interest"],
        "negative": ["不是行情"],
        "priority": 80
      }
    ]
  }
}
```

字段说明：

| 字段 | 说明 |
|------|------|
| `category` | 必填，必须是已有意图类别，例如 `Finance`、`FileOps`、`WebSearch` |
| `keywords` | 大小写不敏感的关键词，出现即命中 |
| `patterns` | 正则表达式，任意一条匹配即命中；无效正则会被跳过并记录 warning |
| `negative` | 否定关键词，命中后跳过该规则 |
| `priority` | 规则优先级，默认 `60`；当前主要用于规则排序 |

## 示例一：默认助手 + 运维助手

这个例子适合最常见的两人/两角色拆分：

- `default` 负责日常对话、文件操作、网页搜索
- `ops` 负责运维、排障、内部管理类任务

```json
{
  "agents": {
    "list": [
      {
        "id": "default",
        "enabled": true,
        "name": "日常助手",
        "intentProfile": "default",
        "allowedMcpServers": ["github", "sqlite"],
        "allowedMcpTools": ["github__list_issues", "sqlite__query"]
      },
      {
        "id": "ops",
        "enabled": true,
        "name": "运维助手",
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

### 这个例子怎么理解

- 默认静态模式下，`default` 在闲聊和后续任务中保留相同工具能力；`Chat.inheritBase` 仅在 `gate` 模式下生效。
- `default` 的 `FileOps` 和 `WebSearch` 分别对应文件操作和网页搜索场景。
- `ops` 的 `DevOps` 允许排障、加密、文件编辑和 HTTP 调试工具。
- `ops` 额外配置了 `denyTools: ["email"]`，即使别处把 `email` 放进来了，这个 profile 也会把它移除。
- `allowedMcpServers` / `allowedMcpTools` 只影响当前 agent 能看到哪些 MCP 资源，不影响 `intentRouter.profiles` 里的工具编排。

## 示例二：客服 + 金融分析 + 平台管理员

这个例子更适合“前台接待 / 财务分析 / 平台运维”拆分：

- `support` 负责日常客服和消息回复
- `finance` 负责行情、图表、告警、日报
- `admin` 负责系统控制和平台维护

```json
{
  "agents": {
    "list": [
      {
        "id": "support",
        "enabled": true,
        "name": "客服",
        "intentProfile": "support",
        "allowedMcpServers": ["sqlite"],
        "allowedMcpTools": ["sqlite__query"]
      },
      {
        "id": "finance",
        "enabled": true,
        "name": "金融分析",
        "intentProfile": "finance",
        "allowedMcpServers": ["market-data", "news"],
        "allowedMcpTools": ["market-data__quote", "news__search"]
      },
      {
        "id": "admin",
        "enabled": true,
        "name": "平台管理员",
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

### 这个例子怎么理解

- `support` 的工具范围由 profile 静态决定；高频聊天不会清空工具，需要限制的基础工具应加入 `denyTools`。
- `finance` 专门把行情、告警、图表和日报工具聚在一起，适合做“金融分析 Agent”。
- `admin` 允许系统控制和运维类工具，但没有金融类能力。
- 如果你把 `finance` 设成某个渠道的 owner，那么这类消息就会优先落到金融 profile 上。
- 如果 `intentRouter.enabled = false`，这三个 profile 仍会存在，在 `loadAllTools: false` 时 profile 部分只使用 `Unknown`；运行时基础工具和 agent 已授权的 MCP 工具仍保留。

> 以上 MCP server / tool 名称只是示例，请替换成你自己 `mcp.json` + `mcp.d` 里真实存在的名称。

## 解析顺序

最终工具集合按下面顺序得到：

1. 选择 agent 对应的 profile：先看 `agents.list[].intentProfile`，再看 `intentRouter.agentProfiles`，最后回退 `defaultProfile`
2. 默认静态模式合并 profile 中所有工具组及所选基础包；仅 `gate` 按当前意图选择工具组
3. 叠加运行时强制工具（如 ghost 所需工具）
4. 应用 `denyTools`
5. 应用运行时其它工具开关或可见性过滤

## 兼容行为

- 如果 `intentRouter` 整段缺失，blockcell 会自动注入内置默认 router
- `agents.list[].intentProfile` 的优先级高于 `intentRouter.agentProfiles`；后者主要用于兼容旧配置
- 如果 `agents.list` 为空，runtime 会回退到一个隐式的 `default` agent
- `allowedMcpServers` / `allowedMcpTools` 是 agent 级别的 MCP 可见性白名单，字段名是 camelCase

## 排查建议

可以先运行：

```bash
blockcell status
blockcell doctor
```

重点看：

- 当前 agent 绑定到哪个 profile
- `intentRouter` 是否通过校验
- 是否引用了未注册的工具名
- `Unknown` 是否有配置
- 如果 profile 里引用了 MCP 工具，MCP server / tool 名称是否与当前 `mcp.json` + `mcp.d` 合并结果一致

## 显式基础能力包

`profiles.<name>.basePacks` 控制默认静态模式的隐式能力。省略字段保持旧版兼容；显式 `[]` 不注入任何基础包。可选值为 `core`（全局内核，含记忆写入、任务及技能管理）、`coding`（文件编辑与 shell 生命周期）、`skills`（技能激活和本地脚本）、`discovery`（工具发现）。包名拼错会拒绝配置。

下面的 profile 仅显式声明文件读取能力：

```json
{"basePacks": [], "coreTools": ["read_file", "grep", "glob"], "intentTools": {"Unknown": []}}
```

空基础包表示不继承隐式能力；`coreTools`、`intentTools`、显式宿主授权、作用域工具以及获白名单授权的 MCP 工具仍可能增加能力。真正只读还需要这些授权和执行策略一致。`denyTools` 始终优先；`gate` 为兼容旧行为保留原路径。

## 宿主批准的技能工具

profile 可设置 `"skillGrants": {"audit": ["read_file", "email"]}`。这是宿主显式批准的 agent 能力，按技能名分组以避免混入领域工具组；工具会在激活前进入静态能力上限。它不是“仅在技能内部可调用”的权限。技能执行仍仅使用自己的工具声明与 agent 能力上限的交集。

技能的 `tools`、`learned: false` 和搜索结果都不能自行授权；普通 `register` 只提供实现，嵌入方可用 `register_host_granted` 显式批准扩展。作用域注册受视图及 guard 生命周期约束，真实 MCP 注册仍需通过当前 agent 的白名单。所有来源都受最终 deny 和执行策略约束。
