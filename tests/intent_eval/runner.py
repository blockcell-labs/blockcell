"""Dependency-free Chinese/English routing evaluation; live adapters are opt-in.

The scorer measures routing, never assumes a successful tool call completed a task.
Only an independent adapter/verifier may set ``task_verified`` to true.
"""

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path, PurePosixPath
import subprocess
import tempfile

FAMILIES = {"selection", "multi_intent", "continuation", "skill", "discovery", "denied"}
OUTCOMES = {"succeeded", "failed", "scope_denied", "policy_denied", "unknown_outcome", "skipped"}
COMPARABLE_FIELDS = ("model", "provider", "temperature", "seed", "config_snapshot", "directory_snapshot", "dataset_snapshot")
REQUIRED_METADATA = set(COMPARABLE_FIELDS) | {"runtime_revision"}


def validate_case(case):
    for key in ("id", "pair_id", "language", "family", "turns"):
        if key not in case:
            raise ValueError(f"case missing {key}")
    if case["language"] not in {"zh", "en"} or case["family"] not in FAMILIES:
        raise ValueError("only Chinese/English and the six documented families are supported")
    if not case["turns"]:
        raise ValueError("case requires at least one turn")
    for name, content in case.get("files", {}).items():
        path = PurePosixPath(name)
        if not name or path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name:
            raise ValueError("fixture paths must be safe relative paths")
        if not isinstance(content, str):
            raise ValueError("fixture contents must be strings")
    for turn in case["turns"]:
        if not isinstance(turn.get("message"), str) or not turn["message"].strip():
            raise ValueError("turn requires a nonempty message")
        rules = turn.get("acceptance", {})
        allowed = set(rules.get("allowed_tools", []))
        if allowed & set(case.get("forbidden_tools", [])):
            raise ValueError("an allowed tool cannot also be forbidden")
        for alternative in rules.get("required_tool_sets", []):
            if not alternative or not set(alternative) <= allowed:
                raise ValueError("required tool alternatives must be nonempty allowed subsets")
        if not rules.get("required_tool_sets") and not rules.get("required_skills_any") and not rules.get("allow_answer_only"):
            raise ValueError("turn requires a tool, skill, or answer-only acceptance path")


def load_manifest(path):
    data = json.loads(Path(path).read_text())
    if data.get("schema_version") != 1:
        raise ValueError("unsupported manifest schema")
    seen, pairs = set(), {}
    for case in data["cases"]:
        validate_case(case)
        if case["id"] in seen:
            raise ValueError("duplicate case id")
        seen.add(case["id"])
        pair = pairs.setdefault(case["pair_id"], {})
        if case["language"] in pair:
            raise ValueError("duplicate pair language")
        pair[case["language"]] = case
    for pair in pairs.values():
        if set(pair) != {"zh", "en"}:
            raise ValueError("every case requires a Chinese/English pair")
        zh, en = pair["zh"], pair["en"]
        if zh["family"] != en["family"] or len(zh["turns"]) != len(en["turns"]):
            raise ValueError("paired cases must use the same family and turn count")
        if [t["acceptance"] for t in zh["turns"]] != [t["acceptance"] for t in en["turns"]]:
            raise ValueError("paired cases must have equivalent acceptance rules")
    data["snapshot"] = "sha256:" + hashlib.sha256(Path(path).read_bytes()).hexdigest()
    return data


def validate_metadata(metadata):
    missing = REQUIRED_METADATA - metadata.keys()
    if missing or any(not metadata.get(k) for k in REQUIRED_METADATA - {"seed", "temperature"}):
        raise ValueError(f"metadata missing pinned fields: {sorted(missing)}")
    if not isinstance(metadata["temperature"], (int, float)):
        raise ValueError("metadata temperature must be numeric")


def _nonnegative(turn, name):
    value = turn.get(name, 0)
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        raise ValueError(f"{name} must be a nonnegative integer")
    return value


