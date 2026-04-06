---
name: edit-task
description: Edit, move, delete, or update tasks in the current branch's board overlay. Use when the user wants to modify task status, reorganize tasks, or manage task lifecycle.
---

# Edit Task

## Editing via overlay (preferred)

If a board is pulled into the overlay under `tasks/`, edit tasks by modifying
files directly.

### Update a task's metadata or description

Edit the TASK.md file directly. Change frontmatter fields or the markdown body:

```markdown
---
uuid: <keep existing uuid>
kind: task
status: active
description: Updated description
deadline: 2026-07-01T00:00:00Z
importance: 1
---

# Updated Title

New detailed description...
```

Then push: `git subcontext board push --path tasks/`

### Move a task (change parent)

Move the directory to a new parent:
```bash
mv tasks/old-parent/my-task tasks/new-parent/my-task
```

The task keeps its UUID. After pushing, the DB parent relationship updates
automatically.

### Delete a task

Remove the directory:
```bash
rm -rf tasks/unwanted-task
```

Then push. Two modes:
- `git subcontext board push --path tasks/` — removes the task from the board entirely
- `git subcontext board push --path tasks/ --mark-done` — marks it as done instead

### Add a subtask

Create a new subdirectory with a TASK.md:
```bash
mkdir -p tasks/parent-task/new-subtask
cat > tasks/parent-task/new-subtask/TASK.md << 'EOF'
---
kind: todo
status: created
description: New subtask description
---
EOF
```

No UUID needed — it's generated on push.

### Mark a task as done

Either edit the frontmatter:
```yaml
status: done
```

Or use the CLI: `git subcontext task done <path>`

## Editing via CLI

### Update fields
```
git subcontext task update <name-or-uuid> --status active --description "new desc"
```

### Update from file
```
git subcontext task update <name-or-uuid> --file TASK.md
```

### Move task in a board
```
git subcontext board move-task <task-uuid> --parent <new-parent-uuid> --board <board-uuid>
```

### Delete task from a board
```
git subcontext board delete-task <task-uuid> --board <board-uuid>
```

## Syncing after edits

After editing overlay files:
1. Save the overlay: `git subcontext save -m "update tasks"`
2. Push to board: `git subcontext board push --path tasks/`

After editing via CLI (board commands auto-sync):
- Run `git subcontext board pull <uuid> --path tasks/` to refresh the overlay

## Viewing a task

```
git subcontext task show <name-or-uuid>
```

Shows the full TASK.md content for the task.
