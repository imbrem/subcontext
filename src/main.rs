pub use subcontext::backend;

mod clone;
mod docs;
mod git;
mod global;
mod hook;
mod install;
mod mcp;
mod namespace;
mod overlay;
mod project;
mod settings;
mod startup;
mod status;
mod submodule;
mod task;
mod uninstall;

use anyhow::{Result, bail};
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

        /// Install the global (system) subcontext.
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

    /// Dump bundled documentation, sample skills, and setup guides to a directory
    Docs {
        /// Destination directory (created if it doesn't exist)
        path: String,
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
        /// Operate on the global (system-level) subcontext.
        #[arg(long, global = true)]
        global: bool,
        /// Operate on the user subcontext.
        #[arg(long, global = true, conflicts_with = "global")]
        user: bool,
        /// Only act on the local (per-repo) subcontext.
        #[arg(long, global = true, conflicts_with_all = ["global", "user"])]
        local: bool,
        #[command(subcommand)]
        command: TaskCommand,
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

    /// Manage the namespace dictionary
    Namespace {
        /// Operate on the global (system-level) subcontext.
        #[arg(long)]
        global: bool,
        /// Operate on the user subcontext.
        #[arg(long, conflicts_with = "global")]
        user: bool,
        #[command(subcommand)]
        command: NamespaceCommand,
    },

    /// Internal hook dispatcher (not for direct use)
    #[command(name = "_hook", hide = true)]
    Hook {
        #[command(subcommand)]
        hook: HookCommand,
    },
}

#[derive(Subcommand)]
enum TaskCommand {
    /// Create a task in the pool
    Add {
        /// Task title
        title: String,
        /// List (e.g. work, personal)
        #[arg(long)]
        list: Option<String>,
        /// Topic
        #[arg(long)]
        topic: Option<String>,
        /// Task type (default: todo)
        #[arg(long = "type")]
        task_type: Option<String>,
        /// Task status (default: active)
        #[arg(long)]
        status: Option<String>,
        /// Mark as important
        #[arg(long)]
        important: bool,
        /// Deadline (ISO8601 timestamp)
        #[arg(long)]
        deadline: Option<String>,
        /// Parent task IDs (comma-separated)
        #[arg(long)]
        parents: Option<String>,
        /// Explicit UUID for this task
        #[arg(long)]
        uuid: Option<String>,
    },
    /// Mark a task as done
    Done {
        /// Task identifier (ID, UUID, or pool/ID)
        task: String,
        /// Completion timestamp (ISO8601). Defaults to now.
        #[arg(long)]
        time: Option<String>,
    },
    /// Mark a task as failed/cancelled
    Fail {
        /// Task identifier (ID, UUID, or pool/ID)
        task: String,
        /// Failure timestamp (ISO8601). Defaults to now.
        #[arg(long)]
        time: Option<String>,
    },
    /// Show a task's TASK.md
    Show {
        /// Task identifier (ID, UUID, or pool/ID)
        task: String,
    },
    /// Update fields on an existing task
    Update {
        /// Task identifier (ID, UUID, or pool/ID)
        task: String,
        /// Updated list
        #[arg(long)]
        list: Option<String>,
        /// Updated topic
        #[arg(long)]
        topic: Option<String>,
        /// Updated type
        #[arg(long, name = "type")]
        task_type: Option<String>,
        /// Updated status
        #[arg(long)]
        status: Option<String>,
        /// Updated importance
        #[arg(long)]
        important: Option<bool>,
        /// Updated deadline
        #[arg(long)]
        deadline: Option<String>,
        /// Updated title
        #[arg(long)]
        title: Option<String>,
    },
    /// Show deadlines for active tasks
    Deadlines {
        /// Only show important tasks
        #[arg(long)]
        important: bool,
        /// Only show deadlines within this duration from now (e.g. 1d, 2w, 3mo, 1y).
        #[arg(long)]
        horizon: Option<String>,
    },
}

