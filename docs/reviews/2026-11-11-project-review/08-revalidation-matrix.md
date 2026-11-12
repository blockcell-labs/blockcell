# 原 24 项复核裁决（2026-11-12）

基线 `main / 5ca38be861b4a3e8f7a1e8d332e242b42ed25710`。24 项均保留：20 项成立，4 项限定成立，0 项撤销；等级仍为 P1 9、P2 15。“限定成立”不是待验证，而是必须按下表收窄原有表述。静态调用链确认不等于完整入口动态验证。

证据标记：API＝实际 crate API 与人工临时数据；入口＝当前完整脚本或真实前端 store；函数＝当前源函数/类型的最小执行；静态＝生产调用链。API 输出见 [jsonl](validation/api-probes.jsonl)，执行范围见 [验证说明](validation/README.md)。各项具体源码、修复边界和行为验收见对应复核笔记。

| ID | 等级 | 裁决 | 证据及最终边界 |
| --- | --- | --- | --- |
| B1-01 | P1 | 限定成立 | API 确认 B 读 A、clear(B) 双删。HTTP 先解析有损 ID，具体选择受文件枚举顺序影响；不能宣称 HTTP 必然双删。 |
| B1-02 | P2 | 成立 | API 确认 provider 删除后复活；源类型/函数还确认 anthropic→默认 openai 被旧值恢复。 |
| B1-03 | P2 | 成立 | API 确认非法时区写入成功、load 失败；限类型合法但语义非法的配置。 |
| B1-04 | P2 | 成立 | API 确认删除 agents.list 首项后，另一 Agent 额度被旧下标值替换；原位 mask 回写不一定损坏。 |
| B1-05 | P2 | 成立 | 真实 MemoryStore 缺失 ID 返回 Err；不是数据库异常都应返回 None。 |
| B2-01 | P1 | 限定成立 | 静态：memory 内既有链接、普通可读外部目标及支持的沙箱读范围；不宣称所有平台可读任意凭据。 |
| B2-02 | P2 | 限定成立 | 静态：技能循环遗漏普通循环的证据更新/有限续轮提醒；普通循环也不是硬完成门。 |
| B2-03 | P2 | 成立 | 静态：第 65 个会话驱逐任意一个旧 tracker，旧会话重建归零；不是全部账本同时归零。 |
| B2-04 | P1 | 成立 | 静态：共享 TaskManager 授权缺 account 和 agent；自动结果汇总还需匹配运行中 typed task。 |
| B3-01 | P1 | 成立 | 实际 Rhai 工具读取人工 deny 哨兵，结合宿主授权静态链；不是完整 Runtime E2E。 |
| B3-02 | P1 | 成立 | 实际 ZIP 工具沿目标既有链接覆盖外部人工哨兵；不自动扩展为 tar 同样存在。 |
| B3-03 | P2 | 成立 | API：列表 name=csv-stats，管理按该 name 失败，按物理目录成功。 |
| B3-04 | P2 | 成立 | API：删除技能后快照仍被列出；是否抢占现行元数据取决于遍历顺序。 |
| B3-05 | P2 | 成立 | 静态：下载读取前截断旧文件，中断留半文件，超限删除；属于失败数据保全。 |
| B4-01 | P1 | 成立 | 静态：原生 Anthropic SSE 嵌套 start usage 被忽略，delta 整体覆盖；限流式路径。 |
| B4-02 | P2 | 成立 | 静态加既有最小解码复现：四个适配器分 chunk 有损 UTF-8 解码；尚未完整适配器测试。 |
| B4-03 | P2 | 成立 | 静态：同 tick 串行等待回执延迟后续任务和停机；operation lock 已释放，不计旧锁问题。 |
| B4-04 | P2 | 成立 | 静态：媒体失败被吞；WeCom 长连接即便媒体成功也丢正文，不仅失败时丢失。 |
| B5-01 | P1 | 成立 | 静态：WS/渠道 slash 传根 Paths，非默认 Agent 清错目录；渠道 account key 也须修正。 |
| B5-02 | P2 | 限定成立 | 当前 resolver 函数及真实 store：后端歧义与前端冒号/下划线混淆分别成立；普通 default:123 往返可成功。 |
| B5-03 | P2 | 成立 | 真实 store：旧聊天 reset 删除当前流消息，error 修改当前聊天状态。 |
| B6-01 | P1 | 成立 | 完整 manifest shell 段：四个正常资产生成空 artifacts，退出 0。 |
| B6-02 | P1 | 成立 | 完整安装入口加外部本地替身：grep 将后缀当选项，正常 release 失败。 |
| B6-03 | P2 | 成立 | 完整 runner CLI：有输出后超时，bytes 导致 JSON 序列化失败、报告未生成。 |

复核笔记：[基础/入口](revalidation-foundation-entry.md)、[Agent/工具](revalidation-agent-tools.md)、[服务/交付](revalidation-services-delivery.md)。笔记中的“本批未运行 Cargo”仅指各复核者执行范围；主审随后使用已安装 Rust 1.96 通过 237 项 core/storage 测试并运行上述 API 验证。

未升级为确认缺陷的线索：cron 超时后 steering/重试/副作用的组合行为、Lark 失败重试与去重，以及所有原生沙箱平台的实际隔离。应补行为测试，不能推断必然重复执行或所有平台越界。
