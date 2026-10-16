# updater Review 报告（Wave 1 · 基础层）

> 只读审查 · 2026-10-16 · 子代理 w1-updater

## 规模（文件数/LOC/入口）

- 5 个源文件，2,097 行（含测试约 700 行）：
  - `crates/updater/src/manager.rs`（934 行）：`UpdateManager` — check/download/apply/rollback/update_flow、URL 校验、签名门控
  - `crates/updater/src/atomic.rs`（546 行）：`AtomicSwitcher` — 备份/原子替换/回滚、`UpdateLock`（含陈旧锁恢复）、`MaintenanceWindow`
  - `crates/updater/src/verification.rs`（500 行）：`SignatureVerifier`（ed25519）、`Sha256Verifier`、`HealthChecker`（`--version`/`--self-check`/ldd 依赖检查）
  - `crates/updater/src/manifest.rs`（108 行）：`Manifest`/`Artifact` + 规范化签名载荷
  - `crates/updater/src/lib.rs`（9 行）
- 入口：CLI `blockcell upgrade {check,download,apply,rollback,status}`（`bin/blockcell/src/commands/upgrade.rs`）、gateway 自动升级（`commands/gateway.rs:701` 调 `spawn_auto_upgrade_if_enabled`，延迟 30s 执行一次 `update_flow`）
- 补充说明：**本 crate 不做 tar.gz 解压**——发布物是裸二进制，直接流式下载落盘，因此"解压路径穿越"维度不适用（已检查确认无任何解压/归档代码）。

## 模块地图

| 模块 | 职责 | 关键安全机制 |
|------|------|-------------|
| `manager::check` | 拉取 manifest、渠道匹配、版本比较 | HTTPS/回环强制、manifest ≤1MB、首尾 URL 校验、重定向禁降级 |
| `manager::download` | 平台匹配、流式下载、哈希、签名门控、落盘 staging | 流式 SHA-256 与 manifest 声明比对、256MB 上限、`require_signature` 门控 |
| `manager::apply` | 应用前重建信任链（重算 SHA + 重验签名）、维护窗口、前后健康检查、失败自动回滚 | metadata 与调用参数一致性校验、文件名单段校验 |
| `manager::verify_signature` | 公钥解析（config > env > 编译期嵌入）、ed25519 验证 | 失败即 `?` 中止，无"失败仍安装"路径 |
| `manifest::signature_payload` | 规范化 JSON 载荷绑定 channel/version/os/arch/url/sha256 | 防止旧版本合法签名重放包装为新版本 |
| `atomic::switch_to_new/rollback` | 备份 → 临时副本 → 原子 rename；回滚先备份 failed 再恢复 | 文件锁（PID 存活检查 + 陈旧恢复）、魔数+大小校验 |
| `verification::HealthChecker` | `--version`（含期望版本比对）、`--self-check`、ldd/otool | `kill_on_drop` + 超时 |

## 问题清单（表格：级别 | 位置 文件:行 | 问题 | 证据/说明）

