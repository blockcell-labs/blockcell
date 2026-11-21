import json
import copy
import os
import math
import shutil
import sys
import uuid
from pathlib import Path

from .adapters import bfcl, coding_eval, eab60
from .config import expand
from .source import command
from .isolation import Proxy, clean_env


class Evaluator:
    def __init__(self, config, archive, adapter):
        self.config = config
        self.archive = archive
        self.adapter = adapter
        self.root = Path(config["_root"])
        self.frozen = self.root / "frozen"

    def evaluate(self, agent, benchmark, idx, cached=False):
        if cached:
            result = self.archive.cached(agent, benchmark, idx, self.config)
            if result is not None:
                return result
        folder = self.root / "runs" / agent["agent_id"] / benchmark / str(idx)
        folder.mkdir(parents=True, exist_ok=True)
        output = folder / "report.json"
        output.unlink(missing_ok=True)
        options = self.config["benchmarks"]["L3"][benchmark.split("_")[0] if benchmark.startswith("eab60_") else benchmark]
        if options.get("command") and benchmark != "bfcl":
            argv = [expand(part, binary=agent["binary"], output=output, frozen=self.frozen) for part in options["command"]]
            command(argv, self.frozen, self.config["agent"]["timeout"], clean_env(folder, self.config["agent"]["key_env"]))
            result = json.loads(output.read_text())
        elif benchmark == "coding_eval":
            settings = self.config["agent"]
            isolated_config = copy.deepcopy(self.config)
            context = {"config": isolated_config, "binary": agent["binary"], "run_root": str(folder),
                       "package_root": str(Path(__file__).resolve().parent.parent),
                       "meter": {"limit": 1, "input_rate": 0, "output_rate": 0}}
            context_path = folder / "context.json"
            wrapper = folder / "agent_wrapper.py"
            wrapper.write_text("import runpy,sys\nsys.argv.insert(1," + repr(str(context_path)) + ")\nrunpy.run_path(" + repr(str(Path(__file__).with_name("eval_bridge.py"))) + ",run_name='__main__')\n")
            with Proxy(settings["upstream"], os.environ.get(settings["key_env"], ""), self.adapter.meter, allowed_model=settings['model']) as proxy:
                isolated_config["agent"]["upstream"] = proxy.url
                context_path.write_text(json.dumps(context))
                result = coding_eval.evaluate(self.frozen, agent["binary"], output, self.config, wrapper,
                                              env=clean_env(folder / "evaluator-home", settings["key_env"]))
        elif benchmark.startswith("eab60_"):
            result = eab60.evaluate(self.frozen, agent["binary"], folder, self.config, benchmark.removeprefix("eab60_"), self.adapter.meter)
        else:
            result = bfcl.evaluate(agent["binary"], output, self.frozen, self.config, self.adapter.meter)
        for row in result.get("results", []):
            if not isinstance(row.get("task_id"), str) or type(row.get("passed")) is not bool:
                raise ValueError("invalid task scorecard")
            row.setdefault("duration_ms", row.get("duration_seconds", 0) * 1000)
            for key in ("tool_calls", "tool_errors", "input_tokens", "output_tokens"):
                row.setdefault(key, 0)
            row.setdefault("failure_reason", row.get("failure_category") or "")
            row.setdefault("trace_path", "")
        tasks = result.get("results", [])
        durations = sorted(t["duration_ms"] for t in tasks)
        result["pass_count"] = sum(t["passed"] for t in tasks)
        result["success_rate"] = result["pass_count"] / len(tasks) if tasks else 0
        result["tokens_total"] = sum(t["input_tokens"] + t["output_tokens"] for t in tasks)
        result["latency_p50"] = durations[max(0, math.ceil(len(tasks) * .5) - 1)] if tasks else 0
        result["latency_p95"] = durations[max(0, math.ceil(len(tasks) * .95) - 1)] if tasks else 0
        result.setdefault("regression_ids", [])
        result.setdefault("safety_violations", [])
        result.setdefault("cost_usd", sum((t["input_tokens"] * self.config["agent"]["input_usd_per_million"] + t["output_tokens"] * self.config["agent"]["output_usd_per_million"]) / 1000000 for t in tasks))
        output.write_text(json.dumps(result, ensure_ascii=False, indent=2))
        self.archive.record(agent, benchmark, idx, result, self.config, output)
        return result

    def full(self, agent, cached=False):
        results = {}
        for name in ("coding_eval", "eab60_dev", "eab60_holdout", "bfcl"):
            count = self.config["benchmarks"]["L3"]["coding_eval"]["runs"] if name == "coding_eval" else 1
            results[name] = [self.evaluate(agent, name, idx, cached) for idx in range(count)]
        return results

    def selfcheck(self, binary):
        work = self.root / "runs" / "selfcheck" / uuid.uuid4().hex
        shutil.copytree(Path(__file__).parent / "fixtures/selfcheck", work)
        try:
            self.adapter.call(binary, "MODE: selfcheck\nModify subject.py so answer() returns 42. Do not change check.py. Run check.py.", work, "selfcheck")
            fixture = Path(__file__).parent / "fixtures/selfcheck/check.py"
            # Always restore the trusted assertion after candidate execution.
            shutil.copy2(fixture, work / "check.py")
            env = clean_env(work / "check-home", self.config["agent"]["key_env"])
            # The assertion imports candidate-modified code and is itself untrusted execution.
            if self.config["agent"]["require_bwrap"]:
                self.adapter.call(binary, "", work, "check", argv_override=["/usr/bin/python3", "-B", "check.py"])
            else:
                command([sys.executable, "-B", "check.py"], work, self.config["agent"]["timeout"], env)
            return True
        except (RuntimeError, TimeoutError):
            return False

    def evidence(self, results):
        # Holdout and safety records must never be included in diagnosis.
        counts = {}
        for name in ("coding_eval", "eab60_dev", "bfcl"):
            for report in results.get(name, []):
                for task in report.get("results", []):
                    if task.get('infrastructure_error'):
                        continue
                    if not task["passed"]:
                        key = name + "/" + task["task_id"]
                        record = counts.setdefault(key, {"task_id": key, "failure_frequency": 0})
                        record["failure_frequency"] += 1
                        record["failure_reason"] = task.get("failure_reason") or task.get("failure_category", "")
                        record["trace_excerpt"] = str(task.get("agent_output_tail", ""))[-2000:]
                        record["acceptance_excerpt"] = str(task.get("acceptance_output_tail", ""))[-2000:]
        return sorted(counts.values(), key=lambda item: -item["failure_frequency"])
