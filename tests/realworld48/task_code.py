"""Twelve deterministic code tasks. Graders/reference code stay outside workspaces.

Each checker executes submitted code in a fresh subprocess and disposable copy.
This is test isolation, not a security sandbox for actively hostile programs.
"""
from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap

TASKS: list[dict] = []
_CASES: dict[str, dict] = {}


def _add(tid, title, filename, prompt, starter, reference, check, inputs=None):
    inputs = inputs or {}
    num = int(tid[1:])
    prompt = ('在当前任务目录完成下列工作。只使用 Python 3.11+ 标准库。不得修改 SPEC.md 或题目提供的数据/测试文件；'
              '允许增加自己的测试。保持指定函数签名/命令接口。\n\n' + prompt)
    TASKS.append(dict(id=tid, category='代码修复' if num <= 6 else '功能实现',
                      difficulty=['easy', 'easy', 'medium', 'medium', 'hard', 'hard'][(num-1) % 6],
                      title=title, prompt=prompt, protected=['SPEC.md', *inputs], output=filename))
    _CASES[tid] = dict(filename=filename, starter=textwrap.dedent(starter).lstrip('\n'),
                       reference=textwrap.dedent(reference).lstrip('\n'),
                       check=textwrap.dedent(check).lstrip('\n'), inputs=inputs, prompt=prompt)


_add('t01', '修复运营统计函数', 'stats.py', '''修复 stats.py 中 mean(xs)、median(xs)、variance(xs)。xs 为有限数字列表；三个函数都应在空列表时抛出 ValueError。
mean 是算术平均数，median 为排序后的中位数（偶数项取中间两项平均），variance 为总体方差（除以 N）。不修改输入列表。
运行现有 test_stats.py 并自行补充边界验证。''', r'''
def mean(xs): return sum(xs) / len(xs)
def median(xs): return sorted(xs)[len(xs)//2]
def variance(xs): return sum((x-mean(xs))**2 for x in xs) / len(xs)-1
''', r'''
def _nonempty(xs):
    if not xs: raise ValueError('empty sample')
def mean(xs):
    _nonempty(xs)
    return sum(xs)/len(xs)
def median(xs):
    _nonempty(xs)
    s=sorted(xs); n=len(s)
    return s[n//2] if n%2 else (s[n//2-1]+s[n//2])/2
def variance(xs):
    m=mean(xs)
    return sum((x-m)**2 for x in xs)/len(xs)
''', r'''
import stats, unittest, math
r = unittest.TextTestRunner().run(unittest.defaultTestLoader.discover('.', pattern='test_stats.py'))
assert r.wasSuccessful()
for xs in ([9],[-7,-2,1,8],[2.5,3.5], [7,1,9,3,5], [0,0,0], [-100,100]):
    orig=xs[:]; ordered=sorted(xs); n=len(xs)
    assert math.isclose(stats.mean(xs),sum(xs)/n,abs_tol=1e-10)
    expected=ordered[n//2] if n%2 else (ordered[n//2-1]+ordered[n//2])/2
    assert math.isclose(stats.median(xs), expected,abs_tol=1e-10)
    assert math.isclose(stats.variance(xs),sum((x-sum(xs)/n)**2 for x in xs)/n,abs_tol=1e-10)
    assert xs == orig
for f in [stats.mean,stats.median,stats.variance]:
    try: f([])
    except ValueError: pass
    else: raise AssertionError('empty input must raise ValueError')
''', {'test_stats.py': '''import unittest
from stats import mean, median, variance
class StatsTests(unittest.TestCase):
    def test_mean(self): self.assertEqual(mean([1,2,3]),2)
    def test_empty(self):
        with self.assertRaises(ValueError): mean([])
    def test_even_median(self): self.assertEqual(median([4,1,3,2]),2.5)
    def test_variance(self): self.assertEqual(variance([2,4,4,4,5,5,7,9]),4)
if __name__ == '__main__': unittest.main()
'''})

_add('t02', '修复业务文本词频脚本', 'run.py', '''修复 run.py 的 top_words(path, k=3)，并使 `python3 run.py` 分析同目录 data.txt，每行输出 `word count`。
单词只由 ASCII 字母 A-Z/a-z 构成，不区分大小写；标点、数字、空白及非 ASCII 字符均作为分隔符。按次数降序，次数相同按单词字典序升序，最多 k 个，返回 [(word,count),...]。
空文件返回 []；k=0 返回 []；负数 k 抛 ValueError。path 是 UTF-8 文本。''', r'''
def top_words(path, k=3):
    counts={}
    for word in open(path).read().split(' '): counts[word]=counts.get(word,0)+1
    return sorted(counts.items(),key=lambda p:p[1])[:k]
if __name__=='__main__':
    for word,n in top_words('data.txt'): print(word,n)
''', r'''
import collections
import re
def top_words(path, k=3):
    if k<0: raise ValueError('negative k')
    with open(path,encoding='utf-8') as f: c=collections.Counter(word.lower() for word in re.findall(r'[A-Za-z]+',f.read()))
    return sorted(c.items(),key=lambda item:(-item[1],item[0]))[:k]
if __name__=='__main__':
    for word,n in top_words('data.txt'): print(word,n)
''', r'''
from pathlib import Path
import run,subprocess,sys
assert run.top_words('data.txt') == [('the',5),('on',2),('sat',2)]
p=subprocess.run([sys.executable,'run.py'],text=True,capture_output=True)
assert p.returncode==0 and p.stdout.splitlines()==['the 5','on 2','sat 2'],(p.stdout,p.stderr)
for text,k,expected in [('Beta,alpha! BETA;Alpha? cat123cat 中文',3,[('alpha',2),('beta',2),('cat',2)]),('',3,[]),('x x y',0,[]),('a B a',8,[('a',2),('b',1)]),('KİA KİB',3,[('a',1),('b',1)])]:
    Path('extra.txt').write_text(text,encoding='utf-8')
    assert run.top_words('extra.txt',k)==expected
try: run.top_words('data.txt',-1)
except ValueError: pass
else: raise AssertionError('negative k')
''', {'data.txt':'the cat sat on the mat\nThe dog sat on the log\nthe end\n'})

