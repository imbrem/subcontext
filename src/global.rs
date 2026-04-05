//! Global (per-user) subcontext.
//!
//! A global subcontext is installed outside any particular Git project. It
//! lives at `$GIT_SUBCONTEXT_PATH` (or `~/.subcontext` by default) and holds
//! a self-contained bare repo + worktrees with `kind: user` rather than
//! `kind: project`.
//!
//! The layout mirrors `.git/.subcontext/` but is nested under a `global/`
//! directory so the user-level root can also host unrelated data (other
//! worktrees, databases, ...).
//!
//! ```text
//! ~/.subcontext/
//! └── global/              ← this nested dir is the subcontext base
//!     ├── repo/
//!     ├── work/
//!     ├── config/
//!     └── state/
//! ```

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::backend::{Backend, GitInvocation};
use crate::git::run_git_in_bare;
use crate::project::{USER_KIND, ensure_config_in};
use crate::task::{TaskScope, init_state_branch_in};

pub const GLOBAL_ENV: &str = "GIT_SUBCONTEXT_PATH";
/// Name of the nested subdirectory holding the bare repo + worktrees.
const NESTED_DIR: &str = "global";
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
    backend.init_bare(&sc_dir, &repo)?;

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
    backend.worktree_add(&repo, &cfg, "config")?;

    // 4. Write the user subcontext.yaml (kind: user).
    ensure_config_in(backend, &cfg, USER_KIND)?;

    // 5. Commit the config tree.
    backend.add_all(&cfg)?;
    let status = backend.status_porcelain(&cfg)?;
    if !status.is_empty() {
        backend.commit(&cfg, "subcontext: init user config")?;
    }

    // 6. Create the default overlay branch and its worktree.
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
    backend.worktree_add(&repo, &work, DEFAULT_OVERLAY_BRANCH)?;

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

