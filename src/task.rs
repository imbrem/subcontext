use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::backend::{Backend, GitInvocation};
use crate::git::{pool_dir, repo_dir, run_git_in_bare, run_work_git, state_dir, subcontext_dir};
use crate::project::read_project_uuid;

pub const DB_NAME: &str = "tasks.db";
pub const INDEX_DB_NAME: &str = "index.db";

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
    /// UUID that owns these tasks (project UUID for local, user UUID for global).
    pub project_uuid: String,
    /// Pool worktree directory (if initialized).
    pub pool_dir: Option<PathBuf>,
}

impl TaskScope {
    /// Build a scope for a local (per-host-repo) subcontext install.
    pub fn for_local(backend: &dyn Backend, root: &Path) -> anyhow::Result<Self> {
        let pd = pool_dir(root);
        let pool = if backend.exists(&pd) { Some(pd) } else { None };
        Ok(Self {
            repo_dir: repo_dir(root),
            state_dir: state_dir(root),
            scratch_base: subcontext_dir(root),
            project_uuid: read_project_uuid(backend, root)?,
            pool_dir: pool,
        })
    }
}

// ─── State branch (state.db) ───────────────────────────────────────

/// Initialize the `state` branch, its worktree, and the state.db schema
/// against the local (per-host-repo) subcontext layout.
pub fn init_state_branch(backend: &dyn Backend, root: &Path) -> Result<()> {
    init_state_branch_in(backend, &repo_dir(root), &state_dir(root))
}

/// Initialize the `state` branch + worktree + state.db schema against an
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

    // Write SCHEMA.md
    backend.write(&state.join("SCHEMA.md"), STATE_SCHEMA_MD.as_bytes())?;

    // Create DB + schema
    let conn = Connection::open(state.join(DB_NAME))?;
    create_state_schema(&conn)?;
    drop(conn);

    // Commit
    commit_state_in(backend, state, "init state db")?;
    Ok(())
}

fn create_state_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS objects (
             uuid       TEXT PRIMARY KEY,
             owner_uuid TEXT NOT NULL,
             owner_type TEXT NOT NULL CHECK (owner_type IN ('pool', 'child'))
         );
         CREATE TABLE IF NOT EXISTS pools (
             uuid           TEXT PRIMARY KEY,
             current_commit TEXT NOT NULL
         );",
    )?;
    Ok(())
}

pub fn open_state_db(state_dir: &Path) -> Result<Connection> {
    Ok(Connection::open(state_dir.join(DB_NAME))?)
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

// ─── Pool (index.db) ────────────────────────────────────────────────

/// Open a connection to a pool's index.db. Returns None if the pool dir
/// doesn't exist or has no index.db.
pub fn open_pool_db(pool_dir: &Path) -> Option<Connection> {
    let path = pool_dir.join(INDEX_DB_NAME);
    Connection::open(path).ok()
}

fn create_pool_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (
             key   TEXT PRIMARY KEY,
             value TEXT
         );
         INSERT OR IGNORE INTO meta (key, value) VALUES ('next_id', '1');
         CREATE TABLE IF NOT EXISTS tasks (
             id        INTEGER PRIMARY KEY,
             uuid      TEXT UNIQUE,
             list      TEXT,
             topic     TEXT,
             type      TEXT NOT NULL DEFAULT 'todo',
             status    TEXT NOT NULL DEFAULT 'active',
             important INTEGER NOT NULL DEFAULT 0,
             deadline  TEXT,
             created   TEXT,
             done      TEXT,
             cancelled TEXT,
             parents   TEXT NOT NULL DEFAULT '[]',
             subtasks  TEXT NOT NULL DEFAULT '[]'
         );
         CREATE VIEW IF NOT EXISTS open AS
             SELECT list, id FROM tasks
             WHERE done IS NULL AND cancelled IS NULL AND list IS NOT NULL;",
    )?;
    Ok(())
}

const POOL_SCHEMA_MD: &str = include_str!("../docs/branches/pool/SCHEMA.md");
const STATE_SCHEMA_MD: &str = include_str!("../docs/branches/state/SCHEMA.md");

