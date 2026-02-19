# 快速开始

本仓库包含 **blockcell**：一个 Rust 自进化智能体框架。

- 你可以用交互式 CLI 运行（`blockcell agent`），也可以用守护进程模式运行（`blockcell gateway`）。
- 支持 Tool Calling（工具调用）、内置工具注册表、子任务/子代理后台执行、以及 WebUI。

## 1）安装

### 方式 A：安装脚本（推荐）

```bash
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/main/blockcell/install.sh | sh
```

默认安装到 `~/.local/bin`。

### 方式 B：源码编译

必需：Rust 1.75+

```bash
cargo build -p blockcell --release
```

二进制在 `target/release/blockcell`。

## 2）生成配置

首次运行初始化：

```bash
blockcell onboard
```

然后编辑 `~/.blockcell/config.json`，至少填入一个模型服务商的 API Key（例如 `providers.openrouter.apiKey`）。

## 3）交互模式运行

```bash
blockcell status
blockcell agent
```

小技巧：

- 输入 `/tasks` 查看后台任务。
- 输入 `/quit` 退出。

## 4）守护进程 + WebUI

启动 gateway：

```bash
blockcell gateway
```

默认端口：

- API 服务：`http://localhost:18790`
- WebUI：`http://localhost:18791`

如果配置了 `gateway.apiToken`：

- HTTP 调用：`Authorization: Bearer <token>`（或 `?token=<token>`）
- WebUI 登录：密码就是同一个 token

## 项目截图

![启动 gateway](screenshot/start-gateway.png)

![WebUI 登录](screenshot/webui-login.png)

![WebUI 对话](screenshot/webui-chat.png)
