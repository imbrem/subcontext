//! Dolt binary management, repo initialization, and SQL execution via MySQL.
//!
//! Provides [`DoltConnection`] which connects to a `dolt sql-server` via the
//! MySQL wire protocol using the `mysql` crate. A process-level server registry
//! ensures only one server runs per dolt repo path.

use anyhow::{Context, Result, bail};
use mysql::prelude::*;
use mysql::{Conn, Opts, OptsBuilder};
use serde_json::Value;
use std::collections::HashMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use crate::backend::Backend;

// ─── Binary management ───────────────────────────────────────────────

const DOLT_VERSION: &str = "1.85.0";

pub fn global_dolt_bin() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home)
        .join(".subcontext")
        .join("bin")
        .join("dolt"))
}

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
        bail!("dolt init failed in {}", dolt_path.display());
    }
    Ok(())
}

// ─── Port allocation ────────────────────────────────────────────────

fn find_free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("failed to bind to free port")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

// ─── Server registry ─────────────────────────────────────────────────

struct DoltServer {
    server: Child,
    port: u16,
    db_name: String,
    ref_count: usize,
}

impl DoltServer {
    fn opts(&self) -> Opts {
        OptsBuilder::new()
            .ip_or_hostname(Some("127.0.0.1"))
            .tcp_port(self.port)
            .user(Some("root"))
            .db_name(Some(&self.db_name))
            .into()
    }
}

fn server_registry() -> &'static Mutex<HashMap<PathBuf, Arc<Mutex<DoltServer>>>> {
    static REG: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<DoltServer>>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

fn ensure_server(repo_path: &Path) -> Result<Arc<Mutex<DoltServer>>> {
    let canonical = std::fs::canonicalize(repo_path).unwrap_or_else(|_| repo_path.to_path_buf());
    let mut registry = server_registry().lock().unwrap();

    if let Some(entry) = registry.get(&canonical) {
        let mut srv = entry.lock().unwrap();
        if srv.server.try_wait().ok().flatten().is_none() {
            srv.ref_count += 1;
            return Ok(Arc::clone(entry));
        }
        drop(srv);
        registry.remove(&canonical);
    }

    let dolt_bin = find_dolt_bin()?;
    let port = find_free_port()?;

    let config_yaml =
        format!("behavior:\n  autocommit: false\nlistener:\n  host: 127.0.0.1\n  port: {port}\n");
    std::fs::write(repo_path.join("config.yaml"), config_yaml.as_bytes())
        .context("failed to write dolt config.yaml")?;

    let config_path = repo_path.join("config.yaml");
    let mut server = Command::new(&dolt_bin)
        .args(["sql-server", "--config", &config_path.to_string_lossy()])
        .current_dir(repo_path)
        .stdout(Stdio::null())
        .stderr({
            let log_path = repo_path.join("sql-server.log");
            std::fs::File::create(&log_path)
                .map(Stdio::from)
                .unwrap_or(Stdio::null())
        })
        .spawn()
        .with_context(|| format!("failed to start dolt sql-server in {}", repo_path.display()))?;

    let db_name = repo_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("dolt")
        .to_string();

    let opts: Opts = OptsBuilder::new()
        .ip_or_hostname(Some("127.0.0.1"))
        .tcp_port(port)
        .user(Some("root"))
        .db_name(Some(&db_name))
        .into();

    let mut connected = false;
    let mut last_err = None;
    for i in 0..80 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Some(status) = server.try_wait().ok().flatten() {
            let log_path = repo_path.join("sql-server.log");
            let stderr_msg = std::fs::read_to_string(&log_path).unwrap_or_default();
            bail!("dolt sql-server exited early with {status} on port {port}: {stderr_msg}");
        }
        match Conn::new(opts.clone()) {
            Ok(_c) => {
                connected = true;
                break;
            }
            Err(e) => {
                last_err = Some(e);
                if i % 20 == 19 {
                    eprintln!(
                        "[subcontext] Waiting for dolt sql-server on port {port}... ({:.1}s)",
                        (i + 1) as f64 * 0.1
                    );
                }
            }
        }
    }

    if !connected {
        let log_path = repo_path.join("sql-server.log");
        let stderr_msg = std::fs::read_to_string(&log_path).unwrap_or_default();
        let _ = server.kill();
        let _ = server.wait();
        bail!(
            "failed to connect to dolt sql-server on port {port} after 8s: {}\nserver log: {stderr_msg}",
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown error".to_string())
        );
    }

    let entry = Arc::new(Mutex::new(DoltServer {
        server,
        port,
        db_name,
        ref_count: 1,
    }));
    registry.insert(canonical, Arc::clone(&entry));
    Ok(entry)
}

fn release_server(repo_path: &Path) {
    let canonical = std::fs::canonicalize(repo_path).unwrap_or_else(|_| repo_path.to_path_buf());
    let mut registry = server_registry().lock().unwrap();
    let should_remove = if let Some(entry) = registry.get(&canonical) {
        let mut srv = entry.lock().unwrap();
        srv.ref_count = srv.ref_count.saturating_sub(1);
        if srv.ref_count == 0 {
            let _ = srv.server.kill();
            let _ = srv.server.wait();
            true
        } else {
            false
        }
    } else {
        false
    };
    if should_remove {
        registry.remove(&canonical);
    }
}