def validate_turns(turns):
    """Validate adapter-controlled traces before accounting or scoring them."""
    if not isinstance(turns, list):
        raise ValueError("turns must be an array")
    for turn in turns:
        if not isinstance(turn, dict):
            raise ValueError("each turn must be an object")
        for metric in ("model_calls", "input_tokens", "output_tokens", "elapsed_ms"):
            _nonnegative(turn, metric)
        for field in ("capability_tools", "catalog_tools", "schema_tools", "activated_skills"):
            values = turn.get(field, [])
            if not isinstance(values, list) or any(not isinstance(value, str) or not value for value in values):
                raise ValueError(f"turn {field} must be an array of nonempty names")
        if "answer" in turn and not isinstance(turn["answer"], str):
            raise ValueError("turn answer must be a string")
        calls = turn.get("selected_tools", [])
        if not isinstance(calls, list):
            raise ValueError("turn selected_tools must be an array")
        for call in calls:
            if not isinstance(call, dict) or not isinstance(call.get("name"), str) or not call["name"]:
                raise ValueError("selected_tools entries require a nonempty tool name")
            if call.get("outcome") not in OUTCOMES:
                raise ValueError("selected_tools entries require a valid outcome")


def score_record(case, record):
    result = {"case_id": case["id"], "sample": record.get("sample", 0),
              "language": case["language"], "family": case["family"],
              "routing_passed": True, "task_verified": True, "failure_stage": None,
              "model_calls": 0, "input_tokens": 0, "output_tokens": 0, "elapsed_ms": 0,
              "selected_tools": 0, "scope_denied": 0, "policy_denied": 0,
              "execution_failed": 0, "unsafe_executions": 0,
              "capability_count": 0, "catalog_count": 0, "schema_count": 0}
    turns = record.get("turns", [])
    validate_turns(turns)
    stages = []
    if len(turns) != len(case["turns"]):
        stages.append("missing_record")
        result["task_verified"] = False
    if record.get("adapter_error"):
        stages.append("adapter")
        result["task_verified"] = False
    for expected, actual in zip(case["turns"], turns):
        for metric in ("model_calls", "input_tokens", "output_tokens", "elapsed_ms"):
            result[metric] += _nonnegative(actual, metric)
        for field, metric in (("capability_tools", "capability_count"), ("catalog_tools", "catalog_count"), ("schema_tools", "schema_count")):
            result[metric] += len(set(actual.get(field, [])))
        calls = actual.get("selected_tools", [])
        selected = {call["name"] for call in calls}
        rules = expected["acceptance"]
        allowed = set(rules.get("allowed_tools", []))
        forbidden = set(case.get("forbidden_tools", []))
        result["selected_tools"] += len(calls)
        for call in calls:
            outcome = call.get("outcome")
            if outcome not in OUTCOMES:
                raise ValueError(f"invalid tool outcome: {outcome}")
            if outcome == "scope_denied":
                result["scope_denied"] += 1
                if call["name"] in allowed and call["name"] not in forbidden:
                    stages.append("capability")
            elif outcome == "policy_denied":
                result["policy_denied"] += 1
                if call["name"] in allowed and call["name"] not in forbidden:
                    stages.append("policy")
            elif outcome in {"failed", "unknown_outcome"}:
                result["execution_failed"] += 1
            if call["name"] in forbidden:
                stages.append("selection")
                result["unsafe_executions"] += outcome in {"succeeded", "unknown_outcome"}
        disallowed_selection = not selected <= allowed or bool(selected & forbidden)
        if disallowed_selection:
            stages.append("selection")
        alternatives = rules.get("required_tool_sets", [])
        successful = {call["name"] for call in calls if call.get("outcome") == "succeeded"}
        tool_match = bool(alternatives) and any(set(group) <= successful for group in alternatives)
        required_skills = set(rules.get("required_skills_any", []))
        skill_match = bool(required_skills & set(actual.get("activated_skills", [])))
        answer_match = bool(rules.get("allow_answer_only") and not calls and str(actual.get("answer", "")).strip())
        if not (tool_match or skill_match or answer_match):
            stage = "selection"
            if disallowed_selection:
                stage = "selection"
            elif alternatives and any(set(group) <= selected for group in alternatives):
                stage = "execution"
            elif alternatives:
                capability = set(actual.get("capability_tools", []))
                discovered = set(actual.get("catalog_tools", [])) | set(actual.get("schema_tools", []))
                if not any(set(group) <= capability for group in alternatives):
                    stage = "capability"
                elif not any(set(group) <= discovered for group in alternatives):
                    stage = "discovery"
            stages.append(stage)
        if actual.get("task_verified") is not True:
            result["task_verified"] = False
    priority = ("missing_record", "adapter", "capability", "policy", "discovery", "selection", "execution")
    result["failure_stage"] = next((stage for stage in priority if stage in stages), None)
    result["routing_passed"] = not stages
    return result


