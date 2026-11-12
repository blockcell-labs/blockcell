# B6 发布、自更新与评测审查

> 历史记录（2026-11-11）：原 24 项已于 2026-11-12 复核。当前裁决、限定、工具链验证与新增问题以 [优化指导](README.md)、[复核矩阵](08-revalidation-matrix.md) 为准；本文旧统计及 Rust 阻断说明仅代表首次审查时的结果。

日期：2026-11-11；基线：`main` / `5ca38be861b4a3e8f7a1e8d332e242b42ed25710`。本批只新增报告，业务源码、测试和已有文档均未修改。没有联网、发布、执行真实安装或读取私钥；所有额外复现使用提取的当前函数、mock 与自动清理的临时目录。

## 实际覆盖与验证边界

- 精读 `crates/updater/src/manager.rs` 的 manifest 检查、受限下载、暂存元数据、安装前摘要/签名重验、版本保护、健康检查、切换及回滚；核对 `manifest.rs::signature_payload` 与发布工作流的规范 JSON 字段顺序。
- 精读 `atomic.rs` 的锁、备份、原子替换与回滚，`verification.rs` 的签名与健康检查，未做 Windows/Linux/macOS 的真实二进制替换或并发压力验证。
- 精读 `install.sh` 全部 source/release/auto 分支、校验与解包、原子安装，`.github/workflows/release.yml` 的版本门、平台产物、签名/manifest 与发布步骤，`bin/blockcell/build.rs` 和 Makefile 的质量检查入口。
- 精读受跟踪的 `tests/coding_eval/runner.py/common_verify.py` 的 fixture 恢复、子进程、验收与指标；精读 intent_eval 的 fixture/adapter/预算校验；抽查 EAB60 startup、隔离环境、配置与报告入口。
- 对照 `tests/README.md`、README 安装说明与三个基础设施脚本。不审查本轮开始已存在的未跟踪 `tests/eval_kit/`、`tests/realworld48/`，不把它们计入产品基线。
- 主审已统一执行三个 infra 脚本、coding_eval 9 项及 intent_eval 23 项单测，结果通过；本批不重复跑。Cargo 受 Rust 1.92 低于仓库要求 1.96 阻断，因此 updater 的结论来自静态调用链，不能宣称其 Rust 测试已通过。
- 下文 Bash 分类、安装资产查询、coding_eval 超时均实际执行当前提取代码/函数；完整 GitHub Actions、真实安装、付费模型评测未执行。

## 确认发现

### B6-01 [P1] 发布脚本的文件名正则不匹配任何正常 updater 资产，manifest 产物列表为空

- 位置：`.github/workflows/release.yml:370`、`:372`、`:374`、`:376`、`:406`、`:413`。
- 前提：正常发布构建生成诸如 `blockcell-v0.1.8-darwin-arm64.updater` 的资产；归档与 raw binary 的命名来自同一 workflow 的 package/prepare raw updater 步骤。
- 实际行为：四个平台的 Bash `[[ "$file" =~ ... ]]` 正则在扩展名前使用了双反斜杠 `\\.`。该写法不匹配正常的单个点，四个分支均落入 `continue`；循环后 `artifacts_json` 仍为初始值 `[]`，且没有非空校验，继续生成并发布 manifest。
- 运行证据：用 Python 从当前 workflow 原样截取四分支，去掉 YAML 缩进，交给 Bash，分别提供 TAG=v0.1.8 与以下文件名。四个结果均为 `no_match`，退出 0：

  ```text
  blockcell-v0.1.8-darwin-arm64.updater: no_match
  blockcell-v0.1.8-darwin-amd64.updater: no_match
  blockcell-v0.1.8-linux-amd64.updater: no_match
  blockcell-v0.1.8-windows-amd64.exe: no_match
  ```

- 影响：手工下载资产仍会发布，但自动更新消费者找不到对应平台，`UpdateManager::download` 在 `manager.rs:141` 报 No artifact。签名循环也没有机会处理任何合法 raw binary。
- 修复方向：用正确的单反斜杠，或对有限文件名采用明确的 shell case 匹配；发布前断言 manifest 包含四个预期 OS/arch 组合。增加真实执行分类段的测试，而非仅检查 workflow 文本中是否出现若干命令。
- 现有测试漏检原因：`tests/release_workflow_infrastructure.sh` 检查 Actions SHA、权限、版本门、npm/build 命令及 checksum basename，未运行 manifest 分类段或断言产物数量。

### B6-02 [P1] 安装脚本把以横线开头的资产后缀当作 grep 选项，正常 release 安装失败

