use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Directory containing a copy of the `subcontext` binary.
fn test_bin_dir() -> &'static PathBuf {
    use std::sync::OnceLock;
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let src = env!("CARGO_BIN_EXE_subcontext");
        let dir = std::env::temp_dir().join(format!("subcontext-bin-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::copy(src, dir.join("subcontext")).unwrap();

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

fn test_path() -> OsString {
    let mut path = OsString::from(test_bin_dir());
    if let Ok(existing) = std::env::var("PATH") {
        path.push(":");
        path.push(existing);
    }
    path
}

fn test_env() -> Vec<(OsString, OsString)> {
    vec![
        (OsString::from("PATH"), test_path()),
        (OsString::from("GIT_AUTHOR_NAME"), OsString::from("Test")),
        (
            OsString::from("GIT_AUTHOR_EMAIL"),
            OsString::from("test@test.com"),
        ),
        (OsString::from("GIT_COMMITTER_NAME"), OsString::from("Test")),
        (
            OsString::from("GIT_COMMITTER_EMAIL"),
            OsString::from("test@test.com"),
        ),
        (
            OsString::from("GIT_CONFIG_GLOBAL"),
            OsString::from("/dev/null"),
        ),
        (
            OsString::from("GIT_CONFIG_SYSTEM"),
            OsString::from("/dev/null"),
        ),
        // Allow file:// transport for local submodule clones in tests
        (OsString::from("GIT_CONFIG_COUNT"), OsString::from("1")),
        (
            OsString::from("GIT_CONFIG_KEY_0"),
            OsString::from("protocol.file.allow"),
        ),
        (
            OsString::from("GIT_CONFIG_VALUE_0"),
            OsString::from("always"),
        ),
    ]
}

fn make_test_repo() -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("subcontext-test-{}-{}", std::process::id(), id));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();

    git(&dir, &["-c", "init.defaultBranch=main", "init"]);
    git(&dir, &["commit", "--allow-empty", "-m", "init"]);

    dir
}

fn cleanup(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .envs(test_env())
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
        .envs(test_env())
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

    // Bare repo exists
    assert!(root.join(".git/.subcontext/repo/HEAD").exists());

    // Config worktree is mounted
    assert!(root.join(".git/.subcontext/config").is_dir());

    // Work worktree is mounted
    assert!(root.join(".git/.subcontext/work").is_dir());

    // Claude settings were written
    let settings = fs::read_to_string(root.join(".claude/settings.local.json")).unwrap();
    assert!(settings.contains("git subcontext startup"));

    // Install should NOT write .mcp.json into the host repo
    assert!(!root.join(".mcp.json").exists());

    // Hook dispatchers installed
    let pc_hook = fs::read_to_string(root.join(".git/hooks/post-checkout")).unwrap();
    assert!(pc_hook.contains("git subcontext _hook post-checkout"));

    let pcm_hook = fs::read_to_string(root.join(".git/hooks/post-commit")).unwrap();
    assert!(pcm_hook.contains("git subcontext _hook post-commit"));

    // Git alias should be configured
    let alias = git(&root, &["config", "alias.subcontext"]);
    assert!(
        alias.contains("subcontext"),
        "git alias should point to subcontext binary"
    );

    // Overlay branch exists
    let branches = git_in_repo(&root, &["branch", "--list", "overlay/main"]);
    assert!(branches.contains("overlay/main"));

    // Config branch exists
    let branches = git_in_repo(&root, &["branch", "--list", "config"]);
    assert!(branches.contains("config"));

    cleanup(&root);
}

#[test]
fn install_reinstall_succeeds() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Re-running install should succeed
    let out = subcontext(&root, &["install"]);
    assert!(
        out.status.success(),
        "reinstall failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("re-installing"));

    cleanup(&root);
}

#[test]
fn install_preserves_existing_claude_settings() {
    let root = make_test_repo();

    fs::create_dir_all(root.join(".claude")).unwrap();
    fs::write(
        root.join(".claude/settings.local.json"),
        r#"{"myCustomKey": true}"#,
    )
    .unwrap();

    subcontext_ok(&root, &["install"]);

    let settings = fs::read_to_string(root.join(".claude/settings.local.json")).unwrap();
    assert!(settings.contains("myCustomKey"));
    assert!(settings.contains("git subcontext startup"));

    cleanup(&root);
}

#[test]
fn install_backs_up_existing_hooks() {
    let root = make_test_repo();

    let hook_path = root.join(".git/hooks/post-checkout");
    fs::create_dir_all(root.join(".git/hooks")).unwrap();
    fs::write(&hook_path, "#!/bin/sh\necho old-hook\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&hook_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook_path, perms).unwrap();
    }

    subcontext_ok(&root, &["install"]);

    let backup = root.join(".git/.subcontext/config/hooks/old/post-checkout");
    assert!(backup.exists());
    let content = fs::read_to_string(backup).unwrap();
    assert!(content.contains("old-hook"));

    cleanup(&root);
}

// ─── Overlay add / save / switch ─────────────────────────────────────

#[test]
fn add_and_save_overlay_file() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Create a file and add to overlay
    fs::write(root.join("NOTES.md"), "private notes\n").unwrap();
    subcontext_ok(&root, &["add", "NOTES.md"]);
    subcontext_ok(&root, &["save", "-m", "add notes"]);

    // File should exist in work/
    assert!(root.join(".git/.subcontext/work/NOTES.md").exists());

    // File should be excluded from git status
    let status = git(&root, &["status", "--porcelain"]);
    assert!(
        !status.contains("NOTES.md"),
        "NOTES.md should be excluded from git status, got: {status}"
    );

    cleanup(&root);
}

#[test]
fn overlay_files_switch_with_branches() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Add a file on main
    fs::write(root.join("NOTES.md"), "main notes\n").unwrap();
    subcontext_ok(&root, &["add", "NOTES.md"]);
    subcontext_ok(&root, &["save", "-m", "main notes"]);

    // Switch to new branch — overlay forks from main
    git(&root, &["checkout", "-b", "feature"]);

    // NOTES.md should be inherited from main's overlay
    let content = fs::read_to_string(root.join("NOTES.md")).unwrap();
    assert_eq!(
        content, "main notes\n",
        "new branch should inherit parent overlay"
    );

    // Overwrite with different content on feature
    fs::write(root.join("NOTES.md"), "feature notes\n").unwrap();
    subcontext_ok(&root, &["add", "NOTES.md"]);
    subcontext_ok(&root, &["save", "-m", "feature notes"]);

    // Switch back to main
    git(&root, &["checkout", "main"]);

    // Should see main notes (not feature notes)
    let content = fs::read_to_string(root.join("NOTES.md")).unwrap();
    assert_eq!(content, "main notes\n");

    cleanup(&root);
}

#[test]
fn new_branch_from_empty_overlay_starts_empty() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Don't add any overlay files on main — switch to feature
    git(&root, &["checkout", "-b", "feature"]);

    // Overlay should still be empty
    let files = fs::read_to_string(root.join(".git/.subcontext/work/.gitkeep")).ok();
    assert!(
        files.is_none(),
        "new branch from empty overlay should be empty"
    );

    // No overlay files should be in root (ignore .claude/ which is created by install)
    let status = git(&root, &["status", "--porcelain"]);
    let non_claude: Vec<&str> = status.lines().filter(|l| !l.contains(".claude/")).collect();
    assert!(
        non_claude.is_empty(),
        "should have no untracked overlay files, got: {status}"
    );

    cleanup(&root);
}

#[test]
fn overlay_wins_over_main_repo() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Create a file tracked by main repo
    fs::write(root.join("shared.txt"), "main version\n").unwrap();
    git(&root, &["add", "shared.txt"]);
    git(&root, &["commit", "-m", "add shared"]);

    // Override with overlay
    fs::write(root.join("shared.txt"), "overlay version\n").unwrap();
    subcontext_ok(&root, &["add", "shared.txt"]);
    subcontext_ok(&root, &["save", "-m", "overlay shared"]);

    // File should show overlay version
    let content = fs::read_to_string(root.join("shared.txt")).unwrap();
    assert_eq!(content, "overlay version\n");

    // git status should NOT show shared.txt as modified (skip-worktree)
    let status = git(&root, &["status", "--porcelain"]);
    assert!(
        !status.contains("shared.txt"),
        "shared.txt should be hidden from git status via skip-worktree, got: {status}"
    );

    cleanup(&root);
}

#[test]
fn remove_restores_main_repo_version() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Create a file tracked by main repo
    fs::write(root.join("shared.txt"), "main version\n").unwrap();
    git(&root, &["add", "shared.txt"]);
    git(&root, &["commit", "-m", "add shared"]);

    // Override with overlay
    fs::write(root.join("shared.txt"), "overlay version\n").unwrap();
    subcontext_ok(&root, &["add", "shared.txt"]);

    // Remove from overlay
    subcontext_ok(&root, &["remove", "shared.txt"]);

    // Should restore main version
    let content = fs::read_to_string(root.join("shared.txt")).unwrap();
    assert_eq!(content, "main version\n");

    cleanup(&root);
}

#[test]
fn remove_deletes_overlay_only_file() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    fs::write(root.join("NOTES.md"), "notes\n").unwrap();
    subcontext_ok(&root, &["add", "NOTES.md"]);

    subcontext_ok(&root, &["remove", "NOTES.md"]);

    assert!(!root.join("NOTES.md").exists());

    cleanup(&root);
}

// ─── Post-checkout hook ─────────────────────────────────────────────

#[test]
fn hook_creates_new_overlay_branch_on_checkout() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    git(&root, &["checkout", "-b", "feature/widgets"]);

    // Overlay branch should exist
    let branches = git_in_repo(&root, &["branch", "--list", "overlay/feature-widgets"]);
    assert!(branches.contains("overlay/feature-widgets"));

    // Work/ should be on the new branch
    let branch = git(
        &root.join(".git/.subcontext/work"),
        &["symbolic-ref", "--short", "HEAD"],
    );
    assert_eq!(branch, "overlay/feature-widgets");

    cleanup(&root);
}

#[test]
fn hook_ignores_file_checkouts() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // flag=0 means file checkout — should be a no-op
    let branch_before = git(
        &root.join(".git/.subcontext/work"),
        &["symbolic-ref", "--short", "HEAD"],
    );
    subcontext_ok(&root, &["_hook", "post-checkout", "a", "b", "0"]);
    let branch_after = git(
        &root.join(".git/.subcontext/work"),
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

    assert!(
        root.join(".git/.subcontext/config/hooks/old/post-checkout")
            .exists()
    );

    let out = subcontext(&root, &["_hook", "post-checkout", "abc123", "def456", "1"]);
    assert!(
        !out.status.success(),
        "hook should propagate old hook failure"
    );

    cleanup(&root);
}

// ─── Post-commit hook ───────────────────────────────────────────────

#[test]
fn post_commit_auto_saves_overlay() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Add a file to overlay
    fs::write(root.join("NOTES.md"), "original\n").unwrap();
    subcontext_ok(&root, &["add", "NOTES.md"]);
    subcontext_ok(&root, &["save", "-m", "initial"]);

    // Modify the overlay file
    fs::write(root.join("NOTES.md"), "modified\n").unwrap();

    // Make a commit in the main repo (triggers post-commit hook)
    fs::write(root.join("dummy.txt"), "x").unwrap();
    git(&root, &["add", "dummy.txt"]);
    git(&root, &["commit", "-m", "trigger post-commit"]);

    // The overlay change should be auto-saved
    let work_content = fs::read_to_string(root.join(".git/.subcontext/work/NOTES.md")).unwrap();
    assert_eq!(work_content, "modified\n");

    cleanup(&root);
}

