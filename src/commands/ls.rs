//! `ds ls`: every instance of the machine, from the registry, with whether
//! its services run. One Docker call for the whole machine, no lock.

use std::collections::BTreeSet;

use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::cli::render::{render_kept_worktrees, render_list_text};
use crate::commands::info::info_port;
use crate::commands::machine::Machine;
use crate::compose::runner::{ComposeRunner, DockerRunner};
use crate::error::ToolError;
use crate::kept_worktrees;
use crate::registry::record::Registry;
use crate::report::{ListReport, ListedInstance, REPORT_VERSION};

/// Prints the instances of the machine.
pub fn run_ls(as_json: bool) -> Result<u8, ToolError> {
    let machine = Machine::from_process()?;
    let registry = machine.store.read()?;
    let running = DockerRunner::from_environment().running_projects().ok();
    let report = list_report(&registry, running.as_ref());
    let mut text = render(&report, &registry, as_json)?;
    if !as_json {
        text.push_str(&render_kept_worktrees(&kept_worktrees::list(
            machine.paths.state_directory(),
        )));
    }
    write_stdout(&text)?;
    Ok(exit_code::SUCCESS)
}

fn render(report: &ListReport, registry: &Registry, as_json: bool) -> Result<String, ToolError> {
    if !as_json {
        let opted_out: Vec<String> = registry
            .opted_out
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        return Ok(render_list_text(report, &opted_out));
    }
    let mut json = serde_json::to_string_pretty(report)
        .map_err(|error| ToolError::WriteReport(std::io::Error::other(error)))?;
    json.push('\n');
    Ok(json)
}

/// The report of the registry; `running` holds the names of the compose
/// projects with a running container, `None` when Docker could not tell.
pub fn list_report(registry: &Registry, running: Option<&BTreeSet<String>>) -> ListReport {
    ListReport {
        version: REPORT_VERSION,
        shared: Vec::new(),
        instances: registry
            .instances
            .iter()
            .map(|instance| ListedInstance {
                name: instance.name.clone(),
                path: instance.path.display().to_string(),
                slot: instance.slot,
                host: instance.host.clone(),
                services_running: running.map(|names| names.contains(&instance.name)),
                ports: instance
                    .ports
                    .iter()
                    .map(|assigned| info_port(instance, assigned))
                    .collect(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::discovery::PortKind;
    use crate::port_range::PortRange;
    use crate::registry::record::{AssignedPort, InstanceRecord, PortDetail};

    fn registry_of(names: &[&str]) -> Registry {
        Registry {
            instances: names
                .iter()
                .enumerate()
                .map(|(index, name)| InstanceRecord {
                    path: PathBuf::from(format!("/work/{name}")),
                    name: (*name).to_owned(),
                    slot: u32::try_from(index).unwrap() + 1,
                    range: PortRange {
                        first: 20050,
                        last: 20099,
                    },
                    ports: vec![AssignedPort {
                        variable: "PORT".to_owned(),
                        port: 20050,
                        detail: PortDetail {
                            kind: PortKind::Web,
                            service: None,
                            target: None,
                        },
                    }],
                    host: format!("{name}.localhost"),
                    web_port_variable: None,
                    is_host_unsaved: false,
                })
                .collect(),
            opted_out: Vec::new(),
        }
    }

    #[test]
    fn should_mark_an_instance_running_when_its_name_is_in_the_running_set() {
        let running: BTreeSet<String> = ["app-1".to_owned()].into();

        let report = list_report(&registry_of(&["app-1", "app-2"]), Some(&running));

        let states: Vec<Option<bool>> = report
            .instances
            .iter()
            .map(|instance| instance.services_running)
            .collect();
        assert_eq!(states, [Some(true), Some(false)]);
    }

    #[test]
    fn should_leave_the_state_unknown_when_docker_could_not_tell() {
        let report = list_report(&registry_of(&["app-1"]), None);

        assert_eq!(report.instances[0].services_running, None);
    }

    #[test]
    fn should_give_the_web_port_its_url_and_list_no_shared_service() {
        let report = list_report(&registry_of(&["app-1"]), None);

        assert_eq!(
            report.instances[0].ports[0].url.as_deref(),
            Some("http://localhost:20050")
        );
        assert_eq!(report.shared, Vec::<String>::new());
    }

    #[test]
    fn should_give_each_instance_its_host() {
        let report = list_report(&registry_of(&["app-1", "app-2"]), None);

        let hosts: Vec<&str> = report
            .instances
            .iter()
            .map(|instance| instance.host.as_str())
            .collect();
        assert_eq!(hosts, ["app-1.localhost", "app-2.localhost"]);
    }

    #[test]
    fn should_open_the_stored_web_port_on_the_host_without_reading_any_config_file() {
        let mut registry = registry_of(&["app-1"]);
        registry.instances[0].web_port_variable = Some("PORT".to_owned());

        let report = list_report(&registry, None);

        assert_eq!(
            report.instances[0].ports[0].url.as_deref(),
            Some("http://app-1.localhost:20050")
        );
    }
}
