//! devslot: run several instances of the same dev stack on one machine.
//!
//! Phase 1 holds the port discovery engine and the read-only `ds doctor`.
#![warn(missing_docs)]

pub mod checkout;
pub mod checks;
pub mod cli;
pub mod commands;
pub mod compose;
pub mod discovery;
pub mod docker;
pub mod error;
pub mod project;
pub mod project_config;
pub mod report;

use std::io::Write;
use std::process::ExitCode;

use clap::Parser;

use cli::args::{Cli, Command};
use cli::exit_code;
use cli::render::render_text;
use cli::style::Style;
use commands::doctor::run_doctor;
use docker::DockerCompose;
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
        Command::Doctor(arguments) => doctor(cli, arguments.json),
    }
}

fn doctor(cli: &Cli, as_json: bool) -> Result<u8, ToolError> {
    let directory = std::env::current_dir().map_err(ToolError::CurrentDirectory)?;
    let checkout = checkout::Checkout::locate(&directory)?;
    let compose = DockerCompose::from_environment(&checkout.root);
    let report = run_doctor(&checkout, &compose)?;
    let text = if as_json {
        let mut json = serde_json::to_string_pretty(&report)
            .map_err(|error| ToolError::WriteReport(std::io::Error::other(error)))?;
        json.push('\n');
        json
    } else {
        render_text(&report, Style::for_process(cli.color))
    };
    write_report(&text)?;
    Ok(if report.has_errors() {
        exit_code::ERROR
    } else {
        exit_code::SUCCESS
    })
}

fn write_report(text: &str) -> Result<(), ToolError> {
    match std::io::stdout().write_all(text.as_bytes()) {
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(ToolError::WriteReport(error)),
        Ok(()) => Ok(()),
    }
}
