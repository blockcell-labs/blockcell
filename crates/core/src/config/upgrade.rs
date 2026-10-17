//! 自升级（auto-upgrade）配置类型。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoUpgradeConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_upgrade_channel")]
    pub channel: String,
    #[serde(default = "default_manifest_url")]
    pub manifest_url: String,
    #[serde(default = "default_require_signature")]
    pub require_signature: bool,
    /// 可信 Ed25519 公钥（32 字节十六进制）。发布构建也可通过
    /// BLOCKCELL_UPDATER_PUBLIC_KEY 在编译期嵌入。
    #[serde(
        default = "default_upgrade_public_key",
        skip_serializing_if = "Option::is_none"
    )]
    pub public_key: Option<String>,
    #[serde(default)]
    pub maintenance_window: String,
}

impl Default for AutoUpgradeConfig {
    fn default() -> Self {
        // 与 serde 字段默认保持一致；尤其是 require_signature 必须默认开启，
        // 否则当配置中缺失整个 autoUpgrade 段时会退化为派生默认的 false。
        Self {
            enabled: false,
            channel: default_upgrade_channel(),
            manifest_url: default_manifest_url(),
            require_signature: default_require_signature(),
            public_key: default_upgrade_public_key(),
            maintenance_window: String::new(),
        }
    }
}

fn default_upgrade_channel() -> String {
    "stable".to_string()
}

fn default_require_signature() -> bool {
    // Fail closed: without an embedded key this used to default to false, which
    // let `autoUpgrade.enabled` accept SHA256-only updates. Requiring a
    // signature with no key errors out in verify_signature instead; users who
    // intentionally want unsigned updates can still set requireSignature=false.
    true
}

fn default_upgrade_public_key() -> Option<String> {
    option_env!("BLOCKCELL_UPDATER_PUBLIC_KEY")
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(ToOwned::to_owned)
}

fn default_manifest_url() -> String {
    "https://github.com/blockcell-labs/blockcell/releases/latest/download/manifest.json".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_requirement_defaults_on_even_without_a_trusted_key() {
        let config = AutoUpgradeConfig::default();

        // Fail closed: missing a key means verification errors out, never that
        // unsigned updates are accepted silently.
        assert!(config.require_signature);
    }
}
