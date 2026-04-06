use anyhow::{Context, Result, bail};
use std::path::Path;
use uuid::Uuid;

use crate::backend::Backend;
use crate::git::config_dir;

pub const CONFIG_FILE: &str = "subcontext.yaml";
pub const SUBCONTEXT_KIND: &str = "project";
pub const SYSTEM_KIND: &str = "system";
pub const USER_KIND: &str = "user";
pub const SUBCONTEXT_VERSION: &str = "0.0.0";

/// Write the initial project config on the config branch, if not present.
/// Returns the project UUID (existing or newly generated).
pub fn ensure_project_config(backend: &dyn Backend, root: &Path) -> Result<String> {
    ensure_config_in(backend, &config_dir(root), SUBCONTEXT_KIND)
}

/// Write an initial subcontext.yaml into `config_dir` with the given kind.
/// Returns the UUID (existing or newly generated).
pub fn ensure_config_in(backend: &dyn Backend, config_dir: &Path, kind: &str) -> Result<String> {
    let path = config_dir.join(CONFIG_FILE);
    if backend.exists(&path) {
        return read_project_uuid_at(backend, config_dir);
    }
    let project_uuid = Uuid::new_v4().to_string();
    let content =
        format!("project_uuid: {project_uuid}\nkind: {kind}\nversion: {SUBCONTEXT_VERSION}\n");
    if let Some(parent) = path.parent() {
        backend.create_dir_all(parent)?;
    }
    backend
        .write(&path, content.as_bytes())
        .context("failed to write subcontext.yaml")?;
    Ok(project_uuid)
}

/// Read the project UUID from the config branch's subcontext.yaml.
pub fn read_project_uuid(backend: &dyn Backend, root: &Path) -> Result<String> {
    read_project_uuid_at(backend, &config_dir(root))
}

pub fn read_project_uuid_at(backend: &dyn Backend, config_dir: &Path) -> Result<String> {
    let path = config_dir.join(CONFIG_FILE);
    let content = backend
        .read_to_string(&path)
        .with_context(|| format!("failed to read {CONFIG_FILE}"))?;
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

/// Read the kind field from a subcontext.yaml in the given config directory.
pub fn read_kind_at(backend: &dyn Backend, config_dir: &Path) -> Result<String> {
    let path = config_dir.join(CONFIG_FILE);
    let content = backend
        .read_to_string(&path)
        .with_context(|| format!("failed to read {CONFIG_FILE}"))?;
    for line in content.lines() {
        if let Some(val) = line.strip_prefix("kind:") {
            let v = val.trim();
            if !v.is_empty() {
                return Ok(v.to_string());
            }
        }
    }
    bail!("kind not found in {CONFIG_FILE}")
}

/// Read the kind field from the local subcontext's config.
pub fn read_kind(backend: &dyn Backend, root: &Path) -> Result<String> {
    read_kind_at(backend, &config_dir(root))
}
