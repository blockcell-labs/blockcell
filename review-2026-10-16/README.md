# blockcell 全仓 Review（2026-10-16）

> 基线 HEAD `5a19eae` · 只读审查（未修改任何源码）  
> 覆盖：Rust ≈259K LOC（10 crate + bin）· webui 57 TS ≈15K LOC · docs 43 篇 · tests 67 py  
> 问题统计：**P0 10 条（去重后 9 项）· P1 77 条 · P2 186 条 · P3 220 条**

## 怎么读

1. **先读 [`summary.md`](summary.md)** —— 总体结论、P0 清单、P1 主题聚类、跨 crate 不变量核对、根因与修复批次建议
2. **再看 [`wave0-baseline.md`](wave0-baseline.md)** —— 静态基线（clippy / 危险模式密度 / git churn 热点），用于理解各波注意力分配
3. **需要细节时按模块查** —— 每份报告含「模块地图 + 问题清单（文件:行 + 证据）+ 亮点 + 建议」

## 目录结构（按七波分解）

```
review-2026-10-16/
├── README.md                    本文件：索引与统计
├── summary.md                   W6 汇总报告（先读这个）
├── wave0-baseline.md            W0 静态基线 + clippy.txt
├── wave0-baseline/clippy.txt    全工作区 clippy 原始输出
├── wave1-foundation/            W1 基础层（4 份）
├── wave2-mechanism/             W2 机制层（8 份）
├── wave3-orchestration/         W3 编排层（8 份）
├── wave4-entry/                 W4 入口层（4 份）
└── wave5-periphery/             W5 周边（3 份）
```

## 报告清单与分级统计

### W1 基础层（≈33K LOC）

| 报告                                            | 覆盖                          | P0    | P1 |
| --------------------------------------------- | --------------------------- | ----- | -- |
| [core.md](wave1-foundation/core.md)           | 共享基础类型 / 路径策略 / 配置 / 错误     | 0     | 2  |
| [storage.md](wave1-foundation/storage.md)     | SQLite 持久层（5 store + 文件存储）  | 0     | 3  |
| [providers.md](wave1-foundation/providers.md) | LLM 接入 / 流式 / provider pool | **1** | 5  |
| [updater.md](wave1-foundation/updater.md)     | 自更新（ed25519 / 安装 / 回滚）      | 0     | 2  |

### W2 机制层（≈67K LOC）

| 报告                                                                           | 覆盖                                             | P0    | P1 |
| ---------------------------------------------------------------------------- | ---------------------------------------------- | ----- | -- |
| [tools-exec-security.md](wave2-mechanism/tools-exec-security.md)             | shell / sandbox / exec / tool_guard / registry | 0     | 0  |
| [tools-fs-knowledge.md](wave2-mechanism/tools-fs-knowledge.md)               | fs / memory / knowledge graph                  | 0     | 6  |
| [tools-web-browser.md](wave2-mechanism/tools-web-browser.md)                 | browser / MCP / web / 流式订阅                     | **1** | 3  |
| [tools-content.md](wave2-mechanism/tools-content.md)                         | office / 图表 / 音视频 / OCR / TTS                  | **1** | 5  |
| [tools-misc.md](wave2-mechanism/tools-misc.md)                               | 设备控制 / 邮件 / 告警 / 加密 / 子代理                      | **2** | 4  |
| [tools-skills-integrations.md](wave2-mechanism/tools-skills-integrations.md) | 技能工具 / termux / napcat 后端                      | 0     | 4  |
| [skills-manager-service.md](wave2-mechanism/skills-manager-service.md)       | SkillManager / 热重载 / 服务层                       | 0     | 1  |
| [skills-evolution.md](wave2-mechanism/skills-evolution.md)                   | 自进化 / 核心进化链路                                   | 0     | 2  |

### W3 编排层（≈100K LOC）

