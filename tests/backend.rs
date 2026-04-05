//! Integration tests for the high-level [`Backend`] git helpers.
//!
//! These tests exercise [`SystemBackend`]'s defaulted trait methods
//! (`init`, `clone`, `worktree_add`, `commit`, …) against real git
//! repositories in `/tmp`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use subcontext::backend::{Backend, SystemBackend};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Configure the test process so every git subprocess sees a clean,
/// deterministic environment. Runs exactly once.
fn init_env() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // SAFETY: called before any test threads spawn git subprocesses
        // and while no other thread is reading env vars.
        unsafe {
            std::env::set_var("GIT_CONFIG_GLOBAL", "/dev/null");
            std::env::set_var("GIT_CONFIG_SYSTEM", "/dev/null");
            std::env::set_var("GIT_AUTHOR_NAME", "Test");
            std::env::set_var("GIT_AUTHOR_EMAIL", "test@test.com");
            std::env::set_var("GIT_COMMITTER_NAME", "Test");
            std::env::set_var("GIT_COMMITTER_EMAIL", "test@test.com");
        }
    });
}

fn tmpdir(tag: &str) -> PathBuf {
    init_env();
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "subcontext-backend-{}-{}-{}",
        std::process::id(),
        tag,
        id
    ));
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn cleanup(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

/// Initialize a non-bare repo at `path` via [`Backend::init`] and seed it
/// with an initial empty commit so worktrees and branches can be created.
fn seed_repo(backend: &dyn Backend, path: &Path) {
    backend.init(path, path).unwrap();
    // Force the initial branch to `main` for determinism across git versions.
    backend
        .git_in(path, &["symbolic-ref", "HEAD", "refs/heads/main"])
        .unwrap();
    backend
        .git_in(path, &["commit", "--allow-empty", "-m", "init"])
        .unwrap();
}

// ─── init / init_bare ─────────────────────────────────────────────────

#[test]
fn init_creates_non_bare_repo() {
    let dir = tmpdir("init");
    let backend = SystemBackend;
    let repo = dir.join("repo");
    fs::create_dir_all(&repo).unwrap();

    backend.init(&repo, &repo).unwrap();

    assert!(repo.join(".git").is_dir(), ".git directory should exist");
    cleanup(&dir);
}

#[test]
fn init_bare_creates_bare_repo() {
    let dir = tmpdir("init-bare");
    let backend = SystemBackend;
    let bare = dir.join("bare.git");

    backend.init_bare(&dir, &bare).unwrap();

    assert!(bare.is_dir(), "bare repo dir should exist");
    // A bare repo has HEAD at the top level, not inside .git/.
    assert!(bare.join("HEAD").is_file(), "bare HEAD should exist");
    assert!(
        !bare.join(".git").exists(),
        "bare repo must not have a .git/ subdir"
    );
    cleanup(&dir);
}

// ─── clone / clone_bare ───────────────────────────────────────────────

#[test]
fn clone_copies_repo() {
    let dir = tmpdir("clone");
    let backend = SystemBackend;
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    seed_repo(&backend, &src);

    let dest = dir.join("dest");
    let url = src.to_string_lossy().to_string();
    backend.clone(&dir, &url, &dest).unwrap();

    assert!(dest.join(".git").is_dir());
    // Cloned repo inherits the initial commit.
    let head = backend.rev_parse(&dest, "HEAD").unwrap();
    let src_head = backend.rev_parse(&src, "HEAD").unwrap();
    assert_eq!(head, src_head);
    cleanup(&dir);
}

#[test]
fn clone_bare_copies_as_bare() {
    let dir = tmpdir("clone-bare");
    let backend = SystemBackend;
    let src = dir.join("src");
    fs::create_dir_all(&src).unwrap();
    seed_repo(&backend, &src);

    let bare = dir.join("mirror.git");
    let url = src.to_string_lossy().to_string();
    backend.clone_bare(&dir, &url, &bare).unwrap();

    assert!(bare.join("HEAD").is_file());
    assert!(!bare.join(".git").exists());
    cleanup(&dir);
}

// ─── add / commit / status_porcelain ──────────────────────────────────

#[test]
fn add_all_and_commit_clear_status() {
    let dir = tmpdir("commit");
    let backend = SystemBackend;
    seed_repo(&backend, &dir);

    fs::write(dir.join("hello.txt"), "hi\n").unwrap();
    assert!(
        !backend.status_porcelain(&dir).unwrap().is_empty(),
        "new file should be visible as untracked"
    );

    backend.add_all(&dir).unwrap();
    assert!(
        backend
            .status_porcelain(&dir)
            .unwrap()
            .contains("hello.txt")
    );

    backend.commit(&dir, "add hello").unwrap();
    assert_eq!(
        backend.status_porcelain(&dir).unwrap(),
        "",
        "working tree should be clean after commit"
    );
    cleanup(&dir);
}

// ─── branch / checkout / rev_parse ────────────────────────────────────

#[test]
fn current_branch_and_checkout_new_branch() {
    let dir = tmpdir("branch");
    let backend = SystemBackend;
    seed_repo(&backend, &dir);

    assert_eq!(backend.current_branch(&dir).unwrap(), "main");

    backend.checkout_new_branch(&dir, "feature/x").unwrap();
    assert_eq!(backend.current_branch(&dir).unwrap(), "feature/x");

    backend.checkout(&dir, "main").unwrap();
    assert_eq!(backend.current_branch(&dir).unwrap(), "main");
    cleanup(&dir);
}

#[test]
fn rev_parse_resolves_head() {
    let dir = tmpdir("rev-parse");
    let backend = SystemBackend;
    seed_repo(&backend, &dir);

    let sha = backend.rev_parse(&dir, "HEAD").unwrap();
    assert_eq!(sha.len(), 40, "HEAD should resolve to a 40-char SHA");
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    cleanup(&dir);
}

// ─── worktree_add / worktree_remove ───────────────────────────────────

#[test]
fn worktree_add_and_remove() {
    let dir = tmpdir("worktree");
    let backend = SystemBackend;
    let repo = dir.join("repo");
    fs::create_dir_all(&repo).unwrap();
    seed_repo(&backend, &repo);

    // Create a branch to attach the worktree to.
    backend.git_in(&repo, &["branch", "side", "main"]).unwrap();

    let wt = dir.join("side-wt");
    let wt_str = wt.to_string_lossy().to_string();
    backend.worktree_add(&repo, &wt, "side").unwrap();

    assert!(wt.join(".git").exists(), "worktree .git file should exist");
    assert_eq!(backend.current_branch(&wt).unwrap(), "side");

    backend.worktree_remove(&repo, &wt_str).unwrap();
    assert!(!wt.exists(), "worktree directory should be removed");
    cleanup(&dir);
}

// ─── git / git_in still exposed ───────────────────────────────────────

#[test]
fn git_in_is_equivalent_to_raw_git() {
    let dir = tmpdir("raw");
    let backend = SystemBackend;
    seed_repo(&backend, &dir);

    let via_helper = backend.git_in(&dir, &["rev-parse", "HEAD"]).unwrap();
    let via_method = backend.rev_parse(&dir, "HEAD").unwrap();
    assert_eq!(via_helper, via_method);
    cleanup(&dir);
}
