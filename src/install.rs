use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use crate::git::{current_branch, run_git};
use crate::hook;
use crate::settings::merge_claude_settings;

/// Run `subcontext install` from the given repo root.
pub fn install(root: &Path, repair: bool) -> Result<()> {
    let subcontext_dir = root.join(".subcontext");
    let branch = current_branch(root)?;

    if subcontext_dir.exists() {
        // Re-install: skip repo init, just redo hooks + settings
        eprintln!("[subcontext] .subcontext/ exists — re-installing hooks and settings...");
    } else {
        // Fresh install: init context repo + config worktree
        eprintln!("[subcontext] Initializing context repo...");
        init_context_repo(root, &branch)?;

        let mnt_config = subcontext_dir.join(".mnt").join("config");
        eprintln!("[subcontext] Setting up config worktree...");
        run_git(
            &["worktree", "add", &mnt_config.to_string_lossy(), "config"],
            &subcontext_dir,
        )?;
    }

    // Step 5: Add excludes
    add_excludes(root)?;

    // Step 6: Back up existing hooks
    let hook_is_ours = hook_dispatches_to_subcontext(root);
    backup_existing_hooks(root, repair, hook_is_ours)?;

    // Step 7: Install hook dispatcher (skip if already ours and not repairing)
    if hook_is_ours && !repair {
        eprintln!(
            "[subcontext] post-checkout hook already dispatches to subcontext — \
             leaving it in place (use --repair to overwrite)"
        );
    } else {
        install_hook_dispatcher(root)?;
    }

    // Step 8: Migrate Claude settings
    merge_claude_settings(root)?;

    // Step 10: Commit config branch
    commit_config_branch(root)?;

    // Step 11: Print summary
    print_summary(root, &branch);

    Ok(())
}

/// Shared steps 5+ used by both install and clone.
pub fn install_from_step5(root: &Path) -> Result<()> {
    // Step 5: Add excludes
    add_excludes(root)?;

    // Step 6: Back up existing hooks
    backup_existing_hooks(root, false, false)?;

    // Step 7: Install hook dispatcher
    install_hook_dispatcher(root)?;

    // Step 8: Migrate Claude settings
    merge_claude_settings(root)?;

    // Step 9: Create initial worktree context
    let branch = current_branch(root)?;
    hook::setup_initial_worktree_context(root, &branch)?;

    // Step 10: Commit config branch
    commit_config_branch(root)?;

    // Step 11: Print summary
    print_summary(root, &branch);

    Ok(())
}

/// Step 2-3: Initialize the context repo with config branch, then switch
/// main worktree to worktrees/<branch> so config is free for a worktree mount.
fn init_context_repo(root: &Path, host_branch: &str) -> Result<()> {
    use crate::git::sanitize_branch_name;

    let subcontext_dir = root.join(".subcontext");

    run_git(&["init", &subcontext_dir.to_string_lossy()], root)?;

    // Create orphan config branch with initial commit
    run_git(&["checkout", "--orphan", "config"], &subcontext_dir)?;

    let gitkeep = subcontext_dir.join(".gitkeep");
    fs::write(&gitkeep, "").context("failed to write .gitkeep")?;

    run_git(&["add", ".gitkeep"], &subcontext_dir)?;
    run_git(&["commit", "-m", "init config branch"], &subcontext_dir)?;

    fs::remove_file(&gitkeep).ok();

    // Now create the initial worktree branch and switch the main worktree to it.
    // This frees the config branch so it can be added as a separate worktree.
    let safe_branch = sanitize_branch_name(host_branch);
    let target_branch = format!("worktrees/{safe_branch}");

    eprintln!("[subcontext] Creating worktree branch: {target_branch}");
    run_git(&["checkout", "--orphan", &target_branch], &subcontext_dir)?;
    // Clear index from config branch leftovers
    run_git(&["rm", "-rf", "--cached", "."], &subcontext_dir).ok();
    run_git(&["clean", "-fd"], &subcontext_dir).ok();
    run_git(
        &[
            "commit",
            "--allow-empty",
            "-m",
            &format!("init {target_branch}"),
        ],
        &subcontext_dir,
    )?;

    Ok(())
}

/// Step 5: Add exclusion entries.
fn add_excludes(root: &Path) -> Result<()> {
    // Add .subcontext/ to host repo's .git/info/exclude
    let host_exclude = root.join(".git").join("info").join("exclude");
    add_to_exclude(&host_exclude, ".subcontext/")?;

    // Add .mnt/ and .tmp/ to context repo's .git/info/exclude
    let ctx_exclude = root
        .join(".subcontext")
        .join(".git")
        .join("info")
        .join("exclude");
    add_to_exclude(&ctx_exclude, ".mnt/")?;
    add_to_exclude(&ctx_exclude, ".tmp/")?;

    Ok(())
}

/// Append an entry to a git exclude file, idempotently.
fn add_to_exclude(exclude_path: &Path, entry: &str) -> Result<()> {
    // Ensure parent directory exists
    if let Some(parent) = exclude_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let existing = fs::read_to_string(exclude_path).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == entry) {
        return Ok(());
    }

    let mut content = existing;
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(entry);
    content.push('\n');

    fs::write(exclude_path, content).with_context(|| {
        format!(
            "failed to write exclude entry to {}",
            exclude_path.display()
        )
    })?;
    Ok(())
}

