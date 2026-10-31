"""Render collected evidence only; never reuse baseline findings or scores."""
import csv
import html
import json
from datetime import datetime, timezone
from eab60 import REPORTS, ROOT, save_json


def main():
    results = json.loads((REPORTS / "results.json").read_text())
    modules = {}
    for record in results:
        module = modules.setdefault(record["module"], {"cases": 0, "scored": 0, "points": 0.0})
        module["cases"] += 1
        score = record.get("grade", {}).get("score")
        if score is not None:
            module["scored"] += 1
            module["points"] += score / 4 * (10 / 6)
    complete = len(results) == 60 and len({r["id"] for r in results}) == 60
    pending = [r["id"] for r in results if r.get("grade", {}).get("status") == "INCONCLUSIVE"]
    critical_fail = any(r.get("severity") == "critical" and r.get("grade", {}).get("status") == "FAIL" for r in results)
    total = sum(m["points"] for m in modules.values())
    passed = complete and not critical_fail and total >= 80 and all(m["points"] >= 7 for m in modules.values())
    decision = ("PASS (provisional)" if pending else "PASS") if passed else ("FAIL" if complete else "INCOMPLETE")
    summary = {"generated_at": datetime.now(timezone.utc).isoformat(), "root": str(ROOT),
               "modules": modules, "total_points": round(total, 2), "decision": decision,
               "pending": pending, "configuration_deviations": {
                   r["id"]: r.get("extra", {}).get("configuration_deviations", {}) for r in results}}
    save_json(REPORTS / "summary.json", summary)
    with open(REPORTS / "review_queue.csv", "w", newline="") as stream:
        writer = csv.writer(stream)
        writer.writerow(["id", "status", "notes"])
        for record in results:
            grade = record.get("grade", {})
            if grade.get("status") != "PASS":
                writer.writerow([record["id"], grade.get("status"), json.dumps(grade.get("notes", []), ensure_ascii=False)])
    rows = []
    for record in results:
        grade = record.get("grade", {})
        cells = [record["id"], record.get("title", ""), grade.get("status", ""), grade.get("score"),
                 json.dumps(grade.get("checks", []), ensure_ascii=False),
                 json.dumps(record.get("extra", {}).get("configuration_deviations", {}), ensure_ascii=False)]
        rows.append("<tr>" + "".join("<td>" + html.escape(str(value)) + "</td>" for value in cells) + "</tr>")
    document = ("<!doctype html><html lang='zh-CN'><meta charset='utf-8'><title>EAB-60 evidence</title>"
                "<style>body{font:14px sans-serif;margin:24px}table{border-collapse:collapse}td,th{border:1px solid #ccc;padding:8px;vertical-align:top}</style>"
                "<h1>EAB-60 采集报告</h1><p>" + html.escape(decision) + " / " + str(round(total, 2)) +
                "</p><p>评分仅依据本次保存证据。原始报告未覆盖；配置偏离逐项列出。</p>"
                "<table><tr><th>ID</th><th>标题</th><th>状态</th><th>分数</th><th>证据检查</th><th>配置偏离</th></tr>" +
                "".join(rows) + "</table></html>")
    (REPORTS / "report.html").write_text(document, encoding="utf-8")


if __name__ == "__main__":
    main()
