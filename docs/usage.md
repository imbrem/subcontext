# Subcontext Usage & Idioms

## Core workflow

### Adding files to the overlay

```bash
subcontext add TASK.md NOTES.md
subcontext save -m "add task and notes"
```

Files are copied into the overlay work directory and tracked by the context
repo. Overlay-only files are excluded from `git status` via
`.git/info/exclude`. Files that exist in both repos use `--skip-worktree`.

### Removing files

```bash
subcontext remove TASK.md
```

If the file also exists in the host repo, the original version is restored.
Otherwise it's deleted from the working tree.

### Saving changes

```bash
subcontext save -m "update tasks"
```

Commits overlay changes to the context repo. The post-commit hook also
auto-saves, so manual saves are mainly for explicit checkpoints.

### Checking status

```bash
subcontext status
```

Shows the host repo, overlay branch, pending changes, and subcontext state.

## Branch workflow

Overlay branches track host branches automatically:

- **`git checkout -b feature`** -- forks the overlay from the current branch
- **`git checkout main`** -- switches to the `overlay/main` branch
- **`git checkout --orphan x`** -- gets an empty overlay
- **`git worktree add`** -- forks overlay from the main checkout

The `post-checkout` hook handles auto-save before switching and auto-apply
after switching. You never need to manage overlay branches manually.

## Task management

### Standalone tasks

```bash
subcontext task add my-task --kind task --description "Do the thing"
subcontext task show my-task
subcontext task done my-task
subcontext task list
```

### Boards (task trees)

Boards store related tasks as a directory tree on a single branch:

```bash
# Create a board
subcontext board create sprint-1 --kind goal --description "Sprint 1"

# Pull into overlay
subcontext board pull <uuid> --path tasks/

# Edit tasks as files
echo '---
kind: task
status: created
description: Implement feature X
---' > tasks/feature-x/TASK.md

# Push back
subcontext board push --path tasks/
```

### Task paths

Tasks can be referenced by hierarchical paths:
- `.` -- current task
- `..` -- parent
- `name` -- child of current
- `name/child` -- nested
- `/name` -- absolute (via namespace)

### Scope flags

- `--global` -- system-level subcontext
- `--user` -- user subcontext
- `--local` -- skip propagating shadow tasks to parents

## Submodules

```bash
subcontext submodule add <url> [path]
subcontext submodule update
subcontext submodule remove <path>
```

Manage git submodules within the overlay.

## Namespaces

```bash
subcontext namespace set myproject <uuid>
subcontext namespace get myproject
subcontext namespace list
subcontext namespace remove myproject
```

Map human-readable names to UUIDs for path resolution.

## MCP server

```bash
subcontext mcp
```

Runs a JSON-RPC 2.0 MCP server over stdio exposing `subcontext_status` and
`subcontext_deadlines` tools. To use with Claude Code, add to your
`~/.claude.json` or project `.claude/settings.local.json`:

```json
{
  "mcpServers": {
    "subcontext": {
      "type": "stdio",
      "command": "/path/to/subcontext",
      "args": ["mcp"]
    }
  }
}
```

## Idioms and best practices

1. **Let auto-save work for you.** The hooks save overlay changes on every
   branch switch and commit. You rarely need explicit `subcontext save`.

2. **Use boards for agent workflows.** Boards let agents create and manage
   tasks as plain files. `board pull` / `board push` syncs the file tree with
   the database.

3. **One overlay directory per board.** Convention is `tasks/` but any path
   works. The board config is stored in `<path>/.board.json`.

4. **Overlay wins.** If you need to override a file from the host repo, just
   `subcontext add` it. The overlay version takes precedence.

5. **Per-branch isolation.** Each branch has its own overlay space. Use this
   to keep feature-specific tasks and notes separate.

6. **Propagation.** When you add a task locally, it automatically propagates
   as a shadow task to parent contexts (user, then global) unless you pass
   `--local`.

7. **Dump docs into your overlay.** Run `subcontext docs <path>` to get
   setup guides and sample skills. Claude Code can read these and create
   project-specific skills.
