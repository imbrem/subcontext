use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::git::{current_branch, run_git, sanitize_branch_name};

/// Handle `subcontext _hook post-checkout <prev> <new> <flag>`.
/// Subcontext's own errors are swallowed (logged to stderr, exits 0).
/// Old hook failures are propagated so the user sees them.
pub fn post_checkout(root: &Path, prev: &str, new: &str, flag: &str) -> Result<()> {
    if let Err(e) = post_checkout_inner(root, prev, new, flag) {
        eprintln!("[subcontext] warning: post-checkout hook failed: {e:#}");
    }

    // Old hook always runs (even if subcontext logic failed) and its failures propagate.
    run_old_hook(root, &[prev, new, flag])
}

fn post_checkout_inner(root: &Path, _prev: &str, _new: &str, flag: &str) -> Result<()> {
    // File checkout (flag=0) — no action needed
    if flag == "0" {
        return Ok(());
    }

    let subcontext_dir = root.join(".subcontext");
    if !subcontext_dir.exists() {
        return Ok(());
    }

    // Step 1: Determine the new branch name
    let branch = current_branch(root)?;
    let safe_branch = sanitize_branch_name(&branch);
    let target_branch = format!("worktrees/{safe_branch}");

    // Step 2: Check if the worktree branch exists
    let branch_list = run_git(&["branch", "--list", &target_branch], &subcontext_dir)?;

    if branch_list.trim().is_empty() {
        // Create orphan branch
        create_orphan_worktree_branch(&subcontext_dir, &target_branch)?;
    }

    // Step 3: Switch main worktree to the target branch
    run_git(&["checkout", &target_branch], &subcontext_dir)?;

    // Step 4: Copy-back settings
    copy_back_settings(root)?;

    Ok(())
}

/// Create an orphan branch for a new worktree context.
fn create_orphan_worktree_branch(subcontext_dir: &Path, branch: &str) -> Result<()> {
    // Save current branch to restore later
    let current = run_git(&["symbolic-ref", "--short", "HEAD"], subcontext_dir).unwrap_or_default();

    run_git(&["checkout", "--orphan", branch], subcontext_dir)?;
    // Remove any staged files from the index
    run_git(&["rm", "-rf", "--cached", "."], subcontext_dir).ok();
    // Clean working tree of tracked files (but not .mnt/ or .tmp/ which are excluded)
    run_git(
        &["clean", "-fd", "-e", ".mnt", "-e", ".tmp"],
        subcontext_dir,
    )
    .ok();
    run_git(
        &["commit", "--allow-empty", "-m", &format!("init {branch}")],
        subcontext_dir,
    )?;

    // Restore to previous branch so the checkout in the caller works consistently
    if !current.is_empty() {
        run_git(&["checkout", &current], subcontext_dir).ok();
    }

    Ok(())
}

/// Set up the initial worktree context during install.
pub fn setup_initial_worktree_context(root: &Path, branch: &str) -> Result<()> {
    let subcontext_dir = root.join(".subcontext");
    let safe_branch = sanitize_branch_name(branch);
    let target_branch = format!("worktrees/{safe_branch}");

    eprintln!("[subcontext] Creating worktree branch: {target_branch}");

    create_orphan_worktree_branch(&subcontext_dir, &target_branch)?;
    run_git(&["checkout", &target_branch], &subcontext_dir)?;

    Ok(())
}

/// Copy settings from config mount back to .claude/settings.local.json.
fn copy_back_settings(root: &Path) -> Result<()> {
    let src = root
        .join(".subcontext")
        .join(".mnt")
        .join("config")
        .join("agents")
        .join("claude")
        .join("settings.local.json");

    if !src.exists() {
        return Ok(());
    }

    let dest_dir = root.join(".claude");
    fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join("settings.local.json");

    fs::copy(&src, &dest).context("failed to copy settings.local.json from config mount")?;

    Ok(())
}

/// Run the old post-checkout hook if it was backed up.
fn run_old_hook(root: &Path, args: &[&str]) -> Result<()> {
    let old_hook = root
        .join(".subcontext")
        .join(".mnt")
        .join("config")
        .join("hooks")
        .join("old")
        .join("post-checkout");

    if !old_hook.exists() {
        return Ok(());
    }

    // Check if executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::metadata(&old_hook)?.permissions();
        if perms.mode() & 0o111 == 0 {
            return Ok(());
        }
    }

    let status = Command::new(&old_hook)
        .args(args)
        .current_dir(root)
        .status()
        .with_context(|| format!("failed to run old hook: {}", old_hook.display()))?;

    if !status.success() {
        anyhow::bail!("old post-checkout hook exited with {}", status);
    }

    Ok(())
}