_add('t03', '修复逐行折扣账单计算', 'billing.py', '''修复 billing.py 的 invoice_total(lines, discount='0', tax='0')，返回两位小数的金额字符串。
lines 为字典列表，每项为 {'qty':非负整数,'unit_price':非负十进制数字符串}；qty 不接受 bool。discount 和 tax 是闭区间 [0,1] 的十进制数字符串，均须有限。
先逐行计算 qty*unit_price*(1-discount)，用 Decimal ROUND_HALF_UP 舍入至分；再对该舍入后的每行净额计算税额并同法舍入；最后累加所有行净额和税额。空账单返回 '0.00'。
所有非法数量/价格/费率（包括缺键、NaN、Infinity）抛 ValueError，不修改输入。''', r'''
def invoice_total(lines, discount='0', tax='0'):
    return str(round(sum(x['qty']*float(x['unit_price']) for x in lines)*(1-float(discount))*(1+float(tax)),2))
''', r'''
from decimal import Decimal, InvalidOperation, ROUND_HALF_UP

def _decimal(value):
    if not isinstance(value,str): raise ValueError('decimal string required')
    try: n=Decimal(value)
    except (InvalidOperation,ValueError): raise ValueError('invalid decimal') from None
    if not n.is_finite() or n<0: raise ValueError('invalid decimal')
    return n

def invoice_total(lines, discount='0', tax='0'):
    discount=_decimal(discount); tax=_decimal(tax)
    if discount>1 or tax>1: raise ValueError('rate out of range')
    cent=Decimal('0.01'); total=Decimal(0)
    for line in lines:
        try: qty=line['qty']; price=_decimal(line['unit_price'])
        except (KeyError,TypeError): raise ValueError('invalid line') from None
        if type(qty) is not int or qty<0: raise ValueError('invalid quantity')
        net=(qty*price*(1-discount)).quantize(cent,rounding=ROUND_HALF_UP)
        vat=(net*tax).quantize(cent,rounding=ROUND_HALF_UP)
        total+=net+vat
    return format(total.quantize(cent),'f')
''', r'''
import billing,copy
from decimal import Decimal,ROUND_HALF_UP
cases=[([], '0','0','0.00'),([{'qty':1,'unit_price':'0.05'}]*3,'0.1','0.1','0.18'),([{'qty':2,'unit_price':'12.345'},{'qty':0,'unit_price':'999'}],'0.25','0.06','19.63'),([{'qty':1,'unit_price':'1.005'}],'0','0','1.01')]
for rows,discount,tax,expected in cases:
    original=copy.deepcopy(rows)
    assert billing.invoice_total(rows,discount,tax)==expected
    assert rows==original
for rows,d,t in [([{'qty':-1,'unit_price':'1'}],'0','0'),([{'qty':True,'unit_price':'1'}],'0','0'),([{'qty':1,'unit_price':'NaN'}],'0','0'),([{'qty':1,'unit_price':'Infinity'}],'0','0'),([{'qty':1}],'0','0'),([], '1.1','0'),([], '-0.1','0'),([], '0','NaN'),([{'qty':1,'unit_price':'oops'}],'0','0')]:
    try: billing.invoice_total(rows,d,t)
    except ValueError: pass
    else: raise AssertionError(('expected ValueError',rows,d,t))
''')

