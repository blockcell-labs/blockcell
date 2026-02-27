use rust_embed::RustEmbed;
use std::path::Path;

/// Embedded builtin skills — compiled into the binary at build time.
/// The `folder` path is relative to the Cargo.toml of the bin crate.
#[derive(RustEmbed)]
#[folder = "../../skills/"]
struct BuiltinSkillAssets;

/// Extract all builtin skills to `workspace/skills/`.
/// Only writes files that do not already exist (never overwrites user modifications).
/// Returns the list of skill names that were newly extracted.
pub fn extract_to_workspace(skills_dir: &Path) -> anyhow::Result<Vec<String>> {
    std::fs::create_dir_all(skills_dir)?;

    let mut extracted_skills: Vec<String> = Vec::new();

    for file_path in BuiltinSkillAssets::iter() {
        let rel = file_path.as_ref(); // e.g. "camera/SKILL.rhai"

        // Derive skill name from first path component
        let skill_name = rel.split('/').next().unwrap_or("").to_string();
        if skill_name.is_empty() {
            continue;
        }

        let dest = skills_dir.join(rel);

        // Skip if already exists — never overwrite user modifications
        if dest.exists() {
            continue;
        }

        // Create parent dirs
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Write embedded bytes
        if let Some(asset) = BuiltinSkillAssets::get(rel) {
            std::fs::write(&dest, asset.data.as_ref())?;
            if !extracted_skills.contains(&skill_name) {
                extracted_skills.push(skill_name);
            }
        }
    }

    Ok(extracted_skills)
}

