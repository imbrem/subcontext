use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::backend::{Backend, GitInvocation};
use crate::git::{
    current_branch, repo_dir, run_git_in_bare, run_work_git, state_dir, subcontext_dir,
};
use crate::project::read_project_uuid;

pub const DB_NAME: &str = "tasks.db";
pub const DEFAULT_KIND: &str = "task";
pub const DEFAULT_STATUS: &str = "created";

/// Path layout for task operations. Decouples task code from whether we're
/// running against a local (.git/.subcontext) or global (~/.subcontext)
/// subcontext.
pub struct TaskScope {
    /// Bare git repo directory.
    pub repo_dir: PathBuf,
    /// State-branch worktree directory.
    pub state_dir: PathBuf,
    /// Directory to stash scratch files (blobs, indexes) during git plumbing.
    pub scratch_base: PathBuf,
    /// Branch name under which `task_names` entries are recorded. For local
    /// installs this is the current host branch; for global it's a fixed
    /// identifier ("global").
    pub host_branch: String,
    /// UUID that owns these tasks (project UUID for local, user UUID for global).
    pub project_uuid: String,
}

impl TaskScope {
    /// Build a scope for a local (per-host-repo) subcontext install.
    pub fn for_local(backend: &dyn Backend, root: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            repo_dir: repo_dir(root),
            state_dir: state_dir(root),
            scratch_base: subcontext_dir(root),
            host_branch: current_branch(backend, root)?,
            project_uuid: read_project_uuid(backend, root)?,
        })
    }

    fn db_path(&self) -> PathBuf {
        self.state_dir.join(DB_NAME)
    }
}

/// Initialize the `state` branch, its worktree, and the tasks.db schema
/// against the local (per-host-repo) subcontext layout.
pub fn init_state_branch(backend: &dyn Backend, root: &Path) -> Result<()> {
    init_state_branch_in(backend, &repo_dir(root), &state_dir(root))
}

/// Initialize the `state` branch + worktree + tasks.db schema against an
/// arbitrary bare repo + state dir. Works for both local and global layouts.
pub fn init_state_branch_in(backend: &dyn Backend, bare: &Path, state: &Path) -> Result<()> {
    // Create empty state branch via plumbing
    let empty_tree = run_git_in_bare(
        backend,
        &["hash-object", "-t", "tree", "/dev/null"],
        bare,
        bare,
    )?;
    let commit = run_git_in_bare(
        backend,
        &["commit-tree", &empty_tree, "-m", "init state branch"],
        bare,
        bare,
    )?;
    run_git_in_bare(
        backend,
        &["update-ref", "refs/heads/state", &commit],
        bare,
        bare,
    )?;

    // Add worktree
    run_git_in_bare(
        backend,
        &["worktree", "add", &state.to_string_lossy(), "state"],
        bare,
        bare,
    )?;

    // Create DB + schema
    let conn = Connection::open(state.join(DB_NAME))?;
    create_schema(&conn)?;
    drop(conn);

    // Commit
    commit_state_in(backend, state, "init tasks db")?;
    Ok(())
}

fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tasks (
             task_uuid       TEXT PRIMARY KEY,
             task_name       TEXT NOT NULL,
             task_status     TEXT NOT NULL,
             task_kind       TEXT NOT NULL,
             project_uuid    TEXT NOT NULL,
             source_uuid     TEXT DEFAULT NULL,
             source_context  TEXT DEFAULT NULL
         );
         CREATE TABLE IF NOT EXISTS task_names (
             branch_name   TEXT NOT NULL,
             task_name     TEXT NOT NULL,
             task_uuid     TEXT NOT NULL,
             PRIMARY KEY (branch_name, task_name)
         );",
    )?;
    Ok(())
}

/// Open the tasks DB at `scope.db_path()` and ensure the schema is
/// up-to-date. Safe to call against a DB created by an older subcontext
/// version: `ALTER TABLE ... ADD COLUMN` is attempted idempotently and
/// swallows the "duplicate column name" error raised when it already exists.
fn open_db(scope: &TaskScope) -> Result<Connection> {
    let conn = Connection::open(scope.db_path())?;
    let _ = conn.execute("ALTER TABLE tasks ADD COLUMN source_uuid TEXT", []);
    let _ = conn.execute("ALTER TABLE tasks ADD COLUMN source_context TEXT", []);
    Ok(conn)
}

