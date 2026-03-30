use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::Path;

use crate::git::{config_dir, run_git, subcontext_dir, CheckoutContext};
use crate::overlay;

/// Run `subcontext uninstall` from the given repo root.
pub fn uninstall(root: &Path) -> Result<()> {
    let sc_dir = subcontext_dir(root);

    if !sc_dir.exists() {
        eprintln!("[subcontext] No .git/.subcontext/ found — nothing to uninstall.");
        return Ok(());
    }

    // Step 1: Unapply overlay (remove overlay files from root, restore both-repo files)
    let ctx = CheckoutContext::main_only(root);
    if let Err(e) = overlay::unapply_overlay(&ctx) {
        eprintln!("[subcontext] warning: failed to unapply overlay: {e:#}");
    }

    // Step 2: Restore or remove hooks
    restore_hook(root, "post-checkout")?;
    restore_hook(root, "post-commit")?;

    // Step 3: Remove git alias
    remove_git_alias(root);

    // Step 4: Remove subcontext entry from Claude settings
    remove_claude_settings(root)?;

    // Step 5: Clean up all subcontext excludes (including worktree sections)
    overlay::clean_all_excludes(root)?;

    // Step 6: Remove .git/.subcontext/
    // First remove worktrees, then the directory
    let work = sc_dir.join("work");
    let config = sc_dir.join("config");
    if work.exists() {
        fs::remove_dir_all(&work).ok();
    }
    if config.exists() {
        fs::remove_dir_all(&config).ok();
    }
    fs::remove_dir_all(&sc_dir).ok();

    eprintln!("[subcontext] Uninstall complete.");
    Ok(())
}

/// Remove the `git subcontext` alias from local git config.
fn remove_git_alias(root: &Path) {
    match run_git(&["config", "--unset", "alias.subcontext"], root) {
        Ok(_) => eprintln!("[subcontext] Removed git alias."),
        Err(_) => {} // alias may not exist
    }
}

/// Remove the subcontext hook dispatcher and restore the original hook if backed up.
fn restore_hook(root: &Path, hook_name: &str) -> Result<()> {
    let hook_path = root.join(".git").join("hooks").join(hook_name);

    if !hook_path.exists() {
        return Ok(());
    }

    // Only touch the hook if it's ours
    let content = fs::read_to_string(&hook_path).unwrap_or_default();
    if !content.contains(&format!("subcontext _hook {hook_name}"))
        && !content.contains(&format!("git subcontext _hook {hook_name}"))
    {
        eprintln!("[subcontext] {hook_name} hook is not ours — skipping.");
        return Ok(());
    }

    let backup_path = config_dir(root).join("hooks").join("old").join(hook_name);

    if backup_path.exists() {
        fs::copy(&backup_path, &hook_path)
            .with_context(|| format!("failed to restore original {hook_name} hook"))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&hook_path)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&hook_path, perms)?;
        }

        eprintln!("[subcontext] Restored original {hook_name} hook.");
    } else {
        fs::remove_file(&hook_path)
            .with_context(|| format!("failed to remove {hook_name} hook"))?;
        eprintln!("[subcontext] Removed {hook_name} hook.");
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
                                .is_some_and(|c| {
                                    c == "git subcontext startup --claude-code"
                                        || c == "subcontext startup --claude-code"
                                        || c == "subcontext startup"
                                })
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

