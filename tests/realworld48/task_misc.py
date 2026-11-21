"""Engineering, document and HTTP tasks. Trusted grader/reference; never copy into agent workspaces."""
from __future__ import annotations
import ast
import contextlib
import csv
import io
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import tokenize
import tomllib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlsplit, parse_qs
from urllib.request import Request, urlopen
from urllib.error import HTTPError

TASKS=[]
def _task(i,title,prompt,protected,output=None):
    TASKS.append(dict(id=f't{i:02}', title=title, category='工程维护' if i<=36 else ('资料整理' if i<=40 else 'HTTP'), difficulty=('easy','easy','medium','medium','hard','hard')[(i-31)%6],prompt=prompt+'\n仅在当前任务目录工作；不要修改受保护输入。所有文本使用 UTF-8。',protected=protected,**({'output':output} if output else {})))

_task(31,'精确提取代码待办','扫描 proj 下所有 .py 文件中的真实 Python 注释，提取含独立单词 TODO 或 FIXME（大小写敏感）的整条注释。排除字符串、README 和 TODOS。输出 result.json 数组，每项 {"path":"相对 proj 路径","line":整数,"comment":"含 # 的注释原文"}，按 path 再 line 升序。',['proj/app.py','proj/db.py','proj/README.md'],'result.json')
_task(32,'重命名用户查询接口','将 users/repo.py 中 get_usr 更名为 get_user，同步 users/service.py、users/__init__.py、cli.py 的导入导出与调用；不得保留旧 Python 接口。保证 greet(1)=="hello alice"、greet(9)=="unknown"、python3 cli.py 1 输出 alice。docs.md 是历史记录，必须原样保留。',['docs.md'])
_task(33,'为账户模块补回归测试','保持 bank.py 不变，编写 test_bank.py，使用 Python 标准库 unittest（无需 pytest）。至少6个测试方法，覆盖存款、取款、余额不足、零/负金额、转账成功和失败后双方余额不变。python3 -m unittest -q test_bank 必须通过。验收会用多种故障变异检查测试实际能发现 bug。',['bank.py'])
_task(34,'提取共享金额格式化函数','invoice.py 的 invoice_total(values) 与 checkout.py 的 checkout_total(values) 含重复金额格式化。提取到新模块 money.py 的 format_money(value)，两个入口导入并实际调用它；不得在原模块保留 Decimal、quantize 或重复格式化。格式化使用 Decimal(str(value)) 与 ROUND_HALF_UP 保留2位小数，入口先求和再舍入，支持空列表与负数。既有 API 不变。',[])
_task(35,'为 CSV 导入器补边界测试','保持 importer.py 不变，编写 test_importer.py，使用标准库 unittest，至少4个测试方法，覆盖 UTF-8 BOM、带逗号及换行的引号字段、qty 无法转换为整数的坏行计数，以及读取完成后资源关闭（patch importer.open 可检查）。python3 -m unittest -q test_importer 必须通过。验收会用行为变异检查测试。',['importer.py'])
_task(36,'迁移便笺到 SQLite','将 notes.py 改为 SQLite 持久化，保留命令：python3 notes.py add "文本"（打印新 id）、list（打印 JSON 数组，每项 id,text）。新增 migrate：把同目录 legacy.json 导入 notes.db，保留 id，重复执行不重复导入；新 id 大于已有最大 id。SQLite notes 表包含 id INTEGER PRIMARY KEY、text TEXT NOT NULL。输入 legacy.json 不可改动；无效记录（缺 id/text 或 id 非整数或 text 非字符串）时整次迁移回滚、退出非零。不得丢失现有笔记。',['legacy.json'])
_task(37,'YAML 迁移为 TOML','把 config.yaml 转成等价 config.toml；字符串、布尔、整数、浮点、数组、嵌套表及表数组类型保持一致。版本和包含引号的字符串不能丢失。仅交付 TOML 即可，可使用任意可用工具。',['config.yaml'],'config.toml')
_task(38,'会议行动项整理','阅读 meeting.txt，输出 result.json：{"actions":[{"task":"原文任务短语","owner":姓名或null,"due":YYYY-MM-DD或null,"evidence":"meeting.txt:L行号"}]}。只提取明确承诺执行的事项，不提取建议和已完成事项。按原文顺序；未明确负责人或日期必须用 null，不根据语境猜测。任务短语不含负责人或日期。',['meeting.txt'],'result.json')
_task(39,'生产配置差异审计','比较 before.json 和 after.json，输出 result.json 数组，按 path 升序；每项 {"path":"/JSON/pointer","kind":"added|removed|changed"}，不输出值。对象递归比较；数组作为整体，顺序变化算 changed；缺失与 null 不等价。路径按 RFC6901 转义 ~ 和 /。包括 token 等敏感字段也仅列路径，禁止泄漏值。',['before.json','after.json'],'result.json')
_task(40,'发布兼容性清单','阅读 CHANGELOG.md、api.md、versions.csv。输出 result.json {"from":"1.4","to":"2.0","breaking":[{"old":旧API,"new":新API或null,"migration":迁移类型rename|remove|type_change,"sources":[引用,...]}]}。只列1.4到2.0升级中明确不兼容项，按old字典序，sources按CHANGELOG.md:L行号、api.md:L行号、versions.csv:L行号顺序；每项引用各文档中对应证据。新增兼容接口不算破坏性。',['CHANGELOG.md','api.md','versions.csv'],'result.json')
_task(41,'仓库元信息查询','通过 HTTP GET {endpoint}/repo 查询仓库元信息。输出 result.json {"stars":整数,"language":字符串,"default_branch":字符串,"source":"/repo"}。必须实际请求，不得猜测。',[],'result.json')
_task(42,'包版本与 Python 约束查询','通过 HTTP GET {endpoint}/package 查询包元信息。仅考虑 releases 中 stable=true 的版本，按数字版本排序选最新；输出 result.json {"version":字符串,"author":原值（允许null）,"requires_python":字符串,"source":"/package"}。必须实际请求。',[],'result.json')
_task(43,'分页工单去重汇总','从 {endpoint}/tickets?page=1 开始按返回 next 相对链接遍历。工单按 id 去重（首次记录优先），id升序。输出 result.json {"tickets":[完整去重工单对象],"counts":{状态:数量}}。必须实际遍历所有页。',[],'result.json')
_task(44,'应对限流导出订单','从 {endpoint}/orders?page=1 拉取全部订单。首次请求会429，遵循 Retry-After（秒），仅针对429/503最多重试3次；随后按 next 分页。输出 result.json 为按id升序的全部订单数组，amount 保持API中的字符串类型。必须实际请求，不能跳过第一页。',[],'result.json')
_task(45,'阅读文档后鉴权查询','先GET {endpoint}/docs 阅读鉴权和分页规则，再取全部客户。仅对本地API使用文档内演示token，禁止把token写入交付文件或日志。输出 result.json 为按 id 升序的完整客户数组。必须实际读文档和鉴权分页。',[],'result.json')
_task(46,'抓取发布公告时间线','抓取 {endpoint}/news 的本站公告链接，相对URL用站点根解析，忽略外站链接，重复公告去重。抓取各公告页面的 version 和 datetime，把时间转换到UTC输出ISO8601（+00:00）。result.json 数组每项 {"version":字符串,"published_at":UTC时间,"source":"站内绝对路径"}，按时间再version升序。必须抓取列表和详情页。',[],'result.json')
_task(47,'同步增量客户与 ETag 缓存','读取 cache.json。先 GET {endpoint}/customers，携带 If-None-Match=cache.etag，正确处理304，随后从 {endpoint}/delta?cursor=cache.cursor 遍历next增量。按id upsert，deleted=true删除；将最终客户按id排序，输出 result.json {"customers":[对象],"cursor":最终cursor,"etag":cache.etag}。输入cache不可修改。必须经历304并完整拉取增量。',['cache.json'],'result.json')
_task(48,'探测依赖故障并保留证据','通过只读GET检查 {endpoint}/health、/deps、/metrics；health首次暂时503，可最多重试3次直到成功，Retry-After秒。结合依赖和指标确定根因。输出 result.json {"reason":"故障依赖名_unavailable","evidence":[{"path":"/health","status":最终HTTP状态码},{"path":"/deps","failed":[失败依赖名按字典序]},{"path":"/metrics","queue_depth":整数}]}。health恢复不能掩盖依赖故障。必须真实请求所有端点。',[],'result.json')

