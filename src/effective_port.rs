//! The port an instance really listens on: the one the registry allocated,
//! replaced by the value `devslot.local.toml` forces when there is one. It is
//! computed for display and for the busy check, never stored: the allocated
//! port stays reserved in the registry.

use std::path::Path;

use crate::discovery::PortKind;
use crate::project_config::{EnvSetting, LOCAL_CONFIG_FILE};
use crate::registry::record::{InstanceRecord, PortDetail};
use crate::worktree_guard::is_tracked;

/// A port variable of an instance with the port it listens on. It is not an
/// `AssignedPort`, so it can never be saved into the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectivePort {
    /// The variable name.
    pub variable: String,
    /// The port the app listens on: the forced one, else the allocated one.
    pub port: u16,
    /// Its kind, service and container port.
    pub detail: PortDetail,
    /// Whether `devslot.local.toml` forces the port.
    pub is_forced: bool,
}

/// The port `setting` forces: it comes from `devslot.local.toml` and its
/// value is a canonical port, written as its number in 1 to 65535 without
/// sign, leading zero or suffix (surrounding blanks aside). The caller checks
/// that the key is a port variable.
pub fn forced_port_of(setting: &EnvSetting) -> Option<u16> {
    if setting.file != LOCAL_CONFIG_FILE {
        return None;
    }
    let text = setting.value.trim();
    let port = text.parse::<u16>().ok()?;
    (port >= 1 && port.to_string() == text).then_some(port)
}

/// The port that `settings` force for `variable`.
pub fn forced_port(settings: &[EnvSetting], variable: &str) -> Option<u16> {
    settings
        .iter()
        .find(|setting| setting.key == variable)
        .and_then(forced_port_of)
}

/// `settings` as far as they may be trusted for display: a canonical forced
/// port in a `devslot.local.toml` that git tracks came with the repository,
/// not from the user, so every setting is dropped then (fail closed, also
/// when git fails or times out). Git is asked only when the file gives a
/// canonical forced port for a variable of `instance`.
pub fn untracked_settings(
    root: &Path,
    instance: &InstanceRecord,
    settings: Vec<EnvSetting>,
) -> Vec<EnvSetting> {
    let forces_a_port = instance
        .ports
        .iter()
        .any(|assigned| forced_port(&settings, &assigned.variable).is_some());
    if !forces_a_port {
        return settings;
    }
    if is_tracked(root, LOCAL_CONFIG_FILE, None).unwrap_or(true) {
        return Vec::new();
    }
    settings
}

/// The ports of `instance` with the forced values of `settings` applied.
pub fn effective_ports(instance: &InstanceRecord, settings: &[EnvSetting]) -> Vec<EffectivePort> {
    instance
        .ports
        .iter()
        .map(|assigned| {
            let forced = forced_port(settings, &assigned.variable);
            EffectivePort {
                variable: assigned.variable.clone(),
                port: forced.unwrap_or(assigned.port),
                detail: assigned.detail.clone(),
                is_forced: forced.is_some(),
            }
        })
        .collect()
}

/// Where `port` can be opened; `None` for a service port. The web port that
/// `[web] port` names is on the host of the instance, the others on
/// `localhost`.
pub fn url_of(instance: &InstanceRecord, port: &EffectivePort) -> Option<String> {
    if port.detail.kind != PortKind::Web {
        return None;
    }
    if is_app_port(instance, port) {
        return Some(format!("http://{}:{}", instance.host, port.port));
    }
    Some(format!("http://localhost:{}", port.port))
}

/// The URL of the web port opened on the host of the instance, when `[web]
/// port` names one of `ports`.
pub fn app_url(instance: &InstanceRecord, ports: &[EffectivePort]) -> Option<String> {
    ports
        .iter()
        .find(|port| is_app_port(instance, port))
        .and_then(|port| url_of(instance, port))
}

fn is_app_port(instance: &InstanceRecord, port: &EffectivePort) -> bool {
    port.detail.kind == PortKind::Web
        && instance.web_port_variable.as_deref() == Some(port.variable.as_str())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::port_range::PortRange;
    use crate::project_config::CONFIG_FILE;
    use crate::registry::record::AssignedPort;

    fn setting(key: &str, value: &str, file: &'static str) -> EnvSetting {
        EnvSetting {
            key: key.to_owned(),
            value: value.to_owned(),
            file,
        }
    }

    fn local(key: &str, value: &str) -> EnvSetting {
        setting(key, value, LOCAL_CONFIG_FILE)
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

    fn port_numbers(ports: &[EffectivePort]) -> Vec<u16> {
        ports.iter().map(|port| port.port).collect()
    }

    #[test]
    fn should_force_a_port_when_the_local_file_gives_its_canonical_number() {
        assert_eq!(forced_port_of(&local("PORT", "3333")), Some(3333));
    }

    #[test]
    fn should_force_a_port_when_the_value_has_surrounding_blanks() {
        assert_eq!(forced_port_of(&local("PORT", " 3333 ")), Some(3333));
    }

    #[test]
    fn should_force_the_highest_port() {
        assert_eq!(forced_port_of(&local("PORT", "65535")), Some(65535));
    }

    #[test]
    fn should_not_force_a_port_when_the_committed_file_gives_it() {
        assert_eq!(forced_port_of(&setting("PORT", "3333", CONFIG_FILE)), None);
    }

    #[test]
    fn should_not_force_a_port_when_the_value_is_not_canonical() {
        let values = [
            "0", "03333", "+3333", "65536", "abc", "", "3333x", "${PORT}",
        ];

        let forced: Vec<Option<u16>> = values
            .iter()
            .map(|value| forced_port_of(&local("PORT", value)))
            .collect();

        assert_eq!(forced, vec![None; values.len()]);
    }

    #[test]
    fn should_find_the_forced_port_of_the_wanted_variable_only() {
        let settings = [local("PORT", "3333"), local("DB_PORT", "5432")];

        assert_eq!(
            (
                forced_port(&settings, "DB_PORT"),
                forced_port(&settings, "ADMIN_PORT")
            ),
            (Some(5432), None)
        );
    }

    #[test]
    fn should_replace_only_the_forced_ports_and_mark_them() {
        let ports = effective_ports(&instance(None), &[local("PORT", "3333")]);

        assert_eq!(port_numbers(&ports), [3333, 20051, 20052]);
        assert_eq!(
            ports.iter().map(|port| port.is_forced).collect::<Vec<_>>(),
            [true, false, false]
        );
    }

    #[test]
    fn should_keep_the_allocated_ports_when_a_forced_value_is_not_canonical() {
        let ports = effective_ports(&instance(None), &[local("PORT", "03333")]);

        assert_eq!(port_numbers(&ports), [20050, 20051, 20052]);
    }

    #[test]
    fn should_put_the_forced_app_port_on_the_host_with_the_forced_number() {
        let instance = instance(Some("PORT"));
        let ports = effective_ports(&instance, &[local("PORT", "3333")]);

        assert_eq!(
            (
                url_of(&instance, &ports[0]),
                url_of(&instance, &ports[1]),
                url_of(&instance, &ports[2]),
                app_url(&instance, &ports)
            ),
            (
                Some("http://app-3f2a.localhost:3333".to_owned()),
                Some("http://localhost:20051".to_owned()),
                None,
                Some("http://app-3f2a.localhost:3333".to_owned())
            )
        );
    }

    #[test]
    fn should_give_no_app_url_when_the_named_variable_is_a_service_port() {
        let instance = instance(Some("DB_PORT"));
        let ports = effective_ports(&instance, &[]);

        assert_eq!(app_url(&instance, &ports), None);
    }
}
