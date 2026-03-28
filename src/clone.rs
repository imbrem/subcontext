use anyhow::{Context, Result, bail};
use std::path::Path;

use crate::git::run_git;
use crate::install::install_from_step5;

/// Run `subcontext clone <url>` from the given repo root.
pub fn clone(root: &Path, url: &str) -> Result<()> {
    let subcontext_dir = root.join(".subcontext");

    if subcontext_dir.exists() {
        bail!(
            ".subcontext/ already exists. Remove it first if you want to clone a fresh context repo."
        );
    }

    // Step 2: Clone the context repo
    eprintln!("[subcontext] Cloning context repo from {url}...");
    run_git(&["clone", url, &subcontext_dir.to_string_lossy()], root)
        .context("failed to clone context repo")?;

    // Step 4: Check out config branch as a worktree
    let mnt_config = subcontext_dir.join(".mnt").join("config");
    eprintln!("[subcontext] Setting up config worktree...");
    run_git(
        &["worktree", "add", &mnt_config.to_string_lossy(), "config"],
        &subcontext_dir,
    )
    .context("failed to set up config worktree (does the 'config' branch exist in the remote?)")?;

    // Steps 5–11
    install_from_step5(root)?;

    Ok(())
}
