# Article 09: Self-Evolution — How AI Automatically Writes Code to Upgrade Itself

> Series: *In-Depth Analysis of the Open Source Project “blockcell”* — 9/14

---

## A bold idea

Most software is static: you install a version and it stays that way until you manually update.

blockcell pursues a bold goal: **let the AI discover problems, write code to fix them, test the changes, and roll them out by itself.**

This is the Self-Evolution system.

---

## What triggers evolution?

Evolution doesn’t happen randomly — it’s triggered by **error patterns**.

```
ErrorTracker:
- monitors execution results for each skill
- counts errors within a time window
- triggers evolution once the error count exceeds a threshold
```

Example:

```
The stock_monitor skill failed 3 times in the last hour
→ ErrorTracker decides: evolve
→ create an evolution record with status: Triggered
```

---

## The evolution pipeline

The pipeline has six stages:

```
Triggered → Generating → Auditing → Compiling → Testing → RollingOut
                                                             ↓ on failure
                                                          Rolled Back
```

### Stage 1: Generating

The system sends a special prompt to the LLM:

```
You are a Rhai script expert.
The stock_monitor skill has the following errors:
  - Error 1: no handling when finance_api returns empty
  - Error 2: no retry logic on network timeout

Please generate a fixed version of SKILL.rhai.
Original code: [original SKILL.rhai]
Error history: [details of last 3 failures]
```

The LLM generates new code and it is saved as a patch.

### Stage 2: Auditing

The system performs static auditing on generated code:
- checks for dangerous operations (e.g., deleting files, exfiltrating data)
- checks code structure and reasonableness
- checks that known error scenarios are handled

If audit fails, it enters a retry loop (up to 3 attempts):

```
Audit fails → convert reason into feedback → regenerate with LLM → audit again
```

### Stage 3: Compiling

Rhai scripts are pre-compiled to catch syntax errors:

```rust
let engine = Engine::new();
engine.compile(&new_code)?;  // syntax errors caught here
```

Compile failures also trigger retries, passing the compiler error back as feedback.

### Stage 4: Testing

Dry-run tests use fixtures under the skill’s `tests/` directory:

```
skills/stock_monitor/tests/
├── test_basic_quote.json
├── test_error_handling.json
└── test_network_timeout.json
```

Each fixture contains input and expected output. The new code must pass all tests.

### Stage 5: RollingOut (canary)

After tests pass, the system does not go straight to 100%. It rolls out gradually:

```
Stage 1: 10% of requests use the new version
Wait 10 minutes and observe error rate...

Stage 2: 50% of requests use the new version
Wait 10 minutes and observe error rate...

Stage 3: 100% of requests use the new version
```

During canary rollout, the system monitors the new version’s error rate. If it is worse than the old version, it rolls back immediately.

---

## Retry-with-feedback mechanism

This is a key design choice: **failures are not the end — they become feedback.**

Each failure is recorded as a `FeedbackEntry`:

```rust
struct FeedbackEntry {
    attempt: u32,           // attempt number
    stage: String,          // which stage failed
    feedback: String,       // failure reason
    previous_code: String,  // previous code
    timestamp: i64,
}
```

On the next attempt, the LLM sees the full failure history:

```
This is attempt #2.
Attempt #1 failed:
  Stage: compile
  Error: line 15: variable 'price' is not defined
Please fix the issue above and generate version #2.
```

This helps the LLM learn from errors and iteratively improve code quality.

---

## Versioning

Each evolution creates a new version:

```
~/.blockcell/workspace/
├── skills/
│   └── stock_monitor/
│       └── SKILL.rhai           # current version
└── tool_versions/
    └── stock_monitor/
        ├── v1_2025-02-01.rhai
        ├── v2_2025-02-10.rhai
        └── v3_2025-02-18.rhai
```

If a new version causes issues, you can manually roll back:

```bash
blockcell evolve rollback stock_monitor
```

---

## Viewing evolution records

```bash
blockcell evolve list

# Example output:
# SKILL           STATUS      ATTEMPT  CREATED
# stock_monitor   RolledOut   1        2025-02-10
# bond_monitor    Generating  2        2025-02-18
```

You can also ask in chat:

```
You: Which skills are evolving right now?
AI: Currently evolving:
    - bond_monitor: attempt #2, stage: Auditing
```

---

## Safety boundaries

Self-evolution requires strict safety boundaries:

1. **Only Rhai scripts can be modified** — Rust core code is not touched
2. **Audit filtering** — generated code must pass security audits
3. **Test validation** — must pass existing test fixtures
4. **Canary rollout** — observation period, no immediate full replacement
5. **Auto rollback** — roll back if performance degrades
6. **Version retention** — all historical versions are preserved

---

## Survival invariants

The system periodically checks its “survival capabilities”:

```rust
struct SurvivalInvariants {
    can_compile: bool,            // can it compile code?
    can_load_capabilities: bool,  // can it load new capabilities?
    can_communicate: bool,        // can it access the network?
    can_evolve: bool,             // can it self-evolve?
}
```

If any invariant becomes false, the system records warnings and attempts to recover, ensuring the core capabilities won’t be permanently damaged.

---

## Practical impact

Self-evolution is especially effective for:

- **API changes**: data source formats change → skills adapt automatically
- **Edge cases**: unusual input causes crashes → add validations automatically
- **Performance optimization**: add caching, reduce unnecessary API calls

---

## Summary

blockcell’s self-evolution system is a complete **AI-driven continuous improvement pipeline**:

```
Error triggers → LLM generates code → audit → compile → test → canary rollout → full rollout
              ↑ failure feedback ←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←
```

This makes blockcell more reliable over time — truly “gets smarter as you use it.”

---

*Previous: [Gateway mode — turning AI into a service](./08_gateway_mode.md)*
*Next: [Finance in practice — monitoring stocks and crypto with blockcell](./10_finance_use_case.md)*

*Repo: https://github.com/blockcell-labs/blockcell*
*Website: https://blockcell.dev*
