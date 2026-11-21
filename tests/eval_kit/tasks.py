import os, json, csv, subprocess, sqlite3, re, collections, sys
B = os.path.abspath(os.environ.get("BLOCKCELL_EVAL_WORKSPACE", os.path.join(os.path.dirname(__file__), "work", "ws")))
def sh(cmd, cwd):
    return subprocess.run(cmd, shell=True, cwd=cwd, capture_output=True, text=True, timeout=120)
TASKS = {}
def task(tid, cat, prompt):
    def deco(f):
        TASKS[tid] = dict(cat=cat, prompt=prompt.replace("{D}", f"{B}/{tid}"), verify=f); return f
    return deco

@task("t01", "代码修复", "目录 {D} 里有 stats.py 和 test_stats.py，运行 `python3 -m pytest -q` 有失败用例。请修复 stats.py（不要修改测试），让全部测试通过，并告诉我修了哪些 bug。")
def v01(d):
    r = sh("python3 -m pytest -q test_stats.py", d)
    return r.returncode == 0, (r.stdout + r.stderr)[-400:]

@task("t02", "功能实现", "请在 {D}/text_utils.py 中按 docstring 规则实现 slugify 函数（保持函数签名不变），并自己写几个用例验证。")
def v02(d):
    sys.path.insert(0, d)
    try:
        import importlib; m = importlib.import_module("text_utils"); importlib.reload(m)
        cases = [("Hello, World!", 50, "hello-world"), ("  Rust 与 Python 对比  ", 50, "rust-python"), ("你好", 50, "untitled"),
                 ("a--b__c", 50, "a-b-c"), ("abc def ghi", 7, "abc-def"), ("---", 50, "untitled"), ("Version 2.0 Released", 50, "version-2-0-released")]
        bad = [(c, m.slugify(c[0], c[1])) for c in cases if m.slugify(c[0], c[1]) != c[2]]
        return not bad, str(bad)[:200]
    except Exception as e: return False, repr(e)[:200]
    finally: sys.path.remove(d)

@task("t03", "报错排查", "在 {D} 下运行 `python3 run.py` 会报错。请修复它，使其正确输出 data.txt 中出现次数最多的前 3 个单词及次数（不区分大小写，按次数降序，每行 `单词 次数`）。")
def v03(d):
    r = sh("python3 run.py", d); lines = r.stdout.strip().splitlines()
    ok = r.returncode == 0 and len(lines) == 3 and lines[0].split() == ["the", "5"] and lines[1].split()[1] == "2" and lines[2].split()[1] == "2"
    return ok, (r.stdout + r.stderr)[-200:]

@task("t04", "数据分析", "{D}/sales.csv 是销售明细（revenue = qty * unit_price）。请统计 2025 年各 region 的总销售额（保留 2 位小数）以及销售额最高的 product，结果写入 {D}/report.json，格式：{\"by_region\": {\"华东\": 123.45, ...}, \"top_product\": \"A100\", \"skipped_rows\": 脏数据行数}。注意数据中可能有脏行。")
def v04(d):
    by = collections.defaultdict(float); prod = collections.defaultdict(float); skipped = 0
    for row in csv.DictReader(open(f"{d}/sales.csv")):
        try: q = int(row["qty"]); p = float(row["unit_price"])
        except: skipped += 1; continue
        by[row["region"]] += q * p; prod[row["product"]] += q * p
    try: rep = json.load(open(f"{d}/report.json"))
    except Exception as e: return False, "no report: " + repr(e)[:100]
    ok = all(abs(rep["by_region"].get(k, -1) - round(v, 2)) < 0.02 for k, v in by.items()) and rep["top_product"] == max(prod, key=prod.get) and int(rep.get("skipped_rows", -1)) == skipped
    return ok, json.dumps(rep, ensure_ascii=False)[:200]

@task("t05", "日志分析", "分析 {D}/access.log（nginx 访问日志），找出请求量最多的前 3 个 IP 及其请求数，以及状态码为 5xx 的请求总数，写入 {D}/answer.json，格式：{\"top_ips\": [[\"ip\", 次数], ...], \"count_5xx\": N}。")
def v05(d):
    ipc = collections.Counter(); n5 = 0
    for l in open(f"{d}/access.log"):
        ipc[l.split()[0]] += 1; n5 += l.split('"')[2].split()[0].startswith("5")
    try: a = json.load(open(f"{d}/answer.json"))
    except Exception as e: return False, repr(e)[:100]
    ok = [list(x) for x in a["top_ips"]] == [list(x) for x in ipc.most_common(3)] and a["count_5xx"] == n5
    return ok, json.dumps(a)[:200]

@task("t06", "代码检索", "扫描 {D}/proj 目录下所有文件中的 TODO 和 FIXME 注释（README 中的普通单词 TODOS 不算），在 {D}/todos.md 中逐行列出，每行格式：`相对路径:行号: 注释内容`。")
def v06(d):
    try: txt = open(f"{d}/todos.md").read()
    except Exception as e: return False, repr(e)[:100]
    need = ["app/main.py:2", "app/main.py:4", "app/db.py:5", "lib/util.sh:3", "lib/deep/x.py:7"]
    miss = [n for n in need if n not in txt]; fp = "README" in txt
    return not miss and not fp, f"miss={miss} readme_fp={fp}"

