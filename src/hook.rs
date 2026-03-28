use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::git::{
    config_dir, current_branch, run_git, sanitize_branch_name, subcontext_dir, CheckoutContext,
};
use crate::overlay;

/// Handle `subcontext _hook post-checkout <prev> <new> <flag>`.
/// Subcontext's own errors are swallowed (logged to stderr, exits 0).
/// Old hook failures are propagated so the user sees them.
pub fn post_checkout(ctx: &CheckoutContext, prev: &str, new: &str, flag: &str) -> Result<()> {
    if let Err(e) = post_checkout_inner(ctx, prev, new, flag) {
        eprintln!("[subcontext] warning: post-checkout hook failed: {e:#}");
    }

    // Old hook always runs and its failures propagate.
    run_old_hook(&ctx.main_root, "post-checkout", &[prev, new, flag])
}

fn post_checkout_inner(ctx: &CheckoutContext, prev: &str, new: &str, flag: &str) -> Result<()> {
    // File checkout (flag=0) — no action needed
    if flag == "0" {
        return Ok(());
    }

    let sc_dir = subcontext_dir(&ctx.main_root);
    if !sc_dir.exists() {
        return Ok(());
    }

    let work = ctx.overlay_work_dir();
    let work_exists = work.exists();

    // 1. If we have an existing overlay work dir, save and capture the current branch
    let prev_overlay_branch = if work_exists {
        let branch = overlay::current_work_branch(ctx)?;
        overlay::save_overlay(ctx, "auto-save (pre-checkout)")?;
        overlay::unapply_overlay(ctx)?;
        branch
    } else {
        None
    };

    // 2. Sync back overlay-only files that survived the host checkout
    if work_exists {
        overlay::sync_back_surviving_files(ctx)?;
    }

    // 3. Determine new overlay branch
    let branch = current_branch(&ctx.checkout_root)?;
    let safe_branch = sanitize_branch_name(&branch);
    let overlay_branch = format!("overlay/{safe_branch}");

    // 4. Create overlay branch if needed — determine fork source
    if !overlay::overlay_branch_exists(&ctx.main_root, &overlay_branch)? {
        let source = determine_fork_source(ctx, prev, new, &prev_overlay_branch)?;
        match source {
            Some(src) => {
                overlay::create_overlay_branch_from(&ctx.main_root, &overlay_branch, &src)?;
            }
            None => {
                overlay::create_overlay_branch(&ctx.main_root, &overlay_branch)?;
            }
        }
    }

    // 5. Create or switch the work directory
    if work_exists {
        overlay::switch_work_branch(ctx, &overlay_branch)?;
    } else {
        overlay::create_overlay_work_dir(ctx, &overlay_branch)?;
    }

    // 6. Apply new overlay (also syncs excludes)
    overlay::apply_overlay(ctx)?;

    Ok(())
}

/// Determine whether a new overlay branch should fork from an existing one.
/// Returns Some(source_branch) to fork, or None to create empty.
fn determine_fork_source(
    ctx: &CheckoutContext,
    prev: &str,
    new: &str,
    prev_overlay: &Option<String>,
) -> Result<Option<String>> {
    const NULL_SHA: &str = "0000000000000000000000000000000000000000";

    // Orphan: unborn HEAD (git checkout --orphan) → always empty
    if run_git(&["rev-parse", "--verify", "HEAD"], &ctx.checkout_root).is_err() {
        return Ok(None);
    }

    // If we have a previous overlay branch from this checkout, check ancestry
    if let Some(src) = prev_overlay {
        if overlay::overlay_branch_exists(&ctx.main_root, src)? {
            // Same commit (checkout -b) → fork
            if prev == new {
                return Ok(Some(src.clone()));
            }
            // Null prev (should not happen if prev_overlay is set, but handle it)
            if prev == NULL_SHA || new == NULL_SHA {
                return Ok(Some(src.clone()));
            }
            // Check if branches share a common ancestor
            if run_git(&["merge-base", prev, new], &ctx.checkout_root).is_ok() {
                return Ok(Some(src.clone()));
            }
            // Unrelated branches (e.g., checkout to an orphan branch) → empty
            return Ok(None);
        }
    }

    // No previous overlay (e.g., new worktree). Try the main checkout's overlay.
    if ctx.is_worktree() {
        if let Some(branch) = overlay::main_checkout_overlay_branch(&ctx.main_root)? {
            return Ok(Some(branch));
        }
    }

    Ok(None)
}

/// Handle `subcontext _hook post-commit`.
pub fn post_commit(ctx: &CheckoutContext) -> Result<()> {
    if let Err(e) = post_commit_inner(ctx) {
        eprintln!("[subcontext] warning: post-commit hook failed: {e:#}");
    }

    run_old_hook(&ctx.main_root, "post-commit", &[])
}

fn post_commit_inner(ctx: &CheckoutContext) -> Result<()> {
    let sc_dir = subcontext_dir(&ctx.main_root);
    if !sc_dir.exists() {
        return Ok(());
    }

    overlay::save_overlay(ctx, "auto-save (post-commit)")
}

/// Run the old hook if it was backed up.
fn run_old_hook(main_root: &Path, hook_name: &str, args: &[&str]) -> Result<()> {
    let old_hook = config_dir(main_root)
        .join("hooks")
        .join("old")
        .join(hook_name);

    if !old_hook.exists() {
        return Ok(());
    }

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
        .current_dir(main_root)
        .status()
        .with_context(|| format!("failed to run old hook: {}", old_hook.display()))?;

    if !status.success() {
        anyhow::bail!("old {hook_name} hook exited with {}", status);
    }

    Ok(())
}