fn commit_state_in(backend: &dyn Backend, state: &Path, message: &str) -> Result<()> {
    run_work_git(backend, &["add", "-A"], state)?;
    let status = run_work_git(backend, &["status", "--porcelain"], state)?;
    if status.is_empty() {
        return Ok(());
    }
    run_work_git(backend, &["commit", "-m", message], state)?;
    Ok(())
}

/// Add a new task. Returns the new task's UUID.
///
/// If `source` is `Some((source_uuid, source_context))`, the new task is a
/// **shadow** of another task: its `source_uuid` / `source_context` columns
/// are populated, and `source_context` is used as the `task_names.branch_name`
/// namespace (so shadow tasks from different origin projects don't collide).
pub fn add_task(
    backend: &dyn Backend,
    scope: &TaskScope,
    name: &str,
    kind: Option<&str>,
    status: Option<&str>,
    source: Option<(&str, &str)>,
) -> Result<String> {
    // Shadow tasks live under their origin's project UUID as the branch
    // namespace; native tasks live under the scope's host_branch.
    let branch: String = match source {
        Some((_, ctx)) => ctx.to_string(),
        None => scope.host_branch.clone(),
    };
    let task_uuid = Uuid::new_v4().to_string();
    let kind = kind.unwrap_or(DEFAULT_KIND);
    let status = status.unwrap_or(DEFAULT_STATUS);

    let conn = open_db(scope)?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT task_uuid FROM task_names WHERE branch_name = ?1 AND task_name = ?2",
            params![branch, name],
            |r| r.get(0),
        )
        .ok();
    if existing.is_some() {
        bail!("task '{name}' already exists on branch '{branch}'");
    }
    let (source_uuid, source_context) = match source {
        Some((u, c)) => (Some(u.to_string()), Some(c.to_string())),
        None => (None, None),
    };
    conn.execute(
        "INSERT INTO tasks (task_uuid, task_name, task_status, task_kind, project_uuid, \
                            source_uuid, source_context) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            task_uuid,
            name,
            status,
            kind,
            scope.project_uuid,
            source_uuid,
            source_context
        ],
    )?;
    conn.execute(
        "INSERT INTO task_names (branch_name, task_name, task_uuid) VALUES (?1, ?2, ?3)",
        params![branch, name, task_uuid],
    )?;
    drop(conn);

    let commit_msg = if source.is_some() {
        format!("task add (shadow): {name}")
    } else {
        format!("task add: {name}")
    };
    commit_state_in(backend, &scope.state_dir, &commit_msg)?;

    let md = build_task_md(&TaskData {
        name: name.to_string(),
        uuid: task_uuid.clone(),
        status: status.to_string(),
        kind: kind.to_string(),
        project_uuid: scope.project_uuid.clone(),
        completed_at: None,
        source_uuid: source_uuid.clone(),
        source_context: source_context.clone(),
    });
    create_task_branch(backend, scope, &task_uuid, &md)?;

    if source.is_some() {
        eprintln!("[subcontext] Added shadow task '{name}' ({task_uuid})");
    } else {
        eprintln!("[subcontext] Added task '{name}' ({task_uuid})");
    }
    Ok(task_uuid)
}

