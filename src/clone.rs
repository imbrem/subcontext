use anyhow::{Context, Result, bail};
use std::path::Path;

use crate::git::{
    current_branch, repo_dir, run_git, run_subcontext_git, sanitize_branch_name, subcontext_dir,
    work_dir, CheckoutContext,
};
use crate::install::install_from_hooks;
use crate::overlay;

/// Run `subcontext clone <url>` from the given repo root.
pub fn clone(root: &Path, url: &str) -> Result<()> {
    let sc_dir = subcontext_dir(root);

    if sc_dir.exists() {
        bail!(
            ".git/.subcontext/ already exists. Remove it first if you want to clone a fresh context repo."
        );
    }

    let repo = repo_dir(root);
    std::fs::create_dir_all(&sc_dir)?;

    // Clone as bare repo
    eprintln!("[subcontext] Cloning context repo from {url}...");
    run_git(&["clone", "--bare", url, &repo.to_string_lossy()], root)
        .context("failed to clone context repo")?;

    // Set up config worktree
    let cfg = sc_dir.join("config");
    run_subcontext_git(&["worktree", "add", &cfg.to_string_lossy(), "config"], root)
        .context("failed to set up config worktree (does the 'config' branch exist in the remote?)")?;

    // Set up work/ worktree for current branch's overlay
    let branch = current_branch(root)?;
    let safe_branch = sanitize_branch_name(&branch);
    let overlay_branch = format!("overlay/{safe_branch}");

    if !overlay::overlay_branch_exists(root, &overlay_branch)? {
        overlay::create_overlay_branch(root, &overlay_branch)?;
    }

    let work = work_dir(root);
    run_subcontext_git(
        &["worktree", "add", &work.to_string_lossy(), &overlay_branch],
        root,
    )?;

    // Apply overlay
    let ctx = CheckoutContext::main_only(root);
    overlay::apply_overlay(&ctx)?;

    // Install hooks, settings, etc.
    install_from_hooks(root, false)?;

    eprintln!("[subcontext] Clone complete.");
    Ok(())
}
