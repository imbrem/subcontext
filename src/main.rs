pub use subcontext::backend;

mod clone;
mod git;
mod global;
mod hook;
mod install;
mod mcp;
mod mcp_config;
mod overlay;
mod project;
mod settings;
mod startup;
mod status;
mod submodule;
mod task;
mod uninstall;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::env;
use std::path::Path;

use backend::{Backend, SystemBackend};
use git::CheckoutContext;

#[derive(Parser)]
#[command(
    name = "subcontext",
    about = "Private, version-controlled context for Git projects"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a subcontext repo in the current Git project
    Install {
        /// Re-install hooks even if they already contain subcontext dispatchers
        #[arg(long)]
        repair: bool,

        /// Install the global (system) subcontext and MCP server into the
        /// user's Claude Code config (`~/.claude.json`).
        #[arg(long, conflicts_with_all = ["repair", "user"])]
        global: bool,

        /// Install a user subcontext under the global (system) subcontext.
        #[arg(long, conflicts_with_all = ["repair", "global"])]
        user: bool,
    },

    /// Clone an existing subcontext repo and attach it to this project
    Clone {
        /// URL of the context repo to clone
        url: String,
    },

    /// Add files to the overlay
    Add {
        /// Files to add
        #[arg(required = true)]
        files: Vec<String>,
    },

    /// Save overlay changes to the subcontext repo
    Save {
        /// Commit message
        #[arg(short, long)]
        message: Option<String>,
    },

    /// Remove files from the overlay
    Remove {
        /// Files to remove
        #[arg(required = true)]
        files: Vec<String>,
    },

    /// Print task context for agent harnesses (no-op stub)
    Startup {
        /// Agent harness identifier
        #[arg(long)]
        claude_code: bool,
    },

    /// Remove subcontext hooks and Claude settings from the current project
    Uninstall,

    /// Show current repo, worktree, and subcontext status
    Status,

    /// Manage submodules within the overlay
    Submodule {
        #[command(subcommand)]
        command: SubmoduleCommand,
    },

    /// Manage tasks
    Task {
        /// Operate on the global (user-level) subcontext instead of the
        /// current repo's subcontext. Implied when run outside a git repo.
        #[arg(long, global = true)]
        global: bool,
        /// Only act on the local (per-repo) subcontext — skip creating a
        /// shadow task in the global subcontext. No effect outside a git
        /// repo or when `--global` is passed.
        #[arg(long, global = true, conflicts_with = "global")]
        local: bool,
        #[command(subcommand)]
        command: TaskCommand,
    },

    /// Sync TASK.md and object.json on an object branch
    ObjectCommit {
        /// Object UUID
        uuid: String,
    },

    /// Run the subcontext MCP server over stdio
    Mcp,

    /// Set the current user context (by UUID)
    SetUser {
        /// UUID of the user subcontext
        uuid: String,
    },

    /// Print the current user context's UUID
    CurrentUser,

    /// Print the current subcontext's UUID
    Uuid,

    /// View the tree of all subcontexts managed by the global subcontext
    Tree,

    /// View the current subcontext's parent
    Parent,

    /// View the current subcontext's children
    Children,

    /// Internal hook dispatcher (not for direct use)
    #[command(name = "_hook", hide = true)]
    Hook {
        #[command(subcommand)]
        hook: HookCommand,
    },
}

