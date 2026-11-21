import os, json, random, sqlite3, textwrap, shutil
B = os.path.abspath(os.environ.get("BLOCKCELL_EVAL_WORKSPACE", os.path.join(os.path.dirname(__file__), "work", "ws")))
def w(task, rel, content):
    p = os.path.join(B, task, rel); os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, "w").write(textwrap.dedent(content).lstrip("\n") if isinstance(content, str) else content)
shutil.rmtree(B, ignore_errors=True); os.makedirs(B)
# t01 bugfix
w("t01", "stats.py", '''
def mean(xs):
    return sum(xs) / len(xs)

def median(xs):
    s = sorted(xs)
    n = len(s)
    return s[n // 2]

def variance(xs):
    m = mean(xs)
    return sum((x - m) ** 2 for x in xs) / len(xs) - 1
''')
w("t01", "test_stats.py", '''
import pytest
from stats import mean, median, variance
def test_mean(): assert mean([1, 2, 3]) == 2
def test_mean_empty():
    with pytest.raises(ValueError): mean([])
def test_median_odd(): assert median([3, 1, 2]) == 2
def test_median_even(): assert median([4, 1, 3, 2]) == 2.5
def test_variance(): assert variance([2, 4, 4, 4, 5, 5, 7, 9]) == 4
''')
# t02 feature
w("t02", "text_utils.py", '''
def slugify(text: str, max_len: int = 50) -> str:
    """把任意标题转成 URL slug。
    规则：
    1. 转小写；去掉首尾空白
    2. 非 [a-z0-9] 的连续字符（含空格、标点、中文等）替换为单个 '-'
    3. 去掉首尾的 '-'
    4. 结果长度不超过 max_len；若截断，不能以 '-' 结尾
    5. 若结果为空，返回 'untitled'
    """
    raise NotImplementedError
''')
# t03 debug from error only
w("t03", "data.txt", "the cat sat on the mat\nThe dog sat on the log\nthe end\n")
w("t03", "run.py", '''
import collections
def top_words(path, k=3):
    counts = {}
    with open(path) as f:
        for line in f:
            for word in line.split(" "):
                counts[word] += 1
    items = sorted(counts.items(), key=lambda x: x[1])
    return items[:k]
if __name__ == "__main__":
    for w, c in top_words("data.txt"):
        print(f"{w} {c}")
''')
# t04 csv
random.seed(7)
regions = ["华东", "华北", "华南", "西部"]; products = ["A100", "B200", "C300", "D400", "E500"]
rows = ["date,region,product,qty,unit_price"]
for i in range(300):
    rows.append(f"2025-{random.randint(1,12):02d}-{random.randint(1,28):02d},{random.choice(regions)},{random.choice(products)},{random.randint(1,20)},{random.choice([9.9,19.5,35,88,120.25])}")
rows.insert(57, "2025-03-05,华东,A100,,19.5")  # dirty row: missing qty
rows.insert(140, "2025-07-09,华南,B200,abc,35")  # dirty row
w("t04", "sales.csv", "\n".join(rows) + "\n")
# t05 log
random.seed(11)
ips = [f"10.0.0.{i}" for i in range(1, 30)]
lines = []
for i in range(1500):
    ip = random.choice(ips[:5]) if random.random() < 0.4 else random.choice(ips)
    st = random.choices([200, 301, 404, 500, 502, 503], [70, 5, 10, 6, 5, 4])[0]
    lines.append(f'{ip} - - [10/Oct/2025:13:{i%60:02d}:{i%60:02d} +0800] "GET /api/v1/item/{i%97} HTTP/1.1" {st} {random.randint(100,5000)} "-" "curl/8.0"')
w("t05", "access.log", "\n".join(lines) + "\n")
# t06 todo scan
w("t06", "proj/app/main.py", "import os\n# TODO: read config from env\ndef main():\n    pass  # FIXME handle errors\n")
w("t06", "proj/app/db.py", "def connect():\n    return None\n\n\n# TODO: add connection pool\n")
w("t06", "proj/lib/util.sh", "#!/bin/sh\necho hi\n# FIXME: quote variables\n")
w("t06", "proj/README.md", "# Proj\nNothing to do here. TODOS are tracked elsewhere.\n")
w("t06", "proj/lib/deep/x.py", "x = 1\n\n\n\n\n\n# TODO(alice): remove x\n")
# t07 refactor
w("t07", "users/repo.py", "_DB = {1: 'alice', 2: 'bob'}\n\ndef get_usr(uid):\n    return _DB.get(uid)\n")
w("t07", "users/service.py", "from users.repo import get_usr\n\ndef greet(uid):\n    name = get_usr(uid)\n    return f'hello {name}' if name else 'unknown'\n")
w("t07", "users/__init__.py", "")
w("t07", "cli.py", "from users import repo\nimport sys\nprint(repo.get_usr(int(sys.argv[1])))\n")
w("t07", "tests/test_users.py", "from users.repo import get_usr\nfrom users.service import greet\n\ndef test_get():\n    assert get_usr(1) == 'alice'\n\ndef test_greet():\n    assert greet(2) == 'hello bob'\n    assert greet(9) == 'unknown'\n")
w("t07", "conftest.py", "")
# t08 write tests
w("t08", "bank.py", '''
class InsufficientFunds(Exception):
    pass

class Account:
    def __init__(self, owner, balance=0):
        if balance < 0:
            raise ValueError("negative opening balance")
        self.owner = owner
        self.balance = balance

    def deposit(self, amount):
        if amount <= 0:
            raise ValueError("amount must be positive")
        self.balance += amount
        return self.balance

    def withdraw(self, amount):
        if amount <= 0:
            raise ValueError("amount must be positive")
        if amount > self.balance:
            raise InsufficientFunds(f"balance {self.balance} < {amount}")
        self.balance -= amount
        return self.balance

    def transfer(self, other, amount):
        self.withdraw(amount)
        other.deposit(amount)
''')
# t10 sqlite
random.seed(3)
db = os.path.join(B, "t10", "shop.db"); os.makedirs(os.path.dirname(db), exist_ok=True)
c = sqlite3.connect(db)
c.execute("create table customers(id integer primary key, name text, city text)")
c.execute("create table orders(id integer primary key, customer_id int, amount real, status text, created_at text)")
names = ["张三","李四","王五","赵六","钱七","孙八","周九","吴十"]
for i, n in enumerate(names, 1): c.execute("insert into customers values(?,?,?)", (i, n, random.choice(["上海","北京","深圳"])))
for i in range(400):
    c.execute("insert into orders(customer_id,amount,status,created_at) values(?,?,?,?)",
      (random.randint(1,8), round(random.uniform(10,900),2), random.choices(["paid","refunded","pending"],[80,10,10])[0],
       f"{random.choice([2024,2025])}-{random.randint(1,12):02d}-{random.randint(1,28):02d}"))
c.commit(); c.close()
# t11 yaml->toml
w("t11", "config.yaml", '''
app:
  name: demo-service
  version: "1.4.2"
  debug: false
server:
  host: 0.0.0.0
  port: 8080
  timeouts: {read: 30, write: 45}
database:
  url: postgres://u:p@db:5432/app
  pool_size: 10
  replicas:
    - host: r1.internal
      weight: 3
    - host: r2.internal
      weight: 1
features: [search, export, "beta-ui"]
ratio: 0.75
''')
os.makedirs(os.path.join(B, "t09"), exist_ok=True); os.makedirs(os.path.join(B, "t12"), exist_ok=True)
os.makedirs(os.path.join(B, "t09b"), exist_ok=True)
print("fixtures ok")
