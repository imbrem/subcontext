use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::Path;

/// Run `subcontext uninstall` from the given repo root.
pub fn uninstall(root: &Path) -> Result<()> {
    let subcontext_dir = root.join(".subcontext");

    if !subcontext_dir.exists() {
        eprintln!("[subcontext] No .subcontext/ found — nothing to uninstall.");
        return Ok(());
    }

    // Step 1: Restore or remove the post-checkout hook
    restore_hook(root)?;

    // Step 2: Remove subcontext entry from Claude settings
    remove_claude_settings(root)?;

    eprintln!("[subcontext] Uninstall complete. .subcontext/ directory was left in place.");
    Ok(())
}

/// Remove the subcontext hook dispatcher and restore the original hook if backed up.
fn restore_hook(root: &Path) -> Result<()> {
    let hook_path = root.join(".git").join("hooks").join("post-checkout");

    if !hook_path.exists() {
        return Ok(());
    }

    // Only touch the hook if it's ours
    let content = fs::read_to_string(&hook_path).unwrap_or_default();
    if !content.contains("subcontext _hook post-checkout") {
        eprintln!("[subcontext] post-checkout hook is not ours — skipping.");
        return Ok(());
    }

    let backup_path = root
        .join(".subcontext")
        .join(".mnt")
        .join("config")
        .join("hooks")
        .join("old")
        .join("post-checkout");

    if backup_path.exists() {
        fs::copy(&backup_path, &hook_path).context("failed to restore original post-checkout hook")?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&hook_path)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&hook_path, perms)?;
        }

        eprintln!("[subcontext] Restored original post-checkout hook.");
    } else {
        fs::remove_file(&hook_path).context("failed to remove post-checkout hook")?;
        eprintln!("[subcontext] Removed post-checkout hook.");
    }

    Ok(())
}

/// Remove the subcontext SessionStart hook from .claude/settings.local.json.
fn remove_claude_settings(root: &Path) -> Result<()> {
    let settings_path = root.join(".claude").join("settings.local.json");

    if !settings_path.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&settings_path)
        .context("failed to read .claude/settings.local.json")?;
    let mut settings: Value =
        serde_json::from_str(&content).context("failed to parse .claude/settings.local.json")?;

    let removed = if let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        if let Some(session_start) = hooks.get_mut("SessionStart").and_then(|s| s.as_array_mut()) {
            let before = session_start.len();
            session_start.retain(|entry| {
                !entry
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .is_some_and(|hooks| {
                        hooks.iter().any(|h| {
                            h.get("command")
                                .and_then(|c| c.as_str())
                                .is_some_and(|c| c == "subcontext startup")
                        })
                    })
            });
            session_start.len() < before
        } else {
            false
        }
    } else {
        false
    };

    if removed {
        let formatted = serde_json::to_string_pretty(&settings)?;
        fs::write(&settings_path, format!("{formatted}\n"))
            .context("failed to write .claude/settings.local.json")?;
        eprintln!("[subcontext] Removed SessionStart hook from Claude settings.");
    }

    Ok(())
}
