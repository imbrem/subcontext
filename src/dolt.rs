//! Dolt binary management, repo initialization, and SQL execution.
//!
//! Provides [`DoltConnection`] which connects to a local `dolt sql-server`
//! via the MySQL protocol using the `mysql` crate. The server is started
//! on-demand and reused across connections within the same process.

use anyhow::{Context, Result, bail};
use mysql::prelude::*;
use mysql::*;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::backend::Backend;

// ─── Binary management ───────────────────────────────────────────────

/// Known Dolt release version to download.
const DOLT_VERSION: &str = "1.85.0";

/// Global counter for unique socket paths across processes.
static SOCKET_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    if let Ok(global) = global_dolt_bin() {
        if global.is_file() {
            return Ok(global);
        }
    }
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

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "dolt init failed in {}: {}",
            dolt_path.display(),
            stderr.trim()
        );
    }
    Ok(())
}

// ─── DoltConnection ──────────────────────────────────────────────────

/// A connection to a local Dolt repository via `dolt sql-server` + MySQL protocol.
///
/// The first `open()` for a given repo starts a `dolt sql-server` on a
/// per-process Unix socket. Subsequent `open()` calls in the same process
/// reuse the existing server. The server is killed when all connections
/// are dropped (tracked via the server handle).
pub struct DoltConnection {
    pool: Pool,
    /// If Some, this connection owns the server process and will kill it on Drop.
    server: Option<Child>,
    socket_path: PathBuf,
}