_add('t04', '修复时区跨年日志归档', 'archive.py', '''修复 archive.py 的 group_events(events)。输入是字典列表，每项包含字符串 id 和 ts。
ts 应为带 UTC offset 的 ISO-8601 日期时间，可用 Z。将有效事件转换到 Asia/Shanghai，按自然日分组，组内按输入顺序保留 id。
返回 {'by_day':{'YYYY-MM-DD':[id,...],...},'invalid':[id,...]}。invalid 按输入顺序列出 ts 非字符串、格式无效或未带时区的事件 id。不得修改输入。
events.json 是示例。使用 Python zoneinfo。''', r'''
def group_events(events):
    out={}
    for e in events: out.setdefault(e['ts'][:10],[]).append(e['id'])
    return {'by_day':out,'invalid':[]}
''', r'''
from datetime import datetime
from zoneinfo import ZoneInfo

def group_events(events):
    result={'by_day':{},'invalid':[]}
    for event in events:
        try:
            ts=event['ts']
            if not isinstance(ts,str): raise ValueError('invalid timestamp')
            dt=datetime.fromisoformat(ts.replace('Z','+00:00'))
            if dt.tzinfo is None or dt.utcoffset() is None: raise ValueError('timezone required')
            day=dt.astimezone(ZoneInfo('Asia/Shanghai')).date().isoformat()
        except (ValueError,TypeError,OverflowError): result['invalid'].append(event['id'])
        else: result['by_day'].setdefault(day,[]).append(event['id'])
    return result
''', r'''
import archive,copy,json
from pathlib import Path
rows=json.loads(Path('events.json').read_text())
expected={'by_day':{'2026-01-01':['a','b'],'2025-12-31':['c']},'invalid':['bad','naive']}
saved=copy.deepcopy(rows)
assert archive.group_events(rows)==expected
assert rows==saved
assert archive.group_events([])=={'by_day':{},'invalid':[]}
assert archive.group_events([{'id':'x','ts':'2024-02-29T23:59:59-05:00'},{'id':'none','ts':None},{'id':'day','ts':'2024-02-29'},{'id':'y','ts':'2024-03-01T01:00:00+09:00'}])=={'by_day':{'2024-03-01':['x','y']},'invalid':['none','day']}
''', {'events.json':'[{"id":"a","ts":"2025-12-31T16:00:00Z"},{"id":"b","ts":"2026-01-01T00:30:00+08:00"},{"id":"c","ts":"2025-12-31T15:59:59Z"},{"id":"bad","ts":"bad"},{"id":"naive","ts":"2026-01-01T00:00:00"}]\n'})

_add('t05', '修复事件重放重复计费', 'ledger.py', '''修复 ledger.py 的 replay(events)，返回 {'balances':{账户:整数分},'rejected':[事件id,...]}。
每项有 id（全局唯一事件号）、account（字符串）、ts（整数顺序时间）、kind（charge/refund）、amount（正整数分，bool 非法），refund 还含 ref（原 charge id）。
相同 id 只处理输入中第一次出现的记录，不再次计费或计入 rejected。去重后按 (ts,id) 升序重放；charge 给账户净收入加 amount。refund 必须引用已成功处理、同账户的 charge，累计退款不能超过该 charge 金额；成功则扣 amount。非法 kind/amount、缺 ref/未知 ref、跨账户/超额退款进入 rejected（重放顺序）。无效事件不建余额账户。只允许回退已处理 charge，不能引用 refund。返回 balances 仅含有成功事件的账户，允许余额为0；不修改输入。''', r'''
def replay(events):
    balances={}
    for e in events: balances[e['account']]=balances.get(e['account'],0)+(e['amount'] if e['kind']=='charge' else -e['amount'])
    return {'balances':balances,'rejected':[]}
''', r'''
def replay(events):
    unique={}
    for event in events: unique.setdefault(event['id'],event)
    balances={}; charges={}; rejected=[]
    for e in sorted(unique.values(),key=lambda e:(e['ts'],e['id'])):
        amount=e['amount']; account=e['account']; kind=e['kind']
        if type(amount) is not int or amount<=0 or kind not in ('charge','refund'):
            rejected.append(e['id']); continue
        if kind=='charge':
            charges[e['id']]=[account,amount]
            balances[account]=balances.get(account,0)+amount
        else:
            charge=charges.get(e.get('ref'))
            if charge is None or charge[0]!=account or amount>charge[1]:
                rejected.append(e['id']); continue
            charge[1]-=amount
            balances[account]-=amount
    return {'balances':balances,'rejected':rejected}
''', r'''
import ledger,copy
rows=[{'id':'r2','account':'a','ts':5,'kind':'refund','amount':80,'ref':'c1'},{'id':'c1','account':'a','ts':2,'kind':'charge','amount':100},{'id':'r1','account':'a','ts':4,'kind':'refund','amount':30,'ref':'c1'},{'id':'c1','account':'a','ts':1,'kind':'charge','amount':999},{'id':'c2','account':'b','ts':3,'kind':'charge','amount':40},{'id':'cross','account':'b','ts':4,'kind':'refund','amount':1,'ref':'c1'},{'id':'early','account':'a','ts':1,'kind':'refund','amount':1,'ref':'c1'}]
saved=copy.deepcopy(rows)
assert ledger.replay(rows)=={'balances':{'a':70,'b':40},'rejected':['early','cross','r2']}
assert rows==saved
assert ledger.replay([])=={'balances':{},'rejected':[]}
rows=[{'id':'a','account':'x','ts':0,'kind':'charge','amount':10},{'id':'b','account':'x','ts':1,'kind':'refund','amount':10,'ref':'a'},{'id':'c','account':'x','ts':2,'kind':'refund','amount':1,'ref':'b'},{'id':'d','account':'y','ts':2,'kind':'charge','amount':True},{'id':'e','account':'y','ts':2,'kind':'unknown','amount':2},{'id':'f','account':'y','ts':2,'kind':'charge','amount':-1}]
assert ledger.replay(rows)=={'balances':{'x':0},'rejected':['c','d','e','f']}
''')

