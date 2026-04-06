# Subcontext Documentation

This directory contains setup guides, usage documentation, and sample skills
for subcontext. Use `subcontext docs <path>` to dump these files into any
directory (e.g. your project's overlay) so that Claude Code can read them and
create project-specific skills.

## Contents

- **setup.md** -- Getting started with subcontext: installation, first overlay,
  branch workflow, and Claude Code integration.
- **usage.md** -- High-level usage patterns, idioms, and command reference.
- **skills/** -- Sample Claude Code skills for task management. These are
  starting points; Claude Code can adapt them to your project's specific needs.

## Quick start

```bash
# Install subcontext in your project
cd your-project
subcontext install

# Dump docs into your overlay so Claude Code can read them
subcontext docs docs/subcontext/
subcontext add docs/subcontext/
subcontext save -m "add subcontext docs"
```

Claude Code will then see the sample skills and setup guide in the working tree
and can create tailored per-repository skills in `.claude/skills/`.
