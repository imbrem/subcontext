mod clone;
mod git;
mod hook;
mod install;
mod overlay;
mod settings;
mod startup;
mod status;
mod submodule;
mod uninstall;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use std::env;
use std::path::Path;

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

    /// Internal hook dispatcher (not for direct use)
    #[command(name = "_hook", hide = true)]
    Hook {
        #[command(subcommand)]
        hook: HookCommand,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = env::current_dir()?;

    match cli.command {
        Commands::Install { repair } => {
            let root = git::find_main_git_root(&cwd)?;
            install::install(&root, repair)?;
        }
        Commands::Clone { url } => {
            let root = git::find_main_git_root(&cwd)?;
            clone::clone(&root, &url)?;
        }
        Commands::Add { files } => {
            let root = git::find_main_git_root(&cwd)?;
            let ctx = CheckoutContext::main_only(&root);
            for file in &files {
                let resolved = resolve_file_path(&cwd, &root, file)?;
                overlay::add_file(&ctx, &resolved)?;
                eprintln!("[subcontext] Added: {resolved}");
            }
        }
        Commands::Save { message } => {
            let root = git::find_main_git_root(&cwd)?;
            let ctx = CheckoutContext::main_only(&root);
            let msg = message.as_deref().unwrap_or("manual save");
            overlay::save_overlay(&ctx, msg)?;
            eprintln!("[subcontext] Saved overlay changes.");
        }
        Commands::Remove { files } => {
            let root = git::find_main_git_root(&cwd)?;
            let ctx = CheckoutContext::main_only(&root);
            for file in &files {
                let resolved = resolve_file_path(&cwd, &root, file)?;
                overlay::remove_file(&ctx, &resolved)?;
                eprintln!("[subcontext] Removed: {resolved}");
            }
        }
        Commands::Startup { .. } => {
            startup::startup()?;
        }
        Commands::Uninstall => {
            let root = git::find_main_git_root(&cwd)?;
            uninstall::uninstall(&root)?;
        }
        Commands::Status => {
            status::status(&cwd)?;
        }
        Commands::Submodule { command } => {
            let root = git::find_main_git_root(&cwd)?;
            let ctx = CheckoutContext::main_only(&root);
            match command {
                SubmoduleCommand::Add { url, path } => {
                    let resolved = path
                        .as_ref()
                        .map(|p| resolve_new_path(&cwd, &root, p))
                        .transpose()?;
                    submodule::add(&ctx, &url, resolved.as_deref())?;
                }
                SubmoduleCommand::Update => {
                    submodule::update(&ctx)?;
                }
                SubmoduleCommand::Remove { path } => {
                    let resolved = resolve_new_path(&cwd, &root, &path)?;
                    submodule::remove(&ctx, &resolved)?;
                }
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
            let ctx = match git::find_checkout_context(&cwd) {
                Ok(ctx) => ctx,
                Err(e) => {
                    eprintln!("[subcontext] warning: {e:#}");
                    return Ok(());
                }
            };
            hook::post_checkout(&ctx, &prev_head, &new_head, &flag)?;
        }
        Commands::Hook {
            hook: HookCommand::PostCommit,
        } => {
            let ctx = match git::find_checkout_context(&cwd) {
                Ok(ctx) => ctx,
                Err(e) => {
                    eprintln!("[subcontext] warning: {e:#}");
                    return Ok(());
                }
            };
            hook::post_commit(&ctx)?;
        }
    }

    Ok(())
}

/// Resolve a path that may not exist yet (e.g., submodule destination) to be relative to root.
fn resolve_new_path(cwd: &Path, root: &Path, path: &str) -> Result<String> {
    let root_canonical = root.canonicalize().unwrap_or(root.to_path_buf());

    let abs = if Path::new(path).is_absolute() {
        Path::new(path).to_path_buf()
    } else {
        let cwd_canonical = cwd.canonicalize().unwrap_or(cwd.to_path_buf());
        cwd_canonical.join(path)
    };

    // Try canonicalize (resolves ..), fall back to manual normalization
    let abs = if let Ok(canonical) = abs.canonicalize() {
        canonical
    } else {
        normalize_path(&abs)
    };

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
fn resolve_file_path(cwd: &Path, root: &Path, file: &str) -> Result<String> {
    let abs = if Path::new(file).is_absolute() {
        Path::new(file).to_path_buf()
    } else {
        cwd.join(file)
    };

    let abs = abs.canonicalize().unwrap_or(abs);
    let root_canonical = root.canonicalize().unwrap_or(root.to_path_buf());

    match abs.strip_prefix(&root_canonical) {
        Ok(rel) => Ok(rel.to_string_lossy().to_string()),
        Err(_) => bail!("file {file} is outside the repository root"),
    }
}
