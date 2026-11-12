# F-01 / F-02 / F-03 修复计划

> **For Claude:** 使用 superpowers:executing-plans 按批执行；用户已明确授权 main 上修复并提交，不创建分支。

**Goal:** 修复取消确认前重复派发、回执清理丢取消、技能空续轮丢答复；F-04 不改动。

**Architecture:** 回执保留活跃 claim 的状态直到退出，取消返回可等待的确认句柄。调度租约在 Drop 时同步撤销回执，活跃执行由异步收尾等待确认后释放；停机等待租约清空。技能保存续轮前答复及 revision，收尾复用普通循环的有效性检查。

**Tech Stack:** Rust 1.96、Tokio、离线锁定 Cargo。

## 1. F-01 / F-02（同批）

1. 将 evidence/remediation_review.rs 移植为 scheduler 集成测试；增加等待取消确认、旧 claim 放下后重试、停机等待的验收。
2. 运行 `cargo test -p blockcell-scheduler -p blockcell-agent -p blockcell --offline --locked --test remediation_review`，确认缺陷断言失败。
3. 修改 crates/core/src/message_receipt.rs：sender 可消耗但保留活跃状态，清理通知取消，Claim Drop 清理并通知，暴露取消确认句柄。
4. 修改 crates/scheduler/src/cron_service.rs：租约记录 receipt ID，Drop 同步取消，异步等待 claim 退出后释放，停机等待收尾。
5. 运行定向回归及 core/scheduler/runtime receipt 测试；验证后提交本批代码、测试和计划。

## 2. F-03

1. 将 evidence/remediation_skill_review.rs 移植为 Agent 集成测试；添加 revision 改变/验证失败后空答复的技能循环回归。
2. 运行回归，确认修复前空答复失败。
3. 修改 crates/agent/src/runtime/turn_flow.rs：保存答复与 revision，空续轮复用 merge_verification_answer；缺失产物明确标注，保留有限提示次数。
4. 运行 Agent 回归和库测试；更新复审状态，保留 F-04 未修，提交本批。