impl DoltConnection {
    /// Open a connection to a Dolt repo at the given path.
    ///
    /// If a server is already running for this repo (in this process),
    /// reuses its socket. Otherwise starts a new server.
    pub fn open(repo_path: &Path) -> Result<Self> {
        let dolt_bin = find_dolt_bin()?;

        // Use a per-process socket path (stable across multiple open() calls)
        let socket_path = repo_path.join(format!(".dolt-server-{}.sock", std::process::id()));

        // Try connecting to existing server first
        if socket_path.exists() {
            let db_name = repo_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("dolt");
            let opts = OptsBuilder::new()
                .socket(Some(socket_path.to_string_lossy().into_owned()))
                .user(Some("root"))
                .db_name(Some(db_name))
                .pool_opts(PoolOpts::new().with_constraints(PoolConstraints::new(1, 2).unwrap()));
            if let Ok(pool) = Pool::new(opts) {
                if let Ok(mut conn) = pool.get_conn() {
                    if conn.query_drop("SELECT 1").is_ok() {
                        return Ok(Self {
                            pool,
                            server: None, // Don't own the server
                            socket_path: socket_path.clone(),
                        });
                    }
                }
            }
            // Stale socket, remove it
            std::fs::remove_file(&socket_path).ok();
        }

        // Need to start a new server
        let counter = SOCKET_COUNTER.fetch_add(1, Ordering::SeqCst);
        let time_component = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let port: u16 = 10000
            + ((std::process::id() as u32 ^ time_component ^ (counter as u32)) % 50000) as u16;
        let port_str = port.to_string();

        // Ensure HOME directory exists (dolt sql-server requires it)
        if let Ok(home) = std::env::var("HOME") {
            std::fs::create_dir_all(&home).ok();
        }

        // Remove auto-generated config.yaml to prevent stale settings
        std::fs::remove_file(repo_path.join("config.yaml")).ok();

        let mut server = Command::new(&dolt_bin)
            .args([
                "sql-server",
                "--socket",
                &socket_path.to_string_lossy(),
                "--host",
                "127.0.0.1",
                "--port",
                &port_str,
            ])
            .current_dir(repo_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| {
                format!("failed to start dolt sql-server in {}", repo_path.display())
            })?;

        // Wait for the socket to appear
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(15);
        while !socket_path.exists() {
            if start.elapsed() > timeout {
                if let Ok(Some(status)) = server.try_wait() {
                    bail!("dolt sql-server exited with {status} before socket was ready");
                }
                bail!(
                    "dolt sql-server did not start within 15s (socket: {}, port: {port})",
                    socket_path.display()
                );
            }
            if let Ok(Some(status)) = server.try_wait() {
                bail!("dolt sql-server exited with {status} before socket was ready");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        // Small delay for the server to accept connections
        std::thread::sleep(std::time::Duration::from_millis(100));

        let db_name = repo_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("dolt");

        let opts = OptsBuilder::new()
            .socket(Some(socket_path.to_string_lossy().into_owned()))
            .user(Some("root"))
            .db_name(Some(db_name))
            .pool_opts(PoolOpts::new().with_constraints(PoolConstraints::new(1, 2).unwrap()));
        let pool = Pool::new(opts)?;

        // Verify connectivity
        let mut conn = pool.get_conn()?;
        conn.query_drop("SELECT 1")?;

        Ok(Self {
            pool,
            server: Some(server),
            socket_path,
        })
    }

    /// Execute a SQL statement that returns no rows.
    pub fn execute(&self, sql: &str, params: &[&str]) -> Result<()> {
        let resolved = substitute_params(sql, params);
        let mut conn = self.pool.get_conn()?;
        conn.query_drop(&resolved)
            .with_context(|| format!("dolt sql execute failed.\nSQL: {resolved}"))?;
        Ok(())
    }

    /// Execute a batch of SQL statements (no params).
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        let mut conn = self.pool.get_conn()?;
        conn.query_drop(sql)
            .with_context(|| format!("dolt sql batch failed.\nSQL: {sql}"))?;
        Ok(())
    }

    /// Query a single row. Returns `None` if no rows match.
    pub fn query_row<T, F>(&self, sql: &str, params: &[&str], f: F) -> Result<Option<T>>
    where
        F: FnOnce(&DoltRow) -> Result<T>,
    {
        let resolved = substitute_params(sql, params);
        let mut conn = self.pool.get_conn()?;
        let rows: Vec<Row> = conn
            .query(&resolved)
            .with_context(|| format!("dolt sql query failed.\nSQL: {resolved}"))?;
        if rows.is_empty() {
            return Ok(None);
        }
        let dolt_row = DoltRow { inner: &rows[0] };
        Ok(Some(f(&dolt_row)?))
    }

    /// Query multiple rows.
    pub fn query_map<T, F>(&self, sql: &str, params: &[&str], f: F) -> Result<Vec<T>>
    where
        F: Fn(&DoltRow) -> Result<T>,
    {
        let resolved = substitute_params(sql, params);
        let mut conn = self.pool.get_conn()?;
        let rows: Vec<Row> = conn
            .query(&resolved)
            .with_context(|| format!("dolt sql query_map failed.\nSQL: {resolved}"))?;
        let mut results = Vec::new();
        for row in &rows {
            let dolt_row = DoltRow { inner: row };
            results.push(f(&dolt_row)?);
        }
        Ok(results)
    }

    /// Create a Dolt commit with the given message. Returns the commit hash.
    pub fn commit(&self, message: &str) -> Result<String> {
        let mut conn = self.pool.get_conn()?;
        conn.query_drop("CALL DOLT_ADD('-A')")?;
        let escaped_msg = message.replace('\'', "''");
        conn.query_drop(format!(
            "CALL DOLT_COMMIT('--allow-empty', '-m', '{escaped_msg}', '--author', 'subcontext <subcontext@local>')"
        ))?;
        self.head_commit()
    }

    /// Get the current HEAD commit hash.
    pub fn head_commit(&self) -> Result<String> {
        let mut conn = self.pool.get_conn()?;
        let hash: Option<String> = conn.query_first("SELECT commit_hash FROM dolt_log LIMIT 1")?;
        hash.context("no commits in dolt repo")
    }
}

impl Drop for DoltConnection {
    fn drop(&mut self) {
        // Only kill the server if we own it
        if let Some(ref mut server) = self.server {
            server.kill().ok();
            server.wait().ok();
            // Clean up the socket file
            std::fs::remove_file(&self.socket_path).ok();
        }
    }
}

// ─── DoltRow ─────────────────────────────────────────────────────────

/// A single row from a Dolt query result.
pub struct DoltRow<'a> {
    inner: &'a Row,
}

impl<'a> DoltRow<'a> {
    pub fn get<T: FromDoltValue>(&self, idx: usize) -> Result<T> {
        T::from_mysql_row(self.inner, idx)
    }
}

/// Trait for converting a MySQL column value to a Rust type.
pub trait FromDoltValue: Sized {
    fn from_mysql_row(row: &Row, idx: usize) -> Result<Self>;
}

impl FromDoltValue for String {
    fn from_mysql_row(row: &Row, idx: usize) -> Result<Self> {
        // Use from_value_opt to avoid panics
        let value = row.as_ref(idx).context("column index out of range")?;
        match mysql::from_value_opt::<String>(value.clone()) {
            Ok(s) => Ok(s),
            Err(_) => bail!("unexpected NULL or invalid type for String column"),
        }
    }
}

impl FromDoltValue for Option<String> {
    fn from_mysql_row(row: &Row, idx: usize) -> Result<Self> {
        let value = row.as_ref(idx).context("column index out of range")?;
        if *value == mysql::Value::NULL {
            return Ok(None);
        }
        match mysql::from_value_opt::<String>(value.clone()) {
            Ok(s) => Ok(Some(s)),
            Err(_) => Ok(None),
        }
    }
}

