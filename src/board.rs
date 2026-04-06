//! Board: a separate object type that holds a task hierarchy.
//!
//! A board lives on an `object/<board-uuid>` branch in a subcontext's bare
//! repo.  Its branch layout is:
//!
//! ```text
//! object/<uuid>/
//! ├── board.db          ← SQLite database (auto-rebuilt from tree)
//! └── tasks/
//!     ├── task-a/
//!     │   ├── TASK.md
//!     │   └── subtask-1/
//!     │       └── TASK.md
//!     └── task-b/
//!         └── TASK.md
//! ```
//!
//! The board is checked out as a worktree so that `board.db` is directly
//! usable by SQLite.

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

use crate::backend::Backend;
use crate::git::{run_git_in_bare, run_work_git};
use crate::task::{DEFAULT_KIND, DEFAULT_STATUS, FrontmatterMap, parse_frontmatter};

pub const DB_NAME: &str = "board.db";

/// A board object: a path to the subcontext that contains it, plus the
/// board's own UUID.
pub struct Board {
    /// Root of the subcontext directory (e.g. `.git/.subcontext/` or
    /// `~/.subcontext/global/`).
    pub subcontext_path: PathBuf,
    /// The board's UUID.
    pub uuid: String,
}

impl Board {
    pub fn new(subcontext_path: PathBuf, uuid: String) -> Self {
        Self {
            subcontext_path,
            uuid,
        }
    }

    /// Path to the subcontext's bare repo.
    pub fn repo_dir(&self) -> PathBuf {
        self.subcontext_path.join("repo")
    }

    /// Path to the board's worktree checkout.
    pub fn worktree_dir(&self) -> PathBuf {
        self.subcontext_path.join("board")
    }

    /// Path to the board's SQLite database.
    pub fn db_path(&self) -> PathBuf {
        self.worktree_dir().join(DB_NAME)
    }

    /// The git branch name for this board.
    pub fn branch_name(&self) -> String {
        format!("object/{}", self.uuid)
    }

    /// The git ref name for this board.
    pub fn ref_name(&self) -> String {
        format!("refs/heads/object/{}", self.uuid)
    }

    /// Path to the `tasks/` directory inside the worktree.
    pub fn tasks_dir(&self) -> PathBuf {
        self.worktree_dir().join("tasks")
    }

    // ─── Worktree management ─────────────────────────────────────────

    /// Ensure the board's worktree is checked out.  Creates it if missing.
    pub fn ensure_worktree(&self, backend: &dyn Backend) -> Result<()> {
        let wt = self.worktree_dir();
        if backend.exists(&wt) {
            return Ok(());
        }
        let repo = self.repo_dir();
        run_git_in_bare(
            backend,
            &[
                "worktree",
                "add",
                &wt.to_string_lossy(),
                &self.branch_name(),
            ],
            &repo,
            &repo,
        )?;
        Ok(())
    }

    // ─── Database ────────────────────────────────────────────────────

    /// Open (or create) the board's SQLite database.
    pub fn open_db(&self) -> Result<Connection> {
        let db = self.db_path();
        let conn = Connection::open(&db)?;
        create_board_schema(&conn)?;
        Ok(conn)
    }

    /// Ensure the database exists and is populated from the task tree.
    /// Creates the DB if it doesn't exist, or opens it if it does.
    /// Returns the connection.
    pub fn ensure_db(&self, backend: &dyn Backend) -> Result<Connection> {
        let db = self.db_path();
        let needs_rebuild = !backend.exists(&db);
        let conn = self.open_db()?;
        if needs_rebuild {
            self.rebuild_db_inner(backend, &conn)?;
        }
        Ok(conn)
    }

    /// Rebuild the board's SQLite database from the `tasks/` tree.
    /// Drops all existing rows and re-scans.  Returns the number of
    /// tasks found.
    pub fn rebuild_db(&self, backend: &dyn Backend) -> Result<usize> {
        let conn = self.open_db()?;
        self.rebuild_db_inner(backend, &conn)
    }

