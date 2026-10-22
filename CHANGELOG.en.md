# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.8] - 2026-10-22

This release runs on two tracks: the **Agent Runtime architecture evolution** (absorbing the composable / observable / interceptable / replayable design of a next-generation Agent Runtime while preserving Self-Evolution and Ghost Learning), and a **repo-wide security and correctness close-out** (multiple review passes across every crate, converging tool admission, sandbox boundaries, and path/credential handling onto shared primitives). The goal is to move from "it runs" to "explainable, replayable, auditable".

### Added

- **Runtime Kernel**: added an event bus (`EventBus`), a serial interception chain (`Waterfall`), and unified scope primitives (`ScopedRegistry` / `ScopeGuard`). Events are dispatched by priority and scope filtering, and listeners support timeout and failure isolation (fail-open / fail-closed).
- **Agent lifecycle events**: added `agent/pre-step`, `agent/request`, `agent/request-error`, `turn/start`, `turn/end`, `step/start`, `step/end`, `tool/pre-execute`, `tool/post-execute`, `tool/result`, `model/response`, `assistant/attempt`, `memory/write`, `subagent/spawn`, `subagent/completed`, `evolution/deployed`, `compaction/start`, and `compaction/end`, classified by Durable / Live / Capability consistency semantics.
- **Guarded Tool Pipeline**: centralized tool toggles, policy, dangerous-operation, path-access, and shell-escalation checks into one pipeline; individual tools no longer handle permissions, approvals, or auditing themselves.
- **Failure semantics**: added `FailureKind`, `Idempotency`, and `ToolOutcome` to distinguish failed / rejected / cancelled / timed-out / not-started / unknown; unknown outcomes are reconciled before retry instead of being retried automatically. Model request errors now pick a recovery strategy by type (compact-then-retry on context overflow, backoff on rate limit, provider switch on auth failure).
- **Session Event Log**: added an append-only session event log (JSONL sidecar, `schema_version`, monotonic `seq`, retention cleanup), with the transcript kept as the primary store and the event log running alongside it incrementally.
- **Session replay and evaluation**: added `blockcell session list / replay / verify` to replay turns, steps, model requests, and tool calls from the event log; added `SessionMetrics` for turn / step / tool-outcome / retry / token / duration metrics.
- **PromptRegistry**: the system prompt is now assembled from pluggable `PromptSection` registrations with scope override, template-variable resolution, and integrity checks; added `PromptContribution` and trust levels so learned skills and candidate behavior are injected behind a low-trust boundary.
- **Agent Loop contract**: separated the `AgentLoop` contract from the default loop implementation so the way a turn is driven can be replaced.
- **Capability seams**: added `SandboxPolicy` / `SandboxProvider` (native macOS Seatbelt / Linux Landlock+seccomp, falling back to approval when unavailable), `ShellExecutor`, `FilesystemBackend` (`LocalFilesystem`), `BrowserBackend` (`CdpBrowserBackend`), and `SubagentProvider` (a unified contract for fork and typed agents, with handles exposing status / cancel / result).
- **Three-layer tool definition**: `ToolDefinition` = `ToolSchema` (model-facing) + `ToolExecutionSpec` (permissions and idempotency) + `ToolPresentation` (presentation), decoupling presentation from execution permissions.
- **Compaction abstraction**: compaction is abstracted into a `ContextCompactor` strategy (`ModelSummaryCompactor` / `ExtractiveCompactor`).
- **`agent/pre-step` decision gate**: consolidates budget, compaction, and context injection, and supports a registrable policy chain that can rewrite or stop the step.
- **Evolution isolation**: added the Evolution Candidate Scope and an evolution trust boundary; candidate skills / prompts / policies are visible only inside a view that explicitly enters candidate evaluation.
- **Auditing and execution environment**: tool-call audit records now carry `call_id`, the execution outcome, and the turn `trace_id`, and record the tool's declared execution environment (`ExecutionEnvironment`), aligned with the `tool/call` event name; rejected, over-limit, and not-started calls are audited too.
- **Skill version observability**: `skill/load` events record the skill's current version, `SessionMetrics` breaks down turn success by `skill@version`, and `blockcell session replay` shows skill versions and per-turn statistics.
- **Subagent budget attribution**: LLM spend from subagents is folded into the originating session's budget, so background agents can no longer bypass global cost control.