BANK='''class InsufficientFunds(Exception): pass
class Account:
    def __init__(self, owner, balance=0):
        if balance < 0: raise ValueError('negative balance')
        self.owner,self.balance=owner,balance
    def deposit(self, amount):
        if amount <= 0: raise ValueError('positive required')
        self.balance += amount
        return self.balance
    def withdraw(self, amount):
        if amount <= 0: raise ValueError('positive required')
        if amount > self.balance: raise InsufficientFunds()
        self.balance -= amount
        return self.balance
    def transfer(self, other, amount):
        self.withdraw(amount)
        other.deposit(amount)
'''
IMPORTER='''import csv

def load(path):
    rows=[]; skipped=0
    with open(path, encoding='utf-8-sig', newline='') as f:
        for row in csv.DictReader(f):
            try: row['qty']=int(row['qty'])
            except (ValueError, TypeError):
                skipped+=1
                continue
            rows.append(row)
    return rows, skipped
'''
GOLD_CONFIG={'app':{'name':'demo "edge"','version':'1.4.2','debug':False},'server':{'port':8080,'timeouts':{'read':30}},'replicas':[{'host':'r1.internal','weight':3},{'host':'r2.internal','weight':1}],'features':['search','导出'],'ratio':0.75}
MEETING='''2026-10-29 产品例会
张敏：我会在2026-11-02提交支付回归报告。
李明：我负责更新部署手册，日期尚未确定。
建议下周考虑更换图标，暂不安排。
行动项：清理过期演示账号；负责人待定；截止2026-11-05。
王芳：压测已完成，无待办。
'''
DOC_GOLD={'actions':[{'task':'提交支付回归报告','owner':'张敏','due':'2026-11-02','evidence':'meeting.txt:L2'},{'task':'更新部署手册','owner':'李明','due':None,'evidence':'meeting.txt:L3'},{'task':'清理过期演示账号','owner':None,'due':'2026-11-05','evidence':'meeting.txt:L5'}]}
RELEASE_GOLD={'from':'1.4','to':'2.0','breaking':[{'old':'get_usr','new':'get_user','migration':'rename','sources':['CHANGELOG.md:L2','api.md:L2','versions.csv:L2']},{'old':'legacy_export','new':None,'migration':'remove','sources':['CHANGELOG.md:L3','api.md:L3','versions.csv:L3']},{'old':'timeout','new':'timeout_ms','migration':'type_change','sources':['CHANGELOG.md:L4','api.md:L4','versions.csv:L4']}]}