    fn rebuild_db_inner(&self, backend: &dyn Backend, conn: &Connection) -> Result<usize> {
        conn.execute("DELETE FROM tasks", [])?;

        let tasks_dir = self.tasks_dir();
        if !backend.exists(&tasks_dir) {
            return Ok(0);
        }

        let mut count = 0;
        self.walk_tasks_dir(backend, &tasks_dir, None, conn, &mut count)?;
        Ok(count)
    }

    /// Recursively walk the tasks/ directory and insert tasks into the DB.
    fn walk_tasks_dir(
        &self,
        backend: &dyn Backend,
        dir: &Path,
        parent_uuid: Option<&str>,
        conn: &Connection,
        count: &mut usize,
    ) -> Result<()> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };

        let mut child_dirs: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                child_dirs.push(path);
            }
        }
        child_dirs.sort();

        for child_dir in &child_dirs {
            let task_md_path = child_dir.join("TASK.md");
            if !task_md_path.exists() {
                continue;
            }
            let content = std::fs::read_to_string(&task_md_path)
                .with_context(|| format!("cannot read {}", task_md_path.display()))?;
            let (pairs, _body) = parse_frontmatter(&content);
            let fm = FrontmatterMap(&pairs);

            let task_name = child_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();

            let task_uuid = fm
                .get("uuid")
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let kind = fm.get("kind").unwrap_or_else(|| DEFAULT_KIND.to_string());
            let status = fm
                .get("status")
                .unwrap_or_else(|| DEFAULT_STATUS.to_string());
            let description = fm.get("description");
            let deadline = fm.get("deadline");
            let importance: f64 = fm
                .get("importance")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0.0);

            conn.execute(
                "INSERT OR REPLACE INTO tasks \
                 (task_uuid, task_name, task_status, task_kind, \
                  task_description, task_deadline, task_importance, \
                  parent_task_uuid) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    task_uuid,
                    task_name,
                    status,
                    kind,
                    description,
                    deadline,
                    importance,
                    parent_uuid
                ],
            )?;
            *count += 1;

            // Recurse into subdirectories for subtasks.
            self.walk_tasks_dir(backend, child_dir, Some(&task_uuid), conn, count)?;
        }
        Ok(())
    }

    /// Check the board's database for corruption and consistency with the
    /// task tree.
    pub fn check(&self, backend: &dyn Backend) -> Result<Vec<String>> {
        let mut issues: Vec<String> = Vec::new();

        let db = self.db_path();
        if !backend.exists(&db) {
            issues.push("board.db does not exist".to_string());
            return Ok(issues);
        }

        let conn = self.open_db()?;

        // 1. SQLite integrity check.
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap_or_else(|_| "error running integrity_check".to_string());
        if integrity != "ok" {
            issues.push(format!("SQLite integrity_check: {integrity}"));
        }

        // 2. Cross-check DB rows against tasks/ tree.
        let tasks_dir = self.tasks_dir();
        if !backend.exists(&tasks_dir) {
            // No tasks dir — DB should be empty.
            let row_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
                .unwrap_or(0);
            if row_count > 0 {
                issues.push(format!(
                    "tasks/ directory missing but DB has {row_count} rows"
                ));
            }
            return Ok(issues);
        }

        // Collect all task UUIDs from DB.
        let mut db_uuids: std::collections::HashSet<String> = std::collections::HashSet::new();
        {
            let mut stmt = conn.prepare("SELECT task_uuid FROM tasks")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            for row in rows {
                if let Ok(uuid) = row {
                    db_uuids.insert(uuid);
                }
            }
        }

        // Walk the tree and collect file-based UUIDs.
        let mut tree_uuids: std::collections::HashSet<String> = std::collections::HashSet::new();
        self.collect_tree_uuids(&tasks_dir, &mut tree_uuids)?;

        // UUIDs in DB but not in tree.
        for uuid in &db_uuids {
            if !tree_uuids.contains(uuid) {
                issues.push(format!("DB has task {uuid} but no TASK.md found in tree"));
            }
        }

        // UUIDs in tree but not in DB.
        for uuid in &tree_uuids {
            if !db_uuids.contains(uuid) {
                issues.push(format!("Tree has TASK.md with uuid {uuid} but no DB row"));
            }
        }

        Ok(issues)
    }

    fn collect_tree_uuids(
        &self,
        dir: &Path,
        uuids: &mut std::collections::HashSet<String>,
    ) -> Result<()> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let task_md = path.join("TASK.md");
                if task_md.exists() {
                    let content = std::fs::read_to_string(&task_md)?;
                    let (pairs, _) = parse_frontmatter(&content);
                    let fm = FrontmatterMap(&pairs);
                    if let Some(uuid) = fm.get("uuid") {
                        uuids.insert(uuid);
                    }
                }
                self.collect_tree_uuids(&path, uuids)?;
            }
        }
        Ok(())
    }

    // ─── Git operations ──────────────────────────────────────────────

    /// Commit all changes in the board worktree.
    pub fn commit(&self, backend: &dyn Backend, message: &str) -> Result<()> {
        let wt = self.worktree_dir();
        run_work_git(backend, &["add", "-A"], &wt)?;
        let status = run_work_git(backend, &["status", "--porcelain"], &wt)?;
        if status.is_empty() {
            return Ok(());
        }
        run_work_git(backend, &["commit", "-m", message], &wt)?;
        Ok(())
    }
}

