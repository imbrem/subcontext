# subcontext

Private, version-controlled context for Git projects.

Subcontext attaches a hidden context repo (`.subcontext/`) to any Git project that automatically shadows your branch structure. Each branch gets its own isolated space for tasks, notes, and agent configuration — useful for AI-assisted workflows where per-branch context matters.

## Features

- **Per-branch context** — `TASK.md` and `NOTES.md` that follow your branches automatically
- **Claude Code integration** — SessionStart hook injects task context into new sessions
- **Shareable** — clone an existing context repo to sync across machines
- **Non-intrusive** — hooks never block git operations; everything lives in `.subcontext/`

## Install from source

Requires [Rust](https://www.rust-lang.org/tools/install) (edition 2024) and `git`.

```bash
git clone https://github.com/anthropics/subcontext.git
cd subcontext
cargo install --path .
```

This places the `subcontext` binary in `~/.cargo/bin/` (make sure it's on your `PATH`).

## Usage

### Initialize in a Git project

```bash
cd your-project
subcontext install
```

This creates `.subcontext/`, installs a `post-checkout` hook, and configures Claude Code's SessionStart hook.

### Clone an existing context repo

```bash
cd your-project
subcontext clone <url>
```

Clones a shared context repo and attaches it to the current project.

### How it works

When you switch branches with `git checkout`, the post-checkout hook automatically switches `.subcontext/` to a matching context branch. Create a `TASK.md` in `.subcontext/` to have it injected into Claude Code sessions on startup.
