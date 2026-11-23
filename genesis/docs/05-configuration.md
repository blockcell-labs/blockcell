# 配置参考

[文档首页](README.md) · 上一篇：[评测](04-evaluation.md) · 下一篇：[开发与贡献](06-development.md)

完整默认值见 [genesis.json](../genesis.json)，加载与 hash 规则见 [config.py](../config.py)。本篇解释经常需要修改的字段及容易误用的地方。

## 加载与路径

```sh
python3 -m genesis --config /absolute/path/to/genesis.local.json status
```

不传 `--config` 时读取包内 `genesis/genesis.json`。加载后，非空的 `OPENAI_BASE_URL` 覆盖 `agent.upstream`，`OPENAI_MODEL` 覆盖 `agent.model`。key 的环境变量名由 `agent.key_env` 决定，默认 `OPENAI_API_KEY`。

`workspace_root` 按当前工作目录转绝对路径。替换锁定 SHA 时应使用新 workspace，已有 frozen SHA 不匹配会失败。配置是可信输入，不应交给模型修改；当前只做部分字段校验，不是完整 JSON Schema 验证器。

## 实验控制 `genesis`

| 字段 | 默认值或约束 | 含义 |
|---|---|---|
| `repository` | Blockcell Git URL | 可改成本地可信仓库路径 |
| `base_sha` / `genesis0_sha` | 两者必须相等 | 初始源码与 frozen 评测源的提交 |
| `workspace_root` | `.genesis` | 状态、缓存、源码、二进制与报告的根目录 |
| `candidates_per_cycle` | 必须为 `2` | 当前版本固定两个假设与候选 |
| `max_patch_files` / `max_patch_lines` | `8` / `400` | 文件数与新增加删除行数上限，含测试和格式化 |
| `max_cycle_minutes` | `480` | 每轮时间上限，构建和评测子进程也受剩余时间约束。独立执行 `baseline` 不开启实验周期 |
| `budget.per_cycle_usd` / `total_usd` | `30` / `30` | 费用估值限制，累计包含显式预留 |
| `stagnation.per_parent_rounds` | `3` | 连续未晋级后尝试探索合格归档版本 |
| `stagnation.global_rounds` | `8` | 全局连续未晋级的停止阈值 |
| `unfit_compile_rounds` | `3` | 连续整轮候选编译失败的 Parent 淘汰阈值 |

初始 SHA 是可复现输入，不是安全认证。默认提交的已知依赖情况见 [SECURITY.md](../SECURITY.md)。

## 模型与请求 `agent`

| 字段 | 默认值或约束 | 含义 |
|---|---|---|
| `command` | argv 数组 | 默认 `{binary} agent -m {prompt}`，不经 shell 解释 |
| `model` / `provider` / `upstream` | 模型 ID / `openai` / API base | 三者必须与实际服务兼容 |
| `key_env` | `OPENAI_API_KEY` | Supervisor 读取 key 的环境变量名 |
| `upstream_stream` | `false` | 完整 JSON 校验后转 SSE；true 使用上游流 |
| `timeout` | `1800` 秒 | Agent 子进程的调用超时 |
| `concurrency` | 整数 `1..5` | 共享上游请求并发上限；coding 独立任务可并行 |
| `retry_429_seconds` | `300` 秒 | 上游限流退避时间 |
| `user_agent` | Genesis 浏览器式 UA | 上游请求 User-Agent，不改变鉴权与计费 |
| `token_limit` | `250000000` | 已知输入加输出 token 的累计限制 |
| `require_bwrap` | `true` | 历史字段名，表示必须使用本机隔离；macOS 自动选 Seatbelt |
| `input_usd_per_million` / `output_usd_per_million` | `0.15` / `0.60` | 示例单价，必须换成实际价格 |
| `http_transport_command` | 默认不配置 | 可选原生 HTTP 传输程序的 argv |

`command` 支持 `{binary}`、`{prompt}`、`{workspace}`、`{task_id}`。模板替换只是字符串替换，不执行命令替换；请保持 argv 数组，勿自行拼接 `sh -c`。

`require_bwrap=false` 只适用于无凭据假 Agent 测试，不能作为真实实验遇到权限问题时的修复方式。并发上限不等于严格美元硬上限；计费细节见[排障](07-operations.md)。

