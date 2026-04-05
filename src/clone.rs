use anyhow::{Context, Result, bail};
use std::path::Path;

use crate::backend::Backend;
use crate::git::{
    CheckoutContext, current_branch, repo_dir, run_subcontext_git, sanitize_branch_name, state_dir,
    subcontext_dir, work_dir,
};
use crate::install::install_from_hooks;
use crate::overlay;

/// Run `subcontext clone <url>` from the given repo root.
pub fn clone(backend: &dyn Backend, root: &Path, url: &str) -> Result<()> {
    let sc_dir = subcontext_dir(root);

    if backend.exists(&sc_dir) {
        bail!(
            ".git/.subcontext/ already exists. Remove it first if you want to clone a fresh context repo."
        );
    }

    let repo = repo_dir(root);
    backend.create_dir_all(&sc_dir)?;

    // Clone as bare repo
    eprintln!("[subcontext] Cloning context repo from {url}...");
    backend
        .clone_bare(root, url, &repo)
        .context("failed to clone context repo")?;

    // Set up config worktree
    let cfg = sc_dir.join("config");
    run_subcontext_git(
        backend,
        &["worktree", "add", &cfg.to_string_lossy(), "config"],
        root,
    )
    .context("failed to set up config worktree (does the 'config' branch exist in the remote?)")?;

    // Set up work/ worktree for current branch's overlay
    let branch = current_branch(backend, root)?;
    let safe_branch = sanitize_branch_name(&branch);
    let overlay_branch = format!("overlay/{safe_branch}");

    if !overlay::overlay_branch_exists(backend, root, &overlay_branch)? {
        overlay::create_overlay_branch(backend, root, &overlay_branch)?;
    }

    let work = work_dir(root);
    run_subcontext_git(
        backend,
        &["worktree", "add", &work.to_string_lossy(), &overlay_branch],
        root,
    )?;

    // Set up state worktree if the remote has a state branch
    if overlay::overlay_branch_exists(backend, root, "state")? {
        let state = state_dir(root);
        run_subcontext_git(
            backend,
            &["worktree", "add", &state.to_string_lossy(), "state"],
            root,
        )?;
    } else {
        // Remote did not ship a state branch; create one locally.
        crate::task::init_state_branch(backend, root)?;
    }

    // Apply overlay
    let ctx = CheckoutContext::main_only(root);
    overlay::apply_overlay(backend, &ctx)?;

    // Install hooks, settings, etc.
    install_from_hooks(backend, root, false)?;

    eprintln!("[subcontext] Clone complete.");
    Ok(())
}
