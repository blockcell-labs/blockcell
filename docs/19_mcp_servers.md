# 第19篇：MCP Server 集成 —— 让 blockcell 接入任意外部工具

> 系列文章：《blockcell 开源项目深度解析》第 19 篇

---

## MCP 是什么

**MCP（Model Context Protocol）** 是由 Anthropic 主导推出的开放协议，目标是让 AI 助手能够以标准化方式调用外部工具和数据源。

可以把它理解为 AI 世界的 **"USB 接口"**：

- 工具提供方（MCP Server）：按协议暴露工具
- AI 客户端（MCP Client）：按协议发现并调用这些工具
- 两端只要遵守同一套协议，就能即插即用

目前 MCP Server 生态已经相当丰富，官方和社区维护了大量现成的服务器：GitHub、SQLite、PostgreSQL、Filesystem、Slack、Google Drive、Puppeteer 浏览器自动化……

---

## blockcell 如何集成 MCP

blockcell 内置了 `McpClient`，通过 **stdio 模式**与 MCP Server 通信：

```
blockcell 进程
    ├── 启动子进程（MCP Server）
    │       stdin ← JSON-RPC 请求（换行分隔）
    │       stdout → JSON-RPC 响应（换行分隔）
    ├── 握手：initialize + notifications/initialized
    ├── 获取工具列表：tools/list
    └── 调用工具：tools/call
```

MCP Server 的每个工具会被注册为 blockcell 内置工具，工具名格式为：

```
<服务器名>__<工具名>
```

例如配置了名为 `sqlite` 的 MCP Server，其 `query` 工具在 blockcell 里就叫 `sqlite__query`。

---

## 快速开始

### 第一步：安装 MCP Server

MCP Server 通常通过 `npx`（Node.js）或 `uvx`（Python）启动，无需单独安装：

```bash
# 测试能否正常启动（Ctrl+C 退出）
uvx mcp-server-sqlite --db-path /tmp/test.db
```

或者 GitHub 工具：

```bash
npx -y @modelcontextprotocol/server-github
```

### 第二步：编辑 config.json

打开 `~/.blockcell/config.json`，添加 `mcpServers` 字段：

```json
{
  "agents": { ... },
  "providers": { ... },

  "mcpServers": {
    "sqlite": {
      "command": "uvx",
      "args": ["mcp-server-sqlite", "--db-path", "/tmp/mydata.db"]
    },
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": {
        "GITHUB_PERSONAL_ACCESS_TOKEN": "ghp_xxxxxxxxxxxx"
      }
    }
  }
}
```

### 第三步：启动 blockcell

```bash
blockcell agent
```

启动时你会在日志中看到：

```
INFO Starting MCP server server=sqlite command=uvx
INFO MCP server mounted successfully server=sqlite
INFO Starting MCP server server=github command=npx
INFO MCP server mounted successfully server=github
```

MCP 工具已经自动加入工具列表，Agent 可以直接调用。

---

## 配置字段说明

```json
"mcpServers": {
  "<服务器名>": {
    "command": "启动命令（必填）",
    "args": ["命令行参数列表（可选）"],
    "env": {
      "环境变量名": "环境变量值"
    },
    "cwd": "子进程工作目录（可选）",
    "enabled": true
  }
}
```

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `command` | string | — | 要执行的命令，如 `npx`、`uvx`、`python` |
| `args` | array | `[]` | 命令行参数 |
| `env` | object | `{}` | 追加到子进程的环境变量 |
| `cwd` | string | null | 子进程的工作目录，不填则继承父进程 |
| `enabled` | bool | `true` | 设为 `false` 可临时禁用而不删除配置 |

---

## 常用 MCP Server 配置示例

### SQLite 数据库

适合本地数据查询、日志分析、简单数据管理。

```json
"sqlite": {
  "command": "uvx",
  "args": ["mcp-server-sqlite", "--db-path", "/Users/yourname/data/notes.db"]
}
```

配置后，对 Agent 说：
> "帮我查一下 notes 数据库里最近 10 条记录"

### GitHub

读取仓库信息、Issues、PR、文件内容。

```json
"github": {
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-github"],
  "env": {
    "GITHUB_PERSONAL_ACCESS_TOKEN": "ghp_xxxxxxxxxxxx"
  }
}
```

配置后，对 Agent 说：
> "帮我列出 blockcell-labs/blockcell 最近 5 个 open Issue"

### 文件系统（扩展访问范围）

允许 Agent 通过 MCP 协议访问指定目录（可作为内置 `read_file` 的补充）。

```json
"filesystem": {
  "command": "npx",
  "args": [
    "-y",
    "@modelcontextprotocol/server-filesystem",
    "/Users/yourname/Documents",
    "/Users/yourname/Projects"
  ]
}
```

### Puppeteer 浏览器自动化

使用 Node.js 驱动 Chrome，适合需要 JavaScript 渲染的页面。

```json
"puppeteer": {
  "command": "npx",
  "args": ["-y", "@modelcontextprotocol/server-puppeteer"]
}
```

### PostgreSQL

连接生产数据库，执行查询。

```json
"postgres": {
  "command": "npx",
  "args": [
    "-y",
    "@modelcontextprotocol/server-postgres",
    "postgresql://user:password@localhost:5432/mydb"
  ]
}
```

---

## 临时禁用某个服务器

不想删除配置，只想暂停某个 MCP Server：

