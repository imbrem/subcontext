use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Directory containing a copy of the `subcontext` binary, created once per
/// process. This is the *only* custom entry on PATH during tests, simulating
/// an environment where only `subcontext` (and system tools) are installed.
fn test_bin_dir() -> &'static PathBuf {
    use std::sync::OnceLock;
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let src = env!("CARGO_BIN_EXE_subcontext");
        let dir = std::env::temp_dir().join(format!("subcontext-bin-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::copy(src, dir.join("subcontext")).unwrap();

        // Ensure the copy is executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(dir.join("subcontext")).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(dir.join("subcontext"), perms).unwrap();
        }

        dir
    })
}

/// Build a PATH that is: <test_bin_dir>:<system PATH>.
/// The hook's `exec subcontext` will resolve to our copied binary.
fn test_path() -> OsString {
    let mut path = OsString::from(test_bin_dir());
    if let Ok(existing) = std::env::var("PATH") {
        path.push(":");
        path.push(existing);
    }
    path
}

/// Create a temp dir with a fresh git repo, returning its path.
fn make_test_repo() -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("subcontext-test-{}-{}", std::process::id(), id));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();

    git(&dir, &["init"]);
    git(&dir, &["commit", "--allow-empty", "-m", "init"]);

    dir
}

fn cleanup(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .env("PATH", test_path())
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn subcontext(cwd: &Path, args: &[&str]) -> std::process::Output {
    let bin = test_bin_dir().join("subcontext");
    Command::new(bin)
        .args(args)
        .env("PATH", test_path())
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn subcontext_ok(cwd: &Path, args: &[&str]) -> String {
    let out = subcontext(cwd, args);
    assert!(
        out.status.success(),
        "subcontext {} failed (exit {}):\nstdout: {}\nstderr: {}",
        args.join(" "),
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

// ─── Install ────────────────────────────────────────────────────────

#[test]
fn install_creates_expected_structure() {
    let root = make_test_repo();

    subcontext_ok(&root, &["install"]);

    // .subcontext/ exists and is a git repo
    assert!(root.join(".subcontext/.git").is_dir());

    // Config worktree is mounted
    assert!(root.join(".subcontext/.mnt/config").is_dir());

    // Claude settings were written
    let settings = fs::read_to_string(root.join(".claude/settings.local.json")).unwrap();
    assert!(settings.contains("subcontext startup"));

    // Settings were copied to config mount
    assert!(
        root.join(".subcontext/.mnt/config/agents/claude/settings.local.json")
            .exists()
    );

    // Hook dispatcher was installed
    let hook = fs::read_to_string(root.join(".git/hooks/post-checkout")).unwrap();
    assert!(hook.contains("subcontext _hook post-checkout"));

    // Host exclude contains .subcontext/
    let exclude = fs::read_to_string(root.join(".git/info/exclude")).unwrap();
    assert!(exclude.contains(".subcontext/"));

    // Context repo exclude contains .mnt/ and .tmp/
    let ctx_exclude = fs::read_to_string(root.join(".subcontext/.git/info/exclude")).unwrap();
    assert!(ctx_exclude.contains(".mnt/"));
    assert!(ctx_exclude.contains(".tmp/"));

    // Main worktree is on worktrees/main
    let branch = git(
        &root.join(".subcontext"),
        &["symbolic-ref", "--short", "HEAD"],
    );
    assert_eq!(branch, "worktrees/main");

    // Config branch exists
    let branches = git(&root.join(".subcontext"), &["branch", "--list", "config"]);
    assert!(branches.contains("config"));

    cleanup(&root);
}

#[test]
fn install_refuses_if_already_exists() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let out = subcontext(&root, &["install"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("already exists"));

    cleanup(&root);
}

#[test]
fn install_preserves_existing_claude_settings() {
    let root = make_test_repo();

    // Write pre-existing settings
    fs::create_dir_all(root.join(".claude")).unwrap();
    fs::write(
        root.join(".claude/settings.local.json"),
        r#"{"myCustomKey": true}"#,
    )
    .unwrap();

    subcontext_ok(&root, &["install"]);

    let settings = fs::read_to_string(root.join(".claude/settings.local.json")).unwrap();
    assert!(settings.contains("myCustomKey"));
    assert!(settings.contains("subcontext startup"));

    cleanup(&root);
}

#[test]
fn install_backs_up_existing_hooks() {
    let root = make_test_repo();

    // Write a pre-existing executable hook
    let hook_path = root.join(".git/hooks/post-checkout");
    fs::write(&hook_path, "#!/bin/sh\necho old-hook\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&hook_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook_path, perms).unwrap();
    }

    subcontext_ok(&root, &["install"]);

    let backup = root.join(".subcontext/.mnt/config/hooks/old/post-checkout");
    assert!(backup.exists());
    let content = fs::read_to_string(backup).unwrap();
    assert!(content.contains("old-hook"));

    cleanup(&root);
}

// ─── Startup ────────────────────────────────────────────────────────

#[test]
fn startup_silent_without_task() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let stdout = subcontext_ok(&root, &["startup"]);
    assert!(stdout.is_empty());

    cleanup(&root);
}

#[test]
fn startup_prints_task_as_json() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    fs::write(root.join(".subcontext/TASK.md"), "Fix the login bug\n").unwrap();

    let stdout = subcontext_ok(&root, &["startup"]);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be valid JSON");
    assert_eq!(parsed["continue"], true);

    let ctx = parsed["additionalContext"].as_str().unwrap();
    assert!(ctx.contains("Active worktree: main"));
    assert!(ctx.contains("Current task:"));
    assert!(ctx.contains("Fix the login bug"));

    cleanup(&root);
}

// ─── Post-checkout hook ─────────────────────────────────────────────

#[test]
fn hook_creates_new_worktree_branch_on_checkout() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Create and switch to a new branch — the real hook fires via PATH
    git(&root, &["checkout", "-b", "feature/widgets"]);

    // Context repo should now be on worktrees/feature-widgets
    let branch = git(
        &root.join(".subcontext"),
        &["symbolic-ref", "--short", "HEAD"],
    );
    assert_eq!(branch, "worktrees/feature-widgets");

    cleanup(&root);
}

#[test]
fn hook_switches_back_to_existing_worktree_branch() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Write a task on main's context
    fs::write(root.join(".subcontext/TASK.md"), "main task\n").unwrap();
    git(&root.join(".subcontext"), &["add", "TASK.md"]);
    git(&root.join(".subcontext"), &["commit", "-m", "add task"]);

    // Switch to a new branch — hook fires automatically
    git(&root, &["checkout", "-b", "other"]);
    assert!(!root.join(".subcontext/TASK.md").exists());

    // Switch back to main — hook fires automatically
    git(&root, &["checkout", "main"]);
    let task = fs::read_to_string(root.join(".subcontext/TASK.md")).unwrap();
    assert_eq!(task, "main task\n");

    cleanup(&root);
}

