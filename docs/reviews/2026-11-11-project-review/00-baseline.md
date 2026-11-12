# B0：基线与验证

> 历史记录（2026-11-11）：原 24 项已于 2026-11-12 复核。当前裁决、限定、工具链验证与新增问题以 [优化指导](README.md)、[复核矩阵](08-revalidation-matrix.md) 为准；本文旧统计及 Rust 阻断说明仅代表首次审查时的结果。

日期：2026-11-11；分支：`main`；HEAD：`5ca38be861b4a3e8f7a1e8d332e242b42ed25710`。

## 范围与工作区

审查计划见 `docs/plans/2026-11-11-project-review.md`。选定目录内 624 个受跟踪文件，受跟踪 Rust/TS/TSX/Python/Shell 约 278,827 行；本轮采用关键路径精读与调用链核对，不能解读为全仓逐行验证。

初始 `git status --short`：

```text
?? docs/reviews/
?? tests/eval_kit/
?? tests/realworld48/
```

原有未跟踪目录保留。新计划位于仓库已有忽略规则覆盖范围内，但文件已经实际写入；报告未进行 Git 提交。没有创建分支、worktree，没有修复业务源码。

## 实际验证

| 命令 | 结果 | 能证明的范围 |
| --- | --- | --- |
| `cargo test --workspace --offline --locked` | **阻断，exit 101** | 当前 rustc 1.92.0，workspace 要求 1.96；未进入编译/测试，不能据此判断 Rust 测试通过或业务实现失败 |
| `npm test --prefix webui` | **29/29 通过，exit 0** | 项目现有 lib 层单测和测试配置类型检查 |
| `webui/node_modules/.bin/tsc -b webui/tsconfig.json --pretty false` | **通过，exit 0** | 前端项目 TypeScript 编译检查；未运行浏览器 E2E |
| `python3 -m unittest discover -s tests/coding_eval -p 'test_*.py'` | **9/9 通过** | coding eval runner/acceptance 的自测；不是实际运行模型做题 |
| `python3 -m unittest discover -s tests/intent_eval -p 'test_*.py'` | **23/23 通过** | intent eval runner 的自测；不是模型路由能力测评 |
| `sh tests/build_supply_chain_infrastructure.sh` | **通过** | WebUI hash manifest 当前有效、Cargo metadata 继承、已知构建配置规则 |
| `sh tests/release_workflow_infrastructure.sh` | **通过** | workflow 文本约束；包含“预算不足而保持 CI 禁用”的显式断言 |
| `sh tests/install_release_infrastructure.sh` | **通过** | 临时目录和 fake binaries 下的安装/校验/原子替换；没有真实安装 |

Node：v26.4.0；Python：3.12.7；Cargo/rustc：1.92.0（Homebrew）。没有下载工具链、修改 lockfile 或绕过 rust-version。

前端 `npm test` 会重新生成受跟踪的 `webui/.test-dist/store.test.js`；初始此文件干净，验证后已仅恢复该项本轮生成的改动。没有修改测试断言使检查通过。

## 限制

- Rust workspace 的测试、clippy 和完整 Gateway/SQLite 集成行为未验证；各报告必须明确哪些结论来自静态调用链。
- 现有前端单测全部通过不代表页面交互和所有 WS 事件隔离正确，本轮另对真实 store 源码打包后执行最小复现，见 B5。
- 未运行真实渠道、外部模型、发布、自更新、跨平台沙箱及性能/并发压力验证。
- 历史 review 仅用于回归线索，当前发现重新定位到本次 HEAD。