/// Create an empty pool: branch, worktree, and state.db registration.
/// Returns the pool UUID. The pool has no schema or files yet — call
/// `init_task_pool` to set it up as a task pool.
pub fn create_pool(
    backend: &dyn Backend,
    bare_repo: &Path,
    pool_path: &Path,
    state_path: &Path,
) -> Result<String> {
    let pool_uuid = Uuid::new_v4().to_string();

    // Create pool branch via plumbing.
    let empty_tree = run_git_in_bare(
        backend,
        &["hash-object", "-t", "tree", "/dev/null"],
        bare_repo,
        bare_repo,
    )?;
    let branch_name = format!("object/{pool_uuid}");
    let commit = run_git_in_bare(
        backend,
        &[
            "commit-tree",
            &empty_tree,
            "-m",
            &format!("init pool {pool_uuid}"),
        ],
        bare_repo,
        bare_repo,
    )?;
    run_git_in_bare(
        backend,
        &["update-ref", &format!("refs/heads/{branch_name}"), &commit],
        bare_repo,
        bare_repo,
    )?;

    // Add worktree
    backend.worktree_add(bare_repo, pool_path, &branch_name)?;

    // Register in state.db
    let state_conn = open_state_db(state_path)?;
    // Use the empty-tree commit as initial current_commit
    state_conn.execute(
        "INSERT INTO pools (uuid, current_commit) VALUES (?1, ?2)",
        params![pool_uuid, commit.trim()],
    )?;
    drop(state_conn);
    commit_state_in(backend, state_path, &format!("pool add: {pool_uuid}"))?;

    Ok(pool_uuid)
}

/// Initialize a pool worktree as a task pool: write SCHEMA.md, create
/// `tasks/` directory, create `index.db` with the task schema, and commit.
pub fn init_task_pool(backend: &dyn Backend, pool_path: &Path) -> Result<()> {
    // Create tasks/ directory
    let tasks_dir = pool_path.join("tasks");
    backend.create_dir_all(&tasks_dir)?;
    // Write .gitkeep so git tracks the empty dir
    backend.write(&tasks_dir.join(".gitkeep"), b"")?;

    // Write SCHEMA.md
    backend.write(&pool_path.join("SCHEMA.md"), POOL_SCHEMA_MD.as_bytes())?;

    // Create index.db with schema
    let conn = Connection::open(pool_path.join(INDEX_DB_NAME))?;
    create_pool_schema(&conn)?;
    drop(conn);

    // Commit pool worktree
    run_work_git(backend, &["add", "-A"], pool_path)?;
    run_work_git(backend, &["commit", "-m", "init task pool"], pool_path)?;

    Ok(())
}

/// Initialize a pool: create pool branch, worktree, SCHEMA.md, index.db,
/// and register it in state.db. Returns the pool UUID.
pub fn init_pool(
    backend: &dyn Backend,
    bare_repo: &Path,
    pool_path: &Path,
    state_path: &Path,
) -> Result<String> {
    let pool_uuid = create_pool(backend, bare_repo, pool_path, state_path)?;
    init_task_pool(backend, pool_path)?;

    // Update state.db with the real commit now that we have content
    let state_conn = open_state_db(state_path)?;
    let pool_commit = run_work_git(backend, &["rev-parse", "HEAD"], pool_path)?;
    state_conn.execute(
        "UPDATE pools SET current_commit = ?1 WHERE uuid = ?2",
        params![pool_commit.trim(), pool_uuid],
    )?;
    drop(state_conn);
    commit_state_in(backend, state_path, &format!("pool init: {pool_uuid}"))?;

    eprintln!("[subcontext] Initialized pool ({pool_uuid})");
    Ok(pool_uuid)
}

/// Initialize a pool for a local subcontext install.
pub fn init_pool_local(backend: &dyn Backend, root: &Path) -> Result<String> {
    init_pool(backend, &repo_dir(root), &pool_dir(root), &state_dir(root))
}

// ─── Pool task operations ───────────────────────────────────────────

/// A pool task parsed from TASK.md frontmatter.
#[derive(Debug, Clone)]
pub struct PoolTask {
    pub id: i64,
    pub uuid: Option<String>,
    pub title: String,
    pub list: Option<String>,
    pub topic: Option<String>,
    pub task_type: String,
    pub status: String,
    pub important: bool,
    pub deadline: Option<String>,
    pub parents: Vec<i64>,
    pub subtasks: Vec<i64>,
    pub created: Option<String>,
    pub done: Option<String>,
    pub cancelled: Option<String>,
    pub body: String,
}