### Changed

- **Toolchain pinned to Rust 1.96.0**: added `rust-toolchain.toml` and raised `rust-version` from 1.85 to 1.96; the Docker builder image and release workflow now use 1.96.0 as well (previously a drifting `rust:1` and 1.92.0 respectively). The old 1.85 floor was not buildable — `home` / `ignore` / `image` already require 1.88.
- Hooks and WebSocket tool events now feed into the EventBus, keeping the existing `hooks.yaml` configuration compatible.
- The ToolRegistry is now backed by the unified scope, so tools can be registered per agent / session / evolution candidate, with the more specific scope overriding on name conflicts.
- Ghost turn review now consumes the Session Event Log instead of inferring tool outcomes, memory writes, and model errors from the transcript.
- Session event-log writes moved off the async runtime (`spawn_blocking`), and listeners now default to a 30s timeout.
- Lock files are now long-lived synchronization anchors, avoiding the mutex being broken by `unlink`.
- `create_docx` now generates DOCX natively via `docx-rs` instead of relying on python-docx.
- **Execution environment decoupled from admission**: `exec` / `exec_local` / `exec_skill_script` now honestly declare `FullAccess` (running without a sandbox but without per-call confirmation), while dangerous commands remain gated by `DangerousOperation` on content; only `shell` with `sandbox_policy=full-access` asks for confirmation each time.
- **Per-message runtime derivation**: a lightweight per-message executor is derived from the long-lived runtime, sharing the response cache, evolution workers, learning coordinator, authorized directories, and budget trackers while isolating abort / steering / turn state, eliminating per-message disk rebuilds.
- **Layer 4 MemorySystem is now session-scoped and shared**, keeping cross-turn tracking state.
- **Policy engine precedence is now Deny > Ask > Allow** (declaration order within a level) instead of first-match-wins.
- **Forked tool permissions default to fail-closed**: when `can_use_tool` is not supplied explicitly, the allowlist is derived from the fork's declared `tool_schemas`.
- **Blocking primitives wrapped uniformly**: synchronous SQLite and cross-process locks no longer run directly inside `async fn` (`run_store_blocking` / `run_blocking`); tools that open their own SQLite now set `WAL` and `busy_timeout`; cron's `operation_lock` only guards short critical sections.
- **External input handling converged on shared primitives**: Python string escaping, filename sanitization, `~` path expansion, channel path/filename sanitization, response-body size limits, SSRF validation, and config secret masking/restoration are no longer reimplemented per module.

### Performance

- The main loop prefetches read-only tool calls concurrently (`read_file` / `list_dir` / `glob` / `grep` / `rg` / `code_search` / `session_search` / `web_fetch` / `web_search`) and skips tool-body execution on a hit, overlapping independent I/O latency.
- The MemorySystem pool now evicts by LRU and is shared per session, so long-lived processes no longer grow linearly.
- Vector search reuses the id map instead of decoding the whole table on every query.
- Hot-path deduplication: BFS statement reuse and merged brief queries; `reload_skills` gained a fingerprint gate so unchanged skills are not rescanned.
- Prompt-cache alignment: `prompt_cache_key` now covers the tools and model fingerprints, the fourth Anthropic cache breakpoint is aligned, and a `fork_alignment_misses` metric was added.

### Security

- Fixed a Python script injection through unescaped style fields in `chart_generate`; nested tool calls inside skill scripts no longer bypass admission checks (nested contexts cannot prompt, so `ask` is treated as a rejection).
- Fixed `path_glob` path traversal, `exec_local` / `exec_skill_script` path escapes, authorized-root escapes, and missing admin permissions.
- Fixed an updater signature-verification bypass and SSRF-defense bypasses in `network_monitor` / `video_process`; all outbound HTTP now goes through `safe_client_builder()` + `ensure_url_allowed()` + a redirect policy.
- Fixed plaintext credential echo in the gateway and CLI (unified masking plus write-side restoration), raw-key encryption leaking into argv, overly permissive `.env` permissions with empty templates overwriting real environment variables, and the `encrypt` tool's IV persistence and algorithm declaration.
- Tightened sandbox and command-execution boundaries: fixed Seatbelt sensitive-read `deny` being overridden by a global `allow`, cross-channel interference between persistent shell sessions, missed `full-access` declarations when reusing a session, and double confirmation with wrong attribution for dangerous commands.
- Added resource limits for IM inbound media downloads, OOXML decompression, CSV parsing, Whisper uploads, tool response bodies, and non-UTF-8 safety scanning.

