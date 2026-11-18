//! `ds info`: the name, slot and ports of the instance of this checkout.

use crate::checkout::Checkout;
use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::cli::render::render_info_text;
use crate::commands::machine::{Machine, locate_checkout};
use crate::compose::runner::{ComposeRunner, ComposeTarget, DockerRunner};
use crate::compose::selection::{DevComposeSelection, select_dev_compose};
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
    describe_registered(checkout, instance, &config, services)
}

/// What is known about a registered checkout, with `services` as its state.
pub fn describe_registered(
    checkout: &Checkout,
    instance: &InstanceRecord,
    config: &ProjectConfig,
    services: ServicesState,
) -> Result<RegisteredInfo, ToolError> {
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
        host: instance.host.clone(),
        app_url: instance
            .app_port()
            .and_then(|assigned| instance.url_of(assigned)),
        range: [instance.range.first, instance.range.last],
        compose_file,
        compose_file_origin,
        ports: instance
            .ports
            .iter()
            .map(|assigned| info_port(instance, assigned))
            .collect(),
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

/// The port of `instance` in the shape both `ds info` and `ds ls` print.
pub fn info_port(instance: &InstanceRecord, assigned: &AssignedPort) -> InfoPort {
    InfoPort {
        variable: assigned.variable.clone(),
        port: assigned.port,
        kind: assigned.detail.kind,
        service: assigned.detail.service.clone(),
        target: assigned.detail.target,
        url: instance.url_of(assigned),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::discovery::PortKind;
    use crate::port_range::PortRange;
    use crate::registry::record::PortDetail;

    fn assigned(variable: &str, port: u16, kind: PortKind) -> AssignedPort {
        AssignedPort {
            variable: variable.to_owned(),
            port,
            detail: PortDetail {
                kind,
                service: None,
                target: None,
            },
        }
    }

    fn instance(web_port_variable: Option<&str>) -> InstanceRecord {
        InstanceRecord {
            path: PathBuf::from("/work/app"),
            name: "app-3f2a".to_owned(),
            slot: 1,
            range: PortRange {
                first: 20050,
                last: 20099,
            },
            ports: vec![
                assigned("PORT", 20050, PortKind::Web),
                assigned("ADMIN_PORT", 20051, PortKind::Web),
                assigned("DB_PORT", 20052, PortKind::Service),
            ],
            host: "app-3f2a.localhost".to_owned(),
            web_port_variable: web_port_variable.map(str::to_owned),
            is_host_unsaved: false,
        }
    }

    fn url_of(instance: &InstanceRecord, variable: &str) -> Option<String> {
        let assigned = instance
            .ports
            .iter()
            .find(|assigned| assigned.variable == variable)
            .unwrap();
        info_port(instance, assigned).url
    }

    #[test]
    fn should_give_a_web_port_its_localhost_url_without_a_web_port_setting() {
        assert_eq!(
            url_of(&instance(None), "PORT").as_deref(),
            Some("http://localhost:20050")
        );
    }

    #[test]
    fn should_give_a_service_port_no_url() {
        assert_eq!(url_of(&instance(None), "DB_PORT"), None);
    }

    #[test]
    fn should_put_the_named_web_port_on_the_host_and_keep_the_others_on_localhost() {
        let instance = instance(Some("PORT"));

        assert_eq!(
            (
                url_of(&instance, "PORT").as_deref(),
                url_of(&instance, "ADMIN_PORT").as_deref()
            ),
            (
                Some("http://app-3f2a.localhost:20050"),
                Some("http://localhost:20051")
            )
        );
    }

    #[test]
    fn should_keep_every_url_on_localhost_when_the_named_variable_is_not_a_port_of_the_instance() {
        let instance = instance(Some("NOPE_PORT"));

        assert_eq!(
            url_of(&instance, "PORT").as_deref(),
            Some("http://localhost:20050")
        );
    }

    #[test]
    fn should_keep_every_url_on_localhost_when_the_named_variable_is_a_service_port() {
        let instance = instance(Some("DB_PORT"));

        assert_eq!(
            url_of(&instance, "PORT").as_deref(),
            Some("http://localhost:20050")
        );
    }

    #[test]
    fn should_give_the_app_url_in_the_report_only_with_a_named_web_port() {
        let checkout_root = tempfile::tempdir().unwrap();
        let checkout = Checkout::locate(checkout_root.path()).unwrap();
        let config = ProjectConfig::default();
        let describe = |instance: &InstanceRecord| {
            describe_registered(&checkout, instance, &config, ServicesState::Unknown)
                .unwrap()
                .app_url
        };

        assert_eq!(
            (describe(&instance(Some("PORT"))), describe(&instance(None))),
            (Some("http://app-3f2a.localhost:20050".to_owned()), None)
        );
    }
}