/// Parse a pool TASK.md from its content.
pub fn parse_pool_task_md(content: &str) -> Result<PoolTask> {
    let (pairs, body) = parse_frontmatter(content);
    let fm = FrontmatterMap(&pairs);

    let id: i64 = fm.get("id").and_then(|s| s.parse().ok()).unwrap_or(0);
    let uuid = fm.get("uuid");
    let title = body
        .lines()
        .find(|l| l.starts_with("# "))
        .map(|l| l[2..].trim().to_string())
        .unwrap_or_default();
    let list = fm.get("list");
    let topic = fm.get("topic");
    let task_type = fm.get("type").unwrap_or_else(|| "todo".to_string());
    let status = fm.get("status").unwrap_or_else(|| "active".to_string());
    let important = fm
        .get("important")
        .map(|s| s == "true" || s == "1")
        .unwrap_or(false);
    let deadline = fm.get("deadline");
    let parents = parse_int_list(&fm.get("parents").unwrap_or_default());
    let subtasks_list = parse_int_list(&fm.get("subtasks").unwrap_or_default());
    let created = fm.get("created");
    let done = fm.get("done");
    let cancelled = fm.get("cancelled");

    Ok(PoolTask {
        id,
        uuid,
        title,
        list,
        topic,
        task_type,
        status,
        important,
        deadline,
        parents,
        subtasks: subtasks_list,
        created,
        done,
        cancelled,
        body,
    })
}

