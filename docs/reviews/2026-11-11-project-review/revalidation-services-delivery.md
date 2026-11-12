# 服务与交付链路复核（B4 / B6）

复核日期：2026-11-12。基线仍为 `main` / `5ca38be861b4a3e8f7a1e8d332e242b42ed25710`。本报告复核 B4 的 4 项、B6 的 3 项及被前置缺陷阻断的安装完整性问题，并沿相同调用链检查遗漏。只新增本文件；不修改生产代码、既有测试及既有报告。

`confirmed` 表示当前源码已形成可核对的缺陷链，不等于完整集成测试通过；`qualified` 表示结论依赖明确前置条件；`needs-test` 表示尚需行为测试界定影响；`rejected` 表示原结论不成立。本轮 7 项既有发现全部保留为 confirmed，另确认 2 项新 P2，安装完整性问题保留为 qualified P1，不与直接可达的确认数混计。

## 复核结论

| 原编号 | 等级 | 分类 | 复核后的结论与限定 |
| --- | --- | --- | --- |
| B4-01 | P1 | confirmed | Anthropic 原生 Messages 流式响应丢失输入/缓存 usage，正常输出增量又整体覆盖旧 usage；不扩展到非流式路径。 |
| B4-02 | P2 | confirmed | 四个适配器对每个网络 chunk 单独做有损 UTF-8 解码，字符跨 chunk 时正文及工具参数损坏。 |
| B4-03 | P2 | confirmed | 同 tick 的任务串行等待完成回执，run_loop 在此期间不能选择 shutdown；不再声称持有 operation lock 阻塞写 API。 |
| B4-04 | P2 | confirmed | 多渠道吞掉媒体错误；WeCom 长连接正文丢失还发生在媒体发送成功时，原报告范围偏窄。 |
| B6-01 | P1 | confirmed | 实际执行 workflow 的 manifest shell 段，4 个正常资产产生 `artifacts=[]`，步骤仍退出 0。 |
| B6-02 | P1 | confirmed | 完整原始安装脚本确认 grep 收到以 `-` 开头的模式并解析为选项；`--amd64` 报错不是 shell 变量被 mock 覆写。 |
| B6-03 | P2 | confirmed | 完整原始 runner CLI 在有输出后超时时 JSON 序列化抛错，报告文件未生成。 |

## B4 逐项复核与验收条件

### B4-01：Anthropic 流式计量

证据仍成立：`crates/providers/src/anthropic.rs:861` 的 `AnthropicStreamEvent` 没有 `message` 字段，`:713` 的 `message_start` 只读取事件顶层 `usage`。原生 SSE 的初始计量位于 `message_start.message.usage`，因此未进入累积结果。`:734` 对 `message_delta.usage` 整体重建 JSON，缺失的输入/缓存字段被写成 null。

下游 `crates/agent/src/runtime.rs:997` 把非数值/不存在的 token 字段归为 0，`runtime/turn_flow.rs:174` 将其送入会话预算及父预算 tracker。以原报告的 10,000 input / 10 output 示例，计入 tracker 的只有输出 10；配置了输入价格时输入费用同样少计。该影响依赖调用流式方法以及相应预算/计价配置，不声称非流式 `normalize_usage` 同样出错。

验收必须同时覆盖：标准嵌套 `message_start`、只含累计 output 的最终 delta、缓存 read/creation 字段、额外中间 delta。最终数值必须保留先前输入和缓存量，只覆盖实际出现的累计字段；通过运行时入口断言会话及父预算计入输入 10,000 和输出 10，并验证非流式结果保持一致的字段契约。不能只为非标准顶层 start usage 写测试。

### B4-02：流式 UTF-8 缓冲

重新核对 `openai.rs:871`、`anthropic.rs:611`、`gemini.rs:544`、`ollama.rs:490`，仍为 `buffer.push_str(&String::from_utf8_lossy(&bytes))`。有损解码在缓存到完整 JSON 行之前发生；网络分段不保证按 UTF-8 字符对齐。既有最小 Rust 程序复现的替换字符机制与四处当前实现一致。本次未重复运行同一独立程序，也未将它冒充完整适配器测试。

修复应让未完成的字节序列跨 chunk 保留。验收在四种实际流协议中，将中文、emoji 和含非 ASCII 路径的工具参数在每个字节边界拆分；同时覆盖 `\r\n`、多事件同 chunk、行跨 chunk。要求最终内容与原始内容逐字节一致，不出现替换字符。单纯断言 JSON 能解析不足以防止此问题。

### B4-03：cron 等待、并发与停机

