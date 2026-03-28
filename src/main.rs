mod clone;
mod git;
mod hook;
mod install;
mod settings;
mod startup;
mod status;
mod uninstall;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::env;

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
        /// Re-install hooks even if they already contain subcontext dispatchers,
        /// backing up the existing hook to hooks/backup/ (not executed)
        #[arg(long)]
        repair: bool,
    },

    /// Clone an existing subcontext repo and attach it to this project
    Clone {
        /// URL of the context repo to clone
        url: String,
    },

    /// Print task context for Claude's SessionStart hook
    Startup,

    /// Remove subcontext hooks and Claude settings from the current project
    Uninstall,

    /// Show current repo, worktree, and subcontext status
    Status,

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
        Commands::Startup => {
            let root = git::find_main_git_root(&cwd)?;
            startup::startup(&root)?;
        }
        Commands::Uninstall => {
            let root = git::find_main_git_root(&cwd)?;
            uninstall::uninstall(&root)?;
        }
        Commands::Status => {
            status::status(&cwd)?;
        }
        Commands::Hook {
            hook:
                HookCommand::PostCheckout {
                    prev_head,
                    new_head,
                    flag,
                },
        } => {
            // The hook must never fail in a way that aborts git
            let root = match git::find_main_git_root(&cwd) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[subcontext] warning: {e:#}");
                    return Ok(());
                }
            };
            hook::post_checkout(&root, &prev_head, &new_head, &flag)?;
        }
    }

    Ok(())
}