_add('t06', '修复递归配置覆盖与共享状态', 'config.py', '''修复 config.py 中 merge_config(defaults, override)，两参数均为 JSON 风格字典。字典遇字典递归合并；其余类型均以 override 值整体替换（列表不拼接，null 即 Python None 表示替换为 None，不表示删除）。
结果应为完全独立的深拷贝，不修改 defaults/override。修改返回值中的任何嵌套容器都不得影响两个参数；不同调用返回值也不共享可变对象。''', r'''
def merge_config(defaults, override):
    defaults.update(override)
    return defaults
''', r'''
from copy import deepcopy
def merge_config(defaults, override):
    result=deepcopy(defaults)
    for key,value in override.items():
        if isinstance(result.get(key),dict) and isinstance(value,dict): result[key]=merge_config(result[key],value)
        else: result[key]=deepcopy(value)
    return result
''', r'''
import config,copy
base={'server':{'port':80,'tls':{'on':False}},'hosts':['a'],'nullable':{'x':1},'nested':[{'k':1}]}
over={'server':{'tls':{'on':True}},'hosts':['b'],'nullable':None,'extra':{'rows':[1,2]}}
a=copy.deepcopy(base); b=copy.deepcopy(over)
r=config.merge_config(base,over)
assert r=={'server':{'port':80,'tls':{'on':True}},'hosts':['b'],'nullable':None,'nested':[{'k':1}],'extra':{'rows':[1,2]}}
assert base==a and over==b
r['extra']['rows'].append(3);r['nested'][0]['k']=7;r['server']['port']=90
assert base==a and over==b
r2=config.merge_config(base,over)
assert r2['extra']['rows']==[1,2] and r2['nested']==[{'k':1}]
assert config.merge_config({'x':1,'y':[1]}, {'x':{'z':2},'y':{}})=={'x':{'z':2},'y':{}}
assert config.merge_config({}, {})=={}
''')

_add('t07', '实现文章 URL slug', 'text_utils.py', '''实现 text_utils.py 中 slugify(text: str, max_len: int=50) -> str。先转小写，将非 [a-z0-9] 连续字符替换为一个 '-'，去掉首尾 '-'，截断到 max_len 后再次去掉末尾 '-'。
若最终为空，返回 'untitled'（该回退值允许超过 max_len）。max_len 必须为正整数，bool 不接受，非法值抛 ValueError。不做 Unicode 音译。''', r'''
def slugify(text: str, max_len: int=50) -> str:
    raise NotImplementedError
''', r'''
import re
def slugify(text: str,max_len: int=50)->str:
    if type(max_len) is not int or max_len<=0: raise ValueError('positive max_len required')
    result=re.sub('[^a-z0-9]+','-',text.lower()).strip('-')[:max_len].rstrip('-')
    return result or 'untitled'
''', r'''
from text_utils import slugify
for text,n,want in [('Hello, World!',50,'hello-world'),('  Rust 与 Python 对比  ',50,'rust-python'),('你好',50,'untitled'),('a--b__c',50,'a-b-c'),('abc def ghi',4,'abc'),('Version 2.0 Released',50,'version-2-0-released'),('éAøB',50,'a-b'),('',1,'untitled'),('0123456789',3,'012')]:
    assert slugify(text,n)==want,(text,n)
for n in [0,-1,True,1.5,'4']:
    try: slugify('x',n)
    except ValueError: pass
    else: raise AssertionError('invalid max_len accepted')
''')

_add('t08', '实现持久化待办命令行', 'todo.py', '''用 Python 标准库实现 todo.py，数据保存在脚本同目录 todo.json，不依赖启动时 cwd。
命令：`python3 todo.py add "文本"`（非空白文本，去掉首尾空白，stdout 仅打印新 ID）；`python3 todo.py list`（ID 升序，每行 `id [ ] 文本` 或 `id [x] 文本`）；`python3 todo.py done ID`（完成，stdout 为空，重复完成成功且无副作用）。
ID 从1开始递增，跨进程持久化。不存在的数据文件代表空列表。空文本、非法/未知 ID、错误参数均以非零退出、stderr 诊断，且已有数据文件必须字节不变。成功命令退出0。数据为 JSON，具体内部结构不限。''', r'''
raise NotImplementedError('implement todo CLI')
''', r'''
import json
from pathlib import Path
import sys

DATA=Path(__file__).resolve().with_name('todo.json')
def main():
    args=sys.argv[1:]
    try:
        rows=json.loads(DATA.read_text(encoding='utf-8')) if DATA.exists() else []
        changed=False
        if args==['list']:
            for row in sorted(rows,key=lambda row:row['id']):
                mark='x' if row['done'] else ' '
                print(f"{row['id']} [{mark}] {row['text']}")
        elif len(args)==2 and args[0]=='add' and args[1].strip():
            tid=max((row['id'] for row in rows),default=0)+1
            rows.append({'id':tid,'text':args[1].strip(),'done':False})
            changed=True; print(tid)
        elif len(args)==2 and args[0]=='done':
            tid=int(args[1]); row=next((row for row in rows if row['id']==tid),None)
            if row is None: raise ValueError('unknown ID')
            if not row['done']: row['done']=True; changed=True
        else: raise ValueError('invalid arguments')
        if changed:
            temp=DATA.with_suffix('.json.tmp')
            temp.write_text(json.dumps(rows,ensure_ascii=False),encoding='utf-8')
            temp.replace(DATA)
    except (ValueError,OSError) as error:
        print(str(error),file=sys.stderr); return 2
    return 0
if __name__=='__main__': sys.exit(main())
''', r'''
import subprocess,sys,tempfile
from pathlib import Path
script=Path('todo.py').resolve(); data=script.with_name('todo.json')
if data.exists(): data.unlink()
def command(*args):
    with tempfile.TemporaryDirectory() as elsewhere:
        return subprocess.run([sys.executable,'-I',str(script),*args],cwd=elsewhere,text=True,capture_output=True,timeout=5)
assert command('list').stdout==''
assert command('add',' 买牛奶 ').stdout.strip()=='1'
assert command('add','draft "weekly"').stdout.strip()=='2'
assert command('done','2').returncode==0
assert command('list').stdout.splitlines()==['1 [ ] 买牛奶','2 [x] draft "weekly"']
original=data.read_bytes()
assert command('done','2').returncode==0 and data.read_bytes()==original
for args in [('done','999'),('done','bad'),('add','  '),('unknown',)]:
    p=command(*args)
    assert p.returncode!=0 and p.stderr and data.read_bytes()==original,args
assert command('add','第三项').stdout.strip()=='3'
assert command('done','1').returncode==0
assert command('list').stdout.splitlines()==['1 [x] 买牛奶','2 [x] draft "weekly"','3 [ ] 第三项']
''')

