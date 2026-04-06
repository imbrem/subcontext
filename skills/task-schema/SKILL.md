---
name: task-schema
description: Reference for subcontext task and board data model. Use when you need to understand how tasks, subtasks, boards, and TASK.md frontmatter work.
---

# Task Schema

Tasks in subcontext are stored as TASK.md files with YAML frontmatter.

## TASK.md format

```markdown
---
uuid: <UUID, auto-generated if missing on board commit/push>
title: <optional display title>
kind: <task|goal|todo|tick>
status: <created|active|inactive|done|failed>
description: <one-line summary>
deadline: <ISO8601 UTC timestamp ending in Z, optional>
importance: <number, optional, 0 = normal, 1.0 = important>
completed_at: <ISO8601 UTC timestamp, set automatically on done/fail>
---

# Task Title

Full markdown description, acceptance criteria, context, notes, etc.
```

## Fields

| Field | Required | Values | Notes |
|-------|----------|--------|-------|
| uuid | No (auto-generated) | UUID v4 | Stable identifier. Auto-generated on `board commit` or `board push` if missing. |
| kind | No (default: task) | task, goal, todo, tick | `goal` = high-level objective, `task` = work item, `todo` = small action, `tick` = recurring |
| status | No (default: created) | created, active, inactive, done, failed | Lifecycle state |
| description | No | string | One-line summary shown in listings |
| deadline | No | ISO8601 ending in Z | e.g. `2026-06-01T00:00:00Z` |
| importance | No (default: 0) | float | 0 = normal, 1.0+ = important |
| title | No | string | Display title (name is the lookup key) |

## Hierarchy

Tasks form a tree. In boards, the hierarchy is the directory structure:

```
tasks/                     # overlay directory (configurable)
  TASK.md                  # board root task
  .board.json              # board sync config (auto-managed)
  feature-a/
    TASK.md                # subtask of root
    design/
      TASK.md              # subtask of feature-a
    implement/
      TASK.md              # subtask of feature-a
  feature-b/
    TASK.md                # subtask of root
```

- **Board root**: `tasks/TASK.md` is the board's root task
- **Subtasks**: Each subdirectory with a `TASK.md` is a child of its parent directory's task
- **Nesting**: Unlimited depth
- **Task name**: The directory name IS the task name (used for path resolution)

## Boards vs standalone tasks

**Boards** store all tasks as a directory tree on a single git branch (`object/<board-uuid>`).
This is the preferred model for agent workflows because:
- Tasks are just files in the working tree
- Agents can create/edit/delete tasks with normal file operations
- `board push` syncs everything back; `board pull` refreshes

**Standalone tasks** each get their own git branch. Used for cross-context references.

## Path resolution

Tasks can be referenced by hierarchical paths:
- `.` — current task (set per-branch)
- `..` — parent of current task
- `name` — child of current task
- `name/child` — walk down
- `/name/...` — absolute (via namespace)
- `/.uuid/<uuid>` — direct UUID reference