def _write(d,rel,s):
    p=d/rel;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(s,encoding='utf8')
def _json(d,rel,obj):_write(d,rel,json.dumps(obj,ensure_ascii=False,indent=2))
def prepare(tid,d):
    d=Path(d);d.mkdir(parents=True,exist_ok=True)
    if tid=='t31':
        _write(d,'proj/app.py','title = "TODO: not a comment"\n# TODO: load env\ndef main():\n    pass  # FIXME handle errors\n')
        _write(d,'proj/db.py','doc = """# FIXME: example"""\n# TODOS are in tracker\n# TODO(alice): pool connections\n')
        _write(d,'proj/README.md','# TODO catalogue\nTODO: prose only\n')
    elif tid=='t32':
        _write(d,'users/repo.py',"_DB={1:'alice',2:'bob'}\ndef get_usr(uid): return _DB.get(uid)\n")
        _write(d,'users/service.py',"from .repo import get_usr\ndef greet(uid):\n    name=get_usr(uid)\n    return f'hello {name}' if name else 'unknown'\n")
        _write(d,'users/__init__.py','from .repo import get_usr\n')
        _write(d,'cli.py','from users import get_usr\nimport sys\nprint(get_usr(int(sys.argv[1])))\n')
        _write(d,'docs.md','Historical interface: get_usr. Do not rewrite this historical note.\n')
    elif tid=='t33':_write(d,'bank.py',BANK)
    elif tid=='t34':
        for file,fun in [('invoice.py','invoice_total'),('checkout.py','checkout_total')]:
            _write(d,file,f"from decimal import Decimal, ROUND_HALF_UP\ndef {fun}(values):\n    total=sum((Decimal(str(v)) for v in values), Decimal(0))\n    return str(total.quantize(Decimal('0.01'), rounding=ROUND_HALF_UP))\n")
    elif tid=='t35':_write(d,'importer.py',IMPORTER)
    elif tid=='t36':
        _json(d,'legacy.json',[{'id':2,'text':'回访客户'},{'id':7,'text':'发票归档'}])
        _write(d,'notes.py',"import json,sys\nfrom pathlib import Path\np=Path(__file__).with_name('legacy.json')\na=json.loads(p.read_text())\nif sys.argv[1]=='list': print(json.dumps(a,ensure_ascii=False))\nelif sys.argv[1]=='add':\n n=max([r['id'] for r in a],default=0)+1;a.append({'id':n,'text':sys.argv[2]});p.write_text(json.dumps(a));print(n)\n")
    elif tid=='t37':
        _write(d,'config.yaml','''app:
  name: 'demo "edge"'
  version: "1.4.2"
  debug: false
server:
  port: 8080
  timeouts: {read: 30}
replicas:
  - host: r1.internal
    weight: 3
  - host: r2.internal
    weight: 1
features: [search, "导出"]
ratio: 0.75
''')
    elif tid=='t38':_write(d,'meeting.txt',MEETING)
    elif tid=='t39':
        _json(d,'before.json',{'server':{'port':80,'optional':None},'features':['a','b'],'token':'fake-local-old','a/b':{'~mode':1},'same':True})
        _json(d,'after.json',{'server':{'port':8080,'tls':True},'features':['b','a'],'token':'fake-local-new','a/b':{'~mode':2},'same':True,'empty':None})
    elif tid=='t40':
        _write(d,'CHANGELOG.md','# 2.0 changes from 1.4\nget_usr renamed to get_user.\nlegacy_export removed without replacement.\ntimeout seconds changed to timeout_ms integer milliseconds.\nNew optional ping API.\n')
        _write(d,'api.md','# Migration API\nget_user(uid): replacement for get_usr(uid).\nlegacy_export is unavailable in 2.0.\ntimeout_ms: integer, multiply old timeout seconds by 1000.\nping(): additive API.\n')
        _write(d,'versions.csv','old,new,version,compatible\nget_usr,get_user,2.0,false\nlegacy_export,,2.0,false\ntimeout,timeout_ms,2.0,false\n,ping,2.0,true\n')
    elif tid=='t47':_json(d,'cache.json',{'etag':'"customer-v7"','cursor':'7','customers':[{'id':1,'name':'旧名'},{'id':2,'name':'已离职'},{'id':3,'name':'保留'}]})
    spec=next(t for t in TASKS if t['id']==tid)
    _write(d,'SPEC.md',spec['prompt'])


def _exec(d,code,timeout=12):
    # The grader itself never imports the candidate module.
    prefix='import sys;sys.path.insert(0,'+repr(str(Path(d).resolve()))+');'
    try:
        p=subprocess.run([sys.executable,'-I','-c',prefix+code],cwd=d,capture_output=True,text=True,timeout=timeout)
        return p.returncode==0,(p.stdout+p.stderr)[-2000:]
    except subprocess.TimeoutExpired:return False,'candidate verification timed out'

def _run(d,args):
    code='import sys,runpy;sys.path.insert(0,'+repr(str(Path(d).resolve()))+');sys.argv='+repr(args)+';runpy.run_path(sys.argv[0],run_name="__main__")'
    return subprocess.run([sys.executable,'-I','-c',code],cwd=d,capture_output=True,text=True,timeout=12)