#[test]
fn hook_ignores_file_checkouts() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // flag=0 means file checkout — should be a no-op
    let branch_before = git(
        &root.join(".subcontext"),
        &["symbolic-ref", "--short", "HEAD"],
    );
    subcontext_ok(&root, &["_hook", "post-checkout", "a", "b", "0"]);
    let branch_after = git(
        &root.join(".subcontext"),
        &["symbolic-ref", "--short", "HEAD"],
    );
    assert_eq!(branch_before, branch_after);

    cleanup(&root);
}

#[test]
fn hook_never_fails_fatally() {
    // Running the hook outside a subcontext project should exit 0
    let root = make_test_repo();
    let out = subcontext(&root, &["_hook", "post-checkout", "a", "b", "1"]);
    assert!(out.status.success());

    cleanup(&root);
}

#[test]
fn hook_propagates_old_hook_failure() {
    let root = make_test_repo();

    // Write a pre-existing hook that always fails
    let hook_dir = root.join(".git/hooks");
    fs::create_dir_all(&hook_dir).unwrap();
    let hook_path = hook_dir.join("post-checkout");
    fs::write(&hook_path, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&hook_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook_path, perms).unwrap();
    }

    subcontext_ok(&root, &["install"]);

    // Old hook was backed up
    assert!(
        root.join(".subcontext/.mnt/config/hooks/old/post-checkout")
            .exists()
    );

    // Invoke the hook directly — should exit non-zero because the old hook fails
    let out = subcontext(&root, &["_hook", "post-checkout", "abc123", "def456", "1"]);
    assert!(
        !out.status.success(),
        "hook should propagate old hook failure, but exited successfully"
    );

    cleanup(&root);
}

// ─── Branch sanitization ────────────────────────────────────────────

#[test]
fn hook_sanitizes_slashes_in_branch_names() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    git(&root, &["checkout", "-b", "feat/nested/deep"]);

    let branch = git(
        &root.join(".subcontext"),
        &["symbolic-ref", "--short", "HEAD"],
    );
    assert_eq!(branch, "worktrees/feat-nested-deep");

    cleanup(&root);
}