### Fixed

- Concurrency and consistency: lost updates on concurrent edits, transaction upgrades, `fs2` file locks, cross-process exclusion for the audit hash chain, mutual exclusion for evolution triggers, cross-process merge-and-save for the capability registry, a shared cross-process lock file between `toggles` and the gateway, and confirmation-queue send timeouts plus replay after capacity exhaustion.
- Memory and knowledge base: `knowledge_forget` now writes a tombstone per item and rolls back only that tombstone on delete failure, per-database forgets are serialized by a file lock, `memory restore` replaces entries with the same `dedup_key`, memory body length is capped, FTS is version-gated, and dedup migration runs once.
- Scheduling: cron period overflow, 7-field expressions, DST-gap computation, and long tasks moved out of `operation_lock`.
- Skills: fingerprints now include a streaming content hash (catching same-length, same-mtime rewrites), directory symlinks are not recursed into, `activate_skill` snapshots are paired, and capability history collisions are fixed.
- Storage and backups: old backups are evicted only after a successful copy, partial artifacts are cleaned up on failure, and the audit lock gained a timeout.
- Configuration: JSON5 environment-variable injection and comment ordering are preserved, unknown keys are kept, `config set` no longer silently drops keys, and `alerts` rule files are written concurrently safely.
- Providers and channels: Gemini default routing 404, `sender_role` passthrough, notification `security.mainSessionKey` can name a trusted target, and typed subagents reuse the parent `memory_file_store`.
- Runtime: malformed tool-argument logs no longer keep the raw prefix; `exec_local` picks an interpreter only before execution, and runtime errors or timeouts no longer trigger a re-run; text approvals match on the tool plus the full JSON arguments; the prefetch budget is tighter; `PromptOnly` covers override directives that omit the previous/system qualifier (still heuristic, not a complete injection defense).
- Disabled selection of the incomplete Linux Landlock backend until read isolation is verified, falling back to explicit approval; no claim of native Linux read isolation.
- Tool safety checks now apply to the final arguments after a listener rewrites them, closing the hole where argument rewriting bypassed policy, dangerous-operation, and path checks.
- Release builds keep panic unwinding, restoring panic isolation for listeners and task supervision.
- Clearing a session now deletes the event log under lock and cleans up the lock file.
- Step numbering is unique within a turn and every step now has a `step/end`; an abnormally exiting turn now emits `turn/end`.
- Tool events are consistently attributed to the persisted session, and forced skill-routing turns re-emit `turn/end`.
- The capability registry now only tolerates skipping the removed `dynamic_library` entries; other deserialization failures still error out.

### Tests and gates

- Added the local gate `make test` (`cargo fmt --all --check` + `cargo test --workspace` + coding-eval + three infrastructure scripts), with `make hooks` installing the `pre-push` hook.
- Fixed a false pass in coding-eval acceptance where zero tests passing counted as success.
- Added protocol-level MCP client tests (real stdio JSON-RPC), cron timezone / DST / 6- and 7-field tests, updater ed25519 signature and version-comparison tests, evolution deployment-chain tests (generate → audit → compile → contract → deploy → observe → rollback), four-path skill hot-reload regression tests, and prompt-cache alignment regressions.
- Sandbox-escalation tests are now deterministically driven instead of depending on host state; migration tests now assert fail-closed behavior.
- `cargo fmt` and `clippy -D warnings` are clean across the repository.

### Docs

- Synced `docs/03`, `09`, `12`, `17`, and `20` with the implementation (including `-v` wiring and pool naming).
- Development-process documents (review reports, fix plans, decomposition drafts) no longer enter version control; only formal design docs are kept.

### Removed

