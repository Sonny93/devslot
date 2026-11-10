//! The command line, as `clap` reads it.

use clap::{Args, Parser, Subcommand};

use super::exit_code::{describe_doctor_exit_codes, describe_exit_codes};
use super::style::ColorChoice;

const ENVIRONMENT_HELP: &str = "\
Environment:
  NO_COLOR        Disable color (also off when TERM=dumb or stdout is not a terminal)
  DEVSLOT_DOCKER  The docker program to run instead of `docker`";

fn top_level_help() -> String {
    format!(
        "Exit codes:\n{}\n\n{ENVIRONMENT_HELP}",
        describe_exit_codes()
    )
}

fn doctor_help() -> String {
    format!(
        "Exit codes:\n{}\n\n{ENVIRONMENT_HELP}",
        describe_doctor_exit_codes()
    )
}

/// The `ds` command line.
#[derive(Debug, Parser)]
#[command(name = "ds", version, about = "Run several instances of the same dev stack on one machine", after_help = top_level_help())]
pub struct Cli {
    /// When to color the output.
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,

    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// The commands of `ds`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Review the project without starting or writing anything
    #[command(after_help = doctor_help())]
    Doctor(DoctorArguments),
}

/// The options of `ds doctor`.
#[derive(Debug, Args)]
pub struct DoctorArguments {
    /// Print the report as JSON, for agents and scripts
    #[arg(long)]
    pub json: bool,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn should_parse_doctor_with_json_when_the_flag_is_given() {
        let cli = Cli::try_parse_from(["ds", "doctor", "--json"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Doctor(DoctorArguments { json: true })
        ));
    }

    #[test]
    fn should_reject_the_command_line_when_the_command_is_unknown() {
        assert!(Cli::try_parse_from(["ds", "nothing"]).is_err());
    }

    #[test]
    fn should_accept_color_after_the_subcommand_when_it_is_global() {
        let cli = Cli::try_parse_from(["ds", "doctor", "--color", "never"]).unwrap();

        assert_eq!(cli.color, ColorChoice::Never);
    }
}
