use anyhow::Result;
use std::path::Path;

use crate::backend::Backend;

/// Each bundled skill: (directory_name, SKILL.md content).
const BUNDLED_SKILLS: &[(&str, &str)] = &[
    ("add-task", include_str!("../skills/add-task/SKILL.md")),
    (
        "task-schema",
        include_str!("../skills/task-schema/SKILL.md"),
    ),
    ("set-task", include_str!("../skills/set-task/SKILL.md")),
    ("edit-task", include_str!("../skills/edit-task/SKILL.md")),
];

/// Install all bundled skills into the global Claude Code skills directory
/// (`~/.claude/skills/`). If a skill directory already exists, print a
/// warning and skip it.
pub fn install_skills(backend: &dyn Backend) -> Result<()> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| anyhow::anyhow!("cannot determine home directory"))?;
    let skills_dir = Path::new(&home).join(".claude").join("skills");
    backend.create_dir_all(&skills_dir)?;

    let mut installed = 0;
    let mut skipped = 0;

    for (name, content) in BUNDLED_SKILLS {
        let dest_dir = skills_dir.join(name);
        let dest_file = dest_dir.join("SKILL.md");

        if backend.exists(&dest_dir) {
            eprintln!(
                "[subcontext] Skill '{}' already exists at {} — skipping",
                name,
                dest_dir.display()
            );
            skipped += 1;
            continue;
        }

        backend.create_dir_all(&dest_dir)?;
        backend.write(&dest_file, content.as_bytes())?;
        eprintln!(
            "[subcontext] Installed skill '{}' to {}",
            name,
            dest_dir.display()
        );
        installed += 1;
    }

    if installed > 0 {
        eprintln!("[subcontext] Installed {installed} skill(s).");
    }
    if skipped > 0 {
        eprintln!("[subcontext] Skipped {skipped} existing skill(s).");
    }
    if installed == 0 && skipped == 0 {
        eprintln!("[subcontext] No skills to install.");
    }

    Ok(())
}
