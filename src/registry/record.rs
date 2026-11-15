//! What the registry holds, as plain values.

use std::path::{Component, Path, PathBuf};

use crate::discovery::{ManagedPort, PortKind};
use crate::port_range::PortRange;

/// What the registry knows about a port variable besides its number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortDetail {
    /// Web or service, as discovery found it.
    pub kind: PortKind,
    /// The compose service that publishes it.
    pub service: Option<String>,
    /// The container port behind it.
    pub target: Option<u16>,
}

impl From<&ManagedPort> for PortDetail {
    fn from(port: &ManagedPort) -> Self {
        Self {
            kind: port.kind,
            service: port.service.clone(),
            target: port.target,
        }
    }
}

/// A port variable of an instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedPort {
    /// The variable name.
    pub variable: String,
    /// The port the instance owns for it.
    pub port: u16,
    /// Its kind, service and container port.
    pub detail: PortDetail,
}

/// A registered checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceRecord {
    /// The canonical path of the checkout.
    pub path: PathBuf,
    /// The compose project name.
    pub name: String,
    /// The slot number the range was taken from.
    pub slot: u32,
    /// The ports the slot covered when it was assigned.
    pub range: PortRange,
    /// The variables and their ports, in assignment order.
    pub ports: Vec<AssignedPort>,
}

impl InstanceRecord {
    /// The port assigned to `variable`.
    pub fn port_of(&self, variable: &str) -> Option<u16> {
        self.ports
            .iter()
            .find(|assigned| assigned.variable == variable)
            .map(|assigned| assigned.port)
    }
}

/// One change to the registry file. Several changes are applied and written
/// together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryChange {
    /// Adds the instance or updates the entry of its path.
    SaveInstance(InstanceRecord),
    /// Removes the entry of this path; an absent entry is not an error.
    RemoveInstance(PathBuf),
    /// Adds this path to the checkouts opted out of automatic registration.
    OptOut(PathBuf),
    /// Takes this path off the opted-out list, because the user registers it again.
    ClearOptOut(PathBuf),
    /// Takes this path off the opted-out list, because its folder is gone.
    ForgetOptOut(PathBuf),
}

/// Everything in the registry file that devslot understands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registry {
    /// The registered checkouts, in file order.
    pub instances: Vec<InstanceRecord>,
    /// The checkouts `ds rm` opted out of automatic registration.
    pub opted_out: Vec<PathBuf>,
}

impl Registry {
    /// The instance registered at `path`, compared as normalized paths.
    pub fn find(&self, path: &Path) -> Option<&InstanceRecord> {
        let wanted = normalize_path(path);
        self.instances
            .iter()
            .find(|instance| normalize_path(&instance.path) == wanted)
    }

    /// Whether `path` is on the opted-out list, compared as normalized paths.
    pub fn is_opted_out(&self, path: &Path) -> bool {
        let wanted = normalize_path(path);
        self.opted_out
            .iter()
            .any(|opted| normalize_path(opted) == wanted)
    }

    /// Every port any instance holds.
    pub fn held_ports(&self) -> impl Iterator<Item = u16> + '_ {
        self.instances
            .iter()
            .flat_map(|instance| instance.ports.iter().map(|assigned| assigned.port))
    }

    /// Whether another instance than the one at `path` already has `name`.
    pub fn name_is_taken_by_other(&self, name: &str, path: &Path) -> bool {
        let own = normalize_path(path);
        self.instances
            .iter()
            .any(|instance| instance.name == name && normalize_path(&instance.path) != own)
    }

    /// Whether any registered range shares a port with `range`.
    pub fn range_is_overlapped(&self, range: PortRange) -> bool {
        self.instances
            .iter()
            .any(|instance| instance.range.overlaps(range))
    }
}

/// The path without `.`, empty or redundant components, `..` resolved
/// lexically. Nothing is read from the disk.
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn should_tell_an_opted_out_path_when_it_differs_by_a_trailing_slash() {
        let registry = Registry {
            opted_out: vec![PathBuf::from("/work/a")],
            ..Registry::default()
        };

        assert_eq!(
            (
                registry.is_opted_out(Path::new("/work/a/")),
                registry.is_opted_out(Path::new("/work/b"))
            ),
            (true, false)
        );
    }

    #[test]
    fn should_ignore_a_trailing_slash_and_dot_components_when_normalizing() {
        assert_eq!(
            normalize_path(Path::new("/a/./b//c/")),
            PathBuf::from("/a/b/c")
        );
    }

    #[test]
    fn should_resolve_parent_components_when_normalizing() {
        assert_eq!(
            normalize_path(Path::new("/a/b/../c")),
            PathBuf::from("/a/c")
        );
    }
}
