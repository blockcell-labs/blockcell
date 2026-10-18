use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use blockcell_core::{mcp_config::McpRootConfig, Config, Paths};

fn unique_temp_base(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
}

#[test]
fn standalone_mcp_paths_are_derived_from_base_directory() {
    let paths = Paths::with_base(PathBuf::from("/tmp/blockcell"));

    assert_eq!(
        paths.mcp_config_file(),
        PathBuf::from("/tmp/blockcell/mcp.json")
    );
    assert_eq!(paths.mcp_dir(), PathBuf::from("/tmp/blockcell/mcp.d"));
    assert_eq!(
        paths.mcp_state_file(),
        PathBuf::from("/tmp/blockcell/mcp-state.json")
    );
}

#[test]
fn standalone_mcp_load_merged_prefers_mcp_d_entries() {
    let base = unique_temp_base("blockcell-core-mcp");
    let paths = Paths::with_base(base.clone());
    fs::create_dir_all(paths.mcp_dir()).expect("create mcp.d");

    fs::write(
        paths.mcp_config_file(),
        r#"{
  "defaults": {
    "startupTimeoutSecs": 21,
    "callTimeoutSecs": 65,
    "autoStart": true
  },
  "servers": {
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "enabled": true
    }
  }
}"#,
    )
    .expect("write mcp.json");

    fs::write(
        paths.mcp_dir().join("github.json"),
        r#"{
  "name": "github",
  "command": "uvx",
  "args": ["custom-github-server"],
  "enabled": true,
  "autoStart": false,
  "callTimeoutSecs": 90
}"#,
    )
    .expect("write github override");

    let resolved = blockcell_core::mcp_config::McpResolvedConfig::load_merged(&paths)
        .expect("merged standalone mcp config");
    let github = resolved.servers.get("github").expect("github server");

    assert_eq!(resolved.defaults.startup_timeout_secs, 21);
    assert_eq!(resolved.defaults.call_timeout_secs, 65);
    assert!(resolved.defaults.auto_start);
    assert_eq!(github.command, "uvx");
    assert_eq!(github.args, vec!["custom-github-server".to_string()]);
    assert!(!github.auto_start);
    assert_eq!(github.call_timeout_secs, 90);

    let _ = fs::remove_dir_all(base);
}

#[test]
fn standalone_mcp_is_not_embedded_in_main_config_json() {
    let json = serde_json::to_value(Config::default()).expect("serialize config");
    assert!(json.get("mcpServers").is_none());
}

#[test]
fn standalone_mcp_agent_permissions_inherit_and_override() {
    let raw = r#"{
  "agents": {
    "defaults": {
      "allowedMcpServers": ["github", "filesystem"],
      "allowedMcpTools": ["github__list_issues"]
    },
    "list": [
      {
        "id": "ops",
        "enabled": true,
        "allowedMcpServers": ["sqlite"]
      }
    ]
  }
}"#;

    let cfg: Config = serde_json::from_str(raw).expect("parse config with mcp permissions");
    let resolved = cfg.resolve_agent_spec("ops").expect("resolved ops agent");

    assert_eq!(
        resolved.defaults.allowed_mcp_servers,
        vec!["sqlite".to_string()]
    );
    assert_eq!(
        resolved.defaults.allowed_mcp_tools,
        vec!["github__list_issues".to_string()]
    );
}

#[cfg(unix)]
#[test]
fn standalone_mcp_save_atomically_replaces_the_target_inode() {
    let base = unique_temp_base("blockcell-core-mcp-atomic");
    fs::create_dir_all(&base).expect("create base");
    let path = base.join("mcp.json");
    let old_link = base.join("old-mcp.json");
    fs::write(&path, r#"{"servers":{"old":{"command":"old"}}}"#).expect("write old");
    fs::hard_link(&path, &old_link).expect("hard link old inode");

    McpRootConfig::default().save(&path).expect("atomic save");

    assert!(fs::read_to_string(&old_link)
        .expect("read old inode")
        .contains("old"));
    let _ = fs::remove_dir_all(base);
}

#[test]
fn mcp_server_with_empty_command_is_rejected() {
    let base = unique_temp_base("blockcell-core-mcp-empty-cmd");
    let paths = Paths::with_base(base.clone());
    fs::create_dir_all(paths.mcp_dir()).expect("create mcp.d");

    fs::write(
        paths.mcp_dir().join("bad.json"),
        r#"{"name": "bad", "command": "  "}"#,
    )
    .expect("write empty-command server file");

    let err = blockcell_core::mcp_config::McpResolvedConfig::load_merged(&paths)
        .expect_err("empty command must be rejected");
    assert!(err.to_string().contains("command cannot be empty"));

    let _ = fs::remove_dir_all(base);
}