/// Generate a pool TASK.md from a PoolTask.
pub fn generate_pool_task_md(t: &PoolTask) -> String {
    let mut lines = vec!["---".to_string()];
    lines.push(format!("id: {}", t.id));
    if let Some(ref uuid) = t.uuid {
        lines.push(format!("uuid: {uuid}"));
    }
    if let Some(ref list) = t.list {
        lines.push(format!("list: {list}"));
    }
    if let Some(ref topic) = t.topic {
        lines.push(format!("topic: {topic}"));
    }
    lines.push(format!("type: {}", t.task_type));
    lines.push(format!("status: {}", t.status));
    if t.important {
        lines.push("important: true".to_string());
    }
    if let Some(ref deadline) = t.deadline {
        lines.push(format!("deadline: {deadline}"));
    }
    if !t.parents.is_empty() {
        let p: Vec<String> = t.parents.iter().map(|i| i.to_string()).collect();
        lines.push(format!("parents: [{}]", p.join(", ")));
    }
    if !t.subtasks.is_empty() {
        let s: Vec<String> = t.subtasks.iter().map(|i| i.to_string()).collect();
        lines.push(format!("subtasks: [{}]", s.join(", ")));
    }
    if let Some(ref created) = t.created {
        lines.push(format!("created: {created}"));
    }
    if let Some(ref done) = t.done {
        lines.push(format!("done: {done}"));
    }
    if let Some(ref cancelled) = t.cancelled {
        lines.push(format!("cancelled: {cancelled}"));
    }
    lines.push("---".to_string());

    let mut result = lines.join("\n");
    if !t.body.is_empty() {
        result.push('\n');
        result.push_str(&t.body);
    } else {
        // Add title as heading
        result.push('\n');
        result.push_str(&format!("# {}\n", t.title));
    }
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

fn parse_int_list(s: &str) -> Vec<i64> {
    let s = s.trim().trim_start_matches('[').trim_end_matches(']');
    if s.is_empty() {
        return vec![];
    }
    s.split(',')
        .filter_map(|p| p.trim().parse::<i64>().ok())
        .collect()
}

/// Add a task to the default pool. Returns the task ID.
#[allow(clippy::too_many_arguments)]
pub fn pool_add_task(
    backend: &dyn Backend,
    scope: &TaskScope,
    title: &str,
    list: Option<&str>,
    topic: Option<&str>,
    task_type: Option<&str>,
    status: Option<&str>,
    important: bool,
    deadline: Option<&str>,
    parents: &[i64],
    uuid: Option<&str>,
) -> Result<i64> {
    let pool_path = scope
        .pool_dir
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no pool initialized — run `subcontext install` first"))?;

    let conn =
        open_pool_db(pool_path).ok_or_else(|| anyhow::anyhow!("cannot open pool index.db"))?;

    // Get next_id
    let next_id: i64 = conn
        .query_row("SELECT value FROM meta WHERE key = 'next_id'", [], |r| {
            r.get::<_, String>(0)
        })?
        .parse()
        .unwrap_or(1);

    // Find actual ID: skip existing task directories
    let mut task_id = next_id;
    let tasks_dir = pool_path.join("tasks");
    loop {
        let dir = tasks_dir.join(task_id.to_string());
        if !backend.exists(&dir) {
            break;
        }
        eprintln!("[subcontext] warning: tasks/{task_id}/ already exists, skipping to next ID");
        task_id += 1;
    }

    // Validate parents exist
    for &parent_id in parents {
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM tasks WHERE id = ?1",
                params![parent_id],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !exists {
            bail!("parent task {parent_id} not found");
        }
    }

    let task_type = task_type.unwrap_or("todo");
    let status = status.unwrap_or("active");
    let created = current_iso8601();
    let parents_json = serde_json::to_string(parents)?;

    // Insert into DB
    conn.execute(
        "INSERT INTO tasks (id, uuid, list, topic, type, status, important, deadline, created, parents, subtasks) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, '[]')",
        params![task_id, uuid, list, topic, task_type, status, important as i64, deadline, created, parents_json],
    )?;

    // Update next_id
    conn.execute(
        "UPDATE meta SET value = ?1 WHERE key = 'next_id'",
        params![(task_id + 1).to_string()],
    )?;

    // Update parent subtasks (DB + TASK.md)
    for &parent_id in parents {
        let current_subtasks: String = conn
            .query_row(
                "SELECT subtasks FROM tasks WHERE id = ?1",
                params![parent_id],
                |r| r.get(0),
            )
            .unwrap_or_else(|_| "[]".to_string());
        let mut subs: Vec<i64> = serde_json::from_str(&current_subtasks).unwrap_or_default();
        if !subs.contains(&task_id) {
            subs.push(task_id);
            let new_json = serde_json::to_string(&subs)?;
            conn.execute(
                "UPDATE tasks SET subtasks = ?1 WHERE id = ?2",
                params![new_json, parent_id],
            )?;
            // Regenerate parent TASK.md
            let parent_md_path = tasks_dir.join(parent_id.to_string()).join("TASK.md");
            if backend.exists(&parent_md_path) {
                let content = backend.read_to_string(&parent_md_path)?;
                if let Ok(mut parent_task) = parse_pool_task_md(&content) {
                    parent_task.subtasks = subs.clone();
                    backend.write(
                        &parent_md_path,
                        generate_pool_task_md(&parent_task).as_bytes(),
                    )?;
                }
            }
        }
    }

    drop(conn);

    // Create TASK.md on disk
    let task = PoolTask {
        id: task_id,
        uuid: uuid.map(|s| s.to_string()),
        title: title.to_string(),
        list: list.map(|s| s.to_string()),
        topic: topic.map(|s| s.to_string()),
        task_type: task_type.to_string(),
        status: status.to_string(),
        important,
        deadline: deadline.map(|s| s.to_string()),
        parents: parents.to_vec(),
        subtasks: vec![],
        created: Some(created),
        done: None,
        cancelled: None,
        body: String::new(),
    };

    let task_dir = tasks_dir.join(task_id.to_string());
    backend.create_dir_all(&task_dir)?;
    backend.write(
        &task_dir.join("TASK.md"),
        generate_pool_task_md(&task).as_bytes(),
    )?;

    // Commit pool worktree
    run_work_git(backend, &["add", "-A"], pool_path)?;
    run_work_git(
        backend,
        &["commit", "-m", &format!("task add: {title}")],
        pool_path,
    )?;

    // Register UUID in state.db if provided
    if let Some(task_uuid) = uuid {
        let state_conn = open_state_db(&scope.state_dir)?;
        // Check for duplicate UUID
        let existing: Option<String> = state_conn
            .query_row(
                "SELECT uuid FROM objects WHERE uuid = ?1",
                params![task_uuid],
                |r| r.get(0),
            )
            .ok();
        if existing.is_some() {
            bail!("UUID {task_uuid} already registered in state.db");
        }
        // Get pool UUID
        let pool_uuid: String = state_conn
            .query_row("SELECT uuid FROM pools LIMIT 1", [], |r| r.get(0))
            .context("no pool registered in state.db")?;
        state_conn.execute(
            "INSERT INTO objects (uuid, owner_uuid, owner_type) VALUES (?1, ?2, 'pool')",
            params![task_uuid, pool_uuid],
        )?;
        drop(state_conn);
        commit_state_in(
            backend,
            &scope.state_dir,
            &format!("object add: {task_uuid}"),
        )?;
    }

    eprintln!("[subcontext] Added task {task_id}: {title}");
    println!("{task_id}");
    Ok(task_id)
}

/// Mark a task as done in the pool.
pub fn pool_done_task(
    backend: &dyn Backend,
    scope: &TaskScope,
    identifier: &str,
    time: Option<&str>,
) -> Result<()> {
    let (pool_path, task_id) = resolve_pool_task(backend, scope, identifier)?;
    let done_time = resolve_timestamp(time)?;

    let conn =
        open_pool_db(&pool_path).ok_or_else(|| anyhow::anyhow!("cannot open pool index.db"))?;

    // Update DB
    conn.execute(
        "UPDATE tasks SET status = 'done', done = ?1 WHERE id = ?2",
        params![done_time, task_id],
    )?;
    drop(conn);

    // Update TASK.md
    let task_md_path = pool_path
        .join("tasks")
        .join(task_id.to_string())
        .join("TASK.md");
    if backend.exists(&task_md_path) {
        let content = backend.read_to_string(&task_md_path)?;
        let (pairs, body) = parse_frontmatter(&content);
        let mut new_pairs = update_frontmatter_field(&pairs, "status", "done");
        new_pairs = update_frontmatter_field(&new_pairs, "done", &done_time);
        let new_md = rebuild_task_md_from_pairs(&new_pairs, &body);
        backend.write(&task_md_path, new_md.as_bytes())?;
    }

    // Commit
    run_work_git(backend, &["add", "-A"], &pool_path)?;
    run_work_git(
        backend,
        &["commit", "-m", &format!("task done: {task_id}")],
        &pool_path,
    )?;

    eprintln!("[subcontext] Marked task {task_id} as done");
    Ok(())
}

/// Mark a task as failed/cancelled in the pool.
pub fn pool_fail_task(
    backend: &dyn Backend,
    scope: &TaskScope,
    identifier: &str,
    time: Option<&str>,
) -> Result<()> {
    let (pool_path, task_id) = resolve_pool_task(backend, scope, identifier)?;
    let fail_time = resolve_timestamp(time)?;

    let conn =
        open_pool_db(&pool_path).ok_or_else(|| anyhow::anyhow!("cannot open pool index.db"))?;

    // Update DB
    conn.execute(
        "UPDATE tasks SET status = 'cancelled', cancelled = ?1 WHERE id = ?2",
        params![fail_time, task_id],
    )?;
    drop(conn);

    // Update TASK.md
    let task_md_path = pool_path
        .join("tasks")
        .join(task_id.to_string())
        .join("TASK.md");
    if backend.exists(&task_md_path) {
        let content = backend.read_to_string(&task_md_path)?;
        let (pairs, body) = parse_frontmatter(&content);
        let mut new_pairs = update_frontmatter_field(&pairs, "status", "cancelled");
        new_pairs = update_frontmatter_field(&new_pairs, "cancelled", &fail_time);
        let new_md = rebuild_task_md_from_pairs(&new_pairs, &body);
        backend.write(&task_md_path, new_md.as_bytes())?;
    }

    // Commit
    run_work_git(backend, &["add", "-A"], &pool_path)?;
    run_work_git(
        backend,
        &["commit", "-m", &format!("task fail: {task_id}")],
        &pool_path,
    )?;

    eprintln!("[subcontext] Marked task {task_id} as cancelled");
    Ok(())
}

/// Show a task's TASK.md content.
pub fn pool_show_task(
    backend: &dyn Backend,
    scope: &TaskScope,
    identifier: &str,
) -> Result<String> {
    let (pool_path, task_id) = resolve_pool_task(backend, scope, identifier)?;
    let task_md_path = pool_path
        .join("tasks")
        .join(task_id.to_string())
        .join("TASK.md");
    if !backend.exists(&task_md_path) {
        bail!("TASK.md not found for task {task_id}");
    }
    backend
        .read_to_string(&task_md_path)
        .context("failed to read TASK.md")
}

/// Update fields on a pool task.
#[allow(clippy::too_many_arguments)]
pub fn pool_update_task(
    backend: &dyn Backend,
    scope: &TaskScope,
    identifier: &str,
    list: Option<&str>,
    topic: Option<&str>,
    task_type: Option<&str>,
    status: Option<&str>,
    important: Option<bool>,
    deadline: Option<&str>,
    title: Option<&str>,
) -> Result<()> {
    let (pool_path, task_id) = resolve_pool_task(backend, scope, identifier)?;

    let conn =
        open_pool_db(&pool_path).ok_or_else(|| anyhow::anyhow!("cannot open pool index.db"))?;

    // Build dynamic SQL
    let mut updates = Vec::new();
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(v) = list {
        updates.push("list = ?");
        param_values.push(Box::new(v.to_string()));
    }
    if let Some(v) = topic {
        updates.push("topic = ?");
        param_values.push(Box::new(v.to_string()));
    }
    if let Some(v) = task_type {
        updates.push("type = ?");
        param_values.push(Box::new(v.to_string()));
    }
    if let Some(v) = status {
        updates.push("status = ?");
        param_values.push(Box::new(v.to_string()));
    }
    if let Some(v) = important {
        updates.push("important = ?");
        param_values.push(Box::new(v as i64));
    }
    if let Some(v) = deadline {
        updates.push("deadline = ?");
        param_values.push(Box::new(v.to_string()));
    }

    if !updates.is_empty() {
        // Renumber placeholders
        let set_clause: String = updates
            .iter()
            .enumerate()
            .map(|(i, u)| u.replace('?', &format!("?{}", i + 1)))
            .collect::<Vec<_>>()
            .join(", ");
        let id_param = param_values.len() + 1;
        let sql = format!("UPDATE tasks SET {set_clause} WHERE id = ?{id_param}");
        param_values.push(Box::new(task_id));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|b| b.as_ref()).collect();
        conn.execute(&sql, param_refs.as_slice())?;
    }
    drop(conn);

    // Update TASK.md
    let task_md_path = pool_path
        .join("tasks")
        .join(task_id.to_string())
        .join("TASK.md");
    if backend.exists(&task_md_path) {
        let content = backend.read_to_string(&task_md_path)?;
        let (pairs, body) = parse_frontmatter(&content);
        let mut new_pairs = pairs;
        if let Some(v) = list {
            new_pairs = update_frontmatter_field(&new_pairs, "list", v);
        }
        if let Some(v) = topic {
            new_pairs = update_frontmatter_field(&new_pairs, "topic", v);
        }
        if let Some(v) = task_type {
            new_pairs = update_frontmatter_field(&new_pairs, "type", v);
        }
        if let Some(v) = status {
            new_pairs = update_frontmatter_field(&new_pairs, "status", v);
        }
        if let Some(v) = important {
            new_pairs =
                update_frontmatter_field(&new_pairs, "important", if v { "true" } else { "false" });
        }
        if let Some(v) = deadline {
            new_pairs = update_frontmatter_field(&new_pairs, "deadline", v);
        }

        let mut new_body = body;
        if let Some(new_title) = title {
            // Replace the first # heading
            let mut lines: Vec<String> = new_body.lines().map(|l| l.to_string()).collect();
            let mut found = false;
            for line in &mut lines {
                if line.starts_with("# ") {
                    *line = format!("# {new_title}");
                    found = true;
                    break;
                }
            }
            if !found && !lines.is_empty() {
                lines.insert(0, format!("# {new_title}"));
            }
            new_body = lines.join("\n");
            if !new_body.ends_with('\n') {
                new_body.push('\n');
            }
        }

        let new_md = rebuild_task_md_from_pairs(&new_pairs, &new_body);
        backend.write(&task_md_path, new_md.as_bytes())?;
    }

    // Commit
    run_work_git(backend, &["add", "-A"], &pool_path)?;
    let status_str = run_work_git(backend, &["status", "--porcelain"], &pool_path)?;
    if !status_str.is_empty() {
        run_work_git(
            backend,
            &["commit", "-m", &format!("task update: {task_id}")],
            &pool_path,
        )?;
    }

    eprintln!("[subcontext] Updated task {task_id}");
    Ok(())
}