#[derive(Subcommand)]
enum TaskCommand {
    /// Create a task with the given name and a fresh UUID
    Add {
        /// Task name (optional if --file is provided; taken from TASK.md frontmatter)
        name: Option<String>,
        /// Path to a TASK.md file (YAML frontmatter + markdown body)
        #[arg(long)]
        file: Option<String>,
        /// Task kind (e.g. goal, todo, tick, task)
        #[arg(long)]
        kind: Option<String>,
        /// Task status (e.g. created, active, inactive, done, failed)
        #[arg(long)]
        status: Option<String>,
        /// Short task description
        #[arg(long)]
        description: Option<String>,
        /// Deadline (ISO8601 UTC timestamp ending in Z)
        #[arg(long)]
        deadline: Option<String>,
        /// Mark as important. Without a value sets importance to 1.0.
        #[arg(long, num_args = 0..=1, default_missing_value = "1.0")]
        important: Option<f64>,
    },
    /// Update an existing task (by UUID or name)
    Update {
        /// Task name or UUID to update
        name_or_uuid: String,
        /// Path to an updated TASK.md file
        #[arg(long)]
        file: Option<String>,
        /// Updated task name
        #[arg(long)]
        name: Option<String>,
        /// Updated task kind
        #[arg(long)]
        kind: Option<String>,
        /// Updated task status
        #[arg(long)]
        status: Option<String>,
        /// Updated description
        #[arg(long)]
        description: Option<String>,
        /// Updated deadline
        #[arg(long)]
        deadline: Option<String>,
        /// Updated importance
        #[arg(long)]
        important: Option<f64>,
    },
    /// Show a task's TASK.md (by name or UUID)
    Show {
        /// Task name or UUID
        name_or_uuid: String,
    },
    /// Mark a task as done
    Done {
        /// Task name
        name: String,
        /// Completion timestamp (ISO8601). Defaults to now.
        #[arg(long)]
        time: Option<String>,
    },
    /// Mark a task as failed
    Fail {
        /// Task name
        name: String,
        /// Failure timestamp (ISO8601). Defaults to now.
        #[arg(long)]
        time: Option<String>,
    },
    /// Show deadlines for active tasks
    Deadlines {
        /// Only show important tasks (importance > 0)
        #[arg(long)]
        important: bool,
        /// Only show deadlines within this duration from now (e.g. 1d, 2w, 3mo, 1y).
        /// Suffixes: s, m (minutes), h, d, w, mo (months), y. 0 = only overdue.
        #[arg(long)]
        horizon: Option<String>,
    },
}

#[derive(Subcommand)]
enum HookCommand {
    /// Handle post-checkout events
    PostCheckout {
        prev_head: String,
        new_head: String,
        flag: String,
    },
    /// Handle post-commit events
    PostCommit,
}

#[derive(Subcommand)]
enum SubmoduleCommand {
    /// Add a submodule to the overlay
    Add {
        /// URL of the repository to add as a submodule
        url: String,
        /// Path where the submodule should be placed (default: derived from URL)
        path: Option<String>,
    },
    /// Initialize and update overlay submodules
    Update,
    /// Remove a submodule from the overlay
    Remove {
        /// Path of the submodule to remove
        path: String,
    },
}

