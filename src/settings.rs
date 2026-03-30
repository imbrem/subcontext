use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

use crate::git::config_dir;

/// The SessionStart hook entry that subcontext injects.
fn subcontext_hook_entry() -> Value {
    json!({
        "hooks": [
            {
                "type": "command",
                "command": "git subcontext startup --claude-code"
            }
        ]
    })
}

/// Merge the subcontext SessionStart hook into the Claude settings.
/// Reads existing settings.local.json, adds the hook, writes back.
/// Also copies the result to the config branch mount.
pub fn merge_claude_settings(root: &Path) -> Result<()> {
    let claude_dir = root.join(".claude");
    fs::create_dir_all(&claude_dir)?;

    let settings_path = claude_dir.join("settings.local.json");

    // Read existing settings or start fresh
    let mut settings: Value = if settings_path.exists() {
        let content = fs::read_to_string(&settings_path)
            .context("failed to read .claude/settings.local.json")?;
        serde_json::from_str(&content).context("failed to parse .claude/settings.local.json")?
    } else {
        json!({})
    };

    // Ensure settings is an object
    let obj = settings
        .as_object_mut()
        .context("settings.local.json root is not an object")?;

    // Get or create the hooks object
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .context("hooks field is not an object")?;

    // Get or create the SessionStart array
    let session_start = hooks_obj.entry("SessionStart").or_insert_with(|| json!([]));
    let session_start_arr = session_start
        .as_array_mut()
        .context("SessionStart field is not an array")?;

    // Check if our hook is already present (match both old and new command strings)
    let already_present = session_start_arr.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .is_some_and(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command").and_then(|c| c.as_str()).is_some_and(|c| {
                        c == "git subcontext startup --claude-code"
                            || c == "subcontext startup --claude-code"
                            || c == "subcontext startup"
                    })
                })
            })
    });

    if !already_present {
        session_start_arr.push(subcontext_hook_entry());
    }

    // Write back to .claude/settings.local.json
    let formatted = serde_json::to_string_pretty(&settings)?;
    fs::write(&settings_path, format!("{formatted}\n"))
        .context("failed to write .claude/settings.local.json")?;

    // Copy to config mount
    let cfg = config_dir(root);
    if cfg.exists() {
        let config_settings_dir = cfg.join("agents").join("claude");
        fs::create_dir_all(&config_settings_dir)?;

        let config_settings_path = config_settings_dir.join("settings.local.json");
        fs::copy(&settings_path, &config_settings_path)
            .context("failed to copy settings to config mount")?;
    }

    eprintln!("[subcontext] Configured Claude SessionStart hook.");
    Ok(())
}
