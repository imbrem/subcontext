use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;

use crate::git::{current_branch, run_git};
use crate::hook;
use crate::settings::merge_claude_settings;

/// Run `subcontext install` from the given repo root.
pub fn install(root: &Path) -> Result<()> {
    let subcontext_dir = root.join(".subcontext");

    // Step 1: Verify .subcontext/ doesn't already exist
    if subcontext_dir.exists() {
        bail!(
            ".subcontext/ already exists. If you want to re-initialize, remove it first, \
             or use `subcontext clone` to attach an existing context repo."
        );
    }

    // Step 2-3: Init context repo (creates config branch)
    eprintln!("[subcontext] Initializing context repo...");
    let branch = current_branch(root)?;
    init_context_repo(root, &branch)?;

    // Step 4: Check out config branch as a worktree
    // (main worktree was switched to worktrees/<branch> in init, so config is free)
    let mnt_config = subcontext_dir.join(".mnt").join("config");
    eprintln!("[subcontext] Setting up config worktree...");
    run_git(
        &["worktree", "add", &mnt_config.to_string_lossy(), "config"],
        &subcontext_dir,
    )?;

    // Step 5: Add excludes
    add_excludes(root)?;

    // Step 6: Back up existing hooks
    backup_existing_hooks(root)?;

    // Step 7: Install hook dispatcher
    install_hook_dispatcher(root)?;

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
    backup_existing_hooks(root)?;

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

/// Step 6: Back up existing hooks.
fn backup_existing_hooks(root: &Path) -> Result<()> {
    let hooks_dir = root.join(".git").join("hooks");
    if !hooks_dir.exists() {
        return Ok(());
    }

    let backup_dir = root
        .join(".subcontext")
        .join(".mnt")
        .join("config")
        .join("hooks")
        .join("old");

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

        fs::create_dir_all(&backup_dir)?;
        let dest = backup_dir.join(path.file_name().unwrap());
        fs::copy(&path, &dest)?;
        eprintln!(
            "[subcontext] Backed up hook: {}",
            path.file_name().unwrap().to_string_lossy()
        );
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
