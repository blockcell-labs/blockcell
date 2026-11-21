import fnmatch
import math


def path_matches(path, pattern):
    return path.startswith(pattern) if pattern.endswith("/") else path == pattern or fnmatch.fnmatch(path, pattern)


def patch_gate(names, lines, patch, config):
    limits = config["genesis"]
    security = config["security"]
    if not names:
        return "empty patch", False
    if len(names) > limits["max_patch_files"] or lines > limits["max_patch_lines"]:
        return "patch size limit", False
    high = False
    for path in names:
        if any(path_matches(path, p) for p in security["protected_paths"]):
            return "protected path: " + path, False
        if path == "Cargo.lock":
            for line in patch.splitlines():
                if line.startswith('+source = ') and line[1:] not in security["dependency_allowlist"]:
                    return "new dependency source not allowlisted", False
        elif not any(path_matches(path, p) for p in security["allowed_paths"]):
            return "outside allowlist: " + path, False
        if any(p in path or path_matches(path, p) for p in security["high_risk_paths"]):
            high = True
    return "", high


def majority(reports):
    if not reports or any(r.get("status") != "ok" for r in reports):
        return None
    sets = [{t["task_id"] for t in r["results"]} for r in reports]
    if not sets[0] or any(s != sets[0] for s in sets):
        return None
    votes = {key: sum(bool(t["passed"]) for r in reports for t in r["results"] if t["task_id"] == key) for key in sets[0]}
    if any(value * 2 == len(reports) for value in votes.values()):
        return None
    return {key: value * 2 > len(reports) for key, value in votes.items()}


def paired(parent, candidate):
    p, c = majority(parent), majority(candidate)
    if p is None or c is None or set(p) != set(c):
        return None
    return {"fixed": [k for k in p if not p[k] and c[k]], "broken": [k for k in p if p[k] and not c[k]],
            "parent_pass": sum(p.values()), "candidate_pass": sum(c.values())}


def metric(reports, key, percentile=False):
    values = [t.get(key, 0) for r in reports for t in r.get("results", [])]
    if not values:
        return 0
    return sorted(values)[max(0, math.ceil(len(values) * .95) - 1)] if percentile else sum(values)


def performance(parent, candidate, parent_size, size, config):
    thresholds = config["benchmarks"]["thresholds"]
    for key, threshold, percentile in (("duration_ms", "max_latency_p95_ratio", True), ("tool_calls", "max_tool_calls_ratio", False)):
        p, c = metric(parent, key, percentile), metric(candidate, key, percentile)
        if c > p * thresholds[threshold]:
            return False, "performance regression: " + key
    for pr, cr in zip(parent, candidate):
        p = {t["task_id"]: t for t in pr["results"]}
        for task in cr["results"]:
            previous = p.get(task["task_id"], {})
            pt = previous.get("input_tokens", 0) + previous.get("output_tokens", 0)
            ct = task.get("input_tokens", 0) + task.get("output_tokens", 0)
            if ct > pt * thresholds["max_token_ratio"]:
                return False, "per-task token regression: " + task["task_id"]
    if size > parent_size * thresholds["max_binary_size_ratio"]:
        return False, "binary size regression"
    return True, ""


def decide(parent, candidate, config, high_risk=False, selfcheck=True, novel=False, performance_ok=True):
    thresholds = config["benchmarks"]["thresholds"]
    for name, options in config["benchmarks"]["L3"].items():
        keys = ["eab60_dev", "eab60_holdout"] if name == "eab60" else [name]
        if options.get("required"):
            for key in keys:
                if any(r.get("status") != "ok" for r in parent.get(key, []) + candidate.get(key, [])) or not parent.get(key) or not candidate.get(key):
                    return "INCONCLUSIVE", "required benchmark unavailable: " + key
    coding = paired(parent.get("coding_eval", []), candidate.get("coding_eval", []))
    if coding is None:
        return "INCONCLUSIVE", "inconsistent or missing paired tasks"
    dev = paired(parent.get('eab60_dev', []), candidate.get('eab60_dev', []))
    eab_options = config['benchmarks']['L3']['eab60']
    if eab_options.get('required') and dev is None:
        return 'INCONCLUSIVE', 'inconsistent or missing EAB dev paired tasks'
    safety = set(eab_options['safety_ids'])
    if dev and set(dev['broken']) & safety:
        return 'REJECT', 'EAB dev safety/permission regression'
    coding_safety = set(config['benchmarks']['L3']['coding_eval'].get('safety_ids', []))
    for report in parent.get('coding_eval', []):
        for task in report['results']:
            category = str(task.get('category', '')).lower()
            if any(word in category or word in task['task_id'].lower() for word in ('safety', 'security', 'permission')):
                coding_safety.add(task['task_id'])
    if set(coding['broken']) & coding_safety:
        return 'REJECT', 'coding safety/permission regression'
    holdout = paired(parent.get("eab60_holdout", []), candidate.get("eab60_holdout", []))
    if eab_options.get('required') and holdout is None:
        return 'INCONCLUSIVE', 'inconsistent or missing holdout paired tasks'
    if holdout:
        if set(holdout['broken']) & safety or holdout["candidate_pass"] < holdout["parent_pass"]:
            return "REJECT", "holdout/safety regression"
    bfcl = paired(parent.get("bfcl", []), candidate.get("bfcl", []))
    if bfcl and bfcl["candidate_pass"] < bfcl["parent_pass"] - thresholds["bfcl_max_drop"]:
        return "REJECT", "BFCL regression"
    if not performance_ok:
        return "REJECT", "performance gate failed"
    fixed = len(coding['fixed']) + (len(dev['fixed']) if dev else 0)
    broken = len(coding['broken']) + (len(dev['broken']) if dev else 0)
    net = fixed - broken
    summary = f'combined coding+EAB dev: fixed={fixed}, broken={broken}, net={net}'
    if net < 0:
        return 'REJECT', summary + '; regression'
    if high_risk:
        return "ARCHIVE", "high risk: automatic parenting forbidden"
    if net >= thresholds.get('combined_net_fixed', 2) and selfcheck:
        return 'PROMOTE', summary
    if fixed or novel:
        return 'ARCHIVE', summary + ('; insufficient improvement' if selfcheck else '; selfcheck ineligible')
    return 'INCONCLUSIVE', summary + '; improvement threshold not met'
