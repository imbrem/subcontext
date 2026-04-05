use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use uuid::Uuid;

use crate::git::config_dir;

const CONFIG_FILE: &str = "subcontext.yaml";
pub const SUBCONTEXT_KIND: &str = "project";
pub const SUBCONTEXT_VERSION: &str = "0.0.0";

/// Write the initial project config on the config branch, if not present.
/// Returns the project UUID (existing or newly generated).
pub fn ensure_project_config(root: &Path) -> Result<String> {
    let path = config_dir(root).join(CONFIG_FILE);
    if path.exists() {
        return read_project_uuid(root);
    }
    let project_uuid = Uuid::new_v4().to_string();
    let content = format!(
        "project_uuid: {project_uuid}\nkind: {SUBCONTEXT_KIND}\nversion: {SUBCONTEXT_VERSION}\n"
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, content).context("failed to write subcontext.yaml")?;
    Ok(project_uuid)
}

/// Read the project UUID from the config branch's subcontext.yaml.
pub fn read_project_uuid(root: &Path) -> Result<String> {
    let path = config_dir(root).join(CONFIG_FILE);
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {CONFIG_FILE}"))?;
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("project_uuid:") {
            let v = val.trim();
            if !v.is_empty() {
                return Ok(v.to_string());
            }
        }
    }
    bail!("project_uuid not found in {CONFIG_FILE}")
}