impl FromDoltValue for f64 {
    fn from_mysql_row(row: &Row, idx: usize) -> Result<Self> {
        let value = row.as_ref(idx).context("column index out of range")?;
        if *value == mysql::Value::NULL {
            return Ok(0.0);
        }
        match mysql::from_value_opt::<f64>(value.clone()) {
            Ok(v) => Ok(v),
            Err(_) => {
                // Try string fallback
                if let Ok(s) = mysql::from_value_opt::<String>(value.clone()) {
                    s.parse().context("invalid f64 string")
                } else {
                    Ok(0.0)
                }
            }
        }
    }
}

impl FromDoltValue for i64 {
    fn from_mysql_row(row: &Row, idx: usize) -> Result<Self> {
        let value = row.as_ref(idx).context("column index out of range")?;
        if *value == mysql::Value::NULL {
            return Ok(0);
        }
        match mysql::from_value_opt::<i64>(value.clone()) {
            Ok(v) => Ok(v),
            Err(_) => {
                if let Ok(s) = mysql::from_value_opt::<String>(value.clone()) {
                    s.parse().context("invalid i64 string")
                } else {
                    Ok(0)
                }
            }
        }
    }
}

impl FromDoltValue for usize {
    fn from_mysql_row(row: &Row, idx: usize) -> Result<Self> {
        let value = row.as_ref(idx).context("column index out of range")?;
        if *value == mysql::Value::NULL {
            return Ok(0);
        }
        match mysql::from_value_opt::<i64>(value.clone()) {
            Ok(v) => Ok(v as usize),
            Err(_) => {
                if let Ok(s) = mysql::from_value_opt::<String>(value.clone()) {
                    s.parse().context("invalid usize string")
                } else {
                    Ok(0)
                }
            }
        }
    }
}

impl FromDoltValue for bool {
    fn from_mysql_row(row: &Row, idx: usize) -> Result<Self> {
        let value = row.as_ref(idx).context("column index out of range")?;
        if *value == mysql::Value::NULL {
            return Ok(false);
        }
        match mysql::from_value_opt::<i64>(value.clone()) {
            Ok(v) => Ok(v != 0),
            Err(_) => Ok(false),
        }
    }
}

// ─── Parameter substitution ──────────────────────────────────────────

fn substitute_params(sql: &str, params: &[&str]) -> String {
    let mut result = sql.to_string();
    for (i, val) in params.iter().enumerate().rev() {
        let placeholder = format!("?{}", i + 1);
        let escaped = sql_escape_string(val);
        result = result.replace(&placeholder, &escaped);
    }
    result
}

fn sql_escape_string(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

// ─── Dolt commit tracking in state branch ────────────────────────────

const DOLT_HEAD_FILE: &str = "dolt_head";

pub fn save_dolt_head(backend: &dyn Backend, state: &Path, dolt_commit: &str) -> Result<()> {
    let head_file = state.join(DOLT_HEAD_FILE);
    backend.write(&head_file, dolt_commit.as_bytes())?;
    Ok(())
}

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

pub fn create_dolt_schema(conn: &DoltConnection) -> Result<()> {
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
         )",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS task_names (
             branch_name   VARCHAR(255) NOT NULL,
             task_name     VARCHAR(255) NOT NULL,
             task_uuid     VARCHAR(36) NOT NULL UNIQUE,
             INDEX idx_task_names_lookup (branch_name, task_name)
         )",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS objects (
             uuid                  VARCHAR(36) PRIMARY KEY,
             `type`                VARCHAR(50) NOT NULL,
             current_commit        VARCHAR(255) NOT NULL,
             board_uuid            VARCHAR(36) DEFAULT NULL,
             source_context_uuid   VARCHAR(36) DEFAULT NULL,
             source_object_uuid    VARCHAR(36) DEFAULT NULL,
             source_context_commit VARCHAR(255) DEFAULT NULL
         )",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS branch_tasks (
             scope_branch  VARCHAR(255) PRIMARY KEY,
             task_uuid     VARCHAR(36) NOT NULL
         )",
    )?;
    Ok(())
}

pub fn create_dolt_global_schema(conn: &DoltConnection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS config (
             `key_name`  VARCHAR(255) PRIMARY KEY,
             value       TEXT NOT NULL
         )",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS parents (
             child_uuid  VARCHAR(36) PRIMARY KEY,
             parent_uuid VARCHAR(36) NOT NULL
         )",
    )?;
    Ok(())
}
