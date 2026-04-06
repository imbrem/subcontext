//! Dolt binary management, repo initialization, and SQL execution.
//!
//! Provides [`DoltConnection`] as a replacement for `rusqlite::Connection`,
//! shelling out to `dolt sql` for SQL operations against a local Dolt repo.
//! Dolt commit tracking is managed via the git state branch.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::backend::Backend;

// ─── Binary management ───────────────────────────────────────────────

/// Known Dolt release version to download.
const DOLT_VERSION: &str = "1.85.0";

/// Return the global Dolt binary path: `~/.subcontext/bin/dolt`.
pub fn global_dolt_bin() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home)
        .join(".subcontext")
        .join("bin")
        .join("dolt"))
}

/// Find the Dolt binary. Checks:
/// 1. `~/.subcontext/bin/dolt`
/// 2. `dolt` on PATH
pub fn find_dolt_bin() -> Result<PathBuf> {
    // Check global install location first
    if let Ok(global) = global_dolt_bin() {
        if global.is_file() {
            return Ok(global);
        }
    }

    // Check PATH
    if let Ok(output) = Command::new("which").arg("dolt").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }

    bail!(
        "dolt binary not found. Run `subcontext install` to download it, \
         or install dolt manually and ensure it's on PATH."
    )
}

/// Download the Dolt binary to `~/.subcontext/bin/dolt`.
pub fn download_dolt(backend: &dyn Backend) -> Result<PathBuf> {
    let bin_dir = {
        let home = std::env::var("HOME").context("HOME not set")?;
        PathBuf::from(home).join(".subcontext").join("bin")
    };
    backend.create_dir_all(&bin_dir)?;

    let dest = bin_dir.join("dolt");
    if backend.is_file(&dest) {
        eprintln!(
            "[subcontext] Dolt binary already exists at {}",
            dest.display()
        );
        return Ok(dest);
    }

    let arch = std::env::consts::ARCH;
    let os = std::env::consts::OS;

    let (os_name, arch_name) = match (os, arch) {
        ("linux", "x86_64") => ("linux", "amd64"),
        ("linux", "aarch64") => ("linux", "arm64"),
        ("macos", "x86_64") => ("darwin", "amd64"),
        ("macos", "aarch64") => ("darwin", "arm64"),
        _ => bail!("unsupported platform: {os}/{arch}. Please install dolt manually."),
    };

    let url = format!(
        "https://github.com/dolthub/dolt/releases/download/v{DOLT_VERSION}/dolt-{os_name}-{arch_name}.tar.gz"
    );

    eprintln!("[subcontext] Downloading dolt v{DOLT_VERSION} for {os_name}/{arch_name}...");

    // Download and extract in one pipeline:
    //   curl -sL <url> | tar xz -C <bin_dir> --strip-components=1 dolt-<os>-<arch>/bin/dolt
    let tar_prefix = format!("dolt-{os_name}-{arch_name}/bin/dolt");
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "curl -fsSL '{}' | tar xz -C '{}' --strip-components=2 '{}'",
            url,
            bin_dir.display(),
            tar_prefix
        ))
        .status()
        .context("failed to run curl | tar")?;

    if !status.success() {
        bail!("failed to download dolt from {url}");
    }

    #[cfg(unix)]
    backend.set_permissions_mode(&dest, 0o755)?;

    eprintln!("[subcontext] Dolt installed to {}", dest.display());
    Ok(dest)
}

// ─── Dolt repo initialization ────────────────────────────────────────

/// Initialize a Dolt repository at the given path.
pub fn init_dolt_repo(backend: &dyn Backend, dolt_path: &Path) -> Result<()> {
    backend.create_dir_all(dolt_path)?;

    // Check if already initialized
    if backend.is_dir(&dolt_path.join(".dolt")) {
        return Ok(());
    }

    let dolt_bin = find_dolt_bin()?;

    let output = Command::new(&dolt_bin)
        .args([
            "init",
            "--name",
            "subcontext",
            "--email",
            "subcontext@local",
        ])
        .current_dir(dolt_path)
        .output()
        .with_context(|| format!("failed to run dolt init in {}", dolt_path.display()))?;

    let status = output.status;

    if !status.success() {
        bail!("dolt init failed in {}", dolt_path.display());
    }

    Ok(())
}

