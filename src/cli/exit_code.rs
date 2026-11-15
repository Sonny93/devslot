//! The exit codes of the `ds` command, as documented in `--help`.

/// Everything went well.
pub const SUCCESS: u8 = 0;
/// `doctor`: at least one error was found in the project.
pub const ERROR: u8 = 1;
/// The command line is invalid. Returned by the argument parser itself.
pub const USAGE: u8 = 2;
/// The tool itself failed: Docker or git unavailable, unreadable file, timeout.
pub const TOOL_FAILURE: u8 = 2;
/// `run`: the command was found but could not be executed.
pub const COMMAND_NOT_EXECUTABLE: u8 = 126;
/// `run`: the command was not found.
pub const COMMAND_NOT_FOUND: u8 = 127;

fn format_codes(codes: &[(u8, &str)]) -> String {
    codes
        .iter()
        .map(|(code, meaning)| format!("  {code:<3}  {meaning}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The exit codes in the form the top-level `--help` shows them.
pub fn describe_exit_codes() -> String {
    [
        (SUCCESS, "success"),
        (
            ERROR,
            "error (doctor: at least one error was found; env, down, reset: checkout not registered; run, up, ensure: refused to register or start; down, reset, rm, prune: refused or Compose failed)",
        ),
        (USAGE, "invalid usage, or the tool itself failed"),
    ]
    .map(|(code, meaning)| format!("  {code}  {meaning}"))
    .join("\n")
}

/// The exit codes in the form `ds env --help` shows them.
pub fn describe_env_exit_codes() -> String {
    format_codes(&[
        (SUCCESS, "the environment was printed"),
        (ERROR, "this checkout is not registered"),
        (
            TOOL_FAILURE,
            "the tool itself failed (unreadable config or registry, invalid [env]), or invalid usage",
        ),
    ])
}

/// The exit codes in the form `ds info --help` shows them.
pub fn describe_info_exit_codes() -> String {
    format_codes(&[
        (
            SUCCESS,
            "the information was printed (also for an unregistered checkout)",
        ),
        (
            TOOL_FAILURE,
            "the tool itself failed (unreadable config or registry), or invalid usage",
        ),
    ])
}

/// The exit codes in the form `ds run --help` shows them.
pub fn describe_run_exit_codes() -> String {
    format_codes(&[
        (
            SUCCESS,
            "the command's own exit code, whatever it is (1 can also mean ds refused to start it)",
        ),
        (
            ERROR,
            "ds refused to start the command: the checks found an error, or no slot or port is free",
        ),
        (
            TOOL_FAILURE,
            "the tool itself failed (Docker or git unavailable, registry locked, unreadable file), or invalid usage",
        ),
        (
            COMMAND_NOT_EXECUTABLE,
            "the command was found but cannot be executed (permission denied, no interpreter)",
        ),
        (COMMAND_NOT_FOUND, "the command was not found"),
    ])
}

/// The exit codes in the form `ds up --help` shows them.
pub fn describe_up_exit_codes() -> String {
    format_codes(&[
        (
            SUCCESS,
            "the services are up; with a run key, the command's own exit code, whatever it is",
        ),
        (
            ERROR,
            "ds refused to start: the checks found an error, no slot is free, a port is used by another program, or the services did not start",
        ),
        (
            TOOL_FAILURE,
            "the tool itself failed (Docker or git unavailable, registry locked, unreadable file), or invalid usage",
        ),
        (
            COMMAND_NOT_EXECUTABLE,
            "the run command was found but cannot be executed",
        ),
        (COMMAND_NOT_FOUND, "the run command was not found"),
    ])
}

/// The exit codes in the form `ds ensure --help` shows them.
pub fn describe_ensure_exit_codes() -> String {
    format_codes(&[
        (
            SUCCESS,
            "the checkout is registered and its .env is current, or there was nothing to do (outside git, outside [auto], opted out, no dev compose)",
        ),
        (
            ERROR,
            "ds refused: the checks found an error, or no slot is free",
        ),
        (
            TOOL_FAILURE,
            "the tool itself failed (Docker or git unavailable, registry locked, unreadable file), or invalid usage",
        ),
    ])
}

/// The exit codes in the form `ds down --help` shows them.
pub fn describe_down_exit_codes() -> String {
    format_codes(&[
        (SUCCESS, "the services are stopped"),
        (ERROR, "this checkout is not registered, or Compose failed"),
        (
            TOOL_FAILURE,
            "the tool itself failed (Docker unavailable, unreadable config or registry), or invalid usage",
        ),
    ])
}

/// The exit codes in the form `ds reset --help` shows them.
pub fn describe_reset_exit_codes() -> String {
    format_codes(&[
        (
            SUCCESS,
            "the data is deleted and the services are started again",
        ),
        (
            ERROR,
            "this checkout is not registered, the service is unknown or shares a volume with another service, or Compose failed",
        ),
        (
            TOOL_FAILURE,
            "the tool itself failed (Docker unavailable, unreadable config or registry), or invalid usage",
        ),
    ])
}

/// The exit codes in the form `ds rm --help` shows them.
pub fn describe_rm_exit_codes() -> String {
    format_codes(&[
        (
            SUCCESS,
            "the checkout is back to its default configuration and opted out",
        ),
        (
            ERROR,
            "nothing was changed: no terminal to ask in and no --yes, the answer was no, .env cannot be rewritten, or Compose failed",
        ),
        (
            TOOL_FAILURE,
            "the tool itself failed; when the data is already deleted the message says so, and running ds rm again finishes the job",
        ),
    ])
}

/// The exit codes in the form `ds prune --help` shows them.
pub fn describe_prune_exit_codes() -> String {
    format_codes(&[
        (
            SUCCESS,
            "nothing to prune, or every gone instance was cleaned and forgotten",
        ),
        (
            ERROR,
            "no terminal to ask in and no --yes, the answer was no, or at least one cleanup failed (that instance stays in the registry)",
        ),
        (
            TOOL_FAILURE,
            "the tool itself failed (unreadable config or registry, registry locked), or invalid usage",
        ),
    ])
}

/// The exit codes in the form `ds doctor --help` shows them.
pub fn describe_doctor_exit_codes() -> String {
    [
        (SUCCESS, "no error found (warnings are allowed)"),
        (ERROR, "at least one error was found"),
        (
            TOOL_FAILURE,
            "the tool itself failed (Docker or git unavailable, unreadable file, timeout), or invalid usage",
        ),
    ]
    .map(|(code, meaning)| format!("  {code}  {meaning}"))
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_list_every_code_with_its_meaning_when_describing_exit_codes() {
        let description = describe_exit_codes();

        assert!(
            [SUCCESS, ERROR, USAGE]
                .iter()
                .all(|code| description.contains(&format!("  {code}  ")))
        );
    }

    #[test]
    fn should_list_the_shell_codes_when_describing_run_exit_codes() {
        let description = describe_run_exit_codes();

        assert!(
            [
                SUCCESS,
                ERROR,
                TOOL_FAILURE,
                COMMAND_NOT_EXECUTABLE,
                COMMAND_NOT_FOUND
            ]
            .iter()
            .all(|code| description.contains(&format!("  {code}  ")))
        );
    }

    #[test]
    fn should_list_the_unregistered_code_when_describing_env_exit_codes() {
        assert!(describe_env_exit_codes().contains("  1    this checkout is not registered"));
    }

    #[test]
    fn should_list_success_for_an_unregistered_checkout_when_describing_info_exit_codes() {
        assert!(describe_info_exit_codes().contains("unregistered"));
    }

    #[test]
    fn should_list_the_shell_codes_when_describing_up_exit_codes() {
        let description = describe_up_exit_codes();

        assert!(
            [
                SUCCESS,
                ERROR,
                TOOL_FAILURE,
                COMMAND_NOT_EXECUTABLE,
                COMMAND_NOT_FOUND
            ]
            .iter()
            .all(|code| description.contains(&format!("  {code}  ")))
        );
    }

    #[test]
    fn should_list_the_three_codes_when_describing_the_other_new_commands() {
        let descriptions = [
            describe_ensure_exit_codes(),
            describe_down_exit_codes(),
            describe_reset_exit_codes(),
            describe_rm_exit_codes(),
            describe_prune_exit_codes(),
        ];

        assert!(descriptions.iter().all(|description| {
            [SUCCESS, ERROR, TOOL_FAILURE]
                .iter()
                .all(|code| description.contains(&format!("  {code}  ")))
        }));
    }

    #[test]
    fn should_list_every_code_with_its_meaning_when_describing_doctor_exit_codes() {
        let description = describe_doctor_exit_codes();

        assert!(
            [SUCCESS, ERROR, TOOL_FAILURE]
                .iter()
                .all(|code| description.contains(&format!("  {code}  ")))
        );
    }
}
