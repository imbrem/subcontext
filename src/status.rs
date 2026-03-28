use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::git;

/// Find the nearest git root and the main repo root.
/// If they differ, we're in a linked worktree.
fn find_git_roots(start: &Path) -> Result<(PathBuf, PathBuf)> {
    let mut current = start
        .canonicalize()
        .context("failed to canonicalize start path")?;
    loop {
        let dot_git = current.join(".git");
        if dot_git.is_dir() {
            return Ok((current.clone(), current));
        }
        if dot_git.is_file() {
            // .git file contains "gitdir: /path/to/.git/worktrees/<name>"
            let contents = std::fs::read_to_string(&dot_git).context("failed to read .git file")?;
            let gitdir = contents
                .strip_prefix("gitdir: ")
                .unwrap_or(&contents)
                .trim();
            let gitdir_path = if Path::new(gitdir).is_absolute() {
                PathBuf::from(gitdir)
            } else {
                current.join(gitdir)
            };
            let main_git_dir = gitdir_path
                .canonicalize()
                .context("failed to resolve worktree gitdir")?;
            // .git/worktrees/<name> → .git/worktrees → .git → repo root
            let main_root = main_git_dir
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf())
                .context("failed to derive main repo root from worktree gitdir")?;
            return Ok((current, main_root));
        }
        if !current.pop() {
            anyhow::bail!("not inside a Git repository");
        }
    }
}

pub fn status(cwd: &Path) -> Result<()> {
    let (current_root, main_root) = find_git_roots(cwd)?;
    let is_worktree = current_root != main_root;

    if is_worktree {
        println!("Worktree:    {}", current_root.display());
        println!("Main repo:   {}", main_root.display());
    } else {
        println!("Main repo:   {}", current_root.display());
        println!("Worktree:    no (this is the main checkout)");
    }

    match git::current_branch(&current_root) {
        Ok(branch) => println!("Branch:      {}", branch),
        Err(_) => println!("Branch:      (detached HEAD)"),
    }

    let main_has_sc = main_root.join(".subcontext").is_dir();
    if is_worktree {
        let wt_has_sc = current_root.join(".subcontext").is_dir();
        println!(
            "Subcontext:  {}",
            if main_has_sc {
                "installed (in main repo)"
            } else if wt_has_sc {
                "installed (in this worktree)"
            } else {
                "not installed"
            }
        );
    } else {
        println!(
            "Subcontext:  {}",
            if main_has_sc {
                "installed"
            } else {
                "not installed"
            }
        );
    }

    Ok(())
}
