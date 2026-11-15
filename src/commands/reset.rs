//! `ds reset [service]`: delete the data of this checkout's instance, then
//! start the services again.

use crate::analysis::{CheckoutAnalysis, ProbeOutcome, ProbeScope, rejection_finding};
use crate::checkout::Checkout;
use crate::cli::exit_code;
use crate::commands::docker_step::checked_step;
use crate::commands::isolation_gate::is_model_isolated;
use crate::commands::machine::{Machine, load_instance_context, locate_checkout};
use crate::compose::model::ComposeConfig;
use crate::compose::runner::{ComposeRunner, ComposeTarget, DockerRunner, delete_project_data};
use crate::docker::DockerCompose;
use crate::error::ToolError;
use crate::reset_plan::{ServiceReset, plan_service_reset};

const NOT_REGISTERED_MESSAGE: &str = "ds: error: this checkout is not registered; nothing to reset";

/// Resets the data of the whole instance, or of one service.
pub fn run_reset(service: Option<&str>) -> Result<u8, ToolError> {
    let checkout = locate_checkout()?;
    let machine = Machine::from_process()?;
    let registry = machine.store.read()?;
    let Some(instance) = registry.find(&checkout.root) else {
        eprintln!("{NOT_REGISTERED_MESSAGE}");
        return Ok(exit_code::ERROR);
    };
    let context = load_instance_context(&checkout, instance)?;
    eprintln!(
        "ds: resetting the data of {} ({})",
        instance.name,
        instance.path.display()
    );
    let runner = DockerRunner::from_environment();
    match service {
        None => reset_everything(&runner, &context.target, context.has_compose_file),
        Some(service) => reset_one_service(&runner, &checkout, &context.target, service),
    }
}

/// Deletes the containers, volumes and networks of the project by label, then
/// `up` when the checkout has a compose file. Nothing is deleted when the
/// resolved model is not isolated.
pub fn reset_everything(
    runner: &dyn ComposeRunner,
    target: &ComposeTarget,
    has_compose_file: bool,
) -> Result<u8, ToolError> {
    let name = &target.project_name;
    if has_compose_file && !is_model_isolated(runner, target)? {
        return Ok(exit_code::ERROR);
    }
    let delete_message = format!("ds: error: the data of {name} was not deleted");
    if checked_step(delete_project_data(runner, name), &delete_message)?.is_none() {
        return Ok(exit_code::ERROR);
    }
    if !has_compose_file {
        return Ok(exit_code::SUCCESS);
    }
    let up_message = format!(
        "ds: error: the services of {name} did not start again; see the Compose output above"
    );
    if checked_step(runner.up(target, &[]), &up_message)?.is_none() {
        return Ok(exit_code::ERROR);
    }
    Ok(exit_code::SUCCESS)
}

fn reset_one_service(
    runner: &dyn ComposeRunner,
    checkout: &Checkout,
    target: &ComposeTarget,
    service: &str,
) -> Result<u8, ToolError> {
    let Some(model) = read_dev_model(checkout)? else {
        return Ok(exit_code::ERROR);
    };
    let plan = match plan_service_reset(&model, service) {
        Ok(plan) => plan,
        Err(refusal) => {
            eprintln!("ds: error: {refusal}");
            return Ok(exit_code::ERROR);
        }
    };
    reset_service(runner, target, service, &plan)
}

fn read_dev_model(checkout: &Checkout) -> Result<Option<ComposeConfig>, ToolError> {
    let compose = DockerCompose::from_environment(&checkout.root);
    let analysis = CheckoutAnalysis::analyze(checkout, &compose, ProbeScope::DevComposeOnly)?;
    let Some(dev_file) = analysis.dev_file().map(str::to_owned) else {
        eprintln!("ds: error: no compose file: there is no service to reset");
        return Ok(None);
    };
    let probed = analysis
        .probed_files
        .into_iter()
        .find(|file| file.name == dev_file);
    if let Some(ProbeOutcome::Read(probed)) = probed.map(|file| file.outcome) {
        return Ok(Some(probed.config));
    }
    let finding = rejection_finding(&dev_file, Some(&dev_file));
    eprintln!("ds: error: {}", finding.message);
    Ok(None)
}

/// Removes the container, anonymous volumes and named volumes of one service,
/// then starts it again.
pub fn reset_service(
    runner: &dyn ComposeRunner,
    target: &ComposeTarget,
    service: &str,
    plan: &ServiceReset,
) -> Result<u8, ToolError> {
    if !is_model_isolated(runner, target)? {
        return Ok(exit_code::ERROR);
    }
    if plan.has_bind_mounts {
        eprintln!("ds: warning: bind mounts of {service} are not reset");
    }
    let remove_message = format!(
        "ds: error: the container of {service} was not removed; see the Compose output above"
    );
    if checked_step(runner.remove_service(target, service), &remove_message)?.is_none() {
        return Ok(exit_code::ERROR);
    }
    let Some(names) = find_volume_names(runner, target, plan)? else {
        return Ok(exit_code::ERROR);
    };
    let volumes_message = format!("ds: error: the volumes of {service} were not deleted");
    if checked_step(runner.remove_volumes(&names), &volumes_message)?.is_none() {
        return Ok(exit_code::ERROR);
    }
    let up_message =
        format!("ds: error: {service} did not start again; see the Compose output above");
    let services = [service.to_owned()];
    if checked_step(runner.up(target, &services), &up_message)?.is_none() {
        return Ok(exit_code::ERROR);
    }
    Ok(exit_code::SUCCESS)
}