_add('t09', '实现跨进程库存流水工具', 'stock.py', '''实现 stock.py。receipts.csv 为不可改动的初始流水，字段 sku,delta，按 SKU 汇总得初始库存；之后的更改保存在脚本同目录 stock.json，不能修改 receipts.csv。
命令：`python3 stock.py show` 输出完整库存 JSON 对象（sku 到整数数量），`python3 stock.py in SKU QTY` 入库，`python3 stock.py out SKU QTY` 出库，成功 in/out 的 stdout 输出该 SKU 最新库存整数。
SKU 为非空白字符串且无首尾空白（否则拒绝），QTY 只能包含 ASCII 数字且数值大于0。不允许负库存；未知 SKU 可入库但不能出库；失败以非零退出并向 stderr 诊断，stock.json 若存在则字节不变，若不存在则不得新建。show 不更改数据文件。数据保存路径与 cwd 无关，跨进程持久化，成功退出0。''', r'''
raise NotImplementedError('implement stock CLI')
''', r'''
import csv,json,re,sys
from pathlib import Path
BASE=Path(__file__).resolve().parent
DATA=BASE/'stock.json'
def initial():
    rows={}
    with (BASE/'receipts.csv').open(encoding='utf-8',newline='') as f:
        for r in csv.DictReader(f): rows[r['sku']]=rows.get(r['sku'],0)+int(r['delta'])
    return rows
def main():
    try:
        rows=json.loads(DATA.read_text(encoding='utf-8')) if DATA.exists() else initial()
        args=sys.argv[1:]
        if args==['show']: print(json.dumps(rows,ensure_ascii=False,sort_keys=True)); return 0
        if len(args)!=3 or args[0] not in ('in','out'): raise ValueError('usage: show | in/out SKU QTY')
        op,sku,text=args
        if not sku or sku.strip()!=sku or not re.fullmatch('[0-9]+',text) or int(text)<=0: raise ValueError('invalid SKU/quantity')
        amount=int(text)*(1 if op=='in' else -1)
        new=rows.get(sku,0)+amount
        if new<0: raise ValueError('insufficient stock')
        rows[sku]=new
        temp=DATA.with_suffix('.json.tmp');temp.write_text(json.dumps(rows,ensure_ascii=False,sort_keys=True),encoding='utf-8');temp.replace(DATA)
        print(new);return 0
    except (OSError,ValueError) as error: print(str(error),file=sys.stderr);return 2
if __name__=='__main__': sys.exit(main())
''', r'''
import subprocess,sys,json,tempfile
from pathlib import Path
script=Path('stock.py').resolve(); data=script.with_name('stock.json')
if data.exists(): data.unlink()
def command(*args):
    with tempfile.TemporaryDirectory() as elsewhere:
        return subprocess.run([sys.executable,'-I',str(script),*args],cwd=elsewhere,text=True,capture_output=True,timeout=5)
assert json.loads(command('show').stdout)=={'pen':10,'book':2}
assert not data.exists()
assert command('out','pen','99').returncode!=0 and not data.exists()
assert command('in','pen','3').stdout.strip()=='13'
assert command('out','pen','13').stdout.strip()=='0'
assert command('in','新商品','7').stdout.strip()=='7'
original=data.read_bytes()
for args in [('out','pen','1'),('out','missing','1'),('in','x','0'),('in','x','-1'),('in','x','1.5'),('in',' x','1'),('unknown',)]:
    p=command(*args);assert p.returncode!=0 and p.stderr and data.read_bytes()==original,args
assert json.loads(command('show').stdout)=={'pen':0,'book':2,'新商品':7}
assert data.read_bytes()==original
''', {'receipts.csv':'sku,delta\npen,12\nbook,2\npen,-2\n'})

