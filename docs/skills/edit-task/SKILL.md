---
name: edit-task
description: Edit, update, complete, or cancel tasks in the pool. Use when the user wants to modify task fields, mark tasks done, or manage task lifecycle.
---

# Edit Task

## Update via CLI

```bash
subcontext task update <id> [options]
```

### Options

| Flag          | Description                          |
|---------------|--------------------------------------|
| `--title T`   | Updated title                        |
| `--list X`    | Updated list                         |
| `--topic Y`   | Updated topic                        |
| `--type Z`    | Updated type (todo, goal, task, tick)|
| `--status S`  | Updated status (active, done, cancelled)|
| `--important` | Updated importance (true/false)      |
| `--deadline D`| Updated deadline (ISO 8601)          |

### Examples

```bash
subcontext task update 3 --status active --topic infra
subcontext task update 5 --deadline 2026-07-01T00:00:00Z --important true
subcontext task update 1 --title "Revised title"
```

## Complete or cancel

```bash
subcontext task done <id>                         # mark done
subcontext task done <id> --time 2026-04-06T12:00:00Z  # explicit timestamp
subcontext task fail <id>                         # mark cancelled
```

## View a task

```bash
subcontext task show <id>
```

Prints the full TASK.md content for the task.

## Direct file editing

Task files live at `.git/.subcontext/pool/tasks/{id}/TASK.md`. You can
edit frontmatter fields or the markdown body directly, then the next
`subcontext task` command will pick up your changes.

```markdown
---
id: 3
list: work
topic: infra
type: todo
status: active
important: true
deadline: 2026-06-01T00:00:00Z
parents: [1]
subtasks: [4, 5]
created: 2026-04-01T00:00:00Z
---
# Deploy v2

Updated description and acceptance criteria...
```

## Scope flags

- `--global` -- operate on the system-level subcontext
- `--user` -- operate on the user subcontext
- `--local` -- only act on the local (per-repo) subcontext