@task("t07", "重构", "在 {D} 项目中把函数 get_usr 重命名为 get_user，所有调用处（包括 cli.py 和测试）都要同步更新，最后确保 `python3 -m pytest -q` 通过且 `python3 cli.py 1` 输出 alice。")
def v07(d):
    r = sh("python3 -m pytest -q", d); r2 = sh("python3 cli.py 1", d); g = sh("grep -rn get_usr --include=*.py .", d)
    return r.returncode == 0 and r2.stdout.strip() == "alice" and not g.stdout.strip(), (r.stdout[-80:] + "|" + r2.stdout + "|" + g.stdout[:80])

@task("t08", "编写测试", "为 {D}/bank.py 编写 pytest 单元测试，保存为 {D}/test_bank.py，至少 6 个测试，覆盖存款、取款、余额不足、非法金额、转账等场景，并运行确认全部通过。")
def v08(d):
    r = sh("python3 -m pytest -q test_bank.py", d)
    m = re.search(r"(\d+) passed", r.stdout); n = int(m.group(1)) if m else 0
    src = open(f"{d}/bank.py").read()
    open(f"{d}/bank.py", "w").write(src.replace("if amount > self.balance:", "if amount > self.balance + 100:"))
    r2 = sh("python3 -m pytest -q test_bank.py", d); open(f"{d}/bank.py", "w").write(src)
    return r.returncode == 0 and n >= 6 and r2.returncode != 0, f"passed={n} mutant_killed={r2.returncode!=0}"

@task("t09", "联网检索", "查询 GitHub 仓库 blockcell-labs/blockcell 当前的 star 数、主要编程语言和默认分支，写入 {D}/repo.json，格式：{\"stars\": N, \"language\": \"...\", \"default_branch\": \"...\"}。")
def v09(d):
    try: a = json.load(open(f"{d}/repo.json"))
    except Exception as e: return False, repr(e)[:100]
    t = json.loads(subprocess.run("curl --fail --silent --show-error --max-time 30 https://api.github.com/repos/blockcell-labs/blockcell", shell=True, capture_output=True, text=True).stdout)
    ok = abs(int(a["stars"]) - t["stargazers_count"]) <= max(5, t["stargazers_count"] * 0.05) and a["language"] == t["language"] and a["default_branch"] == t["default_branch"]
    return ok, f"agent={a} truth={t['stargazers_count']},{t['language']},{t['default_branch']}"

@task("t10", "SQL查询", "{D}/shop.db 是一个 SQLite 数据库（机器上没有 sqlite3 命令行，可以用 python）。请找出 2025 年已支付（status='paid'）订单总金额最高的前 3 位客户，结果写入 {D}/result.csv，表头为 name,total（total 保留 2 位小数，按 total 降序）。")
def v10(d):
    c = sqlite3.connect(f"{d}/shop.db")
    truth = c.execute("select c.name, round(sum(o.amount),2) t from orders o join customers c on c.id=o.customer_id where o.status='paid' and o.created_at like '2025%' group by c.id order by t desc limit 3").fetchall()
    try: rows = list(csv.DictReader(open(f"{d}/result.csv")))
    except Exception as e: return False, repr(e)[:100]
    ok = len(rows) == 3 and all(r["name"] == t[0] and abs(float(r["total"]) - t[1]) < 0.02 for r, t in zip(rows, truth))
    return ok, f"agent={rows} truth={truth}"[:200]

@task("t11", "格式转换", "把 {D}/config.yaml 转换为等价的 TOML 文件 {D}/config.toml，所有键、值和类型（字符串/整数/布尔/浮点/数组/表数组）都要保持一致。")
def v11(d):
    import yaml, toml
    try: a = toml.load(f"{d}/config.toml")
    except Exception as e: return False, repr(e)[:150]
    y = yaml.safe_load(open(f"{d}/config.yaml"))
    return a == y, "" if a == y else f"diff toml={str(a)[:150]}"

@task("t12", "从零开发", "在 {D} 下用 Python 标准库写一个命令行待办工具 todo.py，数据保存在同目录 todo.json。支持：`python3 todo.py add \"文本\"`（新增，打印新任务 id）、`python3 todo.py list`（每行 `id [ ] 文本` 或 `id [x] 文本`）、`python3 todo.py done <id>`（标记完成）。id 从 1 开始自增。写好后自己测试一下。")
def v12(d):
    sh("rm -f todo.json", d)
    a = sh('python3 todo.py add "买牛奶"', d); b = sh('python3 todo.py add "写周报"', d); c = sh("python3 todo.py done 2", d); l = sh("python3 todo.py list", d)
    lines = [x.strip() for x in l.stdout.strip().splitlines()]
    ok = a.returncode == 0 and "1" in a.stdout and len(lines) == 2 and lines[0].startswith("1") and "[ ]" in lines[0] and "买牛奶" in lines[0] and lines[1].startswith("2") and "[x]" in lines[1]
    return ok, l.stdout[:150] + l.stderr[-100:]

@task("t09b", "联网检索", "查询 PyPI 上 Python 包 requests 当前的最新版本号、作者(author) 和要求的最低 Python 版本(requires_python)，author 必须保留 API 原值（允许 null），不要用 author_email 替代，写入 {D}/pkg.json，格式：{\"version\": \"...\", \"author\": \"...\", \"requires_python\": \"...\"}。")
def v09b(d):
    try: a = json.load(open(f"{d}/pkg.json"))
    except Exception as e: return False, repr(e)[:100]
    t = json.loads(subprocess.run("curl --fail --silent --show-error --max-time 30 https://pypi.org/pypi/requests/json", shell=True, capture_output=True, text=True).stdout)["info"]
    ok = a.get("version") == t["version"] and a.get("requires_python") == t["requires_python"] and "author" in a and a["author"] == t["author"]
    return ok, f"agent={a} truth={t['version']},{t['author']},{t['requires_python']}"
