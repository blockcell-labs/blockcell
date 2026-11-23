# 评测与晋级

[文档首页](README.md) · 上一篇：[架构](03-architecture.md) · 下一篇：[配置参考](05-configuration.md)

## 评测必须独立于候选

Candidate 不能通过修改题目或评分器证明改进。Genesis 默认从 frozen worktree 读取评测源码，coding_eval 恢复可信测试文件，再把包含候选修改代码的验收过程放进离线沙箱。EAB 将 Agent 网关与题库、评分目录分离。

诊断阶段只接收 coding/dev/BFCL 的失败证据，不接收 EAB holdout 题目和轨迹。当前 `evidence()` 也会携带允许暴露题目的失败原因与截断输出，并非只给通过数；扩展适配器时应主动审查哪些内容会进入提示词。

## L0–L4

| 阶段 | 验证内容 | 当前行为 |
|---|---|---|
| FORMAT | Rust 格式 | 只处理候选改动的 Rust 文件，使用 `skip_children=true` |
| L0 | 编译、静态检查、测试、依赖审计 | 与 Parent 比较新增诊断或 advisory，而非要求旧问题全部消失 |
| L1 | 补丁大小与路径边界 | 默认最多 8 文件、400 增删行；保护路径拒绝，高风险路径不能自动晋级 |
| REVIEW | Parent 评审 | 保存解释，但它不是确定性验收；前后 diff 必须一致 |
| L2 | coding 快筛 | 运行一次，默认相对 Parent 首轮少通过超过 2 题即拒绝 |
| L3 | 完整能力评测 | 默认 coding 共 3 次，EAB dev/holdout 各一次；BFCL 可选 |
| L4 | 成本/性能与标准修改能力 | 比较 token、延迟、工具调用、二进制大小，执行固定 selfcheck |

L0 的严格 clippy 若 Parent 已失败，会以 `--cap-lints=warn` 补采完整诊断。门禁按诊断类别、路径、源码片段和数量比较新增项；无法解析的失败不会直接放行。审计同样比较新增 advisory，当前任意等级新增漏洞都会拒绝。

这一相对策略便于在有历史问题的仓库迭代，但**不证明 Parent 或候选整体健康**。生产采用前仍需治理已知依赖风险。

## 配对比较

先对每个任务的重复运行做多数投票，再比较相同 task_id：

```text
fixed  = Parent 不通过、Candidate 通过的任务
broken = Parent 通过、Candidate 不通过的任务
net    = fixed 数量 - broken 数量
```

各次报告必须包含相同的非空任务集，每个报告内的 `task_id` 必须唯一。报告非 `ok` 或出现平票时，无法形成有效配对。默认 coding 使用三次投票降低单次运行波动的影响。

当前主收益为 `coding_eval + EAB dev` 的合计净修复数，默认至少 2。例：coding 修复 2 题且破坏 1 题，EAB dev 修复 1 题且无破坏，净修复为 2。此时仍需通过其他门禁。

## 四种判定

| 判定 | 含义与典型触发 |
|---|---|
| `PROMOTE` | 必选评测可用，净修复达标，安全/holdout/性能通过，自检通过，且不是高风险补丁 |
| `ARCHIVE` | 保留有价值或新颖的候选，但不足晋级；高风险补丁即使有效也至多归档 |
| `REJECT` | 净修复数为负，或触发编译、补丁路径、安全、holdout、性能等拒绝门槛 |
| `INCONCLUSIVE` | 必选评测不可用、配对不一致，或尚无足够改进证据 |

这些规则有执行优先级：安全、holdout 与性能失败不能被高净修复数抵消；高风险补丁也可能因为其他失败直接拒绝。自检失败的候选不能晋级，即使保留为 `ARCHIVE`，也不能进入 Parent 选择池。

具体规则以 [gates.decide()](../gates.py) 和 [Supervisor](../supervisor.py) 为准：

- coding 安全题由显式 `safety_ids` 及 Parent 报告的 `category`、`task_id` 关键词识别。候选报告缺少已识别的安全题时，判定为 `INCONCLUSIVE`。
- coding 和 EAB 的安全题须在候选的每次评分中通过，即使父代也未通过。
- holdout 要求总通过数不降低，**不是所有 holdout 单题都必须零回退**；安全题有单独的更严格规则。
- BFCL 若有有效配对，默认允许总通过数最多下降 1。
- coding 性能比较仅使用结果相同且双方都有对应指标的任务。逐题 token、延迟 p95 与总工具调用的默认上限均为 1.15 倍。二进制大小上限为 1.10 倍。
- `commit_enabled=false` 时，即使计算出 `PROMOTE`，最终也会降为 `ARCHIVE`。

## 适配器结果格式

内部归一化后的报告结构示例：

```json
{
  "status": "ok",
  "results": [
    {
      "task_id": "example-01",
      "passed": true,
      "duration_ms": 1200,
      "input_tokens": 1000,
      "output_tokens": 200,
      "tool_calls": 2,
      "tool_errors": 0,
      "failure_reason": "",
      "trace_path": "relative-or-local-evidence-path"
    }
  ]
}
```

每行必须有字符串 `task_id` 与布尔 `passed`。缺失性能指标会跳过该项比较。接入新评测器时应提供实测指标，并校验唯一任务、完整任务集和来源。

必选评测 `skipped` 不是通过。例如 EAB 套件缺失会得到 skipped，不能据此晋级；默认关闭且非必选的 BFCL 可以缺席。

## 缓存与复现

评测缓存绑定 Agent SHA、benchmark、run_idx、模型、dataset_version 和评测配置 hash。配置 hash 包含 EAB 套件路径与文件内容。coding/EAB 逐题 checkpoint 还绑定二进制内容 hash。评测输入在一轮内发生变化时，该轮结果不能用于晋升。上游故障不应被永久缓存成能力失败。

阈值和预算不属于能力输入，调整它们不一定触发重评。反过来，修改外部题库文件、外部命令内容或环境变量指定的 EAB 路径，不保证被顶层 hash 自动发现。应固定套件版本、更新 `dataset_version` 或使用新的实验目录，不能在原路径悄悄替换题目后复用旧结果。

可复现报告至少保留：Parent/Candidate SHA、二进制 hash、实际模型与接口、源码及外部套件版本、配置、逐题结果、预算记录和平台信息。不要只保留最终 `PROMOTE` 或一个总分。

## 测试通过数的正确解释

Python 回归测试验证 Supervisor 行为；Rust workspace 测试验证仓库代码；coding/EAB 才是在指定模型条件下对 Agent 能力的测量。三者不能互相替代。

2026-11-22 本机修复临时目录问题后，可信工作区在**测试专用 macOS 沙箱**完成 Rust 全量 2784 passed / 0 failed / 10 ignored。复现入口为 [test_workspace_sandbox.py](../../scripts/test_workspace_sandbox.py)，该配置为本地 HTTP mock 允许 localhost，并额外读取工作区祖先目录对象。

该结果不表示原样、完全禁网的生产构建 profile 全量通过，也不表示旧锁定 SHA 已包含当前工作区修复。正式引用时应附上对应版本与日志。
