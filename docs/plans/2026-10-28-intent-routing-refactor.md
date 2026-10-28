# Chinese/English intent routing refactor

> Implementation workflow: test-driven-development. Work only on main; commit every completed item separately.

**Goal:** Make Chinese/English task execution independent of keyword classification, with explicit capability grants, bounded discovery and measurable routing outcomes.

**Architecture:** Preserve legacy gate and omitted configuration compatibility. Registration supplies implementations; explicit host/skill grants and profile packs supply capabilities; policy and deny always win. Default rank uses the main model and stable directories, retaining task state in ephemeral user context. No embeddings or Japanese-specific support.

**Tech Stack:** Rust/Tokio workspace, existing fake-provider runtime tests, offline Python evaluation tooling.

## Approved design and tradeoffs

- Adopt the discussed nine improvements, including the subsequent corrections about authorization, caching and measurement. The user requested implementation and one git commit per item.
- Prefer stable main-model tool selection over an extra classifier request or more keyword tables. Optional lexical hints are per-turn ephemeral and disabled by default.
- Prefer lexical Chinese/English discovery over bundled embedding models. Exact identifiers, configured aliases and token/character overlap use stable tie-breaking.
- `basePacks` omitted preserves legacy foundation; explicit empty removes implicit foundation. Registration alone does not grant every built-in. Skill requirements never self-authorize via `learned` metadata.
- Live model quality claims require real runs; deterministic fixtures and runners establish reproducibility but must not be presented as model-quality evidence.

## 1. Baseline and layered observability

Files: agent session_metrics, runtime/process_message_inner.rs, routing evaluation fixtures.
Add failed tests for stage accounting, then record capability/catalog/schema counts and selected/scope-rejected/succeeded/failed outcomes without changing routing. Log structured summaries. Record current deterministic baseline; real model credentials are not presumed available.
Verify: filtered agent metrics/runtime tests. Commit: feat(agent): observe tool routing stages.

## 2. Explicit host authorization contract

Files: tools/registry.rs, agent/runtime.rs and runtime tests.
Add tests showing ordinary registration does not authorize, explicit host grants do, scoped grants expire with views, deny wins. Add typed capability origin and explicit host registration API, preserving existing register semantics and scoped/provider grants.
Verify: tools registry and agent static_scope tests. Commit: feat(tools): distinguish registration from capability grants.

## 3. Profile base packs

Files: core/config/intent.rs, agent/intent.rs, agent/runtime.rs, Chinese/English profile docs.
Add failing compatibility and explicit-empty profile tests. Move implicit foundation into named packs; omitted field preserves compatibility, [] supplies no foundation. Include global core in pack selection. Retain full-access exec compatibility and sandbox protection.
Verify: core config tests and agent static_scope tests. Commit: feat(config): add explicit profile base packs.

## 4. Host-approved skill grants

Files: core/config/intent.rs, agent/runtime.rs and tests, profile docs.
Add tests that configured skill tool grants join the static capability ceiling but skill metadata, learned=false and activation alone cannot expand it. Use profile skill-grant configuration, filtered by availability/deny/policy. Ensure skill execution intersects granted capabilities.
Verify: skill and static_scope regressions. Commit: feat(agent): support host-approved skill tool grants.

## 5. Isolate keyword hints

Files: core/config/intent.rs, agent/runtime/turn_flow.rs, runtime/process_message_inner.rs, prompt_registry.rs, runtime tests.
Failing tests: rank defaults to General regardless of Chinese/English keywords; forced modes and gate remain; optional hints do not change system prompt/schema. Use optional ephemeral hints disabled by default, and expose skill cards in Coding as well. Preserve operation disciplines in common prompts.
Verify: interaction mode and captured-provider prompt tests. Commit: refactor(agent): isolate lexical routing from model context.

## 6. Multi-turn task state

Files: new agent task_context module, runtime and tests.
Failing tests for Chinese/English continuations, new goals replacing old state, bounded tool lists, no approvals implied. Persist compact versioned goal/progress/recent-tool state in session metadata and append only bounded context to current user turn. State must never authorize tools or actions.
Verify: task state and fake-provider follow-up tests. Commit: feat(agent): persist bounded task context across turns.

## 7. Descriptions and directory budget

Files: tool presentation/registry and agent skill directory rendering.
Improve search/activation descriptions and distinguish ambiguous tools using observed examples. Add optional stable directory budget preserving core tools and moving overflow to discovery, not removing capabilities. Provide truncation/cost observations and deterministic ordering. Default preserves compatibility.
Verify: registry presentation and runtime captured-schema tests. Commit: feat(agent): budget stable tool exposure.

## 8. Chinese/English discovery and bounded recovery

Files: tools/tool_search.rs and targeted tools tests; runtime rejection recovery as needed.
Write failing tests for multiword English, Chinese, exact identifiers, empty query, catalog-order independence and bounded fallback. Implement deterministic lexical ranking/aliases, capped results/summary, explicit empty-result recovery. Do not broaden catalog authorization. Distinguish permanent denial from schema/transient retry.
Verify: tools tool_search and agent discovery regressions. Commit: feat(tools): add bounded bilingual lexical discovery.

## 9. Bilingual/multi-turn evaluation harness

Files: tests/intent_eval (new), documentation.
Create Chinese/English paired cases for six scenario families, explicit allowed alternatives, multi-turn transitions and denied capabilities. Add offline validation/scoring and optional command-based real model runner with frozen metadata, repeated samples and budgets. Separate task runs from model calls; report no fabricated live baseline.
Verify: Python unittest and Rust routing regressions. Commit: test(agent): add bilingual routing evaluation suite.

## Final verification

Run formatting, affected Rust library/integration suites, Python evaluation tests, inspect git diff/status and commit history. Use pinned rustc 1.96 explicitly because the shell cargo otherwise selects Homebrew rustc 1.92. Report actual checks and any unrun live evaluation.


## Implementation status

All nine items have separate implementation commits on `main`:

| Item | Commit |
| --- | --- |
| 1. Layered observations | `901a5a5` |
| 2. Typed host grants | `8531fa3` |
| 3. Explicit base packs | `854555d` |
| 4. Host-approved skill grants | `786294f` |
| 5. Isolated lexical hints | `e02addb` |
| 6. Bounded task state | `5bee8af` |
| 7. Stable schema byte budget | `7651537` |
| 8. Bounded Chinese/English discovery | `93b0563` |
| 9. Paired evaluation harness (landed before behavior changes) | `b25c2db` |

Evaluation trace validation and forbidden-selection attribution were tightened in `a69e302`. Final review also adds media-preserving ephemeral context and preserves runtime unknown/not-started outcome semantics. Live model comparison remains unrun; the offline suite establishes reproducibility, not model accuracy.


Final validation completed with Rust 1.96: agent 913/913, tools 560/560, core 122/122 library tests; Python evaluation 23/23; 120 cases/60 bilingual pairs validated; `cargo check -p blockcell --bin blockcell --offline` passed. Changed Rust files pass rustfmt and `git diff --check`. Agent tests use `TMPDIR=/tmp` on macOS because the default `/var/folders` temp root is intentionally denied by the path policy. Regenerable incremental build cache was cleared after a disk-space failure; subsequent verification completed successfully.
