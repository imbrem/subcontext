# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

Subcontext is a Rust CLI that attaches a private, version-controlled context repo (`.subcontext/`) to any Git project. It shadows the host repo's branch structure so each branch gets isolated context (tasks, notes, agent config).

## Build & Test

```bash
cargo build                  # debug build
cargo test                   # run all tests (integration suite in tests/integration.rs)
cargo test <test_name>       # run a single test, e.g. cargo test install_creates
cargo run -- <command>       # run subcontext commands (install, clone, startup, _hook)
```

No linter or formatter is configured — use `cargo clippy` and `cargo fmt` as needed.

## Architecture

All source lives in `src/` with one module per command:

- **main.rs** — Clap derive CLI parser and command dispatch
- **git.rs** — `run_git()` helper (all git ops go through this, no git2 crate), `find_main_git_root()`, `current_branch()`, `sanitize_branch_name()`
- **install.rs** — `subcontext install`: 11-step initialization (context repo, config worktree, excludes, hooks, Claude settings)
- **clone.rs** — `subcontext clone <url>`: clone existing context repo then run install steps 5+
- **hook.rs** — `subcontext _hook post-checkout`: switch context branch on checkout, create orphan worktree branches as needed
- **startup.rs** — `subcontext startup`: output TASK.md as JSON for Claude's SessionStart hook
- **settings.rs** — Merge SessionStart hook entry into `.claude/settings.local.json` (idempotent)

## Key Patterns

**Git operations**: Always use `run_git()` from `git.rs` — never spawn `Command::new("git")` directly. This centralizes error handling and ensures stderr is captured in error messages.

**Error handling**: All functions return `anyhow::Result<T>`. Hooks are special — `post_checkout()` wraps `post_checkout_inner()` and swallows subcontext's own errors, but propagates failures from the user's original (backed-up) hook.

**Idempotency**: Settings merge, excludes, and hook installation all check for existing state before modifying. Re-running install is refused (checks for `.subcontext/` existence), but individual operations are safe to repeat.

**Context repo layout**:
```
.subcontext/
├── .mnt/config/          ← config branch worktree (hooks, agent settings)
├── .tmp/                 ← ephemeral scratch (gitignored)
├── TASK.md               ← per-branch task context
└── NOTES.md              ← per-branch free-form notes
```

## Testing

Integration tests in `tests/integration.rs` use real git repos in `/tmp`. Key test helpers:
- `make_test_repo()` — creates a fresh git repo
- `subcontext_ok()` — runs the binary and asserts exit 0
- `test_path()` — builds PATH so hooks resolve to the test binary

Tests require `git` on PATH. All tests create and clean up their own temp directories.