//! `ds run -- <command>`: register the checkout if needed, then replace this
//! process with the command, run with the instance environment.

use std::ffi::OsString;

use crate::cli::style::{ColorChoice, Style};
use crate::commands::launch::{exec_command, warn_about_replacements};
use crate::commands::machine::{Machine, load_environment, locate_checkout};
use crate::commands::registering::{announce, register_checkout, registration_or_exit_code};
use crate::error::ToolError;
use crate::global_config::GlobalConfig;
use crate::registration::Intent;

const REFUSAL_CLOSING_LINE: &str = "ds run does not start the command until the errors are fixed";

/// Registers the checkout, then runs `command`. Returns only when the command
/// could not be started, with the exit code to use.
pub fn run_command(command: &[OsString], color: ColorChoice) -> Result<u8, ToolError> {
    let checkout = locate_checkout()?;
    let machine = Machine::from_process()?;
    let global = GlobalConfig::load(&machine.paths.config_file)?;
    let outcome = register_checkout(&checkout, &machine, &global, Intent::Explicit)?;
    let style = Style::for_stderr(color);
    let registration =
        match registration_or_exit_code(outcome, &machine, style, REFUSAL_CLOSING_LINE) {
            Ok(registration) => registration,
            Err(code) => return Ok(code),
        };
    announce(&registration, style);
    let environment = load_environment(&checkout, &registration.instance)?;
    warn_about_replacements(&environment);
    Ok(exec_command(command, &environment))
}