#[derive(Subcommand)]
enum NamespaceCommand {
    /// Set a namespace entry: name=uuid (use / for nesting, e.g. tools/editor=uuid)
    Set {
        /// Key path (e.g. "myproject" or "tools/editor")
        key: String,
        /// UUID to map the name to
        uuid: String,
    },
    /// Get a namespace entry by key path
    Get {
        /// Key path (e.g. "myproject" or "tools/editor")
        key: String,
    },
    /// Remove a namespace entry
    Remove {
        /// Key path (e.g. "myproject" or "tools/editor")
        key: String,
    },
    /// List all namespace entries
    List,
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
        Commands::Docs { path } => {
            let dest = if Path::new(&path).is_absolute() {
                Path::new(&path).to_path_buf()
            } else {
                cwd.join(&path)
            };
            docs::dump_docs(backend, &dest)?;
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
            user: user_flag,
            local: _local_flag,
            command,
        } => {
            let local_root = git::find_main_git_root(backend, &cwd).ok();
            let scope = if global_flag {
                global::global_task_scope(backend)?
            } else if user_flag {
                global::user_task_scope(backend)?
            } else if let Some(ref root) = local_root {
                task::TaskScope::for_local(backend, root)?
            } else if global::global_exists(backend)? {
                global::user_task_scope(backend).unwrap_or(global::global_task_scope(backend)?)
            } else {
                bail!("not inside a git repo and no global subcontext installed");
            };

            match command {
                TaskCommand::Add {
                    title,
                    list,
                    topic,
                    task_type,
                    status,
                    important,
                    deadline,
                    parents,
                    uuid,
                } => {
                    let parent_ids: Vec<i64> = match parents {
                        Some(ref p) => p
                            .split(',')
                            .filter_map(|s| s.trim().parse::<i64>().ok())
                            .collect(),
                        None => vec![],
                    };
                    task::pool_add_task(
                        backend,
                        &scope,
                        &title,
                        list.as_deref(),
                        topic.as_deref(),
                        task_type.as_deref(),
                        status.as_deref(),
                        important,
                        deadline.as_deref(),
                        &parent_ids,
                        uuid.as_deref(),
                    )?;
                }
                TaskCommand::Done { task, time } => {
                    task::pool_done_task(backend, &scope, &task, time.as_deref())?;
                }
                TaskCommand::Fail { task, time } => {
                    task::pool_fail_task(backend, &scope, &task, time.as_deref())?;
                }
                TaskCommand::Show { task } => {
                    let content = task::pool_show_task(backend, &scope, &task)?;
                    print!("{content}");
                }
                TaskCommand::Update {
                    task,
                    list,
                    topic,
                    task_type,
                    status,
                    important,
                    deadline,
                    title,
                } => {
                    task::pool_update_task(
                        backend,
                        &scope,
                        &task,
                        list.as_deref(),
                        topic.as_deref(),
                        task_type.as_deref(),
                        status.as_deref(),
                        important,
                        deadline.as_deref(),
                        title.as_deref(),
                    )?;
                }
                TaskCommand::Deadlines { important, horizon } => {
                    let entries = task::pool_list_deadlines(&scope, important, horizon.as_deref())?;
                    print!("{}", task::format_deadlines(&entries));
                }
            }
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
        Commands::Namespace {
            global: global_flag,
            user: user_flag,
            command,
        } => {
            let config_dir = if global_flag {
                global::global_config_dir()?
            } else if user_flag {
                global::user_config_dir()?
            } else {
                let root = git::find_main_git_root(backend, &cwd)?;
                git::config_dir(&root)
            };

            match command {
                NamespaceCommand::Set { key, uuid } => {
                    let segments: Vec<&str> = key.split('/').filter(|s| !s.is_empty()).collect();
                    let mut ns = namespace::read_namespaces(backend, &config_dir)?;
                    namespace::set_namespace(&mut ns, &segments, &uuid)?;
                    namespace::write_namespaces(backend, &config_dir, &ns)?;
                    crate::git::run_work_git(backend, &["add", "-A"], &config_dir)?;
                    let status =
                        crate::git::run_work_git(backend, &["status", "--porcelain"], &config_dir)?;
                    if !status.is_empty() {
                        crate::git::run_work_git(
                            backend,
                            &["commit", "-m", &format!("namespace set: {key}")],
                            &config_dir,
                        )?;
                    }
                    eprintln!("[subcontext] Set namespace '{key}' = {uuid}");
                }
                NamespaceCommand::Get { key } => {
                    let segments: Vec<&str> = key.split('/').filter(|s| !s.is_empty()).collect();
                    let ns = namespace::read_namespaces(backend, &config_dir)?;
                    match namespace::resolve_namespace(&ns, &segments) {
                        Ok((uuid, remaining)) => {
                            if remaining.is_empty() {
                                println!("{uuid}");
                            } else {
                                bail!(
                                    "path resolved to UUID '{uuid}' with remaining segments: {}",
                                    remaining.join("/")
                                );
                            }
                        }
                        Err(e) => bail!("{e}"),
                    }
                }
                NamespaceCommand::Remove { key } => {
                    let segments: Vec<&str> = key.split('/').filter(|s| !s.is_empty()).collect();
                    let mut ns = namespace::read_namespaces(backend, &config_dir)?;
                    let removed = namespace::remove_namespace(&mut ns, &segments)?;
                    if removed.is_some() {
                        namespace::write_namespaces(backend, &config_dir, &ns)?;
                        crate::git::run_work_git(backend, &["add", "-A"], &config_dir)?;
                        let status = crate::git::run_work_git(
                            backend,
                            &["status", "--porcelain"],
                            &config_dir,
                        )?;
                        if !status.is_empty() {
                            crate::git::run_work_git(
                                backend,
                                &["commit", "-m", &format!("namespace remove: {key}")],
                                &config_dir,
                            )?;
                        }
                        eprintln!("[subcontext] Removed namespace '{key}'");
                    } else {
                        eprintln!("[subcontext] Namespace '{key}' not found.");
                    }
                }
                NamespaceCommand::List => {
                    let ns = namespace::read_namespaces(backend, &config_dir)?;
                    let entries = namespace::flatten_namespaces(&ns, "");
                    if entries.is_empty() {
                        eprintln!("[subcontext] No namespace entries.");
                    } else {
                        for (key, uuid) in &entries {
                            println!("{key} = {uuid}");
                        }
                    }
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

/// Resolve a path that may not exist yet to be relative to root.
fn resolve_new_path(backend: &dyn Backend, cwd: &Path, root: &Path, path: &str) -> Result<String> {
    let root_canonical = backend.canonicalize(root).unwrap_or(root.to_path_buf());

    let abs = if Path::new(path).is_absolute() {
        Path::new(path).to_path_buf()
    } else {
        let cwd_canonical = backend.canonicalize(cwd).unwrap_or(cwd.to_path_buf());
        cwd_canonical.join(path)
    };

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
