# 修复后复审与后续优化指导

日期：2026-11-12。审查范围：`5ca38be..3f55126` 六批修复，对照原 32 个跟踪条目的验收要求。此次只更新审查材料，不修改生产代码。

结论：不能维持“全部关闭”的结论。四项遗漏已通过公开 API 探针复现：两项取消生命周期遗漏、技能空续轮丢失答复，以及企业微信正文服务端拒绝被误报成功。重新打开 B4-03 / N-07 / B2-02，并为 B4-04 补充服务端确认的验收要求。其余已审路径本轮未新增确认缺陷，不代表所有边界均已证明正确。

## 补修实现（2026-11-12）

用户授权修复 F-01 / F-02 / F-03，并明确 F-04 不改动。上文发现及下文失败探针保留为修复前证据；当前实施状态如下，最终验证记录见文末。

| 条目 | 本轮实现 | 正式回归 |
| --- | --- | --- |
| F-01 | lease 记录 receipt；Drop 同步取消，活跃 claim 由异步收尾等待退出后释放 lease；run_loop 停机等待收尾 | `crates/scheduler/tests/remediation_review.rs`：中断后无重复、退出后可重试、停机等待确认 |
| F-02 | receiver 关闭时通知取消，sender 被消费后仍保留活跃 claim 状态；多次取消可等待同一退出确认；claim Drop 回收状态 | 同上清理/等待回归，以及 core 回执重复取消、完成后活跃状态保留回归 |
| F-03 | 保存续轮前答复及 revision，收尾复用普通循环 merge；空答复说明验证范围及缺失产物，revision 改变/检查失败时不恢复旧结论 | forced-skill 真实入口集成回归；技能循环未再修改、再次修改、检查失败三个空续轮回归 |
| F-04 | 按用户要求未修复，仍保持打开 | 原模拟服务拒绝证据保留 |

F-03 回归额外确认：工具返回 Ok JSON 且进程 exit_code 非零时，原证据层未标记验证失败。本轮将非零退出码及 timed_out 纳入失败证据，避免技能和普通循环将此类结果当作可恢复旧结论的状态。

## P2 / F-01：tick 中断过早释放执行租约

- 对应 B4-03 / N-07。`crates/scheduler/src/cron_service.rs:23` 的 `FlightLease::drop` 直接删除 in-flight 记录；`crates/core/src/message_receipt.rs:103` 的 guard 析构只发取消信号，不等待 runtime 放下 active claim。
- `run_loop` 停机路径会 `abort_all`；外部直接取消 `run_tick` 也进入此路径。下一次 tick 在旧任务尚未确认结束时可以重新派发同一个 job。已经发出取消信号不等于旧执行已经停止。
- 实测：领取首条派发的 receipt 并保持 claim，abort tick，启动下一 tick；旧 claim 未释放时已收到第二条派发。探针 `review_dropped_tick_must_keep_claimed_job_leased_until_ack` 失败。
- 后续修复：让租约生命周期覆盖取消确认，包括 future 被直接丢弃时；同步 Drop 不能承担异步等待，需明确取消收尾的所有者。验收同时覆盖活跃任务中断、停机收尾、排队未领取、正常完成，并验证旧任务退出后可以重试。

## P2 / F-02：回收关闭 receiver 时丢失活跃任务的取消状态

- 对应 N-07。`crates/core/src/message_receipt.rs:29` 只按 `sender.is_closed()` 从 registry 删除条目，没有通知条目中的 `ClaimState`，也没有保留等待 active claim 的通道。
- 已领取任务的 receiver 被丢弃后，另一笔登记触发清理。随后对旧 ID 调用 `cancel_message_receipt_and_wait` 因查无记录立即返回，旧 claim 的 `cancelled()` 却一直不收到通知。
- 实测：register → claim → drop receiver → register another → cancel-and-wait，等待取消通知 100ms 超时。探针 `review_receiver_reclaim_must_cancel_the_active_claim` 失败。此探针证明生命周期断裂，不声称已复现真实模型造成的重复外部副作用。
- 后续修复：区分无人领取的废弃回执与活跃任务；清理必须传播取消，并保留等待确认所需状态。覆盖清理和超时/显式取消并发发生的情况。

## P2 / F-03：技能补充续轮为空时丢失最终答复

- 对应 B2-02。`crates/agent/src/runtime/turn_flow.rs:423` 起将原答复写入 trace 后进入补验续轮，但最终在第 465 行直接使用 `response.content.unwrap_or_default()`。
- 普通循环在 `process_message_inner.rs:2218` 使用 `merge_verification_answer`，按 revision 和失败证据决定保留原结论或明确说明验证范围。技能循环未复用这段逻辑；补验后空 content 且无工具调用时，可能以空字符串结束，先前完整答复只留在审计记录。
- 实测：通过 `AgentRuntime::process_message` 的 `forced_skill_name` 真实入口，声明缺失产物 required.json。受控 Provider 首次给出完整的无法交付说明，第二次返回空 content；确认恰好两次调用，最终返回空字符串。探针 `forced_skill_empty_continuation_must_deliver_an_explanation` 失败。此用例验证缺失产物续轮；修改后补验的空答复目前仍只有共享代码分支的静态证据。
- 后续修复：统一技能/普通循环的有限续轮收尾规则，空续轮至少交付明确的未完成说明；保留旧结论须检查 revision 和验证失败证据。补充修改 revision 后空答复、验证失败后空答复回归，不得无条件恢复已失效结论。