// ─── DoltConnection ──────────────────────────────────────────────────

/// A connection to a local Dolt repository, analogous to `rusqlite::Connection`.
/// All SQL operations shell out to `dolt sql`.
pub struct DoltConnection {
    /// Path to the Dolt repository (contains `.dolt/`).
    repo_path: PathBuf,
    /// Path to the dolt binary.
    dolt_bin: PathBuf,
}

impl DoltConnection {
    /// Open a connection to a Dolt repo at the given path.
    pub fn open(repo_path: &Path) -> Result<Self> {
        let dolt_bin = find_dolt_bin()?;
        Ok(Self {
            repo_path: repo_path.to_path_buf(),
            dolt_bin,
        })
    }

    /// Execute a SQL statement that returns no rows (INSERT, UPDATE, DELETE, CREATE TABLE, etc.).
    /// Parameters are substituted positionally: `?1`, `?2`, etc. are replaced with the
    /// provided string values. Values are SQL-escaped.
    pub fn execute(&self, sql: &str, params: &[&str]) -> Result<()> {
        let resolved = substitute_params(sql, params);
        self.run_sql(&resolved)?;
        Ok(())
    }

    /// Execute a batch of SQL statements (semicolon-separated, no params).
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        self.run_sql(sql)?;
        Ok(())
    }

    /// Query a single row. Returns `None` if no rows match.
    /// The closure receives a `DoltRow` from which columns can be extracted.
    pub fn query_row<T, F>(&self, sql: &str, params: &[&str], f: F) -> Result<Option<T>>
    where
        F: FnOnce(&DoltRow) -> Result<T>,
    {
        let resolved = substitute_params(sql, params);
        let output = self.run_sql_json(&resolved)?;
        let rows = parse_dolt_json_rows(&output)?;
        if rows.is_empty() {
            return Ok(None);
        }
        let row = DoltRow { columns: &rows[0] };
        Ok(Some(f(&row)?))
    }

    /// Query multiple rows. Returns a Vec built by applying the closure to each row.
    pub fn query_map<T, F>(&self, sql: &str, params: &[&str], f: F) -> Result<Vec<T>>
    where
        F: Fn(&DoltRow) -> Result<T>,
    {
        let resolved = substitute_params(sql, params);
        let output = self.run_sql_json(&resolved)?;
        let rows = parse_dolt_json_rows(&output)?;
        let mut results = Vec::new();
        for row_data in &rows {
            let row = DoltRow { columns: row_data };
            results.push(f(&row)?);
        }
        Ok(results)
    }

    /// Create a Dolt commit with the given message. Returns the commit hash.
    pub fn commit(&self, message: &str) -> Result<String> {
        // Stage all changes
        self.run_dolt(&["add", "-A"])?;

        // Commit (--allow-empty handles the "nothing to commit" case gracefully
        // by creating a commit even when there are no changes, which is fine
        // for our use case since we just want a commit hash to track).
        self.run_dolt(&[
            "commit",
            "--allow-empty",
            "-m",
            message,
            "--author",
            "subcontext <subcontext@local>",
        ])?;

        self.head_commit()
    }

    /// Get the current HEAD commit hash.
    pub fn head_commit(&self) -> Result<String> {
        // Use dolt log to get the latest commit hash
        let output = self.run_sql_json("SELECT commit_hash FROM dolt_log LIMIT 1")?;
        let rows = parse_dolt_json_rows(&output)?;
        if rows.is_empty() {
            bail!("no commits in dolt repo");
        }
        let row = DoltRow { columns: &rows[0] };
        row.get::<String>(0)
    }

    // ─── Internal helpers ────────────────────────────────────────────

    /// Run `dolt sql -q <sql>` and return stdout.
    fn run_sql(&self, sql: &str) -> Result<String> {
        let output = Command::new(&self.dolt_bin)
            .args(["sql", "-q", sql])
            .current_dir(&self.repo_path)
            .output()
            .with_context(|| format!("failed to run dolt sql in {}", self.repo_path.display()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("dolt sql failed: {}\nSQL: {}", stderr.trim(), sql);
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Run `dolt sql -r json -q <sql>` and return JSON stdout.
    fn run_sql_json(&self, sql: &str) -> Result<String> {
        let output = Command::new(&self.dolt_bin)
            .args(["sql", "-r", "json", "-q", sql])
            .current_dir(&self.repo_path)
            .output()
            .with_context(|| format!("failed to run dolt sql in {}", self.repo_path.display()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("dolt sql failed: {}\nSQL: {}", stderr.trim(), sql);
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Run an arbitrary dolt command and return stdout.
    fn run_dolt(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(&self.dolt_bin)
            .args(args)
            .current_dir(&self.repo_path)
            .output()
            .with_context(|| {
                format!(
                    "failed to run dolt {} in {}",
                    args.join(" "),
                    self.repo_path.display()
                )
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("dolt {} failed: {}", args.join(" "), stderr.trim());
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

// ─── DoltRow ─────────────────────────────────────────────────────────

/// A single row from a Dolt query result.
/// Column values are stored as `serde_json::Value` and can be extracted by index.
pub struct DoltRow<'a> {
    columns: &'a [Value],
}

impl<'a> DoltRow<'a> {
    /// Get column value at the given index, converted to the requested type.
    pub fn get<T: FromDoltValue>(&self, idx: usize) -> Result<T> {
        if idx >= self.columns.len() {
            bail!(
                "column index {idx} out of range (have {} columns)",
                self.columns.len()
            );
        }
        T::from_dolt_value(&self.columns[idx])
    }
}

/// Trait for converting a `serde_json::Value` to a Rust type.
pub trait FromDoltValue: Sized {
    fn from_dolt_value(v: &Value) -> Result<Self>;
}

impl FromDoltValue for String {
    fn from_dolt_value(v: &Value) -> Result<Self> {
        match v {
            Value::String(s) => Ok(s.clone()),
            Value::Number(n) => Ok(n.to_string()),
            Value::Null => bail!("unexpected NULL for String column"),
            _ => Ok(v.to_string()),
        }
    }
}

impl FromDoltValue for Option<String> {
    fn from_dolt_value(v: &Value) -> Result<Self> {
        match v {
            Value::Null => Ok(None),
            Value::String(s) => Ok(Some(s.clone())),
            Value::Number(n) => Ok(Some(n.to_string())),
            _ => Ok(Some(v.to_string())),
        }
    }
}

impl FromDoltValue for f64 {
    fn from_dolt_value(v: &Value) -> Result<Self> {
        match v {
            Value::Number(n) => n.as_f64().context("invalid f64"),
            Value::String(s) => s.parse().context("invalid f64 string"),
            Value::Null => Ok(0.0),
            _ => bail!("expected number, got {v}"),
        }
    }
}

impl FromDoltValue for i64 {
    fn from_dolt_value(v: &Value) -> Result<Self> {
        match v {
            Value::Number(n) => n.as_i64().context("invalid i64"),
            Value::String(s) => s.parse().context("invalid i64 string"),
            Value::Null => Ok(0),
            _ => bail!("expected number, got {v}"),
        }
    }
}

impl FromDoltValue for usize {
    fn from_dolt_value(v: &Value) -> Result<Self> {
        match v {
            Value::Number(n) => {
                let i = n.as_i64().context("invalid usize")?;
                Ok(i as usize)
            }
            Value::String(s) => s.parse().context("invalid usize string"),
            Value::Null => Ok(0),
            _ => bail!("expected number, got {v}"),
        }
    }
}

// ─── Parameter substitution ──────────────────────────────────────────

/// Substitute `?1`, `?2`, etc. with SQL-escaped parameter values.
fn substitute_params(sql: &str, params: &[&str]) -> String {
    let mut result = sql.to_string();
    // Replace in reverse order so ?10 doesn't match ?1 first
    for (i, val) in params.iter().enumerate().rev() {
        let placeholder = format!("?{}", i + 1);
        let escaped = sql_escape_string(val);
        result = result.replace(&placeholder, &escaped);
    }
    result
}

/// Escape a string value for SQL (wrap in single quotes, escape inner quotes).
fn sql_escape_string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

// ─── JSON result parsing ─────────────────────────────────────────────

/// Parse `dolt sql -r json` output into rows of column values.
///
/// Dolt JSON output looks like:
/// ```json
/// {"rows": [{"col1": "val1", "col2": 42}, ...]}
/// ```
fn parse_dolt_json_rows(json_str: &str) -> Result<Vec<Vec<Value>>> {
    let trimmed = json_str.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let parsed: Value = serde_json::from_str(trimmed)
        .with_context(|| format!("failed to parse dolt JSON output: {trimmed}"))?;

    let empty_arr = Value::Array(Vec::new());
    let rows_val = parsed.get("rows").unwrap_or(&empty_arr);

    let rows_arr = match rows_val {
        Value::Array(arr) => arr,
        _ => return Ok(Vec::new()),
    };

    let mut result = Vec::new();
    for row_obj in rows_arr {
        match row_obj {
            Value::Object(map) => {
                // Preserve insertion order (serde_json objects maintain order)
                let values: Vec<Value> = map.values().cloned().collect();
                result.push(values);
            }
            _ => continue,
        }
    }

    Ok(result)
}

// ─── Dolt commit tracking in state branch ────────────────────────────

const DOLT_HEAD_FILE: &str = "dolt_head";

/// Write the current dolt commit hash to the state branch.
/// This enables future branching: each git branch can track its own dolt commit.
pub fn save_dolt_head(backend: &dyn Backend, state: &Path, dolt_commit: &str) -> Result<()> {
    let head_file = state.join(DOLT_HEAD_FILE);
    backend.write(&head_file, dolt_commit.as_bytes())?;
    Ok(())
}

/// Read the current dolt commit hash from the state branch.
pub fn read_dolt_head(backend: &dyn Backend, state: &Path) -> Result<Option<String>> {
    let head_file = state.join(DOLT_HEAD_FILE);
    if !backend.is_file(&head_file) {
        return Ok(None);
    }
    let content = backend.read_to_string(&head_file)?;
    let trimmed = content.trim().to_string();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(trimmed))
}

// ─── Schema creation ─────────────────────────────────────────────────

/// Create the tasks database schema in a Dolt repo.
/// Equivalent to the old `create_schema()` for SQLite.
pub fn create_dolt_schema(conn: &DoltConnection) -> Result<()> {
    // Dolt uses MySQL syntax. Execute each statement separately since
    // dolt sql doesn't always handle multi-statement batches well.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tasks (
             task_uuid        VARCHAR(36) PRIMARY KEY,
             task_name        VARCHAR(255) NOT NULL,
             task_status      VARCHAR(50) NOT NULL,
             task_kind        VARCHAR(50) NOT NULL,
             task_description TEXT DEFAULT NULL,
             project_uuid     VARCHAR(36) NOT NULL,
             task_deadline    VARCHAR(50) DEFAULT NULL,
             task_importance  DOUBLE NOT NULL DEFAULT 0.0,
             parent_task_uuid VARCHAR(36) DEFAULT NULL,
             board_uuid       VARCHAR(36) DEFAULT NULL,
             subtasks         TEXT NOT NULL DEFAULT '{}'
         );",
    )?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS task_names (
             branch_name   VARCHAR(255) NOT NULL,
             task_name     VARCHAR(255) NOT NULL,
             task_uuid     VARCHAR(36) NOT NULL UNIQUE,
             INDEX idx_task_names_lookup (branch_name, task_name)
         );",
    )?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS objects (
             uuid                  VARCHAR(36) PRIMARY KEY,
             type                  VARCHAR(50) NOT NULL,
             current_commit        VARCHAR(255) NOT NULL,
             board_uuid            VARCHAR(36) DEFAULT NULL,
             source_context_uuid   VARCHAR(36) DEFAULT NULL,
             source_object_uuid    VARCHAR(36) DEFAULT NULL,
             source_context_commit VARCHAR(255) DEFAULT NULL
         );",
    )?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS branch_tasks (
             scope_branch  VARCHAR(255) PRIMARY KEY,
             task_uuid     VARCHAR(36) NOT NULL
         );",
    )?;

    Ok(())
}

/// Create the extra global schema tables (config, parents) in a Dolt repo.
pub fn create_dolt_global_schema(conn: &DoltConnection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS config (
             key_name   VARCHAR(255) PRIMARY KEY,
             value      TEXT NOT NULL
         );",
    )?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS parents (
             child_uuid  VARCHAR(36) PRIMARY KEY,
             parent_uuid VARCHAR(36) NOT NULL
         );",
    )?;

    Ok(())
}