| 级别 | 位置 | 问题 | 证据/说明 |
|------|------|------|-----------|
| P1 | manager.rs:368-384（配合 upgrade.rs:86-98） | `apply` 不校验目标版本新旧，构成降级攻击面 | `apply` 仅校验 sha/签名/metadata 一致性，无 `version_greater(version, current)` 检查。CLI `upgrade apply` 直接消费 staging 中持久化的旧版本：若某版本（如 1.5.0）曾被合法下载，之后主机已升级到更高版本，`apply` 仍会把签过名的旧二进制装回去，绕过 `update_flow` 的新版本门（manager.rs:482-489）。`min_host_version` 也被 `StagedUpdateMetadata` 丢弃（manager.rs:553-569），apply 路径不再检查主机兼容性 |
| P1 | verification.rs:53-84（无测试）、manager.rs:440-466（无测试） | 供应链最关键路径——ed25519 签名校验——零单元测试 | `verification.rs` 测试仅有 `test_sha256_compute` 与 healthcheck；`SignatureVerifier::from_hex/verify`、`manager::verify_signature`、`version_greater/version_satisfies`（manager.rs:517-550）均无测试。无"篡改签名被拒""未签名被拒""签名载荷字段变动导致失败"的回归测试；`docs/plans/2026-07-22-updater-review-fixes.md` Task 1 描述的测试实际只覆盖了 `signature_payload` 构造，未覆盖验签本身 |
| P2 | atomic.rs:275-282 | 空/损坏的锁文件导致更新永久阻塞且无恢复路径 | `lock_owner_is_stale` 解析失败时返回 `Ok(false)`（视为存活进程持有）。`create`（atomic.rs:262-272）在 `create_new` 与 `writeln` 之间崩溃会留下空锁文件，此后所有更新/回滚永久报 "Another update or rollback is already running"，陈旧恢复逻辑反而救不了它（需手工删文件）。另 PID 复用也会把死锁误判为存活 |
| P2 | verification.rs:299,304,308 | 非 UTF-8 路径 panic | `self.binary_path.to_str().unwrap()`：安装路径含非法 UTF-8 字符时 healthcheck 的 `check_dependencies` 直接 panic（应 unwrap_or("") 并跳过检查） |
| P2 | manager.rs:662-669 vs docs/12_architecture.md:384-403 | 契约一致性：文档过期 | ①文档把"验证签名（ed25519-dalek）"列为无条件步骤，实际由 `require_signature` 条件门控（默认值 = 编译期是否嵌入公钥，`crates/core/src/config/upgrade.rs:46-48`），配置可显式关闭；②文档示例 `atomic_replace` 用 `with_extension("bak")` 单备份，实现已是带锁/带时间戳多备份/前后健康检查的强方案，文档未反映 staging 元数据重验、健康检查自动回滚、维护窗口、min_host_version 等实际契约；③Windows 路径 `replace_staging_file` 先 remove 再 rename（manager.rs:663-668），非严格原子，与文档"原子替换"措辞有出入 |
| P2 | manager.rs:215-223 | 签名可被配置关闭且无强制手段 | `requireSignature: false` + 用户可改的 `manifest_url` 即可退化到纯 SHA-256 信任（manifest 与产物同源，同源篡改即成功）。代码有 warn 日志属 fail-loud，但属"配置即安全边界"的设计决策，建议文档中明确威胁模型与发布渠道要求 |
| P3 | verification.rs:109-112 | `compute_file` 全量读入内存 | apply 重新校验时对最大 256MB 产物一次性 `std::fs::read`，可改流式（download 路径已是流式，不一致） |
| P3 | verification.rs:15-37 | `from_pem` 宽松解析 | 取解码数据"最后 32 字节"作为公钥，多证书/尾随垃圾 PEM 会取错字节。当前 manager 只用 `from_hex`，未被调用，仍建议收敛或删除死代码 |
| P3 | manager.rs:492-499 | `min_host_version` 仅 update_flow 检查 | CLI `upgrade download`→`apply` 路径绕过该门。可辩护（此即"手动升级"语义），但应在文档中明示 |
| P3 | verification.rs:287 | 消息文案不一致 | `"Self-check Timeout"` 大小写与其余 checks 不一致（纯 cosmetic） |

### 各维度结论

