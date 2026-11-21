//! Ports that another registered instance forces in its `devslot.local.toml`
//! and that this checkout also uses: inside its slot, or forced by it too.

use std::path::Path;

use super::finding::{Check, Finding, Level};
use crate::port_range::PortRange;
use crate::project_config::LOCAL_CONFIG_FILE;
use crate::registry::record::{InstanceRecord, normalize_path};

/// A port that this checkout forces for one of its managed variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnForcedPort {
    /// The managed port variable.
    pub variable: String,
    /// The forced port.
    pub port: u16,
}

/// A registered instance with the ports its `devslot.local.toml` forces.
#[derive(Debug, Clone, Copy)]
pub struct OtherForcing<'a> {
    /// The instance that forces the ports.
    pub instance: &'a InstanceRecord,
    /// The canonical forced ports, as `ds ls` shows them.
    pub ports: &'a [u16],
}

/// What the check looks at.
#[derive(Debug, Clone, Copy)]
pub struct SharedForcingInput<'a> {
    /// The checkout under review; an entry of `others` at this path is skipped.
    pub own_path: &'a Path,
    /// The slot of this checkout, when it is registered.
    pub own_range: Option<PortRange>,
    /// The ports this checkout forces.
    pub own_forced: &'a [OwnForcedPort],
    /// The instances of the registry with their forced ports.
    pub others: &'a [OtherForcing<'a>],
}

/// A warning for each port that another instance forces and that lies in the
/// slot of this checkout or is forced by it too. A port this checkout forces
/// inside the slot of that instance is left to `forced_port_conflict`.
pub fn check_forced_port_shared(input: &SharedForcingInput<'_>) -> Vec<Finding> {
    let own = normalize_path(input.own_path);
    input
        .others
        .iter()
        .filter(|other| normalize_path(&other.instance.path) != own)
        .flat_map(|other| findings_for(input, other))
        .collect()
}

fn findings_for(input: &SharedForcingInput<'_>, other: &OtherForcing<'_>) -> Vec<Finding> {
    let mut ports = other.ports.to_vec();
    ports.sort_unstable();
    ports.dedup();
    ports
        .into_iter()
        .flat_map(|port| findings_for_port(input, other.instance, port))
        .collect()
}

fn findings_for_port(
    input: &SharedForcingInput<'_>,
    instance: &InstanceRecord,
    port: u16,
) -> Vec<Finding> {
    let same_port: Vec<&OwnForcedPort> = input
        .own_forced
        .iter()
        .filter(|forced| forced.port == port)
        .collect();
    if !same_port.is_empty() {
        return same_port
            .into_iter()
            .filter(|_| !instance.range.contains(port))
            .map(|forced| both_force(forced, instance, port))
            .collect();
    }
    if input.own_range.is_some_and(|range| range.contains(port)) {
        return vec![in_own_slot(instance, port)];
    }
    Vec::new()
}

fn both_force(forced: &OwnForcedPort, instance: &InstanceRecord, port: u16) -> Finding {
    Finding::new(
        Level::Warning,
        Check::ForcedPortShared,
        format!(
            "port {port} ({}) is also forced by instance {} in its {LOCAL_CONFIG_FILE}",
            forced.variable, instance.name
        ),
    )
    .in_file(LOCAL_CONFIG_FILE)
    .for_key(&forced.variable)
}

fn in_own_slot(instance: &InstanceRecord, port: u16) -> Finding {
    Finding::new(
        Level::Warning,
        Check::ForcedPortShared,
        format!(
            "port {port} lies in this instance's slot and instance {} forces it in its {LOCAL_CONFIG_FILE}",
            instance.name
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;

    const OWN_RANGE: PortRange = PortRange {
        first: 20050,
        last: 20099,
    };

    fn instance(name: &str, path: &str, range: PortRange) -> InstanceRecord {
        InstanceRecord {
            path: PathBuf::from(path),
            name: name.to_owned(),
            slot: 1,
            range,
            ports: vec![],
            host: format!("{name}.localhost"),
            web_port_variable: None,
            is_host_unsaved: false,
        }
    }

    fn other_instance() -> InstanceRecord {
        instance(
            "foo-1234",
            "/work/other",
            PortRange {
                first: 20100,
                last: 20149,
            },
        )
    }

    fn own_forced(variable: &str, port: u16) -> Vec<OwnForcedPort> {
        vec![OwnForcedPort {
            variable: variable.to_owned(),
            port,
        }]
    }

    fn run(
        own_range: Option<PortRange>,
        own_forced: &[OwnForcedPort],
        instance: &InstanceRecord,
        ports: &[u16],
    ) -> Vec<Finding> {
        check_forced_port_shared(&SharedForcingInput {
            own_path: Path::new("/work/mine"),
            own_range,
            own_forced,
            others: &[OtherForcing { instance, ports }],
        })
    }

    #[test]
    fn should_warn_naming_the_variable_and_the_instance_when_both_force_the_same_port() {
        let findings = run(
            None,
            &own_forced("WEB_PORT", 3333),
            &other_instance(),
            &[3333],
        );

        assert_eq!(
            findings,
            vec![
                Finding::new(
                    Level::Warning,
                    Check::ForcedPortShared,
                    "port 3333 (WEB_PORT) is also forced by instance foo-1234 in its devslot.local.toml",
                )
                .in_file(LOCAL_CONFIG_FILE)
                .for_key("WEB_PORT")
            ]
        );
    }

    #[test]
    fn should_warn_when_another_instance_forces_a_port_in_this_slot() {
        let findings = run(Some(OWN_RANGE), &[], &other_instance(), &[20061]);

        assert_eq!(
            findings
                .iter()
                .map(|finding| (finding.level, finding.check, finding.message.as_str()))
                .collect::<Vec<_>>(),
            [(
                Level::Warning,
                Check::ForcedPortShared,
                "port 20061 lies in this instance's slot and instance foo-1234 forces it in its devslot.local.toml"
            )]
        );
    }

    #[test]
    fn should_not_warn_when_the_forced_port_is_outside_this_slot_and_not_forced_here() {
        let findings = run(
            Some(OWN_RANGE),
            &own_forced("WEB_PORT", 3333),
            &other_instance(),
            &[4444],
        );

        assert_eq!(findings, Vec::new());
    }

    #[test]
    fn should_not_warn_in_the_slot_case_when_this_checkout_is_not_registered() {
        let findings = run(None, &[], &other_instance(), &[20061]);

        assert_eq!(findings, Vec::new());
    }

    #[test]
    fn should_not_warn_again_when_the_port_is_already_a_forced_port_conflict() {
        let other = instance("foo-1234", "/work/other", OWN_RANGE);

        let findings = run(None, &own_forced("DB_PORT", 20061), &other, &[20061]);

        assert_eq!(findings, Vec::new());
    }

    #[test]
    fn should_skip_the_entry_of_this_checkout() {
        let own = instance("mine-1", "/work/mine", OWN_RANGE);

        let findings = run(
            Some(OWN_RANGE),
            &own_forced("WEB_PORT", 3333),
            &own,
            &[3333, 20061],
        );

        assert_eq!(findings, Vec::new());
    }

    #[test]
    fn should_warn_once_when_the_other_instance_forces_the_same_port_twice() {
        let findings = run(Some(OWN_RANGE), &[], &other_instance(), &[20061, 20061]);

        assert_eq!(findings.len(), 1);
    }
}