当需要原生 HTTP 传输时，可构建 [genesis_http 示例](../../crates/providers/examples/genesis_http.rs)，并按现有 [README](../README.md) 设置其可执行文件 argv。该程序独立于 Python 标准库依赖要求，需另行编译。

## 构建与晋级

| 字段 | 默认行为 |
|---|---|
| `build.command` | `cargo build --release -p blockcell`，支持 `{worktree}` 与 `{target}` |
| `build.binary` | 共享 target 下的 `release/blockcell`；最终拷贝为 Agent 专属二进制 |
| `build.timeout` | `7200` 秒 |
| `promotion.commit_enabled` | `true`；允许在独立 clone 中保存 detached 谱系提交 |

`promotion.internal`、`promotion.external`、`promotion.selfcheck_required` 当前存在于默认配置中，但状态机没有把它们实现为可切换策略：实际行为由代码确定，selfcheck 始终执行，外部产物只是本地 PR 草稿。不要以为把 `selfcheck_required` 改成 false 能跳过自检。

## 评测 `benchmarks`

| 字段 | 用途 |
|---|---|
| `dataset_version` | 可信题库/评测版本标识，参与缓存身份；题库变更需更新 |
| `L0` | Cargo fmt、clippy、test 命令数组，支持的相对诊断机制见 cargo adapter |
| `audit_command` | 默认 `cargo audit --json --no-fetch`；需离线 advisory 数据库 |
| `L2.coding_eval.reject_if_drop_gt` | 快筛允许的通过数下降阈值，默认 2 |
| `L3.coding_eval.runs` | 完整 coding 重复次数，默认 3；建议使用奇数 |
| `L3.coding_eval.required` | 默认 true |
| `L3.coding_eval.task_ids` | 可选，仅运行选定题号；不宜与完整实验结果混用 |
| `L3.eab60.kit` | 套件路径，优先于 `EAB60_KIT` 环境变量。建议使用绝对路径，套件文件内容参与评测缓存指纹 |
| `L3.eab60.dev` / `holdout` | 题号必须互斥；各 split 题号不能重复 |
| `L3.eab60.safety_ids` | 安全题集合，默认放入 holdout |
| `L3.bfcl.enabled` / `required` | 默认 false / false，需配置可信外部命令 |
| `L3.bfcl.command` / `version` | argv 与外部评测版本；支持 `{binary}`、`{output}` |
| `L3.bfcl.readonly_paths` | 外部命令所需题库/模块只读目录，按最小范围授权 |

`L2.coding_eval.runs` 当前是预留声明，快筛实现固定一次；完整重复数读取 L3。阈值在 `benchmarks.thresholds`，默认数值与判定顺序见[评测文档](04-evaluation.md)。

除 BFCL 外，L3 项也支持 `command` 覆盖。这个路径直接以干净环境执行**可信外部评测器**，不会自动为该命令建立与内置适配器相同的计量和沙箱边界。若它会执行候选代码，接入者必须自行提供隔离、计量和结果完整性保证。推荐优先使用内置适配器或显式 BFCL 接口。

## 搜索与补丁规则

`selector.exploit_probability=0.7` 表示通常从通过数最高的 top-k Parent 中选择，其他情况从合格池探索；`top_k` 默认 3。最终按 `1 / ((1+children_count) × (1+no_promote_rounds))` 加权抽样。强制探索也只会选择资格合格的归档。

`diagnosis.similarity_threshold=0.8` 使用文件集合与改动描述的字符串相似度过滤历史失败假设，不是语义向量去重。`retries=2` 表示初次之外最多两次重试，代码也会将该值上限限制为 2。

| `security` 字段 | 语义 |
|---|---|
| `protected_paths` | 候选不可修改的评测、控制器和其他受保护文件 |
| `allowed_paths` | 普通源码允许范围；`Cargo.lock` 有专门处理 |
| `high_risk_paths` | 高风险模式，候选不能自动成为 Parent |
| `dependency_allowlist` | `Cargo.lock` 新增 `source = ...` 行的允许列表，不是通用依赖漏洞豁免 |

尾部 `/` 模式使用路径前缀，其他普通规则使用精确匹配或 fnmatch；高风险判断还支持子串命中，规则可能偏保守。`high_risk_max_status` 当前是声明字段，实际高风险上限由代码固定为归档。路径策略在 L1 检查，不能阻止发生在更早 L0 的构建脚本执行，必须与沙箱配合。
