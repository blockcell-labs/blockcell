# 第09篇：自我进化 —— AI 如何自动写代码升级自己

> 系列文章：《blockcell 开源项目深度解析》第 9 篇
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

进化流程是一个状态机，按以下阶段推进：

```
Triggered → Generating → Generated → Auditing ─┬─ AuditFailed ────► 重试（最多 3 次）
                                               └─ AuditPassed
                                                     ↓
                                              CompilePassed ──► Observing ──┬─► Completed
                                                     │                       └─► RolledBack
                                                     └─ CompileFailed ──► 重试
```

> 历史记录里还可能出现 `DryRunPassed` / `TestPassed` / `RollingOut` 等旧状态名，
> 它们只用于反序列化兼容，读取时会被 `normalize()` 映射到当前状态。

### 阶段一：Generating（生成）

系统向 LLM 发送一个**按技能形态定制**的提示词（`PromptTool` / `LocalScript` /
`Hybrid` / `RhaiOrchestration` 四类形态各有对应模板），例如 Rhai 编排技能：

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

审计分**两层**：

1. **静态审计**（确定性规则）：扫描危险 shell 命令、危险 Python 目标、Rhai 禁用
   API、prompt 注入特征等，并对注释/字符串做 tokenizer 级区分（避免误报）。
2. **LLM 审计**：用独立会话审阅**应用补丁后的完整脚本**（而非 diff 文本），
   按技能形态选择对应审计提示词。

如果审计不通过，进入**重试循环**（最多 3 次）：

```
审计失败 → 把失败原因作为 Feedback → 重新让 LLM 生成 → 再次审计
```

### 阶段三：Compiling（编译）

编译检查按形态执行，语法错误在这一步捕获（不再有独立的"测试/干跑"阶段）：

| 形态 | 检查方式 |
|---|---|
| `RhaiOrchestration` / Hybrid(Rhai) | Rhai 引擎预编译 |
| Hybrid(Python) | `python3 -m py_compile` |
| `LocalScript` / Hybrid(LocalScript) | 脚本语法检查（含 shebang 判定） |
| `PromptTool` / Hybrid(PromptOnly) | SKILL.md 内容长度下限检查 |

```rust
let engine = Engine::new();
engine.compile(&new_code)?;  // Rhai 语法错误在这里捕获
```

编译失败同样会触发重试，把编译错误作为 Feedback 传给 LLM。

编译通过后还会做一次**契约检查**（`contract_check`）：校验 `meta.yaml` 的
`name` / `description`，以及 `SKILL.md` 的 `## Shared` / `## Prompt` 章节。

### 阶段四：Observing（部署与观察窗口）

契约检查通过后**直接全量部署**新版本，然后进入观察窗口（默认 60 分钟）：

- 观察期内每次调用都会记录成功/失败计数；
- 错误率超过阈值（默认 10%）**立即回滚**，优先于时长判定；
- 窗口到期且错误率正常 → 标记 `Completed`。

这里没有按百分比分流的多阶段灰度：是"全量部署 + 观察 + 自动回滚"模型。

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

每次进化都会在**技能目录内部**创建版本快照（不是独立的 `tool_versions/` 目录）：

```
~/.blockcell/workspace/skills/stock_monitor/
├── SKILL.rhai                 # 当前生效版本
├── version_history.json       # 版本历史（当前版本 + 快照索引）
└── versions/
    ├── v1/                    # 进化前的 baseline 快照
    ├── v2/                    # 进化后
    └── v3/                    # 最新
```

> 首次进化前会先建立 baseline 快照（`ensure_baseline`），否则回滚无处可去。
> 对 staged 新导入的技能，若只有 v1（没有 baseline），回滚会直接删除主技能目录
> 而不是恢复上一版本。

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
# stock_monitor   Completed   1        2025-02-10
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

1. **限定技能资产**：只能改技能自身的资产（`SKILL.md` / `SKILL.py` / `SKILL.rhai` /
   `scripts/`），不能改 Rust 核心代码；能力进化也只允许非 native 的
   `ProviderKind`（bash / python / Rhai）。
2. **双层审计**：生成的代码必须通过静态审计 + LLM 审计。
3. **编译与契约检查**：必须通过语法编译检查与 `meta.yaml` / `SKILL.md` 契约检查。
4. **观察窗口**：部署后有 60 分钟观察期，错误率超阈值即回滚（不是按百分比灰度）。
5. **自动回滚**：新版本变差时立即回滚到 baseline 快照。
6. **版本保留**：历史版本快照保留在技能目录的 `versions/` 下。

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
错误触发 → LLM 生成代码 → 静态审计 + LLM 审计 → 编译/契约检查 → 全量部署 → 观察窗口 → 完成
              ↑ 失败反馈 ←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←←（错误率超阈值则回滚）
```

这让 blockcell 随着使用越来越可靠，真正实现了"越用越聪明"。
---

*上一篇：[Gateway 模式 —— 把 AI 变成一个服务](./08_gateway_mode.md)*
*下一篇：[金融场景实战 —— 用 blockcell 监控股票和加密货币](./10_finance_use_case.md)*

*项目地址：https://github.com/blockcell-labs/blockcell*
*官网：https://blockcell.dev*
