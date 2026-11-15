//! `ds info`: the name, slot and ports of the instance of this checkout.

use crate::checkout::Checkout;
use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::cli::render::render_info_text;
use crate::commands::machine::{Machine, locate_checkout};
use crate::compose::runner::{ComposeRunner, ComposeTarget, DockerRunner};
use crate::compose::selection::{DevComposeSelection, select_dev_compose};
use crate::discovery::PortKind;
use crate::environment::{build_environment, compose_file_paths};
use crate::error::ToolError;
use crate::project_config::ProjectConfig;
use crate::registry::record::{AssignedPort, InstanceRecord};
use crate::report::{InfoPort, InfoReport, REPORT_VERSION, RegisteredInfo, UnregisteredInfo};
use crate::services_state::ServicesState;

const DETECTED_ORIGIN: &str = "detected";

/// Prints what is known about the instance, or that there is none.
pub fn run_info(as_json: bool) -> Result<u8, ToolError> {
    let checkout = locate_checkout()?;
    let machine = Machine::from_process()?;
    let registry = machine.store.read()?;
    let report = match registry.find(&checkout.root) {
        Some(instance) => InfoReport::Registered(registered_info(
            &checkout,
            instance,
            &DockerRunner::from_environment(),
        )?),
        None => InfoReport::Unregistered(UnregisteredInfo {
            version: REPORT_VERSION,
            registered: false,
            path: checkout.root.display().to_string(),
            opted_out: registry.is_opted_out(&checkout.root),
        }),
    };
    write_stdout(&render(&report, as_json)?)?;
    Ok(exit_code::SUCCESS)
}

fn render(report: &InfoReport, as_json: bool) -> Result<String, ToolError> {
    if !as_json {
        return Ok(render_info_text(report));
    }
    let mut json = serde_json::to_string_pretty(report)
        .map_err(|error| ToolError::WriteReport(std::io::Error::other(error)))?;
    json.push('\n');
    Ok(json)
}

fn registered_info(
    checkout: &Checkout,
    instance: &InstanceRecord,
    runner: &dyn ComposeRunner,
) -> Result<RegisteredInfo, ToolError> {
    let config = ProjectConfig::load(&checkout.root)?;
    let services =
        ServicesState::of(&runner.containers(&status_target(checkout, instance, &config)?));
    let selection = select_dev_compose(&checkout.root, config.compose_file.as_deref());
    let (compose_file, compose_file_origin) = match selection {
        DevComposeSelection::Selected(file) => {
            let origin = config.compose_file_origin.unwrap_or(DETECTED_ORIGIN);
            (Some(file), Some(origin.to_owned()))
        }
        DevComposeSelection::ConfiguredMissing(_) | DevComposeSelection::NoneFound => (None, None),
    };
    Ok(RegisteredInfo {
        version: REPORT_VERSION,
        registered: true,
        name: instance.name.clone(),
        path: instance.path.display().to_string(),
        slot: instance.slot,
        range: [instance.range.first, instance.range.last],
        compose_file,
        compose_file_origin,
        ports: instance.ports.iter().map(info_port).collect(),
        services,
    })
}

/// The target of `ps`: the project and the compose files, with the ports and
/// ids of the instance but none of the `[env]` values, so a bad `[env]` never
/// stops `ds info` (`ds run` and `ds doctor` report it).
fn status_target(
    checkout: &Checkout,
    instance: &InstanceRecord,
    config: &ProjectConfig,
) -> Result<ComposeTarget, ToolError> {
    let selection = select_dev_compose(&checkout.root, config.compose_file.as_deref());
    let files = compose_file_paths(&checkout.root, &selection);
    let environment = build_environment(instance, files.as_deref(), &[])?;
    Ok(ComposeTarget {
        project_name: instance.name.clone(),
        files: files.unwrap_or_default(),
        working_directory: checkout.root.clone(),
        environment: environment
            .variables
            .into_iter()
            .map(|variable| (variable.name, variable.value))
            .collect(),
    })
}

fn info_port(assigned: &AssignedPort) -> InfoPort {
    InfoPort {
        variable: assigned.variable.clone(),
        port: assigned.port,
        kind: assigned.detail.kind,
        service: assigned.detail.service.clone(),
        target: assigned.detail.target,
        url: (assigned.detail.kind == PortKind::Web).then(|| InfoPort::url_of(assigned.port)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::record::PortDetail;

    fn assigned(kind: PortKind) -> AssignedPort {
        AssignedPort {
            variable: "PORT".to_owned(),
            port: 20050,
            detail: PortDetail {
                kind,
                service: None,
                target: None,
            },
        }
    }

    #[test]
    fn should_give_a_web_port_its_url() {
        assert_eq!(
            info_port(&assigned(PortKind::Web)).url.as_deref(),
            Some("http://localhost:20050")
        );
    }

    #[test]
    fn should_give_a_service_port_no_url() {
        assert_eq!(info_port(&assigned(PortKind::Service)).url, None);
    }
}
