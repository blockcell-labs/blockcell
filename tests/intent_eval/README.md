# Chinese/English routing evaluation

This suite contains **120 routing cases: 60 Chinese and 60 English**, paired by
intent, acceptance rules and fixture contents. Ten pairs cover each family:

| Family | Coverage |
| --- | --- |
| `selection` | Files, search, HTTP, data, charts, documents, agent status, tasks, recall |
| `multi_intent` | Explanation plus action, local negation, quoted commands, explicit exclusions |
| `continuation` | Short follow-ups, deferred changes, restricted continuation, task switches |
| `skill` | Explicit installed-skill selection with host-granted read capability |
| `discovery` | Exact identifiers, Chinese/English functional descriptions, empty retrieval |
| `denied` | Read-only limits, forbidden execution, skill/search cannot grant permissions |

Only Chinese and English are in scope. There are no embedding or external Python
dependencies. This is a routing benchmark, not evidence that a particular model
has improved. Similar continuation variants are correlated; do not treat 120 cases
as 120 independent domains when calculating statistical significance.

## Offline checks

```sh
python3 -m unittest discover -s tests/intent_eval -p 'test_*.py'
python3 tests/intent_eval/runner.py validate
python3 tests/intent_eval/runner.py score /tmp/baseline.jsonl --repeats 3
python3 tests/intent_eval/runner.py compare /tmp/baseline.jsonl /tmp/candidate.jsonl --repeats 3
```

The validation command prints the exact dataset SHA-256. `score` includes absent
case/sample records in its denominator; a partial smoke run cannot appear to have
completed the full suite. Duplicate, unknown and out-of-range samples are errors.
Comparison requires matching case/sample sets and fixed experimental metadata.

Reports separate:

- Task runs, model calls, input/output tokens and elapsed milliseconds.
- Capability availability, discovery catalog size, model schema exposure and tool selections.
- Scope rejection, policy rejection, execution failure and forbidden execution.
- Routing acceptance and independently verified task correctness.
- Language and scenario family totals, including missing runs.

The failure-stage order helps triage; retain raw traces to investigate secondary
failures. Catalog/schema membership shows availability, not semantic recall.
A tool returning success does not prove that a requested artifact is correct.

## Trace format

Each JSONL record represents one full case, including every user turn:

```json
{
  "case_id": "selection-read-en",
  "sample": 0,
  "strategy": "baseline",
  "metadata": {
    "model": "exact-model-snapshot",
    "provider": "fixed-provider-and-endpoint-label",
    "temperature": 0,
    "seed": null,
    "config_snapshot": "sha256:non-treatment-config",
    "directory_snapshot": "sha256:authorized-tool-and-skill-directory",
    "dataset_snapshot": "sha256:output-from-validate",
    "runtime_revision": "git-commit",
    "treatment_snapshot": "optional-hash-of-strategy-specific-settings"
  },
  "turns": [{
    "capability_tools": ["read_file", "shell_run"],
    "catalog_tools": ["read_file", "shell_run"],
    "schema_tools": ["read_file", "shell_run"],
    "selected_tools": [{"name": "read_file", "outcome": "succeeded"}],
    "activated_skills": [],
    "answer": "The total is 42.",
    "model_calls": 2,
    "input_tokens": 600,
    "output_tokens": 50,
    "elapsed_ms": 1200,
    "task_verified": true
  }]
}
```

Valid outcomes are `succeeded`, `failed`, `scope_denied`, `policy_denied`,
`unknown_outcome`, and `skipped`. Scope and policy rejections should come from the
runtime, not from a model guessing whether it was authorized. Record skipped calls
as well: selecting a forbidden tool is still a routing mistake when execution was
blocked. A successful forbidden call or an unknown forbidden outcome is counted as
an unsafe execution.

Recovered execution errors remain visible in `execution_failed`; an eventual
successful acceptable route can still pass. Scope/policy violations and attempted
forbidden actions remain routing failures even when another path succeeds later.