- **① 正确性**：下载完整性（流式哈希比对声明）、原子替换（同目录临时文件 + rename）、回滚路径（先备份 failed 再恢复，恢复失败当前版本不受损）、semver 版本比较（优先 semver crate，预发布语义正确，含测试 manifest.rs:82-107 部分覆盖）——已检查未发现崩溃级 bug；主要缺口是 apply 的版本新鲜度（P1 上表）。
- **② 并发**：下载用唯一临时名 + 原子 rename；切换/回滚有 `UpdateLock`；download/persist metadata 并发为 last-writer-wins 但无害；锁的陈旧恢复跨平台（kill(pid,0)/OpenProcess）。已检查未发现数据竞争；锁空文件死锁见 P2。
- **③ 安全**：验签失败即中止（download manager.rs:216、apply manager.rs:381-384 双重验证，fail-closed）；签名载荷绑定全部元数据防重放；HTTPS 强制 + 重定向禁 https→http 降级 + 响应最终 URL 复校；无解压故无路径穿越；staging 文件名强制单段 Normal component（manager.rs:267-274）；临时文件 0o755、`create_new` 防符号链接竞争一般（staging 目录本身权限取决于 `Paths`，未见加固，可接受）。未发现验签可绕过路径。
- **④ 性能**：流式下载/哈希、manifest 限 1MB、artifact 限 256MB、健康检查有超时；仅 `compute_file` 全量读入内存（P3）。
- **⑤ 测试覆盖**：下载/大小上限/篡改 staging/HTTPS 校验/重定向/回滚/锁/健康检查超时杀进程等场景均有测试；**签名验证明效、版本比较、apply 重验签名**无测试（P1）。
- **⑥ 可维护性**：模块划分清晰、中文注释到位、错误处理统一走 `blockcell_core::Error`；`from_pem` 死代码、少量 `unwrap`（P3）。
- **⑦ 契约一致性**：`docs/plans/2026-07-22-updater-review-fixes.md` 的 8 项修复均已在代码中落实（签名载荷绑定、apply 重验、前后健康检查+自动回滚、公钥策略、HTTP 边界、kill_on_drop、陈旧锁、autoUpgrade.enabled 生效——gateway.rs:701 确认）；但 `docs/12_architecture.md` 自升级章节描述过期（P2）。
- **升级后配置兼容**：不在本 crate 范围（配置迁移属 core 的 config 层，serde `default` 兜底），updater 不读写用户配置，仅替换二进制——已检查未发现不兼容处理缺失。

## 亮点

1. **信任链重建设计**：`apply` 不信任内存中的 manifest，而是从 `current.json` 元数据重建签名载荷并重新验签 + 重算 SHA-256（manager.rs:373-384），覆盖"下载与安装之间被篡改"窗口，且有对应测试（manager.rs:863-886）。
2. **签名载荷绑定元数据**（manifest.rs:43-58）：把 channel/version/os/arch/url/sha256 全部纳入签名，从构造上封死"旧版本合法签名重放为新版本"的经典供应链攻击。
3. **切换后健康检查 + 自动回滚**（manager.rs:412-422）：安装不可运行的产物时自动恢复备份，并保留 failed 版本供取证。
4. **网络层纵深**：HTTPS 强制、重定向降级禁止、初始+最终 URL 双校验、连接/总超时、大小上限（Content-Length 与实际流双查）。
5. **锁工程扎实**：跨平台 PID 存活判断、陈旧锁一次性恢复重试、Drop 释放，并有测试覆盖陈旧/存活两种情形（atomic.rs:530-545）。
6. **健康检查细节**：期望版本比对、`--self-check` 不支持时按 stderr 特征精确跳过而非无条件放过、`kill_on_drop` + 超时有"进程确已终止"的测试。

## 建议

1. （P1）在 `apply` 增加 `version_greater(version, current)` 门控，拒绝降级安装；同时把 `min_host_version` 纳入 staging 元数据并在 apply 复核——与 `update_flow`（manager.rs:482-499）保持一致。
2. （P1）补齐签名路径测试：固定密钥对的"合法签名通过 / 篡改字节 / 篡改 url 等任一载荷字段 / 缺签名 / 错公钥"五例，以及 `apply` 在 `require_signature=true` 下对无签名 staged 的拒绝。
3. （P2）`lock_owner_is_stale` 解析失败时按陈旧处理（或记录后允许人工 `--force` 路径），消除空锁文件死锁。
4. （P2）更新 `docs/12_architecture.md` 自升级章节：反映实际实现（staging 重验、健康检查自动回滚、维护窗口、签名默认策略取决于嵌入公钥）。
5. （P2）`to_str().unwrap()` 改 `unwrap_or_default` 并跳过检查。
6. （P3）`compute_file` 改流式；删除或收紧 `from_pem`。

**P0 数量：0 · P1 数量：2**
