use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use crate::git::{run_git, run_subcontext_git, run_work_git, CheckoutContext};

// ─── Branch operations (bare repo only, no checkout context needed) ──

/// Create an empty overlay branch via plumbing (no worktree changes).
pub fn create_overlay_branch(root: &Path, branch: &str) -> Result<()> {
    // Create an empty tree (hash the empty file as a tree object)
    let empty_tree = run_subcontext_git(&["hash-object", "-t", "tree", "/dev/null"], root)?;

    // Create a commit pointing to the empty tree
    let commit = run_subcontext_git(
        &[
            "commit-tree",
            &empty_tree,
            "-m",
            &format!("init {branch}"),
        ],
        root,
    )?;

    // Create the ref
    run_subcontext_git(&["update-ref", &format!("refs/heads/{branch}"), &commit], root)?;

    Ok(())
}

/// Create an overlay branch forked from a source branch.
/// The new branch starts with the same content as `source`.
pub fn create_overlay_branch_from(root: &Path, branch: &str, source: &str) -> Result<()> {
    run_subcontext_git(&["branch", branch, source], root)?;
    Ok(())
}

/// Check if an overlay branch exists in the subcontext repo.
pub fn overlay_branch_exists(root: &Path, branch: &str) -> Result<bool> {
    let result =
        run_subcontext_git(&["show-ref", "--verify", &format!("refs/heads/{branch}")], root);
    Ok(result.is_ok())
}

// ─── Work directory management ───────────────────────────────────────

/// Get the current branch name in the overlay work directory.
pub fn current_work_branch(ctx: &CheckoutContext) -> Result<Option<String>> {
    let work = ctx.overlay_work_dir();
    if !work.exists() {
        return Ok(None);
    }
    match run_work_git(&["symbolic-ref", "--short", "HEAD"], &work) {
        Ok(branch) if !branch.is_empty() => Ok(Some(branch)),
        _ => Ok(None),
    }
}

