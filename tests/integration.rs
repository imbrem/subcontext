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
    let dir = std::env::temp_dir().join(format!(
        "subcontext-sub-{}-{}",
        std::process::id(),
        id
    ));
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
    let bare_dir = std::env::temp_dir().join(format!(
        "subcontext-sub-bare-{}-{}",
        std::process::id(),
        id
    ));
    if bare_dir.exists() {
        fs::remove_dir_all(&bare_dir).unwrap();
    }
    git(
        &std::env::temp_dir(),
        &["clone", "--bare", &dir.to_string_lossy(), &bare_dir.to_string_lossy()],
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
        &["submodule", "add", &sub_source.to_string_lossy(), "lib/mylib"],
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
    assert!(root
        .join(".git/.subcontext/work/lib/mylib/lib.rs")
        .exists());

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
    subcontext_ok(
        &root,
        &["submodule", "add", &sub_source.to_string_lossy()],
    );

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
        &["submodule", "add", &sub_source.to_string_lossy(), "lib/mylib"],
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
        &["submodule", "add", &sub_source.to_string_lossy(), "lib/mylib"],
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
        &["submodule", "add", &sub_source.to_string_lossy(), "lib/mylib"],
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
        &["submodule", "add", &sub_source.to_string_lossy(), "lib/mylib"],
    );

    // Both should exist
    assert!(root.join("NOTES.md").exists(), "regular overlay file should exist");
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

    assert!(root.join("NOTES.md").exists(), "overlay file should survive branch switch");
    assert!(
        root.join("lib/mylib/lib.rs").exists(),
        "submodule should survive branch switch"
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
