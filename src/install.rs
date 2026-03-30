use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use crate::git::{
    CheckoutContext, config_dir, current_branch, repo_dir, run_git, run_subcontext_git,
    sanitize_branch_name, subcontext_dir, work_dir,
};
use crate::overlay;
use crate::settings::merge_claude_settings;

/// Run `subcontext install` from the given repo root.
pub fn install(root: &Path, repair: bool) -> Result<()> {
    let sc_dir = subcontext_dir(root);
    let branch = current_branch(root)?;

    if sc_dir.exists() {
        eprintln!("[subcontext] .git/.subcontext/ exists — re-installing hooks and settings...");
    } else {
        eprintln!("[subcontext] Initializing context repo...");
        init_context_repo(root, &branch)?;
    }

    install_git_alias(root)?;
    install_from_hooks(root, repair)?;

    print_summary(&branch);
    Ok(())
}

/// Shared steps: hooks, excludes, settings, config commit. Used by install and clone.
pub fn install_from_hooks(root: &Path, repair: bool) -> Result<()> {
    // Backup existing hooks
    let pc_is_ours = hook_dispatches_to_subcontext(root, "post-checkout");
    let pcm_is_ours = hook_dispatches_to_subcontext(root, "post-commit");
    backup_existing_hooks(root, repair, pc_is_ours, pcm_is_ours)?;

    // Install hook dispatchers
    if pc_is_ours && !repair {
        eprintln!(
            "[subcontext] post-checkout hook already dispatches to subcontext — \
             leaving it in place (use --repair to overwrite)"
        );
    } else {
        install_hook_dispatcher(root, "post-checkout")?;
    }

    if pcm_is_ours && !repair {
        eprintln!(
            "[subcontext] post-commit hook already dispatches to subcontext — \
             leaving it in place (use --repair to overwrite)"
        );
    } else {
        install_hook_dispatcher(root, "post-commit")?;
    }

    // Sync excludes
    let ctx = CheckoutContext::main_only(root);
    overlay::sync_excludes(&ctx)?;

    // Merge Claude settings
    merge_claude_settings(root)?;

    // Commit config branch
    commit_config_branch(root)?;

    Ok(())
}

/// Install a local git alias so `git subcontext` dispatches to the `subcontext` binary.
fn install_git_alias(root: &Path) -> Result<()> {
    // Resolve the absolute path to the currently running binary
    let exe = std::env::current_exe().context("failed to resolve subcontext binary path")?;
    let exe_str = exe.to_string_lossy();
    let alias_value = format!("!{exe_str}");

    run_git(&["config", "alias.subcontext", &alias_value], root)?;
    eprintln!("[subcontext] Configured git alias: git subcontext → {exe_str}");
    Ok(())
}

/// Initialize the subcontext bare repo, config branch/worktree, and first overlay branch.
fn init_context_repo(root: &Path, host_branch: &str) -> Result<()> {
    let sc_dir = subcontext_dir(root);
    let repo = repo_dir(root);

    fs::create_dir_all(&sc_dir)?;

    // 1. Init bare repo
    run_git(&["init", "--bare", &repo.to_string_lossy()], root)?;

    // 2. Create config branch via plumbing
    let empty_tree = run_subcontext_git(&["hash-object", "-t", "tree", "/dev/null"], root)?;
    let config_commit = run_subcontext_git(
        &["commit-tree", &empty_tree, "-m", "init config branch"],
        root,
    )?;
    run_subcontext_git(&["update-ref", "refs/heads/config", &config_commit], root)?;

    // 3. Add config worktree
    let cfg = config_dir(root);
    run_subcontext_git(&["worktree", "add", &cfg.to_string_lossy(), "config"], root)?;

    // 4. Create overlay/<current-branch> via plumbing (empty)
    let safe_branch = sanitize_branch_name(host_branch);
    let overlay_branch = format!("overlay/{safe_branch}");
    overlay::create_overlay_branch(root, &overlay_branch)?;

    // 5. Add work/ worktree pointing to overlay branch
    let work = work_dir(root);
    run_subcontext_git(
        &["worktree", "add", &work.to_string_lossy(), &overlay_branch],
        root,
    )?;

    Ok(())
}

