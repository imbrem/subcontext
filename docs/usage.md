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

Tasks are stored in a **pool** -- a flat collection with sequential integer
IDs. The pool lives at `.git/.subcontext/pool/` and is created automatically
on `subcontext install`.

### Adding tasks

```bash
subcontext task add "Write documentation"
subcontext task add "Deploy v2" --list work --topic infra --deadline 2026-06-01T00:00:00Z
subcontext task add "Fix auth bug" --important --parents 3
```

### Viewing tasks

```bash
subcontext task show <id>
```

### Updating tasks

```bash
subcontext task update <id> --status active --topic infra
subcontext task update <id> --title "New title" --deadline 2026-07-01T00:00:00Z
```

### Completing tasks

```bash
subcontext task done <id>
subcontext task fail <id>
subcontext task done <id> --time 2026-04-06T12:00:00Z
```

### Deadlines

```bash
subcontext task deadlines
subcontext task deadlines --important
subcontext task deadlines --horizon 2w
```

### Scope flags

- `--global` -- system-level subcontext
- `--user` -- user subcontext
- `--local` -- only act on the local (per-repo) subcontext

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

2. **Use the pool for task tracking.** Tasks are stored in a flat pool with
   sequential IDs. Use `task add`, `task done`, and `task update` to manage
   them.

3. **Overlay wins.** If you need to override a file from the host repo, just
   `subcontext add` it. The overlay version takes precedence.

4. **Per-branch isolation.** Each branch has its own overlay space. Use this
   to keep feature-specific tasks and notes separate.

5. **Dump docs into your overlay.** Run `subcontext docs <path>` to get
   setup guides and sample skills. Claude Code can read these and create
   project-specific skills.
