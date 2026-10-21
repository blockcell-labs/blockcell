# Contributing to BlockCell / 参与 BlockCell 贡献

感谢你帮助改进 BlockCell。本文说明提交问题、修改代码和准备 Pull Request 时应遵循的基本约定。

Thank you for helping improve BlockCell. This guide describes the basic expectations for issues, code changes, and pull requests.

## 开始之前 / Before You Start

- 对缺陷修复，请描述复现步骤、预期行为、实际行为以及运行环境。
- 对较大的功能或架构变更，建议先创建 issue，确认范围和兼容性要求。
- 不要在公开 issue、日志、测试数据或提交中包含 API Key、Token、密码及个人数据。

- For bug fixes, include reproduction steps, expected behavior, actual behavior, and environment details.
- For substantial features or architectural changes, open an issue first to align on scope and compatibility.
- Never include API keys, tokens, passwords, or personal data in public issues, logs, fixtures, or commits.

## 开发环境 / Development Environment

BlockCell 是 Rust 2021 workspace，最低支持 Rust 1.96（工具链版本固定在 `rust-toolchain.toml`，与 `Cargo.toml` 的 `rust-version` 保持一致）。克隆仓库后可运行：

BlockCell is a Rust 2021 workspace with a minimum supported Rust version of 1.96 (the toolchain is pinned in `rust-toolchain.toml` and kept in sync with `rust-version` in `Cargo.toml`). After cloning the repository, run:

```bash
cargo build --workspace
cargo test --workspace
```

修改单个 crate 时，优先运行对应测试以缩短反馈时间，例如：

When changing one crate, prefer its focused tests for faster feedback, for example:

```bash
cargo test -p blockcell-core
cargo test -p blockcell-skills
```

### 本地测试门禁 / Local test gate

CI 在获得 GitHub Actions 预算前保持禁用，因此本地门禁是唯一的自动化防线：

CI stays disabled until the project has GitHub Actions budget, so the local gate is the only automated line of defence:

```bash
make test        # cargo test --workspace + harness 自测 + 3 个基础设施脚本
make test-webui  # WebUI 单测（需要 webui/node_modules）
make hooks       # 安装 .githooks/pre-push：推送前自动运行 make test
```

`make test` 会在 `webui/dist` 过期时自动重建（Rust 测试嵌入该产物，`bin/blockcell/build.rs` 会校验哈希）。
单次推送可用 `BLOCKCELL_SKIP_HOOKS=1 git push` 跳过钩子。

`make test` rebuilds a stale `webui/dist` automatically (Rust tests embed it and `bin/blockcell/build.rs` verifies its hash). Skip the hook for a single push with `BLOCKCELL_SKIP_HOOKS=1 git push`.

### tests/ 目录 / The tests/ directory

`tests/` 存放两类资产，都不是产品行为的 e2e 测试：

`tests/` holds two kinds of assets; neither is a product-behaviour e2e suite:

- `tests/coding_eval/` — agent 编码能力评测 harness（20 个隔离任务，`runner.py` 驱动）。自测用 `make test-python`。验收脚本在运行测试前会从原始 fixture 恢复 `test_*.py`，并要求至少跑过 1 个测试，因此删改验收测试无法伪造通过。
- `tests/*.sh` — 安装 / 发布 / 供应链基础设施测试，用 `make test-infra` 运行。

- `tests/coding_eval/` — the coding-ability benchmark harness (20 isolated tasks, driven by `runner.py`); self-tested by `make test-python`. The acceptance step restores the fixture's `test_*.py` before running and refuses a run that discovered no test, so deleting or weakening the acceptance tests cannot fake a pass.
- `tests/*.sh` — install / release / supply-chain infrastructure tests, run by `make test-infra`.

## 修改原则 / Change Guidelines

- 每个提交只处理一个清晰问题，避免混入无关格式化、生成文件或本地系统文件。
- 缺陷修复应先增加能复现问题的测试，再进行最小实现修复。
- 保持公开配置、CLI 参数、文档和实际运行行为一致。
- Skill 修改必须遵循 [`rules/README.md`](rules/README.md) 中的当前执行规范。
- 不要提交 `.DS_Store`、密钥、本地配置、构建产物或无关的工作区改动。

- Keep each commit focused on one clear concern; avoid unrelated formatting, generated files, or local system files.
- For bug fixes, add a test that reproduces the problem before implementing the minimal fix.
- Keep public configuration, CLI options, documentation, and runtime behavior consistent.
- Skill changes must follow the current execution contract in [`rules/README.md`](rules/README.md).
- Do not commit `.DS_Store`, secrets, local configuration, build artifacts, or unrelated workspace changes.

## 提交前检查 / Pre-Submission Checks

至少运行与改动直接相关的测试，并执行格式检查：

Run at least the tests directly related to the change and check formatting:

```bash
cargo fmt --all -- --check
cargo test -p <affected-package>
```

如果改动跨越多个 crate、共享配置或公共接口，请运行完整 workspace 测试（等价于 `make test-rust`）：

For changes spanning multiple crates, shared configuration, or public interfaces, run the full workspace tests (equivalent to `make test-rust`):

```bash
cargo test --workspace
```

提交前建议直接运行 `make test`（见上文「本地测试门禁」）。执行过 `make hooks` 的克隆会在 `git push` 时自动运行它。

Before pushing, prefer running `make test` (see "Local test gate" above). Clones that ran `make hooks` run it automatically on `git push`.

文档修改还应确认相对链接存在，命令示例与当前 CLI 帮助一致。

For documentation changes, also verify that relative links resolve and command examples match the current CLI help.

## Commit 与 Pull Request

- 使用简洁、可读的提交说明，明确修改目的。
- Pull Request 描述应包括：问题背景、解决方案、验证方式和可能的兼容性影响。
- 保持提交历史可审查；根据维护者反馈补充修复时，避免顺手修改无关文件。

- Use concise, readable commit messages that state the purpose of the change.
- Pull request descriptions should include context, solution, verification, and possible compatibility impact.
- Keep history reviewable and avoid modifying unrelated files while addressing review feedback.

## License

提交贡献即表示你同意所提交内容按照本仓库的 [MIT License](LICENSE) 发布。

By contributing, you agree that your contribution is licensed under this repository's [MIT License](LICENSE).