/// Resolve a task identifier to (pool_dir, task_id).
/// Accepts: integer ID, UUID, or "pool-uuid/id".
pub fn resolve_pool_task(
    _backend: &dyn Backend,
    scope: &TaskScope,
    identifier: &str,
) -> Result<(PathBuf, i64)> {
    let pool_path = scope
        .pool_dir
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no pool initialized"))?
        .clone();

    // Try as integer ID first
    if let Ok(id) = identifier.parse::<i64>() {
        let conn =
            open_pool_db(&pool_path).ok_or_else(|| anyhow::anyhow!("cannot open pool index.db"))?;
        let exists: bool = conn
            .query_row("SELECT 1 FROM tasks WHERE id = ?1", params![id], |_| {
                Ok(true)
            })
            .unwrap_or(false);
        if exists {
            return Ok((pool_path, id));
        }
        bail!("task {id} not found in pool");
    }

    // Try as UUID — look up in state.db first, then in pool index.db
    if identifier.contains('-') {
        // Check state.db for UUID → pool mapping
        let state_conn = open_state_db(&scope.state_dir)?;
        let _owner: Option<String> = state_conn
            .query_row(
                "SELECT owner_uuid FROM objects WHERE uuid = ?1 AND owner_type = 'pool'",
                params![identifier],
                |r| r.get(0),
            )
            .ok();

        // Look up ID in pool index.db
        let conn =
            open_pool_db(&pool_path).ok_or_else(|| anyhow::anyhow!("cannot open pool index.db"))?;
        let id: Option<i64> = conn
            .query_row(
                "SELECT id FROM tasks WHERE uuid = ?1",
                params![identifier],
                |r| r.get(0),
            )
            .ok();
        if let Some(id) = id {
            return Ok((pool_path, id));
        }
        bail!("task UUID '{identifier}' not found in pool");
    }

    // Try as "pool-uuid/id" format
    if let Some((_, id_str)) = identifier.rsplit_once('/')
        && let Ok(id) = id_str.parse::<i64>()
    {
        let conn =
            open_pool_db(&pool_path).ok_or_else(|| anyhow::anyhow!("cannot open pool index.db"))?;
        let exists: bool = conn
            .query_row("SELECT 1 FROM tasks WHERE id = ?1", params![id], |_| {
                Ok(true)
            })
            .unwrap_or(false);
        if exists {
            return Ok((pool_path, id));
        }
    }

    bail!("cannot resolve task identifier '{identifier}'")
}

