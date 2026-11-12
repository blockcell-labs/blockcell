# B3 工具与技能审查

> 历史记录（2026-11-11）：原 24 项已于 2026-11-12 复核。当前裁决、限定、工具链验证与新增问题以 [优化指导](README.md)、[复核矩阵](08-revalidation-matrix.md) 为准；本文旧统计及 Rust 阻断说明仅代表首次审查时的结果。

基线：`main` / `5ca38be861b4a3e8f7a1e8d332e242b42ed25710`；日期：2026-11-11。业务源码及测试只读，未执行 Cargo、真实网络、技能脚本、安装或更新。

## 实际覆盖

- 精读 `exec.rs::skill_readonly_roots/execute`、`shell_executor.rs::execute`、`shell_session.rs::create_with_task_manager_and_sandbox/verify_owner/verify_readonly_roots/ShellTool::execute`、`sandbox.rs::build_seatbelt_profile/build_bubblewrap_args/prepare_sandboxed/NativeSandboxProvider::prepare`。
- 精读 `exec_local.rs::execute/sandbox_request/run_command`、`exec_skill_script.rs::load_declared_skill_tools/execute/nested_tool_registry`、`tool_guard.rs::NestedToolGuard`、`registry.rs::execute/with_defaults`，以及 `fs.rs` 读写入口和 `fs_backend.rs::LocalFilesystem`。
- 精读 `skills.rs::ListSkillsTool` 扫描、标准技能身份、脚本资产与 skill_view/skill_manage 委托；交叉核对 `agent/skill_file_store.rs` 的目标解析、快照及目录搜索。
- 精读 `skills/manager.rs` 的加载/重载/格式分类/快照、`openclaw_parser.rs` 标准与 OpenClaw 分流、allowed-tools 与 sidecar 策略；抽查 evolution compile/deploy/rollback 前置条件。
- 精读 `ssrf.rs`、`http_request.rs` 下载分支、`bounded_io.rs`，抽查 web_fetch/search；精读 `file_ops.rs` 的 copy/move、递归与 ZIP/tar 解包路径，抽查 browser/MCP 入口。
- 复核近期 `907b4dc`、`dd5ec46`、`ceed4e9` 的实现与测试。历史报告只用作线索，以下均重新对照当前代码。

覆盖限制：未逐行穷尽 tools/skills 全部模块；没有真实多平台沙箱联调、浏览器/CDP、MCP 服务器或外部内容服务测试。Rust 集成调用链采用静态证明；唯一运行验证为临时目录中的标准库文件操作，见 B3-02，不等同于项目端到端测试。

## 确认发现

### B3-01 [P1] Rhai 嵌套文件工具绕过主运行时的路径策略

- 位置：`crates/tools/src/exec_skill_script.rs:270`、`:289`、`:295`、`:304`；`crates/tools/src/tool_guard.rs:346`；`crates/tools/src/registry.rs:702`；`crates/tools/src/fs.rs:161`、`:186`。
- 前提：已激活一个包含 Rhai 资产且 `meta.yaml` 声明 `read_file` 的技能，允许调用 `exec_skill_script`；全局工具策略未额外禁止 `read_file`。默认工具策略为 Allow，这不需要将敏感目录显式授权。
- 触发：技能脚本执行 `let r = call_tool("read_file", #{path: "~/.ssh/id_rsa"}); set_output(r);`。该示例只作调用链推演，本轮没有读取任何真实凭据。安全回归应改为临时目录中的 deny 路径及哨兵文件。
- 证据：外层 Runtime 的 `path_security.rs:585–598` 对脚本只检查安全相对路径后立即返回；`.rhai` 在 `exec_skill_script.rs:143–148` 不要求全权限脚本确认。嵌套调用仅检查技能自声明的工具名，再经 `NestedToolGuard::admit`；后者只执行 toggles、ToolPolicy、危险操作和 full-access shell 判定，没有 PathPolicy 或会话路径授权。随后调用新建默认 registry 的 `execute`，仅校验参数与 PermissionSet，再直接进入本机文件工具。
- 已排除的兜底：ReadFileTool/WriteFileTool 未重写 `required_permissions`，继承 `lib.rs:718` 的空集合；`fs.rs::read_file_result` 展开路径后直接调用 LocalFilesystem。主 Runtime 默认 deny 的敏感路径（`core/path_policy.rs:135` 起）没有再次判定。
- 影响：原本会被主工具管线拒绝的敏感读取，以及工作区外的写入，可以在技能嵌套调用中执行。技能声明 `read_file` 是工具可用性声明，无法表达或替代对具体路径的授权。
- 修复方向：向嵌套 dispatcher 注入与宿主一致的路径/能力授权接口，且对未知/需确认的嵌套路径 fail closed；或将嵌套文件能力限制到预先授权的 backend。不要仅添加另一组敏感文件名黑名单。
- 验证类型：静态完整调用链；未执行恶意技能。建议回归覆盖同一个 deny 哨兵路径在直接调用和 Rhai 嵌套调用下均被拒绝。

