---
name: add-task
description: Create a new task in the pool via CLI. Use when the user wants to add a task, create a subtask, or populate the task pool.
---

# Add Task

## CLI

```bash
subcontext task add "Title" [options]
```

### Options

| Flag          | Description                                |
|---------------|--------------------------------------------|
| `--list X`    | Grouping label (e.g. work, personal)       |
| `--topic Y`   | Topic tag                                  |
| `--type Z`    | Task type: todo (default), goal, task, tick|
| `--status S`  | Initial status: active (default)           |
| `--important` | Mark as important                          |
| `--deadline D`| Deadline (ISO 8601 timestamp)              |
| `--parents 1,2`| Parent task IDs (comma-separated)         |
| `--uuid U`    | Explicit UUID for cross-context references |

### Examples

```bash
# Simple task
subcontext task add "Write documentation"

# Task with metadata
subcontext task add "Deploy v2" --list work --topic infra --deadline 2026-06-01T00:00:00Z

# Important subtask
subcontext task add "Fix auth bug" --important --parents 3

# Task with explicit type
subcontext task add "Weekly review" --type tick
```

The command prints the assigned task ID to stdout.

Tasks are stored in the pool at `.git/.subcontext/pool/tasks/{id}/TASK.md`.

## Completing tasks

```bash
subcontext task done <id>           # mark as done
subcontext task fail <id>           # mark as cancelled
subcontext task done <id> --time T  # with explicit timestamp
```

## Scope flags

- `--global` -- operate on the system-level subcontext
- `--user` -- operate on the user subcontext
- `--local` -- only act on the local (per-repo) subcontext
