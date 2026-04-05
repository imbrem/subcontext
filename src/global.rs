//! Global (per-user) subcontext.
//!
//! A global subcontext is installed outside any particular Git project. It
//! lives at `$GIT_SUBCONTEXT_PATH` (or `~/.subcontext` by default) and holds
//! a self-contained bare repo + worktrees with `kind: user` rather than
//! `kind: project`.
//!
//! The layout mirrors `.git/.subcontext/` but is nested one level deeper so
//! the user-level directory can also host unrelated data (other worktrees,
//! databases, ...).
//!
//! ```text
//! ~/.subcontext/
//! └── subcontext/         ← this nested dir is the subcontext base
//!     ├── repo/
//!     ├── work/
//!     ├── config/
//!     └── state/
//! ```

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::backend::Backend;
use crate::git::run_git_in_bare;
use crate::project::{USER_KIND, ensure_config_in};
use crate::task::{TaskScope, init_state_branch_in};

pub const GLOBAL_ENV: &str = "GIT_SUBCONTEXT_PATH";
/// Name of the nested subdirectory holding the bare repo + worktrees.
const NESTED_DIR: &str = "subcontext";
/// Default overlay branch used by the global subcontext (no host branch to
/// mirror).
pub const DEFAULT_OVERLAY_BRANCH: &str = "overlay/main";
/// Pseudo-branch recorded in task_names for global tasks.
pub const GLOBAL_HOST_BRANCH: &str = "global";

/// Resolve the global subcontext root directory.
/// Honours `$GIT_SUBCONTEXT_PATH`, otherwise `$HOME/.subcontext`.
pub fn global_root() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os(GLOBAL_ENV) {
        return Ok(PathBuf::from(explicit));
    }
    let home = std::env::var_os("HOME").context("HOME environment variable is not set")?;
    Ok(PathBuf::from(home).join(".subcontext"))
}

/// The nested directory containing the bare repo + worktrees.
pub fn global_subcontext_dir() -> Result<PathBuf> {
    Ok(global_root()?.join(NESTED_DIR))
}

pub fn global_repo_dir() -> Result<PathBuf> {
    Ok(global_subcontext_dir()?.join("repo"))
}

pub fn global_config_dir() -> Result<PathBuf> {
    Ok(global_subcontext_dir()?.join("config"))
}

pub fn global_work_dir() -> Result<PathBuf> {
    Ok(global_subcontext_dir()?.join("work"))
}

pub fn global_state_dir() -> Result<PathBuf> {
    Ok(global_subcontext_dir()?.join("state"))
}

/// Does a global subcontext already exist on this machine?
pub fn global_exists(backend: &dyn Backend) -> Result<bool> {
    Ok(backend.exists(&global_repo_dir()?))
}

/// Install a fresh global subcontext. Idempotent: re-runs leave an existing
/// install alone.
pub fn install(backend: &dyn Backend) -> Result<()> {
    let sc_dir = global_subcontext_dir()?;
    if global_exists(backend)? {
        eprintln!(
            "[subcontext] Global subcontext already installed at {}.",
            sc_dir.display()
        );
        return Ok(());
    }

    eprintln!(
        "[subcontext] Initializing global subcontext at {}...",
        sc_dir.display()
    );
    backend.create_dir_all(&sc_dir)?;

    let repo = global_repo_dir()?;
    let cfg = global_config_dir()?;
    let work = global_work_dir()?;
    let state = global_state_dir()?;

    // 1. Init bare repo.
    backend.create_dir_all(&repo)?;
    // Run git init on the bare repo directory directly.
    backend.git(&crate::backend::GitInvocation::new(
        &["init", "--bare", &repo.to_string_lossy()],
        &sc_dir,
    ))?;

    // 2. Create config branch via plumbing.
    let empty_tree = run_git_in_bare(
        backend,
        &["hash-object", "-t", "tree", "/dev/null"],
        &repo,
        &repo,
    )?;
    let config_commit = run_git_in_bare(
        backend,
        &["commit-tree", &empty_tree, "-m", "init config branch"],
        &repo,
        &repo,
    )?;
    run_git_in_bare(
        backend,
        &["update-ref", "refs/heads/config", &config_commit],
        &repo,
        &repo,
    )?;

    // 3. Add config worktree.
    run_git_in_bare(
        backend,
        &["worktree", "add", &cfg.to_string_lossy(), "config"],
        &repo,
        &repo,
    )?;

    // 4. Write the user subcontext.yaml (kind: user).
    ensure_config_in(backend, &cfg, USER_KIND)?;

    // 5. Commit the config tree.
    crate::git::run_git(backend, &["add", "-A"], &cfg)?;
    let status = crate::git::run_git(backend, &["status", "--porcelain"], &cfg)?;
    if !status.is_empty() {
        crate::git::run_git(
            backend,
            &["commit", "-m", "subcontext: init user config"],
            &cfg,
        )?;
    }

    // 6. Create the default overlay branch and its worktree.
    let empty_tree = run_git_in_bare(
        backend,
        &["hash-object", "-t", "tree", "/dev/null"],
        &repo,
        &repo,
    )?;
    let overlay_commit = run_git_in_bare(
        backend,
        &[
            "commit-tree",
            &empty_tree,
            "-m",
            &format!("init {DEFAULT_OVERLAY_BRANCH}"),
        ],
        &repo,
        &repo,
    )?;
    run_git_in_bare(
        backend,
        &[
            "update-ref",
            &format!("refs/heads/{DEFAULT_OVERLAY_BRANCH}"),
            &overlay_commit,
        ],
        &repo,
        &repo,
    )?;
    run_git_in_bare(
        backend,
        &[
            "worktree",
            "add",
            &work.to_string_lossy(),
            DEFAULT_OVERLAY_BRANCH,
        ],
        &repo,
        &repo,
    )?;

    // 7. Initialize state branch + tasks.db.
    init_state_branch_in(backend, &repo, &state)?;

    eprintln!("[subcontext] Global subcontext installed.");
    Ok(())
}