_add('t10', '实现保留重复行的 CSV 左连接', 'join_csv.py', '''实现 join_csv.py 命令：`python3 join_csv.py CUSTOMERS_CSV ORDERS_CSV OUTPUT_CSV`。
客户字段为 customer_id,name；订单字段为 order_id,customer_id,amount。按 customer_id 将客户姓名左连接到每一笔订单，输出路径必须不同于两个输入路径（测试满足此条件）。输出表头必须为 order_id,customer_id,amount,customer_name，按订单输入顺序保留全部订单及重复行；不存在客户时 customer_name 为空。金额原文本不变，不做数值计算。空客户 ID 视为未知，即使客户表出现空 ID 也不关联。
客户表中非空 ID 重复时，退出非零并向 stderr 诊断，不得创建/修改输出文件。其他正常输入必须退出0。支持带 BOM 的 UTF-8、逗号/双引号/换行字段及中文；不得改动输入。提供 customers.csv 和 orders.csv 供自测。''', r'''
raise NotImplementedError('implement CSV join CLI')
''', r'''
import csv,os,sys,tempfile
from pathlib import Path

def main():
    try:
        if len(sys.argv)!=4: raise ValueError('expected customers orders output')
        customer_path,order_path,output=map(Path,sys.argv[1:])
        customers={}
        with customer_path.open(encoding='utf-8-sig',newline='') as f:
            for row in csv.DictReader(f):
                key=row['customer_id']
                if not key: continue
                if key in customers: raise ValueError('duplicate customer ID')
                customers[key]=row['name']
        with order_path.open(encoding='utf-8-sig',newline='') as f: orders=list(csv.DictReader(f))
        fd,temp=tempfile.mkstemp(prefix='.join-',dir=output.resolve().parent)
        try:
            with os.fdopen(fd,'w',encoding='utf-8',newline='') as f:
                writer=csv.DictWriter(f,fieldnames=['order_id','customer_id','amount','customer_name']);writer.writeheader()
                for row in orders:
                    writer.writerow({key:row[key] for key in ['order_id','customer_id','amount']} | {'customer_name':customers.get(row['customer_id'],'')})
            os.replace(temp,output)
        finally:
            if os.path.exists(temp): os.unlink(temp)
        return 0
    except (OSError,ValueError,KeyError,csv.Error) as error: print(str(error),file=sys.stderr);return 2
if __name__=='__main__': sys.exit(main())
''', r'''
import subprocess,sys,csv
from pathlib import Path
def command(customers,orders,out): return subprocess.run([sys.executable,'-I','join_csv.py',customers,orders,out],text=True,capture_output=True,timeout=5)
p=command('customers.csv','orders.csv','result.csv');assert p.returncode==0,p.stderr
with open('result.csv',newline='',encoding='utf-8') as f:
    reader=csv.DictReader(f); rows=list(reader)
    assert reader.fieldnames==['order_id','customer_id','amount','customer_name']
assert rows==[{'order_id':'o1','customer_id':'c1','amount':'010.00','customer_name':'张,三'},{'order_id':'o1','customer_id':'c1','amount':'010.00','customer_name':'张,三'},{'order_id':'o2','customer_id':'missing','amount':'2','customer_name':''}]
with open('extra-customers.csv','w',encoding='utf-8-sig',newline='') as f:
    w=csv.writer(f);w.writerow(['customer_id','name']);w.writerows([['c2','Alice "A"\nSmith'],['','nobody']])
with open('extra-orders.csv','w',encoding='utf-8',newline='') as f:
    w=csv.writer(f);w.writerow(['order_id','customer_id','amount']);w.writerows([['o3','c2','3.10'],['o4','','0']])
assert command('extra-customers.csv','extra-orders.csv','result.csv').returncode==0
with open('result.csv',encoding='utf-8',newline='') as f: got=list(csv.DictReader(f))
assert [r['customer_name'] for r in got]==['Alice "A"\nSmith','']
Path('extra-customers.csv').write_text('customer_id,name\nx,a\nx,b\n')
before=Path('result.csv').read_bytes()
p=command('extra-customers.csv','extra-orders.csv','result.csv')
assert p.returncode!=0 and p.stderr and Path('result.csv').read_bytes()==before
p=command('extra-customers.csv','extra-orders.csv','must-not-exist.csv')
assert p.returncode!=0 and not Path('must-not-exist.csv').exists()
Path('empty-orders.csv').write_text('order_id,customer_id,amount\n')
assert command('customers.csv','empty-orders.csv','empty.csv').returncode==0
with open('empty.csv',encoding='utf-8',newline='') as f: assert list(csv.DictReader(f))==[]
''', {'customers.csv':'customer_id,name\nc1,"张,三"\nc2,Alice\n','orders.csv':'order_id,customer_id,amount\no1,c1,010.00\no1,c1,010.00\no2,missing,2\n'})

