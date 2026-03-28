# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

Subcontext is a Rust CLI that attaches a private, version-controlled **overlay** repo to any Git project. Arbitrary files from the overlay are materialized directly in the working tree, with "overlay wins" semantics — if a file exists in both repos, the overlay version takes precedence. Each host branch gets its own isolated overlay branch.

## Build & Test

```bash
cargo build                  # debug build
cargo test                   # run all tests (integration suite in tests/integration.rs)
cargo test <test_name>       # run a single test, e.g. cargo test install_creates
cargo run -- <command>       # run subcontext commands (install, clone, add, save, etc.)
```

No linter or formatter is configured — use `cargo clippy` and `cargo fmt` as needed.

## Architecture

All source lives in `src/` with one module per command:

- **main.rs** — Clap derive CLI parser, command dispatch, and path resolution
- **git.rs** — `run_git()`, `run_subcontext_git()`, `run_work_git()` helpers; `CheckoutContext` (main vs worktree); path helpers; `find_main_git_root()`, `find_checkout_context()`, `current_branch()`, `sanitize_branch_name()`
- **overlay.rs** — Core overlay operations: `add_file`, `remove_file`, `save_overlay`, `apply_overlay`, `unapply_overlay`, `sync_excludes`, branch creation/switching
- **install.rs** — `subcontext install [--repair]`: bare repo init, config/work worktrees, hooks, Claude settings
- **clone.rs** — `subcontext clone <url>`: clone existing context repo, set up worktrees, apply overlay
- **hook.rs** — `subcontext _hook post-checkout` (auto-save, unapply, switch, apply) and `post-commit` (auto-save)
- **startup.rs** — `subcontext startup`: no-op stub for Claude's SessionStart hook
- **settings.rs** — Merge/remove SessionStart hook entry in `.claude/settings.local.json`
- **status.rs** — `subcontext status`: show repo, worktree, and subcontext state
- **uninstall.rs** — `subcontext uninstall`: unapply overlay, restore hooks, clean up

## Key Patterns

**CheckoutContext**: Most overlay operations take a `&CheckoutContext` which carries `main_root` (where `.git/` lives), `checkout_root` (where overlay files are applied — same as main_root for the main checkout, different for worktrees), and `worktree_name`. Commands use `find_main_git_root()` (rejects worktrees); hooks use `find_checkout_context()` (resolves through worktree `.git` files).

**Git operations**: Use `run_git()` for host repo ops, `run_subcontext_git()` for the bare context repo, and `run_work_git()` for overlay work directories. `run_work_git()` takes an explicit work path and sets `GIT_DIR`/`GIT_WORK_TREE` because work dirs live inside `.git/`.

**Error handling**: All functions return `anyhow::Result<T>`. Hooks are special — `post_checkout()` wraps `post_checkout_inner()` and swallows subcontext's own errors, but propagates failures from the user's original (backed-up) hook.

**Overlay mechanics**: Files are **copied** between the overlay work dir and the checkout root. Both-repo files use `--skip-worktree` to hide the overlay version from `git status`; overlay-only files are added to `.git/info/exclude` (with per-worktree tagged sections).

**Branch forking**: `git checkout -b` forks the overlay from the previous branch. Orphan branches (`--orphan`) and unrelated branches (no merge-base) get empty overlays. Git worktrees fork from the main checkout's current overlay.

**Auto-save**: The post-checkout hook auto-saves overlay changes before switching branches. The post-commit hook also auto-saves. This prevents silent data loss.

**Idempotency**: Settings merge, excludes, and hook installation all check for existing state before modifying. Re-running install re-installs hooks and settings.

**Storage layout**:
```
.git/.subcontext/
├── repo/            ← bare git repo (all overlay + config branches)
├── work/            ← worktree checkout of main checkout's overlay
├── worktrees/<id>/  ← worktree checkout for each git worktree's overlay
└── config/          ← worktree checkout of config branch (hooks, agent settings)
```

**Branch scheme**: `config` (shared), `overlay/<sanitized-branch>` (per-host-branch). Branch sanitization: `/` → `-`, strip leading dots.

## Testing

Integration tests in `tests/integration.rs` use real git repos in `/tmp`. Key test helpers:
- `make_test_repo()` — creates a fresh git repo with an initial commit
- `subcontext_ok()` / `subcontext()` — runs the binary (asserting success or returning output)
- `git()` / `git_in_repo()` — run git commands in the host repo or bare context repo
- `test_path()` — builds PATH so hooks resolve to the test binary

Tests require `git` on PATH. All tests create and clean up their own temp directories.
