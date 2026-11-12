# 基础存储、Gateway 与前端：反证复核与修复边界

复核日期：2026-11-12。分支 `main`，HEAD `5ca38be861b4a3e8f7a1e8d332e242b42ed25710`。本笔记复核 B1 的 5 项和 B5 的 3 项，不修改原报告、生产源码或测试，不启动真实服务或外联。

结论：原 8 项均有当前实现依据，其中 **6 项 confirmed、2 项 qualified**；没有 rejected。qualified 表示缺陷仍成立，但入口或问题归属需要收窄。原等级保持 **P1 × 2、P2 × 6**。另补充 **2 个独立 P2**；掩码数组错配和配置默认值复活归入原根因，不重复计数。

## 1. 证据与反证方法

- 逐条从入口、规范身份、路径构造、持久化及异常分支向下追踪，重点寻找既有解析器、掩码恢复、序列化和授权兜底能否否定原结论。
- 在临时目录中原样提取当前 session key、配置掩码/恢复、unknown merge 函数，用缓存依赖编译执行；额外直接编译当前 `config/provider.rs`，没有重写其 ModelEntry/ProviderConfig 定义。
- 使用项目 esbuild 将当前 `webui/src/lib/store.ts` 打包，提供最小人工 window/localStorage/document，直接调用真实 store。未修改 store、mock 其内部实现或改变现有断言。
- 本批没有重新运行 Cargo workspace 或浏览器 E2E。主审另行负责工具链与 crate 测试；旧报告所称 Rust 不可运行不应在工具链重新核查后直接沿用。本批最小程序不等同于完整 Gateway、SessionStore、MemoryStore 的端到端执行。

## 2. 原发现逐项裁决

| ID | 裁决 | 等级 | 复核后的准确结论 |
| --- | --- | --- | --- |
| B1-01 | qualified | P1 | 存储 legacy 回退/双删成立；HTTP 先经有损 ID resolver，不能一概描述成直接 clear(B)。WS `/clear` 是确定的双删入口。 |
| B1-02 | confirmed | P2 | 保存把已删除动态 map 项插回；同一根因还会恢复被 serde 省略的默认值，导致 Provider 协议无法改回 openai。 |
| B1-03 | confirmed | P2 | structured/raw/reload 的验证弱于启动，合法类型但非法值可成功写盘并阻断重启。 |
| B1-04 | confirmed | P2 | token 数字被掩码；数组调整后按旧下标恢复，能静默改错 Agent 的真实额度。原位原样回写通常可恢复，不能宣称必然写坏。 |
| B1-05 | confirmed | P2 | 缺失记忆 ID 的真实 Display 文案不匹配外层字符串条件，返回 Err 而非 None。 |
| B5-01 | confirmed | P1 | WS 与普通渠道 slash context 都拿根 Paths，非默认 Agent `/clear` 清错持久目录。 |
| B5-02 | qualified | P2 | 后端列表/resolver 与前端 normalize/send 是两个可独立触发的身份缺口，应分开修复和验收；默认自动 ID 的常规往返可成功。 |
| B5-03 | confirmed | P2 | reset/error 缺少聊天作用域过滤；服务端合法投递旧订阅事件后会修改当前会话。 |

### B1-01：明确区分存储 legacy 缺口与 HTTP resolver

**位置与前提。** `core/session_key.rs:30`、`:52`；`storage/session.rs:115`、`:299`、`:410`。同一 Agent 中 A=`ws:a:b`，B=`ws:a_b`。A 当前 stem=`ws_a_b`，B 当前 stem=`s~ws~3Aa_b`，B legacy stem=`ws_a_b`，不需要任何旧版本遗留文件。

**反证结果。** 当前 stem 不相同，确实否定了“当前编码本身直接碰撞”的说法；但 `read_path` 在 B 当前文件缺失时无归属检查地选择 legacy，`clear` 在 B 当前文件存在时依然删除 legacy。会话 metadata 不包含可校验的原始 session key（`session.rs:94`），无法靠当前第一行判断文件属于 A 还是 B。运行时 `runtime/process_message_inner.rs:393` 直接按持久 key 加载，故首次进入 B 可以实际走回退。

HTTP GET/DELETE/rename（`gateway/sessions.rs:197`、`:254`、`:297`）先调用 `resolve_session_key_from_id`。`session_file_stems`（`:46`）采用未排序 read_dir；A/B 列表 id 都是 `a_b`。当前源函数实测：

