# Pool Schema

A pool is a flat collection of tasks stored on a single git branch
(`object/<pool-uuid>`). Each task gets a sequential integer ID and a
directory under `tasks/`.

## Layout

```
SCHEMA.md
index.db
tasks/{id}/TASK.md
```

## index.db

SQLite database with the pool index.

### `meta`

| Column | Type | Notes                        |
|--------|------|------------------------------|
| key    | TEXT | PRIMARY KEY                  |
| value  | TEXT | e.g. `next_id` counter       |

### `tasks`

| Column    | Type    | Default   | Notes                                        |
|-----------|---------|-----------|----------------------------------------------|
| id        | INTEGER | PK        | Sequential integer, auto-assigned            |
| uuid      | TEXT    |           | Optional stable UUID (UNIQUE)                |
| list      | TEXT    |           | Grouping label (e.g. work, personal)         |
| topic     | TEXT    |           | Topic tag                                    |
| type      | TEXT    | 'todo'    | todo, goal, task, tick                       |
| status    | TEXT    | 'active'  | active, done, cancelled                      |
| important | INTEGER | 0         | 1 = important                                |
| deadline  | TEXT    |           | ISO 8601 timestamp                           |
| created   | TEXT    |           | ISO 8601 timestamp                           |
| done      | TEXT    |           | ISO 8601 timestamp (set on completion)       |
| cancelled | TEXT    |           | ISO 8601 timestamp (set on failure)          |
| parents   | TEXT    | '[]'      | JSON array of parent task IDs                |
| subtasks  | TEXT    | '[]'      | JSON array of child task IDs                 |

### `open` view

```sql
SELECT list, id FROM tasks
WHERE done IS NULL AND cancelled IS NULL AND list IS NOT NULL;
```

## TASK.md frontmatter

Each task directory contains a `TASK.md` with YAML frontmatter:

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
---
# Task title

Detailed description, notes, acceptance criteria, etc.
```

### Fields

| Field     | Required | Values                    | Notes                              |
|-----------|----------|---------------------------|------------------------------------|
| id        | Yes      | integer                   | Sequential pool ID                 |
| uuid      | No       | UUID v4                   | Stable cross-context identifier    |
| list      | No       | string                    | Grouping label                     |
| topic     | No       | string                    | Topic tag                          |
| type      | No       | todo, goal, task, tick    | Default: todo                      |
| status    | No       | active, done, cancelled   | Default: active                    |
| important | No       | true/false                | Default: false                     |
| deadline  | No       | ISO 8601                  | Optional deadline                  |
| parents   | No       | [int, ...]                | Parent task IDs                    |
| subtasks  | No       | [int, ...]                | Child task IDs                     |
| created   | Auto     | ISO 8601                  | Set on creation                    |
| done      | Auto     | ISO 8601                  | Set by `task done`                 |
| cancelled | Auto     | ISO 8601                  | Set by `task fail`                 |

## Task numbering

IDs are sequential integers starting at 1, allocated from the `next_id`
counter in `meta`. If a directory `tasks/{id}/` already exists (e.g. from
a manual creation), the allocator skips to the next available ID.
