use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Run a git command with the given args in the given directory.
/// Returns stdout as a trimmed string.
pub fn run_git(args: &[&str], cwd: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to execute git {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "git {} failed (exit {}): {}",
            args.join(" "),
            output.status,
            stderr.trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(stdout)
}

/// Walk up from `start` to find a directory containing a `.git/` **directory**
/// (not a file, which indicates a linked worktree). Returns the repo root.
pub fn find_main_git_root(start: &Path) -> Result<PathBuf> {
    let mut current = start
        .canonicalize()
        .context("failed to canonicalize start path")?;
    loop {
        let dot_git = current.join(".git");
        if dot_git.exists() {
            if dot_git.is_file() {
                bail!(
                    "found .git file at {} — this is a linked worktree, not the main checkout. \
                     Run subcontext install from the main checkout.",
                    current.display()
                );
            }
            if dot_git.is_dir() {
                return Ok(current);
            }
        }
        if !current.pop() {
            bail!("not inside a Git repository (no .git/ directory found)");
        }
    }
}

/// Get the current branch name via `git symbolic-ref --short HEAD`.
pub fn current_branch(repo_root: &Path) -> Result<String> {
    run_git(&["symbolic-ref", "--short", "HEAD"], repo_root)
        .context("failed to determine current branch (detached HEAD?)")
}

/// Sanitize a branch name for use as a Git branch name segment.
/// Replaces `/` with `-`, strips leading dots.
pub fn sanitize_branch_name(name: &str) -> String {
    let sanitized: String = name.replace('/', "-").trim_start_matches('.').to_string();
    if sanitized.is_empty() {
        "default".to_string()
    } else {
        sanitized
    }
}
