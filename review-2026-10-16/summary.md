# blockcell 全仓 Review 汇总报告（W6）

> 2026-10-16 · 基线 HEAD `5a19eae` · 覆盖 Rust ≈259K LOC + webui 15K TS + docs 43 篇
> 输入：28 份只读报告（1 份静态基线 + 27 份模块报告，由 27 个子代理并行产出）
> 问题分级统计：**P0 10 条（去重后 9 项）/ P1 77 条 / P2 186 条 / P3 220 条**

---

## 1. 总体结论

**工程质量基线不低**：沙箱与确认的正交设计、storage 的事务纪律（Immediate 事务 + 条件 UPDATE）、skills 写路径防护（扫描 + 回滚 + 符号链接拒绝）、updater 的信任链重建（apply 前重算 SHA + 重验签名）、SSRF 双层防护、fork 缓存对齐——都是真实实现且有测试锁定，不是纸面设计。

**但风险高度集中在「外部输入处理面」**，且呈现四个系统性模式：

| # | 系统性问题 | 证据规模 |
|---|---|---|
| 1 | **外部可控字符串未转义就拼进 shell / Python / AppleScript / 路径** | 8 处 P0 中的 5 处 + 多条例 P1（email 附件名、napcat 文件名、wecom media_id） |
| 2 | **安全门未全量接线**（机制正确、覆盖不全） | `requires_confirmation` 在 4+ 个工具未实现；fork 执行期 `can_use_tool` 默认 Allow-all；`ToolPolicy` first-match-wins 使 deny 可被宽 allow 覆盖 |
| 3 | **上限 / 预算不是一等公民** | 下载无 25MB 上限、zip 无解压上限、CSV/音视频全量入内存、`session_memory_systems` 池无上限、WS 消息风暴无背压 |
| 4 | **测试与门禁缺位** | 产品链路 e2e 覆盖为零；CI 被禁用且有测试强制其保持禁用；cron 时间计算与 ed25519 验签零单测 |

**一句话**：问题不在 Rust 本身（无内存安全类 P0），而在**边界处理、安全门接线与反馈闭环**。

---

## 2. P0 清单（9 项去重后，按修复优先级）

| # | 位置 | 问题 | 来源 |
|---|---|---|---|
| 1 | `crates/channels/src/wecom/webhook.rs:436-462` ← `bin/.../gateway/webhooks.rs:113` | **WeCom webhook 验签可整体绕过**：验签条件 `!token.is_empty() && !msg_encrypt.is_empty() && !verify(...)`，`<Encrypt>` 标签缺失即短路跳过验签，body 被当明文 XML 直接入站注入 agent；`/webhook/wecom` 公开可达。**两个代理独立复现** | channels-napcat-wecom / gateway-ws-webhook |
| 2 | `crates/tools/src/app_control.rs:761-808` | **AppleScript 注入**：`click_ui_element` 的 `ui_path` 逐段 split 后未转义即拼进 `set targetEl to {element_ref}`，`(do shell script "...")` 形式即任意命令执行 | tools-misc |
| 3 | `crates/tools/src/app_control.rs:354-386` | **Python 注入**：`get_cg_window_id` 的 `app` 仅 `replace('"',...)`，反斜杠与换行未转义，可闭合字符串执行任意代码 | tools-misc |
| 4 | `crates/tools/src/ocr.rs:218-281` | **Python 注入**：`ocr_vision` 的 `language` 未转义（同函数 `path` 做了转义，唯独 language 漏掉） | tools-content |
| 5 | `crates/tools/src/network_monitor.rs:375-385, 442-448` | **命令注入**：`action_ssl_check` 把 `host` 原样拼进 `sh -c`（同文件 ping/traceroute/dig 均用参数数组，唯独此处拼接） | tools-web-browser |
| 6 | `crates/channels/src/{whatsapp,discord,feishu,lark,dingtalk}.rs` | **入站媒体文件名路径穿越**（任意文件写）：5 个适配器把平台侧用户可控文件名直接 `media_dir.join(...)`；`security.rs::safe_filename` 现成未用，仅 telegram/slack 做了防护 | channels-top |
| 7 | `crates/providers/src/factory.rs:195-204` + `crates/core/src/config.rs:145-155` | **Gemini 默认配置开箱即挂**：默认 `api_type:"openai"` + `/v1beta/openai` base，但 factory 按名称 `"gemini"` 路由到原生 `GeminiProvider` → 请求必 404 | providers |
| 8 | `tests/coding_eval/common_verify.py:8-13` | **验收「零测试即通过」**：`unittest discover` 发现 0 测试时 `Ran 0 tests … OK` 且 exit 0（已实验复证）——删掉 fixture 测试文件即得满分，评测结果不可信 | e2e-tests |
| 9 | `bin/blockcell/src/commands/config_cmd.rs:262-282` | **`config set` 整文件重写，静默丢失用户 JSON5 注释与未建模键**（不可逆数据丢失），并放大 core 的「密钥明文落盘」问题 | cli-ops |

