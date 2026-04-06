//! Global (per-user) subcontext.
//!
//! A global subcontext is installed outside any particular Git project. It
//! lives at `$GIT_SUBCONTEXT_PATH` (or `~/.subcontext` by default) and holds
//! a self-contained bare repo + worktrees with `kind: system` rather than
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

use anyhow::{Context, Result, bail};
use std::fmt::Write as FmtWrite;
use std::path::{Path, PathBuf};

use crate::backend::{Backend, GitInvocation};
use crate::git::run_git_in_bare;
use crate::project::{
    SYSTEM_KIND, USER_KIND, ensure_config_in, read_kind_at, read_project_uuid_at,
};
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

pub fn global_dolt_dir() -> Result<PathBuf> {
    Ok(global_subcontext_dir()?.join("dolt"))
}

/// Config dir for the user subcontext (~/.subcontext/user/config).
pub fn user_config_dir() -> Result<PathBuf> {
    Ok(global_root()?.join("user").join("config"))
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

    // 4. Write the system subcontext.yaml (kind: system).
    ensure_config_in(backend, &cfg, SYSTEM_KIND)?;

    // 5. Commit the config tree.
    backend.add_all(&cfg)?;
    let status = backend.status_porcelain(&cfg)?;
    if !status.is_empty() {
        backend.commit(&cfg, "subcontext: init system config")?;
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

    // 7. Initialize state branch + Dolt database.
    let dolt_path = global_dolt_dir()?;
    // Ensure dolt binary is available.
    if crate::dolt::find_dolt_bin().is_err() {
        crate::dolt::download_dolt(backend)?;
    }
    init_state_branch_in(backend, &repo, &state, &dolt_path)?;

    eprintln!("[subcontext] Global subcontext installed.");
    Ok(())
}

/// Build a TaskScope that targets the global subcontext.
pub fn global_task_scope(backend: &dyn Backend) -> Result<TaskScope> {
    let repo = global_repo_dir()?;
    let state = global_state_dir()?;
    let sc_dir = global_subcontext_dir()?;
    let dolt_path = global_dolt_dir()?;
    let cfg = global_config_dir()?;
    let project_uuid = crate::project::read_project_uuid_at(backend, &cfg)?;
    Ok(TaskScope {
        repo_dir: repo,
        state_dir: state,
        dolt_dir: dolt_path,
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

// ─── User subcontexts ───────────────────────────────────────────────

/// Install a user subcontext as a managed child of the global (system)
/// subcontext. Returns the user UUID.
pub fn install_user(backend: &dyn Backend) -> Result<String> {
    if !global_exists(backend)? {
        bail!("no global (system) subcontext installed — run `subcontext install --global` first");
    }

    let root = global_root()?;
    let user_dir = root.join("user");
    let repo = user_dir.join("repo");
    let cfg = user_dir.join("config");
    let work = user_dir.join("work");
    let state = user_dir.join("state");

    if backend.exists(&repo) {
        let uuid = read_project_uuid_at(backend, &cfg)?;
        eprintln!("[subcontext] User subcontext already installed (UUID: {uuid}).");
        return Ok(uuid);
    }

    eprintln!(
        "[subcontext] Initializing user subcontext at {}...",
        user_dir.display()
    );
    backend.create_dir_all(&user_dir)?;

    // 1. Init bare repo.
    backend.init_bare(&user_dir, &repo)?;

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
    let user_uuid = ensure_config_in(backend, &cfg, USER_KIND)?;

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

    // 7. Initialize state branch + Dolt database.
    let dolt_path = user_dir.join("dolt");
    if crate::dolt::find_dolt_bin().is_err() {
        crate::dolt::download_dolt(backend)?;
    }
    init_state_branch_in(backend, &repo, &state, &dolt_path)?;

    // 8. Register as managed child of the system subcontext.
    if let Some(commit) = register_child(backend, &user_uuid, USER_KIND)? {
        let global_scope = global_task_scope(backend)?;
        let conn = crate::task::open_db(&global_scope)?;
        crate::task::insert_object(&conn, &user_uuid, "managed", &commit, None)?;
        crate::task::dolt_commit_and_track(
            backend,
            &global_scope,
            &format!("object add: {user_uuid}"),
        )?;
    }

    // 9. Set as current user if none exists.
    let global_scope = global_task_scope(backend)?;
    let conn = crate::task::open_db(&global_scope)?;
    ensure_global_extra_schema(&conn)?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT value FROM config WHERE `key` = 'current_user'",
            &[],
            |row| row.get::<Option<String>>(0),
        )?
        .flatten();
    if existing.is_none() {
        conn.execute(
            "REPLACE INTO config (`key`, value) VALUES (?1, ?2)",
            &[&"current_user", &user_uuid.as_str()],
        )?;
        crate::task::dolt_commit_and_track(
            backend,
            &global_scope,
            &format!("set current user: {user_uuid}"),
        )?;
        eprintln!("[subcontext] Set current user to {user_uuid}.");
    }

    eprintln!("[subcontext] User subcontext installed (UUID: {user_uuid}).");
    Ok(user_uuid)
}

/// Build a TaskScope targeting the user subcontext.
pub fn user_task_scope(backend: &dyn Backend) -> Result<TaskScope> {
    let root = global_root()?;
    let user_dir = root.join("user");
    let repo = user_dir.join("repo");
    let state = user_dir.join("state");
    let dolt_path = user_dir.join("dolt");
    let cfg = user_dir.join("config");
    if !backend.exists(&repo) {
        bail!("no user subcontext installed — run `subcontext install --user` first");
    }
    let project_uuid = read_project_uuid_at(backend, &cfg)?;
    Ok(TaskScope {
        repo_dir: repo,
        state_dir: state,
        dolt_dir: dolt_path,
        scratch_base: user_dir,
        host_branch: GLOBAL_HOST_BRANCH.to_string(),
        project_uuid,
    })
}

/// Ensure extra schema tables exist in the global DB (config table, parents
/// table). Called lazily — safe to call multiple times.
pub fn ensure_global_extra_schema(conn: &crate::dolt::DoltConnection) -> Result<()> {
    crate::dolt::create_dolt_global_schema(conn)?;
    Ok(())
}

// ─── Current user ───────────────────────────────────────────────────

/// Get the current user UUID from the system subcontext config.
pub fn get_current_user(backend: &dyn Backend) -> Result<Option<String>> {
    if !global_exists(backend)? {
        return Ok(None);
    }
    let scope = global_task_scope(backend)?;
    let conn = crate::task::open_db(&scope)?;
    ensure_global_extra_schema(&conn)?;
    let val: Option<String> = conn
        .query_row(
            "SELECT value FROM config WHERE `key` = 'current_user'",
            &[],
            |row| row.get::<Option<String>>(0),
        )?
        .flatten();
    Ok(val)
}

/// Set the current user UUID. Validates that the UUID refers to a user-kind
/// managed subcontext.
pub fn set_current_user(backend: &dyn Backend, uuid: &str) -> Result<()> {
    if !global_exists(backend)? {
        bail!("no global (system) subcontext installed");
    }
    let scope = global_task_scope(backend)?;
    let conn = crate::task::open_db(&scope)?;
    ensure_global_extra_schema(&conn)?;

    // Verify it's a managed user subcontext by checking the object branch.
    let repo = global_repo_dir()?;
    let ref_name = format!("refs/heads/object/{uuid}");
    if run_git_in_bare(
        backend,
        &["show-ref", "--verify", "--quiet", &ref_name],
        &repo,
        &repo,
    )
    .is_err()
    {
        bail!("UUID {uuid} is not registered in the system subcontext");
    }

    let json_str = run_git_in_bare(
        backend,
        &["show", &format!("object/{uuid}:object.json")],
        &repo,
        &repo,
    )?;
    let val: serde_json::Value = serde_json::from_str(&json_str)?;
    let kind = val
        .pointer("/data/kind")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if kind != USER_KIND {
        bail!("UUID {uuid} has kind '{kind}', not 'user'");
    }

    conn.execute(
        "REPLACE INTO config (`key`, value) VALUES (?1, ?2)",
        &[&"current_user", uuid],
    )?;
    crate::task::dolt_commit_and_track(backend, &scope, &format!("set current user: {uuid}"))?;
    eprintln!("[subcontext] Current user set to {uuid}.");
    Ok(())
}

// ─── Parent-child relationships ─────────────────────────────────────

/// Record that `child_uuid` has `parent_uuid` as its parent in the system DB.
/// A child can only have one parent; re-setting is an error unless the same.
pub fn set_parent(backend: &dyn Backend, child_uuid: &str, parent_uuid: &str) -> Result<()> {
    let scope = global_task_scope(backend)?;
    let conn = crate::task::open_db(&scope)?;
    ensure_global_extra_schema(&conn)?;

    let existing: Option<String> = conn
        .query_row(
            "SELECT parent_uuid FROM parents WHERE child_uuid = ?1",
            &[child_uuid],
            |row| row.get::<Option<String>>(0),
        )?
        .flatten();

    match existing {
        Some(ref p) if p == parent_uuid => return Ok(()),
        Some(p) => bail!("child {child_uuid} already has parent {p}, cannot set to {parent_uuid}"),
        None => {}
    }

    conn.execute(
        "INSERT INTO parents (child_uuid, parent_uuid) VALUES (?1, ?2)",
        &[child_uuid, parent_uuid],
    )?;
    crate::task::dolt_commit_and_track(
        backend,
        &scope,
        &format!("set parent of {child_uuid} to {parent_uuid}"),
    )?;
    Ok(())
}

/// Get the parent UUID of a child from the system DB.
pub fn get_parent(backend: &dyn Backend, child_uuid: &str) -> Result<Option<String>> {
    if !global_exists(backend)? {
        return Ok(None);
    }
    let scope = global_task_scope(backend)?;
    let conn = crate::task::open_db(&scope)?;
    ensure_global_extra_schema(&conn)?;
    let val: Option<String> = conn
        .query_row(
            "SELECT parent_uuid FROM parents WHERE child_uuid = ?1",
            &[child_uuid],
            |row| row.get::<Option<String>>(0),
        )?
        .flatten();
    Ok(val)
}

/// Get all children of a parent from the system DB.
pub fn get_children(backend: &dyn Backend, parent_uuid: &str) -> Result<Vec<String>> {
    if !global_exists(backend)? {
        return Ok(vec![]);
    }
    let scope = global_task_scope(backend)?;
    let conn = crate::task::open_db(&scope)?;
    ensure_global_extra_schema(&conn)?;
    let children: Vec<String> = conn.query_map(
        "SELECT child_uuid FROM parents WHERE parent_uuid = ?1",
        &[parent_uuid],
        |row| row.get::<String>(0),
    )?;
    Ok(children)
}

// ─── Tree / info helpers ────────────────────────────────────────────

/// Get kind of a managed subcontext from its object branch.
pub fn get_managed_kind(backend: &dyn Backend, uuid: &str) -> Result<String> {
    let repo = global_repo_dir()?;
    let json_str = run_git_in_bare(
        backend,
        &["show", &format!("object/{uuid}:object.json")],
        &repo,
        &repo,
    )?;
    let val: serde_json::Value = serde_json::from_str(&json_str)?;
    Ok(val
        .pointer("/data/kind")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string())
}

/// Build a tree string showing all managed subcontexts under the system subcontext.
pub fn tree_text(backend: &dyn Backend) -> Result<String> {
    if !global_exists(backend)? {
        bail!("no global (system) subcontext installed");
    }
    let scope = global_task_scope(backend)?;
    let system_uuid = scope.project_uuid.clone();

    let conn = crate::task::open_db(&scope)?;
    ensure_global_extra_schema(&conn)?;

    // Collect all managed objects.
    let managed: Vec<(String, String)> = conn.query_map(
        "SELECT uuid, `type` FROM objects WHERE `type` = 'managed'",
        &[],
        |row| Ok((row.get::<String>(0)?, row.get::<String>(1)?)),
    )?;

    // Build parent→children map from the parents table.
    let mut parent_children: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut child_parent: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    {
        let rows: Vec<(String, String)> =
            conn.query_map("SELECT child_uuid, parent_uuid FROM parents", &[], |row| {
                Ok((row.get::<String>(0)?, row.get::<String>(1)?))
            })?;
        for (child, parent) in rows {
            parent_children
                .entry(parent.clone())
                .or_default()
                .push(child.clone());
            child_parent.insert(child, parent);
        }
    }

    let mut out = String::new();
    let system_kind = "system";
    writeln!(out, "{system_uuid} ({system_kind})")?;

    // Find top-level managed (those whose parent is NOT in managed set, or have no parent).
    let managed_uuids: std::collections::HashSet<String> =
        managed.iter().map(|(u, _)| u.clone()).collect();

    fn print_subtree(
        backend: &dyn Backend,
        out: &mut String,
        uuid: &str,
        parent_children: &std::collections::HashMap<String, Vec<String>>,
        managed_uuids: &std::collections::HashSet<String>,
        prefix: &str,
        is_last: bool,
    ) -> Result<()> {
        let kind = get_managed_kind(backend, uuid).unwrap_or_else(|_| "unknown".to_string());
        let connector = if is_last { "└── " } else { "├── " };
        writeln!(out, "{prefix}{connector}{uuid} ({kind})")?;
        let child_prefix = if is_last {
            format!("{prefix}    ")
        } else {
            format!("{prefix}│   ")
        };
        if let Some(children) = parent_children.get(uuid) {
            let relevant: Vec<&String> = children
                .iter()
                .filter(|c| managed_uuids.contains(c.as_str()))
                .collect();
            for (i, child) in relevant.iter().enumerate() {
                let last = i == relevant.len() - 1;
                print_subtree(
                    backend,
                    out,
                    child,
                    parent_children,
                    managed_uuids,
                    &child_prefix,
                    last,
                )?;
            }
        }
        Ok(())
    }

    // Top-level = managed whose parent is the system UUID or who have no parent.
    let mut top_level: Vec<&String> = managed_uuids
        .iter()
        .filter(|u| match child_parent.get(u.as_str()) {
            None => true,
            Some(p) => p == &system_uuid,
        })
        .collect();
    top_level.sort();

    for (i, uuid) in top_level.iter().enumerate() {
        let last = i == top_level.len() - 1;
        print_subtree(
            backend,
            &mut out,
            uuid,
            &parent_children,
            &managed_uuids,
            "",
            last,
        )?;
    }

    Ok(out)
}

/// Build the ancestry chain for a given UUID (walking up parents).
/// Returns vec of (uuid, kind) from the given UUID up to the root.
/// Does NOT include the system subcontext itself.
pub fn ancestry_chain(backend: &dyn Backend, uuid: &str) -> Result<Vec<(String, String)>> {
    if !global_exists(backend)? {
        return Ok(vec![]);
    }
    let scope = global_task_scope(backend)?;
    let system_uuid = scope.project_uuid.clone();
    let conn = crate::task::open_db(&scope)?;
    ensure_global_extra_schema(&conn)?;

    let mut chain = vec![];
    let mut current = uuid.to_string();

    // Walk up parents, stop if we hit the system UUID or run out.
    loop {
        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_uuid FROM parents WHERE child_uuid = ?1",
                &[current.as_str()],
                |row| row.get::<Option<String>>(0),
            )?
            .flatten();
        match parent {
            Some(p) if p != system_uuid => {
                let kind = get_managed_kind(backend, &p).unwrap_or_else(|_| "unknown".to_string());
                chain.push((p.clone(), kind));
                current = p;
            }
            _ => break,
        }
    }
    Ok(chain)
}

/// Get the system subcontext's UUID.
pub fn system_uuid(backend: &dyn Backend) -> Result<String> {
    let cfg = global_config_dir()?;
    read_project_uuid_at(backend, &cfg)
}

/// Get the system subcontext's kind.
pub fn system_kind(backend: &dyn Backend) -> Result<String> {
    let cfg = global_config_dir()?;
    read_kind_at(backend, &cfg)
}
