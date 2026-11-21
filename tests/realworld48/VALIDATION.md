# 准备阶段验证记录

本记录验证题库和运行器，不是实际模型/agent评测。

- 48 个任务：40 离线 + 8 本地 HTTP；简单/中等/困难各16。
- 完整自检：48/48，通过记录见 [SELF_CHECK.json](SELF_CHECK.json)。
- 38 项 unittest 回归通过：代码组4项、数据组7项、工程/资料/HTTP组10项、最终运行器17项。
- 覆盖隐藏输入、SQL换数据执行、事务故障、行为变异、敏感输出、HTTP请求轨迹、重试等待、受保护输入、超时、凭据脱敏、断点恢复、干预分账和真实子进程到本地HTTP服务的端到端链路。
- Python源文件编译检查通过。
- 已生成48份任务输入，真实agent尝试数为0，全部not_run；未调用模型API。

自检四项结果计数：

```json
{
  "baseline": {
    "fail": 48
  },
  "reference": {
    "pass": 48
  },
  "missing_delivery": {
    "fail": 48
  },
  "protected_mutation": {
    "fail": 40,
    "not_applicable": 8
  }
}
```

无受保护输入的任务在该项记not_applicable。参考解成功率仅用于确认判分能够接受正确交付，不能作为待测agent成绩。题库指纹见SELF_CHECK.json，与准备目录run.json一致。

测试环境需要允许监听127.0.0.1。本地测试没有访问公网；HTTP客户端使用http.client，不依赖当前环境的SSL启动补丁。实际API适配和远程工作区同步将在接口信息确定后完成。
