use anyhow::Result;
use std::path::Path;

use crate::backend::Backend;

/// Each bundled doc file: (relative path, content).
const BUNDLED_DOCS: &[(&str, &str)] = &[
    ("README.md", include_str!("../docs/README.md")),
    ("setup.md", include_str!("../docs/setup.md")),
    ("usage.md", include_str!("../docs/usage.md")),
    ("skills/README.md", include_str!("../docs/skills/README.md")),
    (
        "skills/add-task/SKILL.md",
        include_str!("../docs/skills/add-task/SKILL.md"),
    ),
    (
        "skills/edit-task/SKILL.md",
        include_str!("../docs/skills/edit-task/SKILL.md"),
    ),
    (
        "skills/set-task/SKILL.md",
        include_str!("../docs/skills/set-task/SKILL.md"),
    ),
    (
        "skills/task-schema/SKILL.md",
        include_str!("../docs/skills/task-schema/SKILL.md"),
    ),
];

/// Dump all bundled documentation to `dest`. Creates directories as needed.
/// Overwrites existing files.
pub fn dump_docs(backend: &dyn Backend, dest: &Path) -> Result<()> {
    backend.create_dir_all(dest)?;

    for (rel_path, content) in BUNDLED_DOCS {
        let target = dest.join(rel_path);
        if let Some(parent) = target.parent() {
            backend.create_dir_all(parent)?;
        }
        backend.write(&target, content.as_bytes())?;
        eprintln!("[subcontext] Wrote {}", target.display());
    }

    eprintln!(
        "[subcontext] Dumped {} doc files to {}",
        BUNDLED_DOCS.len(),
        dest.display()
    );
    Ok(())
}