/// Check whether a hook dispatches to subcontext.
fn hook_dispatches_to_subcontext(root: &Path, hook_name: &str) -> bool {
    let hook_path = root.join(".git").join("hooks").join(hook_name);
    let content = match fs::read_to_string(&hook_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    content.lines().any(line_invokes_subcontext)
}

/// Check whether a shell script line (outside of comments) invokes subcontext.
fn line_invokes_subcontext(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return false;
    }
    let mut rest = trimmed;
    while let Some(pos) = rest.find("subcontext") {
        if pos == 0 {
            return true;
        }
        let prev = rest.as_bytes()[pos - 1];
        if prev == b' '
            || prev == b'\t'
            || prev == b'/'
            || prev == b'='
            || prev == b'"'
            || prev == b'\''
        {
            return true;
        }
        rest = &rest[pos + "subcontext".len()..];
    }
    false
}

/// Backup existing hooks to config/hooks/old/.
fn backup_existing_hooks(
    root: &Path,
    repair: bool,
    pc_is_ours: bool,
    pcm_is_ours: bool,
) -> Result<()> {
    let hooks_dir = root.join(".git").join("hooks");
    if !hooks_dir.exists() {
        return Ok(());
    }

    let cfg = config_dir(root);
    let old_dir = cfg.join("hooks").join("old");
    let backup_dir = cfg.join("hooks").join("backup");

    for entry in fs::read_dir(&hooks_dir)? {
        let entry = entry?;
        let path = entry.path();

        if !path.is_file() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.ends_with(".sample")
        {
            continue;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::metadata(&path)?.permissions();
            if perms.mode() & 0o111 == 0 {
                continue;
            }
        }

        let hook_name = path.file_name().unwrap().to_string_lossy().to_string();

        let is_ours = (hook_name == "post-checkout" && pc_is_ours)
            || (hook_name == "post-commit" && pcm_is_ours);

        if is_ours {
            if repair {
                fs::create_dir_all(&backup_dir)?;
                let dest = backup_dir.join(&hook_name);
                fs::copy(&path, &dest)?;
                eprintln!(
                    "[subcontext] Saved existing subcontext hook to hooks/backup/{hook_name}"
                );
            }
            continue;
        }

        fs::create_dir_all(&old_dir)?;
        let dest = old_dir.join(&hook_name);
        fs::copy(&path, &dest)?;
        eprintln!("[subcontext] Backed up hook: {hook_name}");
    }

    Ok(())
}

/// Install a hook dispatcher script.
fn install_hook_dispatcher(root: &Path, hook_name: &str) -> Result<()> {
    let hooks_dir = root.join(".git").join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join(hook_name);
    let script = format!(
        r#"#!/bin/sh
# Installed by subcontext. Dispatches to `git subcontext _hook {hook_name}`.
# Your original hook (if any) is backed up and called automatically.
exec git subcontext _hook {hook_name} "$@"
"#
    );

    fs::write(&hook_path, script).with_context(|| format!("failed to write {hook_name} hook"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&hook_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook_path, perms)?;
    }

    eprintln!("[subcontext] Installed {hook_name} hook dispatcher.");
    Ok(())
}

/// Commit everything on the config branch.
fn commit_config_branch(root: &Path) -> Result<()> {
    let cfg = config_dir(root);
    if !cfg.exists() {
        return Ok(());
    }

    run_git(&["add", "-A"], &cfg)?;

    let status = run_git(&["status", "--porcelain"], &cfg)?;
    if status.is_empty() {
        return Ok(());
    }

    run_git(&["commit", "-m", "subcontext: update config"], &cfg)?;

    Ok(())
}

fn print_summary(branch: &str) {
    let safe = sanitize_branch_name(branch);
    eprintln!();
    eprintln!("[subcontext] Installation complete!");
    eprintln!("  Context repo:  .git/.subcontext/repo/");
    eprintln!("  Config mount:  .git/.subcontext/config/");
    eprintln!("  Work mount:    .git/.subcontext/work/");
    eprintln!("  Overlay branch: overlay/{safe}");
    eprintln!("  Hooks:         .git/hooks/post-checkout, .git/hooks/post-commit");
    eprintln!();
    eprintln!("  Use `git subcontext add <file>` to add files to the overlay.");
}
