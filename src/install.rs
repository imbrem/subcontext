use anyhow::Result;
use std::path::Path;

use crate::backend::Backend;
use crate::git::{
    CheckoutContext, config_dir, current_branch, repo_dir, run_git, run_git_in_bare,
    run_subcontext_git, sanitize_branch_name, subcontext_dir, work_dir,
};
use crate::global;
use crate::overlay;
use crate::project::{SUBCONTEXT_KIND, ensure_project_config};
use crate::settings::merge_claude_settings;
use crate::task;

/// Run `subcontext install` from the given repo root.
pub fn install(backend: &dyn Backend, root: &Path, repair: bool) -> Result<()> {
    let sc_dir = subcontext_dir(root);
    let branch = current_branch(backend, root)?;

    if backend.exists(&sc_dir) {
        eprintln!("[subcontext] .git/.subcontext/ exists — re-installing hooks and settings...");
    } else {
        eprintln!("[subcontext] Initializing context repo...");
        init_context_repo(backend, root, &branch)?;
    }

    install_git_alias(backend, root)?;
    install_from_hooks(backend, root, repair)?;

    print_summary(&branch);
    Ok(())
}

/// Shared steps: hooks, excludes, settings, config commit. Used by install and clone.
pub fn install_from_hooks(backend: &dyn Backend, root: &Path, repair: bool) -> Result<()> {
    // Backup existing hooks
    let pc_is_ours = hook_dispatches_to_subcontext(backend, root, "post-checkout");
    let pcm_is_ours = hook_dispatches_to_subcontext(backend, root, "post-commit");
    backup_existing_hooks(backend, root, repair, pc_is_ours, pcm_is_ours)?;

    // Install hook dispatchers
    if pc_is_ours && !repair {
        eprintln!(
            "[subcontext] post-checkout hook already dispatches to subcontext — \
             leaving it in place (use --repair to overwrite)"
        );
    } else {
        install_hook_dispatcher(backend, root, "post-checkout")?;
    }

    if pcm_is_ours && !repair {
        eprintln!(
            "[subcontext] post-commit hook already dispatches to subcontext — \
             leaving it in place (use --repair to overwrite)"
        );
    } else {
        install_hook_dispatcher(backend, root, "post-commit")?;
    }

    // Sync excludes
    let ctx = CheckoutContext::main_only(root);
    overlay::sync_excludes(backend, &ctx)?;

    // Merge Claude settings
    merge_claude_settings(backend, root)?;

    // Ensure project config (UUID, kind, version) is present on config branch
    let project_uuid = ensure_project_config(backend, root)?;

    // Commit config branch
    commit_config_branch(backend, root)?;

    // If a global subcontext exists on this machine, register this project
    // as a child of it (creates an object/<uuid> branch in the global bare
    // repo with object.json containing child data).
    if let Some(commit) = global::register_child(backend, &project_uuid, SUBCONTEXT_KIND)? {
        // Record the managed child in the global objects table.
        if let Ok(global_scope) = global::global_task_scope(backend) {
            let conn = task::open_db(&global_scope)?;
            task::insert_object(&conn, &project_uuid, "managed", &commit, None)?;
            drop(conn);
            task::commit_state_in(
                backend,
                &global_scope.state_dir,
                &format!("object add: {project_uuid}"),
            )?;
        }
    }

    // If there's a current user context, register the project as a child of
    // the user and record the parent relationship in the system DB.
    if global::global_exists(backend)? {
        if let Ok(Some(user_uuid)) = global::get_current_user(backend) {
            // Register as child of the user context.
            let user_scope = global::user_task_scope(backend);
            if let Ok(user_scope) = user_scope {
                // Register child in user context's bare repo.
                let user_repo = user_scope.repo_dir.clone();
                let ref_name = format!("refs/heads/object/{project_uuid}");
                if run_git_in_bare(
                    backend,
                    &["show-ref", "--verify", "--quiet", &ref_name],
                    &user_repo,
                    &user_repo,
                )
                .is_err()
                {
                    // Create child object branch in user's repo.
                    let child_data = serde_json::json!({
                        "uuid": project_uuid,
                        "kind": SUBCONTEXT_KIND,
                    });
                    let object_json = task::build_child_object_json(&child_data);
                    let commit = task::create_object_branch(
                        backend,
                        &user_scope,
                        &project_uuid,
                        &object_json,
                    )?;
                    let conn = task::open_db(&user_scope)?;
                    task::insert_object(&conn, &project_uuid, "managed", &commit, None)?;
                    drop(conn);
                    task::commit_state_in(
                        backend,
                        &user_scope.state_dir,
                        &format!("object add: {project_uuid}"),
                    )?;
                }
            }
            // Record parent relationship in system DB.
            global::set_parent(backend, &project_uuid, &user_uuid).ok();
        }
    }

    Ok(())
}