> **Batch 0 修复状态（2026-10-16，commit `12fdfff`）**
> 已修并验证：P0-1（wecom 验签）、P0-2/3（app_control 注入）、P0-4（ocr 注入）、P0-5（network_monitor 注入）、P0-6（5 适配器文件名穿越），
> 以及 §3-A 的 email 附件名、napcat 媒体文件名、wecom media_id（另含 V3 自查新增 4 处同类漏点）。
> 验证：`cargo check` / `clippy` / `test -p blockcell-channels -p blockcell-tools` 全绿（channels 161 + tools 481）。
> 明细与文件级改动见 [`FIX-BATCH0.md`](FIX-BATCH0.md)。
> **未动**：P0-8（验收假通过，属 Batch 6 闭环批次）与 §3-C~I 的其余条目。
>
> **Batch 1 修复状态（2026-10-16，commit `2f1766c` + `289330a`）**
> §3-B 的准入接线（knowledge_forget / termux_api 七个设备副作用动作 / napcat 群管理 `check_admin_permission`）、
> `ToolPolicy` 改为 Deny > Ask > Allow、fork 路径 `can_use_tool` 默认值改为「按该 fork 声明的 schema 名单派生 allowlist」均已修复并验证
> （tools 481 + core 全绿 + agent 842 passed）。有意未做项及理由见 [`FIX-BATCH1.md`](FIX-BATCH1.md) 的 1.8 / 1.9。
>
> **Batch 2 修复状态（2026-10-16，commit `59601f3` + `4cfb64b` + `a67e5e7` + `6c93115`）**
> P0-7（Gemini 默认路由）、P0-9（`config set` 丢键）、encrypt 的 IV/算法/密码传递、alerts `rules.json` 加锁、
> audit 哈希链跨进程锁、`merge_episode_metadata` 单事务均已修复并验证（providers 98 + storage 84 + tools 484 + bin 167 全绿）。
> 另附带重建了 webui 嵌入产物（`59601f3`），使 bin crate 恢复可编译。明细见 [`FIX-BATCH2.md`](FIX-BATCH2.md)。

---

## 3. P1 主题聚类（77 条）

### A. 注入与转义（除 P0 外）
- `email.rs:770-781` 附件 filename 取自 `Content-Disposition`（攻击者可控）未过滤 `..` → 任意文件写
- `napcat/common.rs:489` `download_media_to_workspace` 对 filename/chat_id 零净化（`auto_download` 默认开）
- `wecom/media.rs:76-82` `media_id` 未消毒 → 路径穿越 + 非 char 边界切片 panic
- `skills/evolution/audit.rs` 静态审计可被 `python3 -c` / `node -e` / `perl -e` one-liner 绕过（自动部署前仅两层 LLM 检查，无人审门）

### B. 准入与权限门未接线
- `knowledge_forget.rs:96-106` 删除类工具未实现 `requires_confirmation`，preview/confirm token 由 LLM 自我批准
- `termux_api.rs:194-259` 全工具未覆写 `requires_confirmation`（sms_send / telephony_call / keystore / download 等设备副作用动作）
- `napcat/group.rs:343-348` `check_admin_permission` 把 `NeedsConfirmation` 当放行（配置项静默 no-op）
- `forked/agent.rs:188`、`agent.rs:601-603` **Fork/Typed 路径执行期权限门失效**：`can_use_tool` 默认 Allow-all，绕过 `TOOL_ROSTERS` 白名单
- `core/tool_policy.rs:311-353` 策略引擎 first-match-wins，无 deny 优先 → 宽 allow 可覆盖精确 deny
- `memory_maintenance.rs` 跨 session 无差别删除（junk_score 启发式可误删其他 session 记忆）

