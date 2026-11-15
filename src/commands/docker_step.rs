//! Running one Docker command of an instance: a failure of the command itself
//! is a refusal with a message, anything else is a tool failure.

use crate::compose::runner::RunnerError;
use crate::error::ToolError;

/// The value when `result` succeeded. When Docker ran and failed, prints
/// `failure_message` and gives `None`; any other failure is an error.
pub fn checked_step<T>(
    result: Result<T, RunnerError>,
    failure_message: &str,
) -> Result<Option<T>, ToolError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(RunnerError::Failed { .. }) => {
            eprintln!("{failure_message}");
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_give_the_value_when_the_step_succeeded() {
        assert_eq!(checked_step(Ok(3), "failed").unwrap(), Some(3));
    }

    #[test]
    fn should_give_nothing_when_docker_ran_and_failed() {
        let failed = Err(RunnerError::Failed {
            command: "up",
            code: Some(1),
        });

        assert_eq!(checked_step::<()>(failed, "failed").unwrap(), None);
    }

    #[test]
    fn should_fail_the_tool_when_docker_cannot_run() {
        let unavailable = Err(RunnerError::Unavailable {
            program: "docker".to_owned(),
            reason: "not found".to_owned(),
        });

        assert!(matches!(
            checked_step::<()>(unavailable, "failed"),
            Err(ToolError::Runner(_))
        ));
    }
}
