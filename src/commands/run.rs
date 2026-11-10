//! `ds run -- <command>`: register the checkout if needed, then replace this
//! process with the command, run with the instance environment.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::Duration;

use crate::cli::exit_code;
use crate::cli::render::render_findings;
use crate::cli::style::{ColorChoice, Style};
use crate::commands::machine::{Machine, load_environment, locate_checkout};
use crate::docker::DockerCompose;
use crate::environment::{EnvironmentVariable, InstanceEnvironment, VariableSource};
use crate::error::ToolError;
use crate::global_config::GlobalConfig;
use crate::port_probe::SystemPortProbe;
use crate::registration::{
    Refusal, Registration, RegistrationContext, RegistrationOutcome, register,
};

/// How long `ds run` waits for the registry lock.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// Registers the checkout, then runs `command`. Returns only when the command
/// could not be started, with the exit code to use.
pub fn run_command(command: &[OsString], color: ColorChoice) -> Result<u8, ToolError> {
    let checkout = locate_checkout()?;
    let machine = Machine::from_process()?;
    let global = GlobalConfig::load(&machine.paths.config_file)?;
    let compose = DockerCompose::from_environment(&checkout.root);
    let outcome = register(&RegistrationContext {
        checkout: &checkout,
        compose: &compose,
        store: &machine.store,
        geometry: global.slots,
        probe: &SystemPortProbe,
        lock_timeout: LOCK_TIMEOUT,
    })?;
    let style = Style::for_stderr(color);
    let registration = match outcome {
        RegistrationOutcome::Registered(registration) => registration,
        RegistrationOutcome::Refused(refusal) => {
            return Ok(report_refusal(&refusal, &machine, style));
        }
    };
    announce(&registration, style);
    let environment = load_environment(&checkout, &registration.instance)?;
    warn_about_replacements(&environment);
    Ok(exec_command(command, &environment))
}

fn report_refusal(refusal: &Refusal, machine: &Machine, style: Style) -> u8 {
    match refusal {
        Refusal::Blocked(findings) => {
            eprintln!("ds: the checks found problems in this checkout:");
            eprint!("{}", render_findings(findings, style));
            eprintln!("ds: error: ds run does not start the command until the errors are fixed");
        }
        Refusal::NoRoom(error) => {
            let message = Refusal::no_room_message(error, &machine.paths.config_file);
            eprintln!("ds: error: {message}");
        }
    }
    exit_code::ERROR
}

fn announce(registration: &Registration, style: Style) {
    if !registration.warnings.is_empty() {
        eprintln!("ds: the checks report warnings:");
        eprint!("{}", render_findings(&registration.warnings, style));
    }
    for notice in registration.notices() {
        eprintln!("{notice}");
    }
}

fn warn_about_replacements(environment: &InstanceEnvironment) {
    for variable in &environment.variables {
        if let Some(message) = replacement_warning(variable, std::env::var_os(&variable.name)) {
            eprintln!("{message}");
        }
    }
}

/// The warning for a variable the caller's environment already sets to another
/// value. The caller's value is shown only for the variables devslot owns
/// (the compose project, the compose files and the devslot ids); the name of
/// anything else is chosen by the repository, so its old value stays hidden.
/// A port variable shows its new value, which is a port number.
fn replacement_warning(
    variable: &EnvironmentVariable,
    current: Option<OsString>,
) -> Option<String> {
    let current = current?;
    if current == variable.value.as_str() {
        return None;
    }
    let name = &variable.name;
    Some(match variable.source {
        VariableSource::Instance => format!(
            "ds: warning: replacing {name} ({} in the environment) with {}",
            current.to_string_lossy(),
            variable.value
        ),
        VariableSource::Port => format!(
            "ds: warning: replacing {name} from the environment with {}",
            variable.value
        ),
        VariableSource::Config => format!("ds: warning: replacing {name} from the environment"),
    })
}

fn exec_command(command: &[OsString], environment: &InstanceEnvironment) -> u8 {
    let Some((program, arguments)) = command.split_first() else {
        return exit_code::USAGE;
    };
    let error = Command::new(program)
        .args(arguments)
        .envs(
            environment
                .variables
                .iter()
                .map(|variable| (&variable.name, &variable.value)),
        )
        .exec();
    eprintln!(
        "ds: error: cannot run {}: {error}",
        program.to_string_lossy()
    );
    exit_code_for(error.kind())
}

const fn exit_code_for(kind: ErrorKind) -> u8 {
    match kind {
        ErrorKind::NotFound => exit_code::COMMAND_NOT_FOUND,
        _ => exit_code::COMMAND_NOT_EXECUTABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variable(name: &str, value: &str, source: VariableSource) -> EnvironmentVariable {
        EnvironmentVariable {
            name: name.to_owned(),
            value: value.to_owned(),
            source,
        }
    }

    #[test]
    fn should_show_both_values_when_a_port_variable_is_replaced() {
        let warning = replacement_warning(
            &variable("PORT", "20050", VariableSource::Port),
            Some(OsString::from("20100")),
        );

        assert_eq!(
            warning.as_deref(),
            Some("ds: warning: replacing PORT from the environment with 20050")
        );
    }

    #[test]
    fn should_not_show_the_old_value_when_a_port_variable_holds_a_token_in_the_environment() {
        let warning = replacement_warning(
            &variable("GITHUB_TOKEN", "20051", VariableSource::Port),
            Some(OsString::from("ghp_fake")),
        );

        assert_eq!(
            warning.as_deref(),
            Some("ds: warning: replacing GITHUB_TOKEN from the environment with 20051")
        );
    }

    #[test]
    fn should_show_the_values_when_the_compose_project_is_replaced() {
        let warning = replacement_warning(
            &variable("COMPOSE_PROJECT_NAME", "app-1", VariableSource::Instance),
            Some(OsString::from("old")),
        );

        assert_eq!(
            warning.as_deref(),
            Some("ds: warning: replacing COMPOSE_PROJECT_NAME (old in the environment) with app-1")
        );
    }

    #[test]
    fn should_not_show_any_value_when_a_configured_key_is_replaced() {
        let warning = replacement_warning(
            &variable("API_TOKEN_URL", "http://x", VariableSource::Config),
            Some(OsString::from("FAKE-SECRET")),
        );

        assert_eq!(
            warning.as_deref(),
            Some("ds: warning: replacing API_TOKEN_URL from the environment")
        );
    }

    #[test]
    fn should_not_warn_when_the_environment_already_has_the_same_value() {
        let warning = replacement_warning(
            &variable("PORT", "20050", VariableSource::Port),
            Some(OsString::from("20050")),
        );

        assert_eq!(warning, None);
    }

    #[test]
    fn should_not_warn_when_the_environment_does_not_set_the_variable() {
        let warning = replacement_warning(&variable("PORT", "20050", VariableSource::Port), None);

        assert_eq!(warning, None);
    }

    #[test]
    fn should_use_127_when_the_program_is_not_found() {
        assert_eq!(exit_code_for(ErrorKind::NotFound), 127);
    }

    #[test]
    fn should_use_126_when_permission_is_denied() {
        assert_eq!(exit_code_for(ErrorKind::PermissionDenied), 126);
    }
}