/// Install a local git alias so `git subcontext` dispatches to the `subcontext` binary.
fn install_git_alias(backend: &dyn Backend, root: &Path) -> Result<()> {
    // Resolve the absolute path to the currently running binary
    let exe = backend.current_exe()?;
    let exe_str = exe.to_string_lossy();
    let alias_value = format!("!{exe_str}");

    run_git(backend, &["config", "alias.subcontext", &alias_value], root)?;
    eprintln!("[subcontext] Configured git alias: git subcontext → {exe_str}");
    Ok(())
}

/// Initialize the subcontext bare repo, config branch/worktree, and first overlay branch.
fn init_context_repo(backend: &dyn Backend, root: &Path, host_branch: &str) -> Result<()> {
    let sc_dir = subcontext_dir(root);
    let repo = repo_dir(root);

    backend.create_dir_all(&sc_dir)?;

    // 1. Init bare repo
    backend.init_bare(root, &repo)?;

    // 2. Create config branch via plumbing
    let empty_tree =
        run_subcontext_git(backend, &["hash-object", "-t", "tree", "/dev/null"], root)?;
    let config_commit = run_subcontext_git(
        backend,
        &["commit-tree", &empty_tree, "-m", "init config branch"],
        root,
    )?;
    run_subcontext_git(
        backend,
        &["update-ref", "refs/heads/config", &config_commit],
        root,
    )?;

    // 3. Add config worktree
    let cfg = config_dir(root);
    run_subcontext_git(
        backend,
        &["worktree", "add", &cfg.to_string_lossy(), "config"],
        root,
    )?;

    // 4. Create overlay/<current-branch> via plumbing (empty)
    let safe_branch = sanitize_branch_name(host_branch);
    let overlay_branch = format!("overlay/{safe_branch}");
    overlay::create_overlay_branch(backend, root, &overlay_branch)?;

    // 5. Add work/ worktree pointing to overlay branch
    let work = work_dir(root);
    run_subcontext_git(
        backend,
        &["worktree", "add", &work.to_string_lossy(), &overlay_branch],
        root,
    )?;

    // 6. Initialize state branch (tasks.db)
    task::init_state_branch(backend, root)?;

    Ok(())
}

/// Check whether a hook dispatches to subcontext.
fn hook_dispatches_to_subcontext(backend: &dyn Backend, root: &Path, hook_name: &str) -> bool {
    let hook_path = root.join(".git").join("hooks").join(hook_name);
    let content = match backend.read_to_string(&hook_path) {
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
    backend: &dyn Backend,
    root: &Path,
    repair: bool,
    pc_is_ours: bool,
    pcm_is_ours: bool,
) -> Result<()> {
    let hooks_dir = root.join(".git").join("hooks");
    if !backend.exists(&hooks_dir) {
        return Ok(());
    }

    let cfg = config_dir(root);
    let old_dir = cfg.join("hooks").join("old");
    let backup_dir = cfg.join("hooks").join("backup");

    for path in backend.read_dir(&hooks_dir)? {
        if !backend.is_file(&path) {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.ends_with(".sample")
        {
            continue;
        }

        #[cfg(unix)]
        {
            let mode = backend.metadata_mode(&path)?;
            if mode & 0o111 == 0 {
                continue;
            }
        }

        let hook_name = path.file_name().unwrap().to_string_lossy().to_string();

        let is_ours = (hook_name == "post-checkout" && pc_is_ours)
            || (hook_name == "post-commit" && pcm_is_ours);

        if is_ours {
            if repair {
                backend.create_dir_all(&backup_dir)?;
                let dest = backup_dir.join(&hook_name);
                backend.copy(&path, &dest)?;
                eprintln!(
                    "[subcontext] Saved existing subcontext hook to hooks/backup/{hook_name}"
                );
            }
            continue;
        }

        backend.create_dir_all(&old_dir)?;
        let dest = old_dir.join(&hook_name);
        backend.copy(&path, &dest)?;
        eprintln!("[subcontext] Backed up hook: {hook_name}");
    }

    Ok(())
}

/// Install a hook dispatcher script.
fn install_hook_dispatcher(backend: &dyn Backend, root: &Path, hook_name: &str) -> Result<()> {
    let hooks_dir = root.join(".git").join("hooks");
    backend.create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join(hook_name);
    let script = format!(
        r#"#!/bin/sh
# Installed by subcontext. Dispatches to `git subcontext _hook {hook_name}`.
# Your original hook (if any) is backed up and called automatically.
exec git subcontext _hook {hook_name} "$@"
"#
    );

    backend.write(&hook_path, script.as_bytes())?;

    #[cfg(unix)]
    {
        backend.set_permissions_mode(&hook_path, 0o755)?;
    }

    eprintln!("[subcontext] Installed {hook_name} hook dispatcher.");
    Ok(())
}

/// Commit everything on the config branch.
fn commit_config_branch(backend: &dyn Backend, root: &Path) -> Result<()> {
    let cfg = config_dir(root);
    if !backend.exists(&cfg) {
        return Ok(());
    }

    backend.add_all(&cfg)?;

    let status = backend.status_porcelain(&cfg)?;
    if status.is_empty() {
        return Ok(());
    }

    backend.commit(&cfg, "subcontext: update config")?;

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
