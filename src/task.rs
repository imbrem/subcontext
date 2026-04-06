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
             task_description TEXT DEFAULT NULL,
             project_uuid    TEXT NOT NULL,
             task_deadline   TEXT DEFAULT NULL,
             task_importance REAL NOT NULL DEFAULT 0.0
         );
         CREATE TABLE IF NOT EXISTS task_names (
             branch_name   TEXT NOT NULL,
             task_name     TEXT NOT NULL,
             task_uuid     TEXT NOT NULL UNIQUE
         );
         CREATE INDEX IF NOT EXISTS idx_task_names_lookup
             ON task_names (branch_name, task_name);
         CREATE TABLE IF NOT EXISTS objects (
             uuid                  TEXT PRIMARY KEY,
             type                  TEXT NOT NULL,
             current_commit        TEXT NOT NULL,
             source_context_uuid   TEXT DEFAULT NULL,
             source_object_uuid    TEXT DEFAULT NULL,
             source_context_commit TEXT DEFAULT NULL
         );",
    )?;
    // Migrate existing databases that lack the new columns.
    let _ = conn.execute_batch(
        "ALTER TABLE tasks ADD COLUMN task_deadline TEXT DEFAULT NULL;
         ALTER TABLE tasks ADD COLUMN task_importance REAL NOT NULL DEFAULT 0.0;",
    );
    // Migrate: allow duplicate task names (old schema had PK on branch+name).
    migrate_task_names_allow_duplicates(conn);
    Ok(())
}

/// Migrate task_names from (branch_name, task_name) PK to UNIQUE(task_uuid).
/// This allows multiple tasks with the same name on the same branch.
fn migrate_task_names_allow_duplicates(conn: &Connection) {
    // Check if task_uuid already has a UNIQUE constraint (new schema).
    // If the old PK exists we need to recreate the table.
    let needs_migration: bool = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='task_names'",
            [],
            |r| r.get::<_, String>(0),
        )
        .map(|sql| sql.contains("PRIMARY KEY (branch_name, task_name)"))
        .unwrap_or(false);

    if needs_migration {
        let _ = conn.execute_batch(
            "CREATE TABLE task_names_new (
                 branch_name TEXT NOT NULL,
                 task_name   TEXT NOT NULL,
                 task_uuid   TEXT NOT NULL UNIQUE
             );
             INSERT OR IGNORE INTO task_names_new SELECT * FROM task_names;
             DROP TABLE task_names;
             ALTER TABLE task_names_new RENAME TO task_names;
             CREATE INDEX IF NOT EXISTS idx_task_names_lookup
                 ON task_names (branch_name, task_name);",
        );
    }
}

pub fn open_db(scope: &TaskScope) -> Result<Connection> {
    Ok(Connection::open(scope.db_path())?)
}

pub fn commit_state_in(backend: &dyn Backend, state: &Path, message: &str) -> Result<()> {
    run_work_git(backend, &["add", "-A"], state)?;
    let status = run_work_git(backend, &["status", "--porcelain"], state)?;
    if status.is_empty() {
        return Ok(());
    }
    run_work_git(backend, &["commit", "-m", message], state)?;
    Ok(())
}

