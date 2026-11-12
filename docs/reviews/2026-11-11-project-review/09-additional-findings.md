# 新增缺陷与条件缺陷（2026-11-12）

新增独立确认项 **7 项：P1 1、P2 6**。与原 24 项合计 **31 项：P1 10、P2 21**。另列条件缺陷 C-01（P1），不混入 31 项；总跟踪条目为 32。这里统一编号，复核笔记中的临时编号按下文映射。

## N-01 · P1 · 默认解压目标没有写授权

位置：`crates/agent/src/runtime/path_security.rs:273`、`crates/tools/src/file_ops.rs:552`。对应工具笔记 N01。

`file_ops` 解压时仅登记源文件 Read，只有显式 destination 才登记 Write；执行层省略 destination 时却写入归档父目录。前提是外部归档允许读取、父目录写入未批准或被拒绝，且宿主文件权限允许写入。工具实际在宿主进程执行，WorkspaceWrite 标签不提供原生沙箱；不需要符号链接，故不与 B3-02 重复。

证据：生产授权链静态核对；真实 FileOpsTool 在临时工作区外解压，无 destination 时确实创建同目录产物（API 输出 NEW-implicit-decompress-destination）。该 API 探针不经过完整 Runtime，授权遗漏由静态链证明。

修复：授权前统一计算有效目标，默认/显式目标共用 Write 准入；同时保留 B3-02 的逐产物路径保护。

验收：读允许、写拒绝时无任何产物；只有读批准时单独请求写批准，非交互模式拒绝；获写批准才成功。ZIP/tar、默认/显式目标均覆盖。

## N-02 · P2 · 过期渠道确认吞掉下一条普通消息

位置：`bin/blockcell/src/commands/gateway.rs:1102`、`:1341`；`crates/agent/src/runtime/path_security.rs:13`、`:736`、`:850`；`crates/agent/src/runtime/tool_pipeline.rs:1027`。

渠道确认将 response sender 放入 pending map，未记录截止时间，也没有在接收端超时/取消时移除。路径确认等待 300 秒，脚本确认等待 120 秒后可以结束，但 map 仍保留 sender。相同 channel/account/chat/sender 的下一条消息被 remove 当作确认回答；send 失败被忽略，随后无条件 continue，消息不再进入 runtime。此期间新确认还会因 contains_key 被拒绝。

证据为当前完整静态生命周期链，未真实等待超时。现有确认 scope 已包含 account/chat/sender，本项不是跨账号批准漏洞。

修复：确认条目持有 deadline 与取消状态；超时/取消主动清理，消费消息前检查存活及期限。过期条目不得吞消息，清理须校验 request_id，防止旧请求清掉新请求。

验收：可控时钟推进至超时后，下一条普通消息恰好投递一次；新确认可创建；有效 yes/拒绝仍只消费一次；取消及截止时刻竞争不泄漏条目或误批准。WS 的 pending 生命周期也应一起检查，但不另计一个已证实用户后果。

## N-03 · P2 · 渠道 slash 回复丢失账号

位置：`bin/blockcell/src/commands/gateway.rs:1397` 及其 PermissionDenied/Error/ExitRequested 分支；`crates/core/src/message.rs:91`；`crates/channels/src/manager.rs:35`；`bin/blockcell/src/commands/gateway/outbound.rs:24`。

同一渠道配置默认账号 A 和非默认账号 B，用户向 B 发送由 Gateway 直接处理的 `/help` 等命令。回复只构造 channel/chat/content，OutboundMessage.account_id 为 None，转发不补齐，manager 选择默认账号 A。结果从错误 bot 回复，或因 A 无法联系该 chat 而发送失败。转发到 Runtime 的命令不应一概算作此路径。

证据为入口到账号选择的静态链，未调用真实 IM；不声称已证明发给另一用户。修复统一使用保留来源账号的 reply 构造入口，覆盖成功、权限拒绝、错误及不支持命令回复。

验收：两个账号共享人工 chat id，成功/失败四类回复均明确投递原账号；无账号 legacy 消息保持约定默认行为。与 B5-01 同批修复渠道 `/clear` 的 account-qualified session key，但清理错误仍归 B5-01，不重复计数。

## N-04 · P2 · 每模型 Max Tokens 设置无效

位置：`webui/src/components/llm/llm-page.tsx:599`、`webui/src/lib/config-merge.ts:38`、`crates/core/src/config/provider.rs:91`、`crates/providers/src/pool.rs:158`、`crates/providers/src/factory.rs:167`。对应 FE-N01。

UI 可编辑并保存 modelPool[].maxTokens，但 ModelEntry 无该字段，反序列化后丢失；Provider 构建也未消费它。保存成功不代表生效，与 B1-04 数值被掩码是独立根因。真实 ModelEntry API 序列化证实字段消失（输出 NEW-model-token-setting）。