fn handle_task_show(
    backend: &dyn backend::Backend,
    scope: &task::TaskScope,
    name_or_uuid: &str,
) -> Result<()> {
    use task::ShowTaskResult;
    match task::show_task(backend, scope, name_or_uuid)? {
        ShowTaskResult::Single(uuid, content) => {
            eprintln!("[subcontext] Task UUID: {uuid}");
            print!("{content}");
        }
        ShowTaskResult::Ambiguous(matches) => {
            println!(
                "Multiple tasks match '{}' ({} matches):",
                name_or_uuid,
                matches.len()
            );
            for m in &matches {
                let desc = m.description.as_deref().unwrap_or("(no description)");
                println!("  {} (branch: {}) — {}", m.uuid, m.branch, desc);
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = env::current_dir()?;
    let backend: &dyn Backend = &SystemBackend;

    match cli.command {
        Commands::Install {
            repair,
            global,
            user,
        } => {
            if global {
                global::install(backend)?;
                mcp_config::install_global(backend)?;
            } else if user {
                global::install_user(backend)?;
            } else {
                let root = git::find_main_git_root(backend, &cwd)?;
                install::install(backend, &root, repair)?;
            }
        }
        Commands::Clone { url } => {
            let root = git::find_main_git_root(backend, &cwd)?;
            clone::clone(backend, &root, &url)?;
        }
        Commands::Add { files } => {
            let root = git::find_main_git_root(backend, &cwd)?;
            let ctx = CheckoutContext::main_only(&root);
            for file in &files {
                let resolved = resolve_file_path(backend, &cwd, &root, file)?;
                overlay::add_file(backend, &ctx, &resolved)?;
                eprintln!("[subcontext] Added: {resolved}");
            }
        }
        Commands::Save { message } => {
            let root = git::find_main_git_root(backend, &cwd)?;
            let ctx = CheckoutContext::main_only(&root);
            let msg = message.as_deref().unwrap_or("manual save");
            overlay::save_overlay(backend, &ctx, msg)?;
            eprintln!("[subcontext] Saved overlay changes.");
        }
        Commands::Remove { files } => {
            let root = git::find_main_git_root(backend, &cwd)?;
            let ctx = CheckoutContext::main_only(&root);
            for file in &files {
                let resolved = resolve_file_path(backend, &cwd, &root, file)?;
                overlay::remove_file(backend, &ctx, &resolved)?;
                eprintln!("[subcontext] Removed: {resolved}");
            }
        }
        Commands::Startup { .. } => {
            startup::startup()?;
        }
        Commands::Uninstall => {
            let root = git::find_main_git_root(backend, &cwd)?;
            uninstall::uninstall(backend, &root)?;
        }
        Commands::Status => {
            status::status(backend, &cwd)?;
        }
        Commands::Submodule { command } => {
            let root = git::find_main_git_root(backend, &cwd)?;
            let ctx = CheckoutContext::main_only(&root);
            match command {
                SubmoduleCommand::Add { url, path } => {
                    let resolved = path
                        .as_ref()
                        .map(|p| resolve_new_path(backend, &cwd, &root, p))
                        .transpose()?;
                    submodule::add(backend, &ctx, &url, resolved.as_deref())?;
                }
                SubmoduleCommand::Update => {
                    submodule::update(backend, &ctx)?;
                }
                SubmoduleCommand::Remove { path } => {
                    let resolved = resolve_new_path(backend, &cwd, &root, &path)?;
                    submodule::remove(backend, &ctx, &resolved)?;
                }
            }
        }
        Commands::Task {
            global: global_flag,
            local: local_flag,
            command,
        } => {
            // Use the global scope when --global is passed OR when we're
            // outside a git repo. Otherwise fall back to the local scope.
            let local_root = git::find_main_git_root(backend, &cwd).ok();
            let use_global = global_flag || local_root.is_none();

            // Helper: run the task add logic for a given scope, handling
            // both --file and positional-name modes.
            let handle_task_add = |backend: &dyn Backend,
                                   scope: &task::TaskScope,
                                   name: Option<String>,
                                   file: Option<String>,
                                   kind: Option<String>,
                                   status: Option<String>,
                                   description: Option<String>,
                                   deadline: Option<String>,
                                   important: Option<f64>|
             -> Result<(String, String, String)> {
                let importance = important.unwrap_or(0.0);
                if let Some(file_path) = file {
                    let md = std::fs::read_to_string(&file_path)
                        .with_context(|| format!("cannot read {file_path}"))?;
                    let (uuid, commit) = task::add_task_from_md(backend, scope, &md, None)?;
                    // Extract name from frontmatter for propagation.
                    let (pairs, _) = task::parse_frontmatter(&md);
                    let task_name = pairs
                        .iter()
                        .find(|(k, _)| k == "name")
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();
                    Ok((uuid, commit, task_name))
                } else {
                    let name = name
                        .ok_or_else(|| anyhow::anyhow!("task name is required (or use --file)"))?;
                    let (uuid, commit) = task::add_task(
                        backend,
                        scope,
                        &name,
                        kind.as_deref(),
                        status.as_deref(),
                        description.as_deref(),
                        deadline.as_deref(),
                        importance,
                        None,
                    )?;
                    Ok((uuid, commit, name))
                }
            };

            if use_global {
                let scope = global::global_task_scope(backend)?;
                match command {
                    TaskCommand::Add {
                        name,
                        file,
                        kind,
                        status,
                        description,
                        deadline,
                        important,
                    } => {
                        handle_task_add(
                            backend,
                            &scope,
                            name,
                            file,
                            kind,
                            status,
                            description,
                            deadline,
                            important,
                        )?;
                    }
                    TaskCommand::Update {
                        name_or_uuid,
                        file,
                        name,
                        kind,
                        status,
                        description,
                        deadline,
                        important,
                    } => {
                        let uuid = task::resolve_task_uuid(backend, &scope, &name_or_uuid)?;
                        let md = file.map(|p| std::fs::read_to_string(&p)).transpose()?;
                        task::update_task(
                            backend,
                            &scope,
                            &uuid,
                            md.as_deref(),
                            name.as_deref(),
                            kind.as_deref(),
                            status.as_deref(),
                            description.as_deref(),
                            deadline.as_deref(),
                            important,
                        )?;
                    }
                    TaskCommand::Show { name_or_uuid } => {
                        handle_task_show(backend, &scope, &name_or_uuid)?;
                    }
                    TaskCommand::Done { name, time } => {
                        task::done_task(backend, &scope, &name, time.as_deref())?;
                    }
                    TaskCommand::Fail { name, time } => {
                        task::fail_task(backend, &scope, &name, time.as_deref())?;
                    }
                    TaskCommand::Deadlines { important, horizon } => {
                        let entries = task::list_deadlines(&scope, important, horizon.as_deref())?;
                        print!("{}", task::format_deadlines(&entries));
                    }
                }
            } else {
                let root = local_root.unwrap();
                let scope = task::TaskScope::for_local(backend, &root)?;
                match command {
                    TaskCommand::Add {
                        name,
                        file,
                        kind,
                        status,
                        description,
                        deadline,
                        important,
                    } => {
                        let (local_uuid, local_commit, task_name) = handle_task_add(
                            backend,
                            &scope,
                            name,
                            file.clone(),
                            kind.clone(),
                            status.clone(),
                            description.clone(),
                            deadline.clone(),
                            important,
                        )?;
                        // Propagate task up the parent chain unless --local.
                        if !local_flag && global::global_exists(backend)? {
                            let importance = important.unwrap_or(0.0);
                            propagate_task_up(
                                backend,
                                &scope.project_uuid,
                                &local_uuid,
                                &local_commit,
                                &task_name,
                                kind.as_deref(),
                                status.as_deref(),
                                description.as_deref(),
                                deadline.as_deref(),
                                importance,
                            )?;
                            if let Some(commit) = global::record_child_checkout_path(
                                backend,
                                &scope.project_uuid,
                                &root.join(".git"),
                            )? {
                                let global_scope = global::global_task_scope(backend)?;
                                let conn = task::open_db(&global_scope)?;
                                conn.execute(
                                    "UPDATE objects SET current_commit = ?1 WHERE uuid = ?2",
                                    rusqlite::params![commit, scope.project_uuid],
                                )?;
                                drop(conn);
                                task::commit_state_in(
                                    backend,
                                    &global_scope.state_dir,
                                    &format!("object update: {}", scope.project_uuid),
                                )?;
                            }
                        }
                    }
                    TaskCommand::Update {
                        name_or_uuid,
                        file,
                        name,
                        kind,
                        status,
                        description,
                        deadline,
                        important,
                    } => {
                        let uuid = task::resolve_task_uuid(backend, &scope, &name_or_uuid)?;
                        let md = file.map(|p| std::fs::read_to_string(&p)).transpose()?;
                        task::update_task(
                            backend,
                            &scope,
                            &uuid,
                            md.as_deref(),
                            name.as_deref(),
                            kind.as_deref(),
                            status.as_deref(),
                            description.as_deref(),
                            deadline.as_deref(),
                            important,
                        )?;
                    }
                    TaskCommand::Show { name_or_uuid } => {
                        handle_task_show(backend, &scope, &name_or_uuid)?;
                    }
                    TaskCommand::Done { name, time } => {
                        task::done_task(backend, &scope, &name, time.as_deref())?;
                    }
                    TaskCommand::Fail { name, time } => {
                        task::fail_task(backend, &scope, &name, time.as_deref())?;
                    }
                    TaskCommand::Deadlines { important, horizon } => {
                        let entries = task::list_deadlines(&scope, important, horizon.as_deref())?;
                        print!("{}", task::format_deadlines(&entries));
                    }
                }
            }
        }
        Commands::ObjectCommit { uuid } => {
            let local_root = git::find_main_git_root(backend, &cwd).ok();
            let scope = if let Some(root) = local_root {
                task::TaskScope::for_local(backend, &root)?
            } else {
                global::global_task_scope(backend)?
            };
            task::object_commit(backend, &scope, &uuid)?;
        }
        Commands::SetUser { uuid } => {
            global::set_current_user(backend, &uuid)?;
        }
        Commands::CurrentUser => match global::get_current_user(backend)? {
            Some(uuid) => println!("{uuid}"),
            None => {
                eprintln!("[subcontext] No current user set.");
                std::process::exit(1);
            }
        },
        Commands::Uuid => {
            let root = git::find_main_git_root(backend, &cwd).ok();
            if let Some(root) = root {
                let uuid = project::read_project_uuid(backend, &root)?;
                println!("{uuid}");
            } else if global::global_exists(backend)? {
                let scope = global::global_task_scope(backend)?;
                println!("{}", scope.project_uuid);
            } else {
                bail!("not inside a git repo and no global subcontext installed");
            }
        }
        Commands::Tree => {
            let text = global::tree_text(backend)?;
            print!("{text}");
        }
        Commands::Parent => {
            let root = git::find_main_git_root(backend, &cwd)?;
            let uuid = project::read_project_uuid(backend, &root)?;
            match global::get_parent(backend, &uuid)? {
                Some(parent) => {
                    let kind = global::get_managed_kind(backend, &parent)
                        .unwrap_or_else(|_| "unknown".to_string());
                    println!("{parent} ({kind})");
                }
                None => {
                    eprintln!("[subcontext] No parent set for this subcontext.");
                    std::process::exit(1);
                }
            }
        }
        Commands::Children => {
            let root = git::find_main_git_root(backend, &cwd)?;
            let uuid = project::read_project_uuid(backend, &root)?;
            let children = global::get_children(backend, &uuid)?;
            if children.is_empty() {
                eprintln!("[subcontext] No children for this subcontext.");
            } else {
                for child in children {
                    let kind = global::get_managed_kind(backend, &child)
                        .unwrap_or_else(|_| "unknown".to_string());
                    println!("{child} ({kind})");
                }
            }
        }
        Commands::Mcp => {
            mcp::run(backend)?;
        }
        Commands::Hook {
            hook:
                HookCommand::PostCheckout {
                    prev_head,
                    new_head,
                    flag,
                },
        } => {
            let ctx = match git::find_checkout_context(backend, &cwd) {
                Ok(ctx) => ctx,
                Err(e) => {
                    eprintln!("[subcontext] warning: {e:#}");
                    return Ok(());
                }
            };
            hook::post_checkout(backend, &ctx, &prev_head, &new_head, &flag)?;
        }
        Commands::Hook {
            hook: HookCommand::PostCommit,
        } => {
            let ctx = match git::find_checkout_context(backend, &cwd) {
                Ok(ctx) => ctx,
                Err(e) => {
                    eprintln!("[subcontext] warning: {e:#}");
                    return Ok(());
                }
            };
            hook::post_commit(backend, &ctx)?;
        }
    }

    Ok(())
}

/// Recursively propagate a task from a child context up through its parent
/// chain. Each parent gets a shadow task whose source points to the original
/// child task.
fn propagate_task_up(
    backend: &dyn Backend,
    child_uuid: &str,
    task_uuid: &str,
    task_commit: &str,
    name: &str,
    kind: Option<&str>,
    status: Option<&str>,
    description: Option<&str>,
    deadline: Option<&str>,
    importance: f64,
) -> Result<()> {
    // Find the parent of this child via the system DB.
    let parent_uuid = match global::get_parent(backend, child_uuid)? {
        Some(p) => p,
        None => {
            // No parent — fall back to legacy behavior: propagate to global
            // (system) context directly.
            let global_scope = global::global_task_scope(backend)?;
            task::add_task(
                backend,
                &global_scope,
                name,
                kind,
                status,
                description,
                deadline,
                importance,
                Some((child_uuid, task_uuid, task_commit)),
            )?;
            return Ok(());
        }
    };

    // Find the TaskScope for the parent. The parent may be the user
    // subcontext or another project — we need to resolve its scope.
    let parent_scope = resolve_scope_for_uuid(backend, &parent_uuid)?;
    let (shadow_uuid, shadow_commit) = task::add_task(
        backend,
        &parent_scope,
        name,
        kind,
        status,
        description,
        deadline,
        importance,
        Some((child_uuid, task_uuid, task_commit)),
    )?;

    // Recurse: propagate from parent to grandparent.
    propagate_task_up(
        backend,
        &parent_uuid,
        &shadow_uuid,
        &shadow_commit,
        name,
        kind,
        status,
        description,
        deadline,
        importance,
    )
}

/// Resolve a TaskScope for a given UUID. Checks if it's the user subcontext
/// or the system subcontext.
fn resolve_scope_for_uuid(backend: &dyn Backend, uuid: &str) -> Result<task::TaskScope> {
    // Check if it's the system (global) subcontext.
    if let Ok(scope) = global::global_task_scope(backend) {
        if scope.project_uuid == uuid {
            return Ok(scope);
        }
    }
    // Check if it's the user subcontext.
    if let Ok(scope) = global::user_task_scope(backend) {
        if scope.project_uuid == uuid {
            return Ok(scope);
        }
    }
    bail!(
        "cannot resolve TaskScope for UUID {uuid} — only system and user subcontexts support receiving propagated tasks"
    )
}

/// Resolve a path that may not exist yet (e.g., submodule destination) to be relative to root.
fn resolve_new_path(backend: &dyn Backend, cwd: &Path, root: &Path, path: &str) -> Result<String> {
    let root_canonical = backend.canonicalize(root).unwrap_or(root.to_path_buf());

    let abs = if Path::new(path).is_absolute() {
        Path::new(path).to_path_buf()
    } else {
        let cwd_canonical = backend.canonicalize(cwd).unwrap_or(cwd.to_path_buf());
        cwd_canonical.join(path)
    };

    // Try canonicalize (resolves ..), fall back to manual normalization
    let abs = backend
        .canonicalize(&abs)
        .unwrap_or_else(|_| normalize_path(&abs));

    match abs.strip_prefix(&root_canonical) {
        Ok(rel) => Ok(rel.to_string_lossy().to_string()),
        Err(_) => bail!("path {path} is outside the repository root"),
    }
}

/// Normalize a path by resolving `.` and `..` components without filesystem access.
fn normalize_path(path: &Path) -> std::path::PathBuf {
    let mut result = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {}
            c => result.push(c),
        }
    }
    result
}

/// Resolve a user-provided file path to be relative to the repo root.
/// Handles both absolute paths and paths relative to the current directory.
fn resolve_file_path(backend: &dyn Backend, cwd: &Path, root: &Path, file: &str) -> Result<String> {
    let abs = if Path::new(file).is_absolute() {
        Path::new(file).to_path_buf()
    } else {
        cwd.join(file)
    };

    let abs = backend.canonicalize(&abs).unwrap_or(abs);
    let root_canonical = backend.canonicalize(root).unwrap_or(root.to_path_buf());

    match abs.strip_prefix(&root_canonical) {
        Ok(rel) => Ok(rel.to_string_lossy().to_string()),
        Err(_) => bail!("file {file} is outside the repository root"),
    }
}
