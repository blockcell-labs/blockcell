# intentRouter Multi-Profile Guide

`intentRouter` is now the **single configuration entry point** for intent-to-tool mapping in blockcell.

## Recommended pattern

- Bind each agent with `agents.list[].intentProfile`
- Define reusable tool sets under `intentRouter.profiles`
- Put shared baseline tools in `coreTools`
- Add intent-specific tools in `intentTools`
- Remove disallowed tools with `denyTools`
- Explicitly configure `Chat` as `{ "inheritBase": false, "tools": [] }`
- Always configure `Unknown`

## Minimal multi-agent / multi-profile example

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

## Resolution order

The final tool set is computed in this order:

1. Choose the profile for the current agent
2. Merge `coreTools` and `intentTools` for the classified intents
3. Add runtime-required tools such as ghost-required tools
4. Apply `denyTools`
5. Apply disabled entries from `toggles.json`

## Compatibility behavior

- If `intentRouter` is missing, blockcell injects the built-in default router automatically
- If `intentRouter.enabled = false`, runtime falls back to that profile's `Unknown` tool set instead of the old hardcoded map
- `agents.list[].intentProfile` takes precedence over `intentRouter.agentProfiles`; the latter mainly exists for backward compatibility

## Troubleshooting

Start with:

```bash
blockcell status
blockcell doctor
```

Check:

- which profile each agent resolves to
- whether `intentRouter` validation passes
- whether any tool names are invalid or unregistered