`cron_service.rs:503` 依次 await 每个 `execute_job_internal`；`:723` 等待 runtime 完成回执，生产上限为 `:17` 的 900 秒。`run_loop:865` 在 tick 分支中 await 整个 `run_tick`，所以一个任务可能延误下一任务最多一个回执等待周期，多个慢任务会累积。operation lock 已在 `:498` 释放，原写 API 被整段等待阻塞的历史论点应排除。

验收使用可控的挂起任务而非真实等待 15 分钟：A/B 同时到期，A 不完成时 B 仍应在指定派发时限内进入执行；shutdown 应在指定时限内终止调度等待；同一 job 在下一 tick 不能重叠启动。实现并发时须先设计 in-flight 状态、完成结果写回和删除/禁用竞态，否则直接并发会放大重复派发窗口。超时是否取消实际任务另见下方 needs-test 项。

### B4-04：媒体错误传播与 WeCom 正文

多个媒体分支仍是记录 Err 后继续，`manager.rs:627` 最终返回 `Ok(())`。因此仅有媒体、全部发送失败时，上层确实收不到失败结果。普通正文发送使用 `?`，所以不把“附件失败但正文成功”无条件描述为整条消息完全丢失。

WeCom 的范围需要扩大：`manager.rs:507` 给第一个媒体传正文，`:522` 在长连接存在媒体时跳过单独正文。但 `wecom/media.rs:679` 把参数命名为 `_caption` 且完全未读取，`:711` 的 `LongConnOutbound::Media` 不含 caption；`:443` 构建的实际 `aibot_send_msg` 只有媒体内容，没有正文。于是**非空正文加媒体的长连接消息，即便媒体上传成功，正文也未发送**。附件读取失败（`media.rs:322`）则连媒体也丢失并被 manager 吞错。另 `media.rs:737` 附近无活动长连接时直接返回 Ok，是同一失败传播链的补充证据。

验收至少包括：media-only 全失败时返回可被调用者识别的失败；正文加首个附件失败时正文有补送或明确失败；正文加附件成功时真实出站协议包含正文；无活动长连接时不能声称投递成功。部分成功的契约需明确，不能只修改日志等级或只保留 caption 变量。

## B6 逐项复核与验收条件

### B6-01：完整 manifest 步骤仍发布空资产列表

`.github/workflows/release.yml:370` / `:372` / `:374` / `:376` 的正则位于 YAML literal block，两个反斜杠会原样交给 Bash，不能按 JSON 字符串转义理解。正常资产命名已对照 `:266` 的 Prepare raw updater binary 步骤及 build matrix。

这次从当前 workflow 截取**完整** Generate updater manifest shell 段，只把 GitHub 表达式替换为固定仓库名。临时目录提供四个正常文件名的 checksums 条目，TAG=v0.1.8，签名公私钥均为空，使用本机 Bash/jq 执行；没有调用 Actions、联网或接触密钥。结果：

```text
actual_manifest_step_exit=0
normal_assets_supplied=4
manifest_artifacts=[]
stderr=
```

这进一步排除“只截取正则改变真实上下文”的解释。验收应执行实际 shell 段，断言四个预期 OS/arch 组合各出现一次，URL basename 和 SHA 对应输入；缺任何目标、空数组和重复目标应使发布失败。签名配置组合及签名载荷绑定另保留原有覆盖；本次不声称真实签名/发布成功。

### B6-02：grep 参数错误，非变量覆写

完整原始 `install.sh` 使用 PATH 外部 mock 验证，未抽取 shell 函数、未覆盖内部函数。uname 仅返回 Linux / x86_64，curl 仅返回正常 `blockcell-v0.1.8-linux-amd64.tar.gz` JSON；grep 包装器记录 argv 后直接执行 `/usr/bin/grep`。git/npm/cargo/包管理器等替换为无副作用的失败哨兵，安装目标位于自动清理的临时目录。