/// Register a child subcontext in the global subcontext by creating an
/// `object/<child_uuid>` branch with a single `object.json` (type "child",
/// child data inlined under "data").
/// Returns `Some(commit_sha)` if a branch was created, `None` if it already
/// existed or no global subcontext is installed.
pub fn register_child(
    backend: &dyn Backend,
    child_uuid: &str,
    child_kind: &str,
) -> Result<Option<String>> {
    if !global_exists(backend)? {
        return Ok(None);
    }
    let repo = global_repo_dir()?;
    let ref_name = format!("refs/heads/object/{child_uuid}");

    // Idempotent: if the branch already exists, leave it alone.
    if run_git_in_bare(
        backend,
        &["show-ref", "--verify", "--quiet", &ref_name],
        &repo,
        &repo,
    )
    .is_ok()
    {
        return Ok(None);
    }

    let child_data = serde_json::json!({
        "uuid": child_uuid,
        "kind": child_kind,
    });
    let object_json = crate::task::build_child_object_json(&child_data);

    // Write blob via scratch file.
    let sc_dir = global_subcontext_dir()?;
    backend.create_dir_all(&sc_dir)?;

    let tmp = sc_dir.join(format!(".child-{child_uuid}.tmp"));
    backend.write(&tmp, object_json.as_bytes())?;
    let blob = run_git_in_bare(
        backend,
        &["hash-object", "-w", &tmp.to_string_lossy()],
        &repo,
        &repo,
    )?;
    backend.remove_file(&tmp).ok();

    // Build a single-entry tree with object.json via a scratch index.
    let idx = sc_dir.join(format!(".child-index-{child_uuid}.tmp"));
    if backend.exists(&idx) {
        backend.remove_file(&idx).ok();
    }
    let git_dir_flag = format!("--git-dir={}", repo.display());
    let cacheinfo = format!("100644,{blob},object.json");
    let idx_os: &std::ffi::OsStr = idx.as_os_str();

    backend.git(&GitInvocation {
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
    let tree = backend.git(&GitInvocation {
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
    Ok(Some(commit))
}

/// Record a checkout path (path to the child's `.git` *folder*) in the
/// `object/<child_uuid>` branch's `object.json` (under `data.checkout_path`).
/// If the file already lists a different path, promote `checkout_path` to an
/// array; adding a path that is already present is a no-op.
///
/// Returns `Some(commit_sha)` if the branch was updated, `None` if no update
/// was needed. No-op if no global subcontext is installed or the object
/// branch doesn't exist yet.
pub fn record_child_checkout_path(
    backend: &dyn Backend,
    child_uuid: &str,
    git_dir: &Path,
) -> Result<Option<String>> {
    if !global_exists(backend)? {
        return Ok(None);
    }
    let repo = global_repo_dir()?;
    let ref_name = format!("refs/heads/object/{child_uuid}");

    // No object branch → nothing to update.
    if run_git_in_bare(
        backend,
        &["show-ref", "--verify", "--quiet", &ref_name],
        &repo,
        &repo,
    )
    .is_err()
    {
        return Ok(None);
    }

    let current = run_git_in_bare(
        backend,
        &["show", &format!("object/{child_uuid}:object.json")],
        &repo,
        &repo,
    )?;
    let mut val: serde_json::Value = serde_json::from_str(&current)
        .with_context(|| format!("invalid object.json on object/{child_uuid}"))?;

    let data = val
        .get_mut("data")
        .context("missing 'data' key in object.json")?;

    let new_path = git_dir.to_string_lossy().to_string();
    match data.get("checkout_path").cloned() {
        None | Some(serde_json::Value::Null) => {
            data["checkout_path"] = serde_json::Value::String(new_path);
        }
        Some(serde_json::Value::String(existing)) => {
            if existing == new_path {
                return Ok(None);
            }
            data["checkout_path"] = serde_json::json!([existing, new_path]);
        }
        Some(serde_json::Value::Array(mut arr)) => {
            if arr
                .iter()
                .any(|v| v.as_str().is_some_and(|s| s == new_path))
            {
                return Ok(None);
            }
            arr.push(serde_json::Value::String(new_path));
            data["checkout_path"] = serde_json::Value::Array(arr);
        }
        Some(_) => {
            data["checkout_path"] = serde_json::Value::String(new_path);
        }
    }

    let new_json = serde_json::to_string_pretty(&val)? + "\n";

    // Write new blob, build tree, commit with the current tip as parent.
    let sc_dir = global_subcontext_dir()?;
    backend.create_dir_all(&sc_dir)?;

    let tmp = sc_dir.join(format!(".child-update-{child_uuid}.tmp"));
    backend.write(&tmp, new_json.as_bytes())?;
    let blob = run_git_in_bare(
        backend,
        &["hash-object", "-w", &tmp.to_string_lossy()],
        &repo,
        &repo,
    )?;
    backend.remove_file(&tmp).ok();

    let idx = sc_dir.join(format!(".child-update-index-{child_uuid}.tmp"));
    if backend.exists(&idx) {
        backend.remove_file(&idx).ok();
    }
    let git_dir_flag = format!("--git-dir={}", repo.display());
    let cacheinfo = format!("100644,{blob},object.json");
    let idx_os: &std::ffi::OsStr = idx.as_os_str();

    backend.git(&GitInvocation {
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
    let tree = backend.git(&GitInvocation {
        args: &[git_dir_flag.as_str(), "write-tree"],
        cwd: &repo,
        env_set: &[("GIT_INDEX_FILE", idx_os)],
        env_remove: &[],
    })?;
    backend.remove_file(&idx).ok();

    let parent = run_git_in_bare(backend, &["rev-parse", &ref_name], &repo, &repo)?;
    let commit = run_git_in_bare(
        backend,
        &[
            "commit-tree",
            &tree,
            "-p",
            &parent,
            "-m",
            &format!("update child {child_uuid} checkout_path"),
        ],
        &repo,
        &repo,
    )?;
    run_git_in_bare(backend, &["update-ref", &ref_name, &commit], &repo, &repo)?;
    Ok(Some(commit))
}
