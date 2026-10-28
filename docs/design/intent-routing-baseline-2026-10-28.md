# Intent routing baseline and measurement

Baseline revision: `e26d8b1`. Languages in this refactor: Chinese and English.

Before strategy changes, the review ran 21 `static_scope_` tests and 36 `intent::tests` successfully with pinned Rust 1.96. These establish capability/legacy routing behavior, not real-model tool-selection accuracy. No live model accuracy baseline has been measured; no improvement percentage is claimed.

Runtime emits `blockcell.routing` inventory and selection_outcome events, correlated by session and turn ID. Inventory records resolved scope count (before toggle filtering; gate may narrow it), searchable catalog count, exposed schema count and serialized schema bytes (not tokens). Selection records all requested tools, scope membership, actual catalog membership and schema exposure. Ordinary tool outcomes and skill outcomes are recorded separately; skipped calls have selections but no execution outcome. Per-message summaries separate scope denials, other rejections, execution failures and execution successes. Tool success is not task correctness; external task validators must judge correctness and distinguish expected denials.

Compare frozen model/provider/config/tool-directory snapshots and isolated task fixtures. Record missing usage rather than estimating cache hits. Use paired task runs and repetitions; task-run count and model-call count differ. The no-keyword-hint strategy is the default experimental control. Bilingual fixtures and a recorder/scorer are under `tests/intent_eval` once item 9 lands.