/// Read the main checkout's current overlay branch (for worktree fork-source).
pub fn main_checkout_overlay_branch(main_root: &Path) -> Result<Option<String>> {
    let main_work = crate::git::work_dir(main_root);
    if !main_work.exists() {
        return Ok(None);
    }
    match run_work_git(&["symbolic-ref", "--short", "HEAD"], &main_work) {
        Ok(branch) if !branch.is_empty() => {
            if overlay_branch_exists(main_root, &branch)? {
                Ok(Some(branch))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

/// Switch the overlay work directory to a different branch.
pub fn switch_work_branch(ctx: &CheckoutContext, branch: &str) -> Result<()> {
    let work = ctx.overlay_work_dir();
    run_work_git(&["checkout", branch], &work)?;
    Ok(())
}

/// Create the overlay work directory as a worktree of the bare repo.
pub fn create_overlay_work_dir(ctx: &CheckoutContext, branch: &str) -> Result<()> {
    let work = ctx.overlay_work_dir();
    if let Some(parent) = work.parent() {
        fs::create_dir_all(parent)?;
    }
    run_subcontext_git(
        &["worktree", "add", &work.to_string_lossy(), branch],
        &ctx.main_root,
    )?;
    Ok(())
}

/// List files tracked by the overlay (work directory).
pub fn list_overlay_files(ctx: &CheckoutContext) -> Result<Vec<String>> {
    let work = ctx.overlay_work_dir();
    if !work.exists() {
        return Ok(vec![]);
    }

    let output = run_work_git(&["ls-files"], &work)?;
    Ok(output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect())
}

// ─── Apply / unapply / save ─────────────────────────────────────────

/// Apply overlay: copy all files from the work dir into the checkout root.
/// Sets skip-worktree on files tracked by both repos.
pub fn apply_overlay(ctx: &CheckoutContext) -> Result<()> {
    let work = ctx.overlay_work_dir();
    let files = list_overlay_files(ctx)?;

    for file in &files {
        let src = work.join(file);
        let dest = ctx.checkout_root.join(file);

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&src, &dest)
            .with_context(|| format!("failed to copy overlay file: {file}"))?;

        // If this file is tracked by the host repo, set skip-worktree
        if is_tracked_by_host(ctx, file)? {
            run_git(&["update-index", "--skip-worktree", file], &ctx.checkout_root).ok();
        }
    }

    sync_excludes(ctx)?;
    Ok(())
}

/// Save overlay: copy tracked overlay files from checkout root back to work dir, then commit.
pub fn save_overlay(ctx: &CheckoutContext, message: &str) -> Result<()> {
    let work = ctx.overlay_work_dir();
    if !work.exists() {
        return Ok(());
    }

    let files = list_overlay_files(ctx)?;

    // Copy overlay files from checkout root back to work/
    for file in &files {
        let src = ctx.checkout_root.join(file);
        let dest = work.join(file);
        if src.exists() {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&src, &dest)?;
        }
    }

    // Stage and commit in work/
    run_work_git(&["add", "-A"], &work)?;
    let status = run_work_git(&["status", "--porcelain"], &work)?;
    if status.is_empty() {
        return Ok(());
    }
    run_work_git(&["commit", "-m", message], &work)?;

    Ok(())
}

/// Unapply overlay: remove overlay-only files from checkout root, restore host-tracked files.
pub fn unapply_overlay(ctx: &CheckoutContext) -> Result<()> {
    let files = list_overlay_files(ctx)?;

    for file in &files {
        let path = ctx.checkout_root.join(file);
        let tracked = is_tracked_by_host(ctx, file)?;

        if tracked {
            // Restore host repo version
            run_git(&["update-index", "--no-skip-worktree", file], &ctx.checkout_root).ok();
            run_git(&["checkout", "--", file], &ctx.checkout_root).ok();
        } else {
            // Remove overlay-only file
            if path.exists() {
                fs::remove_file(&path).ok();
            }
            // Clean up empty parent directories
            if let Some(parent) = path.parent() {
                remove_empty_parents(parent, &ctx.checkout_root);
            }
        }
    }

    Ok(())
}

/// Sync overlay-only files from checkout root back to work dir (they survive host checkout).
pub fn sync_back_surviving_files(ctx: &CheckoutContext) -> Result<()> {
    let work = ctx.overlay_work_dir();
    let files = list_overlay_files(ctx)?;

    for file in &files {
        let src = ctx.checkout_root.join(file);
        if src.exists() && !is_tracked_by_host(ctx, file)? {
            let dest = work.join(file);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&src, &dest)?;
        }
    }

    Ok(())
}

// ─── File add / remove ───────────────────────────────────────────────

/// Add a file to the overlay: copy to work dir, git add, sync excludes.
pub fn add_file(ctx: &CheckoutContext, path: &str) -> Result<()> {
    let src = ctx.checkout_root.join(path);
    let work = ctx.overlay_work_dir();
    let dest = work.join(path);

    anyhow::ensure!(src.exists(), "file does not exist: {path}");

    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&src, &dest).with_context(|| format!("failed to copy {path} to work/"))?;

    run_work_git(&["add", path], &work)?;

    // If tracked by host repo, set skip-worktree
    if is_tracked_by_host(ctx, path)? {
        run_git(&["update-index", "--skip-worktree", path], &ctx.checkout_root).ok();
    }

    sync_excludes(ctx)?;
    Ok(())
}

/// Remove a file from the overlay.
pub fn remove_file(ctx: &CheckoutContext, path: &str) -> Result<()> {
    let work = ctx.overlay_work_dir();

    // Remove from work/ worktree
    run_work_git(&["rm", "-f", path], &work)?;

    let root_path = ctx.checkout_root.join(path);
    let tracked = is_tracked_by_host(ctx, path)?;

    if tracked {
        // Restore host repo version
        run_git(&["update-index", "--no-skip-worktree", path], &ctx.checkout_root).ok();
        run_git(&["checkout", "--", path], &ctx.checkout_root).ok();
    } else {
        // Remove from checkout root
        if root_path.exists() {
            fs::remove_file(&root_path).ok();
        }
        if let Some(parent) = root_path.parent() {
            remove_empty_parents(parent, &ctx.checkout_root);
        }
    }

    sync_excludes(ctx)?;
    Ok(())
}