```text
original_script_exit=1
grep args: <-F -linux-amd64.tar.gz>
grep: unrecognized option `--amd64.tar.gz'
Failed to find a matching release asset for OS=linux ARCH=amd64 VERSION=v0.1.8
installed=False
```

argv 明确包含 linux；BSD grep 把 `-linux...` 当组合选项解析后产生该诊断。因此旧错误文本中的 `--amd64` 不代表 os 被 mock 或函数全局变量覆盖。`:96` 缺 `--`/`-e` 才是根因；末端 head 还掩盖了上游 grep 失败状态，导致误返回资产不存在的 2，auto 会走源码 fallback。

验收需要完整入口的成功 release 用例，并断言未调用 git/npm/cargo；同时断言 auto 只有真正不支持的平台/没有资产才 fallback，API/解析错误返回独立失败。错误 SHA 与不安全归档的测试必须证明已到达相应校验点，不能以任意提前失败满足断言。该修复必须依赖下节完整性失败传播修复同时合入。

### B6-03：完整 runner 报告落盘失败

本次直接运行当前 `tests/coding_eval/runner.py` CLI，选 bugfix-01-divide，timeout=1 秒；agent-command 只是本机 Python 打印并 flush `timeout-marker` 后 sleep 3 秒。输出路径是临时目录，未调用任何模型，runner 自己创建并清理 fixture workspace。

```text
original_runner_exit=1
report_written=False
TypeError: Object of type bytes is not JSON serializable
```

`runner.py:165` 的异常分支保留 bytes，`:227` 带到报告，`:283` 失败。虽然子进程指定 text=True，TimeoutExpired 的捕获输出在本机 Python 3.12 下仍为 bytes，不能以该标志推翻结论。修复需统一 None/bytes/str 到 str；验收应包含输出后超时且最终 JSON 可读取、两任务中第二个超时仍保留首任务结果，并明确 timeout 的分类/通过判定契约。不能只覆盖“无输出的超时”。

## qualified：安装完整性失败仍继续安装（建议 P1）

这不是 B6-02 的另一种描述，而是其修复后暴露的独立失败传播缺陷。`install.sh:294`、`:295` 调 checksum/archive 校验时没有显式检查返回值，`:298` 提取亦未检查；release 从 `if ! install_from_release`（`:335`）调用，auto 从 `install_from_release || ...`（`:345`）调用。这些条件上下文抑制函数体中的 errexit，顶部 `set -e` 不能代替显式返回。

本次加强了既有提取函数复现：运行完整未改源码的脚本，仅 PATH grep 包装器给资产后缀加 `--`，明确模拟 B6-02 修复；curl 使用本地 fixture，真实 tar 解开无害哨兵脚本。两类 fixture 分别为合法归档配错误 SHA、正确 SHA 配含一个无害符号链接的归档。目标预先放置临时旧二进制，不执行哨兵：

```text
case=bad_checksum, method=release, original_script_exit=0, previous_binary_replaced=True
case=bad_checksum, method=auto, original_script_exit=0, previous_binary_replaced=True
case=unsupported_link, method=release, original_script_exit=0, previous_binary_replaced=True
case=unsupported_link, method=auto, original_script_exit=0, previous_binary_replaced=True
```

四组均记录相应 checksum/link 校验错误，却继续 `Extracting...` 并打印 `Installation complete.`。使用真实归档/真实 tar，未通过 shell 函数替换来构造全局变量干扰，也未做真实安装或路径穿越。

限定：未修改的默认入口目前先被 B6-02 阻断，因此标 qualified，而不是声称当前默认下载路径已经成功绕过校验。风险是单独修好资产查找就会使该链可达。验收必须同时覆盖 release/auto 的错误 SHA、缺失校验项、归档校验失败、提取失败：都返回非零，不提取/安装已拒绝的归档，不走源码 fallback，原已安装文件字节保持不变；成功归档仍可安装。

## 新确认项

### NEW-SD-01 · P2 · confirmed：cron 丢弃创建任务的渠道账号，改由默认账号发送

前提：渠道配置多个账号，在非默认账号的会话中通过 cron 工具创建 deliver 任务。`ToolContext.account_id` 已存在（`crates/tools/src/lib.rs:554`），因此上下文有足够信息。

完整丢失链：`tools/src/cron.rs:476` 只提取 channel/chat_id，`:179` 的持久 payload 只保存 channel/to；`scheduler/src/job.rs` 的 JobPayload 没有账号字段；`cron_service.rs:623` 等生成 deliver metadata 时未传账号，`:690` 的 inbound account_id 为 None。reminder 快路径 `runtime/process_message_phases.rs:71` 和 agent 完成分支 `runtime/turn_flow.rs:973` 都使用 `OutboundMessage::new`，没有补账号。该构造函数默认 account_id=None，`channels/manager.rs:40` 的账号选择逻辑遂选配置的默认账号。

例如 Telegram 配置 A 为默认、B 为另一个 bot，同一用户已分别与 A/B 开始私聊；用户在 B 设置提醒，触发后从 A 发出。若仅与 B 建立会话，A 发送还可能失败。此处确认的是错误账号/丢失投递，不泛化为已证明向另一用户泄漏；它与 B2 的任务归属读取/取消缺陷根因位置和用户行为不同。

修复依赖：扩展 JobPayload 的可选 origin/delivery account 字段及持久化兼容；cron 工具、scheduler metadata、reminder/agent 出站和相关历史 session 目标应统一传播。验收需两个同 channel/chat_id、不同 account_id 的任务跨保存/重载/重启，分别断言出站账号仍为创建账号；legacy 无账号任务维持明确的默认选择规则。本项为完整静态调用链确认，未做真实 IM 或本项的 Rust 集成测试；主审的工具链验证另见总报告。

### NEW-SD-02 · P2 · confirmed：全局回执 10 分钟清理早于 cron 15 分钟合法等待

`core/src/message_receipt.rs:14` 的 STALE_RECEIPT_TTL 为 600 秒，`:28` 每次 register 时按注册年龄清理**所有**旧 sender，不判断 receiver 是否仍打开。cron 则允许等待 900 秒。gateway 在 `bin/blockcell/src/commands/gateway.rs:1205` 为每个 agent 创建并启动独立 CronService，registry 却是同一进程的 static；因此单个 service 的串行 tick 不会阻止其他 service 触发清理。

可达时序：agent A 的正常 cron 任务在第 0 秒开始，第 660 秒才结束；agent B 在第 601 秒注册新任务回执，清除 A 的 sender。A 的 `receipt_rx` 立刻得到 channel closed，`cron_service.rs:725` 将其记录为 runtime dropped receipt，尽管其 900 秒等待尚未到期、任务也未结束。A 随后真实完成时 `complete_message_receipt` 已找不到条目，无法将失败改回成功。At 任务的错误分支还保留过期 next_run，可能再次派发；重复执行的实际后果需要下节生命周期测试界定。

修复应以 receiver 已关闭/显式截止时间识别可回收回执，或将清理期限与调用者的合法等待期限协调；只把 600 改大仍应检查不同调用者的契约。验收通过可注入时钟构造两 service：A 在 600 秒后仍有效且能在 900 秒前完成，B 的 register 不得使其失败；已放弃 receiver 的条目可回收且不会无限积累。不能依赖当前 `cfg(test)` cron 5 秒超时测试证明生产 600/900 秒边界正确。本项由直接常量及 sender 丢弃语义确认，未真实等待 11 分钟或运行 Cargo。

## needs-test 与排除项

**cron 超时后的执行/重试生命周期需要补行为验证，不额外计数。** `cancel_message_receipt` 只从 registry 移除 sender，不持有任务 AbortToken；`runtime/message_task.rs:148` 继续 await process_message，只有任务结束才尝试 complete。At 错误分支保留已到期时间。下一次派发同 job 时，`runtime/run_loop.rs:281` 起的 steering 分支又可能把消息路由到同一个活动任务并直接 continue，未把新 receipt 交给任务完成逻辑。因此不应简单声称“超时立即取消”或“必然并行启动第二任务”。需用可控长任务跨过 deadline，验证工具副作用、steering、旧/新回执归属、任务终止后下一 tick 及是否重复执行，再决定是否独立立项。此验收也应纳入 B4-03 的修复设计。

Lark 的提前写 dedup 集合及失败重试仍维持原报告待验证状态；没有用未运行的 handler 测试提升为 confirmed。公开签名关闭配置、已明确停用的 CI workflow、已修复的 operation lock 历史问题，也未重新列为缺陷。

## 修复依赖与验证边界

1. B6-02 与安装完整性失败传播一起修，先加入完整 release 成功路径及真正到达校验点的失败用例；B6-01 可并行修，但发布验收须检查实际 manifest 内容。
2. B4-01 修复输入/缓存计量后通过会话和父预算验证；B4-02 统一字节缓存后覆盖实际协议分段。
3. B4-04 包含“媒体成功仍丢正文”用例；NEW-SD-01 贯穿 cron 持久化和两种 delivery 分支，不能只改 scheduler inbound。
4. B4-03 与 NEW-SD-02 共用一组时间/回执/停机测试，并先明确超时任务的继续、取消或重试契约。
5. B6-03 验收以最终报告成功落盘为准。

本次动态执行了完整 manifest shell 步骤、完整原始安装入口的 grep 失败、模拟前置修复后的四组真实归档安装校验、完整 coding_eval 超时 CLI。全部使用临时文件与本地模拟，无外网、模型调用、真实安装、发布或密钥使用。本分审未运行 Cargo、真实 SSE、IM 或调度器集成测试。主审随后发现本机已安装 1.96，实际通过 core/storage 共 237 项测试并运行 API 探针；不能再将默认 1.92 视为总体阻断，详见总报告。
