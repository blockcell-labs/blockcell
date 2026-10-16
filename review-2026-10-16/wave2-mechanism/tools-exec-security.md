# tools 执行与安全集群 Review 报告（Wave 2 · 机制层）

> 只读审查 · 2026-10-16 · 子代理 w2-exec-security

## 规模

| 文件 | 行数 |
|---|---|
| shell_session.rs | 1521 |
| sandbox.rs | 978 |
| security_scan.rs | 956 |
| exec_local.rs | 824 |
| registry.rs | 1111 |
| exec_skill_script.rs | 544 |
| exec.rs | 455 |
| tool_guard.rs | 450 |
| execution_env.rs | 266 |
| ssrf.rs | 205 |
| fs_backend.rs | 142 |
| shell_executor.rs | 139 |
| process.rs | 138 |
| bounded_io.rs | 68 |
| presentation.rs | 54 |
| registry_builder.rs | 44 |
| **合计** | **7895** |

## 模块地图（文件职责表）

| 文件 | 职责 |
|---|---|
| shell_session.rs | 持久 shell 会话（bash 交互式 / Windows cmd）：创建、命令注入（fd3 命令通道 + marker 追踪）、poll 累积语义、stdin 注入守卫、退出回收（reaper）、owner 校验；`ShellTool` 的声明与四个 action |
| shell_executor.rs | Shell 执行 seam：`ShellExecutor` trait + `LocalShellExecutor`（经 SandboxProvider 一次性执行） |
| sandbox.rs | `SandboxPolicy`（read-only/workspace-write/full-access）、`SandboxProvider` trait、`NativeSandboxProvider`：macOS seatbelt SBPL profile 生成（含敏感读 deny、SBPL 转义）、Linux Landlock ABI1-3 + seccomp 网络过滤（BPF 指令构造与 x32/socketcall 旁路防护）、ApprovalRequired 降级；`prepare_sandboxed` 校验 provider 不得放宽策略 |
| security_scan.rs | Skill/Memory 内容安全扫描：16 类 regex 规则集（危险命令、外泄、持久化、混淆、越狱等）、零宽 Unicode 检测、4 级 TrustLevel 策略、目录递归扫描（symlink/二进制/大小限制） |
| exec.rs | 一次性 exec 工具：构造时固定沙箱策略（config 驱动，拒绝 per-call 覆盖）、`is_dangerous_command` 灾难级命令硬阻断（rm 变体、dd/mkfs/shutdown/fork bomb 等）、输出截断 |
| exec_local.rs | 技能目录内脚本执行：路径限定（相对 + canonicalize 双重校验）、runner 白名单（python3/python/bash/sh/node/php/uv）、Python runner 缓存与 fallback |
| exec_skill_script.rs | `.rhai` 进程内（Rhai dispatcher + NestedToolGuard 准入/审计）与进程脚本（委托 exec_local）；禁止嵌套 exec_skill_script；未声明工具拒绝 |
| tool_guard.rs | `dangerous_operation_items`（exec/shell/file_ops 危险操作判定，Runtime 与嵌套守卫共用）、`is_dangerous_exec_command`、`NestedToolGuard`（技能脚本嵌套调用：toggles/policy/dangerous/full-access 全拒绝，写入审计） |
| process.rs | `run_command_bounded`：stdout/stderr 有界 drain（超限续读丢弃）、整体超时杀进程组（process_group(0)）、管道不关闭时的兜底超时 |
| bounded_io.rs | HTTP 响应体有界读取（内存/落盘两种） |
| execution_env.rs | `ExecutionEnvironment` 描述性声明（6 档，as_str 唯一来源）；`requests_full_access`/`effective_sandbox_is_full_access`——full-access 判定唯一规则 |
| ssrf.rs | SSRF 防护：IP 分类（含 CGNAT/0.0.0.0/8/IPv4-mapped-IPv6）、SafeResolver 连接时校验、URL 预检、重定向策略（≤10 跳 + 私有地址阻断） |
| fs_backend.rs | 文件系统 seam：`FilesystemBackend` trait + LocalFilesystem（含 Office 文档转换） |
| presentation.rs | 工具展示三层定义中的展示层（可见性/描述/分组/排序，不影响执行） |
| registry_builder.rs | 按 agent 配置构建 registry（exec 策略来自 config，展示覆盖，MCP 注入） |
| registry.rs | `ToolRegistry`：全局 + 作用域（agent/session/candidate）工具、presentation 覆盖、tiered schema、`execution_environment`/`requires_confirmation`/`execute`（validate + 权限子集校验） |

