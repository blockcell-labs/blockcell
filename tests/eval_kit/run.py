import os, sys, json, time, subprocess, shutil, glob
import signal
from pathlib import Path
import tasks
BASE = Path(__file__).resolve().parent
WORK = Path(os.environ.get("BLOCKCELL_EVAL_RUN_DIR", str(BASE / "work")))
STATE = Path(os.environ.get("BLOCKCELL_EVAL_STATE", str(Path.home() / ".blockcell")))
BC = os.environ.get("BLOCKCELL_EVAL_BIN", str(BASE.parents[1] / "target" / "release" / "blockcell"))
SNAP = str(WORK / "fixtures_snapshot"); LOG = str(WORK / "logs"); os.makedirs(LOG, exist_ok=True)
ids = sys.argv[1:] or list(tasks.TASKS)
TIMEOUT = int(os.environ.get("BLOCKCELL_EVAL_TIMEOUT", "600"))
if not os.path.isdir(SNAP):
    shutil.copytree(tasks.B, SNAP)
for tid in ids:
    t = tasks.TASKS[tid]; d = f"{tasks.B}/{tid}"
    shutil.rmtree(d, ignore_errors=True); shutil.copytree(f"{SNAP}/{tid}", d)
    # Unique sessions preserve prior logs; never delete state by substring match.
    session = f"cli:eval-{tid}-{time.time_ns()}"
    test_path = os.path.join(d, "test_stats.py")
    original_test = Path(test_path).read_bytes() if tid == "t01" else None
    t0 = time.time()
    with open(f"{LOG}/{tid}.log", "w") as lf:
        try:
            p = subprocess.Popen([BC, "agent", "-s", session, "-m", t["prompt"]], cwd=d, stdout=lf, stderr=subprocess.STDOUT, start_new_session=True)
            rc = p.wait(timeout=TIMEOUT)
        except subprocess.TimeoutExpired:
            rc = "timeout"
            os.killpg(p.pid, signal.SIGTERM)
            try:
                p.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(p.pid, signal.SIGKILL)
                p.wait()
    wall = round(time.time() - t0, 1)
    ev = sorted(glob.glob(str(STATE / "session_events" / "*.jsonl")))
    events = [json.loads(l) for f in ev for l in open(f) if l.strip()]
    events = [e for e in events if e.get("session_id") == session]
    steps = sum(e["kind"] == "model/request" for e in events)
    proposed = sum(len(e["payload"].get("tool_calls") or []) for e in events if e["kind"] == "model/response")
    calls = [e["payload"] for e in events if e["kind"] == "tool/call"]
    results = [e["payload"] for e in events if e["kind"] == "tool/result"]
    errs = [r for r in results if r.get("is_error")]
    usage = [e["payload"].get("usage") or {} for e in events if e["kind"] == "model/response"]
    tin = sum(u.get("input_tokens", 0) for u in usage); tout = sum(u.get("output_tokens", 0) for u in usage)
    first_in = usage[0].get("input_tokens", 0) if usage else 0
    tools_hist = {}
    for c in calls: tools_hist[c.get("tool")] = tools_hist.get(c.get("tool"), 0) + 1
    final = [e["payload"].get("content", "") for e in events if e["kind"] == "assistant/message"]
    logtxt = open(f"{LOG}/{tid}.log", errors="ignore").read()
    try: ok, detail = t["verify"](d)
    except Exception as ex: ok, detail = False, "verify error " + repr(ex)[:150]
    if original_test is not None and (not Path(test_path).exists() or Path(test_path).read_bytes() != original_test):
        ok, detail = False, "original test_stats.py was modified or removed"
    rec = dict(id=tid, cat=t["cat"], ok=ok, rc=rc, wall_s=wall, llm_calls=steps, tool_calls=len(calls), proposed_tool_calls=proposed, completed_tool_calls=len(results), rejected_tool_calls=sum(r.get("outcome") == "rejected" for r in results), tool_errors=len(errs),
               err_samples=[(r.get("tool"), str(r.get("result"))[:160]) for r in errs[:4]], tools=tools_hist,
               in_tok=tin, out_tok=tout, first_in_tok=first_in, http429=logtxt.count("HTTP_STATUS=429"),
               turn_err=[e["payload"] for e in events if e["kind"] == "turn/end" and e["payload"].get("error")][:1],
               final=(final[-1] if final else "")[:400], detail=str(detail)[:300])
    open(WORK / "results.jsonl", "a").write(json.dumps(rec, ensure_ascii=False) + "\n")
    print(tid, ok, wall, steps, len(calls), len(errs), tin, flush=True)
