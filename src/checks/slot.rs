//! The check on the slot of a registered checkout.

use super::finding::{Check, Finding, Level};
use crate::global_config::SlotGeometry;
use crate::registry::record::InstanceRecord;

/// A registered instance and the slot geometry in force now.
#[derive(Debug, Clone, Copy)]
pub struct SlotCheckInput<'a> {
    /// The registry entry of the checkout.
    pub instance: &'a InstanceRecord,
    /// The geometry of the global configuration.
    pub geometry: SlotGeometry,
}

/// Warns when the stored range differs from the slot of the same number in the
/// current geometry. The instance keeps its ports either way.
pub fn check_slot_geometry(input: SlotCheckInput<'_>) -> Option<Finding> {
    let instance = input.instance;
    let current = input.geometry.range_of(instance.slot);
    if current == Some(instance.range) {
        return None;
    }
    let now = current.map_or_else(
        || "outside the configured range".to_owned(),
        |range| range.to_string(),
    );
    Some(Finding::new(
        Level::Warning,
        Check::SlotGeometryChanged,
        format!(
            "the registered range {} no longer matches slot {} of the [slots] settings ({now}); the instance keeps its stored ports",
            instance.range, instance.slot
        ),
    ))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::global_config::GlobalConfig;
    use crate::port_range::PortRange;

    fn instance(slot: u32, first: u16, last: u16) -> InstanceRecord {
        InstanceRecord {
            path: PathBuf::from("/a"),
            name: "a".to_owned(),
            slot,
            range: PortRange { first, last },
            ports: vec![],
            host: "app.localhost".to_owned(),
            web_port_variable: None,
            origin: None,
            is_host_unsaved: false,
        }
    }

    fn geometry_of_size(size: u16) -> SlotGeometry {
        GlobalConfig::from_text(&format!("[slots]\nsize = {size}\n"), Path::new("/c"))
            .unwrap()
            .slots
    }

    #[test]
    fn should_not_warn_when_the_range_matches_the_slot() {
        let input = SlotCheckInput {
            instance: &instance(1, 20050, 20099),
            geometry: SlotGeometry::default(),
        };

        assert_eq!(check_slot_geometry(input), None);
    }

    #[test]
    fn should_warn_when_the_slot_size_changed() {
        let input = SlotCheckInput {
            instance: &instance(1, 20050, 20099),
            geometry: geometry_of_size(100),
        };

        let finding = check_slot_geometry(input).unwrap();

        assert_eq!(
            (finding.level, finding.check),
            (Level::Warning, Check::SlotGeometryChanged)
        );
        assert!(finding.message.contains("20050-20099") && finding.message.contains("20100-20199"));
    }

    #[test]
    fn should_warn_when_the_slot_is_past_the_configured_range() {
        let input = SlotCheckInput {
            instance: &instance(500, 45000, 45049),
            geometry: SlotGeometry::default(),
        };

        assert!(
            check_slot_geometry(input)
                .unwrap()
                .message
                .contains("outside the configured range")
        );
    }
}
