# Rhai nested path admission implementation plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make Rhai nested tool calls honor the host's path policy and current session approvals, and authorize the implicit decompression destination.

**Architecture:** A task-local admission callback in `tools::tool_guard` carries the host's resource checks into `exec_skill_script`; the tool captures it before crossing `spawn_blocking`. The host reuses its path extraction table, path resolution, hard-deny precedence and session authorization. Standalone tool calls retain allowed workspace/skill resource access under a safe fallback and reject external paths that require confirmation.

**Tech Stack:** Rust 1.96, Tokio task-local scopes, existing PathPolicy and ToolPolicy.

Approved by the coordinating agent on 2026-11-12. Work stays on main. The coordinating agent commits the complete repair batch.

### Task 1: Regression tests

- Modify `crates/tools/src/exec_skill_script.rs` tests: allowed workspace path, explicit deny inside workspace, unapproved external path, explicit external allow, batch-write denial and scoped host authorization.
- Run the new denied/unapproved cases and observe successful unauthorized reads/writes causing assertion failures before implementation.
- Run the existing `decompress_default_destination_requires_write_access` test in `crates/agent/src/runtime/path_security.rs` and observe the missing write access.

### Task 2: Carry host admission into Rhai

- Modify `crates/tools/src/tool_guard.rs`: add a scoped synchronous admission callback; run it after existing tool-policy/danger checks and audit rejections. Provide a conservative standalone fallback for file operations that uses the configured policy-file location and resolves paths consistently.
- Modify `crates/tools/src/exec_skill_script.rs`: capture the callback on the async task before `spawn_blocking`, then use it in the nested guard.
- Modify `crates/agent/src/runtime/tool_exec.rs`: scope registry execution with the host callback.
- Modify `crates/agent/src/runtime/path_security.rs`: create the callback from current policy, workspace, skill context and session authorizations; reuse the extraction table and add the implicit decompression write destination.
- No callback may turn a hard deny into an allow. Confirm without an existing grant fails closed. Authorization remains scoped to the current session.

### Task 3: Verify

Run targeted Rust tests with explicit `RUSTC`/`RUSTDOC` paths for 1.96 and Cargo `--offline`, then the affected modules. Verify both safe usage and rejected attempts, and inspect the diff for unrelated edits. Report test results and any limitations to the coordinating agent without committing.