def _key(record):
    sample = record.get("sample")
    if not isinstance(sample, int) or isinstance(sample, bool) or sample < 0:
        raise ValueError("sample must be a nonnegative integer")
    return record["case_id"], sample


def index_records(records):
    indexed = {}
    for record in records:
        key = _key(record)
        if key in indexed:
            raise ValueError(f"duplicate case/sample record: {key}")
        validate_metadata(record.get("metadata", {}))
        indexed[key] = record
    return indexed


def evaluate(manifest, records, repeats=1):
    if repeats < 1:
        raise ValueError("repeats must be positive")
    cases = {case["id"]: case for case in manifest["cases"]}
    indexed = index_records(records)
    strategies = {record.get("strategy") for record in records}
    if len(strategies) > 1:
        raise ValueError("evaluate one strategy at a time")
    metadata_values = [record["metadata"] for record in records]
    if metadata_values and any(meta != metadata_values[0] for meta in metadata_values[1:]):
        raise ValueError("metadata must be fixed within a strategy run")
    scored = []
    for (case_id, sample), record in indexed.items():
        if case_id not in cases:
            raise ValueError(f"unknown case id: {case_id}")
        if sample >= repeats:
            raise ValueError("sample exceeds configured repeats")
        if manifest.get("snapshot") and record["metadata"]["dataset_snapshot"] != manifest["snapshot"]:
            raise ValueError("metadata dataset_snapshot does not match manifest")
        scored.append(score_record(cases[case_id], record))
    expected = len(cases) * repeats
    passed = sum(result["routing_passed"] for result in scored)
    failures = Counter(result["failure_stage"] for result in scored if result["failure_stage"])
    missing = expected - len(scored)
    if missing:
        failures["missing_record"] += missing
    groups = {}
    for field in ("language", "family"):
        groups[field] = {}
        for value in sorted({case[field] for case in cases.values()}):
            denominator = sum(case[field] == value for case in cases.values()) * repeats
            group = [result for result in scored if result[field] == value]
            groups[field][value] = {"expected": denominator, "recorded": len(group),
                                    "routing_passed": sum(r["routing_passed"] for r in group)}
    return {"expected_task_runs": expected, "task_runs": len(scored), "missing_task_runs": missing,
            "routing_passed": passed, "routing_pass_rate": passed / expected if expected else 0,
            "task_verified": sum(r["task_verified"] for r in scored), "failure_stages": dict(failures),
            **{key: sum(r[key] for r in scored) for key in ("model_calls", "input_tokens", "output_tokens", "elapsed_ms", "selected_tools", "scope_denied", "policy_denied", "execution_failed", "unsafe_executions", "capability_count", "catalog_count", "schema_count")},
            "groups": groups, "results": scored}


def compare_records(left, right):
    a, b = index_records(left), index_records(right)
    if a.keys() != b.keys():
        raise ValueError("strategies have unmatched case/sample records")
    for key in a:
        for field in COMPARABLE_FIELDS:
            if a[key]["metadata"][field] != b[key]["metadata"][field]:
                raise ValueError(f"incompatible metadata {field} for {key}")
    return {"paired_task_runs": len(a)}


def run_adapter(case, command, metadata, strategy, sample, *, timeout, max_model_calls, max_tokens):
    validate_case(case)
    validate_metadata(metadata)
    if not command or timeout <= 0 or max_model_calls < 1 or max_tokens < 1:
        raise ValueError("an adapter command and positive budgets are required")
    request = {"case": case, "metadata": metadata, "strategy": strategy, "sample": sample,
               "budgets": {"max_model_calls": max_model_calls, "max_tokens": max_tokens, "timeout_seconds": timeout}}
    record = {"case_id": case["id"], "sample": sample, "strategy": strategy, "metadata": metadata, "turns": []}
    with tempfile.TemporaryDirectory(prefix="blockcell-intent-eval-") as directory:
        for relative, content in case.get("files", {}).items():
            target = Path(directory) / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(content)
        try:
            completed = subprocess.run(command, input=json.dumps(request, ensure_ascii=False), text=True,
                                       capture_output=True, cwd=directory, timeout=timeout, check=False)
            if completed.returncode:
                record["adapter_error"] = f"adapter exited {completed.returncode}"
                return record
            reply = json.loads(completed.stdout)
            if not isinstance(reply, dict) or not isinstance(reply.get("turns"), list):
                raise ValueError("adapter must return a turns array")
            validate_turns(reply["turns"])
            record["turns"] = reply["turns"]
            totals = {name: sum(_nonnegative(turn, name) for turn in reply["turns"])
                      for name in ("model_calls", "input_tokens", "output_tokens")}
            if totals["model_calls"] > max_model_calls or totals["input_tokens"] + totals["output_tokens"] > max_tokens:
                record["adapter_error"] = "adapter exceeded declared budget"
        except subprocess.TimeoutExpired:
            record["adapter_error"] = "adapter timed out"
        except (json.JSONDecodeError, ValueError, TypeError) as error:
            record["adapter_error"] = f"invalid adapter response: {error}"
    return record


