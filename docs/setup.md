# Subcontext Setup

## What is subcontext?

Subcontext attaches a private, version-controlled **overlay** to any Git
project. Files in the overlay are materialized directly in the working tree
with "overlay wins" semantics -- if a file exists in both repos, the overlay
version takes precedence. Each host branch gets its own isolated overlay
branch, so your per-branch context (tasks, notes, agent configuration) follows
you automatically.

## Installation

### From source

```bash
git clone <repo-url>
cd subcontext
cargo install --path .
```

This places the `subcontext` binary in `~/.cargo/bin/` (ensure it's on `PATH`).

### Per-project setup

```bash
cd your-project
subcontext install
```

This:
1. Creates `.git/.subcontext/` with a bare context repo
2. Installs `post-checkout` and `post-commit` hook dispatchers
3. Configures a Claude Code `SessionStart` hook
4. Sets up a `git subcontext` alias

### Global (system-level) setup

```bash
subcontext install --global
```

Creates `~/.subcontext/` as a system-wide context store. Per-project installs
automatically register as children of the global context.

### User subcontext

```bash
subcontext install --user
```

Creates a user-scoped context under the global subcontext. Useful for
multi-user machines or separating personal vs. work contexts.

### Cloning a shared context

```bash
cd your-project
subcontext clone <url>
```

Clones an existing context repo and attaches it to the project.

## Storage layout

```
.git/.subcontext/
  repo/            -- bare git repo (all overlay + config branches)
  work/            -- worktree checkout of the current overlay branch
  worktrees/<id>/  -- overlay worktrees for git worktrees
  config/          -- worktree checkout of the config branch
  state/           -- state branch worktree (tasks.db)
```

## Branch scheme

- `config` -- shared configuration (hooks, agent settings, project metadata)
- `overlay/<sanitized-branch>` -- per-host-branch overlay content
- `state` -- SQLite database for tasks

Branch names are sanitized: `/` becomes `-`, leading dots are stripped.

## Claude Code integration

After `subcontext install`, a `SessionStart` hook is added to
`.claude/settings.local.json`:

```json
{
  "hooks": {
    "SessionStart": [{
      "hooks": [{
        "type": "command",
        "command": "git subcontext startup --claude-code"
      }]
    }]
  }
}
```

This runs on every new Claude Code session, allowing subcontext to inject
context. The hook is a no-op stub by default -- extend `startup.rs` to emit
task summaries or other context.

## Uninstalling

```bash
subcontext uninstall
```

Removes overlay files from the working tree, restores original hooks, removes
the git alias, and cleans up `.git/.subcontext/`.