- 位置：`install.sh:84`、`:96`、`:265`、`:346`。
- 前提：支持的正常平台，例如 Linux x86_64；架构已正确转为 amd64，GitHub API 返回匹配的正常 browser_download_url。
- 实际行为：`suffix="-${os}-${arch}.tar.gz"` 始终以 `-` 开头，却传给 `grep -F "$suffix"`，没有 `--` 或 `-e`。grep 将后缀解释为命令选项，输出错误；随后流水线的 head 返回成功，url 为空，函数返回“没有资产”的状态 2。
- 运行证据：提取当前安装脚本函数定义，只有 curl 用无副作用函数返回正常 GitHub JSON，执行 `github_release_asset_url linux amd64`。实际输出：

  ```text
  asset_lookup_status=2
  grep: unrecognized option `--amd64.tar.gz'
  ```

- 影响：METHOD=release 在正常支持的平台上失败；默认 METHOD=auto 错误地转为源码克隆/构建，要求用户具备 npm/Rust 和构建依赖，违背可用 release 优先安装的行为。
- 修复方向：改为 `grep -F -- "$suffix"` 或 `grep -F -e "$suffix"`，并增加正常 release 安装成功用例。查找流水线本身失败时应保留错误语义，避免误当成“平台没有资产”。完整性异常处理还需同步修正，见下方“被前置缺陷阻断的后继问题”。
- 现有测试漏检原因：安装脚本基础设施测试没有成功 release 用例；checksum mismatch、unsafe archive、auto integrity 都只断言最终失败，没有确认流程确实到达对应校验点。它们可因本条资产查找错误提前失败而全部通过。

### B6-03 [P2] coding_eval 在有输出的 agent 超时时留下 bytes，整批 JSON 报告无法写出

- 位置：`tests/coding_eval/runner.py:165`、`:167`、`:227`、`:283`。
- 前提：某一 agent 子进程在超时前输出过内容，然后超过 `--timeout`。这是 runner 显式支持并应记录的正常失败类型。
- 实际行为：尽管 subprocess.run 设置 text=True，TimeoutExpired 的 stdout 仍可为 bytes。异常分支直接把 `error.stdout` 的切片赋给 agent_output，返回到 `agent_output_tail`；最终 json.dumps 无法序列化，导致主程序在写报告时异常退出。
- 运行证据：直接导入当前 runner，对已存在的 bugfix-01-divide fixture 调用 run_task；agent 使用本机 Python，只执行 `print("timeout-marker", flush=True); time.sleep(5)`，timeout=1 秒。实际输出为：

  ```text
  agent_output_type=bytes
  TypeError: Object of type bytes is not JSON serializable
  ```

  使用 TemporaryDirectory 隔离并清理 workspace；未调用任何模型或服务。
- 影响：单个任务超时可以令整个批次已完成的评测结果无法落盘，丢失报告和失败分类，而不只是产生一个超时任务。
- 修复方向：异常输出统一 decode 到 str，处理 None/bytes/str；增加“有输出后超时”的单测并验证最终 report 可 JSON 序列化。
- 验证类型：实际执行项目当前 `run_task`，复现报告序列化失败；没有运行整套 agent 评测。

## 被前置缺陷阻断的后继问题（不计入当前确认数量）

**安装完整性校验失败未显式终止。** `install.sh:294–295` 依次调用 checksum 和 archive 校验，但没有 `|| return`。外层 release 使用 `if ! install_from_release`（`:335`），auto 使用 `install_from_release || ...`（`:345`），POSIX shell 在此条件上下文中会抑制函数体的 errexit；只依赖全局 `set -e` 无法保证校验失败中止。因此修复 B6-02 后，还会出现打印 SHA 错误但继续解包安装的后继安全缺口。

本轮使用原样 `install_from_release`，在临时目录中用 fake curl/tar 与覆盖后的资产查找函数绕开 B6-02，故意提供错误 SHA。实际输出包括 `SHA-256 checksum mismatch`、`Extracting...`、`install_status=0` 和 `installed_despite_bad_checksum=yes`。这只安装了临时哨兵脚本，未改真实安装路径，也未执行哨兵。该复现明确替换了前置查找，不声称当前默认入口已可到达完整性绕过；应与 B6-02 同批修复并补断言“错误 checksum 后从未调用 tar 提取/安装”。

## 已核对但不列为新缺陷

- `.github/workflows/ci.yml-` 是有意因 Actions 预算关闭，受现有测试和 Makefile 注释明确约束；不将其重命名或记为意外失效。
- updater 的签名载荷绑定 channel/version/OS/arch/URL/SHA；apply 重新校验暂存摘要与签名、拒绝默认降级，并在替换后做健康检查，均可从当前实现确认。
- requireSignature=false 是显式配置且有日志，不把用户明确选择的关闭验证模式当作默认绕过。
- ZIP 链接问题已归 B3；updater 接收独立 raw executable，不能将工具 ZIP 解包缺口重复归到 updater。
- Makefile 的统一入口尚未自动串起全部前端/intent_eval 自测，是覆盖改进项；没有据此声称现有业务逻辑错误或 CI 实际运行过。

## 统计与修复顺序

当前确认 **P0 0 / P1 2 / P2 1**，另有一个受前置缺陷阻断的安装完整性问题，不混入总数。优先同批修复发布 manifest 分类与安装资产查找，并立即完善安装失败传播；随后修复 coding_eval 超时的报告类型错误。Windows 热替换、更新并发/断电恢复和真实发布资产联调仍需后续平台验证。
