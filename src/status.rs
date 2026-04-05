use anyhow::{Context, Result};
use std::fmt::Write;
use std::path::{Path, PathBuf};

use crate::backend::Backend;
use crate::git;

/// Find the nearest git root and the main repo root.
fn find_git_roots(backend: &dyn Backend, start: &Path) -> Result<(PathBuf, PathBuf)> {
    let mut current = backend
        .canonicalize(start)
        .context("failed to canonicalize start path")?;
    loop {
        let dot_git = current.join(".git");
        if backend.is_dir(&dot_git) {
            return Ok((current.clone(), current));
        }
        if backend.is_file(&dot_git) {
            let contents = backend
                .read_to_string(&dot_git)
                .context("failed to read .git file")?;
            let gitdir = contents
                .strip_prefix("gitdir: ")
                .unwrap_or(&contents)
                .trim();
            let gitdir_path = if Path::new(gitdir).is_absolute() {
                PathBuf::from(gitdir)
            } else {
                current.join(gitdir)
            };
            let main_git_dir = backend
                .canonicalize(&gitdir_path)
                .context("failed to resolve worktree gitdir")?;
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

pub fn status(backend: &dyn Backend, cwd: &Path) -> Result<()> {
    let text = status_text(backend, cwd)?;
    print!("{text}");
    Ok(())
}

/// Build the status report as a string. Used by both the CLI and the MCP server.
pub fn status_text(backend: &dyn Backend, cwd: &Path) -> Result<String> {
    let (current_root, main_root) = find_git_roots(backend, cwd)?;
    let is_worktree = current_root != main_root;

    let mut out = String::new();

    if is_worktree {
        writeln!(out, "Worktree:    {}", current_root.display())?;
        writeln!(out, "Main repo:   {}", main_root.display())?;
    } else {
        writeln!(out, "Main repo:   {}", current_root.display())?;
        writeln!(out, "Worktree:    no (this is the main checkout)")?;
    }

    match git::current_branch(backend, &current_root) {
        Ok(branch) => writeln!(out, "Branch:      {}", branch)?,
        Err(_) => writeln!(out, "Branch:      (detached HEAD)")?,
    }

    let sc_dir = main_root.join(".git").join(".subcontext");
    let state = match (is_worktree, backend.is_dir(&sc_dir)) {
        (true, true) => "installed (in main repo)",
        (true, false) => "not installed",
        (false, true) => "installed",
        (false, false) => "not installed",
    };
    writeln!(out, "Subcontext:  {}", state)?;

    Ok(out)
}
