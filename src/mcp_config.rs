use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

const SERVER_NAME: &str = "subcontext";

/// The MCP server entry that subcontext injects into `.mcp.json`.
fn server_entry() -> Value {
    json!({
        "command": "git",
        "args": ["subcontext", "mcp"]
    })
}

/// Merge the subcontext MCP server entry into `.mcp.json` at the project root.
/// Creates the file if it doesn't exist. Idempotent.
pub fn merge_mcp_config(root: &Path) -> Result<()> {
    let path = root.join(".mcp.json");

    let mut config: Value = if path.exists() {
        let content = fs::read_to_string(&path).context("failed to read .mcp.json")?;
        if content.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&content).context("failed to parse .mcp.json")?
        }
    } else {
        json!({})
    };

    let obj = config
        .as_object_mut()
        .context(".mcp.json root is not an object")?;

    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("mcpServers field is not an object")?;

    // Only insert if missing; don't clobber user edits.
    if !servers.contains_key(SERVER_NAME) {
        servers.insert(SERVER_NAME.to_string(), server_entry());
    }

    let formatted = serde_json::to_string_pretty(&config)?;
    fs::write(&path, format!("{formatted}\n")).context("failed to write .mcp.json")?;

    eprintln!("[subcontext] Configured MCP server in .mcp.json.");
    Ok(())
}

/// Remove the subcontext MCP server entry from `.mcp.json`.
/// Removes the file entirely if it becomes empty.
pub fn remove_mcp_config(root: &Path) -> Result<()> {
    let path = root.join(".mcp.json");
    if !path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&path).context("failed to read .mcp.json")?;
    if content.trim().is_empty() {
        return Ok(());
    }

    let mut config: Value =
        serde_json::from_str(&content).context("failed to parse .mcp.json")?;

    let mut removed = false;
    if let Some(obj) = config.as_object_mut()
        && let Some(servers) = obj.get_mut("mcpServers").and_then(|v| v.as_object_mut())
    {
        removed = servers.remove(SERVER_NAME).is_some();

        // If no servers remain, drop the key.
        if servers.is_empty() {
            obj.remove("mcpServers");
        }
    }

    if !removed {
        return Ok(());
    }

    // If the file is now an empty object, remove it entirely so we don't
    // leave behind an unused file in the host repo.
    let is_empty = config
        .as_object()
        .is_some_and(|o| o.is_empty());

    if is_empty {
        fs::remove_file(&path).context("failed to remove empty .mcp.json")?;
        eprintln!("[subcontext] Removed .mcp.json (no servers remained).");
    } else {
        let formatted = serde_json::to_string_pretty(&config)?;
        fs::write(&path, format!("{formatted}\n")).context("failed to write .mcp.json")?;
        eprintln!("[subcontext] Removed subcontext MCP server from .mcp.json.");
    }

    Ok(())
}