- Removed the native dynamic-library capability evolution path.

## [0.1.7] - 2026-06-28

### Added
- Added ModelRouter intelligent routing and connection-phase fallback with `manual`, `cost_optimized`, `quality_first`, and `latency_first` strategies.
- Added the Tool Policy execution policy system with tool-name globs, `|` multi-patterns, channel/path conditions, `allow` / `ask` / `deny` decisions, inherited rule groups, and simulation mode.
- Added global token and cost budget controls with per-session LLM usage tracking to prevent runaway tasks.
- Added the Steering Channel for injecting user messages into an active agent turn while it is still running.
- Added Agent lifecycle hooks for `session_start`, `user_prompt`, `pre_tool_use`, `post_tool_use`, and `agent_stop`, configurable through `~/.blockcell/hooks.yaml`.
- Added SHA-256 hash-chain verification for audit logs, plus SessionStart, SessionEnd, ProviderCall, and BudgetEvent audit events.
- Added on-demand MCP tool discovery: when many MCP tools are allowed, the model sees `mcp_search_tools` while remote tools stay executable but hidden from the default system prompt.
- Added GitHub Actions CI and multi-platform release build workflow.

### Changed
- Updated 2026-06 provider and default model presets across DeepSeek, OpenAI, Anthropic, Gemini, Ollama, GLM, Qwen, and related model families.
- Switched WebUI pages to `React.lazy` + `Suspense` chunks and improved the system-events button layout.
- Reworked Gateway event broadcasting around lightweight `WsEventRouting`, avoiding route-time copies of large content/token payloads.
- Reduced repeated token estimation, history cloning, and per-line allocation in streaming/read paths.
- Made Ghost config hot reload and Cron sync skip full disk reads and parsing when config mtime is unchanged.
- Moved SQLite memory query/write/delete and maintenance operations into `spawn_blocking` to reduce Tokio worker blocking.
- Continued splitting large modules by responsibility across runtime, agent, gateway, config, openai, memory, evolution, and consolidator code.
- Standardized the workspace on Rust 1.85.

### Fixed
- Fixed Gateway API token exposure through URL `?token=` on regular APIs; query tokens are now limited to WebSocket and required file download/serve paths.
- Fixed WebSocket / outbound broadcast session isolation issues and prevented one WS client from approving another session's pending confirmation request.
- Fixed file read/upload size limits, local image serve boundaries, multibyte UTF-8 URL decoding, CLI Unicode cursor editing, and Home/End/Delete key handling.
- Fixed `http_request` SSRF risk, `exec_local` / `exec_skill_script` path escapes, dangerous `rm` command bypasses, timeout orphan processes, and symlink write escapes.
- Fixed Ghost review TOCTOU/path traversal issues, main-flow blocking, throttle leakage, and accidental scanning of `.snapshots` / `.skill_file_store.lockdir`.
- Fixed concurrency, atomicity, transaction commit, recovery, lock-release, budget, and Unicode panic issues across Dream / Session / Auto / Compact memory paths.
- Fixed skill evolution and core evolution state-machine, rollback, staging-path, cross-workflow contamination, infinite retry, Python syntax check, and code-fence injection issues.
- Fixed OpenAI-compatible streaming tool calls, MCP short-name tool loading, hash algorithm use, browser navigation races, CDP listener leaks, and semver prerelease parsing.
- Fixed Windows atomic replacement, path separators, temporary cleanup, cross-process lock deletion, config loss, and schema consistency issues.

### Docs
- Added ModelRouter routing and fallback documentation.
- Added Hook lifecycle event system documentation.
- Updated README / README.en with v0.1.7 security, audit, routing, and hook capabilities.
- Added v0.1.7 release notes.

## [0.1.6] - 2026-05-09