## 问题清单

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| P2 | tool_guard.rs:59-70 | 危险操作确认门（DangerousOperation 阶段）漏检长选项：`rm --recursive --force ~/Documents` 通过 `is_dangerous_exec_command` 不被标记（只匹配 ` -r`/`-rf`/`-fr`/`starts_with("-r")`，`--recursive` 均不匹配），而 exec.rs:115 的 `is_dangerous_rm` 硬阻断覆盖了长选项。两套检测器不一致：短选项会触发确认，长选项静默跳过确认（exec.rs:136-142 只是确认门的兄弟实现，不是同一份规则） | 对 `exec` full-access 配置（config 显式放开）路径影响最大：`rm --recursive --force ~/Documents` 不经用户确认直接执行。建议 tool_guard 复用 exec.rs 的 flag 解析，或至少补 `--recursive`/`--force` 匹配 |
| P2 | sandbox.rs:349-465 | Linux Landlock 后端只限制写（ABI1 无 READ_FILE 句柄，代码也未在 ABI≥5 时启用），与 seatbelt 的敏感读 deny（sandbox.rs:137-171）能力不对等：Linux 上 read-only 策略不限制任何读（~/.ssh、/etc/shadow 均可读），"read-only" 实际只等价于"禁写 + 禁网" | 策略覆盖面声明（schema 描述 "read-only/workspace-write use native OS isolation"）与 Linux 实际能力不符；外泄路径被 seccomp 封堵但读本身不受限。建议在文档/错误信息中说明 Linux 读限制缺失，或在 ABI≥5 时启用 ACCESS_FS_READ_FILE |
| P2 | shell_session.rs:83-94 | `is_dangerous_exec_command` 的 `"kill "` 子串匹配产生误报：任何含 `skill `（后随空格）的命令（如 `node skill build`、`npm run skill test`）被判定为危险操作，触发 DangerousOperation 确认门 | tool_guard.rs:84 `"kill "` 为裸子串匹配（非词边界），`"skill "` 包含 `"kill "`。fail-closed 方向但会造成正常命令被拦/反复弹确认。建议改 `\bkill\b` 词边界匹配 |
| P3 | exec.rs:71 | 块设备重定向 deny 模式 `>\s*/dev/(sd\|hd\|nvme\|disk\|mapper)` 遗漏 `/dev/vda`（AWS/GCP 常见）、`/dev/md*`、`/dev/loop*`、`/dev/ram*`、`/dev/zram*` | `cat x > /dev/vda` 不被硬阻断（sandbox 下被写策略兜底，full-access exec 配置下是缺口） |
| P3 | registry.rs:359-367, 440-445 | `effective_tools()` 每次调用克隆整个全局工具 HashMap；`get()` 每次调用经 `visible_scoped()` 重建作用域视图（空作用域时 early-return 尚可）。schema 构建/tool_names 等高频路径重复克隆 | 约 40+ 工具的 map 克隆开销；可缓存或改迭代器不物化 |
| P3 | exec_local.rs:154-179 | `resolve_script_path` canonicalize 校验与后续 exec 之间存在 TOCTOU 窗口（校验通过后符号链接被替换可越出技能目录） | 技能目录通常受信，窗口极小；如需加固可在 exec 前用 O_NOFOLLOW 打开 |
| P3 | ssrf.rs:41-53 | `host_is_blocked`（重定向策略回调，同步上下文）内使用 `to_socket_addrs` 做阻塞 DNS 解析，会阻塞 tokio worker | 重定向回调由 reqwest 在异步任务中同步调用；可接受但最好预解析或 async 化 |
| P3 | exec.rs:262-275 | 截断判断用字节数（`stdout.len() > max_output_chars`）对比字符上限（10000），`safe_truncate` 才是字符安全截断；多字节输出下语义轻微不一致（截断阈值实际是 10000 字节而非 10000 字符） | 不影响安全，仅展示层细节 |
| P3 | docs/03_tools_system.md | 契约文档缺口：文档仅泛述"支持任意 shell 命令/路径确认"，未记录 execution_environment/requires_confirmation 正交模型、requests_full_access 唯一规则、会话策略快照机制；docs/22 只覆盖路径访问 | 与代码无矛盾，但 P2-3 架构契约只存在于代码注释中，新贡献者难以发现 |
| — | shell_session.rs:922-934, 1131-1187 | 已检查未发现：poll 累积语义正确——`append_capped` 保留尾部且 UTF-8 边界安全，`poll_once` 每轮只返回新字节并推进 `delivered_offset`，跨轮累积；有针对性回归测试（echo first; sleep; echo second、UTF-8 截断、超时不杀进程、write_stdin 分段危险命令、退出码透传、reaper 保输出/注销 task_manager、owner 空字段 fail-closed） | 1521 行测试覆盖较充分（12 个用例），并发（sessions Mutex + output Mutex 锁序一致、reaper try_lock 跳过忙会话）未见问题 |
| — | execution_env.rs / tool_pipeline.rs:387-429 / registry.rs:561 | 不变量①核对一致：`execution_environment` 纯描述（仅 tool/call 事件与审计，tool_pipeline.rs:400-427 仅用于确认提示文案），`requires_confirmation` 为唯一准入依据且经 registry 逐调用查询；exec（Shell/FullAccess）、exec_local、exec_skill_script 均不逐次确认，只有 full-access shell 需要确认，registry.rs:1096-1110 有测试锁定 | — |
| — | execution_env.rs:120-145 | 不变量③核对一致：`requests_full_access` 是唯一规则——新建看显式 sandbox_policy，复用查 `policies` 创建时快照；ShellTool::requires_confirmation（shell_session.rs:700）与 tool_guard.rs:161-163 共用；显式策略优先于会话快照，与 execute 拒绝不一致策略（shell_session.rs:823-834）闭环 | — |
| — | sandbox.rs:246-262, 310-317 | 不变量④核对一致：ApprovalRequired 路径返回 `PermissionDenied`（硬失败，绝不降级为不受限执行），`prepare_sandboxed` 用 `backend.enforces(policy)` 校验 provider 不得放宽；seatbelt profile 顺序（allow 先于 deny）有专门测试防回归 | — |