```text
stems A=ws_a_b B=s~ws~3Aa_b legacyB=ws_a_b
list IDs A=a_b B=a_b
resolve a_b [A,B]=ws:a:b
resolve a_b [B,A]=ws:a_b
resolve a_b [A]=ws:a:b
```

因此只存在 A 时，HTTP 请求 B 的外观 id `a_b` 会先被解析成 A；A/B 并存时，目录顺序决定后续 key。若得到 A，删除 A；若得到 B，`clear(B)` 删除 A/B 两者。HTTP 还会读错或改错元数据，但不能把每种目录顺序都说成双删。WS `/clear` 在 `websocket.rs:419`、`slash_commands/handlers/clear.rs:35` 直接构建 `ws:a_b`，不经过这个 resolver，是更直接的存储缺陷复现入口。

**修复边界。** legacy 迁移需要可验证归属；在归属不明时拒绝自动读取/删除，不能再按有碰撞名称猜测。修复存储回退不替代修复 B5-02 的 HTTP ID 契约；增加排序只能稳定错误选择，不能消除歧义。

**验收。** 用人工 JSONL 在同一临时 Agent 中先建 A，首次 load(B) 不得返回 A；同时保存 A/B，再 clear(B)，A 的 JSONL、events、dream marker 内容均保持；分别交换文件枚举顺序验证 HTTP GET/DELETE/rename 不会指向另一个会话。禁止以先删除旧文件“完成迁移”。

### B1-02：动态 map 删除与省略默认值均被旧值复活

**位置与前提。** `config.rs:460` 无差别递归插回缺键；`Config::save`（`:976`）先序列化再调用该合并。WebUI `llm-page.tsx:150` 确实删除 provider，`config-merge.ts:48` 用删除后的 providers 替换旧对象；Gateway `config_api.rs:69` 调用 save。没有其他删除标记能覆盖这次合并。

**证据。** 当前 merge 函数执行 `{providers:{old:...}} → {providers:{}}`，结果重新包含 old。另直接编译当前 `ProviderConfig`（`config/provider.rs:22`）：old apiType=`anthropic`，incoming apiType=`openai`；默认值被 serde 省略，再被 unknown merge 补回 `anthropic`，断言通过。这是对原发现的具体扩展，不另报问题。

**影响边界。** 旧对象键可以复活，不能据此概括“所有删除均无效”：普通数组整体替换，不经过此递归；显式序列化为 null/false 的字段也不同于被省略的键。`channels.*.accounts`、channelAccountOwners 等动态 map 需要与 providers 一起验收；`ExecConfig` 的 `sandboxPolicy`/`approvalPolicy` 设为 None 后省略也可能保留旧值（`config/tools.rs:63`、`:68`）。

**修复与验收。** 根据已知 schema 区分扩展字段、动态 map 与显式删除；对 save 使用完整对象替换语义或明确 patch/删除协议。验收从落盘删除 provider/账号/路由后重载仍缺失，anthropic→openai 后重载为 openai，安全配置清空后不恢复旧值；真正未知的扩展字段仍按承诺保留。只给 providers 单独加删除特例不足以关闭此项。

### B1-03：保存前校验必须与实际启动一致

**位置与证据。** `config.rs:756`、`:760` 进入 parser，`:793` 仅 serde；raw 写盘 `:796` 随后直接 atomic write；structured `config_api.rs:69` 无语义校验；reload `:107` 同样只调用 parser。真正语义验证是 `Config::load` 的 `:831`，检查 approval policy、cron 范围和 IANA 时区（`:936`）。Gateway 启动 `gateway.rs:686` 经 `load_or_default`，该函数已有文件分支 `config.rs:841` 用 `?` 传播错误，未无条件回落默认。

**限定。** 类型错误通常被 serde 拒绝，此项针对 `cronTickIntervalSecs=0`、`defaultTimezone="Mars/Olympus"` 这样的类型合法/值非法输入。重启前旧进程未必立即退出，不能描述为写盘瞬间 DoS。

**修复与验收。** 所有写入口共用完整语义验证，并保证失败不改变旧文件；保留 raw 文本/注释不是跳过值验证的理由。用 structured、raw、reload 三入口检查 cron 上下界、时区与 approvalPolicy，校验结论须与重启 loader 一致。不能只改前端表单。

### B1-04：从错误展示扩展到数组重排后的静默配置错配

**位置与反证。** `config.rs:548` 按 token 等子串识别秘密，`:572` 对任意非 null 类型覆盖成字符串；Gateway GET（`config_api.rs:24`）使用它。`restore_masked_secrets`（`:595`）确实能恢复原位的 mask，原报告不应被解读为每次 echo 都损坏配置。

