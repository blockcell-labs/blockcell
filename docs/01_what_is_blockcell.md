# 第01篇：什么是 blockcell？一个会自我进化的 AI 智能体框架

> 系列文章：《blockcell 开源项目深度解析》第 1/14 篇

---

## 先说一个场景

你有没有遇到过这样的情况：

你用 ChatGPT 问了一个问题，它给了你一段代码，但你需要手动复制到编辑器里运行。然后运行出错了，你再把错误贴回去问它，它给你改好了，你再复制……

这个过程很繁琐。AI 明明"知道"怎么做，但它只能说，不能做。

**blockcell 想解决的，就是这个问题。**

---

## blockcell 是什么

blockcell 是一个用 **Rust** 编写的开源 AI 智能体框架。

名字里的 **Block + Cell**，分别代表稳定的模块化基座与可进化的能力单元（番外见：[*名字由来*](./14_name_origin.md)）。

一句话描述：**它让 AI 不只是聊天，而是真正能执行任务。**

```
你说："帮我分析一下桌面上的 sales.xlsx，画一张折线图"
blockcell：读文件 → 分析数据 → 调用 Python 画图 → 把图片路径告诉你
```

整个过程不需要你手动操作，AI 自己完成。

---

## 和普通 AI 聊天工具有什么区别

| 对比项 | ChatGPT/Claude 网页版 | blockcell |
|--------|----------------------|-----------|
| 能读写本地文件 | ❌ | ✅ |
| 能执行命令行 | ❌ | ✅ |
| 能操控浏览器 | ❌ | ✅ |
| 能发邮件/消息 | ❌ | ✅ |
| 有持久记忆 | 有限 | ✅ SQLite 全文搜索 |
| 能定时执行任务 | ❌ | ✅ Cron 调度 |
| 能接 Telegram/Slack | ❌ | ✅ |
| 能自我升级 | ❌ | ✅ 自我进化系统 |

---

## 核心架构：Rust 宿主 + Rhai 技能

blockcell 的架构分两层：

```
┌─────────────────────────────────────────────┐
│           Rust 宿主（TCB 可信计算基）          │
│  消息总线 | 工具注册 | 调度器 | 存储 | 审计    │
└─────────────────────────────────────────────┘
                      ↕
┌─────────────────────────────────────────────┐
│           Rhai 技能层（可变层）               │
│  stock_monitor | bond_monitor | 自定义技能   │
└─────────────────────────────────────────────┘
```

**Rust 宿主**是稳定的核心，负责安全、性能和基础能力。它不会轻易改变。

**Rhai 技能**是灵活的扩展层，可以随时添加、修改、甚至让 AI 自动生成新技能。

这个设计的好处是：核心稳定，扩展灵活。就像手机的操作系统和 App 的关系。

---

## 内置了哪些能力

blockcell 开箱即用，内置了 **50+ 工具**，覆盖：

**文件与系统**
- 读写文件、执行命令、目录操作
- 读取 Excel/Word/PDF/PPT

**网络与数据**
- 网页抓取（支持 Markdown 格式，节省 token）
- 浏览器自动化（CDP 协议，可操控真实 Chrome）
- HTTP 请求、WebSocket 订阅

**金融数据**
- A股/港股/美股实时行情（东方财富、Alpha Vantage）
- 加密货币价格（CoinGecko）
- 链上数据、DeFi、NFT

**通信**
- 发邮件（SMTP/IMAP）
- Telegram/Slack/Discord/飞书 消息

**多媒体**
- 截图、录音转文字（Whisper）
- 生成图表（matplotlib/plotly）
- 生成 PPT/Word/Excel

**AI 增强**
- 图片理解（GPT-4o/Claude/Gemini）
- 文字转语音（TTS）
- OCR 文字识别

---

## "自我进化"是什么意思

这是 blockcell 最独特的特性。

当 AI 在执行某个技能时反复出错，系统会自动：

1. 记录错误模式
2. 触发进化流程
3. 让 LLM 生成新版本代码
4. 自动审计、编译、测试
5. 灰度发布（先 10% 流量，再 50%，再 100%）
6. 如果新版本更差，自动回滚

```
错误触发 → LLM 生成新代码 → 审计 → 编译 → 测试 → 灰度发布 → 全量
                                                          ↓ 失败
                                                        自动回滚
```

这意味着 blockcell 会随着使用越来越聪明，自动修复自己的问题。

---

## 支持哪些 AI 模型

blockcell 支持所有 OpenAI 兼容的 API，以及原生支持：

- **OpenAI** (GPT-4o, GPT-4.1 等)
- **Anthropic** (Claude 系列)
- **Google Gemini**
- **DeepSeek**
- **Kimi/Moonshot**
- **Ollama**（本地模型，完全离线）
- **OpenRouter**（一个 key 访问所有模型）

---

## 为什么用 Rust 写

很多人会问：AI 框架不是应该用 Python 吗？

blockcell 选择 Rust 有几个原因：

1. **安全性**：Rust 的内存安全保证，AI 执行代码时不会有意外崩溃
2. **性能**：单机可以跑很多并发任务，不需要 Python 的 GIL 锁
3. **可靠性**：作为"可信计算基"，宿主层必须稳定可靠
4. **跨平台**：编译成单个二进制文件，macOS/Linux/Windows 都能跑

---

## 快速感受一下

```bash
# 安装
curl -fsSL https://raw.githubusercontent.com/blockcell-labs/blockcell/refs/heads/main/install.sh | sh

# 初始化
blockcell onboard

# 编辑配置，填入你的 API Key
# ~/.blockcell/config.json

# 启动对话
blockcell agent
```

然后你就可以说：
- "帮我搜索一下今天的 AI 新闻"
- "读一下桌面上的 report.pdf，总结一下"
- "帮我监控一下茅台的股价，跌破 1500 告诉我"

---

## 本系列文章目录

完整目录与推荐阅读顺序见索引页：

*索引：[系列目录](./00_index.md)*

---

## 小结

blockcell 不是一个聊天机器人，而是一个**可以真正执行任务的 AI 智能体框架**。

它的核心理念是：
- **工具化**：AI 通过工具与真实世界交互
- **技能化**：复杂任务封装成可复用的技能
- **进化**：系统能自动学习和改进
- **安全**：Rust 宿主提供可信的执行环境

如果你想让 AI 真正帮你干活，而不只是聊天，blockcell 值得一试。

---

*下一篇：[5分钟上手 blockcell —— 从安装到第一次对话](./02_quickstart.md)*

*项目地址：https://github.com/blockcell-labs/blockcell*
*官网：https://blockcell.dev*