### C. 路径策略与沙箱绕过
- `video_process.rs:44-47` schema 字段名 `output` 与 runtime 提取的 `output_path` 不匹配 → **该工具所有写操作完全绕过 docs/22 路径策略**
- `network_monitor.rs:639-699` http_check/bandwidth 用裸 `reqwest::Client` → 绕过 `ssrf.rs` 防线（可打 169.254.169.254）
- `tools/sandbox.rs` Linux Landlock 缺读限制（read-only 名不符实）；`tool_guard.rs` 漏检 `rm --recursive` 长选项

### D. 数据完整性与丢失
- `encrypt.rs:129-236` key 模式 IV 不入文件 → **加密文件必然无法解密**；`algorithm` 参数被静默吞掉（宣称 GCM 实际 CBC）；密码经 argv 泄露
- `storage/audit.rs:332-375` 审计哈希链无跨进程互斥（gateway 与 CLI 并发追加即破坏链）
- `storage/ghost_ledger.rs:211-232` `merge_episode_metadata` 读-改-写跨两次锁 → 丢失更新
- `gateway/alerts.rs` `rules.json` 读-改-写全程无锁 → 丢失更新 / 文件损坏
- `channels/manager.rs:311-324` 出站单任务串行，队头阻塞且无重试幂等（部分投递）
- `wecom/media.rs:501-573` 媒体上传期间入站用户消息被静默丢弃
- `napcat/websocket/client.rs:624-710` notice/request 事件绕过 allowlist/群过滤

### E. 资源上限与 DoS
- `office.rs:80-132` docx/pptx zip 解压炸弹（无 entry/大小上限）；`data_process.rs`、`audio_transcribe.rs` 大文件全量入内存
- `channels` whatsapp/dingtalk 入站媒体下载无 25MB 上限
- `runtime.rs:2692-2700` `session_memory_systems` 池无上限（Gateway 长生命周期下随会话数线性泄漏）
- `napcat/websocket/client.rs:1075-1082` 每条消息 spawn 无背压（风暴下任务数爆炸）
- `stream_subscribe` 全局锁串行 + 无总量内存上限

### F. 阻塞 IO 混入 async（违反全仓不变量）
- `tools/knowledge_graph.rs:129-167`、`knowledge_forget.rs`：rusqlite 同步调用裸跑在 `async fn`，且自开连接无 WAL / busy_timeout
- `scheduler/evolution_worker.rs`、`skill_evolution_worker.rs`：`EvolutionWorkflowStore` 全部同步 SQL 置于 async
- `scheduler/consolidator.rs:191`：`CrossProcessLock::acquire`（`thread::sleep` 重试最坏 6s）在 async 中同步调用
- `scheduler/cron_service.rs:417-502` `run_tick` 全程持 `operation_lock` 顺序等待（单任务 15min 回执内，后续任务全延迟且 gateway 写 API 被阻塞）

### G. 缓存与记忆失效
- `auto_memory/injector.rs` **MemoryInjector 全链路死代码**（`inject_into_system_prompt` 无生产调用方）→ Layer 5 注入静默失效，与 docs/05 表述矛盾
- `context.rs:664-675` `FrozenFileMemorySnapshot` 无失效钩子 → 记忆文件变更后会话内检索注入陈旧
- `context.rs:639-646` `reload_skills` 清快照不彻底（技能**修改/删除**路径不刷新 `system_prompt_snapshots`）
- `agent-runtime-sub` `activate_skill` 同批跳过调用时 tool_use 在 history 中悬挂（无配对 tool_result，下轮 Anthropic 400）
- `ghost_metrics.rs:175-229` path_hint 未命中时事件扇出到所有实例
- `agent-forked-cache` `is_compatible_with` + `fork_alignment_miss` 只覆盖 1/5 派生路径