// ─── Uninstall ──────────────────────────────────────────────────────

#[test]
fn uninstall_cleans_up() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Add an overlay file
    fs::write(root.join("NOTES.md"), "notes\n").unwrap();
    subcontext_ok(&root, &["add", "NOTES.md"]);

    subcontext_ok(&root, &["uninstall"]);

    // Hooks should be gone
    assert!(!root.join(".git/hooks/post-checkout").exists());
    assert!(!root.join(".git/hooks/post-commit").exists());

    // Overlay file should be removed
    assert!(!root.join("NOTES.md").exists());

    // .git/.subcontext/ should be gone
    assert!(!root.join(".git/.subcontext").exists());

    // Settings should no longer contain subcontext
    let settings = fs::read_to_string(root.join(".claude/settings.local.json")).unwrap();
    assert!(!settings.contains("git subcontext startup"));

    // Uninstall should never have created a .mcp.json in the host repo
    assert!(!root.join(".mcp.json").exists());

    cleanup(&root);
}

#[test]
fn mcp_status_tool_returns_text() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Send init + tools/list + tools/call over stdin
    use std::io::Write;
    use std::process::Stdio;
    let bin = test_bin_dir().join("subcontext");
    let mut child = Command::new(bin)
        .arg("mcp")
        .envs(test_env())
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stdin = child.stdin.as_mut().unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#
    )
    .unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{{}}}}"#
    )
    .unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"subcontext_status","arguments":{{}}}}}}"#
    )
    .unwrap();
    drop(child.stdin.take());

    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Three responses, one per line
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 3, "expected 3 responses, got: {stdout}");
    assert!(lines[0].contains("protocolVersion"));
    assert!(lines[1].contains("subcontext_status"));
    assert!(lines[2].contains("Main repo:"));
    assert!(lines[2].contains("installed"));

    cleanup(&root);
}

#[test]
fn install_global_no_longer_writes_mcp_config() {
    // install --global should NOT touch ~/.claude.json (MCP installer removed).
    let fake_home = std::env::temp_dir().join(format!(
        "subcontext-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&fake_home).unwrap();

    let bin = test_bin_dir().join("subcontext");
    let out = Command::new(&bin)
        .args(["install", "--global"])
        .envs(test_env())
        .env("HOME", &fake_home)
        .current_dir(&fake_home)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "install --global failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let config_path = fake_home.join(".claude.json");
    assert!(
        !config_path.exists(),
        "~/.claude.json should NOT be created by install --global"
    );

    cleanup(&fake_home);
}

fn make_global_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "subcontext-global-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    dir
}

fn subcontext_with_global(cwd: &Path, global_path: &Path, args: &[&str]) -> std::process::Output {
    let bin = test_bin_dir().join("subcontext");
    Command::new(bin)
        .args(args)
        .envs(test_env())
        .env("GIT_SUBCONTEXT_PATH", global_path)
        .current_dir(cwd)
        .output()
        .unwrap()
}

#[test]
fn install_global_creates_subcontext_directory_with_user_kind() {
    let fake_home = std::env::temp_dir().join(format!(
        "subcontext-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&fake_home).unwrap();

    let global_path = make_global_dir();

    let out = subcontext_with_global(&fake_home, &global_path, &["install", "--global"]);
    assert!(
        out.status.success(),
        "install --global failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Nested dir structure mirrors .git/.subcontext
    assert!(
        global_path.join("global/repo/HEAD").exists(),
        "bare repo should be present"
    );
    assert!(global_path.join("global/config").is_dir());
    assert!(global_path.join("global/work").is_dir());
    assert!(global_path.join("global/state").is_dir());

    // subcontext.yaml has kind: system
    let yaml = fs::read_to_string(global_path.join("global/config/subcontext.yaml")).unwrap();
    assert!(yaml.contains("kind: system"), "yaml: {yaml}");
    assert!(yaml.contains("project_uuid:"));

    // state/tasks.db exists
    assert!(global_path.join("global/state/tasks.db").exists());

    // Re-running is idempotent.
    let out2 = subcontext_with_global(&fake_home, &global_path, &["install", "--global"]);
    assert!(out2.status.success());

    cleanup(&fake_home);
    cleanup(&global_path);
}

#[test]
fn local_install_registers_child_in_global() {
    let fake_home = std::env::temp_dir().join(format!(
        "subcontext-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&fake_home).unwrap();
    let global_path = make_global_dir();

    // Install global first.
    let out = subcontext_with_global(&fake_home, &global_path, &["install", "--global"]);
    assert!(out.status.success());

    // Install locally in a fresh git repo.
    let root = make_test_repo();
    let out = subcontext_with_global(&root, &global_path, &["install"]);
    assert!(
        out.status.success(),
        "local install failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Read the project UUID that was written locally.
    let yaml = fs::read_to_string(root.join(".git/.subcontext/config/subcontext.yaml")).unwrap();
    let project_uuid = yaml
        .lines()
        .find_map(|l| {
            l.strip_prefix("project_uuid:")
                .map(|s| s.trim().to_string())
        })
        .expect("project_uuid missing");

    // Verify the global bare repo now has an object/<uuid> branch.
    let global_repo = global_path.join("global/repo");
    let out = Command::new("git")
        .args([
            &format!("--git-dir={}", global_repo.display()),
            "show-ref",
            "--verify",
            &format!("refs/heads/object/{project_uuid}"),
        ])
        .envs(test_env())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "object branch missing: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The branch contains a single object.json with type "child" and child
    // data inlined under "data".
    let json = Command::new("git")
        .args([
            &format!("--git-dir={}", global_repo.display()),
            "show",
            &format!("object/{project_uuid}:object.json"),
        ])
        .envs(test_env())
        .output()
        .unwrap();
    assert!(json.status.success());
    let text = String::from_utf8_lossy(&json.stdout);
    assert!(text.contains("\"type\": \"child\""), "object.json: {text}");
    assert!(text.contains(&project_uuid), "object.json: {text}");
    assert!(
        text.contains("\"kind\": \"project\""),
        "object.json: {text}"
    );

    cleanup(&fake_home);
    cleanup(&global_path);
    cleanup(&root);
}

#[test]
fn task_add_global_uses_global_subcontext() {
    let fake_home = std::env::temp_dir().join(format!(
        "subcontext-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&fake_home).unwrap();
    let global_path = make_global_dir();

    let out = subcontext_with_global(&fake_home, &global_path, &["install", "--global"]);
    assert!(out.status.success());

    // Running `task add` outside any git repo (cwd=fake_home) should
    // automatically target the global subcontext.
    let out = subcontext_with_global(&fake_home, &global_path, &["task", "add", "review-plans"]);
    assert!(
        out.status.success(),
        "global task add failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // An object/<uuid> branch should exist in the global bare repo.
    let global_repo = global_path.join("global/repo");
    let branches = Command::new("git")
        .args([
            &format!("--git-dir={}", global_repo.display()),
            "branch",
            "--list",
            "object/*",
        ])
        .envs(test_env())
        .output()
        .unwrap();
    assert!(branches.status.success());
    let text = String::from_utf8_lossy(&branches.stdout);
    assert!(
        text.contains("object/"),
        "expected object/* branch, got: {text}"
    );

    // --global flag works from inside a git repo too.
    let root = make_test_repo();
    let out = subcontext_with_global(&root, &global_path, &["task", "--global", "add", "other"]);
    assert!(
        out.status.success(),
        "--global from repo failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    cleanup(&fake_home);
    cleanup(&global_path);
    cleanup(&root);
}

#[test]
fn uninstall_restores_original_hook() {
    let root = make_test_repo();

    let hook_path = root.join(".git/hooks/post-checkout");
    fs::create_dir_all(root.join(".git/hooks")).unwrap();
    fs::write(&hook_path, "#!/bin/sh\necho original-hook\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&hook_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook_path, perms).unwrap();
    }

    subcontext_ok(&root, &["install"]);
    subcontext_ok(&root, &["uninstall"]);

    let hook = fs::read_to_string(&hook_path).unwrap();
    assert!(hook.contains("original-hook"));
    assert!(!hook.contains("subcontext"));

    cleanup(&root);
}

#[test]
fn uninstall_preserves_other_settings() {
    let root = make_test_repo();

    fs::create_dir_all(root.join(".claude")).unwrap();
    fs::write(
        root.join(".claude/settings.local.json"),
        r#"{"myCustomKey": true}"#,
    )
    .unwrap();

    subcontext_ok(&root, &["install"]);
    subcontext_ok(&root, &["uninstall"]);

    let settings = fs::read_to_string(root.join(".claude/settings.local.json")).unwrap();
    assert!(settings.contains("myCustomKey"));
    assert!(!settings.contains("git subcontext startup"));

    cleanup(&root);
}

// ─── Startup ────────────────────────────────────────────────────────

#[test]
fn startup_is_noop() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let stdout = subcontext_ok(&root, &["startup", "--claude-code"]);
    assert!(stdout.is_empty());

    cleanup(&root);
}

// ─── Branch sanitization ────────────────────────────────────────────

#[test]
fn hook_sanitizes_slashes_in_branch_names() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    git(&root, &["checkout", "-b", "feat/nested/deep"]);

    let branches = git_in_repo(&root, &["branch", "--list", "overlay/feat-nested-deep"]);
    assert!(branches.contains("overlay/feat-nested-deep"));

    cleanup(&root);
}

// ─── Repair ─────────────────────────────────────────────────────────

#[test]
fn install_repair_backs_up_subcontext_hook() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    subcontext_ok(&root, &["install", "--repair"]);

    let backup = root.join(".git/.subcontext/config/hooks/backup/post-checkout");
    assert!(
        backup.exists(),
        "repair should create hooks/backup/post-checkout"
    );
    let content = fs::read_to_string(&backup).unwrap();
    assert!(content.contains("subcontext"));

    cleanup(&root);
}

#[test]
fn install_comment_mentioning_subcontext_is_not_detected_as_dispatcher() {
    let root = make_test_repo();

    let hook_path = root.join(".git/hooks/post-checkout");
    fs::create_dir_all(root.join(".git/hooks")).unwrap();
    fs::write(
        &hook_path,
        "#!/bin/sh\n# This hook was written before subcontext was installed\necho hello\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&hook_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook_path, perms).unwrap();
    }

    subcontext_ok(&root, &["install"]);

    let old = root.join(".git/.subcontext/config/hooks/old/post-checkout");
    assert!(
        old.exists(),
        "comment-only mention should be backed up to old/"
    );
    let content = fs::read_to_string(&old).unwrap();
    assert!(content.contains("echo hello"));

    cleanup(&root);
}

// ─── Auto-save on checkout ───────────────────────────────────────────

#[test]
fn checkout_auto_saves_unsaved_overlay_changes() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Add and save a file on main
    fs::write(root.join("NOTES.md"), "original\n").unwrap();
    subcontext_ok(&root, &["add", "NOTES.md"]);
    subcontext_ok(&root, &["save", "-m", "initial"]);

    // Modify the overlay file WITHOUT saving
    fs::write(root.join("NOTES.md"), "modified\n").unwrap();

    // Switch branch — should auto-save before unapply
    git(&root, &["checkout", "-b", "feature"]);

    // Switch back to main — should see the auto-saved changes
    git(&root, &["checkout", "main"]);

    let content = fs::read_to_string(root.join("NOTES.md")).unwrap();
    assert_eq!(
        content, "modified\n",
        "unsaved overlay changes should be preserved across checkout"
    );

    cleanup(&root);
}

// ─── Edge cases ─────────────────────────────────────────────────────

#[test]
fn add_nonexistent_file_fails() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let out = subcontext(&root, &["add", "nonexistent.txt"]);
    assert!(!out.status.success(), "adding nonexistent file should fail");

    cleanup(&root);
}

#[test]
fn add_nested_directory_file() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    fs::create_dir_all(root.join("docs/internal")).unwrap();
    fs::write(root.join("docs/internal/notes.md"), "nested\n").unwrap();

    subcontext_ok(&root, &["add", "docs/internal/notes.md"]);
    subcontext_ok(&root, &["save", "-m", "nested file"]);

    // Should exist in work/
    assert!(
        root.join(".git/.subcontext/work/docs/internal/notes.md")
            .exists()
    );

    // Should be excluded from git status
    let status = git(&root, &["status", "--porcelain"]);
    assert!(
        !status.contains("notes.md"),
        "nested overlay file should be excluded, got: {status}"
    );

    cleanup(&root);
}

#[test]
fn save_with_no_overlay_files_is_noop() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // save with no files should succeed silently
    subcontext_ok(&root, &["save", "-m", "empty"]);

    cleanup(&root);
}

#[test]
fn uninstall_is_idempotent() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);
    subcontext_ok(&root, &["uninstall"]);

    // Second uninstall should succeed (nothing to do)
    let out = subcontext(&root, &["uninstall"]);
    assert!(
        out.status.success(),
        "second uninstall should succeed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    cleanup(&root);
}

#[test]
fn remove_nested_cleans_empty_parents() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    fs::create_dir_all(root.join("a/b")).unwrap();
    fs::write(root.join("a/b/c.md"), "deep\n").unwrap();

    subcontext_ok(&root, &["add", "a/b/c.md"]);
    subcontext_ok(&root, &["remove", "a/b/c.md"]);

    // File should be gone
    assert!(!root.join("a/b/c.md").exists());
    // Empty parent dirs should be cleaned up
    assert!(!root.join("a/b").exists());
    assert!(!root.join("a").exists());

    cleanup(&root);
}

// ─── Orphan branches ─────────────────────────────────────────────────

#[test]
fn orphan_branch_gets_empty_overlay() {
    let root = make_test_repo();

    // Need a tracked file so the repo isn't empty
    fs::write(root.join("README"), "hello\n").unwrap();
    git(&root, &["add", "README"]);
    git(&root, &["commit", "-m", "add readme"]);

    subcontext_ok(&root, &["install"]);

    // Add overlay content on main
    fs::write(root.join("NOTES.md"), "main notes\n").unwrap();
    subcontext_ok(&root, &["add", "NOTES.md"]);
    subcontext_ok(&root, &["save", "-m", "main notes"]);

    // Create an orphan branch (unrelated history)
    git(&root, &["checkout", "--orphan", "orphan-branch"]);
    // git checkout --orphan leaves files staged; clear them
    git(&root, &["rm", "-rf", "."]);
    git(&root, &["commit", "--allow-empty", "-m", "orphan root"]);

    // The overlay should be empty — NOTES.md should NOT be inherited
    assert!(
        !root.join("NOTES.md").exists(),
        "orphan branch should NOT inherit overlay files from previous branch"
    );

    // The overlay branch should exist and be empty
    let branches = git_in_repo(&root, &["branch", "--list", "overlay/orphan-branch"]);
    assert!(branches.contains("overlay/orphan-branch"));

    cleanup(&root);
}

#[test]
fn checkout_to_unrelated_branch_gets_empty_overlay() {
    let root = make_test_repo();

    // Need a tracked file so the repo isn't empty
    fs::write(root.join("README"), "hello\n").unwrap();
    git(&root, &["add", "README"]);
    git(&root, &["commit", "-m", "add readme"]);

    subcontext_ok(&root, &["install"]);

    // Add overlay content on main
    fs::write(root.join("NOTES.md"), "main notes\n").unwrap();
    subcontext_ok(&root, &["add", "NOTES.md"]);
    subcontext_ok(&root, &["save", "-m", "main notes"]);

    // Create an orphan branch with commits (unrelated history)
    git(&root, &["checkout", "--orphan", "unrelated"]);
    git(&root, &["rm", "-rf", "."]);
    git(&root, &["commit", "--allow-empty", "-m", "unrelated root"]);
    git(
        &root,
        &["commit", "--allow-empty", "-m", "unrelated second"],
    );

    // Go back to main
    git(&root, &["checkout", "main"]);

    // Now check out the unrelated branch (it has commits but shares no history)
    git(&root, &["checkout", "unrelated"]);

    // Should still get an empty overlay (branches are unrelated)
    assert!(
        !root.join("NOTES.md").exists(),
        "checking out unrelated branch should not inherit overlay"
    );

    cleanup(&root);
}

// ─── Worktrees ──────────────────────────────────────────────────────

#[test]
fn worktree_gets_overlay_forked_from_main() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Add overlay content on main
    fs::write(root.join("NOTES.md"), "main notes\n").unwrap();
    subcontext_ok(&root, &["add", "NOTES.md"]);
    subcontext_ok(&root, &["save", "-m", "main notes"]);

    // Create a branch for the worktree
    git(&root, &["branch", "feature"]);

    // Create a worktree
    let wt_dir = root.parent().unwrap().join(format!(
        "subcontext-wt-{}-{}",
        std::process::id(),
        COUNTER.load(Ordering::SeqCst)
    ));
    git(
        &root,
        &["worktree", "add", &wt_dir.to_string_lossy(), "feature"],
    );

    // The overlay branch should have been created (forked from main's overlay)
    let branches = git_in_repo(&root, &["branch", "--list", "overlay/feature"]);
    assert!(
        branches.contains("overlay/feature"),
        "overlay/feature branch should be created for worktree"
    );

    // The overlay file should be applied in the worktree
    assert!(
        wt_dir.join("NOTES.md").exists(),
        "overlay file should be applied in worktree"
    );
    let content = fs::read_to_string(wt_dir.join("NOTES.md")).unwrap();
    assert_eq!(
        content, "main notes\n",
        "worktree overlay should inherit content from main"
    );

    // Per-worktree work dir should exist
    let wt_name = wt_dir.file_name().unwrap().to_string_lossy().to_string();
    let wt_work = root.join(".git/.subcontext/worktrees").join(&wt_name);
    assert!(
        wt_work.is_dir(),
        "per-worktree work directory should exist at .git/.subcontext/worktrees/{wt_name}"
    );

    // Clean up worktree
    git(
        &root,
        &["worktree", "remove", "--force", &wt_dir.to_string_lossy()],
    );
    cleanup(&wt_dir);
    cleanup(&root);
}

#[test]
fn worktree_overlay_is_independent_from_main() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Add overlay content on main
    fs::write(root.join("NOTES.md"), "main notes\n").unwrap();
    subcontext_ok(&root, &["add", "NOTES.md"]);
    subcontext_ok(&root, &["save", "-m", "main notes"]);

    // Create worktree
    git(&root, &["branch", "feature"]);
    let wt_dir = root.parent().unwrap().join(format!(
        "subcontext-wt2-{}-{}",
        std::process::id(),
        COUNTER.load(Ordering::SeqCst)
    ));
    git(
        &root,
        &["worktree", "add", &wt_dir.to_string_lossy(), "feature"],
    );

    // Modify the overlay in the worktree (write directly, then save via subcontext)
    fs::write(wt_dir.join("NOTES.md"), "feature notes\n").unwrap();

    // Main checkout should still have its own content
    let main_content = fs::read_to_string(root.join("NOTES.md")).unwrap();
    assert_eq!(
        main_content, "main notes\n",
        "main overlay should be unaffected by worktree changes"
    );

    // Clean up
    git(
        &root,
        &["worktree", "remove", "--force", &wt_dir.to_string_lossy()],
    );
    cleanup(&wt_dir);
    cleanup(&root);
}