// ─── Excludes ────────────────────────────────────────────────────────

/// Update the dynamic section in .git/info/exclude with overlay file list.
/// Uses tagged sections so multiple worktrees can coexist in the shared exclude file.
pub fn sync_excludes(ctx: &CheckoutContext) -> Result<()> {
    let exclude_path = ctx.main_root.join(".git").join("info").join("exclude");

    if let Some(parent) = exclude_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let existing = fs::read_to_string(&exclude_path).unwrap_or_default();

    // Determine our section markers
    let (start_marker, end_marker) = exclude_markers(&ctx.worktree_name);

    // Remove our section, keep everything else (including other worktrees' sections)
    let mut lines: Vec<&str> = Vec::new();
    let mut in_our_section = false;
    for line in existing.lines() {
        if line == start_marker {
            in_our_section = true;
            continue;
        }
        if line == end_marker {
            in_our_section = false;
            continue;
        }
        if !in_our_section {
            lines.push(line);
        }
    }

    // Get overlay files that are NOT tracked by the host repo (those need exclude)
    let overlay_files = list_overlay_files(ctx)?;
    let mut exclude_files: Vec<String> = Vec::new();
    for file in &overlay_files {
        if !is_tracked_by_host(ctx, file)? {
            exclude_files.push(file.clone());
        }
    }

    // Build new content
    let mut content = lines.join("\n");
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }

    if !exclude_files.is_empty() {
        content.push_str(&start_marker);
        content.push('\n');
        for file in &exclude_files {
            content.push_str(file);
            content.push('\n');
        }
        content.push_str(&end_marker);
        content.push('\n');
    }

    fs::write(&exclude_path, content)?;

    Ok(())
}

/// Clean ALL subcontext overlay sections from .git/info/exclude (for uninstall).
pub fn clean_all_excludes(root: &Path) -> Result<()> {
    let exclude_path = root.join(".git").join("info").join("exclude");
    if !exclude_path.exists() {
        return Ok(());
    }

    let existing = fs::read_to_string(&exclude_path)?;
    let mut lines: Vec<&str> = Vec::new();
    let mut in_section = false;
    for line in existing.lines() {
        if line.starts_with("# subcontext-overlay-start") {
            in_section = true;
            continue;
        }
        if line.starts_with("# subcontext-overlay-end") {
            in_section = false;
            continue;
        }
        if !in_section {
            lines.push(line);
        }
    }

    let content = lines.join("\n") + "\n";
    fs::write(&exclude_path, content)?;

    Ok(())
}

// ─── Helpers ─────────────────────────────────────────────────────────

/// Check if a file is tracked by the host repo's index.
fn is_tracked_by_host(ctx: &CheckoutContext, file: &str) -> Result<bool> {
    let result = run_git(&["ls-files", "--error-unmatch", file], &ctx.checkout_root);
    Ok(result.is_ok())
}

/// Section markers for the exclude file. Untagged for main, tagged for worktrees.
fn exclude_markers(worktree_name: &Option<String>) -> (String, String) {
    match worktree_name {
        None => (
            "# subcontext-overlay-start".to_string(),
            "# subcontext-overlay-end".to_string(),
        ),
        Some(name) => (
            format!("# subcontext-overlay-start:{name}"),
            format!("# subcontext-overlay-end:{name}"),
        ),
    }
}

/// Remove empty parent directories up to (but not including) stop_at.
fn remove_empty_parents(dir: &Path, stop_at: &Path) {
    let mut current = dir.to_path_buf();
    while current != stop_at.to_path_buf() {
        if fs::remove_dir(&current).is_err() {
            break; // not empty or doesn't exist
        }
        if !current.pop() {
            break;
        }
    }
}