// ─── Deadlines ─────────────────────────────────────────────────────

/// A deadline entry returned by `pool_list_deadlines`.
pub struct DeadlineEntry {
    pub name: String,
    pub status: String,
    pub deadline: String,
    pub important: bool,
}

/// Parse a human-readable duration string into seconds.
pub fn parse_duration(s: &str) -> Result<f64> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty duration string");
    }
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

pub fn pool_list_deadlines(
    scope: &TaskScope,
    important_only: bool,
    horizon: Option<&str>,
) -> Result<Vec<DeadlineEntry>> {
    let pool_path = scope
        .pool_dir
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no pool initialized"))?;

    let conn =
        open_pool_db(pool_path).ok_or_else(|| anyhow::anyhow!("cannot open pool index.db"))?;

    let horizon_secs: Option<f64> = match horizon {
        Some(h) => Some(parse_duration(h)?),
        None => None,
    };

    let mut sql = String::from(
        "SELECT id, status, deadline, important FROM tasks \
         WHERE done IS NULL AND cancelled IS NULL AND deadline IS NOT NULL",
    );
    if important_only {
        sql.push_str(" AND important > 0");
    }
    sql.push_str(" ORDER BY deadline ASC");

    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(i64, String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .filter_map(|r| r.ok())
        .collect();

    let now_secs = current_unix_secs();
    let mut entries = Vec::new();

    for (id, status, deadline, imp) in rows {
        if let Some(horizon) = horizon_secs
            && let Some(deadline_secs) = parse_iso8601_to_unix(&deadline)
        {
            let cutoff = now_secs as f64 + horizon;
            if (deadline_secs as f64) > cutoff {
                continue;
            }
        }

        // Read title from TASK.md
        let task_md_path = pool_path.join("tasks").join(id.to_string()).join("TASK.md");
        let name = if let Ok(content) = std::fs::read_to_string(&task_md_path) {
            let parsed = parse_pool_task_md(&content).ok();
            parsed
                .map(|t| t.title)
                .unwrap_or_else(|| format!("task {id}"))
        } else {
            format!("task {id}")
        };

        entries.push(DeadlineEntry {
            name,
            status,
            deadline,
            important: imp > 0,
        });
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
        let imp = if e.important { " (important)" } else { "" };
        out.push_str(&format!(
            "- {name} [{status}] deadline: {deadline}{marker}{imp}\n",
            name = e.name,
            status = e.status,
            deadline = e.deadline,
        ));
    }
    out
}

// ─── Frontmatter parsing ───────────────────────────────────────────

/// Parse YAML frontmatter from a markdown string.
/// Returns `(key-value pairs, body after frontmatter)`.
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
    let mut i = 0;
    while i < yaml_lines.len() {
        let line = yaml_lines[i].trim();
        if line.is_empty() || line.starts_with('#') {
            i += 1;
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_string();
            let value = value.trim().to_string();
            // Skip indented blocks (subtasks-style YAML blocks handled inline)
            if value.is_empty() {
                // Could be a block — check if next lines are indented
                let mut j = i + 1;
                while j < yaml_lines.len()
                    && (yaml_lines[j].starts_with("  ") || yaml_lines[j].starts_with('\t'))
                {
                    j += 1;
                }
                if j > i + 1 {
                    // Skip the block entirely
                    i = j;
                    continue;
                }
            }
            let value = strip_yaml_quotes(&value);
            pairs.push((key, value));
        }
        i += 1;
    }

    // Body is everything after the closing ---.
    let body_lines = &lines[end_line + 1..];
    let body = body_lines.join("\n");
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

/// Rebuild a TASK.md from key-value pairs and a body.
fn rebuild_task_md_from_pairs(pairs: &[(String, String)], body: &str) -> String {
    let mut lines = vec!["---".to_string()];
    for (k, v) in pairs {
        lines.push(format!("{}: {}", k, v));
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

/// Update or insert a field in frontmatter pairs.
fn update_frontmatter_field(
    pairs: &[(String, String)],
    key: &str,
    value: &str,
) -> Vec<(String, String)> {
    let mut new_pairs: Vec<(String, String)> = Vec::new();
    let mut found = false;
    for (k, v) in pairs {
        if k == key {
            new_pairs.push((key.to_string(), value.to_string()));
            found = true;
        } else {
            new_pairs.push((k.clone(), v.clone()));
        }
    }
    if !found {
        new_pairs.push((key.to_string(), value.to_string()));
    }
    new_pairs
}

/// Helper to look up a frontmatter value by key.
struct FrontmatterMap<'a>(&'a [(String, String)]);

impl FrontmatterMap<'_> {
    fn get(&self, key: &str) -> Option<String> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }
}

