//! Backend abstraction over git + filesystem operations.
//!
//! All side effects (shelling out to git, reading/writing files) go through
//! the [`Backend`] trait. Production code uses [`SystemBackend`]; tests can
//! swap in a mock implementation.

use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

/// A single git invocation: the args, the working directory, and any
/// environment variables to set or unset before running `git`.
pub struct GitInvocation<'a> {
    pub args: &'a [&'a str],
    pub cwd: &'a Path,
    pub env_set: &'a [(&'a str, &'a OsStr)],
    pub env_remove: &'a [&'a str],
}

impl<'a> GitInvocation<'a> {
    /// Simple invocation: just args + cwd, no env changes.
    pub fn new(args: &'a [&'a str], cwd: &'a Path) -> Self {
        Self {
            args,
            cwd,
            env_set: &[],
            env_remove: &[],
        }
    }
}

/// Abstraction over all git + filesystem side effects.
///
/// Keep the surface narrow and close to the underlying primitives — higher
/// level helpers live in `git.rs` / `overlay.rs` and are built on top of this
/// trait.
pub trait Backend {
    // ─── Git ─────────────────────────────────────────────────────────

    /// Run `git` with the given invocation. Returns stdout trimmed.
    /// Returns an error if git exits with a non-zero status.
    fn git(&self, inv: &GitInvocation<'_>) -> Result<String>;

    // ─── Filesystem ──────────────────────────────────────────────────

    fn read_to_string(&self, path: &Path) -> Result<String>;
    fn write(&self, path: &Path, contents: &[u8]) -> Result<()>;
    fn create_dir_all(&self, path: &Path) -> Result<()>;
    fn copy(&self, from: &Path, to: &Path) -> Result<u64>;
    fn remove_file(&self, path: &Path) -> Result<()>;
    fn remove_dir(&self, path: &Path) -> Result<()>;
    fn remove_dir_all(&self, path: &Path) -> Result<()>;
    fn exists(&self, path: &Path) -> bool;
    fn is_file(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
    fn canonicalize(&self, path: &Path) -> Result<PathBuf>;
    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>>;

    /// Unix file mode bits (from stat). Returns 0 on non-unix.
    fn metadata_mode(&self, path: &Path) -> Result<u32>;
    /// Set unix mode bits. No-op on non-unix.
    fn set_permissions_mode(&self, path: &Path, mode: u32) -> Result<()>;

    // ─── Process / environment ───────────────────────────────────────

    /// Absolute path to the currently running executable.
    fn current_exe(&self) -> Result<PathBuf>;

    /// Run an arbitrary executable (used to invoke backed-up user hooks).
    fn run_command(&self, program: &Path, args: &[&str], cwd: &Path) -> Result<ExitStatus>;
}

// ───────────────────────────────────────────────────────────────────────
// Production implementation
// ───────────────────────────────────────────────────────────────────────

/// Real backend: shells out to `git` and uses `std::fs` / `std::process`.
pub struct SystemBackend;

impl Backend for SystemBackend {
    fn git(&self, inv: &GitInvocation<'_>) -> Result<String> {
        let mut cmd = Command::new("git");
        cmd.args(inv.args).current_dir(inv.cwd);
        for (k, v) in inv.env_set {
            cmd.env(k, v);
        }
        for k in inv.env_remove {
            cmd.env_remove(k);
        }

        let output = cmd
            .output()
            .with_context(|| format!("failed to execute git {}", inv.args.join(" ")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "git {} failed (exit {}): {}",
                inv.args.join(" "),
                output.status,
                stderr.trim()
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn read_to_string(&self, path: &Path) -> Result<String> {
        std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))
    }

    fn write(&self, path: &Path, contents: &[u8]) -> Result<()> {
        std::fs::write(path, contents)
            .with_context(|| format!("failed to write {}", path.display()))
    }

    fn create_dir_all(&self, path: &Path) -> Result<()> {
        std::fs::create_dir_all(path)
            .with_context(|| format!("failed to create directory {}", path.display()))
    }

    fn copy(&self, from: &Path, to: &Path) -> Result<u64> {
        std::fs::copy(from, to)
            .with_context(|| format!("failed to copy {} → {}", from.display(), to.display()))
    }

    fn remove_file(&self, path: &Path) -> Result<()> {
        std::fs::remove_file(path)
            .with_context(|| format!("failed to remove file {}", path.display()))
    }

    fn remove_dir(&self, path: &Path) -> Result<()> {
        std::fs::remove_dir(path)
            .with_context(|| format!("failed to remove directory {}", path.display()))
    }

    fn remove_dir_all(&self, path: &Path) -> Result<()> {
        std::fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove directory tree {}", path.display()))
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn canonicalize(&self, path: &Path) -> Result<PathBuf> {
        path.canonicalize()
            .with_context(|| format!("failed to canonicalize {}", path.display()))
    }

    fn read_dir(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(path)
            .with_context(|| format!("failed to read directory {}", path.display()))?
        {
            entries.push(entry?.path());
        }
        Ok(entries)
    }

    fn metadata_mode(&self, path: &Path) -> Result<u32> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let md = std::fs::metadata(path)
                .with_context(|| format!("failed to stat {}", path.display()))?;
            Ok(md.permissions().mode())
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Ok(0)
        }
    }

    fn set_permissions_mode(&self, path: &Path, mode: u32) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path)
                .with_context(|| format!("failed to stat {}", path.display()))?
                .permissions();
            perms.set_mode(mode);
            std::fs::set_permissions(path, perms)
                .with_context(|| format!("failed to chmod {}", path.display()))?;
            Ok(())
        }
        #[cfg(not(unix))]
        {
            let _ = (path, mode);
            Ok(())
        }
    }

    fn current_exe(&self) -> Result<PathBuf> {
        std::env::current_exe().context("failed to resolve current executable path")
    }

    fn run_command(&self, program: &Path, args: &[&str], cwd: &Path) -> Result<ExitStatus> {
        Command::new(program)
            .args(args)
            .current_dir(cwd)
            .status()
            .with_context(|| format!("failed to run {}", program.display()))
    }
}
