# 更新日志

所有值得注意的变更都会记录在此文件中。

格式基于 [Keep a Changelog](https://keepachangelog.com/en/1.0.0/)，
并遵循 [语义化版本](https://semver.org/spec/v2.0.0.html)。

## [0.1.5] - 2026-04-05

### 新增
- 统一技能运行时，支持 `rhai` 脚本执行链路。
- 增强技能版本管理、审计、演化与服务管理能力。
- 新增 Weixin、QQ 和 NapCatQQ 频道支持。
- 增加 memory 向量索引支持。
- 扩展 WebUI 在聊天、演化与连接状态方面的交互能力。
- 补充技能、CLI、provider 配置、MCP server 和路径访问策略等文档。

### 调整
- 简化技能执行模型与历史处理逻辑。
- 优化消息与工具调用流程，提升核心模块运行一致性。
- 增强 WeCom 长连接支持，并优化频道启动与状态展示。
- 优化 WebUI 聊天体验、消息渲染和前端测试覆盖。
- 更新定时任务的 cron / 时区处理逻辑。

### 修复
- 修复 provider 兼容性问题和部分配置边界情况。
- 修复 gateway / agent / scheduler 的稳定性问题以及重复输出问题。
- 修复路径解析、默认模型调用和媒体处理相关 bug。
- 修复 gateway 断开时 WebUI 卡顿问题。
- 修复 storage / tools / gateway 之间的 memory 规则不一致问题。

### 文档
- 补充并更新了大量中文与英文文档，覆盖技能、渠道、memory、provider 配置和 CLI 使用。
- 新增技能开发相关的 workflow / rules 文档。

## [0.1.4] - 2026-03-25

### 新增
- 当前追踪分支的首次公开版本。
- 核心 agent、provider、storage、scheduler、channels 和 skills 工作区 crate。
- 基础 WebUI 与 gateway 集成。
