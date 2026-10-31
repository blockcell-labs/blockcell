# 目录访问安全策略

Blockcell 内置了可配置的**文件系统访问策略系统**，让你可以精确控制 AI 智能体能够读取、写入、列举或执行哪些路径——无需每次都手动确认。

---

## 核心概念

### 策略结果（PolicyAction）

| 结果 | 含义 |
|------|------|
| `allow` | 直接放行，无需用户确认 |
| `confirm` | 弹出确认请求，由用户决定 |
| `deny` | 直接拒绝，不会弹出确认 |

### 操作类型（ops）

| 操作 | 对应工具 |
|------|---------|
| `read` | `read_file` |
| `write` | `write_file`、`edit_file`、`file_ops`、`data_process`、`audio_transcribe`、`chart_generate`、`office_write`、`video_process`、`health_api`、`encrypt` 等 |
| `list` | `list_dir` |
| `exec` | `exec`（命令执行，`working_dir` 参数） |

### 判定优先级（从高到低）

1. **内置敏感路径保护**（`builtin_protected_paths: true`）→ 拒绝（`~/.ssh`、`/etc` 等）
2. **用户 `deny` 规则** → 拒绝
3. **用户 `allow` 规则**（若比 deny 规则更精确）→ 放行
4. **用户 `confirm` 规则** → 询问用户
5. **`default_policy`** → 默认行为（默认 `confirm`）

> 当前实现里，工作区路径放行和会话授权缓存属于更上层的运行时保护，不是这层 `path_access.json5` 规则文件本身的判定项。

> **最长前缀优先**：当 `deny` 和 `allow` 都匹配同一路径时，前缀更长（更具体）的规则胜出。

---

## 文件位置

| 文件 | 说明 |
|------|------|
| `~/.blockcell/config.json5` | 主配置，控制策略系统的开关和文件路径 |
| `~/.blockcell/path_access.json5` | 具体的路径规则（**首次启动自动生成示例**） |

---

## 主配置（config.json5）

```json5
{
  "security": {
    "pathAccess": {
      // 是否启用策略系统
      "enabled": true,

      // 规则文件路径（支持 ~/）
      "policyFile": "~/.blockcell/path_access.json5",

      // 规则文件缺失/解析失败时的行为
      // "fallback_to_safe_default" | "fail_closed" | "disabled"
      "missingFilePolicy": "fallback_to_safe_default"
    }
  }
}
```

---

## 规则文件（path_access.json5）

```json5
{
  version: 1,

  // 没有任何规则匹配时的默认行为
  default_policy: "confirm",

  // 用户确认过的目录在本次会话内自动放行（减少重复询问）
  cache_confirmed_dirs: true,

  // 始终拒绝内置敏感路径（~/.ssh、/etc 等）
  builtin_protected_paths: true,

  rules: [
    // ── 高优先级：拒绝敏感凭证目录 ────────────────────────────────────
    {
      name: "deny-secrets",
      action: "deny",
      ops: ["read", "write", "list", "exec"],
      paths: [
        "~/.ssh",
        "~/.aws",
        "~/.gnupg",
        "~/.kube",
        "~/.config/gcloud",
        "~/.azure",
        "~/.netrc",
        "/etc",
        "/System",
        "/private/etc",
        "/private/var",
        "/usr/bin",
        "/usr/sbin",
        "/bin",
        "/sbin"
      ]
    },

    // ── 允许开发目录直接访问，无需每次确认 ────────────────────────────
    {
      name: "allow-dev-roots",
      action: "allow",
      ops: ["read", "list", "write"],
      paths: [
        "~/dev",
        "~/projects",
        "~/Desktop",
        "~/Documents"
      ]
    },

    // ── 执行命令时始终需要确认（即使在已允许的目录中）────────────────
    {
      name: "confirm-all-exec",
      action: "confirm",
      ops: ["exec"],
      paths: ["~"]
    }
  ]
}
```

---

## 内置敏感路径

当 `builtin_protected_paths: true`（默认），以下路径**始终被拒绝**，无论用户规则如何：

```
~/.ssh          ~/.aws          ~/.gnupg        ~/.kube
~/.config/gcloud  ~/.azure      ~/.netrc
/etc            /System         /private/etc    /private/var
/usr/bin        /usr/sbin       /bin            /sbin
```

---

## 典型场景示例

### 场景 1：只允许特定项目目录，其余全拒

```json5
{
  default_policy: "deny",
  rules: [
    {
      name: "allow-my-project",
      action: "allow",
      ops: ["read", "write", "list"],
      paths: ["~/projects/my-app"]
    }
  ]
}
```

### 场景 2：读取任意路径，但写入需要确认

```json5
{
  default_policy: "confirm",
  rules: [
    {
      name: "allow-read-anywhere",
      action: "allow",
      ops: ["read", "list"],
      paths: ["~"]
    },
    {
      name: "confirm-write",
      action: "confirm",
      ops: ["write"],
      paths: ["~"]
    }
  ]
}
```

### 场景 3：完全关闭策略系统（退回旧行为）

在 `config.json5` 中：

```json5
{
  "security": {
    "pathAccess": {
      "enabled": false
    }
  }
}
```

---

## 更改后何时生效？

修改 `path_access.json5` 后，**重启 Blockcell** 即可生效（当前版本不支持热重载，`reload_on_change` 字段预留供未来使用）。

---

## 与会话授权缓存的关系

用户在会话中点击"允许"确认的目录，会被加入**本次会话的授权缓存**，在该会话剩余时间内不再询问。

策略系统的优先级高于会话缓存——如果某目录后来被策略标记为 `deny`，下次会话将直接拒绝，不会被旧缓存绕过。

缓存功能可通过 `cache_confirmed_dirs: false` 关闭，关闭后每次访问都会重新询问。

## 拒绝时返回什么

路径拒绝返回结构化 JSON，**一定会带上被拒的路径**，否则模型无法区分"真的越权"和"静态启发式误判"：

```json
{"error": "path access was denied by the user", "code": "PATH_ACCESS_DENIED",
 "paths": ["/xl/worksheets/sheet1.xml"], "scope": "invocation",
 "tool_available": true, "workspace": "/…/workspace", "alternative": "…"}
```

`code` 取值：`PATH_ACCESS_DENIED`（用户/策略拒绝）、`PATH_CONFIRMATION_UNAVAILABLE`（无确认渠道）、
`PATH_CONFIRMATION_TIMEOUT`、`NEEDS_CONFIRMATION_NONINTERACTIVE`（非交互场景需确认）。

路径抽取是**保守的静态启发式**：内联脚本里的 slash 开头字面量（zip 成员名 `/xl/…`、URL path、
正则）会被当作绝对路径。遇到这种误判，`alternative` 会提示"改写命令去掉前导斜杠，或把该字面量移出
命令"，模型据此修正即可，不必放弃任务。

