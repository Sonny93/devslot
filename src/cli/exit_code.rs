//! The exit codes of the `ds` command, as documented in `--help`.

/// Everything went well.
pub const SUCCESS: u8 = 0;
/// `doctor`: at least one error was found in the project.
pub const ERROR: u8 = 1;
/// The command line is invalid. Returned by the argument parser itself.
pub const USAGE: u8 = 2;
/// The tool itself failed: Docker or git unavailable, unreadable file, timeout.
pub const TOOL_FAILURE: u8 = 2;

/// The exit codes in the form the top-level `--help` shows them.
pub fn describe_exit_codes() -> String {
    [
        (SUCCESS, "success"),
        (ERROR, "error (for doctor: at least one error was found)"),
        (USAGE, "invalid usage, or the tool itself failed"),
    ]
    .map(|(code, meaning)| format!("  {code}  {meaning}"))
    .join("\n")
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
    fn should_list_every_code_with_its_meaning_when_describing_doctor_exit_codes() {
        let description = describe_doctor_exit_codes();

        assert!(
            [SUCCESS, ERROR, TOOL_FAILURE]
                .iter()
                .all(|code| description.contains(&format!("  {code}  ")))
        );
    }
}
