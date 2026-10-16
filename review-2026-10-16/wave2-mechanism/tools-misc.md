# tools 杂项集群 Review 报告（Wave 2 · 机制层）

> 只读审查 · 2026-10-16 · 子代理 w2-misc

## 规模

审查范围 19 个文件，合计约 **9,232 LOC**（占 tools crate 38.4K LOC 的 ~24%）：

| 文件 | LOC |
|---|---|
| fuzzy_match.rs | 958 |
| alert_rule.rs | 1007 |
| email.rs | 1013 |
| app_control.rs | 994 |
| encrypt.rs | 798 |
| cron.rs | 873 |
| system_info.rs | 627 |
| agent.rs | 464 |
| camera.rs | 407 |
| message.rs | 370 |
| spawn.rs | 298 |
| plan.rs | 330 |
| toggle_manage.rs | 225 |
| agent_status.rs | 206 |
| send_message.rs | 186 |
| context_remaining.rs | 148 |
| evolution_workflow.rs | 142 |
| tasks.rs | 90 |
| subagent.rs | 96 |

深读：email / alert_rule / encrypt / cron / spawn / subagent / agent；快读：其余 12 个。另抽样核对了 scheduler crate（cron 表达式解析、job JSON 形状）、bin/blockcell 的 alerts CLI 与 gateway 端点、docs/03、docs/17。

## 模块地图（文件职责表）

