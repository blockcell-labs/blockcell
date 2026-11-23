# 运行、恢复与排障

[文档首页](README.md) · 上一篇：[开发与贡献](06-development.md)

## 命令语义

所有命令都支持前置 `--config`；它们不会自动发布或 push。

| 命令 | 用途 | 模型费用 |
|---|---|---|
| `doctor` | 检查工具、隔离后端、必选套件和 key 是否存在 | 无 |
| `init` | 初始化源码、构建 Genesis-0、执行 help | 无 LLM 请求；有构建资源开销 |
| `baseline` | 测量 Genesis-0，尽量复用有效缓存 | 缓存未命中时可能产生 |
| `run --cycles N` | 在已有轮数上追加 N 轮目标 | 有 |
| `resume` | 继续未完成的已持久化目标 | 可能有 |
| `status` | 查看执行状态与 Agent 列表 | 无 |
| `report` | 从本地存档生成 JSON/Markdown 汇总 | 无 |
| `reserve-usage --usd ... --reason ...` | 为未知费用明确保留预算并保存说明 | 不调用模型，本身不代表已付款 |

命令退出码是执行层面的结果，不代表候选能力通过。`run` 在预算停止后也可能正常返回报告。读取 `state`、`stop_reason`、候选判定与必选评测状态，才能解释实验结果。

## 文件地图

默认根目录是 `.genesis/`，可由配置替换。

```text
.genesis/
  repository/                 独立 clone 与 detached 提交谱系
  frozen/                     Genesis-0 源码与冻结评测器
  worktrees/<id>/              诊断、候选和审计 worktree
  target/                     共享编译缓存
  build-home/                 隔离构建 HOME / Cargo HOME / TMPDIR
  agents/<id>/blockcell        每个版本独立保存的二进制
  agents/<id>/build.log        构建日志
  archive.sqlite3             结构化状态与结果
  transitions.jsonl           阶段迁移辅助日志
  L0-baselines/               Parent 静态检查与测试基线
  audit-reports/              原始依赖审计输出
  runs/<id>/patch.diff         实际补丁
  runs/<id>/decision.json      判定及阶段证据
  runs/<id>/<benchmark>/       逐题报告、checkpoint 和原始产物
  runs/agent_calls/            每次调用的 HOME、日志、指标和运行产物
  runs/selfcheck/              固定修改任务
  baseline.json               初始能力汇总
  usage.json                  token、费用估值、未知请求与预留
  report.json / report.md      实验汇总
```

`pr_draft.md` 仅在晋级候选的运行目录生成，不是远端 PR。没有晋级候选时缺少该文件正常。

`status` 虽不请求模型，构造 Supervisor 时仍会打开数据库并检查迁移，因此不是严格只读的文件查看工具。不要在没有锁保护的情况下用其他进程直接写 SQLite。

## 中断与恢复

上游故障通常保存 `blocked_reason` / `blocked_at`，保持原阶段。修复服务后用同一配置执行 `resume`；正常阻断的等待时间会从本轮耗时中调整出去。

周期预算、累计 token 或时间耗尽时，`run` 会记录 `STOPPED`。`resume` 在该状态只返回已有报告。要开始新周期，先按停止原因处理预算、环境或父代资格，再运行 `run --cycles N`。若因全局停滞停止，已保存的停滞计数仍会参与判断。新周期从选父代开始，之前未完成的候选保留其已有记录。

尽量用正常信号终止；进程组清理会处理后代进程。突然断电、强制 kill 或文件系统故障仍可能留下未完成产物，恢复时应先备份状态并检查日志，不能保证每个外部动作只执行一次。

## 费用与未知 usage

估值公式是：

```text
known_cost = (input_tokens × input_rate + output_tokens × output_rate) / 1,000,000
spent      = known_cost + reserved_cost
```

默认价格只是示例。`usage.json` 的已知 token 与费用不会因缺失 usage 而虚构增加；缺失或非法 usage 会记录 `missing_usage_requests` 并阻断后续请求。

先核对服务商请求记录。如果暂时无法取得实际账单，但能够给出足够保守的金额，可显式预留：

```sh
python3 -m genesis --config tmp/genesis.local.json reserve-usage \
  --usd 1 --reason '示例：依据未知请求数量及模型输出上限保守预留，实际账单待核对'
```

`1` 只是语法示例，不是推荐金额。预留会累计到 `reserved_cost`，保留未知请求数量、时间与说明，并解除对应未知请求的阻断；它不会生成真实 token 或声称账单已核销。当前 CLI 没有自动账单查询或预留冲销命令。不能可靠确定预留额时应保持停止，勿直接编辑 `usage.json` 清零。

预算检查不能撤回已经发出的模型请求；最多 `concurrency` 个在途请求的最终 usage 仍可能造成超支。并发与单次输出上限应和可接受的费用风险一起设置。

## 常见问题

| 症状 | 检查与处理 |
|---|---|
| doctor ready，但 init/build 失败 | doctor 不构建或验证完整离线缓存；查看 build.log、工具链和锁定源码依赖 |
| 沙箱后端不可用 | 确认系统/执行环境支持 sandbox-exec 或 bwrap；不要关闭真实候选的隔离 |
| Cargo 离线缺依赖 | 在可信准备阶段下载该锁定版本依赖，再重新运行；候选阶段不临时放开网络 |
| cargo-audit 失败 | 检查工具与离线 advisory 库；区分缺资源、格式异常和实际新增漏洞 |
| EAB skipped / required unavailable | 检查套件、题号、固定版本与路径，skipped 不能算作通过 |
| 401 / 402 / 403 | 修复鉴权、额度或接口权限；不应判为模型能力失败 |
| 429 / HTML 限流页 | 检查共享并发和退避；持续限流先暂停，不要用高频手动重试 |
| 5xx / 畸形 JSON / 无 choices | 核对服务可用性和兼容性；保留恢复点，不应缓存为能力失败 |
| usage missing / invalid | 核对实际费用，必要时按上述方式保守预留；不要补零 |
| unfinished run: use resume | 当前仍有未完成阶段，用同一 workspace 的 resume |
| frozen SHA mismatch | 新建 workspace_root；不要在原 frozen 中手动切换提交 |
| 工作区锁冲突 | 找到仍在使用该工作区的进程；不要删除锁文件绕过排他限制 |
| 所有候选被拒绝 | 先读 decision.json，可能是正常门禁结果；不要先放宽评分阈值 |
| 测试写 /tmp 被拒绝 | 修复 fixture 使用授权临时目录；本地 mock 使用独立测试 profile，见开发文档 |

上游完整 JSON 转 SSE 是默认模式。开启 `upstream_stream=true` 会改变评测身份；遇到异常响应后程序不会自动切换模式并重放原请求，避免重复费用与副作用。

## 备份与公开证据

暂停写入后备份实验目录，SQLite 使用 WAL 时需保留一致快照，可使用 SQLite backup 机制或完整停止后复制。仅复制主 `.sqlite3` 文件可能遗漏未 checkpoint 的数据。

保留运行配置、源码/二进制身份和外部套件版本，避免单独复制报告后丢失复现上下文。工作区包含模型轨迹与任务文件，应视为敏感实验数据，发布前单独脱敏。

更换 Parent SHA、外部题库或安全边界时，优先建立新实验目录。不要将一台机器某个工作区的成功，直接作为其他平台、其他 SHA 或更强威胁模型的验收结论。
