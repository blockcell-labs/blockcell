# intentRouter 多 Profile 配置指南

`intentRouter` 现在是 blockcell 中 **意图到工具映射的唯一配置入口**。

## 推荐做法

- 在 `agents.list[].intentProfile` 上绑定每个 agent 使用的 profile
- 在 `intentRouter.profiles` 中定义可复用工具集
- 用 `coreTools` 放共享基础工具
- 用 `intentTools` 按意图追加工具
- 用 `denyTools` 从最终结果里移除不允许暴露的工具
- 为 `Chat` 显式写 `{ "inheritBase": false, "tools": [] }`
- 始终配置 `Unknown`

## 最小多 Agent / 多 Profile 示例

```json
{
  "agents": {
    "list": [
      { "id": "default", "enabled": true, "intentProfile": "default" },
      { "id": "ops", "enabled": true, "intentProfile": "ops" }
    ]
  },
  "intentRouter": {
    "enabled": true,
    "defaultProfile": "default",
    "profiles": {
      "default": {
        "coreTools": ["read_file", "write_file", "list_dir", "exec", "message"],
        "intentTools": {
          "Chat": { "inheritBase": false, "tools": [] },
          "FileOps": ["edit_file", "file_ops", "office_write"],
          "WebSearch": ["browse", "http_request"],
          "Unknown": ["browse", "http_request"]
        }
      },
      "ops": {
        "coreTools": ["read_file", "list_dir", "exec", "message"],
        "intentTools": {
          "DevOps": ["git_api", "cloud_api", "network_monitor"],
          "Unknown": ["http_request"]
        },
        "denyTools": ["email", "social_media"]
      }
    }
  }
}
```

## 解析顺序

最终工具集合按下面顺序得到：

1. 选择 agent 对应的 profile
2. 根据 intent 合并 `coreTools` 与 `intentTools`
3. 叠加运行时强制工具（如 ghost 所需工具）
4. 应用 `denyTools`
5. 应用 `toggles.json` 中的禁用状态

## 兼容行为

- 如果 `intentRouter` 整段缺失，blockcell 会自动注入内置默认 router
- 如果 `intentRouter.enabled = false`，运行时会退回该 profile 的 `Unknown` 工具集，而不是旧硬编码映射
- `agents.list[].intentProfile` 的优先级高于 `intentRouter.agentProfiles`；后者主要用于兼容旧配置

## 排查建议

可以先运行：

```bash
blockcell status
blockcell doctor
```

重点看：

- 当前 agent 绑定到哪个 profile
- `intentRouter` 是否通过校验
- 是否引用了未注册的工具名