// ─── DoltConnection ──────────────────────────────────────────────────

pub struct DoltConnection {
    conn: Conn,
    repo_path: PathBuf,
}

impl Drop for DoltConnection {
    fn drop(&mut self) {
        release_server(&self.repo_path);
    }
}

impl DoltConnection {
    pub fn open(repo_path: &Path) -> Result<Self> {
        let entry = ensure_server(repo_path)?;
        let opts = entry.lock().unwrap().opts();
        let conn = Conn::new(opts).with_context(|| {
            format!(
                "failed to create MySQL connection to dolt for {}",
                repo_path.display()
            )
        })?;
        Ok(Self {
            conn,
            repo_path: repo_path.to_path_buf(),
        })
    }

    pub fn execute(&mut self, sql: &str, params: &[&str]) -> Result<()> {
        let resolved = substitute_params(sql, params);
        self.conn
            .query_drop(&resolved)
            .with_context(|| format!("dolt sql failed: {resolved}"))?;
        Ok(())
    }

    pub fn execute_batch(&mut self, sql: &str) -> Result<()> {
        self.conn
            .query_drop(sql)
            .with_context(|| format!("dolt sql batch failed: {sql}"))?;
        Ok(())
    }

    pub fn query_row<T, F>(&mut self, sql: &str, params: &[&str], f: F) -> Result<Option<T>>
    where
        F: FnOnce(&DoltRow) -> Result<T>,
    {
        let resolved = substitute_params(sql, params);
        let rows = self.run_query(&resolved)?;
        if rows.is_empty() {
            return Ok(None);
        }
        let row = DoltRow { columns: &rows[0] };
        Ok(Some(f(&row)?))
    }

    pub fn query_map<T, F>(&mut self, sql: &str, params: &[&str], f: F) -> Result<Vec<T>>
    where
        F: Fn(&DoltRow) -> Result<T>,
    {
        let resolved = substitute_params(sql, params);
        let rows = self.run_query(&resolved)?;
        let mut results = Vec::new();
        for row_data in &rows {
            let row = DoltRow { columns: row_data };
            results.push(f(&row)?);
        }
        Ok(results)
    }

    pub fn commit(&mut self, message: &str) -> Result<String> {
        self.conn
            .query_drop("CALL DOLT_ADD('-A')")
            .context("dolt add failed")?;
        let escaped_msg = message.replace('\'', "''");
        self.conn
            .query_drop(format!(
                "CALL DOLT_COMMIT('--allow-empty', '-m', '{escaped_msg}', \
                 '--author', 'subcontext <subcontext@local>')"
            ))
            .context("dolt commit failed")?;
        self.head_commit()
    }

    pub fn head_commit(&mut self) -> Result<String> {
        let result: Option<String> = self
            .conn
            .query_first("SELECT commit_hash FROM dolt_log LIMIT 1")
            .context("failed to query dolt_log")?;
        result.context("no commits in dolt repo")
    }

    fn run_query(&mut self, sql: &str) -> Result<Vec<Vec<Value>>> {
        let result: Vec<mysql::Row> = self
            .conn
            .query(sql)
            .with_context(|| format!("dolt sql failed: {sql}"))?;
        let mut rows = Vec::new();
        for mysql_row in &result {
            let mut values = Vec::new();
            for i in 0..mysql_row.len() {
                let val: mysql::Value = mysql_row.get(i).unwrap_or(mysql::Value::NULL);
                values.push(mysql_value_to_json(&val));
            }
            rows.push(values);
        }
        Ok(rows)
    }
}

fn mysql_value_to_json(val: &mysql::Value) -> Value {
    match val {
        mysql::Value::NULL => Value::Null,
        mysql::Value::Int(i) => serde_json::json!(*i),
        mysql::Value::UInt(u) => serde_json::json!(*u),
        mysql::Value::Float(f) => serde_json::json!(*f),
        mysql::Value::Double(d) => serde_json::json!(*d),
        mysql::Value::Bytes(b) => Value::String(String::from_utf8_lossy(b).to_string()),
        mysql::Value::Date(y, m, d, h, mi, s, _us) => {
            Value::String(format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}"))
        }
        mysql::Value::Time(neg, d, h, mi, s, _us) => {
            let sign = if *neg { "-" } else { "" };
            Value::String(format!("{sign}{d}d {h:02}:{mi:02}:{s:02}"))
        }
    }
}

// ─── DoltRow ─────────────────────────────────────────────────────────

pub struct DoltRow<'a> {
    columns: &'a [Value],
}

impl<'a> DoltRow<'a> {
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
    format!("'{}'", s.replace('\'', "''"))
}

// ─── Dolt commit tracking ───────────────────────────────────────────

const DOLT_HEAD_FILE: &str = "dolt_head";

pub fn save_dolt_head(backend: &dyn Backend, state: &Path, dolt_commit: &str) -> Result<()> {
    let head_file = state.join(DOLT_HEAD_FILE);
    backend.write(&head_file, dolt_commit.as_bytes())?;
    Ok(())
}

#[allow(dead_code)]
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

pub fn create_dolt_schema(conn: &mut DoltConnection) -> Result<()> {
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

pub fn create_dolt_global_schema(conn: &mut DoltConnection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS config (
             key_name   VARCHAR(255) PRIMARY KEY,
             value      TEXT NOT NULL
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
