# subcontext

Private, version-controlled context for Git projects.

Subcontext attaches a hidden context repo (`.subcontext/`) to any Git project that automatically shadows your branch structure. Each branch gets its own isolated space for tasks, notes, and agent configuration -- useful for AI-assisted workflows where per-branch context matters.

## Features

- **Per-branch context** -- overlay files that follow your branches automatically
- **Task management** -- boards, deadlines, hierarchical tasks with CLI and file-based workflows
- **Claude Code integration** -- SessionStart hook, sample skills, and MCP server
- **Shareable** -- clone an existing context repo to sync across machines
- **Non-intrusive** -- hooks never block git operations; everything lives in `.subcontext/`

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

This creates `.git/.subcontext/`, installs hook dispatchers, and configures Claude Code's SessionStart hook.

### Add files to the overlay

```bash
subcontext add TASK.md NOTES.md
subcontext save -m "add context files"
```

### Clone an existing context repo

```bash
cd your-project
subcontext clone <url>
```

### Dump documentation and sample skills

```bash
subcontext docs docs/subcontext/
```

Writes setup guides, usage documentation, and sample Claude Code skills to the given directory. Add them to your overlay so Claude Code can read them and create project-specific skills:

```bash
subcontext add docs/subcontext/
subcontext save -m "add subcontext docs"
```

### How it works

When you switch branches with `git checkout`, the post-checkout hook automatically saves overlay changes, switches to the matching overlay branch, and applies it. The post-commit hook auto-saves. You never need to manage overlay branches manually.

### Task management

```bash
subcontext task add my-task --description "Do the thing"
subcontext board create sprint-1 --kind goal
subcontext board pull <uuid> --path tasks/
# Edit task files directly, then:
subcontext board push --path tasks/
```

See `subcontext docs <path>` for full usage documentation.
