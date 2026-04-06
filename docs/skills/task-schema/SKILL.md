---
name: task-schema
description: Reference for subcontext pool and task data model. Use when you need to understand how pools, tasks, TASK.md frontmatter, and index.db work.
---

# Task Schema

Tasks in subcontext are stored in a **pool** -- a flat collection of tasks
on a single git branch (`object/<pool-uuid>`).

## Pool layout

```
.git/.subcontext/pool/
  SCHEMA.md          # schema documentation
  index.db           # SQLite index (meta, tasks, open view)
  tasks/
    1/TASK.md        # task #1
    2/TASK.md        # task #2
    ...
```

## TASK.md format

```markdown
---
id: 1
uuid: <optional UUID>
list: work
topic: infra
type: todo
status: active
important: true
deadline: 2026-06-01T00:00:00Z
parents: [3]
subtasks: [4, 5]
created: 2026-04-06T12:00:00Z
done: <set on completion>
cancelled: <set on failure>
---
# Task title

Detailed description, acceptance criteria, notes, etc.
```

## Frontmatter fields

| Field     | Required | Values                    | Notes                              |
|-----------|----------|---------------------------|------------------------------------|
| id        | Yes      | integer                   | Sequential pool ID                 |
| uuid      | No       | UUID v4                   | Stable cross-context identifier    |
| list      | No       | string                    | Grouping label (e.g. work, personal)|
| topic     | No       | string                    | Topic tag                          |
| type      | No       | todo, goal, task, tick    | Default: todo                      |
| status    | No       | active, done, cancelled   | Default: active                    |
| important | No       | true/false                | Default: false                     |
| deadline  | No       | ISO 8601                  | Optional deadline                  |
| parents   | No       | [int, ...]                | Parent task IDs                    |
| subtasks  | No       | [int, ...]                | Child task IDs (auto-maintained)   |
| created   | Auto     | ISO 8601                  | Set on creation                    |
| done      | Auto     | ISO 8601                  | Set by `task done`                 |
| cancelled | Auto     | ISO 8601                  | Set by `task fail`                 |

## index.db

SQLite database with the pool index:

- **`meta`** -- key/value store (holds `next_id` counter)
- **`tasks`** -- one row per task, mirrors frontmatter fields
- **`open`** view -- active tasks with a list set: `SELECT list, id FROM tasks WHERE done IS NULL AND cancelled IS NULL AND list IS NOT NULL`

## Task numbering

IDs are sequential integers starting at 1. The `next_id` counter in
`meta` is incremented on each `task add`. If a directory `tasks/{id}/`
already exists, the allocator skips to the next available ID.

## CLI commands

```bash
subcontext task add "Title" [--list X] [--topic Y] [--type Z] [--important] [--deadline D] [--parents 1,2]
subcontext task show <id>
subcontext task update <id> [--title T] [--list X] [--topic Y] [--type Z] [--status S] [--important B] [--deadline D]
subcontext task done <id> [--time T]
subcontext task fail <id> [--time T]
subcontext task deadlines [--important] [--horizon 2w]
```