### Added
- Added the Ghost Native learning loop, capturing reusable lessons at turn end, pre-compress, session rotation, session end, delegation end, and evolution success boundaries, then persisting durable knowledge into `USER.md`, `MEMORY.md`, and workspace skills.
- Added a Ghost learning ledger and background review loop for episode audit, review runs, and restricted tool actions without blocking the main assistant response.
- Added Typed Agent and custom Agent loading from user-level and project-level Markdown definitions, including tool scope, model, skill, MCP, one-shot, permission, and background execution settings.
- Added multi-agent task improvements: checkpoints, chained AbortToken cancellation, progress events, task persistence, and improved result injection.
- Added a unified slash-command system for `/help`, `/tasks`, `/skills`, `/tools`, `/learn`, `/clear`, `/compact`, `/session-metrics`, `/log`, and shared Gateway/Channel handling.
- Added 7-layer memory metrics, circuit breakers, and the `/session-metrics` observability entry point.
- Added 30+ configurable memory and compression thresholds in `config.json5`.
- Added OpenClaw skill parsing, gbrain skill compatibility, and the optional RabitQ vector index backend.

### Changed
- Upgraded the default DeepSeek path toward DeepSeek V4 Pro, including 1M context defaults, `reasoningEffort`, and thinking/reasoning request parameters.
- Improved WebUI streaming for thinking/reasoning content, connection state handling, LLM defaults, and agent progress events.
- Unified skill loading, CLI completion, Gateway skills/search, and SkillIndex around recursive skill pack/category scanning and meta-only skill visibility.
- Unified skill description loading as `meta.yaml`/`meta.json` → `SKILL.md` frontmatter → `SKILL.md` body, fixing YAML multiline descriptions and search ranking gaps.
- Migrated core evolution and skill evolution to durable workflows backed by SQLite workflow records, claim/lease, step checkpoints, and background workers.
- Kept SQLite + FTS5 as the canonical memory store while adding optional RabitQ semantic vector recall.
- Hardened the learning write path with unified security scanning, write guards, snapshots, and undo support.

### Fixed
- Fixed DeepSeek DSML tool-call parsing, cross-delta filtering, and missing `reasoning_content` errors.
- Fixed missing thinking/reasoning display, duplicated CLI output, and missing user/LLM log persistence.
- Fixed deep skill-pack category scanning, composite-name truncation, invisible child skills, and accidental empty-skill registration.
- Fixed core evolution ticks blocking the Agent select loop and delaying user input handling.
- Fixed multi-agent task lifecycle, forked subagent cancellation, result injection, task persistence, and Auto Memory trigger logic.
- Fixed Windows config atomic replacement, Python script execution, colon-containing session IDs, and versioning path separators.
- Fixed memory compaction divide-by-zero panics, UTF-8 boundary panics for Chinese text, config propagation gaps, SQL injection risks, and learning write-safety issues.

### Docs
- Added Ghost Native learning design documentation.
- Added design docs for unified slash commands, session metrics, OpenClaw compatibility, configurable memory thresholds, log commands, intent classifier/loadAllTools, and the integrated learning framework.
- Updated Ghost Maintenance docs to clarify its boundary with embedded Ghost learning.

## [0.1.5] - 2026-04-05

### Added
- Unified skill runtime with `rhai` script execution support.
- Skill versioning, auditing, evolution, and service management improvements.
- New Weixin, QQ, and NapCatQQ channel support.
- Memory vector indexing support.
- Expanded WebUI interaction for chat, evolution, and connection states.
- Additional docs for skills, CLI, provider configuration, MCP servers, and path access policy.

### Changed
- Simplified the skill execution model and history handling.
- Improved message/tool call flow and runtime consistency across core modules.
- Enhanced WeCom long-connection support and channel startup/status display.
- Optimized WebUI chat UX, message rendering, and frontend test coverage.
- Updated cron/timezone handling for scheduled tasks.

### Fixed
- Fixed provider compatibility issues and configuration edge cases.
- Fixed gateway/agent/scheduler stability issues and duplicated output problems.
- Fixed path parsing, default model invocation, and media handling bugs.
- Fixed WebUI lag when the gateway disconnects.
- Fixed memory rule inconsistencies across storage, tools, and gateway.

### Docs
- Added and updated a large set of Chinese and English docs for skills, channels, memory, provider configuration, and CLI usage.
- Added workflow/rules documentation for skill development.

## [0.1.4] - 2026-03-25

### Added
- Initial public release for the current tracked line of BlockCell.
- Core agent, provider, storage, scheduler, channels, and skills workspace crates.
- Basic WebUI and gateway integration.