// ─── Submodule ─────────────────────────────────────────────────────

/// Create a local bare repo that can be used as a submodule source.
fn make_submodule_source() -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("subcontext-sub-{}-{}", std::process::id(), id));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();

    // Create a regular repo with some content
    git(&dir, &["-c", "init.defaultBranch=main", "init"]);
    fs::write(dir.join("lib.rs"), "pub fn hello() {}\n").unwrap();
    git(&dir, &["add", "lib.rs"]);
    git(&dir, &["commit", "-m", "initial lib commit"]);

    // Clone as bare to use as remote source
    let bare_dir =
        std::env::temp_dir().join(format!("subcontext-sub-bare-{}-{}", std::process::id(), id));
    if bare_dir.exists() {
        fs::remove_dir_all(&bare_dir).unwrap();
    }
    git(
        &std::env::temp_dir(),
        &[
            "clone",
            "--bare",
            &dir.to_string_lossy(),
            &bare_dir.to_string_lossy(),
        ],
    );

    cleanup(&dir);
    bare_dir
}

#[test]
fn submodule_add_creates_submodule_in_overlay() {
    let root = make_test_repo();
    let sub_source = make_submodule_source();
    subcontext_ok(&root, &["install"]);

    // Add submodule
    subcontext_ok(
        &root,
        &[
            "submodule",
            "add",
            &sub_source.to_string_lossy(),
            "lib/mylib",
        ],
    );

    // Submodule files should be in checkout root
    assert!(
        root.join("lib/mylib/lib.rs").exists(),
        "submodule file should be copied to checkout root"
    );
    let content = fs::read_to_string(root.join("lib/mylib/lib.rs")).unwrap();
    assert!(content.contains("pub fn hello()"));

    // .gitmodules should exist in checkout root
    assert!(
        root.join(".gitmodules").exists(),
        ".gitmodules should be copied to checkout root"
    );

    // Submodule files should be in work dir
    assert!(root.join(".git/.subcontext/work/lib/mylib/lib.rs").exists());

    // Submodule dir should be excluded from host git status
    let status = git(&root, &["status", "--porcelain"]);
    assert!(
        !status.contains("lib/mylib"),
        "submodule dir should be excluded from git status, got: {status}"
    );

    // .gitmodules should also be excluded
    assert!(
        !status.contains(".gitmodules"),
        ".gitmodules should be excluded from git status, got: {status}"
    );

    cleanup(&sub_source);
    cleanup(&root);
}

#[test]
fn submodule_add_derives_path_from_url() {
    let root = make_test_repo();
    let sub_source = make_submodule_source();
    subcontext_ok(&root, &["install"]);

    // Add submodule without specifying path — should derive from URL
    let source_name = sub_source
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    subcontext_ok(&root, &["submodule", "add", &sub_source.to_string_lossy()]);

    // Submodule should be at the derived path
    assert!(
        root.join(&source_name).join("lib.rs").exists(),
        "submodule should be at path derived from URL: {source_name}"
    );

    cleanup(&sub_source);
    cleanup(&root);
}

#[test]
fn submodule_survives_branch_switch() {
    let root = make_test_repo();
    let sub_source = make_submodule_source();
    subcontext_ok(&root, &["install"]);

    // Add submodule on main
    subcontext_ok(
        &root,
        &[
            "submodule",
            "add",
            &sub_source.to_string_lossy(),
            "lib/mylib",
        ],
    );
    subcontext_ok(&root, &["save", "-m", "add submodule"]);

    // Switch to new branch — submodule should be inherited
    git(&root, &["checkout", "-b", "feature"]);

    assert!(
        root.join("lib/mylib/lib.rs").exists(),
        "submodule should be inherited on new branch"
    );

    // Switch back to main — submodule should still be there
    git(&root, &["checkout", "main"]);

    assert!(
        root.join("lib/mylib/lib.rs").exists(),
        "submodule should be present after switching back to main"
    );

    cleanup(&sub_source);
    cleanup(&root);
}

