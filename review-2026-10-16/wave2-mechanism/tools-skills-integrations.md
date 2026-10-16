# tools 技能与集成集群 Review 报告（Wave 2 · 机制层）

> 只读审查 · 2026-10-16 · 子代理 w2-skills-tools
> 审查范围：crates/tools/src/{skills.rs, skill_manage.rs, skill_manage/, termux_api.rs, termux_api/, napcat/, lib.rs}
> 上下文预算内深读全部 ~9.0K LOC（skill_manage.rs 1314 + helpers 342 + skills.rs 920 + termux 1798 + napcat 3960 + lib.rs 相关段）；对照 BLOCKCELL.md、docs/04_skill_system.md、core/types.rs PermissionSet、registry.rs 执行点。

## 规模

| 文件 | 行数 | 说明 |
|---|---|---|
| skill_manage.rs | 1314 | skill CRUD 工具（FS 实现，含 539 行测试） |
| skill_manage/helpers.rs | 342 | 路径/契约校验、frontmatter 解析、信任级别 |
| skills.rs | 920 | skill_view / skill_manage（Store 委派）/ list_skills（含 480 行测试） |
| termux_api.rs | 425 | TermuxApiTool 定义 + validate + 分发（含 155 行测试） |
| termux_api/actions.rs | 968 | 45 个 action 的 termux-* 命令执行与解析 |
| termux_api/schema.rs | 405 | JSON Schema 程序化构造 |
| napcat/common.rs | 783 | 权限模型、风险分级、WS 调用、媒体下载 |
| napcat/group.rs | 1071 | 群管理工具（读 + 管理员操作） |
| napcat/extend.rs | 1012 | 转发/表情/精华/媒体/下载工具 |
| napcat/user.rs | 633 | 账号/好友/资料工具 |
| napcat/message.rs | 464 | 消息收发/撤回/入群审批/cookies/CSRF |
| napcat/mod.rs | 27 | 模块声明 |
| lib.rs（相关段） | ~600（节选 500+ 行） | Tool trait、ToolContext、SkillFileStoreOps/SkillMutexOps 契约 |

## 模块地图（文件职责表）

| 文件 | 职责 |
|---|---|
| skill_manage.rs | 同名工具"FS 版"实现：create/patch/view/delete/edit/write_file/remove_file；写入前安全扫描 + 失败回滚 + 原子写；per-name 互斥锁 |
| skill_manage/helpers.rs | validate_path_component / validate_skill_file_path / ensure_confined_target（符号链接拒绝）/ find_skill_dir / determine_trust_level / extract_frontmatter / validate_frontmatter / validate_skill_body |
| skills.rs | 注册版技能工具三件套：skill_view、skill_manage（全部委派 agent  crate 的 SkillFileStore）、list_skills（递归扫描 + 资产探测） |
| termux_api.rs | TermuxApiTool：45 动作白名单 validate、Termux 可用性前置检查、动作分发 |
| termux_api/actions.rs | is_termux_available、run_termux_command(+stdin)、parse_termux_output、各 action_* 实现 |
| termux_api/schema.rs | build_schema：action enum + 参数属性 |
| napcat/common.rs | RiskLevel 分级、NapCatPermissionChecker（白名单/封锁/角色/确认）、build_napcat_permissions、build_napcat_user_permissions、call_api(WS)、download_media_to_workspace |
| napcat/{group,user,message,extend}.rs | 各域 NapCat 工具的 Tool 实现（统一模式：check_channel → [check_admin_permission] → ApiRequest → call_api） |
| lib.rs（节选） | Tool trait（execution_environment 描述性 vs requires_confirmation 准入性正交）、ToolContext、SkillMutexOps/SkillFileStoreOps 抽象 |

**关键结构性事实（贯穿全文）**：registry.rs:188 注册的是 `skills::SkillManageTool`（Store 委派版），skill_manage.rs 的 FS 版实现**未被任何地方注册**，仅 `atomic_write_text`/`extract_frontmatter` 被 agent/forked/agent.rs 复用。即 1314 行 skill_manage.rs 中除两个导出函数外整体是死代码，而活路径是 crates/agent/src/skill_file_store.rs（自带 validate_skill_path/ensure_no_symlink_components/scan_learned_skill_*，属另一代理范围）。

## 问题清单（表格：级别 | 位置 文件:行 | 问题 | 证据/说明）