// ─── Schema ──────────────────────────────────────────────────────────

fn create_board_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tasks (
             task_uuid        TEXT PRIMARY KEY,
             task_name        TEXT NOT NULL,
             task_status      TEXT NOT NULL,
             task_kind        TEXT NOT NULL,
             task_description TEXT DEFAULT NULL,
             task_deadline    TEXT DEFAULT NULL,
             task_importance  REAL NOT NULL DEFAULT 0.0,
             parent_task_uuid TEXT DEFAULT NULL
         );",
    )?;
    Ok(())
}

// ─── Board creation ──────────────────────────────────────────────────

/// Create a new board in the given subcontext.  Creates the branch, checks
/// out a worktree, initializes board.db and the tasks/ directory.
/// Returns the Board.
pub fn create_board(backend: &dyn Backend, subcontext_path: &Path) -> Result<Board> {
    let uuid = uuid::Uuid::new_v4().to_string();
    let board = Board::new(subcontext_path.to_path_buf(), uuid.clone());
    let repo = board.repo_dir();
    let ref_name = board.ref_name();

    // Create the branch with an empty tree.
    let empty_tree = run_git_in_bare(
        backend,
        &["hash-object", "-t", "tree", "/dev/null"],
        &repo,
        &repo,
    )?;
    let commit = run_git_in_bare(
        backend,
        &[
            "commit-tree",
            &empty_tree,
            "-m",
            &format!("init board {uuid}"),
        ],
        &repo,
        &repo,
    )?;
    run_git_in_bare(backend, &["update-ref", &ref_name, &commit], &repo, &repo)?;

    // Check out the worktree.
    board.ensure_worktree(backend)?;

    // Create tasks/ directory and board.db.
    let tasks_dir = board.tasks_dir();
    backend.create_dir_all(&tasks_dir)?;
    // Write a .gitkeep so the empty dir is tracked.
    backend.write(&tasks_dir.join(".gitkeep"), b"")?;

    let conn = board.open_db()?;
    drop(conn);

    // Initial commit.
    board.commit(backend, &format!("init board {uuid}"))?;

    eprintln!("[subcontext] Created board ({uuid})");
    println!("{uuid}");
    Ok(board)
}
