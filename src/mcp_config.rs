use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::PathBuf;

use crate::backend::Backend;

const SERVER_NAME: &str = "subcontext";

/// Path to Claude Code's user-scoped config file: `~/.claude.json`.
fn user_config_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME environment variable is not set")?;
    Ok(PathBuf::from(home).join(".claude.json"))
}

/// Build the stdio MCP server entry pointing at the currently-running
/// subcontext binary. We resolve an absolute path because the `git
/// subcontext` alias is only set locally in each installed repo — a
/// *globally*-registered MCP server needs a path that works from anywhere.
fn server_entry(backend: &dyn Backend) -> Result<Value> {
    let exe = backend.current_exe()?;
    let exe_str = exe.to_string_lossy().to_string();
    Ok(json!({
        "type": "stdio",
        "command": exe_str,
        "args": ["mcp"]
    }))
}

/// Install the subcontext MCP server globally (user scope), **inactive by
/// default**.
///
/// Claude Code has no documented mechanism for "install a user-scoped MCP
/// server but keep it inactive". The real `mcpServers` field in
/// `~/.claude.json` always activates its entries on session start. To keep
/// the entry dormant, we write it to a `_disabled_mcpServers` key (an
/// unknown top-level field that Claude Code ignores) as a parking spot.
///
/// The effect: the server is present in the config as a ready-to-use
/// template, but never started by Claude Code. To activate it, the user
/// moves the entry from `_disabled_mcpServers` to `mcpServers` by hand.
///
/// Idempotent: if an entry under either key already exists, it is left alone.
pub fn install_global(backend: &dyn Backend) -> Result<()> {
    let path = user_config_path()?;

    let mut config: Value = if backend.exists(&path) {
        let content = backend
            .read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if content.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&content)
                .with_context(|| format!("failed to parse {}", path.display()))?
        }
    } else {
        json!({})
    };

    let obj = config
        .as_object_mut()
        .with_context(|| format!("{} root is not a JSON object", path.display()))?;

    // If an active entry already exists, don't touch it.
    if obj
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .is_some_and(|s| s.contains_key(SERVER_NAME))
    {
        eprintln!(
            "[subcontext] `{SERVER_NAME}` is already active in {}. Leaving it as-is.",
            path.display()
        );
        return Ok(());
    }

    let disabled = obj
        .entry("_disabled_mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("_disabled_mcpServers field is not an object")?;

    if disabled.contains_key(SERVER_NAME) {
        eprintln!(
            "[subcontext] `{SERVER_NAME}` MCP server already installed (inactive) in {}.",
            path.display()
        );
        return Ok(());
    }

    disabled.insert(SERVER_NAME.to_string(), server_entry(backend)?);

    let formatted = serde_json::to_string_pretty(&config)?;
    backend
        .write(&path, format!("{formatted}\n").as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;

    eprintln!(
        "[subcontext] Installed `{SERVER_NAME}` MCP server (inactive) in {}.",
        path.display()
    );
    eprintln!(
        "[subcontext] To activate it, edit {} and move the \"{SERVER_NAME}\" entry\n\
         [subcontext] from \"_disabled_mcpServers\" into \"mcpServers\".",
        path.display()
    );
    Ok(())
}