### B3-02 [P1] ZIP 解包会沿目标目录中已有的符号链接写出授权范围

- 位置：`crates/tools/src/file_ops.rs:588`、`:596`、`:601`、`:604`；调用方路径提取为 `crates/agent/src/runtime/path_security.rs:273`。
- 前提：解压目的目录位于允许写入的工作区，但它的某个已有子目录或文件是指向目录外的符号链接；ZIP 含同名路径。例如 `workspace/out/link -> outside`，普通 ZIP 文件条目为 `link/sentinel.txt`。
- 实际行为：代码仅检查 ZIP 条目自身的 Unix mode，拒绝“归档内的符号链接”。对 `dst.join(entry.mangled_name())` 未检查既有目标路径的符号链接/规范化归属，随后 `create_dir_all(parent)` 和 `File::create` 会跟随目标侧链接。外层管线只授权 `destination=workspace/out`，不会逐条检查解包产物。
- 影响：普通文件条目可写入甚至截断目录外文件，越过工作区或 PathPolicy 的写限制。ZIP 条目并不需要 `..`、绝对路径或符号链接类型；既有父目录链接或叶文件链接均可触发。
- 验证：用临时目录和独立 Rust 标准库程序运行与当前解包分支相同的 `create_dir_all → File::create → io::copy` 序列，哨兵从 `original` 变成 `archive-entry`，输出为 `outside_sentinel=archive-entry`。这是文件操作原语验证，未运行项目的完整 `action_decompress`，未接触真实工作区外文件；临时目录自动清理。
- 修复方向：解包时针对每一个目标检查并拒绝既有符号链接组件，使用固定目录句柄及不跟随链接的创建方式抵御竞争；先解到全新隔离目录再安全合并也可。补充目标父目录链接和目标叶文件链接的回归测试。
- 历史复核：旧报告中的“解包拒绝符号链接、未发现逃逸”仅覆盖归档条目类型，当前目标侧缺口仍存在。

### B3-03 [P2] 标准技能列表返回的名称不能用于 skill_view / skill_manage

- 位置：`crates/tools/src/skills.rs:350`、`:354`、`:379`、`:95`；目标解析：`crates/agent/src/skill_file_store.rs:120`、`:136`、`:837`。
- 前提：工作区安装标准技能 `skills/csv_stats_dir/SKILL.md`，frontmatter `name: csv-stats`，目录名与标准身份不同。这是近期新增测试明确支持的布局（`skills.rs:478` 附近）。
- 实际行为：`list_skills` 返回 `name=csv-stats`；将该名称交给 `skill_view({name:"csv-stats"})` 或 `skill_manage`，SkillFileStore 只尝试 `skills/csv-stats`，然后递归按目录 basename 查找 `csv-stats`，不读取 frontmatter 身份，因而返回 `skill not found`。真实目录为 `csv_stats_dir`。
- 影响：按列表结果进行查看、修改、删除的正常工作流失败。即使调用者能从返回的绝对 path 自行推断物理目录，工具之间的 name 契约仍不一致。
- 修复方向：明确区分稳定技能身份与物理目标 ID；列表同时返回可传给文件管理接口的 ID，或使目标解析共享标准 loader 的身份映射，并对重复身份明确报歧义。
- 验证类型：静态生产者/消费者契约核对；已与 B2 审查者独立交叉确认。