/// Add a new task. Returns `(task_uuid, branch_commit)`.
///
/// If `source` is `Some((source_context_uuid, source_object_uuid,
/// source_context_commit))`, the new task is a **shadow** of another task:
/// `source_context_uuid` is used as the `task_names.branch_name` namespace
/// (so shadow tasks from different origin projects don't collide), and the
/// source fields are recorded in `object.json` and the `objects` table.
pub fn add_task(
    backend: &dyn Backend,
    scope: &TaskScope,
    name: &str,
    kind: Option<&str>,
    status: Option<&str>,
    description: Option<&str>,
    deadline: Option<&str>,
    importance: f64,
    source: Option<(&str, &str, &str)>,
) -> Result<(String, String)> {
    // Validate deadline if provided.
    if let Some(d) = deadline {
        if !d.ends_with('Z') {
            bail!("--deadline must be an ISO8601 UTC timestamp ending with 'Z' (got: {d})");
        }
    }

    // Shadow tasks live under their origin's project UUID as the branch
    // namespace; native tasks live under the scope's host_branch.
    let branch: String = match source {
        Some((ctx, _, _)) => ctx.to_string(),
        None => scope.host_branch.clone(),
    };
    let task_uuid = Uuid::new_v4().to_string();
    let kind = kind.unwrap_or(DEFAULT_KIND);
    let status = status.unwrap_or(DEFAULT_STATUS);

    let conn = open_db(scope)?;
    let existing: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT task_uuid FROM task_names WHERE branch_name = ?1 AND task_name = ?2")
            .unwrap();
        stmt.query_map(params![branch, name], |r| r.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    };
    if !existing.is_empty() {
        // Print to stdout so agent harnesses can see it.
        println!(
            "WARNING: task name '{}' is not unique on branch '{}'. Existing UUIDs: {}",
            name,
            branch,
            existing.join(", ")
        );
    }
    conn.execute(
        "INSERT INTO tasks (task_uuid, task_name, task_status, task_kind, task_description, project_uuid, task_deadline, task_importance) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![task_uuid, name, status, kind, description, scope.project_uuid, deadline, importance],
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

    let task_data = TaskData {
        name: name.to_string(),
        uuid: task_uuid.clone(),
        status: status.to_string(),
        kind: kind.to_string(),
        project_uuid: scope.project_uuid.clone(),
        completed_at: None,
        description: description.map(|s| s.to_string()),
        deadline: deadline.map(|s| s.to_string()),
        importance,
    };
    let object_json = build_task_object_json(&task_data, source);
    let task_md = generate_task_md(&task_data, "");
    let commit = create_object_branch(
        backend,
        scope,
        &task_uuid,
        &[("object.json", &object_json), ("TASK.md", &task_md)],
    )?;

    // Record in the objects table.
    let conn = open_db(scope)?;
    insert_object(&conn, &task_uuid, "task", &commit, source)?;
    drop(conn);
    commit_state_in(
        backend,
        &scope.state_dir,
        &format!("object add: {task_uuid}"),
    )?;

    if source.is_some() {
        eprintln!("[subcontext] Added shadow task '{name}' ({task_uuid})");
    } else {
        eprintln!("[subcontext] Added task '{name}' ({task_uuid})");
    }
    Ok((task_uuid, commit))
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
    let row: (String, String) = conn
        .query_row(
            "SELECT t.task_uuid, t.task_name \
             FROM task_names n JOIN tasks t ON n.task_uuid = t.task_uuid
             WHERE n.branch_name = ?1 AND n.task_name = ?2",
            params![branch, name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .with_context(|| format!("task '{name}' not found on branch '{branch}'"))?;
    let (task_uuid, _task_name) = row;

    conn.execute(
        "UPDATE tasks SET task_status = 'done' WHERE task_uuid = ?1",
        params![task_uuid],
    )?;
    drop(conn);

    let completed_at = resolve_timestamp(time)?;

    commit_state_in(backend, &scope.state_dir, &format!("task done: {name}"))?;

    // Read existing object.json, update the data fields.
    let obj_branch = format!("object/{task_uuid}");
    let existing = run_git_in_bare(
        backend,
        &["show", &format!("{obj_branch}:object.json")],
        &scope.repo_dir,
        &scope.repo_dir,
    )?;
    let mut val: serde_json::Value = serde_json::from_str(&existing)
        .with_context(|| format!("invalid object.json on {obj_branch}"))?;
    val["data"]["status"] = serde_json::Value::String("done".to_string());
    val["data"]["completed_at"] = serde_json::Value::String(completed_at);

    let new_json = serde_json::to_string_pretty(&val)? + "\n";

    // Sync TASK.md from updated object.json.
    let old_body = read_object_file(backend, scope, &task_uuid, "TASK.md")
        .ok()
        .flatten()
        .map(|md| parse_frontmatter(&md).1)
        .unwrap_or_default();
    let new_md = generate_task_md_from_json(&val, &old_body);

    let new_commit = update_object_branch(
        backend,
        scope,
        &task_uuid,
        &[("object.json", &new_json), ("TASK.md", &new_md)],
    )?;

    // Update the object's current_commit.
    let conn = open_db(scope)?;
    conn.execute(
        "UPDATE objects SET current_commit = ?1 WHERE uuid = ?2",
        params![new_commit, task_uuid],
    )?;
    drop(conn);
    commit_state_in(
        backend,
        &scope.state_dir,
        &format!("object update: {task_uuid}"),
    )?;

    eprintln!("[subcontext] Marked task '{name}' as done");
    Ok(())
}

/// Mark an existing task as failed.
pub fn fail_task(
    backend: &dyn Backend,
    scope: &TaskScope,
    name: &str,
    time: Option<&str>,
) -> Result<()> {
    let branch = &scope.host_branch;

    let conn = open_db(scope)?;
    let row: (String, String) = conn
        .query_row(
            "SELECT t.task_uuid, t.task_name \
             FROM task_names n JOIN tasks t ON n.task_uuid = t.task_uuid
             WHERE n.branch_name = ?1 AND n.task_name = ?2",
            params![branch, name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .with_context(|| format!("task '{name}' not found on branch '{branch}'"))?;
    let (task_uuid, _task_name) = row;

    conn.execute(
        "UPDATE tasks SET task_status = 'failed' WHERE task_uuid = ?1",
        params![task_uuid],
    )?;
    drop(conn);

    let completed_at = resolve_timestamp(time)?;

    commit_state_in(backend, &scope.state_dir, &format!("task fail: {name}"))?;

    let obj_branch = format!("object/{task_uuid}");
    let existing = run_git_in_bare(
        backend,
        &["show", &format!("{obj_branch}:object.json")],
        &scope.repo_dir,
        &scope.repo_dir,
    )?;
    let mut val: serde_json::Value = serde_json::from_str(&existing)
        .with_context(|| format!("invalid object.json on {obj_branch}"))?;
    val["data"]["status"] = serde_json::Value::String("failed".to_string());
    val["data"]["completed_at"] = serde_json::Value::String(completed_at);

    let new_json = serde_json::to_string_pretty(&val)? + "\n";

    // Sync TASK.md from updated object.json.
    let old_body = read_object_file(backend, scope, &task_uuid, "TASK.md")
        .ok()
        .flatten()
        .map(|md| parse_frontmatter(&md).1)
        .unwrap_or_default();
    let new_md = generate_task_md_from_json(&val, &old_body);

    let new_commit = update_object_branch(
        backend,
        scope,
        &task_uuid,
        &[("object.json", &new_json), ("TASK.md", &new_md)],
    )?;

    let conn = open_db(scope)?;
    conn.execute(
        "UPDATE objects SET current_commit = ?1 WHERE uuid = ?2",
        params![new_commit, task_uuid],
    )?;
    drop(conn);
    commit_state_in(
        backend,
        &scope.state_dir,
        &format!("object update: {task_uuid}"),
    )?;

    eprintln!("[subcontext] Marked task '{name}' as failed");
    Ok(())
}

/// A deadline entry returned by `list_deadlines`.
#[allow(dead_code)]
pub struct DeadlineEntry {
    pub name: String,
    pub uuid: String,
    pub status: String,
    pub kind: String,
    pub deadline: String,
    pub importance: f64,
    pub description: Option<String>,
}

/// List tasks with deadlines that are not "done" or "failed".
///
/// - `important_only`: if true, only return tasks with importance > 0.
/// - `horizon_secs`: if `Some(n)`, only return tasks whose deadline is at most
///   `n` seconds in the future from now. If `n == 0`, only past deadlines.
///   If `None`, return all matching tasks regardless of deadline time.
/// Parse a human-readable duration string into seconds.
///
/// Supported suffixes: `s` (seconds), `m` (minutes), `h` (hours),
/// `d` (days), `w` (weeks), `mo` (months, 30 days), `y` (years, 365 days).
/// A bare number without a suffix is treated as seconds.
pub fn parse_duration(s: &str) -> Result<f64> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty duration string");
    }
    // Try suffixes longest-first to match "mo" before "m".
    let (num, multiplier) = if let Some(n) = s.strip_suffix("mo") {
        (n, 30.0 * 86400.0)
    } else if let Some(n) = s.strip_suffix('y') {
        (n, 365.0 * 86400.0)
    } else if let Some(n) = s.strip_suffix('w') {
        (n, 7.0 * 86400.0)
    } else if let Some(n) = s.strip_suffix('d') {
        (n, 86400.0)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600.0)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60.0)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1.0)
    } else {
        (s, 1.0)
    };
    let value: f64 = num
        .trim()
        .parse()
        .with_context(|| format!("invalid duration: {s}"))?;
    Ok(value * multiplier)
}