### H. 契约与文档漂移
- docs/03 `web_search` 宣称 DuckDuckGo 无需 Key，实际 Brave+百度且无 Key 报错
- docs/09 进化阶段名 / 灰度发布 / "只能改 Rhai" 三项均基于 legacy 实现
- providers `pool.rs:399-415` 文档承诺的「冷却保底」未实现；docs/20/28 状态图与代码不符
- `main.rs:15-17` `-v/--verbose` 全局开关是死代码（docs/17 有承诺）
- `alerts add --source` 参数格式与 docs/17 不符；`exec` 默认超时文档 30s 实际 60s

### I. 测试与门禁缺口
- 产品链路（消息处理 / 工具调用 / 沙箱确认 / gateway / WS / skill 加载 / fork）e2e 覆盖为零
- CI 被主动禁用且有测试**强制其保持禁用**；Makefile 无 test 目标
- `scheduler` cron 时间计算（时区 / DST / 6 字段）零单测；`updater` ed25519 验签零单测
- `mcp/client.rs` 零协议级测试；`skills/evolution` 部署 / 契约 / 观察路径零测试
- webui：浏览器通知权限从未申请（alert 通知功能空转）；98 处 `any`

---

## 4. 跨 crate 不变量核对（W6 核心产出）

| 不变量 | 结论 | 依据 |
|---|---|---|
| `execution_environment`（描述性）与 `requires_confirmation`（准入性）正交 | **机制正确，覆盖不全**：正交设计在 tool_pipeline 中落实彻底且有测试；但 knowledge_forget / termux_api / napcat 群管理 / 多个外部动作工具未实现准入 | tools-exec-security、tools-fs-knowledge、tools-skills-integrations |
| shell 会话一次 `poll` 累积所有轮询周期（delivered_offset 语义） | **符合**，且有 `MAX_SESSION_OUTPUT_BYTES` 上限 | tools-exec-security |
| fork 与 response_cache / context 缓存算术对齐 | **符合**；遗留：Anthropic 断点为算术对齐（fork len-4），多轮退化不可观测；C11 routed 未做 | agent-runtime-core、agent-forked-cache |
| `forked/can_use_tool.rs` 的 `TOOL_ROSTERS` 是工具白名单唯一来源 | **不成立**：Fork/Typed 路径 `can_use_tool` 默认 Allow-all，白名单在非 review 路径被绕过 | agent-forked-cache |
| gateway 暴露面与文档安全说明一致 | **不成立**：`GET /v1/channels`、`GET /v1/config` 明文回显全部渠道/provider 密钥并展开 `${ENV}`；默认宽松 CORS；evolution handler 路径穿越 | gateway-http |
| storage 并发不变量（`Arc<Mutex<Connection>>` + WAL） | **被 tools 层多处绕过**：knowledge_graph / knowledge_forget 自开连接、无 WAL、无 busy_timeout | tools-fs-knowledge、storage |
| 路径写策略（docs/22）对写操作全覆盖 | **不成立**：`video_process` 字段名不匹配 → 该工具全部写操作不受策略约束 | tools-content |
| 记忆注入链路（Layer 4/5）在生产生效 | **不成立**：MemoryInjector 无生产调用方（死代码）；记忆文件快照无失效钩子 | agent-memory |

---

## 5. 根因分析（为什么这些问题会同时存在）

1. **缺统一的「外部输入 → 目标语言」转义原语**。8 处注入全部同源：同一个文件里有的分支转义了、有的漏了（`ocr.rs` 的 path 转了 language 没转、`app_control.rs` 的 app/text 转了 ui_path 没转）。说明缺的不是意识，而是**强制的 API 形态**（如 `shell_quote()` / `py_literal()` 包装类型）。
2. **安全 trait 的默认值是 fail-open**。`requires_confirmation` 默认 `false`、`can_use_tool` 默认 `Allow`——忘记实现即等于放行。默认值应反过来（默认拒绝，显式放行）。
3. **上限与预算是逐处手写的**。`bounded_io` / `read_response_limited` 只在 HTTP 路径存在，zip、CSV、音视频、WS 帧、内存池各写各的 → 总有遗漏。
4. **阻塞原语与 async 边界靠人工纪律维持**。storage 自身守得很严，但 tools 与 scheduler 的调用方破例（rusqlite 直接进 async、`thread::sleep` 重试进 async）。
5. **无自动化门禁 → 缺陷无反馈闭环**。CI 禁用 + e2e 为零，导致上述问题长期存活；连「验收脚本假通过」这种事也没被发现。

