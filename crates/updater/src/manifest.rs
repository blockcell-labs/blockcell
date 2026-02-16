use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub channel: String,
    pub version: String,
    pub published_at: String,
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub min_host_version: Option<String>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub os: String,
    pub arch: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub sig: Option<String>,
}

impl Manifest {
    pub fn get_artifact(&self, os: &str, arch: &str) -> Option<&Artifact> {
        self.artifacts
            .iter()
            .find(|a| a.os == os && a.arch == arch)
    }
}
