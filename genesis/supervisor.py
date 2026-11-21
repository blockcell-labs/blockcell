import json
import os
import time
from pathlib import Path

from . import diagnosis, gates, report
from .adapters import cargo
from .agent_adapter import AgentAdapter
from .archive import Archive
from .evaluator import Evaluator
from .isolation import BudgetExceeded, Meter, ProviderUnavailable
from .selector import Selector
from .source import Source, command


class Supervisor:
    def __init__(self, config):
        self.config = config
        self.root = Path(config["_root"])
        self.archive = Archive(self.root)
        self.source = Source(config)
        self.selector = Selector(config)
        self.usage_path = self.root / "usage.json"
        usage = json.loads(self.usage_path.read_text()) if self.usage_path.exists() else {}
        agent = config["agent"]
        self.meter = Meter(config["genesis"]["budget"]["total_usd"], agent["input_usd_per_million"], agent["output_usd_per_million"], token_limit=agent.get('token_limit', 20000000), on_update=self.save_usage, concurrency=agent.get('concurrency', 1), retry_429_seconds=agent.get('retry_429_seconds', 300), user_agent=agent.get('user_agent', 'Mozilla/5.0'))
        self.meter.input_tokens = usage.get("input_tokens", 0)
        self.meter.output_tokens = usage.get("output_tokens", 0)
        self.meter.cost = usage.get("cost", 0)
        self.meter.peak_requests = usage.get('peak_requests', 0)
        self.meter.requests = usage.get('requests', 0)
        self.meter.retries_429 = usage.get('retries_429', 0)
        self.adapter = AgentAdapter(config, self.root / "runs" / "agent_calls", self.meter)
        self.evaluator = Evaluator(config, self.archive, self.adapter)

    def save_usage(self, meter):
        temp = self.usage_path.with_suffix(".new")
        temp.write_text(json.dumps({"input_tokens": meter.input_tokens, "output_tokens": meter.output_tokens, "cost": meter.cost, "upstream_status": meter.upstream_status, "peak_requests": meter.peak_requests, "requests": meter.requests, "retries_429": meter.retries_429}))
        temp.replace(self.usage_path)

    def init(self):
        sha = self.source.initialize()
        current = self.archive.agent("genesis-0")
        if not current or not Path(current["binary"]).is_file():
            self.archive.transition("BUILD_GENESIS0", {"cycle": 0, "global_stagnation": 0})
            binary, seconds = self.source.build("genesis-0", self.root / "frozen")
            self.archive.add("genesis-0", sha, status="PARENT", binary=binary)
            self.archive.update("genesis-0", binary=binary)
            self.archive.transition("IDLE", {"cycle": 0, "global_stagnation": 0})
            print(f"Genesis-0 release build: {seconds:.2f}s; copied to {binary}")
        print(f"Genesis-0 SHA: {sha}\nFrozen worktree: {self.root / 'frozen'}")
        print(self.adapter.help(self.archive.agent("genesis-0")["binary"], self.root / "frozen"))

    def baseline(self):
        agent = self.archive.agent("genesis-0")
        if not agent:
            raise RuntimeError("run init first")
        result = self.evaluator.full(agent, cached=True)
        (self.root / "baseline.json").write_text(json.dumps(result, ensure_ascii=False, indent=2))
        return result

    def transition(self, stage, payload):
        self.archive.transition(stage, payload)
        with (self.root / "transitions.jsonl").open("a") as stream:
            stream.write(json.dumps({"state": stage, "cycle": payload.get("cycle"), "agent_id": payload.get("agent_id"), "time": time.time()}) + "\n")

    def run(self, cycles=1, resume=False):
        if not self.archive.agent("genesis-0"):
            raise RuntimeError("run init first")
        stage, p = self.archive.state()
        if resume and p.get('blocked_at'):
            p['cycle_started'] += time.time() - p.pop('blocked_at')
            p.pop('blocked_reason', None)
            self.transition(stage, p)
        if stage == "BUILD_GENESIS0":
            self.init()
            stage, p = self.archive.state()
        if not resume:
            if stage not in ("IDLE", "STOPPED"):
                raise RuntimeError("unfinished run: use resume")
            p["target_cycle"] = p.get("cycle", 0) + cycles
            self.transition("IDLE", p)
        while True:
            stage, p = self.archive.state()
            if stage == "STOPPED":
                if resume:
                    break
                self.transition("IDLE", p)
                continue
            try:
                if stage == "IDLE":
                    if p.get("cycle", 0) >= p.get("target_cycle", 0):
                        break
                    self.meter.check()
                    if p.get("global_stagnation", 0) >= self.config["genesis"]["stagnation"]["global_rounds"]:
                        self.transition("STOPPED", p)
                        break
                    p.update(cycle=p.get("cycle", 0) + 1, index=0, promoted=False,
                             compile_failures=0, cycle_cost=self.meter.cost, cycle_started=time.time())
                    self.meter.limit = min(self.config["genesis"]["budget"]["total_usd"], self.meter.cost + self.config["genesis"]["budget"]["per_cycle_usd"])
                    self.transition("SELECT_PARENT", p)
                elif stage == "SELECT_PARENT":
                    force = any(a["no_promote_rounds"] >= self.config["genesis"]["stagnation"]["per_parent_rounds"] for a in self.archive.agents())
                    try:
                        parent = self.selector.select(self.archive.agents(), force)
                    except RuntimeError:
                        parent = self.archive.agent("genesis-0")
                    try:
                        self.adapter.help(parent["binary"], self.root / "frozen")
                        if not self.evaluator.selfcheck(parent["binary"]):
                            raise RuntimeError("parent selfcheck failed")
                    except (BudgetExceeded, ProviderUnavailable):
                        raise
                    except (RuntimeError, TimeoutError, OSError):
                        self.archive.update(parent["agent_id"], status="UNFIT")
                        parent = self.archive.agent("genesis-0")
                        self.adapter.help(parent["binary"], self.root / "frozen")
                    p["parent_id"] = parent["agent_id"]
                    self.transition("PARENT_EVAL", p)
                elif stage == "PARENT_EVAL":
                    parent = self.archive.agent(p["parent_id"])
                    baseline_tree = self.root / 'frozen' if parent['agent_id'] == 'genesis-0' else self.source.worktree('audit-' + parent['agent_id'], parent['commit_sha'])
                    p['parent_L0'] = cargo.baseline(baseline_tree, self.config)
                    p["parent_results"] = self.evaluator.full(parent, cached=True)
                    scores = gates.majority(p["parent_results"]["coding_eval"])
                    self.archive.update(parent["agent_id"], pass_count=sum(scores.values()) if scores else 0)
                    self.transition("DIAGNOSE", p)
                elif stage == "DIAGNOSE":
                    parent = self.archive.agent(p["parent_id"])
                    workspace = self.source.worktree(f"diagnose-{p['cycle']}", parent["commit_sha"])
                    p["hypotheses"], p["diagnosis_errors"] = diagnosis.diagnose(self.adapter, parent, workspace,
                        self.archive.failures(parent["agent_id"]), self.evaluator.evidence(p["parent_results"]), self.config)
                    if not p["hypotheses"]:
                        for index in range(self.config["genesis"]["candidates_per_cycle"]):
                            aid = f"agent-{p['cycle']:04d}-{index}"
                            self.archive.add(aid, parent["commit_sha"], parent["agent_id"], status="REJECTED")
                            self.archive.update(aid, reason="diagnosis skipped after retries: " + "; ".join(p["diagnosis_errors"]))
                    self.transition("CREATE_WORKTREE" if p["hypotheses"] else "END_CYCLE", p)
                elif stage == "CREATE_WORKTREE":
                    aid = f"agent-{p['cycle']:04d}-{p['index']}"
                    parent = self.archive.agent(p["parent_id"])
                    self.archive.add(aid, parent["commit_sha"], parent["agent_id"], p["hypotheses"][p["index"]])
                    self.archive.update(aid, hypothesis_json=json.dumps(p['hypotheses'][p['index']]), status='CANDIDATE', reason='')
                    p["agent_id"] = aid
                    p["worktree"] = str(self.source.worktree(aid, parent["commit_sha"]))
                    for key in ("decision", "reason", "candidate_results", "high_risk", "selfcheck", "binary", "build_seconds"):
                        p.pop(key, None)
                    self.transition("PATCH", p)
                elif stage == "PATCH":
                    parent = self.archive.agent(p["parent_id"])
                    limits = self.config['genesis']
                    self.adapter.call(parent["binary"], "MODE: patch\nImplement this hypothesis, keeping protected evaluation files unchanged. "
                        + f"Hard limits: at most {limits['max_patch_files']} files and {limits['max_patch_lines']} added+deleted lines, including tests and formatting. "
                        + "Keep changes small. Offline cargo and a shared target are available: run cargo check -p <changed-crate> and cargo test -p <changed-crate>. "
                        + "Use cargo fmt on changed files only; do not modify unrelated files. Do not run concurrent cargo commands.\n"
                        + json.dumps(p["hypotheses"][p["index"]]), Path(p["worktree"]), "patch")
                    self.transition('FORMAT', p)
                elif stage == 'FORMAT':
                    names, _, _ = self.source.diff(p['worktree'])
                    cargo.format_changed(Path(p['worktree']), names, self.config)
                    self.transition("L0", p)
                elif stage == "L0":
                    folder = self.root / "runs" / p["agent_id"]
                    folder.mkdir(parents=True, exist_ok=True)
                    with cargo.runtime(p['worktree'], folder / 'L0-runtime'):
                        try:
                            p["binary"], p["build_seconds"] = self.source.build(p["agent_id"], p["worktree"])
                        except (RuntimeError, TimeoutError):
                            p["compile_failures"] += 1
                            raise
                        parent = self.archive.agent(p["parent_id"])
                        audit_parent = self.source.worktree("audit-" + parent["agent_id"], parent["commit_sha"])
                        output = cargo.evaluate(Path(p["worktree"]), self.config, audit_parent)
                    (folder / "L0.log").write_text(output)
                    self.archive.update(p["agent_id"], binary=p["binary"])
                    self.transition("L1", p)
                elif stage == "L1":
                    names, lines, patch = self.source.diff(p["worktree"])
                    folder = self.root / "runs" / p["agent_id"]
                    folder.mkdir(parents=True, exist_ok=True)
                    path = folder / "patch.diff"
                    path.write_text(patch)
                    self.archive.update(p["agent_id"], diff_path=str(path))
                    reason, high_risk = gates.patch_gate(names, lines, patch, self.config)
                    if reason:
                        raise RuntimeError(reason)
                    p["high_risk"] = high_risk
                    p["names"] = names
                    self.transition("REVIEW", p)
                elif stage == "REVIEW":
                    parent = self.archive.agent(p["parent_id"])
                    patch = Path(self.archive.agent(p["agent_id"])["diff_path"]).read_text()
                    p["review"] = self.adapter.call(parent["binary"], "MODE: review\nExplain risks; do not modify any file.\n" + patch, Path(p["worktree"]), "review")
                    # Review is untrusted too; recheck its entire resulting diff.
                    names, lines, patch = self.source.diff(p["worktree"])
                    reason, high = gates.patch_gate(names, lines, patch, self.config)
                    if reason:
                        raise RuntimeError(reason)
                    if patch != Path(self.archive.agent(p["agent_id"])["diff_path"]).read_text():
                        raise RuntimeError("review unexpectedly modified source")
                    self.transition("L2", p)
                elif stage == "L2":
                    agent = self.archive.agent(p["agent_id"])
                    quick = self.evaluator.evaluate(agent, "coding_eval", 0, cached=False)
                    parent = p["parent_results"]["coding_eval"][0]
                    drop = sum(t["passed"] for t in parent["results"]) - sum(t["passed"] for t in quick["results"])
                    if drop > self.config["benchmarks"]["L2"]["coding_eval"]["reject_if_drop_gt"]:
                        raise RuntimeError("L2 coding regression")
                    p["quick"] = quick
                    self.transition("L3", p)
                elif stage == "L3":
                    agent = self.archive.agent(p["agent_id"])
                    count = self.config["benchmarks"]["L3"]["coding_eval"]["runs"]
                    p["candidate_results"] = {"coding_eval": [p["quick"]] + [self.evaluator.evaluate(agent, "coding_eval", i) for i in range(1, count)]}
                    for name in ("eab60_dev", "eab60_holdout", "bfcl"):
                        p["candidate_results"][name] = [self.evaluator.evaluate(agent, name, 0)]
                    self.transition("L4", p)
                elif stage == "L4":
                    parent = self.archive.agent(p["parent_id"])
                    p["performance_ok"], p["performance_reason"] = gates.performance(p["parent_results"]["coding_eval"], p["candidate_results"]["coding_eval"],
                        Path(parent["binary"]).stat().st_size, Path(p["binary"]).stat().st_size, self.config)
                    p["selfcheck"] = self.evaluator.selfcheck(p["binary"])
                    self.transition("DECIDE", p)
                elif stage == "DECIDE":
                    previous_modules = set()
                    for a in self.archive.agents():
                        if a["agent_id"] != p["agent_id"]:
                            previous_modules.update(json.loads(a["hypothesis_json"]).get("suspected_files", []))
                    novel = any(name not in previous_modules for name in p["names"])
                    p["decision"], p["reason"] = gates.decide(p["parent_results"], p["candidate_results"], self.config,
                        p["high_risk"], p["selfcheck"], novel, p["performance_ok"])
                    if p['decision'] == 'PROMOTE' and not self.config['promotion'].get('commit_enabled', True):
                        p['decision'], p['reason'] = 'ARCHIVE', p['reason'] + '; promotion disabled by no-commit policy'
                    self.transition("PROMOTE" if p["decision"] == "PROMOTE" else "RECORD", p)
                elif stage == "PROMOTE":
                    sha = self.source.promote(p["agent_id"], p["worktree"])
                    self.archive.update(p["agent_id"], commit_sha=sha)
                    self.archive.rekey(p["agent_id"], sha)
                    p["promoted"] = True
                    self.transition("RECORD", p)
                elif stage == "RECORD":
                    aid = p["agent_id"]
                    status = {"PROMOTE": "PARENT", "REJECT": "REJECTED"}.get(p["decision"], p["decision"])
                    if status == "ARCHIVE" and self.config['promotion'].get('commit_enabled', True):
                        sha = self.source.snapshot(aid, p["worktree"])
                        self.archive.update(aid, commit_sha=sha)
                        self.archive.rekey(aid, sha)
                    scores = gates.majority(p.get("candidate_results", {}).get("coding_eval", []))
                    self.archive.update(aid, status=status, reason=p["reason"], pass_count=sum(scores.values()) if scores else 0)
                    folder = self.root / "runs" / aid
                    folder.mkdir(parents=True, exist_ok=True)
                    (folder / "decision.json").write_text(json.dumps(p, ensure_ascii=False, indent=2))
                    if status == "PARENT":
                        (folder / "pr_draft.md").write_text("# Genesis " + aid + "\n\n" + p["reason"] + "\n\n仅本地晋级，未 push。\n")
                    # Recompute rather than increment to make crash recovery idempotent.
                    parent = self.archive.agent(p["parent_id"])
                    children = sum(a["parent_id"] == parent["agent_id"] for a in self.archive.agents())
                    self.archive.update(parent["agent_id"], children_count=children)
                    p["index"] += 1
                    self.transition("CREATE_WORKTREE" if p["index"] < len(p["hypotheses"]) else "END_CYCLE", p)
                elif stage == "END_CYCLE":
                    parent = self.archive.agent(p["parent_id"])
                    failed = p["compile_failures"] == len(p.get("hypotheses", [])) and bool(p.get("hypotheses"))
                    # Store next counters in the state before applying to agent rows.
                    p["next_no_promote"] = 0 if p["promoted"] else parent["no_promote_rounds"] + 1
                    p["next_compile_fail"] = parent["compile_fail_rounds"] + 1 if failed else 0
                    p["next_global"] = 0 if p["promoted"] else p.get("global_stagnation", 0) + 1
                    self.transition("FINISH_CYCLE", p)
                elif stage == "FINISH_CYCLE":
                    fields = {"no_promote_rounds": p["next_no_promote"], "compile_fail_rounds": p["next_compile_fail"]}
                    if p["next_compile_fail"] >= self.config["genesis"]["unfit_compile_rounds"]:
                        fields["status"] = "UNFIT"
                    self.archive.update(p["parent_id"], **fields)
                    p["global_stagnation"] = p["next_global"]
                    self.transition("IDLE", p)
                else:
                    raise RuntimeError("unknown persisted stage: " + stage)
                if "cycle_started" in p and stage not in ("IDLE", "FINISH_CYCLE"):
                    if time.time() - p["cycle_started"] > self.config["genesis"]["max_cycle_minutes"] * 60:
                        raise BudgetExceeded("cycle time limit")
                    if self.meter.cost - p["cycle_cost"] >= self.config["genesis"]["budget"]["per_cycle_usd"]:
                        raise BudgetExceeded("cycle budget exhausted")
            except ProviderUnavailable as exc:
                p['blocked_reason'] = str(exc)
                p['blocked_at'] = time.time()
                self.transition(stage, p)
                raise
            except BudgetExceeded as exc:
                p["stop_reason"] = str(exc)
                self.transition("STOPPED", p)
                break
            except (RuntimeError, TimeoutError, ValueError, OSError) as exc:
                if stage in ("PATCH", "FORMAT", "L0", "L1", "REVIEW", "L2", "L3", "L4"):
                    p["decision"], p["reason"] = "REJECT", stage + ": " + str(exc)
                    self.transition("RECORD", p)
                else:
                    raise
        return report.write(self.archive, self.root)