### B3-04 [P2] list_skills 把内部快照当作已安装技能，删除后仍会重新列出

- 位置：`crates/tools/src/skills.rs:318`、`:343`、`:406`；快照来源：`crates/agent/src/skill_file_store.rs:56`、`:519`。
- 前提：通过 skill_manage 修改或删除工作区技能，SkillFileStore 将旧技能全目录保存到 `skills/.snapshots/<name>_<stamp>/`；这些目录保留 SKILL.md。
- 实际行为：list_skills 递归扫描所有无技能标识的分类目录，但排除表不含 `.snapshots`，也未统一排除隐藏目录。它会进入快照并登记为可用技能。原生技能出现 `.snapshots/...` 假条目；标准技能因使用 frontmatter name，旧快照与现行版本进入同一个 `seen` 去重集合。
- 影响：已删除技能仍出现在“已安装技能”列表。对标准技能，取决于未排序的 read_dir 顺序，旧快照还可能先占据 name，导致当前版本的路径和说明被跳过。真实 SkillManager 不会把无 manifest 的 `.snapshots` 递归加载，因此目录列表与实际可激活集合分离。
- 修复方向：统一复用技能发现入口，至少跳过 `.snapshots`、锁目录和临时隐藏目录，确保快照不参与身份去重。补“修改/删除 → list_skills”回归。
- 验证类型：静态快照生成与目录遍历调用链。

### B3-05 [P2] 失败的 HTTP 下载会破坏已有 save_to 文件

- 位置：`crates/tools/src/bounded_io.rs:57`、`:60`、`:63`；`crates/tools/src/http_request.rs:440`、`:445`。
- 前提：`http_request` 的 `save_to` 指向一个已有且获准写入的文件，响应体在途中断开、超时，或者超过 100 MiB 限制。
- 实际行为：下载开始即用 File::create 截断原文件，再逐块写入。响应体读取失败通过 `?` 返回，留下半文件；超限则上层直接 remove_file，把原有路径完全删除。
- 影响：工具最终报告下载失败，但先前有效文件已经丢失，无法保留旧版本或安全重试。此问题不依赖绕过路径授权。
- 修复方向：下载到同目录唯一临时文件，完整读完并验证大小/状态后原子替换；失败仅删除本次临时文件。测试预置哨兵内容，在中断及超限后检查原内容保留。
- 验证类型：静态资源与异常路径推演；未执行真实下载或构造大型响应。

## 已核对但不列为缺陷

- 标准技能不受 OpenClaw 开关限制、allowed-tools 优先级与 sidecar 的更严格隐式调用限制，代码和新增单测一致。
- 技能只读根 canonicalize、shell run/write_stdin 的 scope 校验、Landlock 遇无法表达的只读根拒绝执行，均可在当前代码确认；不把平台保守拒绝当作越权。
- `exec_local` 将技能目录纳入可写根是既有脚本执行语义，近期提交明确未改变此策略，不能仅因与 shell 的只读技能资源不同就认定为新漏洞。
- shell poll 没有与 run/write_stdin 同样的只读根比较，但 poll 不注入执行或输入；本轮未证明独立越权后果，未计入确认缺陷。
- 旧 `skill_manage.rs` 仍在仓库，但 registry 使用 `skills.rs::SkillManageTool`；未把旧代码独有的问题冒充活动入口缺陷。

## 统计与建议顺序

确认 **P0 0 / P1 2 / P2 3**。先统一 Rhai 嵌套调用的路径授权、修复 ZIP 目标侧链接处理；随后处理技能目录与目标 ID 契约、排除内部快照、下载失败的文件保全。真实平台沙箱、浏览器、MCP 和全部内容工具留待专项联调，不能从本批结果推出其全部安全。
