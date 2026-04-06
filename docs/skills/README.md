# Sample Skills

These are **sample** Claude Code skills for subcontext task management. They
demonstrate the skill format and common patterns. You can:

1. Use them as-is by copying to `.claude/skills/` in your project
2. Adapt them to your project's specific workflows
3. Let Claude Code read them and create tailored skills for your repository

## How Claude Code skills work

A skill is a directory containing a `SKILL.md` file with YAML frontmatter:

```markdown
---
name: my-skill
description: What this skill does. Claude uses this to decide when to invoke it.
---

# Skill Title

Instructions for Claude Code when this skill is activated.
```

Skills live in:
- **Project-local:** `.claude/skills/` (per-repository, checked in or gitignored)
- **User-global:** `~/.claude/skills/` (available in all projects)

## Creating project-specific skills

The best skills are tailored to your project. Here's how to create one:

1. Create `.claude/skills/<name>/SKILL.md` in your repo
2. Write a description that tells Claude **when** to use the skill
3. Write instructions that tell Claude **how** to use the skill
4. Include project-specific details: file paths, conventions, tools

### Example: project-specific build skill

```markdown
---
name: build-and-test
description: Build the project and run tests. Use when the user asks to build, test, or verify changes.
---

# Build and Test

1. Run `cargo fmt` first (CI requires formatted code)
2. Run `cargo test` to execute the test suite
3. If tests fail, read the error output and fix the issue
4. Re-run until all tests pass
```

### Example: code review skill

```markdown
---
name: review-changes
description: Review staged or recent changes for quality and correctness.
---

# Review Changes

1. Run `git diff --staged` (or `git diff HEAD~1` for the last commit)
2. Check for:
   - Logic errors or edge cases
   - Missing error handling at system boundaries
   - Security issues (injection, XSS, etc.)
   - Style consistency with surrounding code
3. Report findings concisely
```

## Included samples

- **add-task/** -- Create tasks via overlay files or CLI
- **edit-task/** -- Edit, move, delete, and update tasks
- **set-task/** -- Set the current task and manage board overlays
- **task-schema/** -- Reference for TASK.md format and data model