## P2 / F-04：企业微信正文被服务端拒绝仍返回成功

- B4-04 的补充边界：先前修复覆盖媒体服务端拒绝、正文 socket 写入失败；正文服务端确认仍未贯穿返回值。
- `crates/channels/src/wecom/longconn.rs:118` 在 `write.send` 成功后立即完成 `result_tx`；后续服务端错误在第 328 行起只记录日志。`wecom/send.rs:23` 等待的是 socket 写入结果，无法得到服务拒绝。
- 实测：公开 `WeComChannel::run_loop` 连接本机模拟 WebSocket，公开 `send_message` 发送正文，服务端针对该 req_id 返回 `errcode=40001`；确认模拟服务已发送拒绝，调用方结果仍为 Ok。探针 `rejected_text_must_not_report_success` 失败。此用例不连接真实企业微信，不推断具体业务账号的响应行为。
- 后续修复：按协议可靠关联正文请求与服务确认，拒绝、超时、断连均返回错误；使用真正唯一的请求标识或为同 req_id 明确串行规则，避免并发串配。媒体上传独占读流时也必须正确分发正文确认。部分消息已成功时保留部分失败语义，不自动重发整条消息。

## 下一步执行与关闭条件

| 顺序 | 条目 | 必须观察到的行为 |
| --- | --- | --- |
| 1 | F-02 / N-07 | receiver 关闭后登记其他任务，旧活跃 claim 收到取消；cancel-and-wait 在旧 claim 释放前保持等待，释放后结束；未领取条目仍可回收 |
| 2 | F-01 / B4-03 | 中断 tick 后，在旧 claim 确认退出前不能收到同 job 的第二次派发；退出后重试不被永久阻塞；停机确实收尾 |
| 3 | F-03 / B2-02 | forced-skill 空续轮返回非空、真实的完成/未完成说明；revision 改变或验证失败时不复活失效结论；提示次数仍有限 |
| 4 | F-04 / B4-04 补充 | 正文拒绝、确认超时、断连返回 Err；服务确认成功返回 Ok；并发正文/媒体请求不串确认，部分失败不重复已成功内容 |

每项先把对应证据探针移入正式测试并确认失败，再修复、验证相关包，最后按批提交。F-01/F-02 共享取消状态与收尾所有权，应先统一设计，避免分别修复后又相互绕过。四项均通过后才能关闭本轮复审，Windows 原生验证缺口单独保留。

## 验证材料与边界

- Rust 1.96，离线锁定依赖，命令：`cargo test -p blockcell-scheduler -p blockcell-agent -p blockcell --offline --locked --test remediation_review`（显式设置相应 RUSTC/RUSTDOC）。结果：两个探针均失败，编译完成。
- 探针源码：[evidence/remediation_review.rs](evidence/remediation_review.rs)；原始结果：[evidence/remediation_review.log](evidence/remediation_review.log)。源码放在证据目录，避免将预期失败测试留入日常测试集合。复跑时将其复制到 `crates/scheduler/tests/remediation_review.rs`，结束后删除该临时副本。
- 既有全套测试通过记录仍有效，但没有覆盖上述边界。本轮没有重跑全部测试，没有连接真实模型或 IM。Windows ZIP 原生编译/运行仍是验证缺口，不作为已确认的新缺陷。
- 技能探针：[源码](evidence/remediation_skill_review.rs)、[日志](evidence/remediation_skill_review.log)。放入 `crates/agent/tests/`，同上述 package 集合执行 `--test remediation_skill_review`，1 项失败，最终答复为空。
- 企业微信探针：[源码](evidence/remediation_wecom_review.rs)、[日志](evidence/remediation_wecom_review.log)。放入 `crates/channels/tests/`，上述 package 集合加 `-p blockcell-channels`，执行 `--test remediation_wecom_review`，1 项失败，服务端拒绝仍返回 Ok。本机沙箱最初禁止绑定 loopback，授予该模拟测试所需权限后得到此业务断言失败；权限失败未作为缺陷证据。
- 修复前四项预期失败的缺陷探针均保存到 evidence；F-01/02/03 的改进版已纳入正式测试，F-04 仍只保留审查证据。证据测试中的 Provider 与服务端为受控替身，未声称覆盖所有生产竞态或真实模型/IM。


## 本轮补修验收结果

F-01 / F-02 / F-03 的正式回归均通过，本轮这三项关闭；F-04 按用户要求保留打开。取消类修复提交：`d5dab2c`；技能类修复与本文同步提交。

Rust 1.96，显式 RUSTC/RUSTDOC，离线锁定依赖。执行：

```sh
cargo test -p blockcell-core -p blockcell-scheduler -p blockcell-agent -p blockcell --offline --locked --lib --test remediation_review --test remediation_skill_review --test cron_receipt_lifecycle
```

结果：Agent 1003、core 149、scheduler 78，以及集成 3 + 1 + 3，共 1237 项通过。完整结果摘要见 [验证记录](evidence/followup-fixes-validation.txt)。既有 dead-code 警告不属于本次变更。F-04 的 channels 生产目录无 diff；未连接真实模型或 IM。
