# Intent Router 静态能力集验证记录

日期：2026-10-27。实现提交：`7fefdda`，分支：`fix/intent-static-tool-scope`。

## 实现范围

- 默认 `toolSelection: "rank"` 使用 profile 静态工具并集和既有运行时基础能力，连续对话不因意图变化丢失工具；`gate` 保留旧裁剪方式。
- 移除静态模式 Coding 的额外 schema 隐藏，授权工具可发现，搜索后同轮将 light schema 替换为完整 schema。
- skill、MCP 搜索、provider/ghost 追加、预取和执行统一受能力上限与 `denyTools` 约束；shell 别名保留策略检查。
- 新增 18 项 agent 回归测试和 1 项 core policy 回归测试。

静态模式保留此前 Coding 隐式加入的基础工具，窄 profile 需通过 `denyTools` 排除不需要的基础工具，详见 [配置说明](../21_intent_router_profiles.md)。

## 本地验证

使用 Rust 1.96.0，`TMPDIR=/tmp`：

```sh
cargo test -p blockcell-agent --tests -p blockcell-tools --lib -p blockcell-core --test tool_policy --offline
```

共 1,658 项通过，0 失败，包含 agent 单元测试 880 项、tools 单元测试 552 项，以及选中包的其他单元与集成测试。日志：`/tmp/blockcell-intent-integration.log`。

所有修改的 Rust 文件通过定向 `rustfmt --check`，`git diff --check` 通过。

`cargo build -p blockcell --offline` 成功。日志：`/tmp/blockcell-intent-build.log`。

## Flash 实测状态

用户确认：只跑串行，并发数 **1**。计划复用 `blockcell_realtask_bench/tasks` 的原有 12 个任务，每任务一次采样，保留旧运行数据。

旧基准使用 SiliconFlow 的 `deepseek-ai/DeepSeek-V4.1-Flash`，当前环境未取得该服务的可用凭据。此次预检使用本地配置的 DeepSeek 原生服务（`https://api.deepseek.com/v1`），请求模型 `deepseek-flash`。服务尚未成功接受请求，因此该模型标识的可用性也未得到确认。

- `/models` 预检返回 HTTP 401。
- 实现提交后，一次最小 `/chat/completions` 请求同样返回 HTTP 401。
- 12 个真实任务尚未启动；通过率、工具调用次数、token 用量和任务耗时均无有效数据。
- 最小请求的脱敏结果：`/tmp/blockcell-intent-flash-preflight.json`，未记录密钥。

本地回归通过说明已覆盖的行为符合预期，不能据此推断真实模型任务成功率或性能提升。后续需配置可用的 Flash 服务后，按并发 1 完成实测；不同服务或模型的结果应单独标明，不能作为同条件性能对照。
