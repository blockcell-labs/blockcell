"""Re-grade stored evidence without re-running the agent.

The graders only read evidence that `run_all.py` already persisted, so after a
grader fix the whole suite can be re-scored offline (and re-scored again by a
reviewer) without spending another 60 model turns.
"""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import graders
from eab60 import EVIDENCE, REPORTS, KIT, save_json

CASES = {
    json.loads(line)["id"]: json.loads(line)
    for line in (
        (KIT / "cases/blockcell_eab60.jsonl")
        .read_text()
        .splitlines()
    )
    if line.strip()
}


class Ctx:
    pass


def load_ctx(case_dir):
    case = json.loads((case_dir / "case.json").read_text())
    ctx = Ctx()
    ctx.case = case
    ctx.turns = json.loads((case_dir / "turns.json").read_text()) if (case_dir / "turns.json").exists() else []
    ctx.events = json.loads((case_dir / "events.json").read_text()) if (case_dir / "events.json").exists() else []
    ctx.audit = json.loads((case_dir / "audit.json").read_text()) if (case_dir / "audit.json").exists() else []
    ctx.hooks = json.loads((case_dir / "hooks.json").read_text()) if (case_dir / "hooks.json").exists() else []
    ctx.before = json.loads((case_dir / "workspace_before.json").read_text()) if (case_dir / "workspace_before.json").exists() else {}
    ctx.after = json.loads((case_dir / "workspace_after.json").read_text()) if (case_dir / "workspace_after.json").exists() else {}
    ctx.tool_calls = case.get("tool_calls") or []
    ctx.tool_results = [
        {
            "call_id": e["payload"].get("call_id"),
            "outcome": e["payload"].get("outcome"),
            "is_error": e["payload"].get("is_error"),
            "result": e["payload"].get("result"),
        }
        for e in ctx.events
        if e.get("kind") == "tool/result"
    ]
    ctx.diff = case.get("diff") or {}
    ctx.extra = case.get("extra") or {}
    ctx.error = case.get("error")
    ctx.evidence_dir = case_dir
    return case, ctx


def main():
    loaded = []
    for case_dir in sorted(EVIDENCE.iterdir()):
        if not case_dir.is_dir():
            continue
        case_json = case_dir / "case.json"
        if not case_json.exists():
            continue
        record, ctx = load_ctx(case_dir)
        loaded.append((case_dir, record, ctx))

    durations = sorted(r.get("duration_s") or 0 for _, r, _ in loaded)
    stats = {}
    if durations:
        n = len(durations)
        stats = {
            "n": n,
            "mean_s": round(sum(durations) / n, 2),
            "p50_s": round(durations[int(0.5 * (n - 1))], 2),
            "p95_s": round(durations[min(n - 1, int(round(0.95 * (n - 1))))], 2),
            "max_s": round(durations[-1], 2),
        }
    (REPORTS / "latency_stats.json").write_text(json.dumps(stats, indent=2), encoding="utf-8")

    results = []
    for case_dir, record, ctx in loaded:
        case_id = record["id"]
        grader = graders.GRADERS.get(case_id)
        if not grader:
            continue
        # Delivery integrity: does the answer the user receives still contain the
        # answer the model produced before the post-answer verification gate?
        model_outputs = [
            (e.get("payload") or {}).get("content") or ""
            for e in ctx.events
            if e.get("kind") == "model/response"
            and ((e.get("payload") or {}).get("content") or "").strip()
        ]
        delivered = record.get("final_text") or ""
        delivery = {"model_answers": len(model_outputs), "delivered_chars": len(delivered)}
        if model_outputs:
            first = model_outputs[0]
            delivery["model_answer_chars"] = len(first)
            delivery["replaced"] = (
                delivered.strip() != first.strip()
                and len(delivered.strip()) < 0.7 * len(first.strip())
            )
        else:
            delivery["replaced"] = False
        record.setdefault("extra", {})["delivery"] = delivery
        if case_id == "J04":
            record["extra"]["latency_stats"] = stats
        grade = grader(ctx)
        record["grade"] = grade
        record["regraded"] = True
        save_json(case_json, record)
        results.append(record)
        print("%s %-13s %s" % (case_id, grade["status"], grade["score"]))
    save_json(REPORTS / "results.json", results)
    print("regraded %d cases" % len(results))


if __name__ == "__main__":
    main()