**新确认后果，归此项计数。** 恢复数组分支 `config.rs:614–619` 只 zip 下标，不按元素 id。当前 schema 明确允许 `agents.list[].maxTokens`/`maxContextTokens`（`config/agent.rs:312`、`:326`）。通过配置 REST 客户端 GET 后删除第一个 Agent，再把余下对象 PUT 回，剩下 Agent 的 mask 从旧数组第 0 项恢复，服务端接受、保存的就是另一个 Agent 的额度。当前源函数执行与断言：

```text
disk agents.list:
  ops maxTokens=1024 maxContextTokens=4096
  dev maxTokens=8192 maxContextTokens=32768
GET(mask) -> remove ops -> restore(current, incoming)
remaining={"id":"dev","maxTokens":1024,"maxContextTokens":4096}
```

该后果要求删除/重排数组，普通配置页原样携带 agents.list 不会自动触发。不能用 modelPool 删除第一个模型作为“额度被错误继承”的充分证明：Rust ModelEntry 根本不支持该字段，后续 serde 会把它丢掉，见 FE-N01。也没有据此声称当前真实 API secret 已跨 Agent 泄露。

**修复与验收。** 用精确凭据 schema/路径保留数值和容器类型；数组上的“保留原秘密”必须按稳定身份定位，无法定位应要求显式新值或拒绝，不能按下标猜。验收普通额度以数字原样 GET、原样 echo、交换 agents.list 顺序、删除首项、插入新项及凭据替换/清空；余下 Agent 的额度保持其自身值，token 凭据仍被掩码。

### B1-05：不存在的记忆不是存储故障

**位置与证据。** `memory/crud.rs:203` 包装 get_by_id_inner；`:211` 检查 `QueryReturnedNoRows`；`:224` 先通过 Display 格式化 rusqlite 错误。锁定 `rusqlite-0.31.0/src/error.rs:288` 的实际文案为 `Query returned no rows`。大小写和空格不同，匹配永远不会命中该缺失错误；CLI `commands/memory.rs:157` 的 None 分支因此不可达。

**反证边界。** 数据库尚不存在时 CLI 有更早的友好返回；本项前提是已有可打开数据库中的缺失 ID。不意味着数据库异常也应吞为 None，也不是仅依赖将来版本改错误文案的风险。

**修复与验收。** 在转换为 core Error 前用 rusqlite typed variant 或 OptionalExtension。验收已存在返回 Some、缺失/删除后返回 None、真实 SQL/schema 错误仍为 Err，CLI 缺失对象正常提示并成功退出。本批未运行完整 MemoryStore。

### B5-01：多 Agent 的 `/clear` 路径选择错误

**位置与证据。** `websocket.rs:420–425` 传 `state.paths.clone()`，`slash_commands/context.rs:99–110` 原样保存，`clear.rs:49–50` 构造 Store 并清理，`:63`、`:99` 还删除该 Paths 的 session memory 和工具结果。`Paths::for_agent`（`core/paths.rs:165–179`）明确将 ops 指向 `base/agents/ops`；WS 没有调用它。正常渠道 `gateway.rs:1375–1390` 解析了 agent，却同样传根 slash_paths。HTTP sessions 接口已经调用 for_agent，不能把全部删除接口一概说成没隔离。

**必要前提。** ops 等非默认 Agent 存在。若默认 Agent 存在相同 chat id 的数据，会误删默认数据；没有同名数据时表现为 ops 清理无效。P1 来自确定的数据边界/删除对象错误，未声称一个未经登录的用户可以直接发起 WS 请求。

**修复边界。** 在所有 slash context 创建处使用解析后的 Agent Paths，并确保内存缓存、磁盘历史与派生资源使用同一个 canonical key。普通渠道还丢失 account：`gateway.rs:1377` 与 `clear.rs:35` 自行拼 channel/chat，未使用 `InboundMessage::session_key()` 的账号编码（`core/message.rs:21`）。后者应与本轮账号专项结论合并，不能认为只加 for_agent 就完成多账号清理。

**验收。** 默认/ops 同时存在同 chat id，ops `/clear` 只影响 ops 的历史、event、memory、tool-results 与 cache；默认 Agent 的人工哨兵不变。分别跑 WS 和渠道入口，以及同 channel/chat、不同 account 的清理。禁止只单测 ClearCommand 接收“正确 Paths”而漏测入口是否传对。

### B5-02：分开关闭后端 ID 与前端 ID 的两个缺口

