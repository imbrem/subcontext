use anyhow::{Context, Result};
use serde_json::Value;
use std::path::Path;

use crate::backend::Backend;
use crate::git::{CheckoutContext, config_dir, run_git, subcontext_dir};
use crate::overlay;

/// Run `subcontext uninstall` from the given repo root.
pub fn uninstall(backend: &dyn Backend, root: &Path) -> Result<()> {
    let sc_dir = subcontext_dir(root);

    if !backend.exists(&sc_dir) {
        eprintln!("[subcontext] No .git/.subcontext/ found — nothing to uninstall.");
        return Ok(());
    }

    // Step 1: Unapply overlay (remove overlay files from root, restore both-repo files)
    let ctx = CheckoutContext::main_only(root);
    if let Err(e) = overlay::unapply_overlay(backend, &ctx) {
        eprintln!("[subcontext] warning: failed to unapply overlay: {e:#}");
    }

    // Step 2: Restore or remove hooks
    restore_hook(backend, root, "post-checkout")?;
    restore_hook(backend, root, "post-commit")?;

    // Step 3: Remove git alias
    remove_git_alias(backend, root);

    // Step 4: Remove subcontext entry from Claude settings
    remove_claude_settings(backend, root)?;

    // Step 5: Clean up all subcontext excludes (including worktree sections)
    overlay::clean_all_excludes(backend, root)?;

    // Step 6: Remove .git/.subcontext/
    // First remove worktrees, then the directory
    let work = sc_dir.join("work");
    let config = sc_dir.join("config");
    let state = sc_dir.join("state");
    let pool = sc_dir.join("pool");
    if backend.exists(&work) {
        backend.remove_dir_all(&work).ok();
    }
    if backend.exists(&config) {
        backend.remove_dir_all(&config).ok();
    }
    if backend.exists(&state) {
        backend.remove_dir_all(&state).ok();
    }
    if backend.exists(&pool) {
        backend.remove_dir_all(&pool).ok();
    }
    backend.remove_dir_all(&sc_dir).ok();

    eprintln!("[subcontext] Uninstall complete.");
    Ok(())
}

/// Remove the `git subcontext` alias from local git config.
fn remove_git_alias(backend: &dyn Backend, root: &Path) {
    // alias may not exist, so ignore errors
    if run_git(backend, &["config", "--unset", "alias.subcontext"], root).is_ok() {
        eprintln!("[subcontext] Removed git alias.");
    }
}

/// Remove the subcontext hook dispatcher and restore the original hook if backed up.
fn restore_hook(backend: &dyn Backend, root: &Path, hook_name: &str) -> Result<()> {
    let hook_path = root.join(".git").join("hooks").join(hook_name);

    if !backend.exists(&hook_path) {
        return Ok(());
    }

    // Only touch the hook if it's ours
    let content = backend.read_to_string(&hook_path).unwrap_or_default();
    if !content.contains(&format!("subcontext _hook {hook_name}"))
        && !content.contains(&format!("git subcontext _hook {hook_name}"))
    {
        eprintln!("[subcontext] {hook_name} hook is not ours — skipping.");
        return Ok(());
    }

    let backup_path = config_dir(root).join("hooks").join("old").join(hook_name);

    if backend.exists(&backup_path) {
        backend
            .copy(&backup_path, &hook_path)
            .with_context(|| format!("failed to restore original {hook_name} hook"))?;

        #[cfg(unix)]
        {
            backend.set_permissions_mode(&hook_path, 0o755)?;
        }

        eprintln!("[subcontext] Restored original {hook_name} hook.");
    } else {
        backend
            .remove_file(&hook_path)
            .with_context(|| format!("failed to remove {hook_name} hook"))?;
        eprintln!("[subcontext] Removed {hook_name} hook.");
    }

    Ok(())
}

/// Remove the subcontext SessionStart hook from .claude/settings.local.json.
fn remove_claude_settings(backend: &dyn Backend, root: &Path) -> Result<()> {
    let settings_path = root.join(".claude").join("settings.local.json");

    if !backend.exists(&settings_path) {
        return Ok(());
    }

    let content = backend
        .read_to_string(&settings_path)
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
                            h.get("command").and_then(|c| c.as_str()).is_some_and(|c| {
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
        backend
            .write(&settings_path, format!("{formatted}\n").as_bytes())
            .context("failed to write .claude/settings.local.json")?;
        eprintln!("[subcontext] Removed SessionStart hook from Claude settings.");
    }

    Ok(())
}
