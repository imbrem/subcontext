use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use uuid::Uuid;

use crate::git::{
    current_branch, repo_dir, run_subcontext_git, run_work_git, state_dir,
};
use crate::project::read_project_uuid;

const DB_NAME: &str = "tasks.db";
pub const DEFAULT_KIND: &str = "task";
pub const DEFAULT_STATUS: &str = "created";

fn db_path(root: &Path) -> PathBuf {
    state_dir(root).join(DB_NAME)
}

/// Initialize the `state` branch, its worktree, and the tasks.db schema.
pub fn init_state_branch(root: &Path) -> Result<()> {
    // Create empty state branch via plumbing
    let empty_tree = run_subcontext_git(&["hash-object", "-t", "tree", "/dev/null"], root)?;
    let commit = run_subcontext_git(
        &["commit-tree", &empty_tree, "-m", "init state branch"],
        root,
    )?;
    run_subcontext_git(&["update-ref", "refs/heads/state", &commit], root)?;

    // Add worktree
    let state = state_dir(root);
    run_subcontext_git(
        &["worktree", "add", &state.to_string_lossy(), "state"],
        root,
    )?;

    // Create DB + schema
    let conn = Connection::open(db_path(root))?;
    create_schema(&conn)?;
    drop(conn);

    // Commit
    commit_state(root, "init tasks db")?;
    Ok(())
}

fn create_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tasks (
             task_uuid     TEXT PRIMARY KEY,
             task_name     TEXT NOT NULL,
             task_status   TEXT NOT NULL,
             task_kind     TEXT NOT NULL,
             project_uuid  TEXT NOT NULL
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

fn commit_state(root: &Path, message: &str) -> Result<()> {
    let state = state_dir(root);
    run_work_git(&["add", "-A"], &state)?;
    let status = run_work_git(&["status", "--porcelain"], &state)?;
    if status.is_empty() {
        return Ok(());
    }
    run_work_git(&["commit", "-m", message], &state)?;
    Ok(())
}

/// Add a new task.
pub fn add_task(
    root: &Path,
    name: &str,
    kind: Option<&str>,
    status: Option<&str>,
) -> Result<()> {
    let branch = current_branch(root)?;
    let project_uuid = read_project_uuid(root)?;
    let task_uuid = Uuid::new_v4().to_string();
    let kind = kind.unwrap_or(DEFAULT_KIND);
    let status = status.unwrap_or(DEFAULT_STATUS);

    let conn = Connection::open(db_path(root))?;
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
    conn.execute(
        "INSERT INTO tasks (task_uuid, task_name, task_status, task_kind, project_uuid) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![task_uuid, name, status, kind, project_uuid],
    )?;
    conn.execute(
        "INSERT INTO task_names (branch_name, task_name, task_uuid) VALUES (?1, ?2, ?3)",
        params![branch, name, task_uuid],
    )?;
    drop(conn);

    commit_state(root, &format!("task add: {name}"))?;

    let md = build_task_md(&TaskData {
        name: name.to_string(),
        uuid: task_uuid.clone(),
        status: status.to_string(),
        kind: kind.to_string(),
        project_uuid,
        completed_at: None,
    });
    create_task_branch(root, &task_uuid, &md)?;

    eprintln!("[subcontext] Added task '{name}' ({task_uuid})");
    Ok(())
}

/// Mark an existing task as done.
pub fn done_task(root: &Path, name: &str, time: Option<&str>) -> Result<()> {
    let branch = current_branch(root)?;

    let conn = Connection::open(db_path(root))?;
    let row: (String, String, String, String) = conn
        .query_row(
            "SELECT t.task_uuid, t.task_name, t.task_kind, t.project_uuid
             FROM task_names n JOIN tasks t ON n.task_uuid = t.task_uuid
             WHERE n.branch_name = ?1 AND n.task_name = ?2",
            params![branch, name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .with_context(|| format!("task '{name}' not found on branch '{branch}'"))?;
    let (task_uuid, task_name, kind, project_uuid) = row;

    conn.execute(
        "UPDATE tasks SET task_status = 'done' WHERE task_uuid = ?1",
        params![task_uuid],
    )?;
    drop(conn);

    let completed_at = resolve_timestamp(time)?;

    commit_state(root, &format!("task done: {name}"))?;
    let md = build_task_md(&TaskData {
        name: task_name,
        uuid: task_uuid.clone(),
        status: "done".to_string(),
        kind,
        project_uuid,
        completed_at: Some(completed_at),
    });
    update_task_branch(root, &task_uuid, &md)?;

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
    s.push_str("---\n");
    s
}

fn create_task_branch(root: &Path, task_uuid: &str, md: &str) -> Result<()> {
    let ref_name = format!("refs/heads/tasks/{task_uuid}");
    // Defensive: refuse to clobber an existing ref (UUID collision).
    if run_subcontext_git(&["show-ref", "--verify", "--quiet", &ref_name], root).is_ok() {
        bail!("task branch {ref_name} already exists");
    }
    let blob = hash_object(root, md)?;
    let tree = mktree(root, &[("TASK.md", &blob)])?;
    let commit = run_subcontext_git(
        &["commit-tree", &tree, "-m", &format!("init task {task_uuid}")],
        root,
    )?;
    run_subcontext_git(&["update-ref", &ref_name, &commit], root)?;
    Ok(())
}

fn update_task_branch(root: &Path, task_uuid: &str, md: &str) -> Result<()> {
    let branch = format!("tasks/{task_uuid}");
    let ref_name = format!("refs/heads/{branch}");
    let parent = run_subcontext_git(&["rev-parse", &ref_name], root)?;
    let blob = hash_object(root, md)?;
    let tree = mktree(root, &[("TASK.md", &blob)])?;
    let commit = run_subcontext_git(
        &[
            "commit-tree",
            &tree,
            "-p",
            &parent,
            "-m",
            &format!("update task {task_uuid}"),
        ],
        root,
    )?;
    run_subcontext_git(&["update-ref", &ref_name, &commit], root)?;
    Ok(())
}

fn hash_object(root: &Path, content: &str) -> Result<String> {
    let git_dir = repo_dir(root);
    let git_dir_flag = format!("--git-dir={}", git_dir.display());
    let mut child = Command::new("git")
        .args([&git_dir_flag, "hash-object", "-w", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn git hash-object")?;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(content.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "git hash-object failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn mktree(root: &Path, entries: &[(&str, &str)]) -> Result<String> {
    let git_dir = repo_dir(root);
    let git_dir_flag = format!("--git-dir={}", git_dir.display());
    let mut child = Command::new("git")
        .args([&git_dir_flag, "mktree"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn git mktree")?;
    {
        let stdin = child.stdin.as_mut().unwrap();
        for (name, hash) in entries {
            writeln!(stdin, "100644 blob {hash}\t{name}")?;
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!(
            "git mktree failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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
