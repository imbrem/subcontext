---
name: add-task
description: Create a new task in the current branch's board overlay or via CLI commands. Use when the user wants to create a new task, add a subtask, or populate a board.
---

# Add Task

## Preferred: Add via overlay (board workflow)

If a board is pulled into the overlay (e.g. under `tasks/`), create tasks by
writing TASK.md files directly:

1. Create a directory for the new task under the appropriate parent:
   ```
   mkdir -p tasks/my-new-task
   ```

2. Write a TASK.md file:
   ```markdown
   ---
   kind: task
   status: created
   description: One-line summary of the task
   ---

   # My New Task

   Detailed description, acceptance criteria, etc.
   ```

   **You do NOT need to provide a `uuid`.** A UUID is automatically generated
   when you run `board push` or `board commit`.

3. For subtasks, nest directories:
   ```
   mkdir -p tasks/my-new-task/subtask-a
   ```
   And write `tasks/my-new-task/subtask-a/TASK.md`.

4. Push changes to the board:
   ```
   git subcontext board push --path tasks/
   ```

## Alternative: Add via CLI

### To a board
```
git subcontext board add-task <name> --board <board-uuid> [options]
```

Options:
- `--parent <uuid>` — parent task (defaults to board root)
- `--kind <kind>` — task/goal/todo/tick
- `--status <status>` — created/active/inactive
- `--description <text>` — one-line summary
- `--deadline <ISO8601Z>` — deadline timestamp
- `--important [value]` — mark as important (default 1.0)

### Standalone task
```
git subcontext task add <name> [--file TASK.md] [--parent <path>] [options]
```

- `<name>` is required — the lookup name
- `--file` provides a TASK.md with additional fields
- `--parent` makes this a subtask

The command prints the task UUID to stdout.

## Marking tasks done

### Via overlay
Edit the TASK.md frontmatter: change `status: created` to `status: done`.
Then push.

Or delete the task directory and push with `--mark-done`:
```
rm -rf tasks/completed-task
git subcontext board push --path tasks/ --mark-done
```

### Via CLI
```
git subcontext task done <name-or-path>
git subcontext task fail <name-or-path>
```

## Listing tasks

```
git subcontext task list              # children of current task
git subcontext task list <path>       # children of a specific task
git subcontext task roots             # root task UUIDs
```

## Scope flags

- `--user` — operate on the user subcontext
- `--global` — operate on the system subcontext
- `--local` — skip propagating shadow tasks to parent contexts
