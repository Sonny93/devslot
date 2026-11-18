//! `ds down`: stop the processes and the services of this checkout's instance,
//! keeping its data.

use std::path::Path;

use crate::cli::exit_code;
use crate::commands::docker_step::checked_step;
use crate::commands::machine::{Machine, load_instance_context, locate_checkout};
use crate::commands::worktree_removal::scope_of;
use crate::compose::runner::{ComposeRunner, ComposeTarget, DockerRunner};
use crate::error::ToolError;
use crate::instance_processes::{InstanceScope, ProcessControl, SystemProcesses};

const NOT_REGISTERED_MESSAGE: &str = "ds: error: this checkout is not registered; nothing to stop";

/// Stops the processes, then the services, of the instance of the current checkout.
pub fn run_down() -> Result<u8, ToolError> {
    let checkout = locate_checkout()?;
    let machine = Machine::from_process()?;
    let registry = machine.store.read()?;
    let Some(instance) = registry.find(&checkout.root) else {
        eprintln!("{NOT_REGISTERED_MESSAGE}");
        return Ok(exit_code::ERROR);
    };
    let context = load_instance_context(&checkout, instance)?;
    stop_instance(
        &SystemProcesses,
        &DockerRunner::from_environment(),
        &checkout.root,
        &scope_of(instance),
        &context.target,
    )
}

/// Stops the processes of the instance, then its services (even when a
/// process survived); 0 when all stopped, 1 when a process survived or
/// Compose failed.
pub fn stop_instance(
    processes: &dyn ProcessControl,
    runner: &dyn ComposeRunner,
    root: &Path,
    scope: &InstanceScope,
    target: &ComposeTarget,
) -> Result<u8, ToolError> {
    let survivors_remain = stop_processes(processes, root, scope);
    let services_code = stop_services(runner, target)?;
    if survivors_remain {
        return Ok(exit_code::ERROR);
    }
    Ok(services_code)
}

/// Stops the processes of the instance and says what happened; true when a
/// targeted process is still running.
fn stop_processes(processes: &dyn ProcessControl, root: &Path, scope: &InstanceScope) -> bool {
    let stopped = processes.stop(root, Some(scope));
    for line in stopped.report.stopped_lines() {
        eprintln!("{line}");
    }
    for survivor in &stopped.survivors {
        eprintln!("ds: error: {survivor} is still running and could not be stopped");
    }
    if let Some(line) = stopped.report.still_running_line(root) {
        eprintln!("{line}");
    }
    !stopped.survivors.is_empty()
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
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

    use super::*;
    use crate::compose::runner::testing::FakeComposeRunner;
    use crate::instance_processes::{
        ProcessReport, ProcessSummary, Requester, SessionProcess, StopReport,
    };

    struct FakeProcesses<'runner> {
        runner: &'runner FakeComposeRunner,
        outcome: StopReport,
        compose_calls_when_stopped: Cell<Option<usize>>,
        scope_seen: RefCell<Option<InstanceScope>>,
    }

    impl<'runner> FakeProcesses<'runner> {
        fn stopping(runner: &'runner FakeComposeRunner, outcome: StopReport) -> Self {
            Self {
                runner,
                outcome,
                compose_calls_when_stopped: Cell::new(None),
                scope_seen: RefCell::new(None),
            }
        }
    }

    impl ProcessControl for FakeProcesses<'_> {
        fn find(&self, _worktree: &Path, _scope: Option<&InstanceScope>) -> ProcessReport {
            ProcessReport::default()
        }

        fn stop(&self, _worktree: &Path, scope: Option<&InstanceScope>) -> StopReport {
            self.compose_calls_when_stopped
                .set(Some(self.runner.calls().len()));
            *self.scope_seen.borrow_mut() = scope.cloned();
            self.outcome.clone()
        }

        fn foreign(
            &self,
            _worktree: &Path,
            _scope: Option<&InstanceScope>,
            _requester: Requester,
        ) -> Vec<ProcessSummary> {
            Vec::new()
        }

        fn is_session_alive(&self, _session: SessionProcess) -> bool {
            false
        }

        fn own_session(&self, _requester: Requester) -> Option<SessionProcess> {
            None
        }
    }

    fn summary(pid: i32, comm: &str) -> ProcessSummary {
        ProcessSummary {
            pid,
            comm: comm.to_owned(),
        }
    }

    fn scope() -> InstanceScope {
        InstanceScope {
            name: "app-3f2a".to_owned(),
            ports: vec![4100, 4101],
        }
    }

    fn stopped_server() -> StopReport {
        StopReport {
            report: ProcessReport {
                targets: vec![summary(42, "node")],
                others: vec![],
            },
            survivors: vec![],
        }
    }

    fn surviving_server() -> StopReport {
        StopReport {
            report: ProcessReport {
                targets: vec![],
                others: vec![summary(42, "node")],
            },
            survivors: vec![summary(42, "node")],
        }
    }

    fn target(files: Vec<PathBuf>) -> ComposeTarget {
        ComposeTarget {
            project_name: "app-3f2a".to_owned(),
            files,
            working_directory: PathBuf::from("/work/app"),
            environment: vec![],
        }
    }

    fn down(processes: &FakeProcesses, runner: &FakeComposeRunner) -> u8 {
        stop_instance(
            processes,
            runner,
            Path::new("/work/app"),
            &scope(),
            &target(vec![]),
        )
        .unwrap()
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

    #[test]
    fn should_stop_the_processes_with_the_instance_scope_before_compose_down() {
        let runner = FakeComposeRunner::default();
        let processes = FakeProcesses::stopping(&runner, stopped_server());

        let code = down(&processes, &runner);

        assert_eq!(code, 0);
        assert_eq!(processes.compose_calls_when_stopped.get(), Some(0));
        assert_eq!(*processes.scope_seen.borrow(), Some(scope()));
        assert_eq!(runner.calls().len(), 1);
        assert_eq!(runner.calls()[0].command, "down");
    }

    #[test]
    fn should_exit_1_and_still_call_compose_down_when_a_process_survives() {
        let runner = FakeComposeRunner::default();
        let processes = FakeProcesses::stopping(&runner, surviving_server());

        let code = down(&processes, &runner);

        assert_eq!(code, 1);
        assert_eq!(runner.calls().len(), 1);
        assert_eq!(runner.calls()[0].command, "down");
    }

    #[test]
    fn should_exit_1_when_compose_fails_after_the_processes_stopped() {
        let runner = FakeComposeRunner::default().with_failing_commands(vec!["down"]);
        let processes = FakeProcesses::stopping(&runner, stopped_server());

        assert_eq!(down(&processes, &runner), 1);
    }

    #[test]
    fn should_exit_0_when_there_is_no_process_to_stop() {
        let runner = FakeComposeRunner::default();
        let processes = FakeProcesses::stopping(&runner, StopReport::default());

        assert_eq!(down(&processes, &runner), 0);
    }
}
