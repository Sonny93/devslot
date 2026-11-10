//! The command line, as `clap` reads it.

use std::ffi::OsString;

use clap::{Args, Parser, Subcommand, ValueEnum};

use super::exit_code::{
    describe_doctor_exit_codes, describe_env_exit_codes, describe_exit_codes,
    describe_info_exit_codes, describe_run_exit_codes,
};
use super::style::ColorChoice;

const NO_COLOR_HELP: &str =
    "  NO_COLOR         Disable color (also off when TERM=dumb or stdout is not a terminal)";
const DOCKER_HELP: &str = "  DEVSLOT_DOCKER   The docker program to run instead of `docker`";
const XDG_HELP: &str = "  XDG_CONFIG_HOME  Where devslot/config.toml lives, default ~/.config
  XDG_STATE_HOME   Where devslot/registry.toml lives, default ~/.local/state";

fn help_with(codes: &str, environment_lines: &[&str]) -> String {
    format!(
        "Exit codes:\n{codes}\n\nEnvironment:\n{}",
        environment_lines.join("\n")
    )
}

fn top_level_help() -> String {
    help_with(
        &describe_exit_codes(),
        &[NO_COLOR_HELP, DOCKER_HELP, XDG_HELP],
    )
}

fn doctor_help() -> String {
    help_with(
        &describe_doctor_exit_codes(),
        &[NO_COLOR_HELP, DOCKER_HELP, XDG_HELP],
    )
}

fn env_help() -> String {
    format!(
        "Values from [env] in devslot.toml are not printed (the file is committed, so the output could run code under eval); ds run passes them to the command. Values from devslot.local.toml are printed.\n\n{}",
        help_with(&describe_env_exit_codes(), &[XDG_HELP])
    )
}

fn info_help() -> String {
    help_with(&describe_info_exit_codes(), &[XDG_HELP])
}

fn run_help() -> String {
    help_with(
        &describe_run_exit_codes(),
        &[NO_COLOR_HELP, DOCKER_HELP, XDG_HELP],
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
    /// Print the environment of this checkout's instance
    #[command(after_help = env_help())]
    Env(EnvArguments),
    /// Show the name, slot and ports of this checkout's instance
    #[command(after_help = info_help())]
    Info(InfoArguments),
    /// Register this checkout if needed and run a command with the instance environment
    #[command(after_help = run_help())]
    Run(RunArguments),
}

/// How `ds env` prints the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EnvFormat {
    /// `export NAME='value'` lines, for `eval`.
    Sh,
    /// A JSON object.
    Json,
}

/// The options of `ds env`.
#[derive(Debug, Args)]
pub struct EnvArguments {
    /// The output format
    #[arg(long, value_enum, default_value_t = EnvFormat::Sh)]
    pub format: EnvFormat,
}

/// The options of `ds info`.
#[derive(Debug, Args)]
pub struct InfoArguments {
    /// Print the information as JSON, for agents and scripts
    #[arg(long)]
    pub json: bool,
}

/// The arguments of `ds run`.
#[derive(Debug, Args)]
pub struct RunArguments {
    /// The program and its arguments, after `--`. Run directly, never through a shell
    #[arg(last = true, required = true, value_name = "COMMAND")]
    pub command: Vec<OsString>,
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
    fn should_parse_the_command_after_the_double_dash_when_running() {
        let cli = Cli::try_parse_from(["ds", "run", "--", "just", "dev", "--flag"]).unwrap();

        let Command::Run(arguments) = cli.command else {
            panic!("expected run");
        };
        assert_eq!(
            arguments.command,
            ["just", "dev", "--flag"].map(OsString::from)
        );
    }

    #[test]
    fn should_reject_run_when_no_command_is_given() {
        assert!(Cli::try_parse_from(["ds", "run"]).is_err());
        assert!(Cli::try_parse_from(["ds", "run", "--"]).is_err());
    }

    #[test]
    fn should_reject_run_when_the_command_is_not_after_the_double_dash() {
        assert!(Cli::try_parse_from(["ds", "run", "just"]).is_err());
    }

    #[test]
    fn should_default_env_to_the_shell_format_when_none_is_given() {
        let cli = Cli::try_parse_from(["ds", "env"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Env(EnvArguments {
                format: EnvFormat::Sh
            })
        ));
    }

    #[test]
    fn should_parse_env_json_when_the_format_is_given() {
        let cli = Cli::try_parse_from(["ds", "env", "--format", "json"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Env(EnvArguments {
                format: EnvFormat::Json
            })
        ));
    }

    #[test]
    fn should_parse_info_json_when_the_flag_is_given() {
        let cli = Cli::try_parse_from(["ds", "info", "--json"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Info(InfoArguments { json: true })
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
