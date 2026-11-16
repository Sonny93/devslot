//! The command line, as `clap` reads it.

use std::ffi::OsString;

use clap::{Args, Parser, Subcommand, ValueEnum};

use super::exit_code::{
    describe_doctor_exit_codes, describe_down_exit_codes, describe_ensure_exit_codes,
    describe_env_exit_codes, describe_exit_codes, describe_hook_exit_codes,
    describe_info_exit_codes, describe_ls_exit_codes, describe_prune_exit_codes,
    describe_reset_exit_codes, describe_rm_exit_codes, describe_run_exit_codes,
    describe_up_exit_codes, describe_worktree_rm_exit_codes,
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

fn up_help() -> String {
    help_with(
        &describe_up_exit_codes(),
        &[NO_COLOR_HELP, DOCKER_HELP, XDG_HELP],
    )
}

fn ensure_help() -> String {
    help_with(
        &describe_ensure_exit_codes(),
        &[NO_COLOR_HELP, DOCKER_HELP, XDG_HELP],
    )
}

fn down_help() -> String {
    help_with(&describe_down_exit_codes(), &[DOCKER_HELP, XDG_HELP])
}

fn reset_help() -> String {
    help_with(&describe_reset_exit_codes(), &[DOCKER_HELP, XDG_HELP])
}

fn ls_help() -> String {
    help_with(&describe_ls_exit_codes(), &[DOCKER_HELP, XDG_HELP])
}

fn hook_help() -> String {
    help_with(&describe_hook_exit_codes(), &[DOCKER_HELP, XDG_HELP])
}

fn rm_help() -> String {
    help_with(&describe_rm_exit_codes(), &[DOCKER_HELP, XDG_HELP])
}

fn prune_help() -> String {
    help_with(&describe_prune_exit_codes(), &[DOCKER_HELP, XDG_HELP])
}

fn worktree_rm_help() -> String {
    help_with(&describe_worktree_rm_exit_codes(), &[DOCKER_HELP, XDG_HELP])
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
    /// Stop the services of this checkout's instance, keeping its data
    #[command(after_help = down_help())]
    Down,
    /// Register this checkout and write its .env block if needed, without starting anything
    #[command(after_help = ensure_help())]
    Ensure,
    /// Print the environment of this checkout's instance
    #[command(after_help = env_help())]
    Env(EnvArguments),
    /// Show the name, slot, ports and services state of this checkout's instance
    #[command(after_help = info_help())]
    Info(InfoArguments),
    /// Run a Claude Code hook (called by the devslot plugin, not by hand)
    #[command(hide = true, after_help = hook_help())]
    Hook(HookArguments),
    /// List every instance of this machine with its ports and whether its services run
    #[command(after_help = ls_help())]
    Ls(InfoArguments),
    /// Delete the containers and volumes of gone checkouts and forget them
    #[command(after_help = prune_help())]
    Prune(ConfirmationArguments),
    /// Delete the data of this checkout's instance (or of one service) and start it again
    #[command(after_help = reset_help())]
    Reset(ResetArguments),
    /// Delete the instance's data, remove the .env block and opt the checkout out
    #[command(after_help = rm_help())]
    Rm(ConfirmationArguments),
    /// Register this checkout if needed and run a command with the instance environment
    #[command(after_help = run_help())]
    Run(RunArguments),
    /// Register this checkout if needed, start its services, then run the run key (unless --no-run)
    #[command(after_help = up_help())]
    Up(UpArguments),
    /// Manage the linked worktrees of this repository
    Worktree(WorktreeArguments),
}

/// The arguments of `ds worktree`.
#[derive(Debug, Args)]
pub struct WorktreeArguments {
    /// What to do with a worktree
    #[command(subcommand)]
    pub action: WorktreeAction,
}

/// The actions of `ds worktree`.
#[derive(Debug, Subcommand)]
pub enum WorktreeAction {
    /// Stop the processes of a worktree's instance, delete its data and remove the worktree
    #[command(after_help = worktree_rm_help())]
    Rm(WorktreeRmArguments),
}

/// The arguments of `ds worktree rm`.
#[derive(Debug, Args)]
pub struct WorktreeRmArguments {
    /// A worktree name of this repository, or a path (it holds `/` or starts with `.`)
    #[arg(value_name = "NAME_OR_PATH")]
    pub target: String,
    /// Do not ask for confirmation
    #[arg(long)]
    pub yes: bool,
    /// Remove the worktree even with uncommitted changes, commits found nowhere else, or processes of another session or program running in it
    #[arg(long)]
    pub force: bool,
}

/// The arguments of `ds hook`.
#[derive(Debug, Args)]
pub struct HookArguments {
    /// The Claude Code event; the hook reads its JSON input on stdin
    #[command(subcommand)]
    pub event: HookEvent,
}

/// The Claude Code events `ds hook` answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
pub enum HookEvent {
    /// Create the git worktree of a session, register it and print its path
    WorktreeCreate,
    /// Delete the data of a session's worktree, then the worktree and its merged branch
    WorktreeRemove,
    /// Deny `ds rm` and `ds prune`, and make sure the checkout is registered
    PreToolUse,
    /// Tell the agent which instance its checkout is
    SessionStart,
}

/// The arguments of `ds up`.
#[derive(Debug, Args)]
pub struct UpArguments {
    /// Start the services only, without running the run key of devslot.toml
    #[arg(long)]
    pub no_run: bool,
}

/// The options of the commands that delete data after asking.
#[derive(Debug, Args)]
pub struct ConfirmationArguments {
    /// Do not ask for confirmation
    #[arg(long)]
    pub yes: bool,
}

/// The arguments of `ds reset`.
#[derive(Debug, Args)]
pub struct ResetArguments {
    /// The compose service to reset; every service when omitted
    #[arg(value_name = "SERVICE")]
    pub service: Option<String>,
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

/// The options of `ds info` and `ds ls`.
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
    fn should_parse_ls_with_json_when_the_flag_is_given() {
        let cli = Cli::try_parse_from(["ds", "ls", "--json"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Ls(InfoArguments { json: true })
        ));
    }

    #[test]
    fn should_parse_each_hook_event_when_it_is_named() {
        let events = [
            ("worktree-create", HookEvent::WorktreeCreate),
            ("worktree-remove", HookEvent::WorktreeRemove),
            ("pre-tool-use", HookEvent::PreToolUse),
            ("session-start", HookEvent::SessionStart),
        ];
        for (name, event) in events {
            let cli = Cli::try_parse_from(["ds", "hook", name]).unwrap();

            let Command::Hook(arguments) = cli.command else {
                panic!("expected hook");
            };
            assert_eq!(arguments.event, event);
        }
    }

    #[test]
    fn should_reject_a_hook_when_the_event_is_unknown_or_missing() {
        assert!(Cli::try_parse_from(["ds", "hook"]).is_err());
        assert!(Cli::try_parse_from(["ds", "hook", "stop"]).is_err());
    }

    #[test]
    fn should_parse_rm_with_yes_when_the_flag_is_given() {
        let cli = Cli::try_parse_from(["ds", "rm", "--yes"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Rm(ConfirmationArguments { yes: true })
        ));
    }

    #[test]
    fn should_ask_by_default_when_prune_has_no_flag() {
        let cli = Cli::try_parse_from(["ds", "prune"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Prune(ConfirmationArguments { yes: false })
        ));
    }

    #[test]
    fn should_parse_the_service_when_reset_names_one() {
        let cli = Cli::try_parse_from(["ds", "reset", "postgres"]).unwrap();

        let Command::Reset(arguments) = cli.command else {
            panic!("expected reset");
        };
        assert_eq!(arguments.service.as_deref(), Some("postgres"));
    }

    #[test]
    fn should_reset_everything_when_no_service_is_named() {
        let cli = Cli::try_parse_from(["ds", "reset"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Reset(ResetArguments { service: None })
        ));
    }

    #[test]
    fn should_parse_worktree_rm_with_its_flags() {
        let cli =
            Cli::try_parse_from(["ds", "worktree", "rm", "feat", "--yes", "--force"]).unwrap();

        let Command::Worktree(WorktreeArguments {
            action: WorktreeAction::Rm(arguments),
        }) = cli.command
        else {
            panic!("expected worktree rm");
        };
        assert_eq!(arguments.target, "feat");
        assert!(arguments.yes);
        assert!(arguments.force);
    }

    #[test]
    fn should_ask_and_refuse_force_by_default_when_worktree_rm_has_no_flag() {
        let cli = Cli::try_parse_from(["ds", "worktree", "rm", "./x"]).unwrap();

        let Command::Worktree(WorktreeArguments {
            action: WorktreeAction::Rm(arguments),
        }) = cli.command
        else {
            panic!("expected worktree rm");
        };
        assert!(!arguments.yes);
        assert!(!arguments.force);
    }

    #[test]
    fn should_reject_worktree_without_an_action_or_rm_without_a_target() {
        assert!(Cli::try_parse_from(["ds", "worktree"]).is_err());
        assert!(Cli::try_parse_from(["ds", "worktree", "rm"]).is_err());
        assert!(Cli::try_parse_from(["ds", "worktree", "remove", "x"]).is_err());
    }

    #[test]
    fn should_parse_the_commands_without_arguments() {
        for name in ["up", "down", "ensure"] {
            assert!(Cli::try_parse_from(["ds", name]).is_ok(), "{name}");
        }
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