| 级别 | 位置 文件:行 | 问题 | 证据/说明 |
|---|---|---|---|
| P1 | napcat/common.rs:489,442-453 + napcat/extend.rs:887-970 | `download_media_to_workspace` 对 `filename`/`chat_id` 零净化即 `Path::join`，napcat_download_file 工具将两者作为 LLM 可控参数直通 | `filename="../../evil.sh"` → `downloads_dir.join(filename)` 穿越写出；`chat_id="group:../../pwn"` → subdir 穿越；`auto_download_media` 默认 true（config/channels.rs:469）。QQ 侧提示词注入 → agent 调工具 → 宿主任意文件写（内容来自 URL，亦可控）。调用面 extend.rs:954 直传 params。 |
| P1 | napcat/group.rs:343-348 | `check_admin_permission` 把 `PermissionResult::NeedsConfirmation` 当放行（`Ok(())`，仅打 warn 日志） | `admin_permissions.require_confirmation` 配置项对群管理工具是静默 no-op——"需确认"操作实际无确认执行；与 Tool trait 新增的 `requires_confirmation` 准入机制也未接线。 |
| P1 | skill_manage.rs:38 + skills.rs:62 + registry.rs:43,188 | 同名 `skill_manage` 工具双实现并存：注册的是 skills.rs 版（动作集 create/edit/patch/delete/write_file/remove_file/undo_latest、参数 old_text/content/path），skill_manage.rs 版（含 view、old_string/new_string/file_path、category）是死代码 | 两版 schema 漂移（如 view 只在死版、undo_latest 只在活版）；FS 版 20+ 测试强化了一个永不执行的实现，维护者易误判安全覆盖面。 |
| P1 | termux_api.rs:194-259（全工具） | TermuxApiTool 未覆写 `requires_confirmation`（trait 默认 false） | sms_send / telephony_call / clipboard_set / download（任意 URL）/ infrared_transmit / keystore(delete/sign) / wifi_enable 等设备副作用动作无任何用户确认准入；同时未声明 execution_environment（实际 spawn 本地子进程）。安全性完全押注 Termux:API app 自身授权。 |
| P2 | skill_manage.rs:433-449 | `patch_skill` 不校验 frontmatter（create/edit 都校验），patch 掉 SKILL.md 的 name/description 后仍通过扫描并把 meta.json 重写为 `{}` | 违背 BLOCKCELL.md:12 必填 name/description 契约；scan_skill_content 只查危险内容不查契约。虽在死代码内，但 extract_frontmatter 已被复用。 |
| P2 | skill_manage/helpers.rs:237-259 + skills.rs:368 | BLOCKCELL.md:14 明令**禁止**遗留字段 triggers/capabilities/always/output_format，但 validate_frontmatter 只查 name/description 存在性，不拒绝遗留字段；list_skills 输出还对外暴露 `always` | test_extract_frontmatter（skill_manage.rs:1213-1219）主动断言 `always`/`tools` 被解析——与契约反向。docs/04:250 措辞较软（"兼容但不建议新增"），但 BLOCKCELL.md 是硬契约。 |
| P2 | napcat/message.rs:334-454 + common.rs:47-48 | `napcat_get_cookies` / `napcat_get_csrf_token` 归为 ReadOnly 且把会话 cookies/CSRF token 原样塞进工具结果返回给 LLM | 凭据进入模型上下文（可被日志/提示词/后续消息外带），应升 HighRisk 或脱敏/要求确认。 |
| P2 | napcat/common.rs:667-736 | `build_napcat_user_permissions`：allow_from 为空（默认）时所有 QQ 用户获得 `napcat:medium_risk` + `napcat:tools` | send_group_msg / delete_msg / set_group_add_request / set_friend_add_request 等中危操作对任意可触达用户默认开放；registry.rs:580 的 is_subset_of 检查形同虚设。仅靠 channel 层过滤（范围外）兜底。 |
| P2 | skill_manage/helpers.rs:166-177 | `determine_trust_level` 注释声称"其他位置 → Community（较严格）"，代码对一切非 builtin 返回 `Trusted`；且调用方只传 `external_dirs.first()` | 注释与代码矛盾；存在多个 builtin 目录时，次级内置目录里的技能被误判为 Trusted（扫描过严，误拒）。 |
| P2 | skill_manage/helpers.rs:69-101 + skill_manage.rs:406,573,646,710 | `ensure_confined_target(skills_dir, target)` 对外部/内置技能必然失败，patch/edit/write_file/remove_file 改内置技能时报 "resolves outside" 而非 delete 的 "Cannot delete builtin skill" | 错误语义错位（实现上挡住了内置改写——方向正确，但消息误导）；canonicalize 与实际写入之间存在 TOCTOU 窗口（本地 agent 场景可利用性低）。 |
| P2 | termux_api/actions.rs:29-46 | `run_termux_command` 无超时 | termux-location(once/updates)、speech_to_text、fingerprint、dialog 等可无限期挂起，占住工具调用与 worker（可用性/DoS 面）。 |
| P3 | skill_manage/helpers.rs:70,81,136 + skills.rs:40,320 | 阻塞 std::fs（canonicalize/read_dir/metadata）位于 async fn 内 | 大技能树扫描/校验会阻塞 tokio worker；量小可接受，列为模式问题。 |
| P3 | skills.rs:51,323 | `collect_skill_assets`/`scan_skills_dir_recursive` 用 `path.is_dir()`（跟随符号链接）递归，而 skill_manage 的 find_skill_dir 明确拒绝符号链接 | 两套发现逻辑对符号链接策略不一致；符号链接技能可被 list_skills 列出（仅列举，不执行）。 |
| P3 | termux_api.rs:46-93 / schema.rs:38-85 / actions.rs:239,413 | 45 个动作清单三处重复维护（validate 数组、description 字符串、schema enum）；`action_dialog` 把任意 widget 字符串作位置参数、`action_media_player` 把任意 player_action 作子命令；brightness 无 0-255 范围校验 | termux CLI 层 flag 注入（无 shell，影响限于 termux-* 自身报错）；清单漂移风险。 |
| P3 | napcat/mod.rs:11 + common.rs:343-367 | 文档声称 "Mode routing: Automatically routes to HTTP/WebSocket"，实际 call_api 恒走 WS 且 `_config`/`_account_id` 双下划线忽略 | 文档与实现漂移；HTTP 模式已成幻觉。 |
| P3 | napcat/group.rs:325 | sender_role 硬编码 `Some("member")`（带 TODO） | `require_role: admin/owner` 的 tool_overrides 配置永远无法满足（check_role("member","admin")=false → 一律 Denied），角色门是死配置。 |
| P3 | napcat/common.rs:86 | `get_tool_risk_level` 对未知工具默认 MediumRisk | 新工具忘记登记时向"中危可放行"方向 fail-open。 |
| P3 | napcat/common.rs:385-403 | `call_stream_api_via_ws` 整文件缓冲进 `Vec<u8>`，无大小上限 | max_auto_download_size 仅在 channels direct_http 路径生效（downloader.rs:238），WS 流路径无上限 → 内存/磁盘耗尽。 |
| P3 | napcat/message.rs:54 vs extend.rs:74 | message_id 参数类型不一致：get_msg/delete_msg/set_essence_msg 用 integer，get_forward_msg 用 string | LLM 传错类型即 validate 失败；同类参数应统一。 |
| P3 | skill_manage/helpers.rs:180-234 | extract_frontmatter 朴素 YAML 解析：遇第一个 "---" 即截断、无嵌套/多行值/引号内冒号处理 | description 含 "---" 或复杂 YAML 时解析错误（工具自产的 SKILL.md 通常规避，但兼容外部技能时脆弱）。 |
| P3 | skill_manage.rs:281-330 vs 547-569；692-740 | 不一致：create 不调 validate_skill_body 而 edit 调；remove_file 不限制 references/templates/scripts/assets 白名单而 write_file 限制 | 同类动作守卫强度不对称，易成绕过面（均在死代码内）。 |
| P3 | skill_manage.rs:746-765；303-312 | atomic_write_text 临时文件名 = pid+毫秒（同进程同毫秒同路径可冲突）；create 的 exists 检查与 create_dir_all 非原子（跨进程竞态） | skill_mutex 仅串行化同进程同 skill，跨进程/其他调用方（atomic_write_text 是 pub）仍有窄窗口。 |

