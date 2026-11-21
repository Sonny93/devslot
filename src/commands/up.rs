//! `ds up`: register the checkout if needed, start the compose services, then
//! run the `run` command of `devslot.toml` in the foreground when there is one,
//! unless `--no-run` asks for the services only.

use crate::allocation::PortProbe;
use crate::cli::args::UpArguments;
use crate::cli::exit_code;
use crate::cli::style::{ColorChoice, Style};
use crate::commands::docker_step::checked_step;
use crate::commands::isolation_gate::is_model_isolated;
use crate::commands::launch::{exec_shell_script, warn_about_replacements};
use crate::commands::machine::{InstanceContext, Machine, load_instance_context, locate_checkout};
use crate::commands::registering::{announce, register_checkout, registration_or_exit_code};
use crate::compose::runner::{ComposeRunner, ComposeTarget, DockerRunner};
use crate::effective_port::effective_ports;
use crate::error::ToolError;
use crate::global_config::GlobalConfig;
use crate::port_check::busy_ports;
use crate::port_probe::SystemPortProbe;
use crate::project_config::EnvSetting;
use crate::registration::Intent;
use crate::registry::record::InstanceRecord;
use crate::session_leases;

const REFUSAL_CLOSING_LINE: &str = "ds up does not start the services until the errors are fixed";
const DEFAULT_RUN_ORIGIN: &str = "devslot.toml";

/// How starting the services ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpOutcome {
    /// Compose started the services and they are healthy.
    Started,
    /// The checkout has no compose file: there was nothing to start.
    NoComposeFile,
    /// The resolved model is not isolated per instance; nothing was started.
    NotIsolated,
    /// A port of the instance is used by another program; nothing was started.
    PortsBusy,
    /// Compose ran and failed.
    Failed,
}

/// What starting the services needs to know about the instance.
pub struct UpPlan<'a> {
    /// The registered instance.
    pub instance: &'a InstanceRecord,
    /// The compose project to start.
    pub target: &'a ComposeTarget,
    /// Whether the checkout has a dev compose file.
    pub has_compose_file: bool,
    /// Whether an app is run after the services, so its web ports matter too.
    pub runs_app: bool,
    /// The `[env]` settings, some of which may force a port.
    pub env_settings: &'a [EnvSetting],
}

/// Registers the checkout, starts its services, then runs the app.
pub fn run_up(color: ColorChoice, arguments: &UpArguments) -> Result<u8, ToolError> {
    let checkout = locate_checkout()?;
    let machine = Machine::from_process()?;
    let global = GlobalConfig::load(&machine.paths.config_file)?;
    let outcome = register_checkout(&checkout, &machine, &global, Intent::Explicit)?;
    let style = Style::for_stderr(color);
    let registration =
        match registration_or_exit_code(outcome, &machine, style, REFUSAL_CLOSING_LINE) {
            Ok(registration) => registration,
            Err(code) => return Ok(code),
        };
    announce(&registration, style);
    session_leases::record_current_session(machine.paths.state_directory(), &checkout);
    let context = load_instance_context(&checkout, &registration.instance)?;
    let plan = UpPlan {
        instance: &registration.instance,
        target: &context.target,
        has_compose_file: context.has_compose_file,
        runs_app: runs_app(&context, arguments),
        env_settings: &context.config.env,
    };
    let outcome = start_services(&DockerRunner::from_environment(), &SystemPortProbe, &plan)?;
    if matches!(
        outcome,
        UpOutcome::PortsBusy | UpOutcome::Failed | UpOutcome::NotIsolated
    ) {
        return Ok(exit_code::ERROR);
    }
    Ok(finish(&registration.instance, &context, outcome, arguments))
}

fn runs_app(context: &InstanceContext, arguments: &UpArguments) -> bool {
    !arguments.no_run && context.config.run.is_some()
}

fn finish(
    instance: &InstanceRecord,
    context: &InstanceContext,
    outcome: UpOutcome,
    arguments: &UpArguments,
) -> u8 {
    let run = context.config.run.as_deref().filter(|_| !arguments.no_run);
    let Some(run) = run else {
        if outcome == UpOutcome::Started {
            eprintln!("ds: the services of {} are up", instance.name);
        }
        return exit_code::SUCCESS;
    };
    let origin = context.config.run_origin.unwrap_or(DEFAULT_RUN_ORIGIN);
    eprintln!("ds: running {run} (from {origin})");
    warn_about_replacements(&context.environment);
    exec_shell_script(run, &context.environment)
}

/// Checks the ports, then starts the services of the instance.
pub fn start_services(
    runner: &dyn ComposeRunner,
    probe: &dyn PortProbe,
    plan: &UpPlan<'_>,
) -> Result<UpOutcome, ToolError> {
    if !plan.has_compose_file {
        eprintln!("ds: no compose file: no service to start");
        return Ok(UpOutcome::NoComposeFile);
    }
    if !is_model_isolated(runner, plan.target)? {
        return Ok(UpOutcome::NotIsolated);
    }
    let own_ports = own_published_ports(runner, plan.target);
    let ports = effective_ports(plan.instance, plan.env_settings);
    let busy = busy_ports(&ports, plan.runs_app, &own_ports, probe);
    if !busy.is_empty() {
        for port in &busy {
            eprintln!("{}", port.message());
        }
        return Ok(UpOutcome::PortsBusy);
    }
    let message = format!(
        "ds: error: the services of {} did not start or did not become healthy; see the Compose output above",
        plan.instance.name
    );
    match checked_step(runner.up(plan.target, &[]), &message)? {
        Some(()) => Ok(UpOutcome::Started),
        None => Ok(UpOutcome::Failed),
    }
}