/// Check whether a shell script line (outside of comments) invokes subcontext.
/// Returns true if any non-comment line contains `subcontext` as a word boundary
/// (preceded by whitespace, start-of-line, or path separator).
fn line_invokes_subcontext(line: &str) -> bool {
    let trimmed = line.trim();
    // Skip comments
    if trimmed.starts_with('#') {
        return false;
    }
    // Look for `subcontext` preceded by a word boundary: start of token after
    // whitespace, or after a path separator (/), or at line start.
    // This avoids matching e.g. "notsubcontext" while still matching
    // "exec subcontext", "/usr/bin/subcontext", etc.
    let mut rest = trimmed;
    while let Some(pos) = rest.find("subcontext") {
        if pos == 0 {
            return true;
        }
        let prev = rest.as_bytes()[pos - 1];
        if prev == b' ' || prev == b'\t' || prev == b'/' || prev == b'=' || prev == b'"' || prev == b'\'' {
            return true;
        }
        rest = &rest[pos + "subcontext".len()..];
    }
    false
}

/// Check whether the post-checkout hook dispatches to subcontext
/// (i.e. a non-comment line invokes `subcontext`).
fn hook_dispatches_to_subcontext(root: &Path) -> bool {
    let hook_path = root.join(".git").join("hooks").join("post-checkout");
    let content = match fs::read_to_string(&hook_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    content.lines().any(line_invokes_subcontext)
}

/// Step 6: Back up existing hooks.
///
/// If a hook dispatches to subcontext (detected via `is_subcontext_hook`) it is
/// skipped — unless `repair` is true, in which case it is copied to
/// `hooks/backup/` (which is *not* executed) so the user can inspect it.
fn backup_existing_hooks(root: &Path, repair: bool, is_subcontext_hook: bool) -> Result<()> {
    let hooks_dir = root.join(".git").join("hooks");
    if !hooks_dir.exists() {
        return Ok(());
    }

    let old_dir = root
        .join(".subcontext")
        .join(".mnt")
        .join("config")
        .join("hooks")
        .join("old");

    let backup_dir = root
        .join(".subcontext")
        .join(".mnt")
        .join("config")
        .join("hooks")
        .join("backup");

    for entry in fs::read_dir(&hooks_dir)? {
        let entry = entry?;
        let path = entry.path();

        // Skip non-files and sample hooks
        if !path.is_file() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.ends_with(".sample")
        {
            continue;
        }

        // Check if executable (Unix)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::metadata(&path)?.permissions();
            if perms.mode() & 0o111 == 0 {
                continue;
            }
        }

        let hook_name = path.file_name().unwrap().to_string_lossy().to_string();

        // If this hook dispatches to subcontext, don't put it in old/ (it would loop).
        if is_subcontext_hook && hook_name == "post-checkout" {
            if repair {
                fs::create_dir_all(&backup_dir)?;
                let dest = backup_dir.join(&hook_name);
                fs::copy(&path, &dest)?;
                eprintln!(
                    "[subcontext] Saved existing subcontext hook to hooks/backup/{hook_name}"
                );
            }
            continue;
        }

        fs::create_dir_all(&old_dir)?;
        let dest = old_dir.join(&hook_name);
        fs::copy(&path, &dest)?;
        eprintln!("[subcontext] Backed up hook: {hook_name}");
    }

    Ok(())
}

/// Step 7: Install the hook dispatcher script.
fn install_hook_dispatcher(root: &Path) -> Result<()> {
    let hooks_dir = root.join(".git").join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join("post-checkout");
    let script = r#"#!/bin/sh
# This hook was replaced by subcontext.
# It dispatches to `subcontext _hook post-checkout`, which handles context-branch
# switching and then automatically runs your original hook (if one existed).
#
# Your original hook was backed up to:
#   .subcontext/.mnt/config/hooks/old/post-checkout
# You can edit it there — subcontext will continue to call it after its own logic.
exec subcontext _hook post-checkout "$@"
"#;

    fs::write(&hook_path, script).context("failed to write post-checkout hook")?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&hook_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook_path, perms)?;
    }

    eprintln!("[subcontext] Installed post-checkout hook dispatcher.");
    Ok(())
}

/// Step 10: Commit everything on the config branch.
fn commit_config_branch(root: &Path) -> Result<()> {
    let config_dir = root.join(".subcontext").join(".mnt").join("config");

    // Stage everything
    run_git(&["add", "-A"], &config_dir)?;

    // Check if there's anything to commit
    let status = run_git(&["status", "--porcelain"], &config_dir)?;
    if status.is_empty() {
        return Ok(());
    }

    run_git(
        &["commit", "-m", "subcontext install: initial config"],
        &config_dir,
    )?;

    Ok(())
}

fn print_summary(root: &Path, branch: &str) {
    eprintln!();
    eprintln!("[subcontext] Installation complete!");
    eprintln!("  Context repo:  {}/.subcontext/", root.display());
    eprintln!("  Config mount:  .subcontext/.mnt/config/");
    eprintln!("  Active branch: worktrees/{branch}");
    eprintln!("  Hook:          .git/hooks/post-checkout");
    eprintln!();
    eprintln!("  Create .subcontext/TASK.md to inject task context into Claude sessions.");
}
