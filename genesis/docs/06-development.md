# 开发与贡献

[文档首页](README.md) · 上一篇：[配置](05-configuration.md) · 下一篇：[运行与排障](07-operations.md)

## 本地开发原则

Genesis 的 Python 控制器仅依赖标准库。修改 Python 控制逻辑时，先用假 Agent、临时仓库和可控 Provider 响应复现问题，再修改实现；真实模型实验用于验证兼容性和能力，不能取代确定性回归测试。

遵守当前仓库的 [AGENTS.md](../../AGENTS.md) 与维护者工作约定。不要把实验生成的提交、状态库或二进制混进原仓库源码改动。

## 测试分层

| 层次 | 命令或方法 | 能证明什么 |
|---|---|---|
| CLI | `python3 -m genesis --help` | 参数入口可加载 |
| Python 回归 | `python3 -m unittest discover -s genesis/tests -v` | 控制器、门禁、恢复和失败注入行为 |
| 语法 | `python3 -m compileall -q genesis` | Python 文件可编译，不能替代运行测试 |
| Rust 模块 | 对改动的 crate 执行 cargo check/test | 相关 Rust 实现的行为 |
| macOS 全量测试沙箱 | `python3 scripts/test_workspace_sandbox.py` | 可信工作区在显式测试 profile 下的 Rust 回归 |
| 真实基线/实验 | `baseline`、`run` | 指定版本、模型与题库条件下的测量，有费用 |

控制器和晋级回归覆盖父代回退、绝对安全门槛、套件指纹、性能指标和周期超时。测试使用临时 Git 仓库、假 Agent、Provider mock 和失败注入。[核心测试](../tests/test_genesis.py) · [隔离与计量](../tests/test_release_hardening.py) · [评测与恢复](../tests/test_release_completion.py) · [审查回归](../tests/test_review_regressions.py)

Linux 专有测试在 macOS 跳过属于平台条件，不证明 Linux 已验收。CI 的 [genesis.yml](../../.github/workflows/genesis.yml) 当前运行 macOS Python 回归与语法检查，不调用真实模型，也不运行全部 Rust 能力实验。

若本机执行环境禁止安装 Seatbelt，原生边界测试需要在允许调用 `sandbox-exec` 的环境验证。不要通过关闭产品隔离来获得绿色结果。

## 修改状态机

新增阶段或持久化字段时，至少考虑：

1. 阶段进入之前保存什么，动作完成之后保存什么。
2. 中断发生在文件、SQLite 与外部命令之间时，恢复是否会重跑；重跑如何避免重复计数。
3. `ProviderUnavailable` 是否保留原阶段，`BudgetExceeded` 是否正确停止，其他异常是否应归因给候选。
4. 旧工作区缺少新字段时如何兼容，迁移是否需要排他锁。

参考 `FORMAT`、`FINISH_CYCLE` 和 Parent 资格的实现。应测试中断后的二次进入，而不只是无中断的直通路径。持久化身份变化要同时审查缓存：旧结果可能不再适用。

## 添加评测器

已有外部工具优先评估 BFCL 接口；自定义新类型则需要同时修改适配器、Evaluator 编排、配置、判定、诊断证据过滤和测试。当前不是动态插件系统，添加文件不会自动注册。

适配器应完成：

- 明确可信评分器和不可信被测代码的边界，隔离执行被测代码，使用去凭据环境。
- 固定数据集身份，验证题号唯一、题目完整、`passed` 为布尔值，输出规范结果。
- 将模型请求接入共享 Meter；基础设施故障传播为独立错误，不能悄悄写成 `passed=false`。
- 保留逐题证据；需要恢复时绑定二进制和评测身份，采用临时文件再替换写入 checkpoint。
- 显式决定哪些失败信息可进入 diagnose，holdout 内容不能回灌。

BFCL 的最小配置形状如下，需合并进现有 JSON：

```json
{
  "enabled": true,
  "required": true,
  "version": "your-suite-v1",
  "command": ["/absolute/path/to/trusted-evaluator", "--binary", "{binary}", "--output", "{output}"],
  "readonly_paths": ["/absolute/path/to/fixed-suite"]
}
```

外部程序必须在 `{output}` 写入非空 `results`，每行包含 `task_id` 与布尔 `passed`，其他指标见[结果协议](04-evaluation.md)。它运行在 BFCL 沙箱内，应通过被测 Agent 的本地代理访问模型，不能依赖宿主 HOME 或自行读取 key。

BFCL 示例只是接口，不代表已附带 BFCL 题库、官方评分实现或其许可。外部工具若启动额外进程或需要其他只读资源，要单独验证边界。

## 修改隔离或计量

这类改动需要正反两类证据：必要工作能完成；禁止的文件与网络访问仍被拒绝。不能只测“沙箱命令成功”。还要测试候选子进程、软链接、构建脚本、评分目录、凭据配置和中断清理。

计量回归应覆盖：有效 usage、缺失/非法 usage、完整 JSON 与 SSE、429、鉴权/余额错误、5xx、畸形响应、并发请求和中途停止。未知费用必须留下诊断，不能通过补零绕过预算。

测试夹具优先使用 `tempfile`、`TMPDIR` 或授权工作区的唯一子目录，避免写死宿主 `/tmp`。涉及 Agent 内部路径策略的 fixture 可以参照 [test_support.rs](../../crates/agent/src/test_support.rs)：macOS 系统临时路径可能被产品敏感路径规则拒绝，所以授权位置也要与产品策略兼容。

本地 mock 的回环网络权限应留在显式测试 profile；不要为了跑完整测试扩大生产 Candidate profile。

## 提交供审核的改动

改动说明应写清具体问题、改变后的行为、验证命令及平台。新增配置要同时说明默认值、是否影响缓存、是否需要迁移。涉及评测的变动要解释是否影响既有成绩的可比性。

公开证据前移除 key、用户文件、会话内容、模型返回的敏感内容和本机路径中的私人信息。提交可复现的小 fixture 与脱敏错误即可，不要直接上传完整 `.genesis/`。

开源发布时，应检查文档相对链接所需的源码和辅助脚本是否包含在发布范围；Supervisor 源码包不应混入外部题库、真实运行数据库、模型响应或构建缓存。