def _gold31(d):
    rows=[]
    for p in sorted((d/'proj').rglob('*.py')):
        for tok in tokenize.generate_tokens(io.StringIO(p.read_text()).readline):
            if tok.type==tokenize.COMMENT and re.search(r'\b(?:TODO|FIXME)\b',tok.string):
                rows.append({'path':p.relative_to(d/'proj').as_posix(),'line':tok.start[0],'comment':tok.string})
    return rows

def _diff(a,b,path=''):
    if isinstance(a,dict) and isinstance(b,dict):
        out=[]
        for k in sorted(set(a)|set(b)):
            p=path+'/'+k.replace('~','~0').replace('/','~1')
            if k not in a:out.append({'path':p,'kind':'added'})
            elif k not in b:out.append({'path':p,'kind':'removed'})
            else:out+=_diff(a[k],b[k],p)
        return out
    return [] if type(a)==type(b) and a==b else [{'path':path,'kind':'changed'}]


def _test_suite(d,module,source,mutations,minimum):
    testfile=d/f'test_{module}.py'
    if not testfile.is_file():return False,'missing test module'
    tree=ast.parse(testfile.read_text())
    methods=[n for n in ast.walk(tree) if isinstance(n,(ast.FunctionDef,ast.AsyncFunctionDef)) and n.name.startswith('test_')]
    if len(methods)<minimum:return False,f'need {minimum} test methods'
    # Run in fresh copy; mutations never touch candidate artifacts.
    with tempfile.TemporaryDirectory(prefix='rw48-mutation-') as tmp:
        copy=Path(tmp)/'work';shutil.copytree(d,copy)
        _write(copy,f'{module}.py',source)
        ok,detail=_exec(copy,f'import unittest; r=unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromName("test_{module}")); assert r.testsRun >= {minimum} and r.wasSuccessful()')
        if not ok:return False,'base suite failed: '+detail
        for i,(before,after) in enumerate(mutations):
            assert before in source
            _write(copy,f'{module}.py',source.replace(before,after))
            shutil.rmtree(copy/'__pycache__',ignore_errors=True)
            ok,_=_exec(copy,f'import unittest; r=unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromName("test_{module}")); assert r.testsRun >= {minimum} and r.wasSuccessful()')
            if ok:return False,f'surviving behavioral mutation {i+1}'
        return True,f'{len(methods)} tests; killed {len(mutations)} mutations'


def verify(tid,d):
    d=Path(d)
    try:
        if tid=='t32':
            for rel in ['users/repo.py','users/service.py','users/__init__.py','cli.py']:
                tree=ast.parse((d/rel).read_text())
                for node in ast.walk(tree):
                    if (isinstance(node,ast.Name) and node.id=='get_usr') or (isinstance(node,ast.Attribute) and node.attr=='get_usr') or (isinstance(node,(ast.FunctionDef,ast.AsyncFunctionDef)) and node.name=='get_usr') or (isinstance(node,ast.alias) and (node.name=='get_usr' or node.asname=='get_usr')):return False,'legacy Python interface remains'
            ok,detail=_exec(d,"from users import get_user;from users.service import greet;assert get_user(1)=='alice' and get_user(2)=='bob' and get_user(90) is None;assert greet(1)=='hello alice' and greet(9)=='unknown'")
            p=_run(d,['cli.py','1']);return ok and p.returncode==0 and p.stdout.strip()=='alice',detail+p.stderr
        if tid=='t33':return _test_suite(d,'bank',BANK,[('if amount > self.balance:', 'if amount > self.balance+100:'),('self.balance += amount','self.balance += amount*2'),('self.balance -= amount','self.balance += amount'),('if amount <= 0:', 'if amount < 0:'),('self.withdraw(amount)\n        other.deposit(amount)','pass'),('other.deposit(amount)','other.deposit(amount*2)'),('self.withdraw(amount)\n        other.deposit(amount)','other.balance += 1\n        self.withdraw(amount)\n        other.deposit(amount)')],6)
        if tid=='t34':
            for rel in ['invoice.py','checkout.py']:
                tree=ast.parse((d/rel).read_text())
                imports=[n for n in ast.walk(tree) if (isinstance(n,ast.ImportFrom) and n.module=='money') or (isinstance(n,ast.Import) and any(x.name=='money' for x in n.names))]
                forbidden=any((isinstance(n,ast.Name) and n.id in ('Decimal','ROUND_HALF_UP')) or (isinstance(n,ast.Attribute) and n.attr in ('Decimal','quantize','ROUND_HALF_UP')) or (isinstance(n,ast.alias) and n.name in ('Decimal','ROUND_HALF_UP')) for n in ast.walk(tree))
                if not imports or forbidden:return False,'shared extraction missing'
            return _exec(d,FORMAT_CHECK)
        if tid=='t35':return _test_suite(d,'importer',IMPORTER,[("encoding='utf-8-sig'","encoding='utf-8'"),('csv.DictReader(f)','csv.DictReader(line.replace(\'"\',\'\') for line in f)'),("skipped+=1","skipped+=0"),('return rows, skipped','return rows, skipped+1'),("with open(path, encoding='utf-8-sig', newline='') as f:","f=open(path, encoding='utf-8-sig', newline='')\n    if True:")],4)
        if tid=='t36':return _verify_notes(d)
        if tid=='t37':
            actual=tomllib.loads((d/'config.toml').read_text());return json.dumps(actual,sort_keys=True)==json.dumps(GOLD_CONFIG,sort_keys=True),'typed TOML equivalence'
        actual=json.loads((d/'result.json').read_text())
        if tid=='t31':expected=_gold31(d)
        elif tid=='t38':expected=DOC_GOLD
        elif tid=='t39':expected=_diff(json.loads((d/'before.json').read_text()),json.loads((d/'after.json').read_text()))
        elif tid=='t40':expected=RELEASE_GOLD
        else:
            expected=_http_gold(tid)
            good,why=_audit_ok(tid)
            if not good:return False,why
            if tid=='t45':
                for path in d.rglob('*'):
                    if path.is_file() and not path.is_symlink() and _TOKEN.encode() in path.read_bytes():return False,'demonstration token written to workspace file'
        return json.dumps(actual,sort_keys=True,ensure_ascii=False)==json.dumps(expected,sort_keys=True,ensure_ascii=False),'exact structured result comparison'
    except (OSError,ValueError,KeyError,TypeError,subprocess.TimeoutExpired,SyntaxError,sqlite3.Error) as e:return False,type(e).__name__+': '+str(e)[:250]