| 文件 | 职责 | 执行规格声明 |
|---|---|---|
| app_control.rs | macOS AppleScript/System Events 应用自动化（11 个 action：activate/type/press_key/click_menu/read_ui/screenshot/click_ui_element 等），osascript + screencapture + python3(Quartz) 子进程 | **未声明** environment / requires_confirmation |
| system_info.rs | 系统硬件/软件/网络/工具探测（df、sysctl、system_profiler 等只读命令），含 capability 进化子 action | idempotency=Safe |
| camera.rs | macOS 摄像头拍照，imagecapture → ffmpeg(avfoundation) → screencapture 三级降级 | 未声明 |
| email.rs | SMTP 发送（lettre）+ IMAP 收取（async-imap + rustls），手写 MIME 解析/解码 | Network + Idempotency::Required；**无** requires_confirmation |
| alert_rule.rs | 告警规则 CRUD + 手动 evaluate（9 种算子、冷却、on_trigger 工具回调），持久化 workspace/alerts/rules.json | 未声明 |
| message.rs | 经 outbound 总线向任意渠道/chat 发消息与媒体，跨渠道 target_name 联系人解析，媒体自动复制进 workspace | Network + Idempotency::Required；**无** requires_confirmation |
| send_message.rs | 向运行中的 agent 任务发消息（ONE_SHOT 拒绝） | Idempotency::Required |
| cron.rs | 定时任务 add/list/remove，at/every/cron 三种调度，校验 cron 表达式与时区，文件锁 + 原子写 | 未声明 |
| tasks.rs | list_tasks：查询后台子代理任务 | — |
| plan.rs | update_plan：会话计划存储（LRU 256 会话 + 增量注入 + 恢复渲染） | — |
| toggle_manage.rs | 技能/工具开关读写 toggles.json | — |
| agent_status.rs | agent 节点/渠道绑定只读查询 | — |
| context_remaining.rs | 查询会话 token 预算快照 | — |
| subagent.rs | SubagentProvider/Handle/Request/Lineage 契约（纯 trait，96 行） | — |
| spawn.rs | spawn 工具：技能路由 `__SKILL_EXEC__` 或自由任务，ForkChild 拒绝 | 未声明 |
| agent.rs | agent 工具：Fork（同步只读）/ Typed（后台）子代理，subagent_type 格式校验、coder workspace_scope 防逃逸、去重检查 | 未声明 |
| encrypt.rs | 文件加解密（openssl enc 子进程）/密码生成/哈希/编解码/校验和 | 未声明 |
| fuzzy_match.rs | 9 策略模糊查找替换（供 skill patch），行号映射 + 转义偏移映射 | — |
| evolution_workflow.rs | 进化工作流 list/get/steps/cancel/retry/unblock 透传 | 未声明 |

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| P0 | app_control.rs:761-808 | **click_ui_element AppleScript 注入（可致任意命令执行）** | ui_path 逐段 split('>') 后**未转义**直接拼进 element_ref 嵌入脚本 `set targetEl to {element_ref}`（806 行）。ui_path 为 `(do shell script "...")` 形式即可执行任意 shell。同文件其它路径（app/text）均经 escape_applescript 处理，唯独此处漏掉。LLM 被 prompt injection 诱导即触发。 |
| P0 | app_control.rs:354-386 | **get_cg_window_id Python 注入（可致任意代码执行）** | app 参数仅 `replace('"', "\\\"")`（370 行）后插入 Python 源码 `app_name = "{}"`；**反斜杠与换行未转义**。app=`x"\nimport os;os.system("id")\nx="` 即可注入。且此替换比 escape_applescript 更弱（未转义 `\`）。 |
| P1 | encrypt.rs:129-157 / 184-236 | **key 模式加解密 IV 不一致，数据不可恢复** | 加密时每次随机生成 16 字节 IV 作为 `-iv` 传入 openssl（155-156 行），但 IV **未写入输出文件**；解密分支（233-236 行）只传 `-K` 不传 `-iv`，openssl 对 CBC 无 IV 直接报错或按零 IV 解密 → key 模式加密的文件**必然无法解密**。属静默数据损坏路径。 |
| P1 | encrypt.rs:11-29 vs 129-140 | **algorithm 参数被静默吞掉，实际与声明不符** | schema/docstring 宣称 `aes-256-gcm`/`chacha20-poly1305`（AEAD），实现硬编码 `-aes-256-cbc -pbkdf2 -iter 100000`（131-135 行），algorithm 入参在 execute 中从未读取。用户以为得到 GCM（防篡改），实际得到未认证 CBC。契约违反 + 安全强度低于广告。 |
| P1 | encrypt.rs:142-144 | **密码经命令行参数传递** | `-pass pass:{pw}`（144 行）使明文密码出现在进程 argv，`ps` 可读。多用户机器上属凭据泄露。应改 `-pass stdin` 或 env/file 方式。 |
| P1 | email.rs:770-781 | **附件保存路径穿越（任意文件写）** | `expand_path(dir, workspace).join(&filename)`（771 行）中 filename 取自邮件 `Content-Disposition`（攻击者完全可控），未过滤 `..`。恶意邮件 `filename="../../.ssh/authorized_keys"` → agent 读信并 save_attachments_to 时任意写。 |
| P2 | email.rs:351-353 | **阻塞式 SMTP 发送占用 tokio worker** | lettre `mailer.send()` 为同步阻塞调用，直接在 async fn 中执行，会阻塞事件循环线程直至 SMTP 往返完成。应 `spawn_blocking`。 |
| P2 | email.rs:52-55, 193-200 | **凭据作为工具参数反复透传** | 每次调用都要求明文 username/password 入参，凭据进入模型上下文、会话记录与审计日志；无凭据存储/复用机制。设计层面应收敛到受保护的凭据存储（tool 参数声明"每调用必填密码"）。 |
| P2 | email.rs:593-596 | **IMAP search query 未消毒即下发** | `session.uid_search(query)` 将 query 原样拼进 IMAP 命令。query 含 CRLF 可注入附加 IMAP 命令（如 `X\r\nDELETE ...`）。schema 声称"supports IMAP search syntax"半开放，但未做最小消毒。 |
| P2 | alert_rule.rs:582-585, 689-691 | **evaluate 使用全新默认 ToolRegistry，绕过 runtime 护栏** | source/on_trigger 工具调用走 `ToolRegistry::with_defaults()`（582、689 行）而非运行时注册表：① 配置/MCP 注册的 source 工具永远找不到；② on_trigger 在 `require_confirm:false` 时直接执行任意工具，绕过 runtime 的 permissions/confirmation/sandbox 流水线（仅靠 LLM 自填的布尔位把关）。LLM 可设 `require_confirm:false` 让告警自动下单（示例中 exchange_api place_order 正是此类）。 |
| P2 | alert_rule.rs:107-124 | **rules.json 读-改-写无文件锁** | load/save 全程无锁（对比 cron.rs:41-45 用 ExclusiveFileLock）；并发 create/update/evaluate 互相覆盖，状态丢失。且 save 用裸 `std::fs::write`（122 行）非原子写，崩溃可致规则库截断。 |
| P2 | alert_rule.rs:47-48 vs 消费方 | **check_interval_secs 无人消费，自动评估未实现** | 全仓 grep：check_interval_secs 仅出现在 alert_rule.rs 自身与无关配置；无 runtime tick 按间隔评估规则，只能靠手动 `evaluate` action。docs/03:290-294 "Rules periodically evaluate…"与 schema "How often to check" 均承诺周期评估——**功能未落地**，用户以为的监控实际不存在。 |
| P2 | cron.rs:116 | **every_seconds * 1000 未做溢出检查** | delay 路径有 `checked_mul`（73 行），every 路径直接 `every * 1000`（116 行）。`every_seconds=i64::MAX` 时 release 下回绕成负数 everyMs，debug 下 panic——同一文件内校验强度不一致。 |
| P2 | app_control.rs / email.rs / message.rs（全文件） | **外部动作工具缺 requires_confirmation，app_control 甚至缺 environment 声明** | 按仓库不变量（execution_environment 描述性 / requires_confirmation 准入性正交）：app_control 的 type/press_key/click_menu/click_ui_element 会真实操作用户桌面（cmd+q 可退出应用、误点菜单），email 可向任意地址发信，message 可跨渠道推送——三者均默认 `requires_confirmation=false`。参照 shell_session.rs:700-703 的先例（full-access shell 逐次确认），这些工具至少应对高风险参数集声明确认。app_control 还完全未声明 execution_environment（默认 InProcess），与其实际派生 osascript/python3/screencapture 子进程的事实不符，审计环境字段失真。 |
| P3 | encrypt.rs:616-619 | **hex decode 奇数长度 panic** | `&text[i..i.min(text.len()-1)+2]`：奇数长度时末次切片越界（如 "abc" → text[2..4]）。decode 入参为用户/模型可控文本，属可复现 panic（tokio 通常兜成错误，但仍应修）。同处逻辑本可用 from_str_radix 步进 2 的正确写法。 |
| P3 | encrypt.rs:332-342 | **密码生成取模偏差** | `(rng.next_u64() as usize) % chars.len()` 非均匀采样（charset 非 2 的幂时）。熵损失小但属实；建议 rejection sampling 或 rand 的 gen_range。 |
| P3 | encrypt.rs:681-798 | **测试缺口：无加解密 round-trip 测试** | 现有测试覆盖 schema/密码/哈希/编解码，唯独没有 encrypt_file→decrypt_file 往返测试——若有，key 模式 IV 缺陷（P1）在 CI 中即可暴露。 |
| P3 | email.rs:598-606 | **search "most recent" 假设未保证** | `uids.iter().copied().collect()` 后取"最后 N 条"，但 async-imap 的 NameSet 迭代顺序不保证按 UID 排序，超过 limit 时返回的是任意子集而非最近子集。应先 sort。 |
| P3 | email.rs:411-413 | **IMAP TCP 连接无超时** | `TcpStream::connect` 与整体 login 无超时包裹，对端挂起将无限期占用任务。SMTP 侧（lettre）同样未设 timeout。 |
| P3 | email.rs:513 | **uid u64→u32 截断** | `params["uid"].as_u64().unwrap() as u32`，超大 uid 静默截断（validate 只查存在性）。危害低（最多查不到），但类型处理草率。 |
| P3 | alert_rule.rs:788-789 | **eq 算子用 f64::EPSILON 做绝对比较** | `(current-threshold).abs() < 2.2e-16` 对解析后的浮点数几乎永不成立，eq 实际等效于"精确位相等"。建议相对 epsilon 或文档化。 |
| P3 | alert_rule.rs:265-289 | **create 不校验 operator 枚举** | validate 的 create 分支不检查 operator 取值；非法算子可入库，evaluate 时落入 `_ => false` 静默不触发。enum 仅存在于 JSON schema 描述文字中。 |
| P3 | cron.rs:270-273 | **remove 前缀匹配可能误删** | `id.starts_with(job_id)`：LLM 传短前缀会命中多条 job 一并删除（返回 removed 计数可察觉，但误删已发生）。建议精确匹配或删除前返回候选。 |
| P3 | spawn.rs:104 | **`__SKILL_EXEC__:skill:query` 字符串协议脆弱** | skill 名或 query 含 `:` 时解析端产生歧义（query 拼接在尾部可缓解，但协议本身无转义）。建议结构化参数传递。 |
| P3 | message.rs:161-172 | **媒体复制目标名冲突** | 不同源文件同名时 `media_dir.join(filename)` 相互覆盖；先复制者静默丢失。 |
| P3 | toggle_manage.rs:134 | **toggles.json 裸写非原子** | `std::fs::write` 直接覆盖，崩溃/并发可截断。与 cron/alert 的一致性参考（core 有 atomic_write）。 |
| P3 | evolution_workflow.rs | **零测试** | 纯透传工具，142 行无任何 #[cfg(test)]（其余文件均有）。 |
| P3 | agent.rs:198 | **Fork 模式同步等待无超时** | `handle.result().await` 在工具内同步阻塞父 agent 直到子代理完成，子代理挂起则父回合无限期卡死。建议加超时/中断语义。 |

## 亮点

- **fuzzy_match.rs 工程质量突出**：9 策略链 + 两套偏移映射（行号映射修复策略 2-7 的位置 BUG、逐字节转义映射修复策略 5 的行数变化问题），测试覆盖了回归 BUG 案例（766-782、925-957 行）与重叠 span 去重——是本集群测试密度与深度最高的文件。
- **cron.rs 的持久化规范**：ExclusiveFileLock + atomic_write + 同一 `cron` crate 与 scheduler/cron_service.rs:778 对齐校验，写入的 job JSON 形状（camelCase: atMs/everyMs/runImmediately/scriptKind）与 scheduler/src/job.rs 的 serde rename 完全匹配——跨 crate 契约一致性好；测试覆盖了时区/溢出/负数/无效 store 等边界。
- **subagent.rs 契约设计**：权限显式声明（无"复制父权限"选项）、SubagentLineage 血缘、取消传播，文档即契约；agent.rs 的 subagent_type 格式校验、coder workspace_scope 防 `../` 逃逸、同类型任务去重、ForkChild 拒绝 spawn 均有测试。
- **email 的 TLS 与协议细节**：rustls + 原生根证书（非 accept-invalid-certs），STARTTLS/465 分流，MIME encoded-word/QP/base64 解码与正文截断（50K 字符）齐备。
- **plan.rs** 的 LRU 驱逐 + injected_revision 增量注入 + 恢复渲染，状态机小而完整。

## 建议

1. **立即修**：app_control 两处注入（click_ui_element 对 ui_path 做白名单字符校验而非字符串拼接；get_cg_window_id 改用 argv 传参而非字符串插值 Python）；encrypt key 模式要么把 IV 写入文件头（自描述格式），要么禁用 key 模式；email 附件 filename 做 basename 消毒。
2. **契约对齐**：encrypt 要么实现声明的 GCM/ChaCha20（openssl enc 不支持，需换 aes-gcm/chacha20poly1305 crate），要么把 schema/docstring 改成 CBC 并明示未认证；password 改 `-pass stdin`。
3. **准入性补齐**：app_control/email/message 评估对高风险参数集声明 `requires_confirmation`（对齐 shell_session 先例）；app_control 声明 Shell/OutOfProcess 类 environment，保证审计字段真实。
4. **alert_rule**：加文件锁与原子写（复用 cron 模式）；evaluate 改为从 ToolContext 拿运行时注册表（需在 lib.rs 增加 registry 句柄 trait，与 SpawnHandle 同款解循环依赖）；check_interval 要么实现 runtime tick 消费，要么从 schema/docs 移除"periodically"承诺。
5. **小修清单**：cron every 溢出加 checked_mul；email search 先 sort uid、连接加超时；encrypt hex 解码奇数长度与取模偏差；alert_rule create 校验 operator；toggle_manage 改 atomic_write；evolution_workflow 补基础 schema 测试。
