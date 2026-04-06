---
name: set-task
description: Set or switch the current task for this branch, initializing the tasks/ overlay directory from a board. Use when starting work on a task, switching tasks, or setting up the task overlay.
---

# Set Task

Set the current branch's active task and sync the board overlay.

## Setting up a board overlay for the first time

1. Create a board (if one doesn't exist):
   ```
   git subcontext board create <board-name> --kind goal --description "Board description"
   ```
   This prints the board UUID.

2. Pull the board into the overlay:
   ```
   git subcontext board pull <board-uuid> --path tasks/
   ```
   This materializes the board tree under `tasks/` in the working directory.
   - Use `--filter-done` to exclude completed/failed tasks
   - Use `--task <uuid>` to pull only a specific subtask's subtree
   - Use `--path <dir>/` to customize the overlay directory (must end with `/`)

3. Set the current task:
   ```
   git subcontext task set <task-name-or-path>
   ```

## Switching the current task

```
git subcontext task set <name-or-path>
```

Examples:
- `git subcontext task set feature-a` — set to a child of current task
- `git subcontext task set ..` — go up to parent
- `git subcontext task set .` — keep current (no-op)

To unset: `git subcontext task set` (no argument)

## Switching to a different board

1. Push current changes:
   ```
   git subcontext board push --path tasks/
   ```

2. Remove the current overlay files (they're tracked in the overlay, so just remove them):
   ```
   git subcontext remove tasks/.board.json
   git subcontext remove tasks/TASK.md
   # ... remove all overlay files under tasks/
   ```

3. Pull the new board:
   ```
   git subcontext board pull <new-board-uuid> --path tasks/
   ```

## Refreshing from the board

To re-sync overlay files from the board (e.g., after another agent updated it):
```
git subcontext board pull <board-uuid> --path tasks/
```

## Custom overlay paths

The overlay path is configurable per use. Common patterns:
- `tasks/` — default, recommended
- `.tasks/` — hidden directory
- `context/tasks/` — nested under a context directory

The path is stored in `tasks/.board.json` so `board push` knows where to read from.

## Saving overlay state

After editing task files, save the overlay:
```
git subcontext save -m "update tasks"
```

Then push to the board:
```
git subcontext board push --path tasks/
```

Use `--mark-done` with push to mark deleted tasks as done instead of removing them from the board:
```
git subcontext board push --path tasks/ --mark-done
```