修复选择应明确：实现逐模型覆盖及默认回退，或将 UI 改成实际支持的全局/Agent 设置。不能只增加 serde 字段而不传到请求。

验收：两个模型配置不同限额，保存/重载与 mock Provider 实际请求均符合设置；缺省回退和非法范围有明确规则。详见基础/入口复核笔记。

## N-05 · P2 · 非默认 Agent 的流可列出但管理接口无法停止

位置：`bin/blockcell/src/commands/gateway/streams.rs:7`、`:24`；`crates/tools/src/stream_subscribe.rs:1133`、`:1408`；`bin/blockcell/src/commands/streams_cmd.rs:155`。对应 FE-N02。

GET 列出全局流，DELETE 却始终传默认 workspace；ops 等 Agent 的流归属不同目录，工具层 owner 检查拒绝，管理 API/CLI 返回不存在、流仍运行。工具层检查正确，不应通过删除 owner 校验来修复。证据为静态调用链，没有建立真实外部流。

修复让管理列表/操作保留 owner Agent，并使用相应 Paths；或在受权管理层明确解析所有者。

验收以 fake registry/cancel handle 创建默认和 ops 流：各自停止只取消自身并更新正确目录中的恢复规则；错误 owner 不影响流，CLI 同样可定位和停止。

## N-06 · P2 · cron 持久化及投递丢失来源账号

位置：`crates/tools/src/cron.rs:476`、`crates/scheduler/src/job.rs` 的 JobPayload、`crates/scheduler/src/cron_service.rs:623`、`:690`、`crates/agent/src/runtime/process_message_phases.rs:71`、`crates/agent/src/runtime/turn_flow.rs:973`。对应 NEW-SD-01。

非默认渠道账号创建的 deliver 任务只持久化 channel/to，scheduler 生成的 inbound 和两种出站分支都没有恢复 account_id；最终由默认 bot 发送或失败。区别于 N-03 的即时 slash 回复，本项涉及持久化 schema 和定时投递生命周期，需独立验收。静态全链确认，未真实 IM 投递。

修复将可选来源/投递账号贯穿工具、JobPayload、metadata、reminder 和 agent 完成回复；定义旧任务无账号时的兼容规则。

验收两个账号同 channel/chat 的任务跨保存、重载、重启，仍分别由原账号投递；两种 delivery 路径均覆盖，旧任务默认行为明确。

## N-07 · P2 · 回执清理期限短于 cron 合法等待期限

位置：`crates/core/src/message_receipt.rs:14`、`:28`；`crates/scheduler/src/cron_service.rs:17`、`:723`；`bin/blockcell/src/commands/gateway.rs:1205`。对应 NEW-SD-02。

全局 registry 在 register 时按年龄清掉超过 600 秒的 sender，cron 允许等待 900 秒；不同 Agent 各有调度服务但共用 registry。A 在 0 秒启动、660 秒正常完成，B 在 601 秒注册会提前丢掉 A 的 sender，A 被记录为 runtime dropped receipt；随后正常完成无法补回结果。单 service 串行不构成防线。静态常量和 sender 生命周期确认，未实际等待 11 分钟。

修复按 receiver 存活及调用者 deadline 清理，协调回执生命周期；仅扩大常量不能解决其他调用者契约问题。与 B4-03 一起设计 in-flight、超时及停机。

验收可注入时钟下两个 service 的有效等待互不清除，放弃的 receiver 可回收；超过 600 秒但不足 900 秒的合法完成仍成功。超时后的重试、副作用与 steering 需要额外行为测试，暂不宣称必然重复执行。

## C-01 · 条件 P1 · 校验失败仍继续安装

位置：`install.sh:294`、`:295`、`:298`、`:335`、`:345`。旧报告已记录线索，本轮强化证据，**不算新发现**。

B6-02 当前阻断正常资产查找。模拟其修复后，条件调用上下文抑制 errexit，而校验/提取步骤无显式失败返回，错误 SHA 或被拒绝的链接归档仍继续安装。完整原脚本仅通过外部 grep 包装器模拟 `--` 修复，真实无害 tar 和临时安装目录验证：错误 SHA/不支持链接 × release/auto 四组全部退出 0，旧临时二进制被替换。没有真实安装或外联。

这是独立失败传播根因，必须与 B6-02 同批修复，不能等待前置修复上线后再处理。显式传播校验、下载、提取失败，仅对约定的“无资产/不支持平台”允许 auto fallback。

验收必须确认用例真正到达校验点：错误 SHA、缺失校验项、不安全归档、提取失败均非零退出且保留旧二进制，不继续提取已拒绝归档、不 fallback；正常 release 成功且不触发源码构建。完整实验细节见服务/交付复核笔记。