fn find_volume_names(
    runner: &dyn ComposeRunner,
    target: &ComposeTarget,
    plan: &ServiceReset,
) -> Result<Option<Vec<String>>, ToolError> {
    let mut names = Vec::new();
    for key in &plan.volume_keys {
        let message = format!("ds: error: the volumes of {key} could not be listed");
        let Some(found) = checked_step(
            runner.project_volumes(&target.project_name, Some(key)),
            &message,
        )?
        else {
            return Ok(None);
        };
        names.extend(found);
    }
    Ok(Some(names))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::compose::runner::testing::FakeComposeRunner;

    fn target() -> ComposeTarget {
        ComposeTarget {
            project_name: "app-3f2a".to_owned(),
            files: vec![PathBuf::from("/work/app/dev.compose.yml")],
            working_directory: PathBuf::from("/work/app"),
            environment: vec![],
        }
    }

    fn commands(runner: &FakeComposeRunner) -> Vec<&'static str> {
        runner.calls().iter().map(|call| call.command).collect()
    }

    fn postgres_plan() -> ServiceReset {
        ServiceReset {
            volume_keys: vec!["postgres-volume".to_owned()],
            has_bind_mounts: false,
        }
    }

    #[test]
    fn should_delete_the_volumes_then_start_the_services_when_resetting_everything() {
        let runner = FakeComposeRunner::default();

        let code = reset_everything(&runner, &target(), true).unwrap();

        assert_eq!(code, 0);
        assert_eq!(
            commands(&runner),
            ["config", "docker ps", "volume ls", "network ls", "up"]
        );
    }

    #[test]
    fn should_not_start_the_services_when_there_is_no_compose_file() {
        let runner = FakeComposeRunner::default();

        reset_everything(&runner, &target(), false).unwrap();

        assert_eq!(commands(&runner), ["docker ps", "volume ls", "network ls"]);
    }

    #[test]
    fn should_stop_and_exit_1_when_deleting_the_volumes_fails() {
        let runner = FakeComposeRunner::default().with_failing_commands(vec!["volume ls"]);

        let code = reset_everything(&runner, &target(), true).unwrap();

        assert_eq!(code, 1);
        assert_eq!(commands(&runner), ["config", "docker ps", "volume ls"]);
    }

    #[test]
    fn should_remove_the_container_then_the_volumes_then_start_only_the_service() {
        let runner =
            FakeComposeRunner::default().with_volumes(vec!["app-3f2a_postgres-volume".to_owned()]);

        let code = reset_service(&runner, &target(), "postgres", &postgres_plan()).unwrap();

        let calls = runner.calls();
        assert_eq!(code, 0);
        assert_eq!(
            calls.iter().map(|call| call.command).collect::<Vec<_>>(),
            ["config", "rm", "volume ls", "volume rm", "up"]
        );
        assert_eq!(calls[2].services, ["postgres-volume"]);
        assert_eq!(calls[3].services, ["app-3f2a_postgres-volume"]);
        assert_eq!(calls[4].services, ["postgres"]);
    }

    #[test]
    fn should_not_call_volume_rm_when_no_volume_carries_the_label() {
        let runner = FakeComposeRunner::default();

        reset_service(&runner, &target(), "postgres", &postgres_plan()).unwrap();

        assert_eq!(commands(&runner), ["config", "rm", "volume ls", "up"]);
    }

    #[test]
    fn should_delete_nothing_when_the_resolved_model_is_not_isolated() {
        let runner = FakeComposeRunner::default()
            .with_model(r#"{"volumes":{"data":{"name":"victim_pgdata"}}}"#);

        let everything = reset_everything(&runner, &target(), true).unwrap();
        let one = reset_service(&runner, &target(), "postgres", &postgres_plan()).unwrap();

        assert_eq!((everything, one), (1, 1));
        assert_eq!(commands(&runner), ["config", "config"]);
    }

    #[test]
    fn should_not_start_the_service_when_removing_its_container_fails() {
        let runner = FakeComposeRunner::default().with_failing_commands(vec!["rm"]);

        let code = reset_service(&runner, &target(), "postgres", &postgres_plan()).unwrap();

        assert_eq!(code, 1);
        assert_eq!(commands(&runner), ["config", "rm"]);
    }
}