#[test]
fn submodule_remove_cleans_up() {
    let root = make_test_repo();
    let sub_source = make_submodule_source();
    subcontext_ok(&root, &["install"]);

    subcontext_ok(
        &root,
        &[
            "submodule",
            "add",
            &sub_source.to_string_lossy(),
            "lib/mylib",
        ],
    );

    // Verify it exists
    assert!(root.join("lib/mylib/lib.rs").exists());

    // Remove it
    subcontext_ok(&root, &["submodule", "remove", "lib/mylib"]);

    // Submodule files should be gone from checkout root
    assert!(
        !root.join("lib/mylib").exists(),
        "submodule dir should be removed from checkout root"
    );

    // Should not be in excludes anymore
    let status = git(&root, &["status", "--porcelain"]);
    assert!(
        !status.contains("lib/mylib"),
        "submodule should not appear in git status after removal"
    );

    cleanup(&sub_source);
    cleanup(&root);
}

#[test]
fn submodule_update_initializes_submodules() {
    let root = make_test_repo();
    let sub_source = make_submodule_source();
    subcontext_ok(&root, &["install"]);

    // Add submodule
    subcontext_ok(
        &root,
        &[
            "submodule",
            "add",
            &sub_source.to_string_lossy(),
            "lib/mylib",
        ],
    );

    // Verify submodule update works (should be a no-op since already initialized)
    subcontext_ok(&root, &["submodule", "update"]);

    assert!(
        root.join("lib/mylib/lib.rs").exists(),
        "submodule files should still be present after update"
    );

    cleanup(&sub_source);
    cleanup(&root);
}

#[test]
fn submodule_coexists_with_regular_overlay_files() {
    let root = make_test_repo();
    let sub_source = make_submodule_source();
    subcontext_ok(&root, &["install"]);

    // Add a regular overlay file
    fs::write(root.join("NOTES.md"), "my notes\n").unwrap();
    subcontext_ok(&root, &["add", "NOTES.md"]);
    subcontext_ok(&root, &["save", "-m", "add notes"]);

    // Add a submodule
    subcontext_ok(
        &root,
        &[
            "submodule",
            "add",
            &sub_source.to_string_lossy(),
            "lib/mylib",
        ],
    );

    // Both should exist
    assert!(
        root.join("NOTES.md").exists(),
        "regular overlay file should exist"
    );
    assert!(
        root.join("lib/mylib/lib.rs").exists(),
        "submodule file should exist"
    );

    // Neither should appear in git status
    let status = git(&root, &["status", "--porcelain"]);
    assert!(
        !status.contains("NOTES.md"),
        "regular overlay file should be excluded, got: {status}"
    );
    assert!(
        !status.contains("lib/mylib"),
        "submodule should be excluded, got: {status}"
    );

    // Switch branch and back — both should survive
    git(&root, &["checkout", "-b", "feature"]);
    git(&root, &["checkout", "main"]);

    assert!(
        root.join("NOTES.md").exists(),
        "overlay file should survive branch switch"
    );
    assert!(
        root.join("lib/mylib/lib.rs").exists(),
        "submodule should survive branch switch"
    );

    cleanup(&sub_source);
    cleanup(&root);
}

#[test]
fn submodule_content_changes_survive_save() {
    let root = make_test_repo();
    let sub_source = make_submodule_source();
    subcontext_ok(&root, &["install"]);

    // Add submodule
    subcontext_ok(
        &root,
        &[
            "submodule",
            "add",
            &sub_source.to_string_lossy(),
            "lib/mylib",
        ],
    );

    // Modify a file inside the submodule in the checkout root
    fs::write(root.join("lib/mylib/lib.rs"), "pub fn modified() {}\n").unwrap();

    // Save — should persist the change
    subcontext_ok(&root, &["save", "-m", "modify submodule file"]);

    // Switch to feature and back to main
    git(&root, &["checkout", "-b", "feature"]);
    git(&root, &["checkout", "main"]);

    // The modified content should be preserved
    let content = fs::read_to_string(root.join("lib/mylib/lib.rs")).unwrap();
    assert_eq!(
        content, "pub fn modified() {}\n",
        "submodule content changes should survive save + branch switch"
    );

    cleanup(&sub_source);
    cleanup(&root);
}

// ─── Helper ─────────────────────────────────────────────────────────

/// Run a git command in the subcontext bare repo.
fn git_in_repo(root: &Path, args: &[&str]) -> String {
    let repo_path = root.join(".git/.subcontext/repo");
    let git_dir_flag = format!("--git-dir={}", repo_path.display());
    let mut full_args = vec![git_dir_flag.as_str()];
    full_args.extend_from_slice(args);
    git(root, &full_args)
}

// ─── Tasks ──────────────────────────────────────────────────────────

#[test]
fn install_writes_project_config_and_state_branch() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // subcontext.yaml on config branch with a project UUID
    let cfg = fs::read_to_string(root.join(".git/.subcontext/config/subcontext.yaml")).unwrap();
    assert!(cfg.contains("project_uuid:"));
    assert!(cfg.contains("kind: project"));
    assert!(cfg.contains("version: 0.0.0"));

    // State branch + worktree + tasks.db exist
    let branches = git_in_repo(&root, &["branch", "--list", "state"]);
    assert!(branches.contains("state"));
    assert!(root.join(".git/.subcontext/state/tasks.db").exists());

    cleanup(&root);
}

#[test]
fn task_add_creates_task_branch_and_updates_db() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    subcontext_ok(&root, &["task", "add", "write-docs", "--kind", "todo"]);

    // An object/<uuid> branch was created
    let branches = git_in_repo(&root, &["branch", "--list", "object/*"]);
    assert!(
        branches.contains("object/"),
        "expected an object/<uuid> branch, got: {branches}"
    );

    // Extract the UUID from the branch name
    let uuid = branches
        .lines()
        .next()
        .unwrap()
        .trim()
        .trim_start_matches("* ")
        .trim_start_matches("object/")
        .to_string();

    // object.json on the task branch has all task data inlined
    let obj_json = git_in_repo(&root, &["show", &format!("object/{uuid}:object.json")]);
    assert!(obj_json.contains("\"type\": \"task\""), "obj: {obj_json}");
    assert!(
        obj_json.contains(&format!("\"uuid\": \"{uuid}\"")),
        "obj: {obj_json}"
    );
    assert!(obj_json.contains("\"kind\": \"todo\""), "obj: {obj_json}");
    assert!(
        obj_json.contains("\"status\": \"created\""),
        "obj: {obj_json}"
    );
    assert!(obj_json.contains("\"project_uuid\":"), "obj: {obj_json}");
    assert!(
        obj_json.contains("\"description\": null"),
        "obj: {obj_json}"
    );
    // name is stored in parent, not in object.json
    assert!(
        !obj_json.contains("\"name\":"),
        "name should not be in object.json: {obj_json}"
    );

    // State branch has a commit for the task add
    let log = git_in_repo(&root, &["log", "--oneline", "state"]);
    assert!(log.contains("task add: write-docs"));

    cleanup(&root);
}

#[test]
fn task_done_marks_task_and_adds_completed_at() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);
    subcontext_ok(&root, &["task", "add", "ship-it"]);

    subcontext_ok(
        &root,
        &["task", "done", "ship-it", "--time", "2026-01-02T03:04:05Z"],
    );

    let branches = git_in_repo(&root, &["branch", "--list", "object/*"]);
    let uuid = branches
        .lines()
        .next()
        .unwrap()
        .trim()
        .trim_start_matches("* ")
        .trim_start_matches("object/")
        .to_string();

    let obj_json = git_in_repo(&root, &["show", &format!("object/{uuid}:object.json")]);
    assert!(obj_json.contains("\"status\": \"done\""), "obj: {obj_json}");
    assert!(
        obj_json.contains("\"completed_at\": \"2026-01-02T03:04:05Z\""),
        "obj: {obj_json}"
    );

    let log = git_in_repo(&root, &["log", "--oneline", "state"]);
    assert!(log.contains("task done: ship-it"));

    cleanup(&root);
}

#[test]
fn task_done_accepts_now_and_rejects_local_time() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);
    subcontext_ok(&root, &["task", "add", "a"]);
    subcontext_ok(&root, &["task", "add", "b"]);

    // "now" is accepted
    subcontext_ok(&root, &["task", "done", "a", "--time", "now"]);

    // Non-UTC (no 'Z') is rejected
    let out = subcontext(
        &root,
        &["task", "done", "b", "--time", "2026-04-05T12:00:00"],
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("ending with 'Z'"), "stderr: {stderr}");

    // Find the task branch for "a" and confirm completed_at ends with Z
    let branches = git_in_repo(&root, &["branch", "--list", "object/*"]);
    for line in branches.lines() {
        let uuid = line
            .trim()
            .trim_start_matches("* ")
            .trim_start_matches("object/");
        let obj_json = git_in_repo(&root, &["show", &format!("object/{uuid}:object.json")]);
        if obj_json.contains("\"name\": \"a\"") {
            assert!(obj_json.contains("\"status\": \"done\""), "obj: {obj_json}");
            // completed_at value ends with Z
            let line = obj_json
                .lines()
                .find(|l| l.contains("completed_at"))
                .unwrap();
            assert!(line.contains('Z'), "completed_at should be UTC: {line}");
        }
    }

    cleanup(&root);
}

#[test]
fn task_add_duplicate_name_warns() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);
    subcontext_ok(&root, &["task", "add", "dup"]);

    // Duplicate names are allowed but emit a warning on stdout.
    let out = subcontext(&root, &["task", "add", "dup"]);
    assert!(out.status.success(), "duplicate task name should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("WARNING") && stdout.contains("not unique"),
        "expected uniqueness warning, got: {stdout}"
    );

    cleanup(&root);
}

#[test]
fn task_done_unknown_task_fails() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let out = subcontext(&root, &["task", "done", "nope"]);
    assert!(!out.status.success(), "done on unknown task should fail");

    cleanup(&root);
}

