pub use subcontext::backend;

mod clone;
mod docs;
mod dolt;
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
        /// Only act on the local (per-repo) subcontext — skip creating a
        /// shadow task in the global subcontext.
        #[arg(long, global = true, conflicts_with_all = ["global", "user"])]
        local: bool,
        #[command(subcommand)]
        command: TaskCommand,
    },

    /// Manage boards (task trees)
    Board {
        /// Operate on the global (system-level) subcontext.
        #[arg(long, global = true)]
        global: bool,
        /// Operate on the user subcontext.
        #[arg(long, global = true, conflicts_with = "global")]
        user: bool,
        #[command(subcommand)]
        command: BoardCommand,
    },

    /// Sync TASK.md and object.json on an object branch
    ObjectCommit {
        /// Object UUID
        uuid: String,
    },

    /// Run the subcontext MCP server over stdio
    Mcp,

    /// Run dolt commands against the subcontext database
    Dolt {
        /// Arguments to pass to the dolt binary
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

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
        /// Parent task (name/path or UUID). Makes this a subtask.
        #[arg(long)]
        parent: Option<String>,
    },
    /// Update an existing task (by UUID or name/path)
    Update {
        /// Task name, path, or UUID to update
        name_or_uuid: String,
        /// Path to an updated TASK.md file
        #[arg(long)]
        file: Option<String>,
        /// Updated task title
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
    /// Show a task's TASK.md (by name/path or UUID)
    Show {
        /// Task name, path, or UUID
        name_or_uuid: String,
    },
    /// Mark a task as done (supports hierarchical paths like parent/child)
    Done {
        /// Task name or path (e.g. "mytask", "parent/child", "/root-task", ".")
        name: String,
        /// Completion timestamp (ISO8601). Defaults to now.
        #[arg(long)]
        time: Option<String>,
    },
    /// Mark a task as failed (supports hierarchical paths like parent/child)
    Fail {
        /// Task name or path (e.g. "mytask", "parent/child", "/root-task", ".")
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
        /// Filter deadlines by board (name or UUID)
        #[arg(long)]
        board: Option<String>,
    },
    /// Set (or unset) the current task for this branch
    Set {
        /// Task name/path to set as current. Omit to unset.
        name: Option<String>,
    },
    /// List subtasks of the current task (or root tasks)
    List {
        /// Task name/path whose subtasks to list. Omit for current task's children.
        name: Option<String>,
    },
    /// List root task UUIDs (tasks with no parent)
    Roots,
    /// Visualize the task tree as JSON or Mermaid
    #[command(name = "tree")]
    Tree {
        /// Output format: json or mermaid (default: json)
        #[arg(long, default_value = "json")]
        format: String,
        /// Filter: active (default, excludes done/failed), all, or a specific status
        #[arg(long, default_value = "active")]
        filter: String,
        /// Maximum tree depth
        #[arg(long)]
        max_depth: Option<usize>,
        /// Maximum children per node
        #[arg(long)]
        max_breadth: Option<usize>,
        /// Maximum total nodes in output
        #[arg(long)]
        max_size: Option<usize>,
        /// Root task (name/path or UUID). Omit for all roots.
        root: Option<String>,
    },
}