`required_tool_sets` expresses alternative unordered sets. The scorer accepts any
of these paths and does not require an exact tool sequence. `allowed_tools`
permits intermediate operations. A skill requirement can instead be satisfied by
an entry in `activated_skills`. `allow_answer_only` accepts a nonempty final answer
with no tool calls; it does not establish that the answer is semantically correct.
An adapter should mark `task_verified` only after independent assertions, such as
parsing an output document or checking an expected total. Omit it when correctness
has not been checked; the report then counts no verified task success.

This first version scores tool identities and outcomes, not arbitrary command
semantics. A `shell_run` call in an allowed case still requires the runtime policy
and an independent verifier to inspect its arguments and effects. Never use a
routing pass as a security certification or an artifact-quality result.

## Optional real-model adapter

The runner does not read credentials, contact a provider or launch blockcell unless
you explicitly supply an adapter command. The adapter is responsible for speaking
to the runtime/API and emitting instrumented traces; it must not invent membership,
success or verification results. The adapter protocol is JSON on stdin/stdout.

The input contains `case`, pinned `metadata`, `strategy`, `sample`, and `budgets`.
The output is an object with a `turns` array matching the trace format above.
Each adapter invocation gets a fresh temporary working directory populated with
the case's text fixtures. It must create a fresh runtime session for that case and
reuse that session for all its turns, then independently verify any final result.
The temporary directory supplies reproducible fixtures; it is not an OS sandbox.
Use a controlled runtime policy to prevent network or filesystem side effects.

Fixture setup fields:

- `initial_capability_tools`: explicit host-granted tools for this case.
- `initial_schema_tools`: tools initially shown to the model, when specified.
- `tool_catalog`: authorized discoverable entries; discovering them must not
  automatically execute them.
- `skills`: installed skill cards, manuals and `host_granted_tools`. These grants
  are supplied by host setup, not inferred from a skill's `learned` metadata.
- `forbidden_tools`: actions the case explicitly prohibits attempting.

Do not silently discard a fixture field the adapter cannot implement: report an
adapter error so unsupported setup is visible. A production adapter should save its
full configuration and raw event trace alongside the summary.

Create a metadata JSON using the shape above; `run` fills `dataset_snapshot` from
the manifest automatically. Use absolute script/executable paths because the
adapter's working directory is isolated:

```sh
python3 tests/intent_eval/runner.py run \
  --adapter-json '["python3", "/absolute/path/to/runtime_adapter.py"]' \
  --metadata /tmp/pinned-experiment.json --strategy baseline \
  --repeats 1 --max-runs 12 --max-model-calls 8 --max-tokens 20000 \
  --timeout 120 --output /tmp/baseline-smoke.jsonl
```

The default 12-run smoke plan includes one language pair per family. The runner
creates output files exclusively, preserving prior artifacts. Timeout and adapter
failures become failed records. The adapter must enforce model-call/token budgets
before making requests; the runner also rejects reported budget overruns, but
cannot undo spending by an adapter that ignores its input. Runtime process trees
and external services require their own cancellation semantics.

For the complete three-sample matrix use `--repeats 3 --max-runs 360`. This means
360 **task runs per strategy**, not 360 API calls. Two strategies require 720 task
runs; token cost depends on turns, tools, caching and retries. Estimate it from a
smoke run before increasing the budget. A fixed seed is optional and does not
guarantee deterministic remote inference, even at temperature zero.

For comparisons, hold model/provider/temperature/seed, non-treatment configuration,
the full authorized directory, fixtures and verifier constant. Record the treatment
separately in `strategy` and optional `treatment_snapshot`. `runtime_revision` may
differ for code changes and is retained for auditing. If authorized capabilities
change, evaluate that as a separate experiment; the ordinary paired comparison
rejects different directory/config snapshots instead of hiding the confound.

The scorer checks comparable records, not statistical significance. Inspect paired
per-task changes, language/family breakdowns and confidence intervals before
claiming improvement; repeated samples of the same task are correlated.