// ─── Child object support (used by global.rs / install.rs) ─────────

/// Build a complete `object.json` for a child object.
pub fn build_child_object_json(child_data: &serde_json::Value) -> String {
    let obj = serde_json::json!({
        "type": "child",
        "data": child_data,
    });
    serde_json::to_string_pretty(&obj).unwrap() + "\n"
}

/// Create a new `object/<uuid>` branch containing the given files.
pub fn create_object_branch(
    backend: &dyn Backend,
    scope: &TaskScope,
    uuid: &str,
    files: &[(&str, &str)],
) -> Result<String> {
    let ref_name = format!("refs/heads/object/{uuid}");
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

/// Insert a row into the state.db `objects` table.
pub fn insert_object(
    conn: &Connection,
    uuid: &str,
    owner_uuid: &str,
    owner_type: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO objects (uuid, owner_uuid, owner_type) VALUES (?1, ?2, ?3)",
        params![uuid, owner_uuid, owner_type],
    )?;
    Ok(())
}

// ─── Git plumbing helpers ──────────────────────────────────────────

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

// ─── Time utilities ────────────────────────────────────────────────

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

fn current_unix_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn parse_iso8601_to_unix(s: &str) -> Option<i64> {
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
        assert_eq!(iso8601_from_unix(946_684_800), "2000-01-01T00:00:00Z");
        assert_eq!(iso8601_from_unix(1_775_997_296), "2026-04-12T12:34:56Z");
        assert_eq!(iso8601_from_unix(1_709_251_199), "2024-02-29T23:59:59Z");
    }

    #[test]
    fn iso8601_handles_pre_epoch() {
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
        assert_eq!(out.len(), 20);
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
        assert_eq!(parse_iso8601_to_unix("2026-04-12T12:34:56"), None);
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
    fn generate_pool_task_md_roundtrip() {
        let task = PoolTask {
            id: 1,
            uuid: Some("abc-123".to_string()),
            title: "My Task".to_string(),
            list: Some("work".to_string()),
            topic: Some("testing".to_string()),
            task_type: "todo".to_string(),
            status: "active".to_string(),
            important: true,
            deadline: Some("2026-04-10T00:00:00Z".to_string()),
            parents: vec![3],
            subtasks: vec![5, 8],
            created: Some("2026-04-05T19:00:00Z".to_string()),
            done: None,
            cancelled: None,
            body: String::new(),
        };
        let md = generate_pool_task_md(&task);
        assert!(md.contains("id: 1"));
        assert!(md.contains("uuid: abc-123"));
        assert!(md.contains("list: work"));
        assert!(md.contains("type: todo"));
        assert!(md.contains("important: true"));
        assert!(md.contains("parents: [3]"));
        assert!(md.contains("subtasks: [5, 8]"));
        assert!(md.contains("# My Task"));
    }

    #[test]
    fn parse_int_list_works() {
        assert_eq!(parse_int_list("[]"), vec![] as Vec<i64>);
        assert_eq!(parse_int_list("[1, 2, 3]"), vec![1, 2, 3]);
        assert_eq!(parse_int_list("1, 2"), vec![1, 2]);
        assert_eq!(parse_int_list(""), vec![] as Vec<i64>);
    }
}