---

## 6. 值得保留的亮点

- **沙箱与确认的正交设计**：描述性与准入性分离，审计记录准确，不靠环境推断准入（tools-exec-security 逐条核对通过）
- **storage**：`Arc<Mutex<Connection>>` + WAL 串行化模型、租约 claim 用 Immediate 事务 + 条件 UPDATE、SQL 全参数化、fs2 跨进程锁 + 原子写、哈希链篡改检测有测试
- **updater 信任链**：apply 前重算 SHA + 重验签名、签名载荷绑定元数据、健康检查失败自动回滚、验签 fail-closed
- **skills 写路径防护**：扫描 + 回滚 + 符号链接拒绝 + 互斥锁 + 孤儿记录磁盘对账
- **SSRF 双层防护**（`ssrf.rs` 连接时校验）、seccomp 旁路防护、SBPL 顺序回归测试
- **fork 缓存对齐**：TOOL_ROSTERS 三派生 + 5 测试、四道路径安全门、取消链路实测、原子写 + 回滚
- **webui**：WS 重连设计成熟、token 存 sessionStorage、无 XSS 面、聊天虚拟列表 + delta 批处理
- **memory_file_store**、`fuzzy_match`、`cron` 工具被多个代理评为工程质量标杆

---

## 7. 修复批次建议

> 原则：P0 集中一批修完再回归；每批后跑受影响 crate 的单测（注意需先 `npm run build`，见 W0 §2）。

- **Batch 0（安全阻断级，最高优先）**：wecom 验签（P0-1）、5 适配器文件名清洗（P0-6）、4 处脚本/命令注入（P0-2/3/4/5）、email 附件名 + napcat 文件名 + wecom media_id（P1-A）
- **Batch 1（准入与权限）✅ 已完成**：`requires_confirmation` 接线到 knowledge_forget / termux_api；`check_admin_permission` 改 fail-closed；`ToolPolicy` 加 deny 优先；fork 路径 `can_use_tool` 默认改 fail-closed。提交 `2f1766c` + `289330a`
- **Batch 2（数据与配置）✅ 已完成**：`config set` 保留未建模键与 `${ENV}` 占位符（P0-9）、Gemini 默认路由（P0-7）、encrypt IV/algorithm/密码传递、alerts rules 加锁、audit 哈希链跨进程锁、`merge_episode_metadata` 单事务。提交 `59601f3` + `4cfb64b` + `a67e5e7` + `6c93115`
- **Batch 3（路径与凭据收敛）**：`video_process` 字段对齐、`network_monitor` 接入 ssrf、gateway/CLI 密钥回显脱敏、`.env` 权限与覆盖语义
- **Batch 4（资源上限）**：统一 zip / 下载 / CSV / 音视频 / WS 帧 / 内存池上限，池加 LRU
- **Batch 5（async 纪律）**：rusqlite 与跨进程锁调用全部出 async（`spawn_blocking`）
- **Batch 6（闭环）**：验收脚本假通过（P0-8）、恢复 CI 门禁、补 cron/ed25519/evolution/MCP 测试、文档同步（docs/03、09、17、20）

---

## 8. 覆盖度与局限

- **clippy 未覆盖 bin crate**：`bin/blockcell/build.rs:101` 的嵌入 webui 哈希守卫使构建直接失败（详见 W0 §2），bin 的结论来自人工代理而非编译基线
- **未做**：运行时实测（性能 / 并发压力 / 真实 IM 联调）、webui 构建产物与 vendor/ 目录、docs0（37 篇）与 docs1（14 篇）深读
- **分级口径**：P0-P3 来自各代理独立判断；P2/P3 未做跨报告去重，P0/P1 已人工去重（wecom 验签两条合并为 1 项）
- **可信度**：wecom 验签 P0 由 channels 与 gateway 两个代理独立复现；`requires_confirmation` 正交不变量由 tools-exec-security 逐条核对；其余结论均为单代理判断，修复前建议按报告中的「文件:行 + 证据」复核
