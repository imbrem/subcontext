use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

const SERVER_NAME: &str = "subcontext";

/// Path to Claude Code's user-scoped config file: `~/.claude.json`.
fn user_config_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME environment variable is not set")?;
    Ok(PathBuf::from(home).join(".claude.json"))
}

/// The stdio MCP server entry. Uses `git subcontext mcp` so the user gets the
/// same binary that the `git subcontext` alias resolves to in whichever repo
/// they are working in.
fn server_entry() -> Value {
    json!({
        "type": "stdio",
        "command": "git",
        "args": ["subcontext", "mcp"]
    })
}

/// Install the subcontext MCP server globally (user scope), **inactive by
/// default**. The entry is written to `~/.claude.json` under
/// `_disabled_mcpServers` so the user must explicitly move it to
/// `mcpServers` (via `/mcp` in Claude Code, or by editing the file) to
/// activate it.
///
/// Idempotent: if an entry under either key already exists, it is left alone.
pub fn install_global() -> Result<()> {
    let path = user_config_path()?;

    let mut config: Value = if path.exists() {
        let content = fs::read_to_string(&path)
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

    let obj = config.as_object_mut().with_context(|| {
        format!("{} root is not a JSON object", path.display())
    })?;

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

    disabled.insert(SERVER_NAME.to_string(), server_entry());

    let formatted = serde_json::to_string_pretty(&config)?;
    fs::write(&path, format!("{formatted}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;

    eprintln!(
        "[subcontext] Installed `{SERVER_NAME}` MCP server (inactive) in {}.",
        path.display()
    );
    eprintln!(
        "[subcontext] To activate it, run /mcp in Claude Code or move the entry \
         from `_disabled_mcpServers` to `mcpServers`."
    );
    Ok(())
}