/// Mark an existing task as done.
pub fn done_task(
    backend: &dyn Backend,
    scope: &TaskScope,
    name: &str,
    time: Option<&str>,
) -> Result<()> {
    let branch = &scope.host_branch;

    let conn = open_db(scope)?;
    let row: (
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
    ) = conn
        .query_row(
            "SELECT t.task_uuid, t.task_name, t.task_kind, t.project_uuid, \
                    t.source_uuid, t.source_context
             FROM task_names n JOIN tasks t ON n.task_uuid = t.task_uuid
             WHERE n.branch_name = ?1 AND n.task_name = ?2",
            params![branch, name],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .with_context(|| format!("task '{name}' not found on branch '{branch}'"))?;
    let (task_uuid, task_name, kind, project_uuid, source_uuid, source_context) = row;

    conn.execute(
        "UPDATE tasks SET task_status = 'done' WHERE task_uuid = ?1",
        params![task_uuid],
    )?;
    drop(conn);

    let completed_at = resolve_timestamp(time)?;

    commit_state_in(backend, &scope.state_dir, &format!("task done: {name}"))?;

    let md = build_task_md(&TaskData {
        name: task_name,
        uuid: task_uuid.clone(),
        status: "done".to_string(),
        kind,
        project_uuid,
        completed_at: Some(completed_at),
        source_uuid,
        source_context,
    });
    update_task_branch(backend, scope, &task_uuid, &md)?;

    eprintln!("[subcontext] Marked task '{name}' as done");
    Ok(())
}

struct TaskData {
    name: String,
    uuid: String,
    status: String,
    kind: String,
    project_uuid: String,
    completed_at: Option<String>,
    source_uuid: Option<String>,
    source_context: Option<String>,
}

fn build_task_md(t: &TaskData) -> String {
    let mut s = String::new();
    s.push_str("---\n");
    s.push_str(&format!("name: {}\n", t.name));
    s.push_str(&format!("uuid: {}\n", t.uuid));
    s.push_str(&format!("status: {}\n", t.status));
    s.push_str(&format!("kind: {}\n", t.kind));
    s.push_str(&format!("project_uuid: {}\n", t.project_uuid));
    if let Some(ts) = &t.completed_at {
        s.push_str(&format!("completed_at: {ts}\n"));
    }
    if let Some(u) = &t.source_uuid {
        s.push_str(&format!("source_uuid: {u}\n"));
    }
    if let Some(c) = &t.source_context {
        s.push_str(&format!("source_context: {c}\n"));
    }
    s.push_str("---\n");
    s
}

fn create_task_branch(
    backend: &dyn Backend,
    scope: &TaskScope,
    task_uuid: &str,
    md: &str,
) -> Result<()> {
    let ref_name = format!("refs/heads/tasks/{task_uuid}");
    // Defensive: refuse to clobber an existing ref (UUID collision).
    if run_git_in_bare(
        backend,
        &["show-ref", "--verify", "--quiet", &ref_name],
        &scope.repo_dir,
        &scope.repo_dir,
    )
    .is_ok()
    {
        bail!("task branch {ref_name} already exists");
    }
    let blob = hash_object(backend, scope, md)?;
    let tree = build_tree(backend, scope, "TASK.md", &blob)?;
    let commit = run_git_in_bare(
        backend,
        &[
            "commit-tree",
            &tree,
            "-m",
            &format!("init task {task_uuid}"),
        ],
        &scope.repo_dir,
        &scope.repo_dir,
    )?;
    run_git_in_bare(
        backend,
        &["update-ref", &ref_name, &commit],
        &scope.repo_dir,
        &scope.repo_dir,
    )?;
    Ok(())
}

fn update_task_branch(
    backend: &dyn Backend,
    scope: &TaskScope,
    task_uuid: &str,
    md: &str,
) -> Result<()> {
    let branch = format!("tasks/{task_uuid}");
    let ref_name = format!("refs/heads/{branch}");
    let parent = run_git_in_bare(
        backend,
        &["rev-parse", &ref_name],
        &scope.repo_dir,
        &scope.repo_dir,
    )?;
    let blob = hash_object(backend, scope, md)?;
    let tree = build_tree(backend, scope, "TASK.md", &blob)?;
    let commit = run_git_in_bare(
        backend,
        &[
            "commit-tree",
            &tree,
            "-p",
            &parent,
            "-m",
            &format!("update task {task_uuid}"),
        ],
        &scope.repo_dir,
        &scope.repo_dir,
    )?;
    run_git_in_bare(
        backend,
        &["update-ref", &ref_name, &commit],
        &scope.repo_dir,
        &scope.repo_dir,
    )?;
    Ok(())
}

/// Hash `content` as a blob in the subcontext bare repo, returning its SHA.
/// Routes through the Backend by writing to a scratch file and calling
/// `git hash-object -w`.
fn hash_object(backend: &dyn Backend, scope: &TaskScope, content: &str) -> Result<String> {
    let tmp = scratch_path(&scope.scratch_base, "blob");
    if let Some(parent) = tmp.parent() {
        backend.create_dir_all(parent)?;
    }
    backend.write(&tmp, content.as_bytes())?;
    let result = run_git_in_bare(
        backend,
        &["hash-object", "-w", &tmp.to_string_lossy()],
        &scope.repo_dir,
        &scope.repo_dir,
    );
    backend.remove_file(&tmp).ok();
    result
}

/// Build a single-entry tree in the subcontext bare repo using a temporary
/// index file (so we don't need to pipe stdin into `git mktree`).
fn build_tree(backend: &dyn Backend, scope: &TaskScope, name: &str, blob: &str) -> Result<String> {
    let idx = scratch_path(&scope.scratch_base, "index");
    if let Some(parent) = idx.parent() {
        backend.create_dir_all(parent)?;
    }
    // Ensure no stale index is left from a prior failed run.
    if backend.exists(&idx) {
        backend.remove_file(&idx).ok();
    }

    let git_dir_flag = format!("--git-dir={}", scope.repo_dir.display());
    let cacheinfo = format!("100644,{blob},{name}");
    let idx_os: &OsStr = idx.as_os_str();

    let update_args = [
        git_dir_flag.as_str(),
        "update-index",
        "--add",
        "--cacheinfo",
        &cacheinfo,
    ];
    backend.git(&GitInvocation {
        args: &update_args,
        cwd: &scope.repo_dir,
        env_set: &[("GIT_INDEX_FILE", idx_os)],
        env_remove: &[],
    })?;

    let write_tree_args = [git_dir_flag.as_str(), "write-tree"];
    let tree = backend.git(&GitInvocation {
        args: &write_tree_args,
        cwd: &scope.repo_dir,
        env_set: &[("GIT_INDEX_FILE", idx_os)],
        env_remove: &[],
    })?;

    backend.remove_file(&idx).ok();
    Ok(tree)
}

fn scratch_path(base: &Path, tag: &str) -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    base.join(format!(".task-{tag}-{nanos}-{}.tmp", std::process::id()))
}