**后端子项。** `session_key.rs:79–89` 将 A 的传统 stem 与 B 的编码 stem 都解为 `a_b`；`:100–126` resolver 选首个同 id 项。`sessions.rs:119` 列表输出该 id，GET/DELETE/rename 均使用同一 resolver，因此无需 UI normalize 就已存在歧义。删除风险与 B1-01 的 legacy 双删应分别验收；不要将修复其中一个视为另一个已关闭。

**前端子项。** `store.ts:6` 以冒号转下划线比较事件（`:268`）和绑定 ID（`:282`）；`chat-page.tsx:352`、`:372` 又把下划线转回冒号。服务端 `assign_session_id`（`gateway/chat.rs:9`）保留任意非 default id，当前协议没有禁止字面下划线。真实 store 重现：当前 a_b 收到了 a:b 的 `wrong-session` token。即使先让 HTTP 完全无损，WS 仍有此问题。

**排除误报。** 本次实测 `default:123 -> default_123 -> default:123`，常规自动会话的下一轮没有因为这一对替换必然分叉。自定义 ID、原有字面下划线与跨会话混入才是确定缺陷；不扩大成 Gateway 身份认证绕过。

**修复与验收。** 为 API 返回完整、无损、带必要 channel/account/Agent 语义的 opaque handle，读取/删除/重命名只按该 handle 解析；前端不再自己编码/反编码身份。兼容迁移必须能够明确识别旧格式，歧义返回错误。保留原 B5-02 作为总 ID，设置两个必须分别通过的子验收：HTTP A/B 的 list/get/rename/delete；WS 的 token、session_bound、send/cancel 在 a:b 与 a_b 间严格隔离。可选后续拆工单，但不得重复算作新增发现。

### B5-03：服务端过滤正确不等于 UI 当前会话过滤正确

**位置与可达性。** `store.ts:263` 的 chatEventTypes 不含 stream_reset/error，`:429–436` 无条件删当前 streaming assistant，`:539–546` 无条件添加错误并停止 loading。`websocket.rs:395–401` 订阅发送过的会话并累计；`ws_event_visible_to_connection`（`:99`）只验证该连接曾订阅该 Agent/chat，并不知道 UI 当前选择。`message_dispatch.rs:480–483` 的 reset 真实包含原 Agent/chat，故可被 UI 正确区分但当前遗漏。

**动态证据。** 当前 chat-B 放置一条 streaming assistant，输入 chat-A/ops 的 reset 后 message_count=0；继续输入其 error，chat-B 出现 `❌ Error: A failed`，isLoading=false。真实 store 源码执行，断言通过。

**修复与验收。** 将所有修改聊天 state 的有归属事件统一按当前 Agent/chat 过滤；连接层、无归属登录错误等保留独立全局错误通道。验收切换会话和 Agent 后旧订阅的 reset/error 不改当前状态，本会话 reset/error 正常工作。不能靠 WS 不再保留订阅来掩盖 store 契约缺口，也不能简单忽略所有 error。

## 3. 新发现：独立根因

### FE-N01 · confirmed · P2：LLM 页面每模型 Max Tokens 设置会被后端静默丢弃

**位置。** `webui/src/components/llm/llm-page.tsx:50`、`:168`、`:599–600` 提供字段和输入；`webui/src/lib/config-merge.ts:38` 将它写入 `agents.defaults.modelPool[]`；后端 `crates/core/src/config/provider.rs:91–119` 的 ModelEntry 没有 max_tokens；`providers/src/pool.rs:158–170` 没有传递该设置，`providers/src/factory.rs:167` 始终取 `config.agents.defaults.max_tokens`。

**前提与实际行为。** 用户在 LLM 页面修改某个模型的 Max Tokens 后保存。Gateway `config_api.rs:69` 的 Config 反序列化默认忽略未知字段，save 后该数组项不再有 maxTokens；`merge_unknown_keys` 只遍历 Object，不会恢复数组内未知项。API 仍报告成功，Provider 实际输出上限沿用全局/Agent defaults，用户对某个模型的设置无效。

**运行证据。** 直接编译当前 `config/provider.rs` 的真实 ModelEntry，再执行当前 merge 函数：输入 `{model:"m",provider:"p",maxTokens:4096}`，typed-save 输出 `{model:"m",provider:"p",weight:1,priority:1}`；断言 maxTokens 缺失。即使 raw config 曾存该键，ProviderPool 也从未消费它。没有向真实模型请求。

**与旧发现的区别。** B1-04 修正掩码只能修读取数值，无法让不存在的 Rust 字段生效；此项是前后端 schema/运行配置契约缺失，不能用“数值不再被 mask”关闭。对纯 raw 文件的未知字段保留承诺也不能冒充 Provider 行为支持。