## 亮点

1. **纵深防御分层清晰**：validate 硬阻断（灾难级命令）→ DangerousOperation 确认门（较宽规则集）→ OS 沙箱（seatbelt/landlock 硬隔离）→ 路径策略。full-access 是显式 opt-in（config 或逐次确认），provider 契约上无法静默放宽（`prepare_sandboxed` + PermissiveProvider 反例测试）。
2. **shell 会话设计严谨**：fd3 独立命令通道与 stdin 分离（避免交互输入与命令注入互相污染）、marker + UUID 完成追踪、超时不杀进程可续 poll、输出环形缓冲 1MB 上限、跨 stdin 分片的危险命令拼接检测（`normalize_continued_lines`）、退出会话保留未读输出 60s、进程组整体 kill 防泄漏。
3. **SSRF 防护完整**：连接时 DNS 校验（SafeResolver 防 DNS rebinding）+ URL 预检 + 重定向私有地址拦截 + IP 分类覆盖 0.0.0.0/8、CGNAT、IPv4-mapped-IPv6。
4. **seccomp BPF 手写规范**：x32 ABI 拒绝、i386 `socketcall` 旁路封禁、架构校验防多 ABI 绕过，且有 BPF 模拟器测试逐条验证。
5. **SBPL 转义 fail-closed**：控制字符直接拒绝而非转义重写，避免 profile 注入。
6. **嵌套工具守卫对称**：Rhai 脚本内调用与 Runtime 管线共用 `dangerous_operation_items` 与 full-access 规则，无确认环境一律拒绝（fail-closed）并写入审计。
7. 测试文化突出：每个安全关键决策都有对应回归测试（SBPL 行序、provider 不得放宽、owner 空字段、shell 累积语义、UTF-8 截断边界等），注释解释"为什么"而非"是什么"。

## 建议

1. 统一两套危险命令检测器：让 tool_guard 的确认门复用 exec.rs 的 `is_dangerous_command`（或抽取共享 flag 解析），消除 `--recursive` 长选项缺口（P2）。
2. 在沙箱不可强制的维度上如实声明：Linux Landlock 读限制缺失应在 schema 描述或运行时提示中说明，避免"read-only"在 Linux 上被误解为完整隔离（P2）。
3. `is_dangerous_exec_command` 的子串模式（`"kill "`）改词边界匹配，消除误报（P2，低成本）。
4. 补齐块设备 deny 列表的常见变体（vda/md/loop/ram/zram）（P3）。
5. `effective_tools()` 高频路径避免全量克隆，可考虑 Arc 快照或 Cow（P3）。
6. 将 execution_environment/requires_confirmation 正交模型、requests_full_access 唯一规则补进 docs/03_tools_system.md（当前只在代码注释），docs/22 增加沙箱策略与执行环境章节（P3）。
