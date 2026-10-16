# blockcell 全仓 Review 分解方案（2026-10-16）

## 总量摸底

| 区域 | 文件数 | 规模 |
|---|---|---|
| crates/（10 个 crate） | ~440 .rs | ≈229K LOC |
| bin/blockcell（CLI + gateway） | 70 .rs | ≈30K LOC |
| webui（TS） | 57 | ≈15K LOC |
| tests/（e2e，python） | 67 py | — |
| docs + docs0 + docs1 | ~180 md | 主文档 29 篇 |
| skills/（运行时资产） | 9 yaml / 4 rhai / 3 py / 6 json | — |

crate 明细（Rust LOC）：agent 70K / tools 49K / bin 30K / channels 24K / skills 17K / storage 12K / core 11K / providers 8K / scheduler 6.5K / updater 2K。

## 分解原则

1. **依赖分层**：从底层 crate 审到入口层，问题在正确的抽象层暴露，避免在上层反复推断下层语义。
2. **层内并行**：每层拆多个只读子代理并行，单代理上下文控制在 ≈20K LOC 以内（agent 70K 必须拆）。
3. **横切单派**：跨 crate 主题（沙箱与确认语义、fork 缓存对齐、evolution 契约、配置漂移）单独派代理，不混进层审。
4. **串行收口**：最后一波只做跨 crate 不变量核对、问题去重分级、汇总报告。
5. **审查期只读不改**：遵守 CLAUDE.md 的 surgical changes，修复按 P0/P1 集中批次进行，每批后回归测试。

## 波次表

| Wave | 范围 | 规模 | 并行度 | 审查重点 |
|---|---|---|---|---|
| W0 静态基线 | 全仓 | — | 1 | cargo check/clippy 底座；unwrap/unsafe/TODO 密度；git 近半年改动热点图；文档地图 |
| W1 基础层 | core / storage / providers / updater | 33K | 4 | 数据结构与不变量、错误类型设计、trait 边界、SQLite schema 与迁移、provider 协议 |
| W2 机制层 | tools / skills | 67K | 4 | 工具协议正确性、超时与取消、沙箱与权限声明、skill 契约（meta.yaml / SKILL.md）、evolution 补丁链路 |
| W3 编排层 | agent / scheduler / channels | 100K | 8-10 | 每消息 runtime 状态机、并发与锁、fork/response_cache 对齐、消息处理链路、IM 协议 |
| W4 入口层 | bin/blockcell CLI + gateway | 30K | 3 | 与 crates 的接线正确性、CORS/鉴权/webhook 暴露面、错误处理、WS 生命周期、配置加载 |
| W5 周边 | webui / tests / docs / skills 资产 | 15K+ | 3 | 前后端 API 契约一致、e2e 覆盖、文档与实现同步、运行时资产合规 |
| W6 汇总 | 跨 crate | — | 串行 | 跨 crate 不变量核对、问题去重分级、审查报告 |

顺序：W0 → W1 → W2 → W3 → W4（W5 任意时刻后台并行）→ W6。W3 的 tools 相关部分建议等 W2 结论出来再启动，避免重复推导工具协议。

## 每个审查单元的统一 checklist（7 维）

1. **正确性**：边界条件、被吞掉的错误、unwrap/expect、整数溢出与下标、状态机死分支
2. **并发**：锁粒度与死锁、async 中阻塞调用、取消安全、Send/Sync 误用、共享缓存竞争
3. **安全**：沙箱逃逸、注入、路径穿越、凭据落盘与日志泄漏、外部输入校验
4. **性能**：热点重复分配、阻塞 IO 混入 async、无界轮询/缓冲、N+1 调用
5. **测试**：主要分支是否有覆盖、是否有"静默跳过"型守卫（见 MEMORY.md）
6. **可维护性**：死代码、复制粘贴块、TODO/FIXME 处置、命名与注释
7. **契约一致性**：与 docs/、BLOCKCELL.md、公共 API 期望是否一致

## 子代理产出统一格式

```
模块 / 规模（LOC）
问题清单：文件:行 + P0-P3 分级 + 一句证据
亮点：值得保留的设计
建议：下一步动作（仅建议，不在审查期执行）
```

## W6 必须核对的跨 crate 不变量

- `Tool::execution_environment`（描述性，仅审计）与 `Tool::requires_confirmation`（准入性）正交，不得用 FullAccess 推断需要确认
- shell 会话一次 `poll` 必须累积所有轮询周期（delivered_offset 语义）
- fork 与 response_cache / context 缓存算术对齐（已知遗留：Anthropic 断点为算术对齐而非逻辑边界；C11 routed 未做；Config 未 Arc 化）
- gateway 暴露面（CORS / webhooks）与 docs/08、docs/22 的安全说明一致
- 29 篇主文档按模块归属到各波，随层审查同步核对

## 验收标准

- 每个 P0/P1 问题有证据（文件:行 + 复现思路）
- 汇总报告含：分级问题清单、模块健康度、测试覆盖缺口、文档漂移清单
- P0 清零后才进入正常修复批次