#[test]
fn task_add_creates_shadow_task_in_global() {
    let fake_home = std::env::temp_dir().join(format!(
        "subcontext-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&fake_home).unwrap();
    let global_path = make_global_dir();

    // Install global, then install locally in a fresh repo.
    let out = subcontext_with_global(&fake_home, &global_path, &["install", "--global"]);
    assert!(out.status.success());
    let root = make_test_repo();
    let out = subcontext_with_global(&root, &global_path, &["install"]);
    assert!(out.status.success());

    // Read the project UUID for the local install.
    let yaml = fs::read_to_string(root.join(".git/.subcontext/config/subcontext.yaml")).unwrap();
    let project_uuid = yaml
        .lines()
        .find_map(|l| {
            l.strip_prefix("project_uuid:")
                .map(|s| s.trim().to_string())
        })
        .expect("project_uuid missing");

    // Add a task locally — a shadow should be created in global.
    let out = subcontext_with_global(&root, &global_path, &["task", "add", "plan-feature"]);
    assert!(
        out.status.success(),
        "task add failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("shadow task"),
        "expected shadow task message, got: {stderr}"
    );

    // Verify the shadow row in the global tasks.db via the objects table.
    let global_state_db = global_path.join("global/state/tasks.db");
    let conn = rusqlite::Connection::open(&global_state_db).unwrap();

    // The shadow lives under the origin project UUID as its task_names
    // namespace, so the name doesn't collide with global-only tasks.
    let (branch_name, task_uuid): (String, String) = conn
        .query_row(
            "SELECT branch_name, task_uuid FROM task_names WHERE task_name = 'plan-feature'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(branch_name, project_uuid);

    // Check the objects table has source info for the shadow task.
    let (obj_type, source_context_uuid, source_object_uuid): (String, String, String) = conn
        .query_row(
            "SELECT type, source_context_uuid, source_object_uuid FROM objects WHERE uuid = ?1",
            [&task_uuid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("shadow task object row missing");
    assert_eq!(obj_type, "task");
    assert_eq!(source_context_uuid, project_uuid);
    assert!(!source_object_uuid.is_empty());
    drop(conn);

    // object.json now has checkout_path in the data pointing to the child's .git.
    let global_repo = global_path.join("global/repo");
    let json = Command::new("git")
        .args([
            &format!("--git-dir={}", global_repo.display()),
            "show",
            &format!("object/{project_uuid}:object.json"),
        ])
        .envs(test_env())
        .output()
        .unwrap();
    assert!(json.status.success());
    let text = String::from_utf8_lossy(&json.stdout);
    let expected = root.join(".git").to_string_lossy().to_string();
    assert!(
        text.contains(&expected) && text.contains("checkout_path"),
        "expected checkout_path {expected} in object.json: {text}"
    );

    cleanup(&fake_home);
    cleanup(&global_path);
    cleanup(&root);
}

#[test]
fn task_add_local_skips_shadow() {
    let fake_home = std::env::temp_dir().join(format!(
        "subcontext-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&fake_home).unwrap();
    let global_path = make_global_dir();

    let out = subcontext_with_global(&fake_home, &global_path, &["install", "--global"]);
    assert!(out.status.success());
    let root = make_test_repo();
    let out = subcontext_with_global(&root, &global_path, &["install"]);
    assert!(out.status.success());

    // --local skips the shadow task.
    let out = subcontext_with_global(
        &root,
        &global_path,
        &["task", "--local", "add", "private-task"],
    );
    assert!(out.status.success());

    let global_state_db = global_path.join("global/state/tasks.db");
    let conn = rusqlite::Connection::open(&global_state_db).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE task_name = 'private-task'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "--local should not create shadow task");

    cleanup(&fake_home);
    cleanup(&global_path);
    cleanup(&root);
}

#[test]
fn task_add_shadow_promotes_checkout_path_to_list() {
    let fake_home = std::env::temp_dir().join(format!(
        "subcontext-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&fake_home).unwrap();
    let global_path = make_global_dir();

    let out = subcontext_with_global(&fake_home, &global_path, &["install", "--global"]);
    assert!(out.status.success());

    // Two local clones of the same project (forced by copying the config
    // branch UUID). Simulate by creating two repos, installing, and
    // overwriting the second's project_uuid with the first's.
    let root_a = make_test_repo();
    let out = subcontext_with_global(&root_a, &global_path, &["install"]);
    assert!(out.status.success());
    let yaml_a =
        fs::read_to_string(root_a.join(".git/.subcontext/config/subcontext.yaml")).unwrap();
    let project_uuid = yaml_a
        .lines()
        .find_map(|l| {
            l.strip_prefix("project_uuid:")
                .map(|s| s.trim().to_string())
        })
        .unwrap();

    // Add first task → checkout_path becomes a string.
    let out = subcontext_with_global(&root_a, &global_path, &["task", "add", "t1"]);
    assert!(out.status.success());

    let root_b = make_test_repo();
    let out = subcontext_with_global(&root_b, &global_path, &["install"]);
    assert!(out.status.success());
    // Force repo B to share the same project_uuid (simulating a clone).
    let yaml_b =
        fs::read_to_string(root_b.join(".git/.subcontext/config/subcontext.yaml")).unwrap();
    let new_yaml = yaml_b
        .lines()
        .map(|l| {
            if l.starts_with("project_uuid:") {
                format!("project_uuid: {project_uuid}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(
        root_b.join(".git/.subcontext/config/subcontext.yaml"),
        new_yaml,
    )
    .unwrap();

    // Add another task from repo B → should promote checkout_path to a list.
    let out = subcontext_with_global(&root_b, &global_path, &["task", "add", "t2"]);
    assert!(
        out.status.success(),
        "task add from second checkout failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let global_repo = global_path.join("global/repo");
    let json = Command::new("git")
        .args([
            &format!("--git-dir={}", global_repo.display()),
            "show",
            &format!("object/{project_uuid}:object.json"),
        ])
        .envs(test_env())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&json.stdout);
    let path_a = root_a.join(".git").to_string_lossy().to_string();
    let path_b = root_b.join(".git").to_string_lossy().to_string();
    assert!(
        text.contains(&path_a) && text.contains(&path_b),
        "both paths should appear in object.json: {text}"
    );
    assert!(
        text.contains("checkout_path") && text.contains('['),
        "checkout_path should now be an array: {text}"
    );

    cleanup(&fake_home);
    cleanup(&global_path);
    cleanup(&root_a);
    cleanup(&root_b);
}

#[test]
fn uninstall_removes_state_worktree() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);
    subcontext_ok(&root, &["uninstall"]);

    assert!(!root.join(".git/.subcontext").exists());

    cleanup(&root);
}

fn subcontext_with_global_ok(cwd: &Path, global_path: &Path, args: &[&str]) -> String {
    let out = subcontext_with_global(cwd, global_path, args);
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

fn read_uuid(root: &Path) -> String {
    let yaml = fs::read_to_string(root.join(".git/.subcontext/config/subcontext.yaml")).unwrap();
    yaml.lines()
        .find_map(|l| {
            l.strip_prefix("project_uuid:")
                .map(|s| s.trim().to_string())
        })
        .expect("project_uuid missing")
}

#[test]
fn install_user_creates_user_subcontext() {
    let fake_home = std::env::temp_dir().join(format!(
        "subcontext-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&fake_home).unwrap();
    let global_path = make_global_dir();

    // Install system (global) first.
    subcontext_with_global_ok(&fake_home, &global_path, &["install", "--global"]);

    // Install user subcontext.
    subcontext_with_global_ok(&fake_home, &global_path, &["install", "--user"]);

    // Verify user dir exists with kind: user.
    let user_yaml = fs::read_to_string(global_path.join("user/config/subcontext.yaml")).unwrap();
    assert!(user_yaml.contains("kind: user"), "yaml: {user_yaml}");

    // Current user should be set automatically.
    let stdout = subcontext_with_global_ok(&fake_home, &global_path, &["current-user"]);
    let user_uuid = stdout.trim();
    assert!(
        !user_uuid.is_empty(),
        "current user UUID should not be empty"
    );

    // The user UUID should appear in the yaml.
    assert!(
        user_yaml.contains(user_uuid),
        "user yaml should contain the UUID"
    );

    cleanup(&fake_home);
    cleanup(&global_path);
}

#[test]
fn task_propagates_to_user_context_not_global() {
    let fake_home = std::env::temp_dir().join(format!(
        "subcontext-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&fake_home).unwrap();
    let global_path = make_global_dir();

    // Set up system -> user -> project hierarchy.
    subcontext_with_global_ok(&fake_home, &global_path, &["install", "--global"]);
    subcontext_with_global_ok(&fake_home, &global_path, &["install", "--user"]);

    let root = make_test_repo();
    subcontext_with_global_ok(&root, &global_path, &["install"]);

    // Add a task in the project.
    subcontext_with_global_ok(&root, &global_path, &["task", "add", "my-task"]);

    // The shadow should appear in the user subcontext's tasks.db.
    let user_db = global_path.join("user/state/tasks.db");
    let conn = rusqlite::Connection::open(&user_db).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE task_name = 'my-task'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(count > 0, "task should be propagated to user context");
    drop(conn);

    cleanup(&fake_home);
    cleanup(&global_path);
    cleanup(&root);
}

#[test]
fn task_propagation_is_recursive() {
    // Tests that a task created in a project propagates all the way up:
    // project -> user -> system (global).
    let fake_home = std::env::temp_dir().join(format!(
        "subcontext-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&fake_home).unwrap();
    let global_path = make_global_dir();

    // 1. Install system subcontext.
    subcontext_with_global_ok(&fake_home, &global_path, &["install", "--global"]);

    // 2. Install user subcontext.
    subcontext_with_global_ok(&fake_home, &global_path, &["install", "--user"]);

    // 3. Install project subcontext.
    let root = make_test_repo();
    subcontext_with_global_ok(&root, &global_path, &["install"]);

    // 4. Add a task in the project (without --local).
    subcontext_with_global_ok(&root, &global_path, &["task", "add", "recursive-task"]);

    // 5. Verify the task exists in the project's local DB.
    let local_db = root.join(".git/.subcontext/state/tasks.db");
    let conn = rusqlite::Connection::open(&local_db).unwrap();
    let local_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE task_name = 'recursive-task'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(local_count, 1, "task should exist locally");
    drop(conn);

    // 6. Verify the task propagated to the user context.
    let user_db = global_path.join("user/state/tasks.db");
    let conn = rusqlite::Connection::open(&user_db).unwrap();
    let user_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE task_name = 'recursive-task'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        user_count > 0,
        "task should propagate to user context (got {user_count})"
    );
    drop(conn);

    // 7. Verify the task propagated to the system (global) context.
    let global_db = global_path.join("global/state/tasks.db");
    let conn = rusqlite::Connection::open(&global_db).unwrap();
    let global_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE task_name = 'recursive-task'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        global_count > 0,
        "task should propagate recursively to global/system context (got {global_count})"
    );
    drop(conn);

    cleanup(&fake_home);
    cleanup(&global_path);
    cleanup(&root);
}

#[test]
fn tree_shows_managed_hierarchy() {
    let fake_home = std::env::temp_dir().join(format!(
        "subcontext-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&fake_home).unwrap();
    let global_path = make_global_dir();

    subcontext_with_global_ok(&fake_home, &global_path, &["install", "--global"]);
    subcontext_with_global_ok(&fake_home, &global_path, &["install", "--user"]);

    let root = make_test_repo();
    subcontext_with_global_ok(&root, &global_path, &["install"]);

    let stdout = subcontext_with_global_ok(&fake_home, &global_path, &["tree"]);
    assert!(stdout.contains("(system)"), "tree should show system root");
    assert!(stdout.contains("(user)"), "tree should show user node");
    assert!(
        stdout.contains("(project)"),
        "tree should show project node: {stdout}"
    );

    cleanup(&fake_home);
    cleanup(&global_path);
    cleanup(&root);
}

#[test]
fn parent_and_children_commands() {
    let fake_home = std::env::temp_dir().join(format!(
        "subcontext-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&fake_home).unwrap();
    let global_path = make_global_dir();

    subcontext_with_global_ok(&fake_home, &global_path, &["install", "--global"]);
    subcontext_with_global_ok(&fake_home, &global_path, &["install", "--user"]);

    let root = make_test_repo();
    subcontext_with_global_ok(&root, &global_path, &["install"]);

    // Project's parent should be the user UUID.
    let parent_out = subcontext_with_global_ok(&root, &global_path, &["parent"]);
    assert!(
        parent_out.contains("(user)"),
        "parent should be user: {parent_out}"
    );

    // Project should have no children.
    let out = subcontext_with_global(&root, &global_path, &["children"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("No children"),
        "project should have no children: stdout={}, stderr={}",
        String::from_utf8_lossy(&out.stdout),
        stderr
    );

    // UUID command should print the project's UUID.
    let uuid_out = subcontext_with_global_ok(&root, &global_path, &["uuid"]);
    let project_uuid = read_uuid(&root);
    assert_eq!(uuid_out.trim(), project_uuid);

    cleanup(&fake_home);
    cleanup(&global_path);
    cleanup(&root);
}

#[test]
fn status_shows_global_and_ancestry() {
    let fake_home = std::env::temp_dir().join(format!(
        "subcontext-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&fake_home).unwrap();
    let global_path = make_global_dir();

    subcontext_with_global_ok(&fake_home, &global_path, &["install", "--global"]);
    subcontext_with_global_ok(&fake_home, &global_path, &["install", "--user"]);

    let root = make_test_repo();
    subcontext_with_global_ok(&root, &global_path, &["install"]);

    let stdout = subcontext_with_global_ok(&root, &global_path, &["status"]);
    assert!(
        stdout.contains("Global:") && stdout.contains("(system)"),
        "status should show global system: {stdout}"
    );
    assert!(
        stdout.contains("Ancestry:") && stdout.contains("(user)"),
        "status should show user in ancestry: {stdout}"
    );

    cleanup(&fake_home);
    cleanup(&global_path);
    cleanup(&root);
}

// ─── TASK.md integration tests ────────────────────���─────────────────

#[test]
fn task_add_creates_task_md_on_object_branch() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    subcontext_ok(
        &root,
        &[
            "task",
            "add",
            "write-docs",
            "--kind",
            "todo",
            "--description",
            "Write the docs",
        ],
    );

    // Extract UUID from object branch.
    let branches = git_in_repo(&root, &["branch", "--list", "object/*"]);
    let uuid = branches
        .lines()
        .next()
        .unwrap()
        .trim()
        .trim_start_matches("* ")
        .trim_start_matches("object/")
        .to_string();

    // TASK.md should exist on the object branch.
    let task_md = git_in_repo(&root, &["show", &format!("object/{uuid}:TASK.md")]);
    assert!(task_md.contains("kind: todo"), "TASK.md: {task_md}");
    assert!(task_md.contains("status: created"), "TASK.md: {task_md}");

    // object.json should also exist.
    let obj_json = git_in_repo(&root, &["show", &format!("object/{uuid}:object.json")]);
    assert!(obj_json.contains("\"kind\": \"todo\""), "obj: {obj_json}");

    cleanup(&root);
}

#[test]
fn task_add_from_file() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Write a TASK.md file.
    let task_md = "---\nkind: goal\nstatus: active\ndescription: From a file\ndeadline: 2026-12-31T00:00:00Z\nimportance: 2.0\n---\n# File Task\n\nDetailed description here.\n";
    fs::write(root.join("TASK.md"), task_md).unwrap();

    let out = subcontext(&root, &["task", "add", "file-task", "--file", "TASK.md"]);
    assert!(
        out.status.success(),
        "task add --file failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // stdout should contain the UUID.
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let uuid_line = stdout.lines().last().unwrap().trim();
    assert!(
        uuid_line.contains('-'),
        "expected UUID on stdout, got: {stdout}"
    );

    // Check TASK.md on the object branch — should contain the original body.
    let stored_md = git_in_repo(&root, &["show", &format!("object/{uuid_line}:TASK.md")]);
    assert!(
        stored_md.contains("# File Task"),
        "stored TASK.md: {stored_md}"
    );
    assert!(
        stored_md.contains("Detailed description here"),
        "stored TASK.md: {stored_md}"
    );
    // name: is stripped from stored TASK.md (names live in parent namespace)

    // object.json should have the parsed fields.
    let obj_json = git_in_repo(&root, &["show", &format!("object/{uuid_line}:object.json")]);
    assert!(obj_json.contains("\"kind\": \"goal\""), "obj: {obj_json}");
    assert!(
        obj_json.contains("\"status\": \"active\""),
        "obj: {obj_json}"
    );
    assert!(
        obj_json.contains("\"deadline\": \"2026-12-31T00:00:00Z\""),
        "obj: {obj_json}"
    );

    cleanup(&root);
}

#[test]
fn task_show_prints_task_md() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let task_md = "---\nkind: task\n---\n# Show Me\n\nBody content.\n";
    fs::write(root.join("TASK.md"), task_md).unwrap();
    subcontext_ok(&root, &["task", "add", "show-me", "--file", "TASK.md"]);

    let stdout = subcontext_ok(&root, &["task", "show", "show-me"]);
    assert!(
        stdout.contains("kind: task"),
        "show should print TASK.md frontmatter: {stdout}"
    );
    assert!(
        stdout.contains("# Show Me"),
        "show should print TASK.md body: {stdout}"
    );

    cleanup(&root);
}

#[test]
fn task_show_ambiguous_lists_matches() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    subcontext_ok(
        &root,
        &["task", "add", "ambig", "--description", "First task"],
    );
    subcontext_ok(
        &root,
        &["task", "add", "ambig", "--description", "Second task"],
    );

    let stdout = subcontext_ok(&root, &["task", "show", "ambig"]);
    assert!(
        stdout.contains("Multiple tasks match"),
        "expected ambiguity message: {stdout}"
    );
    assert!(
        stdout.contains("First task") && stdout.contains("Second task"),
        "should list both descriptions: {stdout}"
    );

    cleanup(&root);
}

#[test]
fn task_update_by_name() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);
    subcontext_ok(&root, &["task", "add", "update-me"]);

    subcontext_ok(
        &root,
        &[
            "task",
            "update",
            "update-me",
            "--status",
            "active",
            "--description",
            "Now active",
        ],
    );

    // Verify object.json was updated.
    let branches = git_in_repo(&root, &["branch", "--list", "object/*"]);
    let uuid = branches
        .lines()
        .next()
        .unwrap()
        .trim()
        .trim_start_matches("* ")
        .trim_start_matches("object/")
        .to_string();

    let obj_json = git_in_repo(&root, &["show", &format!("object/{uuid}:object.json")]);
    assert!(
        obj_json.contains("\"status\": \"active\""),
        "obj: {obj_json}"
    );
    assert!(
        obj_json.contains("\"description\": \"Now active\""),
        "obj: {obj_json}"
    );

    // TASK.md should also be updated.
    let task_md = git_in_repo(&root, &["show", &format!("object/{uuid}:TASK.md")]);
    assert!(task_md.contains("status: active"), "TASK.md: {task_md}");

    cleanup(&root);
}

#[test]
fn task_update_from_file() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);
    subcontext_ok(&root, &["task", "add", "file-update"]);

    let new_md = "---\nname: file-update\nkind: goal\nstatus: active\n---\n# Updated Body\n";
    fs::write(root.join("updated.md"), new_md).unwrap();

    subcontext_ok(
        &root,
        &["task", "update", "file-update", "--file", "updated.md"],
    );

    let branches = git_in_repo(&root, &["branch", "--list", "object/*"]);
    let uuid = branches
        .lines()
        .next()
        .unwrap()
        .trim()
        .trim_start_matches("* ")
        .trim_start_matches("object/")
        .to_string();

    let obj_json = git_in_repo(&root, &["show", &format!("object/{uuid}:object.json")]);
    assert!(obj_json.contains("\"kind\": \"goal\""), "obj: {obj_json}");
    assert!(
        obj_json.contains("\"status\": \"active\""),
        "obj: {obj_json}"
    );

    let task_md = git_in_repo(&root, &["show", &format!("object/{uuid}:TASK.md")]);
    assert!(task_md.contains("# Updated Body"), "TASK.md: {task_md}");

    cleanup(&root);
}

#[test]
fn task_done_syncs_task_md() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);
    subcontext_ok(&root, &["task", "add", "sync-done"]);

    subcontext_ok(
        &root,
        &[
            "task",
            "done",
            "sync-done",
            "--time",
            "2026-06-01T12:00:00Z",
        ],
    );

    let branches = git_in_repo(&root, &["branch", "--list", "object/*"]);
    let uuid = branches
        .lines()
        .next()
        .unwrap()
        .trim()
        .trim_start_matches("* ")
        .trim_start_matches("object/")
        .to_string();

    let task_md = git_in_repo(&root, &["show", &format!("object/{uuid}:TASK.md")]);
    assert!(
        task_md.contains("status: done"),
        "TASK.md should show done: {task_md}"
    );
    assert!(
        task_md.contains("completed_at: 2026-06-01T12:00:00Z"),
        "TASK.md should show completed_at: {task_md}"
    );

    cleanup(&root);
}

#[test]
fn object_commit_reports_in_sync() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);
    subcontext_ok(&root, &["task", "add", "commit-test"]);

    let branches = git_in_repo(&root, &["branch", "--list", "object/*"]);
    let uuid = branches
        .lines()
        .next()
        .unwrap()
        .trim()
        .trim_start_matches("* ")
        .trim_start_matches("object/")
        .to_string();

    // TASK.md already exists (created by add_task), so object-commit should
    // report them in sync.
    let out = subcontext(&root, &["object-commit", &uuid]);
    assert!(
        out.status.success(),
        "object-commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("in sync"),
        "expected in-sync message: {stderr}"
    );

    cleanup(&root);
}

#[test]
fn task_add_no_name_no_file_fails() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let out = subcontext(&root, &["task", "add"]);
    assert!(
        !out.status.success(),
        "task add without name or --file should fail"
    );

    cleanup(&root);
}

// ─── Docs dump tests ────────────────────────────────────────────────

#[test]
fn docs_dumps_files_to_directory() {
    let dest = std::env::temp_dir().join(format!(
        "subcontext-docs-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));

    let bin = test_bin_dir().join("subcontext");
    let out = Command::new(&bin)
        .args(["docs", dest.to_str().unwrap()])
        .envs(test_env())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "docs failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Root docs should exist.
    assert!(dest.join("README.md").exists(), "README.md should exist");
    assert!(dest.join("setup.md").exists(), "setup.md should exist");
    assert!(dest.join("usage.md").exists(), "usage.md should exist");

    // Sample skills should exist.
    assert!(
        dest.join("skills/README.md").exists(),
        "skills/README.md should exist"
    );
    assert!(
        dest.join("skills/add-task/SKILL.md").exists(),
        "skills/add-task/SKILL.md should exist"
    );
    assert!(
        dest.join("skills/task-schema/SKILL.md").exists(),
        "skills/task-schema/SKILL.md should exist"
    );

    // Content should be non-empty and contain expected strings.
    let setup = fs::read_to_string(dest.join("setup.md")).unwrap();
    assert!(
        setup.contains("subcontext install"),
        "setup.md should contain install instructions"
    );

    let skill = fs::read_to_string(dest.join("skills/add-task/SKILL.md")).unwrap();
    assert!(
        skill.contains("name: add-task"),
        "sample skill should have frontmatter"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Dumped"), "should print summary: {stderr}");

    let _ = fs::remove_dir_all(&dest);
}

#[test]
fn docs_overwrites_existing_files() {
    let dest = std::env::temp_dir().join(format!(
        "subcontext-docs-overwrite-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("README.md"), "old content").unwrap();

    let bin = test_bin_dir().join("subcontext");
    let out = Command::new(&bin)
        .args(["docs", dest.to_str().unwrap()])
        .envs(test_env())
        .output()
        .unwrap();
    assert!(out.status.success());

    // Should be overwritten with bundled content.
    let content = fs::read_to_string(dest.join("README.md")).unwrap();
    assert_ne!(content, "old content", "should overwrite existing files");
    assert!(
        content.contains("Subcontext"),
        "should contain bundled content"
    );

    let _ = fs::remove_dir_all(&dest);
}

// ─── Namespace ──────────────────────────────────────────────────────

#[test]
fn namespace_set_get_list_remove() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let uuid1 = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let uuid2 = "11111111-2222-3333-4444-555555555555";

    // Set a flat entry.
    subcontext_ok(&root, &["namespace", "set", "myproject", uuid1]);

    // Get it back.
    let out = subcontext_ok(&root, &["namespace", "get", "myproject"]);
    assert_eq!(out.trim(), uuid1);

    // Set a nested entry.
    subcontext_ok(&root, &["namespace", "set", "tools/editor", uuid2]);

    // Get the nested entry.
    let out = subcontext_ok(&root, &["namespace", "get", "tools/editor"]);
    assert_eq!(out.trim(), uuid2);

    // List all.
    let out = subcontext_ok(&root, &["namespace", "list"]);
    assert!(out.contains("myproject"));
    assert!(out.contains(uuid1));
    assert!(out.contains("tools/editor"));
    assert!(out.contains(uuid2));

    // Remove flat entry.
    subcontext_ok(&root, &["namespace", "remove", "myproject"]);
    let out = subcontext(&root, &["namespace", "get", "myproject"]);
    assert!(!out.status.success());

    // Remove nested entry (should clean up empty parent).
    subcontext_ok(&root, &["namespace", "remove", "tools/editor"]);
    let out = subcontext_ok(&root, &["namespace", "list"]);
    assert!(!out.contains("tools"));

    cleanup(&root);
}

#[test]
fn namespace_rejects_dot_prefix() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let out = subcontext(
        &root,
        &[
            "namespace",
            "set",
            ".bad",
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        ],
    );
    assert!(
        !out.status.success(),
        "names starting with '.' should be rejected"
    );

    cleanup(&root);
}

#[test]
fn namespace_user_flag_uses_user_config() {
    let fake_home = std::env::temp_dir().join(format!(
        "subcontext-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&fake_home).unwrap();
    let global_path = make_global_dir();

    subcontext_with_global_ok(&fake_home, &global_path, &["install", "--global"]);
    subcontext_with_global_ok(&fake_home, &global_path, &["install", "--user"]);

    let root = make_test_repo();
    subcontext_with_global_ok(&root, &global_path, &["install"]);

    let uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    // Set in user namespace.
    subcontext_with_global_ok(
        &root,
        &global_path,
        &["namespace", "--user", "set", "foo", uuid],
    );

    // Get from user namespace.
    let out =
        subcontext_with_global_ok(&root, &global_path, &["namespace", "--user", "get", "foo"]);
    assert_eq!(out.trim(), uuid);

    // Should NOT be visible in the project namespace.
    let out = subcontext_with_global(&root, &global_path, &["namespace", "get", "foo"]);
    assert!(!out.status.success());

    cleanup(&fake_home);
    cleanup(&global_path);
    cleanup(&root);
}

#[test]
fn task_path_dotdot_walks_to_parent() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Create parent and child tasks.
    subcontext_ok(&root, &["task", "add", "parent-task"]);
    subcontext_ok(
        &root,
        &["task", "add", "child-task", "--parent", "parent-task"],
    );

    // Set current task to child.
    subcontext_ok(&root, &["task", "set", "parent-task/child-task"]);

    // Use .. to reference the parent — stderr should show the parent's UUID.
    let show_out = subcontext(&root, &["task", "show", ".."]);
    assert!(show_out.status.success(), "task show .. should succeed");
    let show_stderr = String::from_utf8_lossy(&show_out.stderr);
    let show_stdout = String::from_utf8_lossy(&show_out.stdout);
    // The .. path should resolve to the parent task (verified by UUID in stderr).
    assert!(
        show_stderr.contains("Task UUID:"),
        ".. should resolve to parent task, stderr: {show_stderr}, stdout: {show_stdout}"
    );
    // The parent's TASK.md should contain basic frontmatter.
    assert!(
        show_stdout.contains("kind: task"),
        ".. should show parent TASK.md, got: {show_stdout}"
    );

    cleanup(&root);
}

#[test]
fn task_path_dot_uuid_resolves_directly() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    // Create a task and capture its UUID.
    let add_out = subcontext(&root, &["task", "add", "uuid-task"]);
    let stderr = String::from_utf8_lossy(&add_out.stderr);
    // Extract UUID from "[subcontext] Added task 'uuid-task' (UUID)"
    let uuid = stderr
        .split('(')
        .nth(1)
        .and_then(|s| s.split(')').next())
        .unwrap()
        .trim();

    // Resolve via /.uuid/<uuid>. The stderr should print the Task UUID.
    let show_out = subcontext(&root, &["task", "show", &format!("/.uuid/{uuid}")]);
    assert!(
        show_out.status.success(),
        "task show /.uuid/<uuid> should succeed"
    );
    let show_stderr = String::from_utf8_lossy(&show_out.stderr);
    assert!(
        show_stderr.contains(uuid),
        "/.uuid/<uuid> should resolve to the task with UUID {uuid}, stderr: {show_stderr}"
    );

    cleanup(&root);
}

// ─── Board tests ──────────────────────────────────────────────────

#[test]
fn board_create_creates_board_branch_with_tree() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    subcontext_ok(
        &root,
        &[
            "board",
            "create",
            "work",
            "--kind",
            "goal",
            "--description",
            "My work board",
        ],
    );

    // A board branch (object/<uuid>) should exist.
    let branches = git_in_repo(&root, &["branch", "--list", "object/*"]);
    assert!(
        branches.contains("object/"),
        "expected a board branch, got: {branches}"
    );

    let uuid = branches
        .lines()
        .next()
        .unwrap()
        .trim()
        .trim_start_matches("* ")
        .trim_start_matches("object/")
        .to_string();

    // object.json should be a tree-format task.
    let obj_json = git_in_repo(&root, &["show", &format!("object/{uuid}:object.json")]);
    assert!(
        obj_json.contains("\"type\": \"task\""),
        "should be task type, got: {obj_json}"
    );
    assert!(
        obj_json.contains("\"format\": \"tree\""),
        "should have format tree, got: {obj_json}"
    );
    assert!(
        obj_json.contains(&format!("\"uuid\": \"{uuid}\"")),
        "should have board UUID, got: {obj_json}"
    );

    // TASK.md should exist with root task metadata.
    let task_md = git_in_repo(&root, &["show", &format!("object/{uuid}:TASK.md")]);
    assert!(
        task_md.contains("kind: goal"),
        "root TASK.md should have kind: {task_md}"
    );
    assert!(
        task_md.contains("status: created"),
        "root TASK.md should have status: {task_md}"
    );
    // TASK.md should NOT contain subtasks key.
    assert!(
        !task_md.contains("subtasks:"),
        "TASK.md should not contain subtasks key: {task_md}"
    );

    cleanup(&root);
}

#[test]
fn board_add_task_creates_subtask_directory() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let create_out = subcontext(&root, &["board", "create", "work"]);
    assert!(create_out.status.success());
    let board_uuid = String::from_utf8_lossy(&create_out.stdout)
        .trim()
        .to_string();

    subcontext_ok(
        &root,
        &[
            "board",
            "add-task",
            "write-docs",
            "--board",
            &board_uuid,
            "--kind",
            "todo",
            "--description",
            "Write documentation",
        ],
    );

    // The board tree should now have write-docs/TASK.md.
    let task_md = git_in_repo(
        &root,
        &["show", &format!("object/{board_uuid}:write-docs/TASK.md")],
    );
    assert!(task_md.contains("kind: todo"), "subtask TASK.md: {task_md}");
    assert!(
        task_md.contains("status: created"),
        "subtask TASK.md: {task_md}"
    );

    // The root TASK.md should still be there.
    let root_md = git_in_repo(&root, &["show", &format!("object/{board_uuid}:TASK.md")]);
    assert!(root_md.contains("kind: task"), "root TASK.md: {root_md}");

    cleanup(&root);
}

#[test]
fn board_add_nested_subtasks() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let create_out = subcontext(&root, &["board", "create", "work"]);
    assert!(create_out.status.success());
    let board_uuid = String::from_utf8_lossy(&create_out.stdout)
        .trim()
        .to_string();

    // Add a direct subtask.
    subcontext_ok(
        &root,
        &["board", "add-task", "project-a", "--board", &board_uuid],
    );

    // Add a nested subtask under project-a.
    // First find project-a's UUID.
    let show_out = subcontext(&root, &["task", "show", "project-a"]);
    let stderr = String::from_utf8_lossy(&show_out.stderr);
    let project_a_uuid = stderr
        .split("Task UUID: ")
        .nth(1)
        .and_then(|s| s.lines().next())
        .unwrap()
        .trim();

    subcontext_ok(
        &root,
        &[
            "board",
            "add-task",
            "sub-task",
            "--board",
            &board_uuid,
            "--parent",
            project_a_uuid,
        ],
    );

    // The tree should have project-a/sub-task/TASK.md.
    let nested_md = git_in_repo(
        &root,
        &[
            "show",
            &format!("object/{board_uuid}:project-a/sub-task/TASK.md"),
        ],
    );
    assert!(
        nested_md.contains("kind: task"),
        "nested subtask: {nested_md}"
    );

    cleanup(&root);
}

#[test]
fn board_commit_syncs_state_db() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let create_out = subcontext(&root, &["board", "create", "work"]);
    assert!(create_out.status.success());
    let board_uuid = String::from_utf8_lossy(&create_out.stdout)
        .trim()
        .to_string();

    subcontext_ok(
        &root,
        &["board", "add-task", "task-one", "--board", &board_uuid],
    );

    // Run board commit to sync.
    let commit_out = subcontext(&root, &["board", "commit", &board_uuid]);
    assert!(
        commit_out.status.success(),
        "board commit failed: {}",
        String::from_utf8_lossy(&commit_out.stderr)
    );
    let stderr = String::from_utf8_lossy(&commit_out.stderr);
    assert!(
        stderr.contains("Synchronized"),
        "should report sync: {stderr}"
    );

    cleanup(&root);
}

#[test]
fn board_task_done_updates_board_tree() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let create_out = subcontext(&root, &["board", "create", "work"]);
    assert!(create_out.status.success());
    let board_uuid = String::from_utf8_lossy(&create_out.stdout)
        .trim()
        .to_string();

    subcontext_ok(
        &root,
        &["board", "add-task", "finish-me", "--board", &board_uuid],
    );

    // Mark the subtask as done.
    // First set the board root as current task so we can navigate.
    subcontext_ok(&root, &["task", "set", "work"]);
    subcontext_ok(
        &root,
        &[
            "task",
            "done",
            "finish-me",
            "--time",
            "2026-06-01T00:00:00Z",
        ],
    );

    // Check the board tree — finish-me/TASK.md should have status: done.
    let task_md = git_in_repo(
        &root,
        &["show", &format!("object/{board_uuid}:finish-me/TASK.md")],
    );
    assert!(
        task_md.contains("status: done"),
        "subtask should be done: {task_md}"
    );
    assert!(
        task_md.contains("completed_at: 2026-06-01T00:00:00Z"),
        "subtask should have completed_at: {task_md}"
    );

    cleanup(&root);
}

#[test]
fn board_move_task_changes_parent() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let create_out = subcontext(&root, &["board", "create", "work"]);
    assert!(create_out.status.success());
    let board_uuid = String::from_utf8_lossy(&create_out.stdout)
        .trim()
        .to_string();

    // Add two direct subtasks: project-a and project-b.
    subcontext_ok(
        &root,
        &["board", "add-task", "project-a", "--board", &board_uuid],
    );
    subcontext_ok(
        &root,
        &["board", "add-task", "project-b", "--board", &board_uuid],
    );

    // Add a child under project-a.
    let show_out = subcontext(&root, &["task", "show", "project-a"]);
    let stderr = String::from_utf8_lossy(&show_out.stderr);
    let project_a_uuid = stderr
        .split("Task UUID: ")
        .nth(1)
        .and_then(|s| s.lines().next())
        .unwrap()
        .trim()
        .to_string();

    subcontext_ok(
        &root,
        &[
            "board",
            "add-task",
            "child-task",
            "--board",
            &board_uuid,
            "--parent",
            &project_a_uuid,
        ],
    );

    // Verify child-task is under project-a.
    let child_md = git_in_repo(
        &root,
        &[
            "show",
            &format!("object/{board_uuid}:project-a/child-task/TASK.md"),
        ],
    );
    assert!(child_md.contains("uuid:"), "child should exist: {child_md}");

    // Get child-task UUID.
    let show_out2 = subcontext(&root, &["task", "show", "child-task"]);
    let stderr2 = String::from_utf8_lossy(&show_out2.stderr);
    let child_uuid = stderr2
        .split("Task UUID: ")
        .nth(1)
        .and_then(|s| s.lines().next())
        .unwrap()
        .trim()
        .to_string();

    // Get project-b UUID.
    let show_out3 = subcontext(&root, &["task", "show", "project-b"]);
    let stderr3 = String::from_utf8_lossy(&show_out3.stderr);
    let project_b_uuid = stderr3
        .split("Task UUID: ")
        .nth(1)
        .and_then(|s| s.lines().next())
        .unwrap()
        .trim()
        .to_string();

    // Move child-task from project-a to project-b.
    subcontext_ok(
        &root,
        &[
            "board",
            "move-task",
            &child_uuid,
            "--parent",
            &project_b_uuid,
            "--board",
            &board_uuid,
        ],
    );

    // child-task should now be under project-b.
    let moved_md = git_in_repo(
        &root,
        &[
            "show",
            &format!("object/{board_uuid}:project-b/child-task/TASK.md"),
        ],
    );
    assert!(
        moved_md.contains("uuid:"),
        "child should be under project-b: {moved_md}"
    );
    // The UUID should be preserved.
    assert!(
        moved_md.contains(&child_uuid),
        "UUID should be preserved after move: {moved_md}"
    );

    cleanup(&root);
}

#[test]
fn board_delete_task_removes_from_tree() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let create_out = subcontext(&root, &["board", "create", "work"]);
    assert!(create_out.status.success());
    let board_uuid = String::from_utf8_lossy(&create_out.stdout)
        .trim()
        .to_string();

    // Add a subtask.
    subcontext_ok(
        &root,
        &[
            "board",
            "add-task",
            "doomed-task",
            "--board",
            &board_uuid,
            "--description",
            "This will be deleted",
        ],
    );

    // Verify it exists.
    let task_md = git_in_repo(
        &root,
        &["show", &format!("object/{board_uuid}:doomed-task/TASK.md")],
    );
    assert!(task_md.contains("uuid:"), "task should exist: {task_md}");

    // Get UUID.
    let show_out = subcontext(&root, &["task", "show", "doomed-task"]);
    let stderr = String::from_utf8_lossy(&show_out.stderr);
    let task_uuid = stderr
        .split("Task UUID: ")
        .nth(1)
        .and_then(|s| s.lines().next())
        .unwrap()
        .trim()
        .to_string();

    // Delete it.
    subcontext_ok(
        &root,
        &["board", "delete-task", &task_uuid, "--board", &board_uuid],
    );

    // The tree should no longer have doomed-task/TASK.md.
    let ls_out = git_in_repo(
        &root,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            &format!("object/{board_uuid}"),
        ],
    );
    assert!(
        !ls_out.contains("doomed-task"),
        "doomed-task should be gone: {ls_out}"
    );

    // The task should also not be in the DB (task show should fail).
    let show_after = subcontext(&root, &["task", "show", &task_uuid]);
    assert!(
        !show_after.status.success(),
        "task show should fail after delete"
    );

    cleanup(&root);
}

#[test]
fn board_pull_push_roundtrip() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let create_out = subcontext(&root, &["board", "create", "work"]);
    assert!(create_out.status.success());
    let board_uuid = String::from_utf8_lossy(&create_out.stdout)
        .trim()
        .to_string();

    // Add some tasks to the board.
    subcontext_ok(
        &root,
        &[
            "board",
            "add-task",
            "task-one",
            "--board",
            &board_uuid,
            "--description",
            "First task",
        ],
    );
    subcontext_ok(
        &root,
        &[
            "board",
            "add-task",
            "task-two",
            "--board",
            &board_uuid,
            "--description",
            "Second task",
        ],
    );

    // Pull the board into the overlay.
    subcontext_ok(&root, &["board", "pull", &board_uuid, "--path", "tasks/"]);

    // Check that files appeared in the working tree.
    assert!(
        root.join("tasks/TASK.md").exists(),
        "root TASK.md should exist in overlay"
    );
    assert!(
        root.join("tasks/task-one/TASK.md").exists(),
        "task-one/TASK.md should exist in overlay"
    );
    assert!(
        root.join("tasks/task-two/TASK.md").exists(),
        "task-two/TASK.md should exist in overlay"
    );
    assert!(
        root.join("tasks/.board.json").exists(),
        ".board.json config should exist"
    );

    // Modify a task in the overlay (simulating agent editing).
    let task_one_path = root.join("tasks/task-one/TASK.md");
    let original = fs::read_to_string(&task_one_path).unwrap();
    let modified = original.replace("status: created", "status: active");
    fs::write(&task_one_path, &modified).unwrap();

    // Push changes back.
    subcontext_ok(&root, &["board", "push", "--path", "tasks/"]);

    // Verify the board tree was updated.
    let board_md = git_in_repo(
        &root,
        &["show", &format!("object/{board_uuid}:task-one/TASK.md")],
    );
    assert!(
        board_md.contains("status: active"),
        "board should reflect pushed changes: {board_md}"
    );

    cleanup(&root);
}

#[test]
fn board_pull_filter_done() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let create_out = subcontext(&root, &["board", "create", "work"]);
    assert!(create_out.status.success());
    let board_uuid = String::from_utf8_lossy(&create_out.stdout)
        .trim()
        .to_string();

    subcontext_ok(
        &root,
        &["board", "add-task", "active-task", "--board", &board_uuid],
    );
    subcontext_ok(
        &root,
        &["board", "add-task", "done-task", "--board", &board_uuid],
    );

    // Mark done-task as done.
    subcontext_ok(&root, &["task", "set", "work"]);
    subcontext_ok(&root, &["task", "done", "done-task"]);

    // Pull with --filter-done.
    subcontext_ok(
        &root,
        &[
            "board",
            "pull",
            &board_uuid,
            "--path",
            "tasks/",
            "--filter-done",
        ],
    );

    // active-task should be present, done-task should NOT be.
    assert!(
        root.join("tasks/active-task/TASK.md").exists(),
        "active-task should be in overlay"
    );
    assert!(
        !root.join("tasks/done-task/TASK.md").exists(),
        "done-task should be filtered out"
    );

    cleanup(&root);
}

#[test]
fn board_push_mark_done_on_delete() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let create_out = subcontext(&root, &["board", "create", "work"]);
    assert!(create_out.status.success());
    let board_uuid = String::from_utf8_lossy(&create_out.stdout)
        .trim()
        .to_string();

    subcontext_ok(
        &root,
        &["board", "add-task", "keep-task", "--board", &board_uuid],
    );
    subcontext_ok(
        &root,
        &["board", "add-task", "finish-task", "--board", &board_uuid],
    );

    // Pull.
    subcontext_ok(&root, &["board", "pull", &board_uuid, "--path", "tasks/"]);

    // "Delete" finish-task from overlay by removing its directory.
    fs::remove_dir_all(root.join("tasks/finish-task")).unwrap();
    // Also remove from overlay git tracking.
    let work_dir = root.join(".git/.subcontext/work");
    if work_dir.exists() {
        let _ = Command::new("git")
            .args(["rm", "-rf", "tasks/finish-task"])
            .envs(test_env())
            .env("GIT_DIR", work_dir.join(".git"))
            .env("GIT_WORK_TREE", &work_dir)
            .current_dir(&work_dir)
            .output();
    }

    // Push with --mark-done.
    subcontext_ok(&root, &["board", "push", "--path", "tasks/", "--mark-done"]);

    // finish-task should still be in the board tree but marked as done.
    let board_md = git_in_repo(
        &root,
        &["show", &format!("object/{board_uuid}:finish-task/TASK.md")],
    );
    assert!(
        board_md.contains("status: done"),
        "deleted task should be marked done: {board_md}"
    );

    // keep-task should still be created.
    let keep_md = git_in_repo(
        &root,
        &["show", &format!("object/{board_uuid}:keep-task/TASK.md")],
    );
    assert!(
        keep_md.contains("status: created"),
        "kept task should still be created: {keep_md}"
    );

    cleanup(&root);
}

#[test]
fn board_commit_generates_missing_uuids() {
    let root = make_test_repo();
    subcontext_ok(&root, &["install"]);

    let create_out = subcontext(&root, &["board", "create", "work"]);
    assert!(create_out.status.success());
    let board_uuid = String::from_utf8_lossy(&create_out.stdout)
        .trim()
        .to_string();

    // Manually inject a TASK.md without a uuid into the board tree via
    // board pull → create file → board push.
    subcontext_ok(&root, &["board", "pull", &board_uuid, "--path", "tasks/"]);

    // Create a new subtask directory with a TASK.md that has NO uuid.
    let new_task_dir = root.join("tasks/no-uuid-task");
    fs::create_dir_all(&new_task_dir).unwrap();
    let task_content = "---\nkind: todo\nstatus: created\ndescription: I have no UUID\n---\n";
    fs::write(new_task_dir.join("TASK.md"), task_content).unwrap();

    // Also write into the overlay work dir and git-add it.
    let work_dir = root.join(".git/.subcontext/work");
    let work_task_dir = work_dir.join("tasks/no-uuid-task");
    fs::create_dir_all(&work_task_dir).unwrap();
    fs::write(work_task_dir.join("TASK.md"), task_content).unwrap();
    // Add to overlay git tracking.
    let git_dir = work_dir.join(".git");
    Command::new("git")
        .args(["add", "tasks/no-uuid-task/TASK.md"])
        .envs(test_env())
        .env("GIT_DIR", &git_dir)
        .env("GIT_WORK_TREE", &work_dir)
        .current_dir(&work_dir)
        .output()
        .unwrap();

    // Push to get it into the board tree.
    subcontext_ok(&root, &["board", "push", "--path", "tasks/"]);

    // After push (which calls board_commit), the TASK.md should now have a uuid.
    let task_md = git_in_repo(
        &root,
        &["show", &format!("object/{board_uuid}:no-uuid-task/TASK.md")],
    );
    assert!(
        task_md.contains("uuid:"),
        "TASK.md should have a generated uuid after commit: {task_md}"
    );

    // Running board commit again should produce the SAME uuid (stable).
    subcontext_ok(&root, &["board", "commit", &board_uuid]);
    let task_md2 = git_in_repo(
        &root,
        &["show", &format!("object/{board_uuid}:no-uuid-task/TASK.md")],
    );
    assert_eq!(task_md, task_md2, "uuid should be stable across commits");

    cleanup(&root);
}
