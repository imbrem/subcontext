use anyhow::Result;
use serde_json::json;
use std::fs;
use std::path::Path;

use crate::git::current_branch;

/// Run `subcontext startup`. Outputs JSON for Claude's SessionStart hook.
///
/// The JSON format uses `additionalContext` to inject text into Claude's
/// context, which is the structured way to communicate with SessionStart.
/// This lets us add more fields (e.g. `continue`, env vars) later.
pub fn startup(root: &Path) -> Result<()> {
    let task_path = root.join(".subcontext").join("TASK.md");

    if !task_path.exists() {
        return Ok(());
    }

    let contents = fs::read_to_string(&task_path)?;
    if contents.trim().is_empty() {
        return Ok(());
    }

    let branch = current_branch(root).unwrap_or_else(|_| "unknown".to_string());

    let context = format!(
        "\
Active worktree: {branch}

Current task:

{contents}"
    );

    let output = json!({
        "additionalContext": context,
        "continue": true,
    });

    println!("{}", serde_json::to_string(&output)?);

    Ok(())
}
