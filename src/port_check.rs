//! Before `ds up` starts the services: which ports of the instance another
//! program already holds.

use crate::allocation::PortProbe;
use crate::discovery::PortKind;
use crate::effective_port::EffectivePort;

/// A port of the instance that another program uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusyPort {
    /// The variable that carries the port.
    pub variable: String,
    /// The port.
    pub port: u16,
}

impl BusyPort {
    /// The line telling the user about it.
    pub fn message(&self) -> String {
        format!(
            "ds: error: port {} ({}) is used by another program",
            self.port, self.variable
        )
    }
}

/// The service ports, and the web ports when `includes_web_ports`, that the
/// probe says are busy and that the instance's own containers do not publish.
/// `ports` are the ports the instance really uses, forced ones included.
pub fn busy_ports(
    ports: &[EffectivePort],
    includes_web_ports: bool,
    own_ports: &[u16],
    probe: &dyn PortProbe,
) -> Vec<BusyPort> {
    ports
        .iter()
        .filter(|port| includes_web_ports || port.detail.kind == PortKind::Service)
        .filter(|port| !own_ports.contains(&port.port))
        .filter(|port| !probe.is_free(port.port))
        .map(|port| BusyPort {
            variable: port.variable.clone(),
            port: port.port,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::effective_port::effective_ports;
    use crate::port_range::PortRange;
    use crate::project_config::{EnvSetting, LOCAL_CONFIG_FILE};
    use crate::registry::record::{AssignedPort, InstanceRecord, PortDetail};

    struct BusyAt(Vec<u16>);

    impl PortProbe for BusyAt {
        fn is_free(&self, port: u16) -> bool {
            !self.0.contains(&port)
        }
    }

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
                assigned("PORT", 20050, PortKind::Web),
                assigned("DB_PORT", 20051, PortKind::Service),
                assigned("MAIL_PORT", 20052, PortKind::Service),
            ],
            host: "app.localhost".to_owned(),
            web_port_variable: None,
            origin: None,
            is_host_unsaved: false,
        }
    }

    fn ports_with(settings: &[EnvSetting]) -> Vec<EffectivePort> {
        effective_ports(&instance(), settings)
    }

    fn forcing(key: &str, value: &str) -> EnvSetting {
        EnvSetting {
            key: key.to_owned(),
            value: value.to_owned(),
            file: LOCAL_CONFIG_FILE,
        }
    }

    fn variables(ports: &[BusyPort]) -> Vec<&str> {
        ports.iter().map(|port| port.variable.as_str()).collect()
    }

    #[test]
    fn should_ignore_a_busy_web_port_when_no_app_is_run() {
        let busy = busy_ports(&ports_with(&[]), false, &[], &BusyAt(vec![20050, 20051]));

        assert_eq!(variables(&busy), ["DB_PORT"]);
    }

    #[test]
    fn should_include_a_busy_web_port_when_an_app_is_run() {
        let busy = busy_ports(&ports_with(&[]), true, &[], &BusyAt(vec![20050, 20051]));

        assert_eq!(variables(&busy), ["PORT", "DB_PORT"]);
    }

    #[test]
    fn should_skip_a_busy_port_that_the_instance_containers_publish() {
        let busy = busy_ports(
            &ports_with(&[]),
            true,
            &[20051],
            &BusyAt(vec![20051, 20052]),
        );

        assert_eq!(variables(&busy), ["MAIL_PORT"]);
    }

    #[test]
    fn should_find_nothing_when_every_port_is_free() {
        assert_eq!(
            busy_ports(&ports_with(&[]), true, &[], &BusyAt(vec![])),
            vec![]
        );
    }

    #[test]
    fn should_report_the_forced_port_when_another_program_uses_it() {
        let busy = busy_ports(
            &ports_with(&[forcing("PORT", "3333")]),
            true,
            &[],
            &BusyAt(vec![3333]),
        );

        assert_eq!(
            busy,
            vec![BusyPort {
                variable: "PORT".to_owned(),
                port: 3333
            }]
        );
    }

    #[test]
    fn should_not_report_the_allocated_port_when_the_variable_is_forced_elsewhere() {
        let busy = busy_ports(
            &ports_with(&[forcing("PORT", "3333")]),
            true,
            &[],
            &BusyAt(vec![20050]),
        );

        assert_eq!(busy, vec![]);
    }

    #[test]
    fn should_name_the_port_and_the_variable_in_the_message() {
        let port = BusyPort {
            variable: "DB_PORT".to_owned(),
            port: 20051,
        };

        assert_eq!(
            port.message(),
            "ds: error: port 20051 (DB_PORT) is used by another program"
        );
    }
}
