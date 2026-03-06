# 快速开始

本仓库包含 **blockcell**：一个 Rust 自进化智能体框架。

- 你可以用交互式 CLI 运行（`blockcell agent`），也可以用守护进程模式运行（`blockcell gateway`）。
- 支持 Tool Calling（工具调用）、内置工具注册表、子任务/子代理后台执行、以及 WebUI。

## 1）安装

### 方式 A：安装脚本（推荐）

```bash
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/refs/heads/main/install.sh | sh
```

默认安装到 `~/.local/bin`。

### 方式 B：源码编译

必需：Rust 1.75+

```bash
cargo build -p blockcell --release
```

二进制在 `target/release/blockcell`。

## 2）生成配置

首次运行推荐直接使用配置向导：

```bash
blockcell setup
```

它会自动创建 `~/.blockcell/`、写入 provider 配置，并在你启用外部渠道时自动补 `channelOwners` 的默认绑定。若你后续需要把同一渠道的不同账号路由到不同 agent，可以再手动补 `channelAccountOwners`。若你更喜欢旧流程，也仍可执行 `blockcell onboard` 后手动编辑 `~/.blockcell/config.json`。

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
- WebSocket：也可用 `?token=<token>`

WebUI 登录密码与 API token 现在分离：

- 若设置了 `gateway.webuiPass`，WebUI 使用该固定密码
- 若未设置，Gateway 启动时会打印一个临时密码
- 若 `gateway.apiToken` 为空，Gateway 会自动生成并持久化一个 token

## 项目截图

![启动 gateway](screenshot/start-gateway.png)

![WebUI 登录](screenshot/webui-login.png)

![WebUI 对话](screenshot/webui-chat.png)