```json
"sqlite": {
  "command": "uvx",
  "args": ["mcp-server-sqlite", "--db-path", "/tmp/test.db"],
  "enabled": false
}
```

---

## 工具命名规则

MCP 工具在 blockcell 内的名称格式为 `<服务器名>__<工具名>`（双下划线分隔）。

例如：

| 服务器名 | MCP 原始工具名 | blockcell 工具名 |
|----------|---------------|-----------------|
| `sqlite` | `query` | `sqlite__query` |
| `sqlite` | `list_tables` | `sqlite__list_tables` |
| `github` | `list_issues` | `github__list_issues` |
| `github` | `get_file_contents` | `github__get_file_contents` |

Agent 会自动使用正确的工具名，你不需要手动记忆，直接用自然语言描述需求即可。

---

## 工作原理

blockcell 在启动时（`mount_mcp_servers`）依次：

1. **启动子进程** — `tokio::process::Command` 启动 MCP Server，捕获 stdin/stdout
2. **握手** — 发送 `initialize` 请求（协议版本 `2024-11-05`），收到响应后发送 `notifications/initialized`
3. **发现工具** — 发送 `tools/list`，解析返回的工具列表（名称、描述、inputSchema）
4. **注册工具** — 每个 MCP 工具封装为 `McpToolWrapper`（实现 `Tool` trait），注册进 `ToolRegistry`
5. **运行时调用** — Agent 调用工具时，`McpToolWrapper.execute()` 发送 `tools/call` 给子进程

整个通信是全异步的，多个 MCP Server 并行运行互不干扰。

```
                    ┌──────────────────────────────┐
                    │         blockcell             │
                    │                               │
  用户消息 ──→  LLM │─→ 决定调用 sqlite__query      │
                    │         │                     │
                    │   ToolRegistry.execute()       │
                    │         │                     │
                    │   McpToolWrapper              │
                    │         │ tools/call (JSON-RPC)│
                    └─────────┼────────────────────┘
                              │ stdin
                    ┌─────────▼────────────────────┐
                    │    MCP Server (子进程)         │
                    │    uvx mcp-server-sqlite       │
                    └──────────────────────────────┘
                              │ stdout (结果)
                    ┌─────────▼────────────────────┐
                    │    返回结果给 LLM              │
                    │    LLM 生成最终回答            │
                    └──────────────────────────────┘
```

---

## 与内置工具的区别

| 维度 | 内置工具 | MCP 工具 |
|------|---------|---------|
| 实现语言 | Rust（编译进二进制） | 任意语言（独立进程） |
| 启动方式 | 无需额外启动 | 需要配置并启动子进程 |
| 性能 | 极低延迟 | 有进程间通信开销（通常 <10ms） |
| 生态 | blockcell 内置 50+ | 社区持续增长，已有数百个 |
| 扩展方式 | 修改 Rust 代码 | 配置 `mcpServers` 即可 |

**建议**：核心高频操作（读写文件、执行命令、网络请求）用内置工具；特定服务集成（GitHub、数据库、第三方平台）用 MCP。

---

## 自定义 MCP Server

如果现有 Server 不满足需求，可以自己实现一个。以 Python 为例：

```python
# my_server.py
from mcp.server import Server
from mcp.server.stdio import stdio_server
from mcp.types import Tool, TextContent
import mcp.types as types

app = Server("my-tools")

@app.list_tools()
async def list_tools() -> list[Tool]:
    return [
        Tool(
            name="hello",
            description="向指定名字打招呼",
            inputSchema={
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "名字"}
                },
                "required": ["name"]
            }
        )
    ]

@app.call_tool()
async def call_tool(name: str, arguments: dict) -> list[TextContent]:
    if name == "hello":
        return [TextContent(type="text", text=f"你好，{arguments['name']}！")]
    raise ValueError(f"Unknown tool: {name}")

async def main():
    async with stdio_server() as (read_stream, write_stream):
        await app.run(read_stream, write_stream, app.create_initialization_options())

if __name__ == "__main__":
    import asyncio
    asyncio.run(main())
```

然后在 config.json 中配置：

```json
"my-tools": {
  "command": "python",
  "args": ["/path/to/my_server.py"]
}
```

配置后对 Agent 说：
> "用 my-tools 的 hello 工具，传入名字：世界"

---

## 故障排查

### MCP Server 启动失败

查看 blockcell 日志：

```
ERROR Failed to start MCP server server=sqlite error=...
```

常见原因：
- `uvx`/`npx` 未安装 → 安装 Python/Node.js
- 包名拼写错误 → 先手动在终端测试命令
- 权限问题 → 检查 `cwd` 和文件权限

### 工具未出现在列表中

```bash
# 确认 MCP Server 返回了工具
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | uvx mcp-server-sqlite --db-path /tmp/test.db
```

### 工具调用返回错误

MCP Server 返回 `isError: true` 时，blockcell 会将错误信息透传给 Agent，Agent 会自动尝试修正参数后重试。

---

## 相关文档

- [工具系统](./03_tools_system.md) — blockcell 内置工具总览
- [架构深度解析](./12_architecture.md) — crate 结构与设计模式
- [MCP 官方规范](https://spec.modelcontextprotocol.io) — 协议详细文档
- [MCP Server 生态列表](https://github.com/modelcontextprotocol/servers) — 官方维护的 Server 列表