def _verify_notes(d):
    with tempfile.TemporaryDirectory(prefix='rw48-notes-') as tmp:
        work=Path(tmp)/'work';shutil.copytree(d,work)
        (work/'notes.db').unlink(missing_ok=True)
        legacy_original=(work/'legacy.json').read_bytes()
        p=_run(work,['notes.py','migrate'])
        if p.returncode:return False,'migrate failed: '+p.stderr
        repeat=_run(work,['notes.py','migrate'])
        if repeat.returncode:return False,'repeat migration failed'
        p=_run(work,['notes.py','list'])
        if p.returncode or json.loads(p.stdout)!=[{'id':2,'text':'回访客户'},{'id':7,'text':'发票归档'}]:return False,'migration rows/idempotence'
        p=_run(work,['notes.py','add','后续跟进'])
        if p.returncode or p.stdout.strip()!='8':return False,'persistent monotonic id'
        p=_run(work,['notes.py','list'])
        if json.loads(p.stdout)[-1]!={'id':8,'text':'后续跟进'}:return False,'new note persisted'
        if (work/'legacy.json').read_bytes()!=legacy_original:return False,'legacy input modified by CLI'
        _json(work,'legacy.json',[{'id':20,'text':'增量\n迁移'}])
        if _run(work,['notes.py','migrate']).returncode or _run(work,['notes.py','migrate']).returncode:return False,'incremental migration failed'
        listed=_run(work,['notes.py','list'])
        if json.loads(listed.stdout)!=[{'id':2,'text':'回访客户'},{'id':7,'text':'发票归档'},{'id':8,'text':'后续跟进'},{'id':20,'text':'增量\n迁移'}]:return False,'alternate migration rows not preserved'
        c=sqlite3.connect(work/'notes.db');cols=c.execute('pragma table_info(notes)').fetchall();before=c.execute('select id,text from notes order by id').fetchall();c.close()
        if not any(x[1]=='id' and x[2].upper()=='INTEGER' and x[5]==1 for x in cols) or not any(x[1]=='text' and x[2].upper()=='TEXT' and x[3]==1 for x in cols):return False,'SQLite schema incorrect'
        _json(work,'legacy.json',[{'id':10,'text':'would insert'},{'id':'bad','text':'invalid'}])
        p=_run(work,['notes.py','migrate'])
        c=sqlite3.connect(work/'notes.db');after=c.execute('select id,text from notes order by id').fetchall();c.close()
        return p.returncode!=0 and before==after,'migration rollback and CLI persistence'

# Service state stays in trusted process memory. No audit or gold files enter workspace.
_AUDITS={}
_CUSTOMERS=[{'id':1,'name':'陈晨'},{'id':2,'name':'R&D, Ltd'},{'id':3,'name':'Müller'}]
_TICKETS=[{'id':1,'status':'open','title':'退款核实'},{'id':2,'status':'closed','title':'发票'},{'id':3,'status':'open','title':'地址修正'},{'id':4,'status':'pending','title':'调查'}]
_ORDERS=[{'id':1,'amount':'10.05'},{'id':2,'amount':'0.00'},{'id':3,'amount':'19.90'},{'id':4,'amount':'100.01'}]
_TOKEN='local-demo-access-48'

def _http_gold(tid):
    return {
        't41':{'stars':417,'language':'Rust','default_branch':'main','source':'/repo'},
        't42':{'version':'2.12.1','author':None,'requires_python':'>=3.10','source':'/package'},
        't43':{'tickets':_TICKETS,'counts':{'open':2,'closed':1,'pending':1}},
        't44':_ORDERS,
        't45':_CUSTOMERS,
        't46':[{'version':'1.9.0','published_at':'2026-10-26T16:30:00+00:00','source':'/news/old'},{'version':'2.0.0','published_at':'2026-10-28T01:00:00+00:00','source':'/news/new'}],
        't47':{'customers':[{'id':1,'name':'新名'},{'id':3,'name':'保留'},{'id':4,'name':'新客户'}],'cursor':'9','etag':'"customer-v7"'},
        't48':{'reason':'database_unavailable','evidence':[{'path':'/health','status':200},{'path':'/deps','failed':['database']},{'path':'/metrics','queue_depth':83}]},
    }[tid]