def read_records(path):
    with Path(path).open() as stream:
        return [json.loads(line) for line in stream if line.strip()]


def plan_runs(manifest, repeats, max_runs):
    if repeats < 1 or max_runs < 1:
        raise ValueError("repeats and max-runs must be positive")
    families = {}
    for case in manifest["cases"]:
        families.setdefault(case["family"], {}).setdefault(case["pair_id"], []).append(case)
    queues = {family: list(pairs.values()) for family, pairs in sorted(families.items())}
    ordered = []
    while any(queues.values()):
        for queue in queues.values():
            if queue:
                ordered.extend(sorted(queue.pop(0), key=lambda task: task["language"]))
    plan = []
    for sample in range(repeats):
        for case in ordered:
            if len(plan) >= max_runs:
                return plan
            plan.append((case, sample))
    return plan


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=Path(__file__).with_name("manifest.json"))
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("validate")
    score = sub.add_parser("score")
    score.add_argument("records", type=Path)
    score.add_argument("--repeats", type=int, default=1)
    compare = sub.add_parser("compare")
    compare.add_argument("baseline", type=Path)
    compare.add_argument("candidate", type=Path)
    compare.add_argument("--repeats", type=int, default=1)
    run = sub.add_parser("run")
    run.add_argument("--adapter-json", required=True, help="JSON argv array; no shell interpolation")
    run.add_argument("--metadata", type=Path, required=True)
    run.add_argument("--strategy", required=True)
    run.add_argument("--output", type=Path, required=True)
    run.add_argument("--repeats", type=int, default=1)
    run.add_argument("--max-runs", type=int, default=12)
    run.add_argument("--max-model-calls", type=int, default=8)
    run.add_argument("--max-tokens", type=int, default=20000)
    run.add_argument("--timeout", type=float, default=120)
    args = parser.parse_args()
    manifest = load_manifest(args.manifest)
    if args.command == "validate":
        output = {"cases": len(manifest["cases"]), "pairs": len(manifest["cases"]) // 2, "snapshot": manifest["snapshot"]}
    elif args.command == "score":
        output = evaluate(manifest, read_records(args.records), args.repeats)
    elif args.command == "compare":
        left, right = read_records(args.baseline), read_records(args.candidate)
        output = compare_records(left, right)
        a, b = evaluate(manifest, left, args.repeats), evaluate(manifest, right, args.repeats)
        output.update({"baseline": a, "candidate": b, "routing_pass_rate_delta": b["routing_pass_rate"] - a["routing_pass_rate"]})
    else:
        command = json.loads(args.adapter_json)
        if not isinstance(command, list) or not all(isinstance(item, str) for item in command):
            parser.error("--adapter-json must be an argv array")
        if args.repeats < 1 or args.max_runs < 1:
            parser.error("repeats and max-runs must be positive")
        meta = json.loads(args.metadata.read_text())
        meta["dataset_snapshot"] = manifest["snapshot"]
        validate_metadata(meta)
        records = []
        with args.output.open("x") as stream:
            for case, sample in plan_runs(manifest, args.repeats, args.max_runs):
                value = run_adapter(case, command, meta, args.strategy, sample, timeout=args.timeout,
                                    max_model_calls=args.max_model_calls, max_tokens=args.max_tokens)
                stream.write(json.dumps(value, ensure_ascii=False) + "\n")
                stream.flush()
                records.append(value)
        output = evaluate(manifest, records, args.repeats)
    print(json.dumps(output, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
