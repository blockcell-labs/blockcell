# Review Remediation Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix all 31 confirmed findings and conditional C-01, committing each completed batch on main.

**Architecture:** Follow the approved R1–R6 design and per-finding acceptance in docs/reviews/2026-11-11-project-review. Share resource authorization and canonical origin identity; preserve existing files on failed operations; make lifecycle and configuration contracts explicit.

**Tech Stack:** Rust 1.96, Tokio, TypeScript, Python, Bash.

## Execution

For every item: add behavior regression; run it and record the expected failure; implement; run focused and affected-suite checks; inspect diff. Commit only the current batch and its tests/status. Existing untracked tests/eval_kit and tests/realworld48 remain outside staging.

1. R1: forked/can_use_tool.rs and memory entry schemas deny shell in memory-only forks while retaining constrained file tools; runtime/path_security.rs registers implicit decompress Write; tools/exec_skill_script.rs shares nested authorization; tools/file_ops.rs securely creates archive outputs; tools/bounded_io.rs and http_request.rs commit only completed downloads. Run agent and tools library regressions with explicit Rust 1.96.
2. R2: session storage/core session keys, task/checkpoint authorization, gateway and webui identity, slash reply account, streams owner and cron delivery account. Use full-origin and ambiguous-ID regressions; preserve historical data.
3. R3: release.yml and install.sh; run complete release manifest/installer tests with local fixtures, including checksum and extraction failure after asset lookup succeeds.
4. R4: core config/budget and provider usage/model settings; run actual save/reload, masking reorder and streaming budget regressions.
5. R5: gateway confirmation lifecycle, scheduler in-flight/shutdown and receipts, provider UTF-8 buffering, channel media; use controlled timing and fake transports.
6. R6: skill termination/discovery/manage identity, memory missing-ID and coding evaluator timeout; run focused and affected suites.

The review README contains exact issue mapping and acceptance details. Track batch commit and remaining limits in remediation-progress.md. No new branches/worktrees; user explicitly authorized main and sequential batch commits.
