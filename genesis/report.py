import json
from pathlib import Path


def write(archive, root):
    root = Path(root)
    agents = archive.agents()
    state, payload = archive.state()
    evaluations = [dict(row) for row in archive.db.execute("SELECT agent_id,benchmark,dataset_version,model_id,config_hash,run_idx,pass_count,tokens,cost,latency_p50,report_path FROM evaluations ORDER BY agent_id,benchmark,run_idx")]
    usage_path = root / "usage.json"
    usage = json.loads(usage_path.read_text()) if usage_path.exists() else {}
    data = {"state": state, "cycle": payload, "agents": agents, "evaluations": evaluations, "usage": usage}
    (root / "report.json").write_text(json.dumps(data, ensure_ascii=False, indent=2))
    lines = ["# Blockcell Genesis 实验报告", "", f"状态：{state}", "", "| Agent | Parent | Status | SHA | Reason |", "|---|---|---|---|---|"]
    lines.extend(f"| {a['agent_id']} | {a['parent_id']} | {a['status']} | {a['commit_sha']} | {a['reason']} |" for a in agents)
    (root / "report.md").write_text("\n".join(lines) + "\n")
    return str(root / "report.md")
