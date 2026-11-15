//! devslot: run several instances of the same dev stack on one machine.
//!
//! Phase 1 holds the port discovery engine and the read-only `ds doctor`;
//! phase 2 adds the registry, port allocation, `ds env`, `ds info` and `ds run`.
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
pub mod discovery;
pub mod docker;
pub mod environment;
pub mod error;
pub mod global_config;
pub mod instance_name;
pub mod paths;
pub mod port_probe;
pub mod port_range;
pub mod project;
pub mod project_config;
pub mod references;
pub mod registration;
pub mod registry;
pub mod report;
pub mod reserved_names;
pub mod shell_quote;

use std::process::ExitCode;

use clap::Parser;

use cli::args::{Cli, Command};
use cli::exit_code;
use commands::doctor::run_doctor_command;
use commands::env::run_env;
use commands::info::run_info;
use commands::run::run_command;
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
        Command::Env(arguments) => run_env(arguments.format),
        Command::Info(arguments) => run_info(arguments.json),
        Command::Run(arguments) => run_command(&arguments.command, cli.color),
    }
}
