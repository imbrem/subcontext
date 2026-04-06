---
name: add-task
description: Create, update, or view hierarchical tasks in the subcontext task system using TASK.md files with YAML frontmatter. Use when the user wants to create a new task, update an existing task, view task details, or manage task lifecycle.
---

# Add Task Skill

Create or update a task in the subcontext task system by writing a TASK.md file
and running the appropriate subcontext command.

## Creating a new task

1. Write a TASK.md file with YAML frontmatter and a markdown body describing the task:

```markdown
---
title: <optional display title>
kind: <task|goal|todo|tick>
status: <created|active|inactive>
description: <one-line summary>
deadline: <ISO8601 UTC timestamp ending in Z, optional>
importance: <number, optional, 0 = normal>
subtasks:
  - <child-name-1>
  - <child-name-2>
---

# <Task Title>

<Full markdown description of the task, acceptance criteria, context, etc.>
```

Note: `name:` is **not** stored in the task data. The task's lookup name is
passed as a positional argument and stored in the parent's namespace.

2. Run: `git subcontext task add <name> [--file TASK.md] [--parent <path>]`

   - `<name>` is **required** — it's the lookup name for this task.
   - `--file` optionally provides a TASK.md with additional fields.
   - `--parent` makes this a subtask of the given parent (name/path or `/uuid`).

3. The command prints the task UUID to stdout. **Capture and report the UUID to the user.**

## Hierarchical paths

Tasks form a tree. The current task is set per-branch with `task set`.

- `.` — the current task
- `name` — child of current task
- `name/child` — walk down from current task
- `/uuid` — jump to a task by UUID
- `/uuid/name` — start from UUID, walk down

Use `task roots` to list root task UUIDs (tasks with no parent).

## Setting the current task

```
git subcontext task set <name-or-path>   # set current task for this branch
git subcontext task set                  # unset
```

## Listing subtasks

```
git subcontext task list              # children of current task (or root tasks)
git subcontext task list <path>       # children of a specific task
git subcontext task roots             # list root task UUIDs
```

## Updating an existing task

To update a task by name or UUID:

- **From a file:** Edit TASK.md, then run:
  `git subcontext task update <name-or-uuid> --file TASK.md`

- **Individual fields:**
  `git subcontext task update <name-or-uuid> --status active --description "new desc"`

The update command syncs both object.json and TASK.md on the object branch.

## Viewing a task

`git subcontext task show <name-or-uuid>`

- If a single task matches, prints the full TASK.md content.
- If multiple tasks match the name, prints all matching UUIDs with descriptions.

## Marking tasks done or failed

- `git subcontext task done <name-or-path> [--time <ISO8601Z>]`
- `git subcontext task fail <name-or-path> [--time <ISO8601Z>]`

Supports hierarchical paths (e.g. `parent/child`, `.`).

## Scope flags

- `--user` — operate on the user subcontext
- `--global` — operate on the system subcontext
- `--local` — skip propagating shadow tasks to parent contexts

## Syncing object.json and TASK.md

`git subcontext object-commit <uuid>` ensures both files are in sync on the
object branch. If one is missing, it generates it from the other.
