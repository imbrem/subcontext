# Add Task Skill

Create or update a task in the subcontext task system by writing a TASK.md file
and running the appropriate subcontext command.

## Creating a new task

1. Write a TASK.md file with YAML frontmatter and a markdown body describing the task:

```markdown
---
name: <short-task-name>
kind: <task|goal|todo|tick>
status: <created|active|inactive>
description: <one-line summary>
deadline: <ISO8601 UTC timestamp ending in Z, optional>
importance: <number, optional, 0 = normal>
---

# <Task Title>

<Full markdown description of the task, acceptance criteria, context, etc.>
```

2. Run: `git subcontext task add --file TASK.md`

3. The command prints the task UUID to stdout. **Capture and report the UUID to the user.**

4. If the task name is not unique, the command prints a WARNING to stdout listing
   the existing UUIDs that share the same name. **Always show this warning to the user**
   so they can distinguish tasks by UUID.

## Updating an existing task

To update a task by name or UUID:

- **From a file:** Edit TASK.md, then run:
  `git subcontext task update <name-or-uuid> --file TASK.md`

- **Individual fields:**
  `git subcontext task update <name-or-uuid> --status active --description "new desc"`

The update command syncs both object.json and TASK.md on the object branch.
If the name is ambiguous (multiple tasks share it), the command will error with
a list of matching UUIDs. Use the UUID directly to resolve ambiguity.

## Viewing a task

`git subcontext task show <name-or-uuid>`

- If a single task matches, prints the full TASK.md content.
- If multiple tasks match the name, prints all matching UUIDs with descriptions.

## Marking tasks done or failed

- `git subcontext task done <name> [--time <ISO8601Z>]`
- `git subcontext task fail <name> [--time <ISO8601Z>]`

## Syncing object.json and TASK.md

`git subcontext object-commit <uuid>` ensures both files are in sync on the
object branch. If one is missing, it generates it from the other.
