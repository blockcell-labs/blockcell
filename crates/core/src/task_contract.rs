//! Explicit task requirements. This data describes a task and never grants access.
use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationPolicy {
    #[default]
    Allowed,
    Required,
    Deferred,
    Forbidden,
}

impl VerificationPolicy {
    pub fn permits_execution(self) -> bool {
        matches!(self, Self::Allowed | Self::Required)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Deliverable {
    pub path: String,
    #[serde(default = "required_by_default")]
    pub required: bool,
    /// Require a new/updated file during this task, rather than any existing copy.
    #[serde(default)]
    pub must_update: bool,
}

fn required_by_default() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskContract {
    #[serde(default)]
    pub verification: VerificationPolicy,
    #[serde(default)]
    pub deliverables: Vec<Deliverable>,
}

impl TaskContract {
    pub fn validate(&self) -> crate::Result<()> {
        if self.deliverables.len() > 32 {
            return Err(crate::Error::Validation(
                "At most 32 deliverables are allowed".into(),
            ));
        }
        for item in &self.deliverables {
            let path = Path::new(&item.path);
            if item.path.is_empty()
                || item.path.len() > 4096
                || item.path.contains('\0')
                || path
                    .components()
                    .any(|part| matches!(part, Component::ParentDir))
            {
                return Err(crate::Error::Validation("Invalid deliverable path".into()));
            }
        }
        Ok(())
    }

    /// Existence is a delivery check, not proof that the contents are correct.
    /// Never inspect a declared path outside the workspace.
    pub fn first_missing_output(&self, workspace: &Path) -> Option<String> {
        self.deliverables
            .iter()
            .filter(|item| item.required)
            .find_map(|item| {
                let candidate = resolve_workspace_output(workspace, &item.path)?;
                (!candidate.exists()).then(|| item.path.clone())
            })
    }
}

/// Resolve a declared output without probing paths beyond the workspace.
pub fn resolve_workspace_output(workspace: &Path, output: &str) -> Option<std::path::PathBuf> {
    let path = Path::new(output);
    let relative = if path.is_absolute() {
        path.strip_prefix(workspace).ok()?
    } else {
        path
    };
    if relative
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return None;
    }
    let candidate = workspace.join(relative);
    let root = workspace.canonicalize().ok()?;
    let mut ancestor = candidate.as_path();
    while !ancestor.exists() {
        ancestor = ancestor.parent()?;
    }
    if !ancestor.canonicalize().ok()?.starts_with(root) {
        return None;
    }
    Some(candidate)
}