pub fn list_deadlines(
    scope: &TaskScope,
    important_only: bool,
    horizon: Option<&str>,
) -> Result<Vec<DeadlineEntry>> {
    let horizon_secs: Option<f64> = match horizon {
        Some(h) => Some(parse_duration(h)?),
        None => None,
    };
    let conn = open_db(scope)?;

    let mut sql = String::from(
        "SELECT t.task_name, t.task_uuid, t.task_status, t.task_kind, \
                t.task_deadline, t.task_importance, t.task_description \
         FROM task_names n JOIN tasks t ON n.task_uuid = t.task_uuid \
         WHERE n.branch_name = ?1 \
           AND t.task_status NOT IN ('done', 'failed') \
           AND t.task_deadline IS NOT NULL",
    );
    if important_only {
        sql.push_str(" AND t.task_importance > 0.0");
    }
    sql.push_str(" ORDER BY t.task_deadline ASC");

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![scope.host_branch], |r| {
        Ok(DeadlineEntry {
            name: r.get(0)?,
            uuid: r.get(1)?,
            status: r.get(2)?,
            kind: r.get(3)?,
            deadline: r.get(4)?,
            importance: r.get(5)?,
            description: r.get(6)?,
        })
    })?;

    let now_secs = current_unix_secs();
    let mut entries = Vec::new();
    for row in rows {
        let entry = row?;
        if let Some(horizon) = horizon_secs {
            if let Some(deadline_secs) = parse_iso8601_to_unix(&entry.deadline) {
                let cutoff = now_secs as f64 + horizon;
                if (deadline_secs as f64) > cutoff {
                    continue;
                }
            }
        }
        entries.push(entry);
    }

    Ok(entries)
}

/// Format deadline entries as human-readable text.
pub fn format_deadlines(entries: &[DeadlineEntry]) -> String {
    if entries.is_empty() {
        return "No upcoming deadlines.".to_string();
    }
    let now_secs = current_unix_secs();
    let mut out = String::new();
    for e in entries {
        let overdue = parse_iso8601_to_unix(&e.deadline)
            .map(|d| d < now_secs)
            .unwrap_or(false);
        let marker = if overdue { " [OVERDUE]" } else { "" };
        let imp = if e.importance > 0.0 {
            format!(" (importance: {:.1})", e.importance)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "- {name} [{status}] deadline: {deadline}{marker}{imp}\n",
            name = e.name,
            status = e.status,
            deadline = e.deadline,
        ));
        if let Some(desc) = &e.description {
            out.push_str(&format!("  {desc}\n"));
        }
    }
    out
}

fn current_unix_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parse an ISO8601 UTC timestamp (ending in 'Z') to Unix seconds.
/// Returns `None` if the format is not recognized.
fn parse_iso8601_to_unix(s: &str) -> Option<i64> {
    // Expected: YYYY-MM-DDTHH:MM:SSZ
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let mut time_parts = time.split(':');
    let h: i64 = time_parts.next()?.parse().ok()?;
    let m: i64 = time_parts.next()?.parse().ok()?;
    let s: i64 = time_parts.next()?.parse().ok()?;
    Some(days_from_civil(year, month, day) * 86400 + h * 3600 + m * 60 + s)
}

/// Convert (year, month, day) to days since Unix epoch. Inverse of
/// `civil_from_days`. Based on Howard Hinnant's algorithm.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let m = if month <= 2 {
        month as i64 + 9
    } else {
        month as i64 - 3
    };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u64;
    let doy = (153 * m as u64 + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe as i64 - 719468
}

pub struct TaskData {
    name: String,
    uuid: String,
    status: String,
    kind: String,
    project_uuid: String,
    completed_at: Option<String>,
    description: Option<String>,
    deadline: Option<String>,
    importance: f64,
}

/// Build a complete `object.json` for a task, with all data inlined under
/// the `"data"` key.
fn build_task_object_json(t: &TaskData, source: Option<(&str, &str, &str)>) -> String {
    let mut data = serde_json::json!({
        "name": t.name,
        "uuid": t.uuid,
        "status": t.status,
        "kind": t.kind,
        "project_uuid": t.project_uuid,
        "importance": t.importance,
    });
    if let Some(ts) = &t.completed_at {
        data["completed_at"] = serde_json::Value::String(ts.clone());
    }
    // Always include description (null when absent).
    data["description"] = match &t.description {
        Some(d) => serde_json::Value::String(d.clone()),
        None => serde_json::Value::Null,
    };
    // Always include deadline (null when absent).
    data["deadline"] = match &t.deadline {
        Some(d) => serde_json::Value::String(d.clone()),
        None => serde_json::Value::Null,
    };

    let mut obj = serde_json::json!({
        "type": "task",
        "data": data,
    });
    if let Some((ctx_uuid, obj_uuid, ctx_commit)) = source {
        obj["source_context_uuid"] = serde_json::Value::String(ctx_uuid.to_string());
        obj["source_object_uuid"] = serde_json::Value::String(obj_uuid.to_string());
        obj["source_context_commit"] = serde_json::Value::String(ctx_commit.to_string());
    }
    serde_json::to_string_pretty(&obj).unwrap() + "\n"
}