## 亮点

- **skill_manage 的写路径防御纵深**（虽为死代码但设计值得保留）：写入前扫描（scan_skill_content/scan_skill_dir_with_trust）→ 失败整体回滚（remove_dir_all / 还原原文 / 删文件）→ ensure_confined_target 在 canonicalize 层面拒绝符号链接逃逸 → 测试直接覆盖符号链接场景（test_write_file_rejects_symlinked_support_directory、test_find_skill_dir_rejects_symlinked_skill）。
- **并发设计正确**：skill_mutex 经 `try_acquire(name)` 返回 Drop 守卫、贯穿整个 execute（注释明确"防止 TOCTOU: can_modify + acquire 分离"），比 check-then-act 模式严谨；SkillMutexOps 抽象放在 lib.rs 避免循环依赖。
- **napcat 双权限轨 + 风险分级**：registry 层 PermissionSet 子集检查（含 napcat:napcat_* → napcat:tools 通配符，core/types.rs:152）+ 工具层 NapCatPermissionChecker（block_from 优先、白名单继承 allowed_admins→allow_from、default_policy 默认 deny）；default_admin_policy 默认 "deny"，管理员工具默认全拒，fail-closed 方向正确。
- **termux 无 shell 注入面**：全部经 `Command::args(&[&str])` argv 传参，stdin 通道传递文本；parse_termux_output JSON/文本双模 + 非零退出码透传。
- **list_skills 递归 category 路径累积正确**（skills.rs:337-341，深层 category 不截断）、workspace 覆盖 builtin 去重（seen 集合）、manifest.json skill pack 双扫。
- **PermissionSet 通配符语义**与 is_subset_of 的 AND 判定配合 registry.rs:580 强制点，权限模型闭环可读。

