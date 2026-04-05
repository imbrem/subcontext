use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

use crate::backend::Backend;
use crate::git::{CheckoutContext, run_work_git};
use crate::overlay;

/// Add a git submodule to the overlay.
pub fn add(
    backend: &dyn Backend,
    ctx: &CheckoutContext,
    url: &str,
    path: Option<&str>,
) -> Result<()> {
    let work = ctx.overlay_work_dir();
    if !backend.exists(&work) {
        bail!("subcontext is not installed (run `git subcontext install` first)");
    }

    let submodule_path = match path {
        Some(p) => p.to_string(),
        None => derive_path_from_url(url),
    };

    eprintln!("[subcontext] Adding submodule {url} at {submodule_path}...");

    // Add submodule in the overlay work directory
    run_work_git(
        backend,
        &["submodule", "add", url, &submodule_path],
        &work,
    )?;

    // Commit the submodule addition
    run_work_git(backend, &["add", "-A"], &work)?;
    run_work_git(
        backend,
        &["commit", "-m", &format!("add submodule {submodule_path}")],
        &work,
    )?;

    // Apply overlay (copies submodule files + .gitmodules to checkout root)
    overlay::apply_overlay(backend, ctx)?;

    eprintln!("[subcontext] Submodule added: {submodule_path}");
    Ok(())
}

/// Initialize and update overlay submodules.
pub fn update(backend: &dyn Backend, ctx: &CheckoutContext) -> Result<()> {
    let work = ctx.overlay_work_dir();
    if !backend.exists(&work) {
        bail!("subcontext is not installed (run `git subcontext install` first)");
    }

    if !backend.exists(&work.join(".gitmodules")) {
        eprintln!("[subcontext] No submodules to update.");
        return Ok(());
    }

    eprintln!("[subcontext] Updating submodules...");

    // Force initialization (fatal on error, unlike apply_overlay's soft init)
    run_work_git(
        backend,
        &["submodule", "update", "--init", "--recursive"],
        &work,
    )?;

    // Re-apply overlay to copy submodule contents to checkout root
    overlay::apply_overlay(backend, ctx)?;

    eprintln!("[subcontext] Submodules updated.");
    Ok(())
}

/// Remove a submodule from the overlay.
pub fn remove(backend: &dyn Backend, ctx: &CheckoutContext, path: &str) -> Result<()> {
    let work = ctx.overlay_work_dir();
    if !backend.exists(&work) {
        bail!("subcontext is not installed (run `git subcontext install` first)");
    }

    // Verify the path is actually a submodule
    let submodules = overlay::list_overlay_submodule_paths(backend, ctx)?;
    if !submodules.contains(&path.to_string()) {
        bail!("{path} is not a submodule in the overlay");
    }

    eprintln!("[subcontext] Removing submodule {path}...");

    // Remove submodule directory from checkout root
    let checkout_path = ctx.checkout_root.join(path);
    if backend.exists(&checkout_path) {
        backend.remove_dir_all(&checkout_path)?;
    }

    // Deinit and remove in work dir
    run_work_git(backend, &["submodule", "deinit", "-f", path], &work)?;
    run_work_git(backend, &["rm", "-f", path], &work)?;

    // Clean up modules directory in gitdir
    clean_submodule_gitdir(backend, &work, path);

    // Commit
    run_work_git(backend, &["add", "-A"], &work)?;
    let status = run_work_git(backend, &["status", "--porcelain"], &work)?;
    if !status.is_empty() {
        run_work_git(
            backend,
            &["commit", "-m", &format!("remove submodule {path}")],
            &work,
        )?;
    }

    // Re-apply overlay to sync .gitmodules and excludes
    overlay::apply_overlay(backend, ctx)?;

    eprintln!("[subcontext] Submodule removed: {path}");
    Ok(())
}

/// Derive a submodule path from its URL (like git submodule add does).
fn derive_path_from_url(url: &str) -> String {
    let name = url.rsplit('/').next().unwrap_or(url);
    name.strip_suffix(".git").unwrap_or(name).to_string()
}

/// Clean up the submodule's git directory from the worktree's modules/ folder.
fn clean_submodule_gitdir(backend: &dyn Backend, work: &Path, submodule_path: &str) {
    let dot_git = work.join(".git");
    let gitdir = if backend.is_file(&dot_git) {
        if let Ok(content) = backend.read_to_string(&dot_git) {
            let p = content.strip_prefix("gitdir: ").unwrap_or(&content).trim();
            if Path::new(p).is_absolute() {
                PathBuf::from(p)
            } else {
                work.join(p)
            }
        } else {
            return;
        }
    } else {
        dot_git
    };
    let modules_path = gitdir.join("modules").join(submodule_path);
    if backend.exists(&modules_path) {
        let _ = backend.remove_dir_all(&modules_path);
    }
}
