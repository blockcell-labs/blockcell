import json
import sqlite3
from datetime import datetime, timezone
from pathlib import Path


class Archive:
    def __init__(self, root):
        Path(root).mkdir(parents=True, exist_ok=True)
        self.db = sqlite3.connect(Path(root) / "archive.sqlite3")
        self.db.row_factory = sqlite3.Row
        self.db.executescript("""
        PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS agents (
          agent_id TEXT PRIMARY KEY, commit_sha TEXT, parent_id TEXT,
          hypothesis_json TEXT, status TEXT, diff_path TEXT, created_at TEXT,
          children_count INTEGER DEFAULT 0, reason TEXT DEFAULT '',
          binary TEXT DEFAULT '', pass_count INTEGER DEFAULT 0,
          no_promote_rounds INTEGER DEFAULT 0, compile_fail_rounds INTEGER DEFAULT 0);
        CREATE TABLE IF NOT EXISTS evaluations (
          agent_id TEXT, commit_sha TEXT, benchmark TEXT, dataset_version TEXT,
          model_id TEXT, config_hash TEXT, run_idx INTEGER, pass_count INTEGER,
          tokens INTEGER, cost REAL, latency_p50 REAL, report_path TEXT,
          report_json TEXT, PRIMARY KEY(agent_id, benchmark, run_idx));
        CREATE TABLE IF NOT EXISTS task_results (
          agent_id TEXT, benchmark TEXT, task_id TEXT, run_idx INTEGER,
          passed INTEGER, failure_reason TEXT, trace_path TEXT,
          PRIMARY KEY(agent_id, benchmark, task_id, run_idx));
        CREATE TABLE IF NOT EXISTS cycle_state (
          id INTEGER PRIMARY KEY CHECK(id=1), state TEXT NOT NULL, payload TEXT NOT NULL);
        """)
        self.db.commit()

    def agent(self, agent_id):
        row = self.db.execute("SELECT * FROM agents WHERE agent_id=?", (agent_id,)).fetchone()
        return dict(row) if row else None

    def agents(self):
        return [dict(r) for r in self.db.execute("SELECT * FROM agents ORDER BY created_at, agent_id")]

    def add(self, agent_id, sha, parent=None, hypothesis=None, status="CANDIDATE", binary=""):
        with self.db:
            self.db.execute("INSERT OR IGNORE INTO agents(agent_id,commit_sha,parent_id,hypothesis_json,status,created_at,binary) VALUES(?,?,?,?,?,?,?)",
                            (agent_id, sha, parent, json.dumps(hypothesis or {}), status, datetime.now(timezone.utc).isoformat(), binary))

    def update(self, agent_id, **fields):
        allowed = set(self.agent(agent_id)) - {"agent_id"}
        if not fields.keys() <= allowed:
            raise ValueError("unknown agent fields")
        with self.db:
            self.db.execute("UPDATE agents SET " + ",".join(k + "=?" for k in fields) + " WHERE agent_id=?", (*fields.values(), agent_id))

    def transition(self, state, payload):
        with self.db:
            self.db.execute("INSERT OR REPLACE INTO cycle_state VALUES(1,?,?)", (state, json.dumps(payload)))

    def state(self):
        row = self.db.execute("SELECT state,payload FROM cycle_state WHERE id=1").fetchone()
        return (row[0], json.loads(row[1])) if row else ("IDLE", {"cycle": 0, "global_stagnation": 0})

    def record(self, agent, benchmark, idx, report, config, path):
        tasks = report.get("results", [])
        with self.db:
            self.db.execute("INSERT OR REPLACE INTO evaluations VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)", (
                agent["agent_id"], agent["commit_sha"], benchmark,
                config["benchmarks"]["dataset_version"], config["agent"]["model"], config["_hash"], idx,
                sum(bool(t["passed"]) for t in tasks), sum(t.get("input_tokens", 0) + t.get("output_tokens", 0) for t in tasks),
                report.get("cost_usd", 0), report.get("latency_p50", 0), str(path), json.dumps(report)))
            self.db.execute("DELETE FROM task_results WHERE agent_id=? AND benchmark=? AND run_idx=?", (agent["agent_id"], benchmark, idx))
            self.db.executemany("INSERT INTO task_results VALUES(?,?,?,?,?,?,?)", [(
                agent["agent_id"], benchmark, t["task_id"], idx, bool(t["passed"]), t.get("failure_reason", ""), t.get("trace_path", "")) for t in tasks])

    def cached(self, agent, benchmark, idx, config):
        row = self.db.execute("SELECT e.report_json FROM evaluations e JOIN agents a ON e.agent_id=a.agent_id WHERE a.agent_id=? AND e.commit_sha=? AND benchmark=? AND dataset_version=? AND model_id=? AND config_hash=? AND run_idx=?", (
            agent["agent_id"], agent["commit_sha"], benchmark, config["benchmarks"]["dataset_version"], config["agent"]["model"], config["_hash"], idx)).fetchone()
        return json.loads(row[0]) if row else None

    def failures(self, parent_id):
        lineage = set()
        while parent_id:
            lineage.add(parent_id)
            parent_id = self.agent(parent_id)["parent_id"]
        return [{"hypothesis": json.loads(a["hypothesis_json"]), "reason": a["reason"]}
                for a in self.agents() if a["parent_id"] in lineage and a["status"] in ("REJECTED", "INCONCLUSIVE")]

    def rekey(self, agent_id, sha):
        with self.db:
            self.db.execute("UPDATE evaluations SET commit_sha=? WHERE agent_id=?", (sha, agent_id))