#[derive(Subcommand)]
enum BoardCommand {
    /// Create a new board with a root task
    Create {
        /// Board name
        name: Option<String>,
        /// Path to a TASK.md file for the root task
        #[arg(long)]
        file: Option<String>,
        /// Task kind
        #[arg(long)]
        kind: Option<String>,
        /// Task status
        #[arg(long)]
        status: Option<String>,
        /// Short description
        #[arg(long)]
        description: Option<String>,
        /// Deadline (ISO8601 UTC ending in Z)
        #[arg(long)]
        deadline: Option<String>,
        /// Importance
        #[arg(long, num_args = 0..=1, default_missing_value = "1.0")]
        important: Option<f64>,
    },
    /// Add a subtask to a board
    AddTask {
        /// Task name
        name: String,
        /// Board UUID (or name/path of the board root task)
        #[arg(long)]
        board: String,
        /// Parent task within the board (name/path or UUID). Defaults to board root.
        #[arg(long)]
        parent: Option<String>,
        /// Task kind
        #[arg(long)]
        kind: Option<String>,
        /// Task status
        #[arg(long)]
        status: Option<String>,
        /// Short description
        #[arg(long)]
        description: Option<String>,
        /// Deadline (ISO8601 UTC ending in Z)
        #[arg(long)]
        deadline: Option<String>,
        /// Importance
        #[arg(long, num_args = 0..=1, default_missing_value = "1.0")]
        important: Option<f64>,
    },
    /// Synchronize a board's tree into the state DB
    Commit {
        /// Board UUID
        uuid: String,
    },
    /// Delete a task from a board
    DeleteTask {
        /// Task UUID to delete
        task: String,
        /// Board UUID
        #[arg(long)]
        board: String,
    },
    /// Move a task to a new parent within a board
    MoveTask {
        /// Task UUID to move
        task: String,
        /// New parent task UUID
        #[arg(long)]
        parent: String,
        /// Board UUID
        #[arg(long)]
        board: String,
    },
    /// Pull a board's tasks into the overlay (materializes files in working tree)
    Pull {
        /// Board UUID (or name/path)
        board: String,
        /// Overlay directory prefix (default: "tasks/")
        #[arg(long, default_value = "tasks/")]
        path: String,
        /// Only pull a specific subtask's subtree
        #[arg(long)]
        task: Option<String>,
        /// Exclude done/failed tasks
        #[arg(long)]
        filter_done: bool,
    },
    /// Push overlay task files back to the board branch
    Push {
        /// Overlay directory prefix (default: "tasks/")
        #[arg(long, default_value = "tasks/")]
        path: String,
        /// Mark deleted tasks as done instead of removing them
        #[arg(long)]
        mark_done: bool,
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

fn handle_task_show(
    backend: &dyn backend::Backend,
    scope: &task::TaskScope,
    name_or_uuid: &str,
) -> Result<()> {
    use task::ShowTaskResult;
    // If the input looks like a path (contains /, starts with ., etc.),
    // try resolving it as a task path first.
    let resolved = if name_or_uuid.contains('/')
        || name_or_uuid == "."
        || name_or_uuid == ".."
        || name_or_uuid.starts_with('/')
    {
        let mut conn = task::open_db(scope)?;
        task::resolve_task_path(&mut conn, scope, name_or_uuid, Some(backend)).ok()
    } else {
        None
    };
    let lookup = resolved.as_deref().unwrap_or(name_or_uuid);
    match task::show_task(backend, scope, lookup)? {
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
            local: local_flag,
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
            let is_local_scope = !global_flag && !user_flag && local_root.is_some();

            // Helper: resolve --parent to a UUID via path resolution.
            let resolve_parent = |parent: &Option<String>| -> Result<Option<String>> {
                match parent {
                    Some(p) => {
                        let mut conn = task::open_db(&scope)?;
                        Ok(Some(task::resolve_task_path(
                            &mut conn,
                            &scope,
                            p,
                            Some(backend),
                        )?))
                    }
                    None => Ok(None),
                }
            };

            match command {
                TaskCommand::Add {
                    name,
                    file,
                    kind,
                    status,
                    description,
                    deadline,
                    important,
                    parent,
                } => {
                    let importance = important.unwrap_or(0.0);
                    let parent_uuid = resolve_parent(&parent)?;
                    let (local_uuid, local_commit, task_name) = if let Some(file_path) = file {
                        let md = std::fs::read_to_string(&file_path)
                            .with_context(|| format!("cannot read {file_path}"))?;
                        let (uuid, commit) = task::add_task_from_md(
                            backend,
                            &scope,
                            &md,
                            None,
                            name.as_deref(),
                            parent_uuid.as_deref(),
                        )?;
                        let (pairs, _) = task::parse_frontmatter(&md);
                        let task_name = name.unwrap_or_else(|| {
                            pairs
                                .iter()
                                .find(|(k, _)| k == "name")
                                .map(|(_, v)| v.clone())
                                .unwrap_or_default()
                        });
                        (uuid, commit, task_name)
                    } else {
                        let name = name.ok_or_else(|| {
                            anyhow::anyhow!("task name is required (or use --file)")
                        })?;
                        let (uuid, commit) = task::add_task(
                            backend,
                            &scope,
                            &name,
                            kind.as_deref(),
                            status.as_deref(),
                            description.as_deref(),
                            deadline.as_deref(),
                            importance,
                            None,
                            parent_uuid.as_deref(),
                        )?;
                        (uuid, commit, name)
                    };
                    // Propagate up unless --local or non-local scope.
                    if is_local_scope && !local_flag && global::global_exists(backend)? {
                        let importance = important.unwrap_or(0.0);
                        let root = local_root.as_ref().unwrap();
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
                            let mut conn = task::open_db(&global_scope)?;
                            conn.execute(
                                "UPDATE objects SET current_commit = ?1 WHERE uuid = ?2",
                                &[commit.as_str(), scope.project_uuid.as_str()],
                            )?;
                            task::dolt_commit_and_track_with(
                                backend,
                                &mut conn,
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
                TaskCommand::Deadlines {
                    important,
                    horizon,
                    board,
                } => {
                    let board_uuid = board
                        .as_ref()
                        .map(|b| task::resolve_task_uuid(backend, &scope, b))
                        .transpose()?;
                    let entries = task::list_deadlines(
                        &scope,
                        important,
                        horizon.as_deref(),
                        board_uuid.as_deref(),
                    )?;
                    print!("{}", task::format_deadlines(&entries));
                }
                TaskCommand::Set { name } => match name {
                    Some(ref n) => {
                        let mut conn = task::open_db(&scope)?;
                        let uuid = task::resolve_task_path(&mut conn, &scope, n, Some(backend))?;
                        drop(conn);
                        task::set_branch_task(backend, &scope, &uuid)?;
                        eprintln!(
                            "[subcontext] Current task for branch '{}' set to '{n}' ({uuid})",
                            scope.host_branch
                        );
                    }
                    None => {
                        task::unset_branch_task(backend, &scope)?;
                        eprintln!(
                            "[subcontext] Unset current task for branch '{}'",
                            scope.host_branch
                        );
                    }
                },
                TaskCommand::List { name } => {
                    let parent_uuid = match name {
                        Some(ref n) => {
                            let mut conn = task::open_db(&scope)?;
                            Some(task::resolve_task_path(
                                &mut conn,
                                &scope,
                                n,
                                Some(backend),
                            )?)
                        }
                        None => {
                            let mut conn = task::open_db(&scope)?;
                            task::get_branch_task(&mut conn, &scope.host_branch)?
                        }
                    };
                    let tasks = task::list_subtasks(&scope, parent_uuid.as_deref())?;
                    print!("{}", task::format_subtasks(&tasks, name.as_deref()));
                }
                TaskCommand::Roots => {
                    let uuids = task::list_root_uuids(&scope)?;
                    if uuids.is_empty() {
                        eprintln!("[subcontext] No root tasks.");
                    } else {
                        for uuid in &uuids {
                            println!("{uuid}");
                        }
                    }
                }
                TaskCommand::Tree {
                    format,
                    filter,
                    max_depth,
                    max_breadth,
                    max_size,
                    root,
                } => {
                    let tree_filter = match filter.as_str() {
                        "active" => task::TreeFilter::Active,
                        "all" => task::TreeFilter::All,
                        other => task::TreeFilter::Status(other.to_string()),
                    };
                    let opts = task::TreeOptions {
                        filter: tree_filter,
                        max_depth,
                        max_breadth,
                        max_size,
                    };
                    let root_uuid = match root {
                        Some(ref r) => Some(task::resolve_task_uuid(backend, &scope, r)?),
                        None => None,
                    };
                    let tree = task::build_task_tree(&scope, root_uuid.as_deref(), &opts)?;
                    match format.as_str() {
                        "json" => {
                            println!("{}", task::tree_to_json(&tree)?);
                        }
                        "mermaid" => {
                            print!("{}", task::tree_to_mermaid(&tree));
                        }
                        other => {
                            bail!("unknown format '{other}': expected 'json' or 'mermaid'");
                        }
                    }
                }
            }
        }
        Commands::Board {
            global: global_flag,
            user: user_flag,
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
                BoardCommand::Create {
                    name,
                    file,
                    kind,
                    status,
                    description,
                    deadline,
                    important,
                } => {
                    let importance = important.unwrap_or(0.0);
                    if let Some(file_path) = file {
                        let md = std::fs::read_to_string(&file_path)
                            .with_context(|| format!("cannot read {file_path}"))?;
                        task::create_board_from_md(backend, &scope, &md, None, name.as_deref())?;
                    } else {
                        let name = name.ok_or_else(|| {
                            anyhow::anyhow!("board name is required (or use --file)")
                        })?;
                        task::create_board(
                            backend,
                            &scope,
                            &name,
                            kind.as_deref(),
                            status.as_deref(),
                            description.as_deref(),
                            deadline.as_deref(),
                            importance,
                            None,
                        )?;
                    }
                }
                BoardCommand::AddTask {
                    name,
                    board,
                    parent,
                    kind,
                    status,
                    description,
                    deadline,
                    important,
                } => {
                    let importance = important.unwrap_or(0.0);
                    let board_uuid = task::resolve_task_uuid(backend, &scope, &board)?;
                    let parent_uuid = match parent {
                        Some(ref p) => task::resolve_task_uuid(backend, &scope, p)?,
                        None => board_uuid.clone(),
                    };
                    task::add_task_to_board(
                        backend,
                        &scope,
                        &board_uuid,
                        &parent_uuid,
                        &name,
                        kind.as_deref(),
                        status.as_deref(),
                        description.as_deref(),
                        deadline.as_deref(),
                        importance,
                    )?;
                }
                BoardCommand::Commit { uuid } => {
                    task::board_commit(backend, &scope, &uuid)?;
                }
                BoardCommand::DeleteTask { task, board } => {
                    let board_uuid = task::resolve_task_uuid(backend, &scope, &board)?;
                    let task_uuid = task::resolve_task_uuid(backend, &scope, &task)?;
                    task::delete_task_from_board(backend, &scope, &task_uuid, &board_uuid)?;
                }
                BoardCommand::MoveTask {
                    task,
                    parent,
                    board,
                } => {
                    let board_uuid = task::resolve_task_uuid(backend, &scope, &board)?;
                    let task_uuid = task::resolve_task_uuid(backend, &scope, &task)?;
                    let parent_uuid = task::resolve_task_uuid(backend, &scope, &parent)?;
                    task::move_task_in_board(
                        backend,
                        &scope,
                        &task_uuid,
                        &parent_uuid,
                        &board_uuid,
                    )?;
                }
                BoardCommand::Pull {
                    board,
                    path,
                    task: task_filter,
                    filter_done,
                } => {
                    let root = local_root
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("board pull requires a local git repo"))?;
                    let ctx = CheckoutContext::main_only(root);
                    let board_uuid = task::resolve_task_uuid(backend, &scope, &board)?;
                    let root_task = match task_filter {
                        Some(ref t) => Some(task::resolve_task_uuid(backend, &scope, t)?),
                        None => None,
                    };
                    task::board_pull(
                        backend,
                        &scope,
                        &ctx,
                        &board_uuid,
                        &path,
                        root_task.as_deref(),
                        filter_done,
                    )?;
                }
                BoardCommand::Push { path, mark_done } => {
                    let root = local_root
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("board push requires a local git repo"))?;
                    let ctx = CheckoutContext::main_only(root);
                    task::board_push(backend, &scope, &ctx, &path, mark_done)?;
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
                    // Commit the config worktree.
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
        Commands::Dolt { args } => {
            let dolt_bin = dolt::find_dolt_bin()?;
            // Determine the dolt repo path: use local project if available,
            // otherwise global.
            let dolt_repo = if let Ok(root) = git::find_main_git_root(backend, &cwd) {
                git::dolt_dir(&root)
            } else if let Ok(scope) = global::global_task_scope(backend) {
                scope.dolt_dir.clone()
            } else {
                anyhow::bail!("no subcontext found; run `subcontext install` first");
            };
            if !backend.is_dir(&dolt_repo) {
                anyhow::bail!(
                    "dolt repo not found at {}; run `subcontext install` to initialize",
                    dolt_repo.display()
                );
            }
            let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let status = std::process::Command::new(&dolt_bin)
                .args(&str_args)
                .current_dir(&dolt_repo)
                .status()
                .with_context(|| "failed to run dolt".to_string())?;
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
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
#[allow(clippy::too_many_arguments)]
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
                None,
            )?;
            return Ok(());
        }
    };

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
        None,
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
    if let Ok(scope) = global::global_task_scope(backend)
        && scope.project_uuid == uuid
    {
        return Ok(scope);
    }
    if let Ok(scope) = global::user_task_scope(backend)
        && scope.project_uuid == uuid
    {
        return Ok(scope);
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