/// Build a complete `object.json` for a child object.
pub fn build_child_object_json(child_data: &serde_json::Value) -> String {
    let obj = serde_json::json!({
        "type": "child",
        "data": child_data,
    });
    serde_json::to_string_pretty(&obj).unwrap() + "\n"
}

/// Insert a row into the `objects` table.
pub fn insert_object(
    conn: &Connection,
    uuid: &str,
    obj_type: &str,
    commit: &str,
    source: Option<(&str, &str, &str)>,
) -> Result<()> {
    let (src_ctx, src_obj, src_commit) = match source {
        Some((c, o, cc)) => (Some(c), Some(o), Some(cc)),
        None => (None, None, None),
    };
    conn.execute(
        "INSERT INTO objects (uuid, type, current_commit, \
                              source_context_uuid, source_object_uuid, source_context_commit) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![uuid, obj_type, commit, src_ctx, src_obj, src_commit],
    )?;
    Ok(())
}

/// Create a new `object/<uuid>` branch containing the given files.
/// Each entry in `files` is `(filename, content)`.
pub fn create_object_branch(
    backend: &dyn Backend,
    scope: &TaskScope,
    uuid: &str,
    files: &[(&str, &str)],
) -> Result<String> {
    let ref_name = format!("refs/heads/object/{uuid}");
    // Defensive: refuse to clobber an existing ref (UUID collision).
    if run_git_in_bare(
        backend,
        &["show-ref", "--verify", "--quiet", &ref_name],
        &scope.repo_dir,
        &scope.repo_dir,
    )
    .is_ok()
    {
        bail!("object branch {ref_name} already exists");
    }
    let mut entries = Vec::new();
    for (name, content) in files {
        let blob = hash_object(backend, scope, content)?;
        entries.push((name.to_string(), blob));
    }
    let entry_refs: Vec<(&str, &str)> = entries
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_str()))
        .collect();
    let tree = build_tree(backend, scope, &entry_refs)?;
    let commit = run_git_in_bare(
        backend,
        &["commit-tree", &tree, "-m", &format!("init object {uuid}")],
        &scope.repo_dir,
        &scope.repo_dir,
    )?;
    run_git_in_bare(
        backend,
        &["update-ref", &ref_name, &commit],
        &scope.repo_dir,
        &scope.repo_dir,
    )?;
    Ok(commit)
}

/// Update the `object/<uuid>` branch with new files.
/// Each entry in `files` is `(filename, content)`.
pub fn update_object_branch(
    backend: &dyn Backend,
    scope: &TaskScope,
    uuid: &str,
    files: &[(&str, &str)],
) -> Result<String> {
    let ref_name = format!("refs/heads/object/{uuid}");
    let parent = run_git_in_bare(
        backend,
        &["rev-parse", &ref_name],
        &scope.repo_dir,
        &scope.repo_dir,
    )?;
    let mut entries = Vec::new();
    for (name, content) in files {
        let blob = hash_object(backend, scope, content)?;
        entries.push((name.to_string(), blob));
    }
    let entry_refs: Vec<(&str, &str)> = entries
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_str()))
        .collect();
    let tree = build_tree(backend, scope, &entry_refs)?;
    let commit = run_git_in_bare(
        backend,
        &[
            "commit-tree",
            &tree,
            "-p",
            &parent,
            "-m",
            &format!("update object {uuid}"),
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
    Ok(commit)
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

/// Build a tree with one or more files in the subcontext bare repo using a
/// temporary index file. Each entry is `(filename, blob_sha)`.
fn build_tree(
    backend: &dyn Backend,
    scope: &TaskScope,
    entries: &[(&str, &str)],
) -> Result<String> {
    let idx = scratch_path(&scope.scratch_base, "index");
    if let Some(parent) = idx.parent() {
        backend.create_dir_all(parent)?;
    }
    if backend.exists(&idx) {
        backend.remove_file(&idx).ok();
    }

    let git_dir_flag = format!("--git-dir={}", scope.repo_dir.display());
    let idx_os: &OsStr = idx.as_os_str();

    for (name, blob) in entries {
        let cacheinfo = format!("100644,{blob},{name}");
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
    }

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

// ─── TASK.md Parsing & Generation ──────────────────────────────────

/// Parse YAML frontmatter from a markdown string.
/// Returns `(key-value pairs, body after frontmatter)`.
/// If no valid frontmatter is found, returns empty pairs and full content.
pub fn parse_frontmatter(content: &str) -> (Vec<(String, String)>, String) {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() || lines[0].trim() != "---" {
        return (vec![], content.to_string());
    }

    let mut end_line = None;
    for (i, line) in lines.iter().enumerate().skip(1) {
        if line.trim() == "---" {
            end_line = Some(i);
            break;
        }
    }

    let end_line = match end_line {
        Some(i) => i,
        None => return (vec![], content.to_string()),
    };

    let yaml_lines = &lines[1..end_line];
    let mut pairs = vec![];
    for line in yaml_lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            let value = strip_yaml_quotes(&value);
            pairs.push((key, value));
        }
    }

    // Body is everything after the closing ---.
    let body_lines = &lines[end_line + 1..];
    let body = body_lines.join("\n");
    // Preserve trailing newline if original content had one.
    let body = if content.ends_with('\n') && !body.is_empty() && !body.ends_with('\n') {
        body + "\n"
    } else {
        body
    };

    (pairs, body)
}

fn strip_yaml_quotes(s: &str) -> String {
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        return s[1..s.len() - 1].to_string();
    }
    s.to_string()
}