/// Resolve a user-provided timestamp. `None` or `Some("now")` → current UTC time.
/// Any other value must be an ISO8601 UTC timestamp ending in 'Z'.
fn resolve_timestamp(time: Option<&str>) -> Result<String> {
    match time {
        None => Ok(current_iso8601()),
        Some(s) if s.eq_ignore_ascii_case("now") => Ok(current_iso8601()),
        Some(s) => {
            if !s.ends_with('Z') {
                bail!(
                    "--time must be an ISO8601 UTC timestamp ending with 'Z' \
                     (got: {s}). Use \"now\" for the current time."
                );
            }
            Ok(s.to_string())
        }
    }
}

/// Current time as an ISO8601 UTC timestamp (seconds precision, 'Z' suffix).
/// Uses Unix time (seconds since 1970-01-01T00:00:00Z), not local time.
fn current_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso8601_from_unix(secs)
}

fn iso8601_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let s = rem % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Convert days since Unix epoch (1970-01-01) to (year, month, day).
/// Based on Howard Hinnant's civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso8601_epoch_is_1970() {
        assert_eq!(iso8601_from_unix(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn iso8601_known_values() {
        // 2000-01-01T00:00:00Z = 946684800
        assert_eq!(iso8601_from_unix(946_684_800), "2000-01-01T00:00:00Z");
        // 2026-04-12T12:34:56Z = 1775997296
        assert_eq!(iso8601_from_unix(1_775_997_296), "2026-04-12T12:34:56Z");
        // Leap day 2024-02-29T23:59:59Z = 1709251199
        assert_eq!(iso8601_from_unix(1_709_251_199), "2024-02-29T23:59:59Z");
    }

    #[test]
    fn iso8601_handles_pre_epoch() {
        // 1969-12-31T23:59:59Z = -1
        assert_eq!(iso8601_from_unix(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn resolve_timestamp_accepts_now() {
        let out = resolve_timestamp(Some("now")).unwrap();
        assert!(out.ends_with('Z'));
        let out = resolve_timestamp(Some("NOW")).unwrap();
        assert!(out.ends_with('Z'));
    }

    #[test]
    fn resolve_timestamp_accepts_z_terminated() {
        let t = "2026-04-05T12:00:00Z";
        assert_eq!(resolve_timestamp(Some(t)).unwrap(), t);
    }

    #[test]
    fn resolve_timestamp_rejects_non_utc() {
        assert!(resolve_timestamp(Some("2026-04-05T12:00:00")).is_err());
        assert!(resolve_timestamp(Some("2026-04-05T12:00:00+02:00")).is_err());
    }

    #[test]
    fn resolve_timestamp_none_is_current() {
        let out = resolve_timestamp(None).unwrap();
        assert!(out.ends_with('Z'));
        assert_eq!(out.len(), 20); // YYYY-MM-DDTHH:MM:SSZ
    }
}
