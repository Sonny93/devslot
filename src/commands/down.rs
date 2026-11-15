//! `ds down`: stop the services of this checkout's instance, keeping its data.

use crate::cli::exit_code;
use crate::commands::docker_step::checked_step;
use crate::commands::machine::{Machine, load_instance_context, locate_checkout};
use crate::compose::runner::{ComposeRunner, ComposeTarget, DockerRunner};
use crate::error::ToolError;

const NOT_REGISTERED_MESSAGE: &str = "ds: error: this checkout is not registered; nothing to stop";

/// Stops the services of the instance of the current checkout.
pub fn run_down() -> Result<u8, ToolError> {
    let checkout = locate_checkout()?;
    let machine = Machine::from_process()?;
    let registry = machine.store.read()?;
    let Some(instance) = registry.find(&checkout.root) else {
        eprintln!("{NOT_REGISTERED_MESSAGE}");
        return Ok(exit_code::ERROR);
    };
    let context = load_instance_context(&checkout, instance)?;
    stop_services(&DockerRunner::from_environment(), &context.target)
}

/// `down` on the target; 0 when it worked, 1 when Compose failed.
pub fn stop_services(runner: &dyn ComposeRunner, target: &ComposeTarget) -> Result<u8, ToolError> {
    let message = format!(
        "ds: error: the services of {} did not stop; see the Compose output above",
        target.project_name
    );
    if checked_step(runner.down(target), &message)?.is_none() {
        return Ok(exit_code::ERROR);
    }
    eprintln!("ds: the services of {} are stopped", target.project_name);
    Ok(exit_code::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::compose::runner::testing::FakeComposeRunner;

    fn target(files: Vec<PathBuf>) -> ComposeTarget {
        ComposeTarget {
            project_name: "app-3f2a".to_owned(),
            files,
            working_directory: PathBuf::from("/work/app"),
            environment: vec![],
        }
    }

    #[test]
    fn should_exit_0_and_call_down_once_when_compose_succeeds() {
        let runner = FakeComposeRunner::default();

        let code = stop_services(&runner, &target(vec![])).unwrap();

        assert_eq!(code, 0);
        assert_eq!(runner.calls().len(), 1);
        assert_eq!(runner.calls()[0].command, "down");
    }

    #[test]
    fn should_exit_1_when_compose_fails() {
        let runner = FakeComposeRunner::default().with_failing_commands(vec!["down"]);

        assert_eq!(stop_services(&runner, &target(vec![])).unwrap(), 1);
    }

    #[test]
    fn should_pass_the_compose_files_of_the_target() {
        let runner = FakeComposeRunner::default();
        let files = vec![PathBuf::from("/work/app/dev.compose.yml")];

        stop_services(&runner, &target(files.clone())).unwrap();

        assert_eq!(runner.calls()[0].files, files);
    }
}
