//! devslot: run several instances of the same dev stack on one machine.
//!
//! Phase 1 holds the port discovery engine and the read-only `ds doctor`;
//! phase 2 adds the registry, port allocation, `ds env`, `ds info` and `ds run`;
//! phase 3 adds the `.env` block writer and `ds up`, `ds down`, `ds reset`,
//! `ds rm`, `ds prune` and `ds ensure`;
//! phase 4 adds `ds ls`, the `ds hook` commands and the Claude Code plugin.
#![warn(missing_docs)]

pub mod allocation;
pub mod analysis;
pub mod auto_scope;
pub mod block_entries;
pub mod checkout;
pub mod checks;
pub mod cli;
pub mod commands;
pub mod compose;
pub mod deadline;
pub mod discovery;
pub mod docker;
pub mod ensure_cache;
pub mod env_sync;
pub mod environment;
pub mod error;
pub mod git_worktree;
pub mod glob_match;
pub mod global_config;
pub mod instance_name;
pub mod instance_processes;
pub mod kept_worktrees;
pub mod paths;
pub mod port_check;
pub mod port_probe;
pub mod port_range;
pub mod process;
pub mod project;
pub mod project_config;
pub mod references;
pub mod registration;
pub mod registry;
pub mod report;
pub mod require_scope;
pub mod reserved_names;
pub mod reset_plan;
pub mod services_state;
pub mod session_leases;
pub mod shell_quote;
#[cfg(test)]
pub(crate) mod test_executable;
pub mod worktree_guard;
pub mod worktree_identity;
pub mod worktree_lock;
pub mod worktree_require_override;

use std::process::ExitCode;

use clap::Parser;

use cli::args::{Cli, Command, WorktreeAction};
use cli::exit_code;
use commands::doctor::run_doctor_command;
use commands::down::run_down;
use commands::ensure::run_ensure;
use commands::env::run_env;
use commands::hook::run_hook;
use commands::info::run_info;
use commands::ls::run_ls;
use commands::prune::run_prune;
use commands::reset::run_reset;
use commands::rm::run_rm;
use commands::run::run_command;
use commands::up::run_up;
use commands::worktree_land::run_worktree_land;
use commands::worktree_rm::run_worktree_rm;
use error::ToolError;

/// Parses the command line, runs the command and returns the exit code.
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    match execute(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("ds: error: {error}");
            ExitCode::from(exit_code::TOOL_FAILURE)
        }
    }
}

fn execute(cli: &Cli) -> Result<u8, ToolError> {
    match &cli.command {
        Command::Doctor(arguments) => run_doctor_command(cli.color, arguments.json),
        Command::Down => run_down(),
        Command::Ensure => run_ensure(cli.color),
        Command::Env(arguments) => run_env(arguments.format),
        Command::Hook(arguments) => Ok(run_hook(arguments.event)),
        Command::Info(arguments) => run_info(arguments.json),
        Command::Ls(arguments) => run_ls(arguments.json),
        Command::Prune(arguments) => run_prune(arguments.yes.into()),
        Command::Reset(arguments) => run_reset(arguments.service.as_deref()),
        Command::Rm(arguments) => run_rm(arguments.yes.into()),
        Command::Run(arguments) => run_command(&arguments.command, cli.color),
        Command::Up(arguments) => run_up(cli.color, arguments),
        Command::Worktree(arguments) => match &arguments.action {
            WorktreeAction::Land(land) => run_worktree_land(land),
            WorktreeAction::Rm(rm) => run_worktree_rm(rm),
        },
    }
}
