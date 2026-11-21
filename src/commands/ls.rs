//! `ds ls`: every instance of the machine, from the registry, with whether
//! its services run. One Docker call for the whole machine, no lock. The
//! only file read per instance is its `devslot.local.toml`, for forced ports.

use std::collections::BTreeSet;

use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::cli::render::{render_kept_worktrees, render_list_text};
use crate::commands::info::info_port;
use crate::commands::machine::Machine;
use crate::compose::runner::{ComposeRunner, DockerRunner};
use crate::effective_port::{app_url, effective_ports, read_forcing_settings};
use crate::error::ToolError;
use crate::kept_worktrees;
use crate::project_config::EnvSetting;
use crate::registry::record::{InstanceRecord, Registry};
use crate::report::{ListReport, ListedInstance, REPORT_VERSION};

/// Prints the instances of the machine.
pub fn run_ls(as_json: bool) -> Result<u8, ToolError> {
    let machine = Machine::from_process()?;
    let registry = machine.store.read()?;
    let running = DockerRunner::from_environment().running_projects().ok();
    let report = list_report(&registry, running.as_ref(), &read_forcing_settings);
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
/// `read_settings` gives the settings that may force the ports of an instance.
pub fn list_report(
    registry: &Registry,
    running: Option<&BTreeSet<String>>,
    read_settings: &dyn Fn(&InstanceRecord) -> Vec<EnvSetting>,
) -> ListReport {
    ListReport {
        version: REPORT_VERSION,
        shared: Vec::new(),
        instances: registry
            .instances
            .iter()
            .map(|instance| listed_instance(instance, running, read_settings))
            .collect(),
    }
}

fn listed_instance(
    instance: &InstanceRecord,
    running: Option<&BTreeSet<String>>,
    read_settings: &dyn Fn(&InstanceRecord) -> Vec<EnvSetting>,
) -> ListedInstance {
    let ports = effective_ports(instance, &read_settings(instance));
    ListedInstance {
        name: instance.name.clone(),
        project: instance
            .origin
            .as_ref()
            .map(|origin| origin.project.clone()),
        path: instance.path.display().to_string(),
        worktree: instance.origin.as_ref().map(|origin| origin.is_worktree),
        slot: instance.slot,
        host: instance.host.clone(),
        app_url: app_url(instance, &ports),
        services_running: running.map(|names| names.contains(&instance.name)),
        ports: ports.iter().map(|port| info_port(instance, port)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;
    use crate::discovery::PortKind;
    use crate::port_range::PortRange;
    use crate::project_config::LOCAL_CONFIG_FILE;
    use crate::registry::record::{AssignedPort, CheckoutOrigin, PortDetail};

    fn no_settings(_: &InstanceRecord) -> Vec<EnvSetting> {
        Vec::new()
    }

    fn web_port(variable: &str, port: u16) -> AssignedPort {
        AssignedPort {
            variable: variable.to_owned(),
            port,
            detail: PortDetail {
                kind: PortKind::Web,
                service: None,
                target: None,
            },
        }
    }

    fn instance(name: &str, slot: u32, first_port: u16) -> InstanceRecord {
        InstanceRecord {
            path: PathBuf::from(format!("/work/{name}")),
            name: name.to_owned(),
            slot,
            range: PortRange {
                first: first_port,
                last: first_port + 49,
            },
            ports: vec![web_port("PORT", first_port)],
            host: format!("{name}.localhost"),
            web_port_variable: None,
            origin: None,
            is_host_unsaved: false,
        }
    }

    fn origin(project: &str, is_worktree: bool) -> CheckoutOrigin {
        CheckoutOrigin {
            project: project.to_owned(),
            is_worktree,
        }
    }

    fn registry_of(names: &[&str]) -> Registry {
        Registry {
            instances: names
                .iter()
                .enumerate()
                .map(|(index, name)| instance(name, u32::try_from(index).unwrap() + 1, 20050))
                .collect(),
            opted_out: Vec::new(),
        }
    }

    fn golden_registry() -> Registry {
        let mut main = instance("app-main", 1, 20050);
        main.origin = Some(origin("my-links", false));
        main.web_port_variable = Some("PORT".to_owned());
        main.ports.push(AssignedPort {
            variable: "DB_PORT".to_owned(),
            port: 20051,
            detail: PortDetail {
                kind: PortKind::Service,
                service: Some("postgres".to_owned()),
                target: Some(5432),
            },
        });
        let mut linked = instance("app-linked", 2, 20100);
        linked.origin = Some(origin("my-links", true));
        let mut forced = instance("app-forced", 3, 20150);
        forced.web_port_variable = Some("PORT".to_owned());
        Registry {
            instances: vec![main, linked, forced],
            opted_out: Vec::new(),
        }
    }

    fn forcing_the_port_of_the_third_instance(instance: &InstanceRecord) -> Vec<EnvSetting> {
        if instance.name != "app-forced" {
            return Vec::new();
        }
        vec![EnvSetting {
            key: "PORT".to_owned(),
            value: "3333".to_owned(),
            file: LOCAL_CONFIG_FILE,
        }]
    }

    #[test]
    fn should_print_the_v1_contract_when_the_registry_covers_every_case() {
        let running: BTreeSet<String> = ["app-main".to_owned()].into();

        let report = list_report(
            &golden_registry(),
            Some(&running),
            &forcing_the_port_of_the_third_instance,
        );

        assert_eq!(
            serde_json::to_value(report).unwrap(),
            json!({
                "version": 1,
                "shared": [],
                "instances": [
                    {
                        "name": "app-main",
                        "project": "my-links",
                        "path": "/work/app-main",
                        "worktree": false,
                        "slot": 1,
                        "host": "app-main.localhost",
                        "app_url": "http://app-main.localhost:20050",
                        "services_running": true,
                        "ports": [
                            {
                                "var": "PORT",
                                "port": 20050,
                                "kind": "web",
                                "url": "http://app-main.localhost:20050"
                            },
                            {
                                "var": "DB_PORT",
                                "port": 20051,
                                "kind": "service",
                                "service": "postgres",
                                "target": 5432
                            }
                        ]
                    },
                    {
                        "name": "app-linked",
                        "project": "my-links",
                        "path": "/work/app-linked",
                        "worktree": true,
                        "slot": 2,
                        "host": "app-linked.localhost",
                        "app_url": null,
                        "services_running": false,
                        "ports": [
                            {
                                "var": "PORT",
                                "port": 20100,
                                "kind": "web",
                                "url": "http://localhost:20100"
                            }
                        ]
                    },
                    {
                        "name": "app-forced",
                        "project": null,
                        "path": "/work/app-forced",
                        "worktree": null,
                        "slot": 3,
                        "host": "app-forced.localhost",
                        "app_url": "http://app-forced.localhost:3333",
                        "services_running": false,
                        "ports": [
                            {
                                "var": "PORT",
                                "port": 3333,
                                "kind": "web",
                                "url": "http://app-forced.localhost:3333",
                                "forced": true
                            }
                        ]
                    }
                ]
            })
        );
    }

    #[test]
    fn should_leave_the_state_unknown_when_docker_could_not_tell() {
        let report = list_report(&registry_of(&["app-1"]), None, &no_settings);

        assert_eq!(report.instances[0].services_running, None);
    }
}