## 各维度结论

- **① 正确性**：termux 45 动作白名单 validate + 必填参数校验完整、分发 match 全覆盖；napcat 工具 unwrap() 均在 validate() 之后（registry 先 validate）安全；已发现问题 #1/#2/#5/#8/#9/#10/#19。执行环境不变量（execution_environment 描述性 vs requires_confirmation 准入性正交）在 termux 上双双缺省（#4）。
- **② 并发**：skill 侧互斥锁/原子写/回滚设计为本集群最佳实践；残留问题 #22/#23（窄窗口）。napcat WS 全局连接复用（channels 侧，范围外）无工具层竞态。
- **③ 安全（主维度）**：供应链边界说明——**社区技能安装入口不在本范围**（community_hub.rs，另一代理负责）；本范围内的供应链相邻面是 skill 脚本资产写入（write_file scripts/，有 scan_learned_skill_content 前置扫描，活路径在 agent/skill_file_store.rs）与执行（exec_skill_script.rs 的守卫调用面，本集群文件不绕过、不自行 exec）。本集群自身问题 #1-#10。
- **④ 性能**：#11 无超时、#18 整文件缓冲、#12 阻塞 IO。
- **⑤ 测试覆盖**：skill_manage.rs 20+ 用例（含安全回滚、符号链接、遍历、尺寸限制——优）；skills.rs 路由+列表测试充分；**termux 仅 validate/parse 单测、无 action 级测试**（可接受但 #11 无回归保护）；**napcat 仅 common.rs 3 个用例——P1 下载路径、check_admin_permission、权限构造均零覆盖**（最薄弱环节）。
- **⑥ 可维护性**：#3 双实现、#14 三处清单、#15 文档漂移、#16 TODO 死配置。
- **⑦ 契约一致性**：meta.json vs meta.yaml——BLOCKCELL.md:11 与 docs/04:54 称运行时契约为 meta.yaml+SKILL.md，但 create/patch/edit 只写 meta.json（docs/04:129 放宽为"任一标识文件即可识别"，加载器兼容，故列 P3 级矛盾而非缺陷）；遗留字段 #6 与 BLOCKCELL.md:14 直接冲突；活版 skill_manage schema（description 必填、undo_latest）与死版漂移 #3。

## 建议（按优先级）

1. **修 download_media_to_workspace 路径穿越**（P1）：filename 只取 `Path::file_name()` 并拒绝含 `/`、`\`、`..` 的输入；chat_id 限定 `^(user\|group):[0-9]+$`；补单元测试（穿越用例 + 正常用例）。
2. **接通确认机制**（P1）：check_admin_permission 的 NeedsConfirmation 改为拒绝或路由到 Tool::requires_confirmation；termux 对 sms_send/telephony_call/download/infrared_transmit/keystore(delete/sign)/wifi_enable 覆写 requires_confirmation；为 termux 子进程加超时（tokio::time::timeout 包裹 output()）。
3. **消除 skill_manage 双实现**（P1）：下线 skill_manage.rs 的 SkillManageTool（保留 atomic_write_text/extract_frontmatter 导出），或把 FS 版独有的 view/frontmatter 校验/回滚测试迁移进活路径 SkillFileStore；文档注明唯一实现位置。
4. **收紧 napcat 权限默认值**（P2）：allow_from 为空时不给 medium_risk/napcat:tools；get_cookies/get_csrf_token 升 HighRisk 或结果脱敏；get_tool_risk_level 未知工具默认 HighRisk 或显式登记。
5. **补契约校验**（P2）：validate_frontmatter 拒绝 triggers/capabilities/always/output_format（对齐 BLOCKCELL.md:14）；patch 路径补 frontmatter 校验防抹除必填字段；list_skills 移除 `always` 输出字段。
6. **修小一致性**（P3）：determine_trust_level 遍历全部 external_dirs 或修正注释；termux 动作清单单一来源化；napcat 文档改 WS-only；message_id 类型统一；sender_role TODO 排期或删除 require_role 配置面。
