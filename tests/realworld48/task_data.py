"""Deterministic data, SQL and operations tasks t13–t30 (stdlib only).

Only prepare() output goes to the agent. Gold data, executable checks and reference
calculations stay here in the evaluator process. Monetary JSON values are strings.
"""
from __future__ import annotations

import collections
import csv
import datetime as dt
import decimal
import hashlib
import json
import math
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
import tempfile

D = decimal.Decimal


def _jwrite(path, obj):
    Path(path).write_text(json.dumps(obj, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')


def _readcsv(d, name):
    with (d / name).open(encoding='utf-8', newline='') as f:
        return list(csv.DictReader(f))


def _money(x):
    return str(D(x).quantize(D('.01'), rounding=decimal.ROUND_HALF_UP))


TASKS = []


def _task(n, title, prompt, protected, output='result.json'):
    TASKS.append(dict(id=f't{n:02}', title=title, category=('数据分析' if n < 19 else 'SQL' if n < 25 else '运维分析'),
                      difficulty=('easy', 'easy', 'medium', 'medium', 'hard', 'hard')[(n - 13) % 6],
                      prompt=prompt + '\n在当前工作目录完成。不得修改任何输入文件或 SPEC.md。金额用两位小数字符串，比例用四位小数字符串；JSON 不允许额外字段。',
                      protected=['SPEC.md'] + protected, output=output))


_task(13, '年度销售汇总', '''财务要核对 2025 年销售。读取 sales.csv，生成 result.json：{"by_region":{"地区":"金额"},"top_product":"SKU","skipped_rows":整数}。
先验证每行 date 为真实 ISO 日期、qty 为非负整数、unit_price 为有限非负 Decimal；任一不合法计入 skipped_rows（不分年份），合法但非2025年忽略。销售额 qty*unit_price，汇总后 HALF_UP 保留两位。地区/SKU按原字符串分组，零额也保留；销售额并列的 top_product 取 SKU 字典序最小。''', ['sales.csv'])
_task(14, '渠道转化率', '''读取 funnel.csv，为配置 channels.json 中每个渠道生成 result.json 的 channels 数组，按渠道名排序。每项 {"channel":字符串,"visitors":整数,"converted":整数,"rate":"0.0000"}。
按(channel,user)去重计算访问人数，只考虑 event=visit 和 buy。某用户必须存在该渠道 visit，且至少一次 buy 时间大于或等于该用户该渠道首次 visit 才算转化；buy 早于首次访问不算。rate=converted/visitors，HALF_UP四位；无访问为0.0000。输入时间均是UTC ISO。''', ['funnel.csv', 'channels.json'])
_task(15, '支付银行对账', '''读取 payments.csv 和 bank.csv，以 transaction_id 对账。相同交易号相同金额的重复行只保留一条；某一侧同交易号出现不同金额则列入 conflicts，该交易不再参与其他分类。
result.json 为 {"matched":[交易号],"amount_mismatches":[{"id":交易号,"payment":"金额","bank":"金额"}],"payment_only":[交易号],"bank_only":[交易号],"conflicts":[交易号],"duplicate_rows":{"payments":数量,"bank":数量}}。重复计数指已出现相同(id,金额)的后续行；所有列表按id排序。金额精确 Decimal。''', ['payments.csv', 'bank.csv'])
_task(16, '订阅续费流失分析', '''以 observation.json 的观察日 2025-03-31 评估 subscriptions.csv。每行是一位独立订阅用户，start/end 均为包含端点的ISO日期。end>=观察日且start<=观察日算 active（即使已预约取消）；end<观察日算 churned；未来开始的不进 active/churned。renewed=用户renewed字段为true且非未来开始。result.json={"by_plan":[{"plan":字符串,"eligible":整数,"active":整数,"churned":整数,"renewed":整数,"churn_rate":"0.0000"}]}，eligible=非未来开始人数，churn_rate=churned/eligible，无分母0。计划名排序，保留仅未来开始的计划。''', ['subscriptions.csv', 'observation.json'])
_task(17, '多仓快照合并', '''读取 snapshots/a.csv 与 snapshots/b.csv，按(warehouse,sku)合并。取 updated_at 最大版本（UTC），同最大时间若有不同 qty 则记为冲突且该键不进入库存；同值重复无冲突。只对最终采用的负数库存告警。
result.json={"inventory":[{"warehouse":字符串,"sku":字符串,"qty":整数,"updated_at":字符串}],"conflicts":[{"warehouse":字符串,"sku":字符串,"updated_at":字符串,"quantities":[升序去重整数]}],"negative_stock":[{"warehouse":字符串,"sku":字符串,"qty":整数}]}。各数组按(warehouse,sku)排序，更新时间沿用输入ISO格式。''', ['snapshots/a.csv', 'snapshots/b.csv'])
_task(18, '多币种月度利润', '''读取 orders.csv(id,date,currency,amount,cost)、refunds.csv(id,order_id,date,amount)、rates.csv(date,currency,cny_rate)，生成 result.json={"months":[{"month":"YYYY-MM","revenue":"金额","refunds":"金额","cost":"金额","profit":"金额"}],"missing_rates":[{"kind":"order或refund","id":字符串,"currency":字符串,"date":字符串}]}。
CNY恒定1，其他币种取不晚于该事件日期的最近汇率，订单收入与成本均用订单日汇率，退款用退款日和原订单币种，退款归退款发生月，退款不退成本。缺汇率事件整体跳过并报告，不因原订单缺汇率而跳过有汇率的退款。逐事件换算后不舍入，按月求和最后HALF_UP两位；profit=revenue-refunds-cost。所有至少有成功事件的月份输出并排序，missing_rates按(kind,id)排序。''', ['orders.csv', 'refunds.csv', 'rates.csv'])
_task(19, '年度付费客户排行', '''读取 shop.db(customers/ orders)，编写单条只读 SQLite 查询 query.sql，并输出其结果到 result.json 数组，每项 {"customer_id":整数,"name":字符串,"total":"金额"}。
仅 status='paid' 且 created_at>=2025-01-01 且<2026-01-01 的订单，金额字段 amount_cents 为整数分。按客户ID分组（同名不合并），仅有符合订单的客户，按总分降序、客户ID升序，取前三。query.sql 列别名必须是 customer_id,name,total_cents；total_cents为整数。验收会另换数据执行SQL。''', ['shop.db'])
_task(20, '寻找从未下单的客户', '''读取 crm.db，编写 query.sql 和 result.json。返回 is_test=0 且没有任何 orders.customer_id 指向的客户，NULL customer_id不匹配任何人，任何状态订单均算下过单。
query.sql 必须单条只读 SELECT，列别名 customer_id,name；result.json为相同对象数组，按customer_id升序。隐藏验收包含空订单表。''', ['crm.db'])
_task(21, '客户最近有效订单', '''读取 orders.db。订单 status!='cancelled' 为有效；按每客户 created_at最大，若相同取id最大的一单。包括从未有效下单的客户，订单字段均null。query.sql 单条只读SELECT，列别名 customer_id,name,order_id,created_at,amount_cents，按customer_id升序。
result.json为对象数组，金额仍是整数分或null，与SQL结果完全一致。隐藏验收会改变订单。''', ['orders.db'])
_task(22, '商品复购率', '''读取 commerce.db(products,orders,order_items)，仅status='paid'的订单。每个商品按客户统计不同订单数；一单多行同商品只计一次；购买>=2个不同订单的客户为复购客户。
query.sql只读SELECT，列别名product_id,name,buyers,repeat_buyers；保留零销售商品，按product_id升序。result.json为数组，每项还需rate字段=repeat_buyers/buyers，HALF_UP四位，零分母0.0000。隐藏验收会改变数据。''', ['commerce.db'])
_task(23, '用户表安全迁移', '''legacy.db含旧users(id,name,email)和orders(id,user_id,total_cents)。请写 migrate.py（运行方式 python3 migrate.py legacy.db）并在现有DB执行。
迁移后 users列精确为id,display_name,email,email_key，数据原name原样搬到display_name，email_key=trim(email).lower()，email为NULL则email_key为NULL。id主键；users.email_key有名为idx_users_email_key的普通非唯一索引；orders保留全部数据、列，user_id继续REFERENCES users(id)。不可保留users_old临时表。要求事务原子性与重复执行幂等。seed.sql为只读审计来源；不得改它。验收运行迁移两次并在不同数据上重放，不要用硬编码ID或重新造库。''', ['seed.sql'], output='migrate.py')
_task(24, '修复JOIN收入翻倍', '''report.sql的JOIN将商品分类和退款多对多相乘，修复并保存为query.sql。shop.db含customers(id,name)、orders(id,customer_id,status,amount_cents)、refunds(id,order_id,amount_cents)、order_tags(order_id,tag)。
统计每客户所有paid订单的净收入=订单amount_cents-该订单全部退款amount_cents，不能因多个tag或多个退款倍增；包含零订单客户，净额允许负数，非paid订单及其退款排除。输出列customer_id,name,net_cents，按customer_id升序；单条只读SQLite查询。无需生成JSON。隐藏验收会替换数据。''', ['shop.db', 'report.sql'], output='query.sql')
_task(25, '访问日志IP与5xx', '''分析access.log，合法格式为IP - - [时间] "请求" 三位HTTP状态 字节数 "referer" "ua"；状态必须100..599、IP必须为合法IPv4。其他行计bad_lines。
result.json={"top_ips":[["IP",次数],...],"count_5xx":整数,"bad_lines":整数}，只统计合法行；top_ips最多3个，按次数降序、相同次数按IP字符串升序。''', ['access.log'])
_task(26, 'CI失败步骤提取', '''读取ci/build.log和ci/test.log，每行JSON对象至少ts,job,step,event。只把event=step_end且exit_code整数非零的行作为失败，错误文本或event=output不算；同一(job,step)可能重跑，取ts最大（相同时取路径字典序最大、再行号最大）的step_end最终状态，最终成功则不报告。
result.json={"failures":[{"job":字符串,"step":字符串,"exit_code":整数,"source":"相对路径:行号"}],"bad_lines":整数}，failures按(job,step)排序；非JSON行计bad_lines，输入其余JSON均合法。''', ['ci/build.log', 'ci/test.log'])
_task(27, '接口p95与错误率', '''读取requests.jsonl，每行对象route,status,latency_ms。route必须以/开头字符串，status为100..599整数(布尔不算)，latency_ms为有限非负数字(布尔不算)；坏JSON或不符合计bad_rows。
去掉route第一个?及其后查询串后分组，按route排序。result.json={"routes":[{"route":字符串,"count":整数,"p95_ms":数字,"error_rate":"0.0000"}],"bad_rows":整数}。p95为nearest-rank即排序后第ceil(0.95*n)个样本，错误为status>=500，比例HALF_UP四位。''', ['requests.jsonl'])
_task(28, '跨服务trace故障归因', '''读取services/api.jsonl和services/db.jsonl，均为JSON，每行字段trace_id,span_id,parent_id,service,start,status。start均UTC；仅报告至少一个status=error的trace。根因定义为该trace所有error span中start最早者，同时间取span_id字典序最小。不要将后续上游报错当首因。
result.json={"traces":[{"trace_id":字符串,"first_failed_span":字符串,"service":字符串,"source":"相对路径:行号","chain":[根span到首失败span的span_id]}]}，trace按ID排序。parent_id=null为根，输入父子关系无环且完整。''', ['services/api.jsonl', 'services/db.jsonl'])
_task(29, '重试任务最终失败分析', '''读取worker/a.jsonl与worker/b.jsonl，按job_id聚合所有事件(ts,job_id,attempt,status)，status取running/retry/failed/succeeded。按ts升序，同时间attempt升序，仍相同按路径和行号升序，最后事件定义最终状态（running/retry是unfinished）。
result.json={"failed":[{"job_id":字符串,"attempts":整数,"last_attempt":整数,"source":"相对路径:行号"}],"succeeded":整数,"unfinished":整数}。attempts为不同attempt数、last_attempt为最后事件attempt；最终成功即使前面失败也不在failed；failed按job_id排序。''', ['worker/a.jsonl', 'worker/b.jsonl'])
_task(30, '磁盘清理dry-run', '''模拟磁盘清理，只生成result.json，不得删除/改动tree下内容。policy.json给出now=2025-03-31T00:00:00Z、retention_days=30、protected_prefixes=["keep/"]。
从tree递归选择普通文件：mtime严格早于now-30天、非保护前缀；所有符号链接一律跳过，不跟随目录符号链接。按相对tree的POSIX路径排序。
result.json={"candidates":[{"path":字符串,"size":字节整数}],"total_bytes":整数}。输入含隐藏文件也需考虑；边界时刻不清理；symlink目标即使位于tree内也不能额外统计。''', ['policy.json', 'tree/old.log', 'tree/boundary.log', 'tree/new.log', 'tree/.old-cache', 'tree/keep/old.log', 'tree/nested/stale.txt', 'tree/link-old', 'tree/link-outside', 'tree/link-dir'])


CSV = {
 't13': {'sales.csv': 'date,region,product,qty,unit_price\n2025-01-01,华东,A,2,10.005\n2025-02-28,华东,B,1,19.99\n2025-12-31,华北,A,1,2.50\n2024-12-31,华北,B,99,999\n2025-04-01,西部,C,0,100\n2025-02-30,华东,A,2,1\n2025-06-01,华南,A,no,3\n2024-05-02,华南,A,1,NaN\n2025-08-01,华南,B,-1,3\n'},
 't14': {'funnel.csv': 'channel,user,event,ts\nemail,u1,visit,2025-03-01T00:00:00Z\nemail,u1,visit,2025-03-01T00:01:00Z\nemail,u1,buy,2025-03-01T00:02:00Z\nemail,u2,buy,2025-02-28T00:00:00Z\nemail,u2,visit,2025-03-01T00:00:00Z\nemail,u3,visit,2025-03-01T00:00:00Z\nads,u1,visit,2025-03-01T00:00:00Z\nads,u1,buy,2025-03-01T00:00:00Z\nads,u4,buy,2025-03-01T00:00:00Z\n'},
 't15': {'payments.csv':'transaction_id,amount\np1,10.00\np2,20.00\np2,20.0\np3,30\np4,40\np4,41\np6,60\n','bank.csv':'transaction_id,amount\np1,10\np2,19.99\np2,19.990\np4,40\np5,50\np7,70\np7,71\n'},
 't16': {'subscriptions.csv':'user,plan,start,end,renewed,cancelled_at\nu1,basic,2025-01-31,2025-03-31,true,2025-03-10\nu2,basic,2025-02-28,2025-03-30,false,\nu3,basic,2025-04-01,2025-05-01,true,\nu4,pro,2024-12-31,2025-04-30,true,\nu5,pro,2025-01-01,2025-02-28,false,2025-02-01\nu6,future,2025-04-30,2025-05-31,false,\n'},
 't17': {'snapshots/a.csv':'warehouse,sku,qty,updated_at\nW1,A,3,2025-03-01T00:00:00Z\nW1,A,7,2025-03-02T00:00:00Z\nW1,B,-2,2025-03-03T00:00:00Z\nW2,A,4,2025-03-01T00:00:00Z\nW2,C,-5,2025-03-01T00:00:00Z\n','snapshots/b.csv':'warehouse,sku,qty,updated_at\nW1,A,7,2025-03-02T00:00:00Z\nW1,B,2,2025-03-02T00:00:00Z\nW2,A,5,2025-03-01T00:00:00Z\nW2,C,0,2025-03-02T00:00:00Z\n'},
 't18': {'orders.csv':'id,date,currency,amount,cost\no1,2025-01-10,USD,10.00,4.00\no2,2025-01-20,CNY,100.005,30\no3,2025-02-02,EUR,10,2\no4,2025-01-01,EUR,10,2\no5,2025-02-02,JPY,100,50\n','refunds.csv':'id,order_id,date,amount\nr1,o1,2025-02-01,2\nr2,o4,2025-02-03,1\nr3,o5,2025-01-01,1\n','rates.csv':'date,currency,cny_rate\n2025-01-01,USD,7.1\n2025-02-01,USD,7.2\n2025-02-01,EUR,8\n2025-02-05,EUR,9\n'},
}

SQL_SCHEMAS = {
 't19': '''CREATE TABLE customers(id INTEGER PRIMARY KEY,name TEXT); CREATE TABLE orders(id INTEGER PRIMARY KEY,customer_id INTEGER,amount_cents INTEGER,status TEXT,created_at TEXT);
INSERT INTO customers VALUES(1,'同名'),(2,'同名'),(3,'丙'),(4,'丁'),(5,'空');
INSERT INTO orders VALUES(1,1,1000,'paid','2025-01-01'),(2,1,250,'paid','2025-12-31'),(3,2,1250,'paid','2025-06-01'),(4,3,900,'paid','2025-01-03'),(5,4,99999,'pending','2025-01-01'),(6,3,10000,'paid','2024-12-31'),(7,4,99999,'paid','2026-01-01');''',
 't20': '''CREATE TABLE customers(id INTEGER PRIMARY KEY,name TEXT,is_test INTEGER); CREATE TABLE orders(id INTEGER PRIMARY KEY,customer_id INTEGER,status TEXT);
INSERT INTO customers VALUES(1,'Alice',0),(2,'Test',1),(3,'Bob',0),(4,'陈',0),(5,'Dana',0); INSERT INTO orders VALUES(1,1,'paid'),(2,NULL,'paid'),(3,3,'cancelled');''',
 't21': '''CREATE TABLE customers(id INTEGER PRIMARY KEY,name TEXT); CREATE TABLE orders(id INTEGER PRIMARY KEY,customer_id INTEGER,status TEXT,created_at TEXT,amount_cents INTEGER);
INSERT INTO customers VALUES(1,'Alice'),(2,'Bob'),(3,'陈'),(4,'空');
INSERT INTO orders VALUES(1,1,'paid','2025-03-01T00:00:00Z',100),(2,1,'paid','2025-03-02T00:00:00Z',200),(3,1,'paid','2025-03-02T00:00:00Z',300),(4,1,'cancelled','2025-03-03T00:00:00Z',900),(5,2,'cancelled','2025-01-01T00:00:00Z',400),(6,3,'pending','2025-02-01T00:00:00Z',500);''',
 't22': '''CREATE TABLE products(id INTEGER PRIMARY KEY,name TEXT); CREATE TABLE orders(id INTEGER PRIMARY KEY,customer_id INTEGER,status TEXT); CREATE TABLE order_items(order_id INTEGER,product_id INTEGER,qty INTEGER);
INSERT INTO products VALUES(1,'A'),(2,'B'),(3,'零销量'),(4,'退款'); INSERT INTO orders VALUES(1,1,'paid'),(2,1,'paid'),(3,2,'paid'),(4,2,'refunded'),(5,3,'paid'); INSERT INTO order_items VALUES(1,1,1),(1,1,2),(2,1,1),(3,1,1),(3,2,1),(4,4,1),(5,2,1);''',
 't23': '''PRAGMA foreign_keys=ON; CREATE TABLE users(id INTEGER PRIMARY KEY,name TEXT NOT NULL,email TEXT); CREATE TABLE orders(id INTEGER PRIMARY KEY,user_id INTEGER REFERENCES users(id),total_cents INTEGER NOT NULL);
INSERT INTO users VALUES(1,'Alice',' Alice@Example.COM '),(2,'张三',NULL),(3,'Another','alice@example.com'); INSERT INTO orders VALUES(10,1,1200),(11,2,3400),(12,3,0);''',
 't24': '''CREATE TABLE customers(id INTEGER PRIMARY KEY,name TEXT); CREATE TABLE orders(id INTEGER PRIMARY KEY,customer_id INTEGER,status TEXT,amount_cents INTEGER); CREATE TABLE refunds(id INTEGER PRIMARY KEY,order_id INTEGER,amount_cents INTEGER); CREATE TABLE order_tags(order_id INTEGER,tag TEXT);
INSERT INTO customers VALUES(1,'Alice'),(2,'Bob'),(3,'Zero'),(4,'Negative'); INSERT INTO orders VALUES(1,1,'paid',1000),(2,1,'paid',1000),(3,2,'pending',9999),(4,4,'paid',100); INSERT INTO refunds VALUES(1,1,100),(2,1,200),(3,3,100),(4,4,150); INSERT INTO order_tags VALUES(1,'x'),(1,'y'),(2,'z'),(4,'x');''',
}
DB_NAMES = {'t19':'shop.db','t20':'crm.db','t21':'orders.db','t22':'commerce.db','t23':'legacy.db','t24':'shop.db'}


def _jsonlines(rows):
    return '\n'.join(json.dumps(x, ensure_ascii=False) for x in rows) + '\n'


def prepare(tid: str, d: Path) -> None:
    d = Path(d); d.mkdir(parents=True, exist_ok=True)
    task = next(t for t in TASKS if t['id'] == tid)
    (d / 'SPEC.md').write_text(task['prompt'] + '\n', encoding='utf-8')
    for name, content in CSV.get(tid, {}).items():
        p = d / name; p.parent.mkdir(parents=True, exist_ok=True); p.write_text(content, encoding='utf-8')
    if tid == 't14': _jwrite(d / 'channels.json', ['organic', 'email', 'ads'])
    if tid == 't16': _jwrite(d / 'observation.json', {'date':'2025-03-31'})
    if tid in SQL_SCHEMAS:
        c = sqlite3.connect(d / DB_NAMES[tid]); c.executescript(SQL_SCHEMAS[tid]); c.close()
    if tid == 't23':
        (d / 'seed.sql').write_text(SQL_SCHEMAS[tid], encoding='utf-8')
        (d / 'migrate.py').write_text('raise NotImplementedError("implement transactional migration")\n', encoding='utf-8')
    if tid == 't24':
        (d / 'report.sql').write_text("SELECT c.id customer_id,c.name,SUM(o.amount_cents-COALESCE(r.amount_cents,0)) net_cents FROM customers c LEFT JOIN orders o ON o.customer_id=c.id LEFT JOIN refunds r ON r.order_id=o.id LEFT JOIN order_tags t ON t.order_id=o.id WHERE o.status='paid' GROUP BY c.id ORDER BY c.id;\n", encoding='utf-8')
    if tid == 't25':
        rows = []
        for ip, statuses in [('10.0.0.1',[200,500,502]),('10.0.0.2',[200,404,503]),('10.0.0.3',[200,200]),('10.0.0.4',[501])]:
            for st in statuses: rows.append(f'{ip} - - [31/Mar/2025:12:00:00 +0000] "GET /api HTTP/1.1" {st} 20 "-" "curl/8"')
        rows += ['broken line', '999.0.0.1 - - [now] "GET / HTTP/1.1" 500 2 "-" "ua"', '10.0.0.1 - - [now] "GET / HTTP/1.1" 999 2 "-" "ua"']
        (d / 'access.log').write_text('\n'.join(rows)+'\n', encoding='utf-8')
    if tid == 't26':
        (d/'ci').mkdir()
        (d/'ci/build.log').write_text(_jsonlines([
            dict(ts='2025-03-01T10:00:00Z',job='build',step='compile',event='output',text='error failed exit 1'),
            dict(ts='2025-03-01T10:01:00Z',job='build',step='compile',event='step_end',exit_code=0),
            dict(ts='2025-03-01T10:02:00Z',job='build',step='lint',event='step_end',exit_code=2),
            dict(ts='2025-03-01T10:03:00Z',job='build',step='lint',event='step_end',exit_code=0),
        ])+'bad-json\n',encoding='utf-8')
        (d/'ci/test.log').write_text(_jsonlines([
            dict(ts='2025-03-01T10:01:00Z',job='test',step='unit',event='step_end',exit_code=1),
            dict(ts='2025-03-01T10:02:00Z',job='test',step='integration',event='step_end',exit_code=137),
            dict(ts='2025-03-01T10:03:00Z',job='test',step='upload',event='output',text='tests failed'),
        ]),encoding='utf-8')
    if tid == 't27':
        rows = [dict(route='/items?page='+str(i),status=500 if i==20 else 200,latency_ms=i) for i in range(1,21)]
        rows += [dict(route='/health',status=200,latency_ms=0),dict(route='/health?full=1',status=503,latency_ms=5),dict(route='bad',status=200,latency_ms=2),dict(route='/x',status=True,latency_ms=2),dict(route='/x',status=200,latency_ms=-1)]
        (d/'requests.jsonl').write_text(_jsonlines(rows)+'{oops\n',encoding='utf-8')
    if tid == 't28':
        (d/'services').mkdir()
        (d/'services/api.jsonl').write_text(_jsonlines([
            dict(trace_id='T1',span_id='a',parent_id=None,service='gateway',start='2025-03-01T00:00:00Z',status='ok'),
            dict(trace_id='T1',span_id='b',parent_id='a',service='api',start='2025-03-01T00:00:03Z',status='error'),
            dict(trace_id='T2',span_id='d',parent_id=None,service='api',start='2025-03-01T00:00:00Z',status='ok'),
            dict(trace_id='T3',span_id='z',parent_id=None,service='api',start='2025-03-01T00:00:00Z',status='error'),
        ]),encoding='utf-8')
        (d/'services/db.jsonl').write_text(_jsonlines([
            dict(trace_id='T1',span_id='c',parent_id='a',service='database',start='2025-03-01T00:00:01Z',status='error'),
            dict(trace_id='T3',span_id='y',parent_id='z',service='database',start='2025-03-01T00:00:00Z',status='error'),
        ]),encoding='utf-8')
    if tid == 't29':
        (d/'worker').mkdir()
        def e(t,j,a,s): return dict(ts='2025-03-01T00:00:'+t+'Z',job_id=j,attempt=a,status=s)
        (d/'worker/a.jsonl').write_text(_jsonlines([e('01','J1',1,'failed'),e('02','J2',1,'retry'),e('03','J3',1,'running'),e('04','J4',1,'succeeded'),e('05','J5',1,'failed')]),encoding='utf-8')
        (d/'worker/b.jsonl').write_text(_jsonlines([e('06','J2',2,'failed'),e('05','J1',2,'succeeded'),e('05','J5',2,'failed')]),encoding='utf-8')
    if tid == 't30':
        _jwrite(d/'policy.json',dict(now='2025-03-31T00:00:00Z',retention_days=30,protected_prefixes=['keep/']))
        cutoff = dt.datetime(2025,3,1,tzinfo=dt.timezone.utc).timestamp()
        for name,content,age in [('old.log','abc',-1),('boundary.log','xxxx',0),('new.log','new',1),('.old-cache','12345',-20),('keep/old.log','secret',-100),('nested/stale.txt','stale!',-100)]:
            p=d/'tree'/name;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(content,encoding='utf-8');os.utime(p,(cutoff+age,cutoff+age))
        (d/'tree'/'link-old').symlink_to('old.log')
        (d/'tree'/'link-outside').symlink_to('../policy.json')
        (d/'tree'/'link-dir').symlink_to('nested',target_is_directory=True)


GOLD = {
 't13':dict(by_region={'华东':'40.00','华北':'2.50','西部':'0.00'},top_product='A',skipped_rows=4),
 't14':dict(channels=[dict(channel='ads',visitors=1,converted=1,rate='1.0000'),dict(channel='email',visitors=3,converted=1,rate='0.3333'),dict(channel='organic',visitors=0,converted=0,rate='0.0000')]),
 't15':dict(matched=['p1'],amount_mismatches=[dict(id='p2',payment='20.00',bank='19.99')],payment_only=['p3','p6'],bank_only=['p5'],conflicts=['p4','p7'],duplicate_rows=dict(payments=1,bank=1)),
 't16':dict(by_plan=[dict(plan='basic',eligible=2,active=1,churned=1,renewed=1,churn_rate='0.5000'),dict(plan='future',eligible=0,active=0,churned=0,renewed=0,churn_rate='0.0000'),dict(plan='pro',eligible=2,active=1,churned=1,renewed=1,churn_rate='0.5000')]),
 't17':dict(inventory=[dict(warehouse='W1',sku='A',qty=7,updated_at='2025-03-02T00:00:00Z'),dict(warehouse='W1',sku='B',qty=-2,updated_at='2025-03-03T00:00:00Z'),dict(warehouse='W2',sku='C',qty=0,updated_at='2025-03-02T00:00:00Z')],conflicts=[dict(warehouse='W2',sku='A',updated_at='2025-03-01T00:00:00Z',quantities=[4,5])],negative_stock=[dict(warehouse='W1',sku='B',qty=-2)]),
 't18':dict(months=[dict(month='2025-01',revenue='171.01',refunds='0.00',cost='58.40',profit='112.61'),dict(month='2025-02',revenue='80.00',refunds='22.40',cost='16.00',profit='41.60')],missing_rates=[dict(kind='order',id='o4',currency='EUR',date='2025-01-01'),dict(kind='order',id='o5',currency='JPY',date='2025-02-02'),dict(kind='refund',id='r3',currency='JPY',date='2025-01-01')]),
 't19':[dict(customer_id=1,name='同名',total='12.50'),dict(customer_id=2,name='同名',total='12.50'),dict(customer_id=3,name='丙',total='9.00')],
 't20':[dict(customer_id=4,name='陈'),dict(customer_id=5,name='Dana')],
 't21':[dict(customer_id=1,name='Alice',order_id=3,created_at='2025-03-02T00:00:00Z',amount_cents=300),dict(customer_id=2,name='Bob',order_id=None,created_at=None,amount_cents=None),dict(customer_id=3,name='陈',order_id=6,created_at='2025-02-01T00:00:00Z',amount_cents=500),dict(customer_id=4,name='空',order_id=None,created_at=None,amount_cents=None)],
 't22':[dict(product_id=1,name='A',buyers=2,repeat_buyers=1,rate='0.5000'),dict(product_id=2,name='B',buyers=2,repeat_buyers=0,rate='0.0000'),dict(product_id=3,name='零销量',buyers=0,repeat_buyers=0,rate='0.0000'),dict(product_id=4,name='退款',buyers=0,repeat_buyers=0,rate='0.0000')],
 't25':dict(top_ips=[['10.0.0.1',3],['10.0.0.2',3],['10.0.0.3',2]],count_5xx=4,bad_lines=3),
 't26':dict(failures=[dict(job='test',step='integration',exit_code=137,source='ci/test.log:2'),dict(job='test',step='unit',exit_code=1,source='ci/test.log:1')],bad_lines=1),
 't27':dict(routes=[dict(route='/health',count=2,p95_ms=5,error_rate='0.5000'),dict(route='/items',count=20,p95_ms=19,error_rate='0.0500')],bad_rows=4),
 't28':dict(traces=[dict(trace_id='T1',first_failed_span='c',service='database',source='services/db.jsonl:1',chain=['a','c']),dict(trace_id='T3',first_failed_span='y',service='database',source='services/db.jsonl:2',chain=['z','y'])]),
 't29':dict(failed=[dict(job_id='J2',attempts=2,last_attempt=2,source='worker/b.jsonl:1'),dict(job_id='J5',attempts=2,last_attempt=2,source='worker/b.jsonl:3')],succeeded=2,unfinished=1),
 't30':dict(candidates=[dict(path='.old-cache',size=5),dict(path='nested/stale.txt',size=6),dict(path='old.log',size=3)],total_bytes=14),
}


def _verify_tree(d):
    cutoff=dt.datetime(2025,3,1,tzinfo=dt.timezone.utc).timestamp()
    specs={'old.log':('abc',-1),'boundary.log':('xxxx',0),'new.log':('new',1),'.old-cache':('12345',-20),'keep/old.log':('secret',-100),'nested/stale.txt':('stale!',-100)}
    links={'link-old':'old.log','link-outside':'../policy.json','link-dir':'nested'}
    actual=set()
    for root,dirs,files in os.walk(d/'tree',followlinks=False):
        for name in files+dirs:
            p=Path(root)/name
            if p.is_symlink() or p.is_file():actual.add(p.relative_to(d/'tree').as_posix())
    assert actual==set(specs)|set(links),'dry-run input tree paths changed'
    for name,(content,age) in specs.items():
        p=d/'tree'/name
        assert not p.is_symlink() and p.read_text()==content,'dry-run input file changed'
        assert p.stat().st_mtime==cutoff+age,'dry-run input mtime changed'
    for name,target in links.items():
        p=d/'tree'/name
        assert p.is_symlink() and os.readlink(p)==target,'dry-run symlink changed'


def _exact(a,b):
    if type(a) is not type(b):
        # JSON number int/float interchangeable except bool; business integer fields remain strict.
        return isinstance(a,(int,float)) and not isinstance(a,bool) and isinstance(b,float) and a==b
    if isinstance(a,dict): return a.keys()==b.keys() and all(_exact(a[k],b[k]) for k in a)
    if isinstance(a,list): return len(a)==len(b) and all(_exact(x,y) for x,y in zip(a,b))
    return a==b


def _query(tid,d,sql,hidden=False):
    c=sqlite3.connect(':memory:');c.row_factory=sqlite3.Row;c.executescript(SQL_SCHEMAS[tid])
    if hidden:
        if tid=='t19':c.executescript("INSERT INTO customers VALUES(10,'隐藏'); INSERT INTO orders VALUES(100,10,123456,'paid','2025-10-01');")
        elif tid=='t20':c.execute('DELETE FROM orders')
        elif tid=='t21':c.execute("INSERT INTO orders VALUES(100,2,'paid','2025-12-31T00:00:00Z',789)")
        elif tid=='t22':c.executescript("INSERT INTO orders VALUES(100,4,'paid'),(101,4,'paid'); INSERT INTO order_items VALUES(100,3,1),(101,3,1);")
        elif tid=='t24':c.executescript("INSERT INTO customers VALUES(8,'Hidden'); INSERT INTO orders VALUES(100,8,'paid',777),(101,8,'paid',777); INSERT INTO refunds VALUES(100,100,1),(101,100,2); INSERT INTO order_tags VALUES(100,'a'),(100,'b'),(101,'c');")
    # Restrict writes/DDL/PRAGMA/ATTACH; bounded VM work prevents runaway recursive queries.
    denied={sqlite3.SQLITE_INSERT,sqlite3.SQLITE_UPDATE,sqlite3.SQLITE_DELETE,sqlite3.SQLITE_ATTACH,sqlite3.SQLITE_DETACH,sqlite3.SQLITE_PRAGMA,sqlite3.SQLITE_CREATE_TABLE,sqlite3.SQLITE_DROP_TABLE,sqlite3.SQLITE_ALTER_TABLE}
    c.set_authorizer(lambda action,*args:sqlite3.SQLITE_DENY if action in denied else sqlite3.SQLITE_OK)
    budget=[0]
    def progress():budget[0]+=1;return int(budget[0]>1000)
    c.set_progress_handler(progress,1000)
    try:return [dict(row) for row in c.execute(sql)]
    finally:c.close()


def _migration_state(path):
    c=sqlite3.connect(path)
    try:
        columns=c.execute('PRAGMA table_info(users)').fetchall()
        assert [x[1] for x in columns]==['id','display_name','email','email_key'],'users columns differ'
        assert columns[0][5]==1,'id must remain PK'
        assert not c.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='users_old'").fetchall(),'temporary table remains'
        order_columns=c.execute('PRAGMA table_info(orders)').fetchall()
        assert [(x[1],x[2].upper(),x[3],x[5]) for x in order_columns]==[('id','INTEGER',0,1),('user_id','INTEGER',0,0),('total_cents','INTEGER',1,0)],'orders columns, types, constraints or primary key changed'
        indexes=c.execute('PRAGMA index_list(users)').fetchall();idx=[x for x in indexes if x[1]=='idx_users_email_key']
        assert idx and idx[0][2]==0,'missing nonunique index'
        assert [x[2] for x in c.execute('PRAGMA index_info(idx_users_email_key)')]==['email_key'],'wrong indexed columns'
        fks=c.execute('PRAGMA foreign_key_list(orders)').fetchall()
        assert any(x[2]=='users' and x[3]=='user_id' and x[4]=='id' for x in fks),'foreign key lost'
        assert not c.execute('PRAGMA foreign_key_check').fetchall(),'foreign key violation'
        return c.execute('SELECT * FROM users ORDER BY id').fetchall(),c.execute('SELECT * FROM orders ORDER BY id').fetchall()
    finally:c.close()


def _logical_db_snapshot(path):
    """Schema and row snapshot; avoids equating SQLite physical page bytes."""
    with sqlite3.connect(path) as connection:
        schema=connection.execute("SELECT type,name,tbl_name,sql FROM sqlite_master ORDER BY type,name").fetchall()
        contents={}
        for kind,name,_,_ in schema:
            if kind!='table':continue
            quoted='"'+name.replace('"','""')+'"'
            contents[name]=sorted(connection.execute('SELECT * FROM '+quoted).fetchall(),key=repr)
        return schema,contents


def _verify_migration(d):
    expected_users=[(1,'Alice',' Alice@Example.COM ','alice@example.com'),(2,'张三',None,None),(3,'Another','alice@example.com','alice@example.com')]
    expected_orders=[(10,1,1200),(11,2,3400),(12,3,0)]
    assert _migration_state(d/'legacy.db')==(expected_users,expected_orders),'existing DB contents differ'
    # Run submitted migration on a fresh alternate database, twice, without modifying delivery.
    with tempfile.TemporaryDirectory() as tmp:
        p=Path(tmp)/'alternate.db';c=sqlite3.connect(p);c.executescript(SQL_SCHEMAS['t23']);c.execute("INSERT INTO users VALUES(17,'Extra',' X@Y.COM ')");c.execute('INSERT INTO orders VALUES(99,17,123)');c.commit();c.close()
        for _ in range(2):
            r=subprocess.run([sys.executable,'-I',str((d/'migrate.py').resolve()),str(p)],cwd=tmp,capture_output=True,text=True,timeout=10)
            assert r.returncode==0,'migration failed: '+r.stderr[-200:]
            assert _migration_state(p)==(expected_users+[(17,'Extra',' X@Y.COM ','x@y.com')],expected_orders+[(99,17,123)]),'hidden migration differs'
    # A table occupying the required index name forces an incompatibility. An
    # atomic migration must fail without persisting earlier ALTER/UPDATE work.
    # A preflight rejection also satisfies this contract; do not require a
    # particular migration algorithm or depend on an UPDATE-specific trigger.
    with tempfile.TemporaryDirectory() as tmp:
        p=Path(tmp)/'incompatible-index.db'
        with sqlite3.connect(p) as connection:
            connection.executescript(SQL_SCHEMAS['t23'])
            connection.executescript('CREATE TABLE idx_users_email_key(marker TEXT); INSERT INTO idx_users_email_key VALUES("must remain");')
        before=_logical_db_snapshot(p)
        r=subprocess.run([sys.executable,'-I',str((d/'migrate.py').resolve()),str(p)],cwd=tmp,capture_output=True,text=True,timeout=10)
        assert r.returncode!=0,'migration must reject incompatible existing index name'
        assert _logical_db_snapshot(p)==before,'failed migration left partial schema/data changes'
    return True,'schema, index, foreign keys, original/hidden data, idempotence and atomic failure verified' 


def verify(tid: str, d: Path):
    d=Path(d)
    try:
        if tid=='t23':return _verify_migration(d)
        if tid in ('t19','t20','t21','t22','t24'):
            sql=(d/'query.sql').read_text(encoding='utf-8')
            actual=_query(tid,d,sql)
            reference=_query(tid,d,QUERIES[tid])
            assert _exact(actual,reference),'SQL original dataset differs'
            assert _exact(_query(tid,d,sql,True),_query(tid,d,QUERIES[tid],True)),'SQL hidden dataset differs'
            if tid=='t24':return True,'SQL passes original and hidden multi-join data'
        if tid=='t30':_verify_tree(d)
        actual=json.loads((d/'result.json').read_text(encoding='utf-8'))
        assert _exact(actual,GOLD[tid]),'result.json differs from exact typed expected output'
        return True,'all output fields and boundary cases match'
    except (OSError,ValueError,AssertionError,KeyError,sqlite3.Error,subprocess.TimeoutExpired) as e:
        return False,f'{type(e).__name__}: {e}'


QUERIES = {
 't19': "SELECT c.id customer_id,c.name,SUM(o.amount_cents) total_cents FROM customers c JOIN orders o ON o.customer_id=c.id WHERE o.status='paid' AND o.created_at>='2025-01-01' AND o.created_at<'2026-01-01' GROUP BY c.id,c.name ORDER BY total_cents DESC,c.id LIMIT 3",
 't20': "SELECT c.id customer_id,c.name FROM customers c WHERE c.is_test=0 AND NOT EXISTS(SELECT 1 FROM orders o WHERE o.customer_id=c.id) ORDER BY c.id",
 't21': "WITH ranked AS (SELECT *,ROW_NUMBER() OVER(PARTITION BY customer_id ORDER BY created_at DESC,id DESC) rn FROM orders WHERE status!='cancelled') SELECT c.id customer_id,c.name,o.id order_id,o.created_at,o.amount_cents FROM customers c LEFT JOIN ranked o ON o.customer_id=c.id AND o.rn=1 ORDER BY c.id",
 't22': "WITH counts AS (SELECT i.product_id,o.customer_id,COUNT(DISTINCT o.id) n FROM order_items i JOIN orders o ON o.id=i.order_id WHERE o.status='paid' GROUP BY i.product_id,o.customer_id) SELECT p.id product_id,p.name,COUNT(c.customer_id) buyers,COALESCE(SUM(CASE WHEN c.n>=2 THEN 1 ELSE 0 END),0) repeat_buyers FROM products p LEFT JOIN counts c ON c.product_id=p.id GROUP BY p.id,p.name ORDER BY p.id",
 't24': "WITH r AS (SELECT order_id,SUM(amount_cents) total FROM refunds GROUP BY order_id),n AS (SELECT o.customer_id,SUM(o.amount_cents-COALESCE(r.total,0)) net FROM orders o LEFT JOIN r ON r.order_id=o.id WHERE o.status='paid' GROUP BY o.customer_id) SELECT c.id customer_id,c.name,COALESCE(n.net,0) net_cents FROM customers c LEFT JOIN n ON n.customer_id=c.id ORDER BY c.id",
}


MIGRATION = """import sqlite3,sys
c=sqlite3.connect(sys.argv[1])
c.execute('PRAGMA foreign_keys=ON')
try:
    c.execute('BEGIN IMMEDIATE')
    names=[r[1] for r in c.execute('PRAGMA table_info(users)')]
    if names==['id','name','email']:
        c.execute('ALTER TABLE users RENAME COLUMN name TO display_name')
        c.execute('ALTER TABLE users ADD COLUMN email_key TEXT')
    elif names!=['id','display_name','email','email_key']:
        raise ValueError('unsupported schema')
    c.execute('UPDATE users SET email_key=lower(trim(email))')
    c.execute('CREATE INDEX IF NOT EXISTS idx_users_email_key ON users(email_key)')
    c.commit()
except BaseException:
    c.rollback()
    raise
finally:
    c.close()
"""


def solve(tid: str, d: Path):
    """Offline reference implementation, never used in the scored agent run."""
    d=Path(d)
    if tid in QUERIES:
        (d/'query.sql').write_text(QUERIES[tid]+';\n',encoding='utf-8')
        if tid=='t24':return
        out=_query(tid,d,QUERIES[tid])
        if tid=='t19':out=[dict(customer_id=r['customer_id'],name=r['name'],total=_money(D(r['total_cents'])/100)) for r in out]
        if tid=='t22':
            for r in out:r['rate']=_rate(r['repeat_buyers'],r['buyers'])
    elif tid=='t23':
        (d/'migrate.py').write_text(MIGRATION,encoding='utf-8')
        subprocess.run([sys.executable,'-I',str((d/'migrate.py').resolve()),str((d/'legacy.db').resolve())],check=True,capture_output=True,timeout=10)
        return
    else:
        out=CALCULATORS[tid](d)
    _jwrite(d/'result.json',out)


def _rate(a,b):
    return str((D(a)/D(b) if b else D(0)).quantize(D('.0001'),rounding=decimal.ROUND_HALF_UP))


def _sales(d):
    by=collections.defaultdict(D);prod=collections.defaultdict(D);bad=0
    for r in _readcsv(d,'sales.csv'):
        try:
            date=dt.date.fromisoformat(r['date']);q=int(r['qty']);p=D(r['unit_price'])
            if q<0 or not p.is_finite() or p<0:raise ValueError('invalid numeric')
        except (ValueError,decimal.InvalidOperation):bad+=1;continue
        if date.year!=2025:continue
        amount=q*p;by[r['region']]+=amount;prod[r['product']]+=amount
    return dict(by_region={k:_money(v) for k,v in by.items()},top_product=min(prod,key=lambda k:(-prod[k],k)),skipped_rows=bad)


def _funnel(d):
    events=collections.defaultdict(list)
    for r in _readcsv(d,'funnel.csv'):events[(r['channel'],r['user'])].append((r['ts'],r['event']))
    channels=[]
    for channel in sorted(json.loads((d/'channels.json').read_text())):
        visitors=converted=0
        for (ch,user),rows in events.items():
            if ch!=channel:continue
            visits=[t for t,e in rows if e=='visit']
            if not visits:continue
            visitors+=1;converted+=any(e=='buy' and t>=min(visits) for t,e in rows)
        channels.append(dict(channel=channel,visitors=visitors,converted=converted,rate=_rate(converted,visitors)))
    return dict(channels=channels)


def _reconcile(d):
    sides=[];dups=[]
    for file in ['payments.csv','bank.csv']:
        values=collections.defaultdict(set);dup=0
        for r in _readcsv(d,file):
            amount=D(r['amount']);seen=values[r['transaction_id']]
            if amount in seen:dup+=1
            seen.add(amount)
        sides.append(values);dups.append(dup)
    p,b=sides;conflicts=sorted(k for k in p.keys()|b.keys() if len(p.get(k,set()))>1 or len(b.get(k,set()))>1)
    out=dict(matched=[],amount_mismatches=[],payment_only=[],bank_only=[],conflicts=conflicts,duplicate_rows=dict(payments=dups[0],bank=dups[1]))
    for k in sorted((p.keys()|b.keys())-set(conflicts)):
        if k not in p:out['bank_only'].append(k)
        elif k not in b:out['payment_only'].append(k)
        elif p[k]==b[k]:out['matched'].append(k)
        else:out['amount_mismatches'].append(dict(id=k,payment=_money(next(iter(p[k]))),bank=_money(next(iter(b[k])))))
    return out


def _subscriptions(d):
    day=json.loads((d/'observation.json').read_text())['date'];plans={}
    for r in _readcsv(d,'subscriptions.csv'):
        p=plans.setdefault(r['plan'],dict(plan=r['plan'],eligible=0,active=0,churned=0,renewed=0))
        if r['start']>day:continue
        p['eligible']+=1;p['active']+=r['end']>=day;p['churned']+=r['end']<day;p['renewed']+=r['renewed']=='true'
    for p in plans.values():p['churn_rate']=_rate(p['churned'],p['eligible'])
    return dict(by_plan=[plans[k] for k in sorted(plans)])


def _inventory(d):
    groups=collections.defaultdict(list)
    for file in ['snapshots/a.csv','snapshots/b.csv']:
        for r in _readcsv(d,file):groups[(r['warehouse'],r['sku'])].append(r)
    out=dict(inventory=[],conflicts=[],negative_stock=[])
    for (warehouse,sku),rows in sorted(groups.items()):
        newest=max(r['updated_at'] for r in rows);qty=sorted(set(int(r['qty']) for r in rows if r['updated_at']==newest))
        if len(qty)>1:out['conflicts'].append(dict(warehouse=warehouse,sku=sku,updated_at=newest,quantities=qty));continue
        out['inventory'].append(dict(warehouse=warehouse,sku=sku,qty=qty[0],updated_at=newest))
        if qty[0]<0:out['negative_stock'].append(dict(warehouse=warehouse,sku=sku,qty=qty[0]))
    return out


def _profits(d):
    rates=_readcsv(d,'rates.csv');orders=_readcsv(d,'orders.csv');by_id={r['id']:r for r in orders};months={};missing=[]
    def rate(currency,date):
        if currency=='CNY':return D(1)
        found=[r for r in rates if r['currency']==currency and r['date']<=date]
        return D(max(found,key=lambda r:r['date'])['cny_rate']) if found else None
    events=[('order',r,r['currency']) for r in orders]+[('refund',r,by_id[r['order_id']]['currency']) for r in _readcsv(d,'refunds.csv')]
    for kind,r,currency in events:
        fx=rate(currency,r['date'])
        if fx is None:missing.append(dict(kind=kind,id=r['id'],currency=currency,date=r['date']));continue
        m=months.setdefault(r['date'][:7],dict(revenue=D(0),refunds=D(0),cost=D(0)))
        if kind=='order':m['revenue']+=D(r['amount'])*fx;m['cost']+=D(r['cost'])*fx
        else:m['refunds']+=D(r['amount'])*fx
    rows=[]
    for month,m in sorted(months.items()):
        m['profit']=m['revenue']-m['refunds']-m['cost'];rows.append(dict(month=month,**{k:_money(v) for k,v in m.items()}))
    return dict(months=rows,missing_rates=sorted(missing,key=lambda x:(x['kind'],x['id'])))


def _logs(d):
    import ipaddress
    ips=collections.Counter();bad=errors=0
    pattern=re.compile(r'^(\S+) - - \[([^\]]+)\] "[^"\n]+" (\d{3}) \d+ "[^"\n]*" "[^"\n]*"$')
    for line in (d/'access.log').read_text().splitlines():
        m=pattern.fullmatch(line)
        try:
            if not m:raise ValueError()
            ipaddress.IPv4Address(m[1]);status=int(m[3])
            if not 100<=status<=599:raise ValueError()
        except ValueError:bad+=1;continue
        ips[m[1]]+=1;errors+=status>=500
    return dict(top_ips=[[ip,n] for ip,n in sorted(ips.items(),key=lambda x:(-x[1],x[0]))[:3]],count_5xx=errors,bad_lines=bad)


def _ci(d):
    latest={};bad=0
    for name in ['ci/build.log','ci/test.log']:
        for n,line in enumerate((d/name).read_text().splitlines(),1):
            try:r=json.loads(line)
            except ValueError:bad+=1;continue
            if r['event']!='step_end':continue
            key=(r['job'],r['step']);order=(r['ts'],name,n)
            if key not in latest or order>latest[key][0]:latest[key]=(order,r,f'{name}:{n}')
    return dict(failures=[dict(job=k[0],step=k[1],exit_code=r['exit_code'],source=source) for k,(_,r,source) in sorted(latest.items()) if r['exit_code']!=0],bad_lines=bad)


def _latencies(d):
    groups=collections.defaultdict(list);bad=0
    for line in (d/'requests.jsonl').read_text().splitlines():
        try:
            r=json.loads(line);route=r['route'];status=r['status'];latency=r['latency_ms']
            if not isinstance(route,str) or not route.startswith('/') or type(status) is not int or not 100<=status<=599 or type(latency) not in (int,float) or not math.isfinite(latency) or latency<0:raise ValueError()
        except (ValueError,KeyError,TypeError):bad+=1;continue
        groups[route.split('?',1)[0]].append((status,latency))
    return dict(routes=[dict(route=route,count=len(rows),p95_ms=sorted(x[1] for x in rows)[math.ceil(.95*len(rows))-1],error_rate=_rate(sum(x[0]>=500 for x in rows),len(rows))) for route,rows in sorted(groups.items())],bad_rows=bad)


def _traces(d):
    groups=collections.defaultdict(dict)
    for name in ['services/api.jsonl','services/db.jsonl']:
        for n,line in enumerate((d/name).read_text().splitlines(),1):
            r=json.loads(line);r['source']=f'{name}:{n}';groups[r['trace_id']][r['span_id']]=r
    out=[]
    for trace,spans in sorted(groups.items()):
        errors=[r for r in spans.values() if r['status']=='error']
        if not errors:continue
        first=min(errors,key=lambda x:(x['start'],x['span_id']));chain=[];p=first
        while p:
            chain.append(p['span_id']);p=spans.get(p['parent_id'])
        out.append(dict(trace_id=trace,first_failed_span=first['span_id'],service=first['service'],source=first['source'],chain=chain[::-1]))
    return dict(traces=out)


def _jobs(d):
    jobs=collections.defaultdict(list)
    for name in ['worker/a.jsonl','worker/b.jsonl']:
        for n,line in enumerate((d/name).read_text().splitlines(),1):
            r=json.loads(line);jobs[r['job_id']].append(((r['ts'],r['attempt'],name,n),r,f'{name}:{n}'))
    out=dict(failed=[],succeeded=0,unfinished=0)
    for job,rows in sorted(jobs.items()):
        _,last,source=max(rows,key=lambda x:x[0])
        if last['status']=='failed':out['failed'].append(dict(job_id=job,attempts=len(set(r['attempt'] for _,r,_ in rows)),last_attempt=last['attempt'],source=source))
        elif last['status']=='succeeded':out['succeeded']+=1
        else:out['unfinished']+=1
    return out


def _cleanup(d):
    policy=json.loads((d/'policy.json').read_text());cutoff=dt.datetime.fromisoformat(policy['now'].replace('Z','+00:00')).timestamp()-policy['retention_days']*86400;out=[]
    for root,dirs,files in os.walk(d/'tree',followlinks=False):
        dirs[:]=[x for x in dirs if not (Path(root)/x).is_symlink()]
        for name in files:
            p=Path(root)/name;rel=p.relative_to(d/'tree').as_posix()
            if p.is_symlink() or not p.is_file() or any(rel.startswith(prefix) for prefix in policy['protected_prefixes']):continue
            stat=p.stat()
            if stat.st_mtime<cutoff:out.append(dict(path=rel,size=stat.st_size))
    return dict(candidates=sorted(out,key=lambda x:x['path']),total_bytes=sum(x['size'] for x in out))


CALCULATORS = dict(t13=_sales,t14=_funnel,t15=_reconcile,t16=_subscriptions,t17=_inventory,t18=_profits,t25=_logs,t26=_ci,t27=_latencies,t28=_traces,t29=_jobs,t30=_cleanup)