_add('t11', '实现 TTL 与 LRU 配置缓存', 'cache.py', '''实现 cache.py 中 ConfigCache(ttl=30, max_entries=32, clock=time.monotonic)，以及方法 get(path)。get 以 UTF-8 从 JSON 文件加载并返回 JSON 值，路径按 resolve() 后的绝对路径作为缓存键。
同一键命中时不重新读文件，命中会提升为最近使用；now >= 成功读取时刻+ttl 即过期，需重新加载。容量满时淘汰最久未使用项；ttl=0 意味着每次重新加载。clock 为可注入的零参数秒数函数，用于确定性测试。
每次返回独立深拷贝，调用方不可污染缓存；读取或 JSON 解析失败应原样传播异常且不缓存失败结果，过期失败后不得回退旧值。ttl 为有限非负 int/float（bool 不接受），max_entries 为正 int（bool 不接受），非法构造参数抛 ValueError。返回值可为任何 JSON 类型。''', r'''
import time
class ConfigCache:
    def __init__(self,ttl=30,max_entries=32,clock=time.monotonic): raise NotImplementedError
    def get(self,path): raise NotImplementedError
''', r'''
from collections import OrderedDict
from copy import deepcopy
import json,math,time
from pathlib import Path

class ConfigCache:
    def __init__(self,ttl=30,max_entries=32,clock=time.monotonic):
        if isinstance(ttl,bool) or not isinstance(ttl,(int,float)) or not math.isfinite(ttl) or ttl<0: raise ValueError('invalid ttl')
        if type(max_entries) is not int or max_entries<=0: raise ValueError('invalid capacity')
        self.ttl=ttl;self.max_entries=max_entries;self.clock=clock;self.entries=OrderedDict()
    def get(self,path):
        key=str(Path(path).resolve());now=self.clock()
        entry=self.entries.get(key)
        if entry is not None and now<entry[0]+self.ttl:
            self.entries.move_to_end(key)
            return deepcopy(entry[1])
        self.entries.pop(key,None)
        with open(key,encoding='utf-8') as f: value=json.load(f)
        self.entries[key]=(self.clock(),deepcopy(value))
        while len(self.entries)>self.max_entries: self.entries.popitem(last=False)
        return deepcopy(value)
''', r'''
import cache,json
from pathlib import Path
now=[100.0]
c=cache.ConfigCache(ttl=10,max_entries=2,clock=lambda:now[0])
a=Path('a.json');b=Path('b.json');z=Path('z.json')
a.write_text('{"rows":[1]}');b.write_text('2');z.write_text('3')
r=c.get(a);r['rows'].append(9)
assert c.get('./a.json')=={'rows':[1]}
a.write_text('{"rows":[2]}')
assert c.get(a)=={'rows':[1]}
now[0]=110
assert c.get(a)=={'rows':[2]}
assert c.get(b)==2
assert c.get(a)=={'rows':[2]}
assert c.get(z)==3
b.write_text('20')
assert c.get(b)==20,'LRU must evict b after a was used'
now[0]=200
a.write_text('broken')
for _ in range(2):
    try: c.get(a)
    except json.JSONDecodeError: pass
    else: raise AssertionError('JSON error must propagate; no stale fallback')
a.write_text('null');assert c.get(a) is None
a.write_text('false');assert c.get(a) is None
now[0]=210;assert c.get(a) is False
missing=Path('missing.json')
try: c.get(missing)
except FileNotFoundError: pass
else: raise AssertionError('missing file')
missing.write_text('[4]');assert c.get(missing)==[4]
fresh=cache.ConfigCache(ttl=0,clock=lambda:now[0]);a.write_text('1');assert fresh.get(a)==1
a.write_text('2');assert fresh.get(a)==2
for kwargs in [{'ttl':-1},{'ttl':True},{'ttl':float('inf')},{'ttl':float('nan')},{'ttl':'1'},{'max_entries':0},{'max_entries':True},{'max_entries':1.5}]:
    try: cache.ConfigCache(**kwargs)
    except ValueError: pass
    else: raise AssertionError(('invalid parameters',kwargs))
''')