fn own_published_ports(runner: &dyn ComposeRunner, target: &ComposeTarget) -> Vec<u16> {
    runner.containers(target).map_or_default(|containers| {
        containers
            .into_iter()
            .flat_map(|container| container.published_ports)
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::compose::runner::ContainerState;
    use crate::compose::runner::testing::FakeComposeRunner;
    use crate::discovery::PortKind;
    use crate::port_range::PortRange;
    use crate::registry::record::{AssignedPort, PortDetail};

    struct BusyAt(Vec<u16>);

    impl PortProbe for BusyAt {
        fn is_free(&self, port: u16) -> bool {
            !self.0.contains(&port)
        }
    }

    fn instance() -> InstanceRecord {
        InstanceRecord {
            path: PathBuf::from("/work/app"),
            name: "app-3f2a".to_owned(),
            slot: 1,
            range: PortRange {
                first: 20050,
                last: 20099,
            },
            ports: vec![
                AssignedPort {
                    variable: "PORT".to_owned(),
                    port: 20050,
                    detail: PortDetail {
                        kind: PortKind::Web,
                        service: None,
                        target: None,
                    },
                },
                AssignedPort {
                    variable: "DB_PORT".to_owned(),
                    port: 20051,
                    detail: PortDetail {
                        kind: PortKind::Service,
                        service: Some("postgres".to_owned()),
                        target: Some(5432),
                    },
                },
            ],
            host: "app.localhost".to_owned(),
            web_port_variable: None,
            is_host_unsaved: false,
        }
    }

    fn target() -> ComposeTarget {
        ComposeTarget {
            project_name: "app-3f2a".to_owned(),
            files: vec![PathBuf::from("/work/app/dev.compose.yml")],
            working_directory: PathBuf::from("/work/app"),
            environment: vec![],
        }
    }

    fn start(
        runner: &FakeComposeRunner,
        busy: Vec<u16>,
        has_compose_file: bool,
        runs_app: bool,
    ) -> UpOutcome {
        let instance = instance();
        let target = target();
        start_services(
            runner,
            &BusyAt(busy),
            &UpPlan {
                instance: &instance,
                target: &target,
                has_compose_file,
                runs_app,
                env_settings: &[],
            },
        )
        .unwrap()
    }

    fn commands(runner: &FakeComposeRunner) -> Vec<&'static str> {
        runner.calls().iter().map(|call| call.command).collect()
    }

    #[test]
    fn should_start_the_services_after_looking_at_the_own_containers_when_every_port_is_free() {
        let runner = FakeComposeRunner::default();

        let outcome = start(&runner, vec![], true, false);

        assert_eq!(outcome, UpOutcome::Started);
        assert_eq!(commands(&runner), ["config", "ps", "up"]);
    }

    #[test]
    fn should_not_start_anything_when_a_service_port_is_used_by_another_program() {
        let runner = FakeComposeRunner::default();

        let outcome = start(&runner, vec![20051], true, false);

        assert_eq!(outcome, UpOutcome::PortsBusy);
        assert_eq!(commands(&runner), ["config", "ps"]);
    }

    #[test]
    fn should_ignore_a_busy_web_port_when_no_app_runs_after_the_services() {
        let runner = FakeComposeRunner::default();

        assert_eq!(start(&runner, vec![20050], true, false), UpOutcome::Started);
    }

    #[test]
    fn should_refuse_a_busy_web_port_when_an_app_runs_after_the_services() {
        let runner = FakeComposeRunner::default();

        assert_eq!(
            start(&runner, vec![20050], true, true),
            UpOutcome::PortsBusy
        );
    }

    #[test]
    fn should_accept_a_busy_port_that_a_container_of_the_instance_publishes() {
        let runner = FakeComposeRunner::default().with_containers(vec![ContainerState {
            service: "postgres".to_owned(),
            state: "running".to_owned(),
            published_ports: vec![20051],
        }]);

        assert_eq!(start(&runner, vec![20051], true, false), UpOutcome::Started);
    }

    #[test]
    fn should_check_the_ports_with_an_empty_set_when_listing_the_containers_fails() {
        let runner = FakeComposeRunner::default().with_failing_commands(vec!["ps"]);

        assert_eq!(
            start(&runner, vec![20051], true, false),
            UpOutcome::PortsBusy
        );
    }

    #[test]
    fn should_report_a_failure_when_compose_up_fails() {
        let runner = FakeComposeRunner::default().with_failing_commands(vec!["up"]);

        assert_eq!(start(&runner, vec![], true, false), UpOutcome::Failed);
    }

    #[test]
    fn should_start_nothing_when_the_resolved_model_is_not_isolated() {
        let runner = FakeComposeRunner::default()
            .with_model(r#"{"volumes":{"data":{"name":"victim_pgdata"}}}"#);

        let outcome = start(&runner, vec![], true, false);

        assert_eq!(outcome, UpOutcome::NotIsolated);
        assert_eq!(commands(&runner), ["config"]);
    }

    #[test]
    fn should_make_no_docker_call_when_there_is_no_compose_file() {
        let runner = FakeComposeRunner::default();

        let outcome = start(&runner, vec![20051], false, true);

        assert_eq!(outcome, UpOutcome::NoComposeFile);
        assert_eq!(runner.calls(), vec![]);
    }
}