/// Build a TaskScope that targets the global subcontext.
pub fn global_task_scope(backend: &dyn Backend) -> Result<TaskScope> {
    let repo = global_repo_dir()?;
    let state = global_state_dir()?;
    let sc_dir = global_subcontext_dir()?;
    let cfg = global_config_dir()?;
    let project_uuid = crate::project::read_project_uuid_at(backend, &cfg)?;
    Ok(TaskScope {
        repo_dir: repo,
        state_dir: state,
        scratch_base: sc_dir,
        host_branch: GLOBAL_HOST_BRANCH.to_string(),
        project_uuid,
    })
}

/// Register a child subcontext in the global subcontext by creating a
/// `children/<child_uuid>` branch containing a small JSON metadata blob.
/// No-op (returns `Ok`) if no global subcontext is installed.
pub fn register_child(backend: &dyn Backend, child_uuid: &str, child_kind: &str) -> Result<()> {
    if !global_exists(backend)? {
        return Ok(());
    }
    let repo = global_repo_dir()?;
    let ref_name = format!("refs/heads/children/{child_uuid}");

    // Idempotent: if the branch already exists, leave it alone.
    if run_git_in_bare(
        backend,
        &["show-ref", "--verify", "--quiet", &ref_name],
        &repo,
        &repo,
    )
    .is_ok()
    {
        return Ok(());
    }

    let json = format!("{{\n  \"uuid\": \"{child_uuid}\",\n  \"kind\": \"{child_kind}\"\n}}\n");

    // Write the JSON blob via `git hash-object -w --stdin` would need stdin
    // support; instead write a scratch file and hash it, mirroring task.rs.
    let sc_dir = global_subcontext_dir()?;
    backend.create_dir_all(&sc_dir)?;
    let tmp = sc_dir.join(format!(".child-{child_uuid}.tmp"));
    backend.write(&tmp, json.as_bytes())?;
    let blob = run_git_in_bare(
        backend,
        &["hash-object", "-w", &tmp.to_string_lossy()],
        &repo,
        &repo,
    )?;
    backend.remove_file(&tmp).ok();

    // Build a one-entry tree with child.json via a scratch index.
    let idx = sc_dir.join(format!(".child-index-{child_uuid}.tmp"));
    if backend.exists(&idx) {
        backend.remove_file(&idx).ok();
    }
    let git_dir_flag = format!("--git-dir={}", repo.display());
    let cacheinfo = format!("100644,{blob},child.json");
    let idx_os: &std::ffi::OsStr = idx.as_os_str();

    backend.git(&crate::backend::GitInvocation {
        args: &[
            git_dir_flag.as_str(),
            "update-index",
            "--add",
            "--cacheinfo",
            &cacheinfo,
        ],
        cwd: &repo,
        env_set: &[("GIT_INDEX_FILE", idx_os)],
        env_remove: &[],
    })?;
    let tree = backend.git(&crate::backend::GitInvocation {
        args: &[git_dir_flag.as_str(), "write-tree"],
        cwd: &repo,
        env_set: &[("GIT_INDEX_FILE", idx_os)],
        env_remove: &[],
    })?;
    backend.remove_file(&idx).ok();

    let commit = run_git_in_bare(
        backend,
        &[
            "commit-tree",
            &tree,
            "-m",
            &format!("register child {child_uuid}"),
        ],
        &repo,
        &repo,
    )?;
    run_git_in_bare(backend, &["update-ref", &ref_name, &commit], &repo, &repo)?;

    eprintln!("[subcontext] Registered child {child_uuid} ({child_kind}) in global subcontext.");
    Ok(())
}
