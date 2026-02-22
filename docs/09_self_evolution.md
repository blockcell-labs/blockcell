# 第09篇：自我进化 —— AI 如何自动写代码升级自己

> 系列文章：《blockcell 开源项目深度解析》第 9/14 篇

---

## 一个大胆的想法

大多数软件都是静态的：你安装了什么版本，它就一直是那个版本，直到你手动更新。

blockcell 有一个大胆的设计目标：**让 AI 自己发现问题，自己写代码修复，自己测试，自己发布。**

这就是"自我进化"（Self-Evolution）系统。

---

## 进化的触发条件

进化不是随机发生的，它由**错误模式**触发。

```
错误追踪器（ErrorTracker）：
- 监控每个技能的执行结果
- 在时间窗口内统计错误次数
- 当错误次数超过阈值，触发进化
```

举个例子：

```
stock_monitor 技能在过去 1 小时内失败了 3 次
→ ErrorTracker 判断：需要进化
→ 创建进化记录，状态：Triggered
```

---

## 进化流水线

进化流程分为 6 个阶段：

```
Triggered → Generating → Auditing → Compiling → Testing → RollingOut
                                                              ↓ 失败
                                                           Rolled Back
```

### 阶段一：Generating（生成）

系统向 LLM 发送一个特殊的提示词：

```
你是一个 Rhai 脚本专家。
当前技能 stock_monitor 出现了以下错误：
  - 错误1：finance_api 返回空数据时未处理
  - 错误2：网络超时时没有重试逻辑

请生成一个修复版本的 SKILL.rhai 脚本。
原始代码：[原始 SKILL.rhai 内容]
错误历史：[最近 3 次错误详情]
```

LLM 生成新版本的代码，保存为补丁（patch）。

### 阶段二：Auditing（审计）

系统对生成的代码进行静态审计：
- 检查是否有危险操作（如删除文件、网络外发）
- 检查代码结构是否合理
- 检查是否处理了已知的错误场景

如果审计不通过，进入**重试循环**（最多 3 次）：

```
审计失败 → 把失败原因作为 Feedback → 重新让 LLM 生成 → 再次审计
```

### 阶段三：Compiling（编译）

Rhai 脚本会被 Rhai 引擎预编译，检查语法错误：

```rust
let engine = Engine::new();
engine.compile(&new_code)?;  // 语法错误在这里捕获
```

编译失败同样会触发重试，把编译错误作为 Feedback 传给 LLM。

### 阶段四：Testing（测试）

使用技能目录里的测试用例（`tests/` 目录）进行干跑测试：

```
skills/stock_monitor/tests/
├── test_basic_quote.json      # 基础查询测试
├── test_error_handling.json   # 错误处理测试
└── test_network_timeout.json  # 超时处理测试
```

每个测试用例包含输入和期望输出，新代码必须通过所有测试。

### 阶段五：RollingOut（灰度发布）

测试通过后，不是直接全量发布，而是**灰度发布**：

```
第1阶段：10% 的请求使用新版本
等待 10 分钟，观察错误率...

第2阶段：50% 的请求使用新版本
等待 10 分钟，观察错误率...

第3阶段：100% 的请求使用新版本
```

灰度期间，系统持续监控新版本的错误率。如果新版本的错误率高于旧版本，立即回滚。

---

## 重试与反馈机制

这是进化系统的关键设计：**失败不是终点，而是反馈。**

每次失败都会记录一个 `FeedbackEntry`：

```rust
struct FeedbackEntry {
    attempt: u32,           // 第几次尝试
    stage: String,          // 在哪个阶段失败
    feedback: String,       // 失败原因
    previous_code: String,  // 上一次的代码
    timestamp: i64,
}
```

下一次生成时，LLM 会看到完整的失败历史：

```
这是第 2 次尝试。
第 1 次失败原因：
  阶段：compile
  错误：第 15 行：变量 'price' 未定义
请修复上述问题，生成第 2 版代码。
```

这种机制让 LLM 能够从错误中学习，逐步改进代码质量。

---

## 版本管理

每次进化都会创建一个新版本：

```
~/.blockcell/workspace/
├── skills/
│   └── stock_monitor/
│       └── SKILL.rhai          # 当前版本
└── tool_versions/
    └── stock_monitor/
        ├── v1_2025-02-01.rhai  # 版本1
        ├── v2_2025-02-10.rhai  # 进化后
        └── v3_2025-02-18.rhai  # 最新
```

如果新版本出了问题，可以手动回滚：

```bash
blockcell evolve rollback stock_monitor
```

---

## 查看进化记录

```bash
blockcell evolve list

# 输出：
# SKILL           STATUS      ATTEMPT  CREATED
# stock_monitor   RolledOut   1        2025-02-10
# bond_monitor    Generating  2        2025-02-18
```

在对话中也可以查询：

```
你: 帮我看看哪些技能正在进化
AI: 正在进化的技能：
    - bond_monitor：第2次尝试，当前阶段：Auditing
```

---

## 进化系统的安全边界

自我进化需要安全边界：

1. **只能修改 Rhai 脚本**：不能修改 Rust 核心代码
2. **审计过滤**：生成的代码必须通过安全审计
3. **测试验证**：必须通过现有测试用例
4. **灰度发布**：有观察期，不直接全量替换
5. **自动回滚**：新版本变差时立即回滚
6. **版本保留**：所有历史版本都保留

---

## 生存不变量

系统会定期检查自己的"生存能力"：

```rust
struct SurvivalInvariants {
    can_compile: bool,            // 能编译代码吗？
    can_load_capabilities: bool,  // 能加载新能力吗？
    can_communicate: bool,        // 能联网吗？
    can_evolve: bool,             // 能自我进化吗？
}
```

如果任何一个不变量为 false，系统会记录警告并尝试修复。这确保了 blockcell 的核心能力不会因为某个错误而永久损坏。

---

## 实际效果

自我进化系统在以下场景特别有效：

- **API 变更**：数据源 API 格式改变，技能自动适配
- **边界情况**：特殊输入导致崩溃，自动添加校验逻辑
- **性能优化**：自动添加缓存、减少不必要的 API 调用

---

## 小结

blockcell 的自我进化系统是一个完整的 **AI 驱动的持续改进流水线**：

```
错误触发 → LLM 生成代码 → 审计 → 编译 → 测试 → 灰度发布 → 全量
              ↑ 失败反馈 ←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←
```

这让 blockcell 随着使用越来越可靠，真正实现了"越用越聪明"。
---

*上一篇：[Gateway 模式 —— 把 AI 变成一个服务](./08_gateway_mode.md)*
*下一篇：[金融场景实战 —— 用 blockcell 监控股票和加密货币](./10_finance_use_case.md)*

*项目地址：https://github.com/blockcell-labs/blockcell*
*官网：https://blockcell.dev*