@contextlib.contextmanager
def service(tid,d):
    audit=[];counts={};_AUDITS[tid]=audit
    class Handler(BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_POST(self):self._send(405,{'error':'read-only'})
        def do_PUT(self):self._send(405,{'error':'read-only'})
        def do_PATCH(self):self._send(405,{'error':'read-only'})
        def do_DELETE(self):self._send(405,{'error':'read-only'})
        def do_GET(self):
            u=urlsplit(self.path);q=parse_qs(u.query);p=u.path
            counts[self.path]=counts.get(self.path,0)+1
            if tid=='t41' and p=='/repo':return self._send(200,{'stargazers_count':417,'language':'Rust','default_branch':'main','forks':26})
            if tid=='t42' and p=='/package':return self._send(200,{'author':None,'requires_python':'>=3.10','releases':[{'version':'2.9.0','stable':True},{'version':'3.0.0rc1','stable':False},{'version':'2.12.1','stable':True},{'version':'2.12.0','stable':True}]})
            if tid=='t43' and p=='/tickets':
                page=q.get('page',['1'])[0]
                if page=='1':return self._send(200,{'items':_TICKETS[:2],'next':'/tickets?page=2'})
                if page=='2':return self._send(200,{'items':[_TICKETS[1],_TICKETS[2]],'next':'/tickets?page=3'})
                if page=='3':return self._send(200,{'items':[_TICKETS[3]],'next':None})
            if tid=='t44' and p=='/orders':
                page=q.get('page',['1'])[0]
                if page=='1' and counts[self.path]==1:return self._send(429,{'error':'rate limited'},{'Retry-After':'1'})
                if page=='1':return self._send(200,{'items':_ORDERS[:2],'next':'/orders?page=2'})
                if page=='2':return self._send(200,{'items':_ORDERS[2:],'next':None})
            if tid=='t45':
                if p=='/docs':return self._send(200,{'method':'GET','path':'/clients','auth_header':'X-Demo-Token','token':_TOKEN,'pagination':'Follow next relative URL until null.'})
                if p=='/clients':
                    if self.headers.get('X-Demo-Token')!=_TOKEN:return self._send(401,{'error':'bad token'})
                    if q.get('cursor',[''])[0]=='c2':return self._send(200,{'items':_CUSTOMERS[2:],'next':None})
                    return self._send(200,{'items':_CUSTOMERS[:2],'next':'/clients?cursor=c2'})
            if tid=='t46':
                pages={'/news':'<html><a href="/news/new">2.0</a><a href="news/old">1.9</a><a href="/news/new">duplicate</a><a href="https://invalid.example/offsite">offsite</a></html>','/news/new':'<article><h1>2.0.0</h1><time datetime="2026-10-28T09:00:00+08:00">Release</time></article>','/news/old':'<article><h1>1.9.0</h1><time datetime="2026-10-27T00:30:00+08:00">Release</time></article>'}
                if p in pages:return self._send(200,pages[p],html=True)
            if tid=='t47':
                if p=='/customers':
                    if self.headers.get('If-None-Match')=='"customer-v7"':return self._send(304,None,{'ETag':'"customer-v7"'})
                    return self._send(200,{'customers':_http_gold(tid)['customers']},{'ETag':'"customer-v9"'})
                if p=='/delta':
                    cursor=q.get('cursor',[''])[0]
                    if cursor=='7':return self._send(200,{'changes':[{'id':1,'name':'新名'},{'id':2,'deleted':True}],'cursor':'8','next':'/delta?cursor=8'})
                    if cursor=='8':return self._send(200,{'changes':[{'id':4,'name':'新客户'},{'id':1,'name':'新名'}],'cursor':'9','next':None})
            if tid=='t48':
                if p=='/health':return self._send(503,{'status':'warming'},{'Retry-After':'1'}) if counts[self.path]==1 else self._send(200,{'status':'ready'})
                if p=='/deps':return self._send(200,{'database':'down','cache':'up','search':'up'})
                if p=='/metrics':return self._send(200,{'queue_depth':83,'db_errors':42,'http_errors':1})
            return self._send(404,{'error':'unknown route'})
        def _send(self,status,obj,headers=None,html=False):
            # Audit stores routes/status only, never demonstration or external credentials.
            audit.append({'path':self.path,'status':status,'method':self.command,'monotonic':time.monotonic()})
            raw=(obj if html else json.dumps(obj,ensure_ascii=False)).encode('utf8') if obj is not None else b''
            self.send_response(status);self.send_header('Content-Type','text/html; charset=utf-8' if html else 'application/json')
            self.send_header('Content-Length',str(len(raw)))
            for k,v in (headers or {}).items():self.send_header(k,v)
            self.end_headers()
            if raw:self.wfile.write(raw)
    server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    try:yield f'http://127.0.0.1:{server.server_port}'
    finally:server.shutdown();server.server_close();thread.join(timeout=3)

def _audit_ok(tid):
    rows=_AUDITS.get(tid,[])
    seen={(x['path'],x['status']) for x in rows}
    required={
        't41':{('/repo',200)},'t42':{('/package',200)},
        't43':{('/tickets?page=1',200),('/tickets?page=2',200),('/tickets?page=3',200)},
        't44':{('/orders?page=1',429),('/orders?page=1',200),('/orders?page=2',200)},
        't45':{('/docs',200),('/clients',200),('/clients?cursor=c2',200)},
        't46':{('/news',200),('/news/new',200),('/news/old',200)},
        't47':{('/customers',304),('/delta?cursor=7',200),('/delta?cursor=8',200)},
        't48':{('/health',503),('/health',200),('/deps',200),('/metrics',200)},
    }[tid]
    if not required<=seen:return False,'required HTTP evidence missing: '+repr(sorted(required-seen))
    if any(x.get('method')!='GET' for x in rows):return False,'only GET requests permitted'
    if tid in ('t44','t48'):
        path='/orders?page=1' if tid=='t44' else '/health'
        requests=[x for x in rows if x['path']==path]
        if len(requests)>4:return False,'retry limit exceeded'
        for first,second in zip(requests,requests[1:]):
            if first['status'] in (429,503) and second['monotonic']-first['monotonic']<0.90:return False,'Retry-After ignored'
    if tid=='t45':
        doc=next(i for i,r in enumerate(rows) if r['path']=='/docs' and r['status']==200)
        client=next(i for i,r in enumerate(rows) if r['path'].startswith('/clients') and r['status']==200)
        if doc>=client:return False,'documentation must be read before authenticated clients request'
    return True,'HTTP route, status, retry and method audit passed' 


def solve(tid,d):
    d=Path(d)
    if tid=='t31':_json(d,'result.json',_gold31(d))
    elif tid=='t32':
        for rel in ['users/repo.py','users/service.py','users/__init__.py','cli.py']:_write(d,rel,(d/rel).read_text().replace('get_usr','get_user'))
    elif tid=='t33':_write(d,'test_bank.py',BANK_TESTS)
    elif tid=='t34':
        _write(d,'money.py',"from decimal import Decimal, ROUND_HALF_UP\ndef sum_money(values): return sum((Decimal(str(v)) for v in values),Decimal(0))\ndef format_money(value): return str(Decimal(str(value)).quantize(Decimal('0.01'),rounding=ROUND_HALF_UP))\n")
        for rel,name in [('invoice.py','invoice_total'),('checkout.py','checkout_total')]:_write(d,rel,f"from money import format_money, sum_money\ndef {name}(values):\n    return format_money(sum_money(values))\n")
    elif tid=='t35':_write(d,'test_importer.py',IMPORTER_TESTS)
    elif tid=='t36':_write(d,'notes.py',NOTES_REFERENCE)
    elif tid=='t37':_write(d,'config.toml','''features = ["search", "导出"]
ratio = 0.75
[app]
name = 'demo "edge"'
version = "1.4.2"
debug = false
[server]
port = 8080
[server.timeouts]
read = 30
[[replicas]]
host = "r1.internal"
weight = 3
[[replicas]]
host = "r2.internal"
weight = 1
''')
    elif tid=='t38':_json(d,'result.json',DOC_GOLD)
    elif tid=='t39':_json(d,'result.json',_diff(json.loads((d/'before.json').read_text()),json.loads((d/'after.json').read_text())))
    elif tid=='t40':_json(d,'result.json',RELEASE_GOLD)
    elif os.environ.get('BLOCKCELL_EVAL_ENDPOINT'):
        _solve_http(tid,d,os.environ['BLOCKCELL_EVAL_ENDPOINT'])
    else:
        with service(tid,d) as endpoint:_solve_http(tid,d,endpoint)


def _solve_http(tid,d,endpoint):
    import time
    from datetime import datetime,timezone
    def get(path,headers=None,raw=False):
        from http.client import HTTPConnection
        host=urlsplit(endpoint)
        for attempt in range(4):
            conn=HTTPConnection(host.hostname,host.port,timeout=5)
            try:
                conn.request('GET',path,headers=headers or {})
                r=conn.getresponse();body=r.read().decode('utf8');status=r.status
                if status==304:return 304,None
                if status in (429,503) and attempt<3:
                    time.sleep(float(r.getheader('Retry-After','0')));continue
                if status!=200:raise ValueError(f'HTTP status {status}')
                return status,body if raw else json.loads(body)
            finally:conn.close()
    if tid=='t41':
        _,a=get('/repo');out={'stars':a['stargazers_count'],'language':a['language'],'default_branch':a['default_branch'],'source':'/repo'}
    elif tid=='t42':
        _,a=get('/package');latest=max((x['version'] for x in a['releases'] if x['stable']),key=lambda x:tuple(map(int,x.split('.'))));out={'version':latest,'author':a['author'],'requires_python':a['requires_python'],'source':'/package'}
    elif tid in ('t43','t44','t45'):
        headers={};p='/tickets?page=1' if tid=='t43' else '/orders?page=1'
        if tid=='t45':
            _,doc=get('/docs');headers={doc['auth_header']:doc['token']};p=doc['path']
        byid={}
        while p:
            _,a=get(p,headers)
            for row in a['items']:byid.setdefault(row['id'],row)
            p=a['next']
        items=sorted(byid.values(),key=lambda r:r['id'])
        if tid=='t43':
            from collections import Counter
            out={'tickets':items,'counts':dict(Counter(r['status'] for r in items))}
        else:out=items
    elif tid=='t46':
        _,html=get('/news',raw=True);paths=set(re.findall(r'href="([^"]+)"',html));out=[]
        for p in paths:
            if '://' in p:continue
            p='/'+p.lstrip('/')
            _,page=get(p,raw=True);v=re.search(r'<h1>([^<]+)</h1>',page).group(1);dt=re.search(r'datetime="([^"]+)"',page).group(1)
            out.append({'version':v,'published_at':datetime.fromisoformat(dt).astimezone(timezone.utc).isoformat(),'source':p})
        out.sort(key=lambda r:(r['published_at'],r['version']))
    elif tid=='t47':
        cache=json.loads((d/'cache.json').read_text());status,_=get('/customers',{'If-None-Match':cache['etag']});assert status==304
        byid={r['id']:r for r in cache['customers']};p='/delta?cursor='+cache['cursor'];cursor=cache['cursor']
        while p:
            _,a=get(p)
            for row in a['changes']:
                if row.get('deleted'):byid.pop(row['id'],None)
                else:byid[row['id']]=row
            cursor=a['cursor'];p=a['next']
        out={'customers':sorted(byid.values(),key=lambda r:r['id']),'cursor':cursor,'etag':cache['etag']}
    else:
        status,_=get('/health');_,deps=get('/deps');_,metrics=get('/metrics');failed=sorted(k for k,v in deps.items() if v!='up');out={'reason':failed[0]+'_unavailable','evidence':[{'path':'/health','status':status},{'path':'/deps','failed':failed},{'path':'/metrics','queue_depth':metrics['queue_depth']}]}
    _json(d,'result.json',out)


FORMAT_CHECK = """import money, invoice, checkout
from unittest.mock import patch, Mock
from contextlib import ExitStack
assert money.format_money('2.675')=='2.68'
assert money.format_money('-1.005')=='-1.01'
assert money.format_money(0)=='0.00'
assert invoice.invoice_total([])=='0.00'
assert invoice.invoice_total(['1.005','1.005'])=='2.01'
assert checkout.checkout_total(['-1.005','0'])=='-1.01'
assert invoice.invoice_total(['999999999999999.99','0.01'])=='1000000000000000.00'
for mod,fn in [(invoice,invoice.invoice_total),(checkout,checkout.checkout_total)]:
    original=money.format_money
    aliases=[name for name,value in vars(mod).items() if value is original]
    probe=Mock(return_value='probe')
    with ExitStack() as stack:
        stack.enter_context(patch.object(money,'format_money',probe))
        for name in aliases:stack.enter_context(patch.object(mod,name,probe))
        assert fn([1,2])=='probe'
        probe.assert_called_once()
"""


BANK_TESTS='''import unittest
from bank import Account, InsufficientFunds
class BankTests(unittest.TestCase):
    def test_deposit(self):
        a=Account('a',10);self.assertEqual(a.deposit(5),15)
    def test_withdraw(self):
        a=Account('a',10);self.assertEqual(a.withdraw(3),7)
    def test_insufficient(self):
        a=Account('a',10)
        with self.assertRaises(InsufficientFunds):a.withdraw(11)
        self.assertEqual(a.balance,10)
    def test_zero(self):
        for op in ['deposit','withdraw']:
            with self.assertRaises(ValueError):getattr(Account('a'),op)(0)
    def test_negative(self):
        for op in ['deposit','withdraw']:
            with self.assertRaises(ValueError):getattr(Account('a'),op)(-2)
    def test_transfer(self):
        a,b=Account('a',20),Account('b',3);a.transfer(b,7);self.assertEqual((a.balance,b.balance),(13,10))
    def test_failed_transfer(self):
        a,b=Account('a',20),Account('b',3)
        with self.assertRaises(InsufficientFunds):a.transfer(b,21)
        self.assertEqual((a.balance,b.balance),(20,3))
'''
IMPORTER_TESTS='''import io, tempfile, unittest
from pathlib import Path
from unittest.mock import patch
import importer
class ImportTests(unittest.TestCase):
    def load_text(self,text):
        with tempfile.TemporaryDirectory() as d:
            p=Path(d)/'input.csv';p.write_text(text,encoding='utf8');return importer.load(p)
    def test_bom(self):
        rows,n=self.load_text('\\ufeffname,qty\\n甲,2\\n');self.assertEqual((rows,n),([{'name':'甲','qty':2}],0))
    def test_quotes(self):
        rows,n=self.load_text('name,qty\\n"ACME, Ltd",3\\n"two\\nlines",4\\n');self.assertEqual(rows,[{'name':'ACME, Ltd','qty':3},{'name':'two\\nlines','qty':4}]);self.assertEqual(n,0)
    def test_bad(self):
        rows,n=self.load_text('name,qty\\na,no\\nb,4\\n');self.assertEqual((rows,n),([{'name':'b','qty':4}],1))
    def test_closed(self):
        f=io.StringIO('name,qty\\na,1\\n')
        with patch('importer.open',return_value=f):self.assertEqual(importer.load('unused')[1],0)
        self.assertTrue(f.closed)
'''
NOTES_REFERENCE='''import json, sqlite3, sys
from pathlib import Path
root=Path(__file__).resolve().parent
c=sqlite3.connect(root/'notes.db')
c.execute('CREATE TABLE IF NOT EXISTS notes(id INTEGER PRIMARY KEY,text TEXT NOT NULL)')
try:
    cmd=sys.argv[1]
    if cmd=='migrate':
        rows=json.loads((root/'legacy.json').read_text(encoding='utf8'))
        with c:
            for r in rows:
                if not isinstance(r,dict) or type(r.get('id')) is not int or not isinstance(r.get('text'),str):raise ValueError('invalid legacy row')
                c.execute('INSERT OR IGNORE INTO notes VALUES (?,?)',(r['id'],r['text']))
    elif cmd=='add':
        with c:
            uid=c.execute('SELECT coalesce(max(id),0)+1 FROM notes').fetchone()[0]
            c.execute('INSERT INTO notes VALUES (?,?)',(uid,sys.argv[2]))
        print(uid)
    elif cmd=='list':print(json.dumps([dict(id=i,text=t) for i,t in c.execute('SELECT id,text FROM notes ORDER BY id')],ensure_ascii=False))
    else:raise ValueError('unknown command')
finally:c.close()
'''
