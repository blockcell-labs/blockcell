# Original 13-task evaluation kit

Maintained source from `tmp/blockcell_eval_kit.tgz`. The archive remains unchanged.

Set `BLOCKCELL_EVAL_WORKSPACE`, `BLOCKCELL_EVAL_RUN_DIR`, `BLOCKCELL_EVAL_BIN`, and `BLOCKCELL_EVAL_STATE` as needed. Configure the agent provider/workspace separately; the runner does not overwrite user configuration. Use a dedicated agent state/workspace for evaluation.

Generate fixtures with `python3 tests/eval_kit/make_fixtures.py`, then run `python3 tests/eval_kit/run.py`. These commands are documentation only; they were not run during this fix.

The t01 verifier targets only test_stats.py and rejects changes to that original test. The t09b verifier compares author exactly, including null. Results distinguish proposed, executed and completed calls, including pre-execution rejections.
