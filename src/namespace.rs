//! Nested namespace dictionaries for subcontext configs.
//!
//! Each subcontext (project, user, system) can store a `namespaces.json` file
//! on its config branch containing a nested dictionary mapping names to UUIDs:
//!
//! ```json
//! {
//!   "myproject": "aaaaaaaa-...",
//!   "tools": {
//!     "editor": "bbbbbbbb-...",
//!     "linter": "cccccccc-..."
//!   }
//! }
//! ```
//!
//! Leaf values are UUID strings; intermediate values are nested objects.
//! Names **must not** start with `.` — dot-prefixed names are reserved for
//! interpolation (`.project`, `.user`, `.uuid`).

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::Path;

use crate::backend::Backend;

pub const NAMESPACES_FILE: &str = "namespaces.json";

/// Read the namespace dictionary from a config directory.
/// Returns an empty object if the file doesn't exist.
pub fn read_namespaces(
    backend: &dyn Backend,
    config_dir: &Path,
) -> Result<serde_json::Map<String, Value>> {
    let path = config_dir.join(NAMESPACES_FILE);
    if !backend.exists(&path) {
        return Ok(serde_json::Map::new());
    }
    let content = backend
        .read_to_string(&path)
        .with_context(|| format!("failed to read {NAMESPACES_FILE}"))?;
    let val: Value = serde_json::from_str(&content)
        .with_context(|| format!("invalid JSON in {NAMESPACES_FILE}"))?;
    val.as_object()
        .cloned()
        .context("namespaces.json must be a JSON object")
}

/// Write the namespace dictionary to a config directory.
pub fn write_namespaces(
    backend: &dyn Backend,
    config_dir: &Path,
    ns: &serde_json::Map<String, Value>,
) -> Result<()> {
    let path = config_dir.join(NAMESPACES_FILE);
    let content = serde_json::to_string_pretty(&Value::Object(ns.clone()))? + "\n";
    if let Some(parent) = path.parent() {
        backend.create_dir_all(parent)?;
    }
    backend
        .write(&path, content.as_bytes())
        .with_context(|| format!("failed to write {NAMESPACES_FILE}"))?;
    Ok(())
}

/// Validate a namespace name: must not be empty and must not start with '.'.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("namespace name must not be empty");
    }
    if name.starts_with('.') {
        bail!("namespace name '{name}' must not start with '.' (reserved for interpolation)");
    }
    Ok(())
}

/// Set a value in the namespace dictionary at a given dot-separated key path.
///
/// `key_path` is a slice of name segments, e.g. `["tools", "editor"]`.
/// The final segment maps to `uuid`. Intermediate segments create nested
/// objects as needed.
pub fn set_namespace(
    ns: &mut serde_json::Map<String, Value>,
    key_path: &[&str],
    uuid: &str,
) -> Result<()> {
    if key_path.is_empty() {
        bail!("namespace key path must not be empty");
    }
    for seg in key_path {
        validate_name(seg)?;
    }

    if key_path.len() == 1 {
        ns.insert(key_path[0].to_string(), Value::String(uuid.to_string()));
        return Ok(());
    }

    // Walk/create intermediate objects.
    let key = key_path[0];
    let entry = ns
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    match entry {
        Value::Object(inner) => set_namespace(inner, &key_path[1..], uuid),
        Value::String(_) => {
            bail!("cannot create nested key under '{key}' — it is already mapped to a UUID");
        }
        _ => bail!("unexpected value type at '{key}' in namespace"),
    }
}

/// Remove a key from the namespace dictionary at a given path.
/// Returns the removed value, if any.
pub fn remove_namespace(
    ns: &mut serde_json::Map<String, Value>,
    key_path: &[&str],
) -> Result<Option<Value>> {
    if key_path.is_empty() {
        bail!("namespace key path must not be empty");
    }
    if key_path.len() == 1 {
        return Ok(ns.remove(key_path[0]));
    }

    let key = key_path[0];
    match ns.get_mut(key) {
        Some(Value::Object(inner)) => {
            let result = remove_namespace(inner, &key_path[1..])?;
            // Clean up empty intermediate objects.
            if inner.is_empty() {
                ns.remove(key);
            }
            Ok(result)
        }
        _ => Ok(None),
    }
}

/// Resolve a path through the namespace dictionary.
///
/// Walks the nested dictionary consuming segments. When a leaf UUID string is
/// found, returns `(uuid, remaining_segments)`.
///
/// For example, given namespace `{"tools": {"editor": "abc-123"}}` and path
/// segments `["tools", "editor", "some-task"]`, returns `("abc-123", ["some-task"])`.
pub fn resolve_namespace<'a>(
    ns: &serde_json::Map<String, Value>,
    segments: &'a [&'a str],
) -> Result<(String, &'a [&'a str])> {
    if segments.is_empty() {
        bail!("empty path in namespace resolution");
    }
    let key = segments[0];
    match ns.get(key) {
        Some(Value::String(uuid)) => Ok((uuid.clone(), &segments[1..])),
        Some(Value::Object(inner)) => {
            if segments.len() == 1 {
                bail!("'{key}' is a namespace group, not a UUID — path is incomplete");
            }
            resolve_namespace(inner, &segments[1..])
        }
        Some(_) => bail!("unexpected value type at '{key}' in namespace"),
        None => bail!("'{key}' not found in namespace"),
    }
}

/// List all entries in the namespace (flattened to key-path → uuid).
pub fn flatten_namespaces(
    ns: &serde_json::Map<String, Value>,
    prefix: &str,
) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for (key, val) in ns {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}/{key}")
        };
        match val {
            Value::String(uuid) => result.push((full_key, uuid.clone())),
            Value::Object(inner) => {
                result.extend(flatten_namespaces(inner, &full_key));
            }
            _ => {}
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}