/// Generate a TASK.md string from task data and an optional body.
pub fn generate_task_md(data: &TaskData, body: &str) -> String {
    let mut lines = vec!["---".to_string()];
    lines.push(format!("name: {}", data.name));
    lines.push(format!("kind: {}", data.kind));
    lines.push(format!("status: {}", data.status));
    if let Some(desc) = &data.description {
        lines.push(format!("description: {}", desc));
    }
    if let Some(deadline) = &data.deadline {
        lines.push(format!("deadline: {}", deadline));
    }
    if data.importance != 0.0 {
        if data.importance == data.importance.floor() {
            lines.push(format!("importance: {}", data.importance as i64));
        } else {
            lines.push(format!("importance: {}", data.importance));
        }
    }
    if let Some(ts) = &data.completed_at {
        lines.push(format!("completed_at: {}", ts));
    }
    lines.push("---".to_string());

    let mut result = lines.join("\n");
    if !body.is_empty() {
        result.push('\n');
        result.push_str(body);
    }
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Generate TASK.md from a parsed `object.json` Value and an optional body.
fn generate_task_md_from_json(val: &serde_json::Value, body: &str) -> String {
    let data = &val["data"];
    let mut lines = vec!["---".to_string()];
    if let Some(name) = data["name"].as_str() {
        lines.push(format!("name: {}", name));
    }
    if let Some(kind) = data["kind"].as_str() {
        lines.push(format!("kind: {}", kind));
    }
    if let Some(status) = data["status"].as_str() {
        lines.push(format!("status: {}", status));
    }
    if let Some(desc) = data["description"].as_str() {
        lines.push(format!("description: {}", desc));
    }
    if let Some(deadline) = data["deadline"].as_str() {
        lines.push(format!("deadline: {}", deadline));
    }
    let importance = data["importance"].as_f64().unwrap_or(0.0);
    if importance != 0.0 {
        if importance == importance.floor() {
            lines.push(format!("importance: {}", importance as i64));
        } else {
            lines.push(format!("importance: {}", importance));
        }
    }
    if let Some(completed_at) = data["completed_at"].as_str() {
        lines.push(format!("completed_at: {}", completed_at));
    }
    lines.push("---".to_string());

    let mut result = lines.join("\n");
    if !body.is_empty() {
        result.push('\n');
        result.push_str(body);
    }
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Read a file from an object branch. Returns `None` if the file doesn't exist.
pub fn read_object_file(
    backend: &dyn Backend,
    scope: &TaskScope,
    uuid: &str,
    filename: &str,
) -> Result<Option<String>> {
    let obj_branch = format!("object/{uuid}");
    match run_git_in_bare(
        backend,
        &["show", &format!("{obj_branch}:{filename}")],
        &scope.repo_dir,
        &scope.repo_dir,
    ) {
        Ok(content) => Ok(Some(content)),
        Err(_) => Ok(None),
    }
}

// ─── Task from TASK.md ─────────────────────────────────────────────

/// Create a task from TASK.md content. The full markdown is stored in the
/// object branch alongside the generated object.json. Returns `(uuid, commit)`.
pub fn add_task_from_md(
    backend: &dyn Backend,
    scope: &TaskScope,
    md_content: &str,
    source: Option<(&str, &str, &str)>,
) -> Result<(String, String)> {
    let (pairs, body) = parse_frontmatter(md_content);
    let fm = FrontmatterMap(&pairs);

    let name = fm
        .get("name")
        .ok_or_else(|| anyhow::anyhow!("TASK.md frontmatter must contain 'name'"))?;
    let kind = fm.get("kind");
    let status = fm.get("status");
    let description = fm.get("description");
    let deadline = fm.get("deadline");
    let importance: f64 = fm
        .get("importance")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    if let Some(d) = &deadline {
        if !d.ends_with('Z') {
            bail!("TASK.md deadline must be an ISO8601 UTC timestamp ending with 'Z' (got: {d})");
        }
    }

    let branch: String = match source {
        Some((ctx, _, _)) => ctx.to_string(),
        None => scope.host_branch.clone(),
    };
    let task_uuid = Uuid::new_v4().to_string();
    let kind_str = kind.as_deref().unwrap_or(DEFAULT_KIND);
    let status_str = status.as_deref().unwrap_or(DEFAULT_STATUS);

    let conn = open_db(scope)?;
    let existing: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT task_uuid FROM task_names WHERE branch_name = ?1 AND task_name = ?2")
            .unwrap();
        stmt.query_map(params![branch, name], |r| r.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    };
    if !existing.is_empty() {
        println!(
            "WARNING: task name '{}' is not unique on branch '{}'. Existing UUIDs: {}",
            name,
            branch,
            existing.join(", ")
        );
    }
    conn.execute(
        "INSERT INTO tasks (task_uuid, task_name, task_status, task_kind, task_description, project_uuid, task_deadline, task_importance) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![task_uuid, name, status_str, kind_str, description, scope.project_uuid, deadline, importance],
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

    let task_data = TaskData {
        name: name.to_string(),
        uuid: task_uuid.clone(),
        status: status_str.to_string(),
        kind: kind_str.to_string(),
        project_uuid: scope.project_uuid.clone(),
        completed_at: None,
        description: description.map(|s| s.to_string()),
        deadline: deadline.map(|s| s.to_string()),
        importance,
    };
    let object_json = build_task_object_json(&task_data, source);
    // Store the original TASK.md as-is; regenerate with the UUID embedded in frontmatter.
    let task_md = {
        let mut regenerated_lines = vec!["---".to_string()];
        regenerated_lines.push(format!("uuid: {}", task_uuid));
        for (k, v) in &pairs {
            regenerated_lines.push(format!("{}: {}", k, v));
        }
        regenerated_lines.push("---".to_string());
        let mut out = regenerated_lines.join("\n");
        if !body.is_empty() {
            out.push('\n');
            out.push_str(&body);
        }
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out
    };
    let commit = create_object_branch(
        backend,
        scope,
        &task_uuid,
        &[("object.json", &object_json), ("TASK.md", &task_md)],
    )?;

    let conn = open_db(scope)?;
    insert_object(&conn, &task_uuid, "task", &commit, source)?;
    drop(conn);
    commit_state_in(
        backend,
        &scope.state_dir,
        &format!("object add: {task_uuid}"),
    )?;

    if source.is_some() {
        eprintln!("[subcontext] Added shadow task '{name}' ({task_uuid})");
    } else {
        eprintln!("[subcontext] Added task '{name}' ({task_uuid})");
    }
    println!("{task_uuid}");
    Ok((task_uuid, commit))
}

/// Helper for looking up frontmatter values by key.
struct FrontmatterMap<'a>(&'a [(String, String)]);

impl<'a> FrontmatterMap<'a> {
    fn get(&self, key: &str) -> Option<String> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }
}

// ─── Task Update ───────────────────────────────────────────────────

/// Update an existing task by UUID. If `md_content` is provided, parse it and
/// update both object.json and TASK.md. Otherwise, update individual fields.
/// Returns the new commit SHA.
pub fn update_task(
    backend: &dyn Backend,
    scope: &TaskScope,
    uuid: &str,
    md_content: Option<&str>,
    name: Option<&str>,
    kind: Option<&str>,
    status: Option<&str>,
    description: Option<&str>,
    deadline: Option<&str>,
    importance: Option<f64>,
) -> Result<String> {
    // Read current object.json.
    let obj_branch = format!("object/{uuid}");
    let existing_json = run_git_in_bare(
        backend,
        &["show", &format!("{obj_branch}:object.json")],
        &scope.repo_dir,
        &scope.repo_dir,
    )
    .with_context(|| format!("object branch {obj_branch} not found"))?;
    let mut val: serde_json::Value = serde_json::from_str(&existing_json)?;

    let new_md;

    if let Some(md) = md_content {
        // TASK.md is source of truth — update object.json from it.
        let (pairs, _body) = parse_frontmatter(md);
        let fm = FrontmatterMap(&pairs);

        if let Some(n) = fm.get("name") {
            val["data"]["name"] = serde_json::Value::String(n);
        }
        if let Some(k) = fm.get("kind") {
            val["data"]["kind"] = serde_json::Value::String(k);
        }
        if let Some(s) = fm.get("status") {
            val["data"]["status"] = serde_json::Value::String(s);
        }
        if let Some(d) = fm.get("description") {
            val["data"]["description"] = serde_json::Value::String(d);
        } else {
            val["data"]["description"] = serde_json::Value::Null;
        }
        if let Some(d) = fm.get("deadline") {
            val["data"]["deadline"] = serde_json::Value::String(d);
        }
        if let Some(imp) = fm.get("importance") {
            if let Ok(v) = imp.parse::<f64>() {
                val["data"]["importance"] = serde_json::json!(v);
            }
        }
        // Store TASK.md as-is.
        new_md = md.to_string();
    } else {
        // Update from individual fields.
        if let Some(n) = name {
            val["data"]["name"] = serde_json::Value::String(n.to_string());
        }
        if let Some(k) = kind {
            val["data"]["kind"] = serde_json::Value::String(k.to_string());
        }
        if let Some(s) = status {
            val["data"]["status"] = serde_json::Value::String(s.to_string());
        }
        if let Some(d) = description {
            val["data"]["description"] = serde_json::Value::String(d.to_string());
        }
        if let Some(d) = deadline {
            val["data"]["deadline"] = serde_json::Value::String(d.to_string());
        }
        if let Some(imp) = importance {
            val["data"]["importance"] = serde_json::json!(imp);
        }
        // Regenerate TASK.md from updated object.json.
        let old_body = read_object_file(backend, scope, uuid, "TASK.md")
            .ok()
            .flatten()
            .map(|md| parse_frontmatter(&md).1)
            .unwrap_or_default();
        new_md = generate_task_md_from_json(&val, &old_body);
    }

    let updated_json = serde_json::to_string_pretty(&val)? + "\n";

    // Update DB.
    let task_name = val["data"]["name"].as_str().unwrap_or("").to_string();
    let task_status = val["data"]["status"]
        .as_str()
        .unwrap_or("created")
        .to_string();
    let task_kind = val["data"]["kind"]
        .as_str()
        .unwrap_or(DEFAULT_KIND)
        .to_string();
    let task_desc = val["data"]["description"].as_str().map(|s| s.to_string());
    let task_deadline = val["data"]["deadline"].as_str().map(|s| s.to_string());
    let task_imp = val["data"]["importance"].as_f64().unwrap_or(0.0);

    let conn = open_db(scope)?;
    conn.execute(
        "UPDATE tasks SET task_name=?1, task_status=?2, task_kind=?3, \
         task_description=?4, task_deadline=?5, task_importance=?6 \
         WHERE task_uuid=?7",
        params![
            task_name,
            task_status,
            task_kind,
            task_desc,
            task_deadline,
            task_imp,
            uuid
        ],
    )?;
    // Update task_names entry.
    conn.execute(
        "UPDATE task_names SET task_name=?1 WHERE task_uuid=?2",
        params![task_name, uuid],
    )?;
    drop(conn);
    commit_state_in(backend, &scope.state_dir, &format!("task update: {uuid}"))?;

    let new_commit = update_object_branch(
        backend,
        scope,
        uuid,
        &[("object.json", &updated_json), ("TASK.md", &new_md)],
    )?;

    let conn = open_db(scope)?;
    conn.execute(
        "UPDATE objects SET current_commit = ?1 WHERE uuid = ?2",
        params![new_commit, uuid],
    )?;
    drop(conn);
    commit_state_in(backend, &scope.state_dir, &format!("object update: {uuid}"))?;

    eprintln!("[subcontext] Updated task '{task_name}' ({uuid})");
    Ok(new_commit)
}

// ─── Task Show ─────────────────────────────────────────────────────

/// Result of looking up a task for display.
pub enum ShowTaskResult {
    /// Single match — TASK.md content.
    Single(String, String), // (uuid, task_md_content)
    /// Multiple tasks match the given name.
    Ambiguous(Vec<TaskMatch>),
}

/// A task that matched a name lookup.
#[allow(dead_code)]
pub struct TaskMatch {
    pub uuid: String,
    pub name: String,
    pub description: Option<String>,
    pub branch: String,
}

/// Look up a task by name or UUID and return its TASK.md.
/// If a name matches multiple tasks, return all matches.
pub fn show_task(
    backend: &dyn Backend,
    scope: &TaskScope,
    name_or_uuid: &str,
) -> Result<ShowTaskResult> {
    // Try UUID first.
    if let Ok(Some(md)) = read_object_file(backend, scope, name_or_uuid, "TASK.md") {
        return Ok(ShowTaskResult::Single(name_or_uuid.to_string(), md));
    }

    // Look up by name across all branches.
    let conn = open_db(scope)?;
    let mut stmt = conn.prepare(
        "SELECT n.task_uuid, n.task_name, t.task_description, n.branch_name \
         FROM task_names n JOIN tasks t ON n.task_uuid = t.task_uuid \
         WHERE n.task_name = ?1",
    )?;
    let matches: Vec<TaskMatch> = stmt
        .query_map(params![name_or_uuid], |r| {
            Ok(TaskMatch {
                uuid: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                branch: r.get(3)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

    match matches.len() {
        0 => bail!("no task found matching '{name_or_uuid}'"),
        1 => {
            let m = &matches[0];
            let md = read_object_file(backend, scope, &m.uuid, "TASK.md")?;
            let content = match md {
                Some(c) => c,
                None => {
                    // Generate from object.json.
                    let json_str = run_git_in_bare(
                        backend,
                        &["show", &format!("object/{}:object.json", m.uuid)],
                        &scope.repo_dir,
                        &scope.repo_dir,
                    )?;
                    let val: serde_json::Value = serde_json::from_str(&json_str)?;
                    generate_task_md_from_json(&val, "")
                }
            };
            Ok(ShowTaskResult::Single(m.uuid.clone(), content))
        }
        _ => Ok(ShowTaskResult::Ambiguous(matches)),
    }
}

// ─── Task Lookup by UUID or Name ───────────────────────────────────

/// Resolve a name-or-UUID to a single task UUID. If the input looks like a
/// UUID (contains dashes) and a matching object branch exists, use that.
/// Otherwise look up by name. If the name matches multiple tasks on the
/// current branch, prefer the current-branch match. If still ambiguous, bail
/// with a list.
pub fn resolve_task_uuid(
    backend: &dyn Backend,
    scope: &TaskScope,
    name_or_uuid: &str,
) -> Result<String> {
    // UUID path.
    if name_or_uuid.contains('-') {
        let ref_name = format!("refs/heads/object/{name_or_uuid}");
        if run_git_in_bare(
            backend,
            &["show-ref", "--verify", "--quiet", &ref_name],
            &scope.repo_dir,
            &scope.repo_dir,
        )
        .is_ok()
        {
            return Ok(name_or_uuid.to_string());
        }
    }

    // Name lookup.
    let conn = open_db(scope)?;
    let mut stmt =
        conn.prepare("SELECT n.task_uuid, n.branch_name FROM task_names n WHERE n.task_name = ?1")?;
    let matches: Vec<(String, String)> = stmt
        .query_map(params![name_or_uuid], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    match matches.len() {
        0 => bail!("no task found matching '{name_or_uuid}'"),
        1 => Ok(matches[0].0.clone()),
        _ => {
            // Prefer current branch.
            for (uuid, branch) in &matches {
                if branch == &scope.host_branch {
                    return Ok(uuid.clone());
                }
            }
            let list: Vec<String> = matches
                .iter()
                .map(|(u, b)| format!("  {} (branch: {})", u, b))
                .collect();
            bail!(
                "task name '{}' is ambiguous — matches {} tasks:\n{}",
                name_or_uuid,
                matches.len(),
                list.join("\n")
            );
        }
    }
}

// ─── Object Commit (sync TASK.md ↔ object.json) ───────────────────

/// Synchronize TASK.md and object.json on an object branch.
///
/// - If only object.json exists → generate TASK.md from it.
/// - If only TASK.md exists → generate object.json from it.
/// - If both exist → check if the frontmatter matches object.json data;
///   if not, bail (user must resolve the conflict).
/// - If neither exists → bail.
///
/// Returns the new commit SHA, or `None` if no changes were needed.
pub fn object_commit(
    backend: &dyn Backend,
    scope: &TaskScope,
    uuid: &str,
) -> Result<Option<String>> {
    let json_opt = read_object_file(backend, scope, uuid, "object.json")?;
    let md_opt = read_object_file(backend, scope, uuid, "TASK.md")?;

    match (json_opt, md_opt) {
        (None, None) => bail!("object/{uuid} has neither object.json nor TASK.md"),

        (Some(json_str), None) => {
            // Generate TASK.md from object.json.
            let val: serde_json::Value = serde_json::from_str(&json_str)?;
            let md = generate_task_md_from_json(&val, "");
            let commit = update_object_branch(
                backend,
                scope,
                uuid,
                &[("object.json", &json_str), ("TASK.md", &md)],
            )?;
            update_object_commit(backend, scope, uuid, &commit)?;
            eprintln!("[subcontext] Generated TASK.md for object {uuid}");
            Ok(Some(commit))
        }

        (None, Some(md_str)) => {
            // Generate object.json from TASK.md.
            let (pairs, _body) = parse_frontmatter(&md_str);
            let fm = FrontmatterMap(&pairs);
            let name = fm.get("name").unwrap_or_else(|| "unnamed".to_string());
            let kind = fm.get("kind").unwrap_or_else(|| DEFAULT_KIND.to_string());
            let status = fm
                .get("status")
                .unwrap_or_else(|| DEFAULT_STATUS.to_string());
            let task_data = TaskData {
                name,
                uuid: uuid.to_string(),
                status,
                kind,
                project_uuid: scope.project_uuid.clone(),
                completed_at: fm.get("completed_at"),
                description: fm.get("description"),
                deadline: fm.get("deadline"),
                importance: fm
                    .get("importance")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0),
            };
            let json = build_task_object_json(&task_data, None);
            let commit = update_object_branch(
                backend,
                scope,
                uuid,
                &[("object.json", &json), ("TASK.md", &md_str)],
            )?;
            update_object_commit(backend, scope, uuid, &commit)?;
            eprintln!("[subcontext] Generated object.json for object {uuid}");
            Ok(Some(commit))
        }

        (Some(json_str), Some(md_str)) => {
            // Both exist — verify frontmatter matches object.json.
            let val: serde_json::Value = serde_json::from_str(&json_str)?;
            let (pairs, _body) = parse_frontmatter(&md_str);
            let fm = FrontmatterMap(&pairs);

            let mut mismatches = Vec::new();
            check_field(&val, &fm, "name", &mut mismatches);
            check_field(&val, &fm, "kind", &mut mismatches);
            check_field(&val, &fm, "status", &mut mismatches);

            if !mismatches.is_empty() {
                bail!(
                    "TASK.md and object.json are out of sync for object {uuid}:\n{}",
                    mismatches.join("\n")
                );
            }
            eprintln!("[subcontext] object.json and TASK.md are in sync for {uuid}");
            Ok(None)
        }
    }
}

fn check_field(
    val: &serde_json::Value,
    fm: &FrontmatterMap<'_>,
    field: &str,
    mismatches: &mut Vec<String>,
) {
    let json_val = val["data"][field].as_str().unwrap_or("");
    if let Some(md_val) = fm.get(field) {
        if md_val != json_val {
            mismatches.push(format!(
                "  {}: object.json='{}' vs TASK.md='{}'",
                field, json_val, md_val
            ));
        }
    }
}

fn update_object_commit(
    backend: &dyn Backend,
    scope: &TaskScope,
    uuid: &str,
    commit: &str,
) -> Result<()> {
    let conn = open_db(scope)?;
    conn.execute(
        "UPDATE objects SET current_commit = ?1 WHERE uuid = ?2",
        params![commit, uuid],
    )?;
    drop(conn);
    commit_state_in(backend, &scope.state_dir, &format!("object commit: {uuid}"))
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

    #[test]
    fn parse_iso8601_roundtrip() {
        assert_eq!(parse_iso8601_to_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso8601_to_unix("2000-01-01T00:00:00Z"),
            Some(946_684_800)
        );
        assert_eq!(
            parse_iso8601_to_unix("2026-04-12T12:34:56Z"),
            Some(1_775_997_296)
        );
    }

    #[test]
    fn parse_iso8601_rejects_bad_input() {
        assert_eq!(parse_iso8601_to_unix("not a date"), None);
        assert_eq!(parse_iso8601_to_unix("2026-04-12T12:34:56"), None); // no Z
    }

    #[test]
    fn parse_duration_units() {
        assert_eq!(parse_duration("0").unwrap(), 0.0);
        assert_eq!(parse_duration("30s").unwrap(), 30.0);
        assert_eq!(parse_duration("5m").unwrap(), 300.0);
        assert_eq!(parse_duration("2h").unwrap(), 7200.0);
        assert_eq!(parse_duration("1d").unwrap(), 86400.0);
        assert_eq!(parse_duration("1w").unwrap(), 7.0 * 86400.0);
        assert_eq!(parse_duration("1mo").unwrap(), 30.0 * 86400.0);
        assert_eq!(parse_duration("1y").unwrap(), 365.0 * 86400.0);
    }

    #[test]
    fn parse_duration_fractional() {
        assert_eq!(parse_duration("0.5d").unwrap(), 43200.0);
    }

    #[test]
    fn parse_duration_rejects_empty() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
    }

    #[test]
    fn days_from_civil_roundtrip() {
        // Verify days_from_civil is inverse of civil_from_days
        for secs in [0i64, 946_684_800, 1_775_997_296, -86400] {
            let days = secs.div_euclid(86400);
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn parse_frontmatter_basic() {
        let input = "---\nname: my-task\nkind: todo\nstatus: created\n---\n# Body\nHello\n";
        let (pairs, body) = parse_frontmatter(input);
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0], ("name".to_string(), "my-task".to_string()));
        assert_eq!(pairs[1], ("kind".to_string(), "todo".to_string()));
        assert_eq!(pairs[2], ("status".to_string(), "created".to_string()));
        assert!(body.starts_with("# Body"));
        assert!(body.contains("Hello"));
    }

    #[test]
    fn parse_frontmatter_no_frontmatter() {
        let input = "Just regular markdown\n";
        let (pairs, body) = parse_frontmatter(input);
        assert!(pairs.is_empty());
        assert_eq!(body, input);
    }

    #[test]
    fn parse_frontmatter_quoted_values() {
        let input = "---\nname: \"quoted value\"\nkind: 'single'\n---\n";
        let (pairs, _body) = parse_frontmatter(input);
        assert_eq!(pairs[0].1, "quoted value");
        assert_eq!(pairs[1].1, "single");
    }

    #[test]
    fn parse_frontmatter_empty_body() {
        let input = "---\nname: test\n---\n";
        let (pairs, body) = parse_frontmatter(input);
        assert_eq!(pairs.len(), 1);
        assert!(body.is_empty() || body.trim().is_empty());
    }

    #[test]
    fn generate_task_md_roundtrip() {
        let data = TaskData {
            name: "my-task".to_string(),
            uuid: "abc-123".to_string(),
            status: "created".to_string(),
            kind: "todo".to_string(),
            project_uuid: "proj-1".to_string(),
            completed_at: None,
            description: Some("A test task".to_string()),
            deadline: Some("2026-04-10T00:00:00Z".to_string()),
            importance: 1.5,
        };
        let md = generate_task_md(&data, "# Body\nContent here\n");
        let (pairs, body) = parse_frontmatter(&md);
        let fm = FrontmatterMap(&pairs);
        assert_eq!(fm.get("name").unwrap(), "my-task");
        assert_eq!(fm.get("kind").unwrap(), "todo");
        assert_eq!(fm.get("status").unwrap(), "created");
        assert_eq!(fm.get("description").unwrap(), "A test task");
        assert_eq!(fm.get("deadline").unwrap(), "2026-04-10T00:00:00Z");
        assert_eq!(fm.get("importance").unwrap(), "1.5");
        assert!(body.contains("# Body"));
        assert!(body.contains("Content here"));
    }
}