_add('t12', '实现嵌套 JSONL 日志脱敏工具', 'redact.py', '''实现 `python3 redact.py INPUT_JSONL OUTPUT_JSONL SUMMARY_JSON`，逐行读取 UTF-8 JSONL。空白行忽略，非法 JSON 行跳过并计数；合法 JSON 顶层允许对象、数组及标量。
递归遍历对象/数组：键名不区分大小写，email 的值全部替换为 '[EMAIL]'，phone 的值全部替换为 '[PHONE]'，token/authorization/password 的值全部替换为 '[REDACTED]'；这些敏感键不管原值类型都替换整值，不继续遍历原值。其他字段/值包括非敏感文本必须原样保留。
输出每个合法输入值为一行 JSON，保留原顺序，中文不要求转义方式。summary.json 为 {'processed':合法行数,'skipped':非法非空行数,'redacted':替换字段次数}。不得修改输入；正常转换包括跳过坏行时退出0。不应把原始敏感值打印到 stdout/stderr。示例输入为 logs.jsonl。''', r'''
raise NotImplementedError('implement JSONL redaction CLI')
''', r'''
import json,sys
from pathlib import Path

MASKS={'email':'[EMAIL]','phone':'[PHONE]','token':'[REDACTED]','authorization':'[REDACTED]','password':'[REDACTED]'}
def redact(value):
    count=0
    if isinstance(value,dict):
        result={}
        for key,item in value.items():
            if key.lower() in MASKS: result[key]=MASKS[key.lower()];count+=1
            else: result[key],n=redact(item);count+=n
        return result,count
    if isinstance(value,list):
        result=[]
        for item in value:
            cleaned,n=redact(item);result.append(cleaned);count+=n
        return result,count
    return value,0

def main():
    if len(sys.argv)!=4: print('expected input output summary',file=sys.stderr);return 2
    inp,out,summary=map(Path,sys.argv[1:])
    stats={'processed':0,'skipped':0,'redacted':0}
    with inp.open(encoding='utf-8') as source,out.open('w',encoding='utf-8') as target:
        for line in source:
            if not line.strip(): continue
            try: value=json.loads(line)
            except (ValueError,TypeError): stats['skipped']+=1;continue
            value,n=redact(value)
            stats['processed']+=1;stats['redacted']+=n
            target.write(json.dumps(value,ensure_ascii=False)+'\n')
    summary.write_text(json.dumps(stats),encoding='utf-8');return 0
if __name__=='__main__': sys.exit(main())
''', r'''
import subprocess,sys,json
from pathlib import Path
p=subprocess.run([sys.executable,'-I','redact.py','logs.jsonl','clean.jsonl','summary.json'],text=True,capture_output=True,timeout=5)
assert p.returncode==0,p.stderr
assert json.loads(Path('summary.json').read_text())=={'processed':3,'skipped':1,'redacted':4}
rows=[json.loads(x) for x in Path('clean.jsonl').read_text().splitlines()]
assert rows==[{'email':'[EMAIL]','nested':{'Phone':'[PHONE]','message':'keep me'},'ok':True},[{'TOKEN':'[REDACTED]','auth':{'password':'[REDACTED]'}},7],'ordinary x@y.invalid']
for secret in ['private@example.invalid','13800000000','secret-key-1','pw-fixture-secret-2']:
    assert secret not in p.stdout+p.stderr, 'raw sensitive fixture value leaked to process output'
Path('extra.jsonl').write_text('\nnull\n{"email":{"token":"nested-secret-3"},"PHONE":null,"n":4,"arr":[false,{"Authorization":"Bearer private-auth-token-4"}]}\ninvalid\n\n',encoding='utf-8')
p=subprocess.run([sys.executable,'-I','redact.py','extra.jsonl','extra-clean.jsonl','extra-summary.json'],text=True,capture_output=True,timeout=5)
assert p.returncode==0,p.stderr
for secret in ['nested-secret-3','private-auth-token-4','Bearer private-auth-token-4']:
    assert secret not in p.stdout+p.stderr, 'raw nested sensitive/header value leaked to process output'
assert json.loads(Path('extra-summary.json').read_text())=={'processed':2,'skipped':1,'redacted':3}
assert [json.loads(x) for x in Path('extra-clean.jsonl').read_text().splitlines()]==[None,{'email':'[EMAIL]','PHONE':'[PHONE]','n':4,'arr':[False,{'Authorization':'[REDACTED]'}]}]
Path('empty.jsonl').write_text(' \n\n')
p=subprocess.run([sys.executable,'-I','redact.py','empty.jsonl','empty-clean.jsonl','empty-summary.json'],text=True,capture_output=True,timeout=5)
assert p.returncode==0 and Path('empty-clean.jsonl').read_text()==''
assert json.loads(Path('empty-summary.json').read_text())=={'processed':0,'skipped':0,'redacted':0}
''', {'logs.jsonl':'{"email":"private@example.invalid","nested":{"Phone":"13800000000","message":"keep me"},"ok":true}\nnot-json\n\n[{"TOKEN":"secret-key-1","auth":{"password":"pw-fixture-secret-2"}},7]\n"ordinary x@y.invalid"\n'})


def prepare(tid: str, d: Path) -> None:
    """Materialize only public instructions, starter code and sample inputs."""
    d = Path(d)
    d.mkdir(parents=True, exist_ok=True)
    case = _CASES[tid]
    (d / 'SPEC.md').write_text(case['prompt'] + '\n', encoding='utf-8')
    (d / case['filename']).write_text(case['starter'], encoding='utf-8')
    for relative, content in case['inputs'].items():
        destination = d / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_text(content, encoding='utf-8')


def verify(tid: str, d: Path) -> tuple[bool, str]:
    """Exercise submitted behavior in a subprocess; do not import it in grader."""
    case = _CASES[tid]
    try:
        with tempfile.TemporaryDirectory(prefix='rw48-code-check-') as temporary:
            work = Path(temporary) / 'work'
            shutil.copytree(Path(d), work, ignore=shutil.ignore_patterns('__pycache__', '*.pyc'))
            # Import only submitted business module after stdlib bootstrap. -I ignores
            # PYTHONPATH, user site packages and startup hooks from the task directory.
            program = 'import sys\nsys.path.insert(0, ' + repr(str(work)) + ')\n' + case['check']
            env = os.environ.copy()
            env['PYTHONDONTWRITEBYTECODE'] = '1'
            completed = subprocess.run([sys.executable, '-I', '-c', program], cwd=work,
                                       capture_output=True, text=True, timeout=30, env=env)
            if completed.returncode != 0:
                return False, (completed.stdout + completed.stderr)[-2500:]
            return True, 'hidden behavior checks passed'
    except subprocess.TimeoutExpired:
        return False, 'hidden behavior checks timed out (30 seconds)'
    except (OSError, shutil.Error) as error:
        return False, 'cannot execute submitted solution: ' + str(error)


def solve(tid: str, d: Path) -> None:
    """Install independent general reference implementation for offline selfcheck."""
    case = _CASES[tid]
    (Path(d) / case['filename']).write_text(case['reference'], encoding='utf-8')