**修复边界与验收。** 明确产品选择：实现 ModelEntry 可选 maxTokens、逐模型覆盖/回退并传入全部 Provider 构建路径；或将 UI 明确改为真实的全局/Agent 设置。无论哪种选择，都不能保留当前可编辑但无效的逐模型输入。验收设置两个模型为不同上限，保存、重载后的值和 mock Provider 收到的请求上限一致；缺省条目按既定规则回退，类型/范围错误明确拒绝。不要只加 serde 字段而漏掉 pool/factory 消费。

### FE-N02 · confirmed · P2：全局流列表与默认工作区停止接口不一致，非默认 Agent 流无法管理停止

**位置。** `bin/blockcell/src/commands/gateway/streams.rs:7` 全局 list，`:24–30` DELETE 固定 `state.paths.workspace()`；`crates/tools/src/stream_subscribe.rs:275–285` 按 ToolContext workspace 操作，`:606` 保存 owner_workspace，`:1130–1135` 停止前要求 owner_workspace 完全匹配；public wrapper `:1408` 只规范化调用者传入的 workspace。

**前提与调用链。** ops Agent 用 stream_subscribe 创建一条流，归属 `base/agents/ops/workspace`。GET `/v1/streams` 的 `list_streams`（`:1417–1433`）返回所有 Agent 流但不返回 owner Agent；CLI `commands/streams_cmd.rs:155–167` 从这个列表解析 ID 并 DELETE。Gateway 不接收 AgentScopedQuery，传入的始终是根 workspace，所有权检查确定失败，返回 Stream not found。流和它的任务仍在运行；支持 `auto_restore` 时其 ops 落盘规则也未删除。

**影响与限定。** 已登录管理者/本机 CLI 无法通过现有 stop 管理入口停止非默认 Agent 的流。不是跨 Agent 越权：工具层的归属检查实际上正确拒绝；全局管理员列表本身也不能直接判为泄露。ops 自己通过带正确 workspace 的工具 unsubscribe 仍可停止。WebUI 当前流页面只读展示，没有声称其存在停止按钮。

**证据强度。** 当前生产调用链和不可变目录关系的严格静态证明；没有创建真实流、连接外部服务器或运行完整 Gateway。

**修复边界与验收。** 为列表条目带上稳定 owner Agent，并让管理 API/CLI 显式传递 Agent、使用 for_agent 选择 workspace；或由受权的管理层安全解析流所属对象。保留工具层 owner 校验，不能删除它来使 DELETE 成功。用人工 registry 注入默认/ops 两条流及 fake cancel handle，验收各自停止只取消本条并更新正确 workspace 的规则，错误 owner 不影响任何流；CLI 解析前缀应带所属 Agent，restore 也应明确其目标范围。

## 4. 本批运行结果与建议修复分组

| 最小检查 | 实际结果 | 证明限度 |
| --- | --- | --- |
| 当前 session key/resolver + mask/restore + unknown merge 函数 | 编译成功，所有断言通过；打印上述 ID 顺序差异及 Agent 额度错配 | 未执行完整 SessionStore/HTTP |
| 当前 ModelEntry/ProviderConfig + merge | maxTokens 被丢弃；openai 被恢复成 anthropic，断言通过 | 未调用真实 Provider |
| 当前 store.ts 打包后执行 | a:b token 混入 a_b；旧作用域 reset/error 改当前状态；默认自动 ID 往返成功 | 人工浏览器对象，无浏览器 E2E |
| Gateway stream、启动校验、MemoryStore 缺失分支 | 源码与锁定依赖完整链核对 | 静态，未运行真实流、重启或 SQLite Store |

建议按行为组合修复：先处理 B1-01/B5-01 的删除对象边界，并协调完整账号 key；随后联合处理 B5-02 的 HTTP handle 与前端 opaque ID；配置组同时关闭 B1-02/03/04，包含默认值复活和数组身份恢复，另明确 FE-N01 的产品契约；最后处理 B5-03、B1-05 与 FE-N02 的各自行为回归。每组的关闭依据应是上面的行为验收，不是仅既有单测通过。

没有把以下情况算作新缺陷：有总管理员 token 的 HTTP 管理接口可以选择多个 Agent；Memory/File/Session HTTP 的 for_agent 路径选择当前已存在；文件 canonicalize 与打开间的竞态仍需专门并发证明；潜在 Agent 目录编码碰撞需要独立输入约束与平台核查。本批不提供整个 Gateway/存储层“已无遗漏”的保证。
