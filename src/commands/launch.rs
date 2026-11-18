//! Replacing the `ds` process with the command of `ds run` or `ds up`, run with
//! the instance environment.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::cli::exit_code;
use crate::environment::{
    EnvironmentVariable, INSTANCE_VARIABLE, InstanceEnvironment, VariableSource,
};
use crate::instance_processes::RUN_VARIABLE;

const SHELL_PROGRAM: &str = "/bin/sh";
const SHELL_COMMAND_FLAG: &str = "-c";

/// Prints a warning for each variable of the instance that the caller's
/// environment already sets to another value.
pub fn warn_about_replacements(environment: &InstanceEnvironment) {
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

/// Replaces this process with `script` run by `/bin/sh -c`. Returns only when
/// the shell could not be started, with the exit code to use.
pub fn exec_shell_script(script: &str, environment: &InstanceEnvironment) -> u8 {
    let command = [
        OsString::from(SHELL_PROGRAM),
        OsString::from(SHELL_COMMAND_FLAG),
        OsString::from(script),
    ];
    exec_command(&command, environment)
}

/// Replaces this process with `command`, run directly and never through a
/// shell. Returns only when it could not be started, with the exit code to use.
pub fn exec_command(command: &[OsString], environment: &InstanceEnvironment) -> u8 {
    let Some((program, arguments)) = command.split_first() else {
        return exit_code::USAGE;
    };
    let mut launched = Command::new(program);
    launched.args(arguments).envs(
        environment
            .variables
            .iter()
            .map(|variable| (&variable.name, &variable.value)),
    );
    if let Some(name) = launched_marker(environment) {
        launched.env(RUN_VARIABLE, name);
    }
    let error = launched.exec();
    eprintln!(
        "ds: error: cannot run {}: {error}",
        program.to_string_lossy()
    );
    exit_code_for(error.kind())
}

/// The instance name for `DEVSLOT_RUN`: the marker `ds down` looks for, set
/// only on what devslot launches, never by `ds env` nor the `.env` block.
fn launched_marker(environment: &InstanceEnvironment) -> Option<&str> {
    environment
        .variables
        .iter()
        .find(|variable| variable.name == INSTANCE_VARIABLE)
        .map(|variable| variable.value.as_str())
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
    fn should_mark_the_launched_process_with_the_instance_name() {
        let environment = InstanceEnvironment {
            variables: vec![variable(
                INSTANCE_VARIABLE,
                "app-1",
                VariableSource::Instance,
            )],
        };

        assert_eq!(launched_marker(&environment), Some("app-1"));
    }

    #[test]
    fn should_not_mark_the_process_when_the_environment_has_no_instance() {
        let environment = InstanceEnvironment { variables: vec![] };

        assert_eq!(launched_marker(&environment), None);
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
