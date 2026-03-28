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

/// Run a git command in the subcontext bare repo.
pub fn run_subcontext_git(args: &[&str], root: &Path) -> Result<String> {
    let git_dir_flag = format!("--git-dir={}", repo_dir(root).display());
    let mut full_args: Vec<&str> = vec![&git_dir_flag];
    full_args.extend_from_slice(args);
    run_git(&full_args, root)
}

/// Run a git command in an overlay work directory.
/// Must set GIT_DIR explicitly because work dirs live inside .git/ and
/// git's directory walk would find the main repo's .git dir first.
pub fn run_work_git(args: &[&str], work: &Path) -> Result<String> {
    // Read the gitdir from work/.git file
    let dot_git = work.join(".git");
    let gitdir = if dot_git.is_file() {
        let content = std::fs::read_to_string(&dot_git)
            .context("failed to read work/.git file")?;
        let path = content
            .strip_prefix("gitdir: ")
            .unwrap_or(&content)
            .trim();
        if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            work.join(path)
        }
    } else {
        work.join(".git")
    };

    let output = Command::new("git")
        .args(args)
        .current_dir(work)
        .env("GIT_DIR", &gitdir)
        .env("GIT_WORK_TREE", work)
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .with_context(|| format!("failed to execute git {} in work/", args.join(" ")))?;

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

// ─── Path helpers ────────────────────────────────────────────────────

/// .git/.subcontext
pub fn subcontext_dir(root: &Path) -> PathBuf {
    root.join(".git").join(".subcontext")
}

/// .git/.subcontext/work (main checkout's overlay worktree)
pub fn work_dir(root: &Path) -> PathBuf {
    subcontext_dir(root).join("work")
}

/// .git/.subcontext/config
pub fn config_dir(root: &Path) -> PathBuf {
    subcontext_dir(root).join("config")
}

/// .git/.subcontext/repo
pub fn repo_dir(root: &Path) -> PathBuf {
    subcontext_dir(root).join("repo")
}

// ─── Checkout context ────────────────────────────────────────────────

/// Identifies a checkout location. For the main checkout, main_root == checkout_root.
/// For a linked worktree, they differ and worktree_name is set.
pub struct CheckoutContext {
    /// The main repo root (has .git/ directory containing .subcontext/).
    pub main_root: PathBuf,
    /// Where overlay files are applied. Same as main_root for the main checkout.
    pub checkout_root: PathBuf,
    /// Git worktree name (from .git/worktrees/<name>). None for main checkout.
    pub worktree_name: Option<String>,
}

impl CheckoutContext {
    /// Context for the main checkout (not a worktree).
    pub fn main_only(root: &Path) -> Self {
        Self {
            main_root: root.to_path_buf(),
            checkout_root: root.to_path_buf(),
            worktree_name: None,
        }
    }

    /// The overlay work directory for this checkout.
    pub fn overlay_work_dir(&self) -> PathBuf {
        match &self.worktree_name {
            None => subcontext_dir(&self.main_root).join("work"),
            Some(name) => subcontext_dir(&self.main_root).join("worktrees").join(name),
        }
    }

    pub fn is_worktree(&self) -> bool {
        self.worktree_name.is_some()
    }
}

/// Resolve the checkout context from the current directory.
/// Works for both the main checkout and linked worktrees.
pub fn find_checkout_context(start: &Path) -> Result<CheckoutContext> {
    let mut current = start
        .canonicalize()
        .context("failed to canonicalize start path")?;
    loop {
        let dot_git = current.join(".git");
        if dot_git.is_dir() {
            return Ok(CheckoutContext::main_only(&current));
        }
        if dot_git.is_file() {
            let content = std::fs::read_to_string(&dot_git)
                .context("failed to read .git worktree file")?;
            let gitdir_str = content
                .strip_prefix("gitdir: ")
                .unwrap_or(&content)
                .trim();
            let gitdir = if Path::new(gitdir_str).is_absolute() {
                PathBuf::from(gitdir_str)
            } else {
                current.join(gitdir_str)
            };
            let gitdir = gitdir
                .canonicalize()
                .context("failed to canonicalize worktree gitdir")?;
            // gitdir is like /main/.git/worktrees/<name>
            let wt_name = gitdir
                .file_name()
                .context("invalid worktree gitdir")?
                .to_string_lossy()
                .to_string();
            let main_git = gitdir
                .parent() // .git/worktrees/
                .and_then(|p| p.parent()) // .git/
                .context("failed to derive main .git from worktree gitdir")?;
            let main_root = main_git
                .parent()
                .context("failed to derive main repo root")?
                .to_path_buf();
            return Ok(CheckoutContext {
                main_root,
                checkout_root: current,
                worktree_name: Some(wt_name),
            });
        }
        if !current.pop() {
            bail!("not inside a Git repository");
        }
    }
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