| 报告                                                                       | 覆盖                                           | P0    | P1 |
| ------------------------------------------------------------------------ | -------------------------------------------- | ----- | -- |
| [agent-runtime-core.md](wave3-orchestration/agent-runtime-core.md)       | runtime.rs（每消息生命周期 / fork 派生）                | 0     | 1  |
| [agent-runtime-sub.md](wave3-orchestration/agent-runtime-sub.md)         | runtime/ 子目录（工具管线 / 学习 / 路径安全）               | 0     | 2  |
| [agent-forked-cache.md](wave3-orchestration/agent-forked-cache.md)       | forked/ + response_cache                     | 0     | 2  |
| [agent-memory.md](wave3-orchestration/agent-memory.md)                   | 记忆家族（Layer 1-5）                              | 0     | 3  |
| [agent-context-misc.md](wave3-orchestration/agent-context-misc.md)       | context / compact / metrics / ghost / intent | 0     | 1  |
| [channels-top.md](wave3-orchestration/channels-top.md)                   | IM 通道顶层（协议 / 路由 / 出站）                        | **1** | 2  |
| [channels-napcat-wecom.md](wave3-orchestration/channels-napcat-wecom.md) | napcat（QQ）/ 企业微信实现                           | **1** | 6  |
| [scheduler.md](wave3-orchestration/scheduler.md)                         | 定时任务调度                                       | 0     | 4  |

### W4 入口层（≈30K LOC）

| 报告                                                         | 覆盖                                         | P0    | P1 |
| ---------------------------------------------------------- | ------------------------------------------ | ----- | -- |
| [cli-core-tui.md](wave4-entry/cli-core-tui.md)             | main.rs / agent TUI / 斜杠命令                 | 0     | 2  |
| [cli-ops.md](wave4-entry/cli-ops.md)                       | 运维子命令（config / session / memory / cron …）  | **1** | 6  |
| [gateway-http.md](wave4-entry/gateway-http.md)             | gateway HTTP 面（路由 / CORS / files / 配置 API） | 0     | 2  |
| [gateway-ws-webhook.md](wave4-entry/gateway-ws-webhook.md) | WS / webhook / stream / chat               | **1** | 3  |

### W5 周边

| 报告                                                                   | 覆盖                       | P0    | P1 |
| -------------------------------------------------------------------- | ------------------------ | ----- | -- |
| [webui.md](wave5-periphery/webui.md)                                 | webui/src（57 TS）         | 0     | 1  |
| [e2e-tests.md](wave5-periphery/e2e-tests.md)                         | tests/（评测 harness + 脚本）  | **1** | 2  |
| [docs-and-skill-assets.md](wave5-periphery/docs-and-skill-assets.md) | docs 一致性抽查 + skills 资产合规 | 0     | 3  |

## 分解方法

按依赖分层、层内并行、横切单派、串行收口，详见 [`docs/design/review-decomposition-2026-10-16.md`](../docs/design/review-decomposition-2026-10-16.md)。

- 27 个只读子代理并行执行，单代理上下文 ≤20K LOC（agent 70K 拆 5 份、tools 49K 拆 6 份、bin 30K 拆 4 份）
- 每个单元统一 7 维 checklist：正确性 / 并发 / 安全 / 性能 / 测试覆盖 / 可维护性 / 契约一致性
- 产出统一为「问题清单（文件:行 + P0-P3 + 一句证据）」格式，便于 W6 去重与汇总

## 已知局限

- `clippy` 未覆盖 bin crate：`bin/blockcell/build.rs:101` 的嵌入 webui 哈希守卫在 webui 源码变更后直接使构建失败（W0 §2），bin 结论来自人工代理
- 未做运行时实测（性能 / 并发压力 / 真实 IM 联调）、未审 webui 构建产物与 vendor/、docs0/docs1 未深读
- P0/P1 已人工去重（wecom 验签两条合并）；P2/P3 未跨报告去重

