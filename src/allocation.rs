//! Slot and port allocation. Pure: the machine is asked through [`PortProbe`].

use std::collections::HashSet;
use std::path::PathBuf;

use thiserror::Error;

use crate::discovery::ManagedPort;
use crate::global_config::SlotGeometry;
use crate::registry::record::{AssignedPort, InstanceRecord, PortDetail, Registry};

/// The first slot a new instance may take: slot 0 is reserved.
const FIRST_ASSIGNABLE_SLOT: u32 = 1;

/// Whether a port is free on this machine.
pub trait PortProbe {
    /// True when nothing else uses `port`.
    fn is_free(&self, port: u16) -> bool;
}

/// A port variable an instance needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortRequest {
    /// The variable name.
    pub variable: String,
    /// Its kind, service and container port.
    pub detail: PortDetail,
}

impl From<&ManagedPort> for PortRequest {
    fn from(port: &ManagedPort) -> Self {
        Self {
            variable: port.variable.clone(),
            detail: PortDetail::from(port),
        }
    }
}

/// Why no port can be given. These are refusals, not tool failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AllocationError {
    /// The instance needs more ports than its slot holds.
    #[error("{requested} ports requested, slots hold {capacity}")]
    SlotFull {
        /// How many variables the instance needs.
        requested: usize,
        /// How many ports the slot holds.
        capacity: usize,
    },
    /// The slot has room but its other ports are used by other programs.
    #[error("no free port left in the slot for {variable}: the others are used by other programs")]
    SlotBusy {
        /// The variable that got no port.
        variable: String,
    },
    /// Every slot of the range is taken or has a busy port.
    #[error("no free slot of {requested} ports is left in the configured range")]
    NoFreeSlot {
        /// How many ports the instance needs.
        requested: usize,
    },
}

/// Who a new instance is.
#[derive(Debug, Clone)]
pub struct NewInstanceIdentity {
    /// The canonical checkout path.
    pub path: PathBuf,
    /// The compose project name.
    pub name: String,
}

/// Gives a known instance a port for each variable it does not hold yet.
///
/// The instance keeps its slot, its range and every port, including those of
/// variables that disappeared. New variables take the first port of the stored
/// range that no instance holds and that is free.
pub fn allocate_known(
    existing: &InstanceRecord,
    registry: &Registry,
    requests: &[PortRequest],
    probe: &dyn PortProbe,
) -> Result<InstanceRecord, AllocationError> {
    let mut record = existing.clone();
    let mut taken: HashSet<u16> = registry.held_ports().collect();
    let missing = requests
        .iter()
        .filter(|request| record.port_of(&request.variable).is_none())
        .count();
    let requested = record.ports.len() + missing;
    for request in requests {
        if let Some(assigned) = record
            .ports
            .iter_mut()
            .find(|assigned| assigned.variable == request.variable)
        {
            assigned.detail = request.detail.clone();
            continue;
        }
        if requested > record.range.len() {
            return Err(AllocationError::SlotFull {
                requested,
                capacity: record.range.len(),
            });
        }
        let port = record
            .range
            .ports()
            .find(|port| !taken.contains(port) && probe.is_free(*port))
            .ok_or_else(|| AllocationError::SlotBusy {
                variable: request.variable.clone(),
            })?;
        taken.insert(port);
        record.ports.push(assigned_port(request, port));
    }
    Ok(record)
}

/// Gives a new instance the smallest slot from 1 of the current geometry whose
/// range overlaps no registered range and whose ports are all free. The
/// variables take its first ports in request order.
pub fn allocate_new(
    identity: NewInstanceIdentity,
    registry: &Registry,
    geometry: SlotGeometry,
    requests: &[PortRequest],
    probe: &dyn PortProbe,
) -> Result<InstanceRecord, AllocationError> {
    if requests.len() > geometry.size() {
        return Err(AllocationError::SlotFull {
            requested: requests.len(),
            capacity: geometry.size(),
        });
    }
    for slot in FIRST_ASSIGNABLE_SLOT..geometry.slot_count() {
        let Some(range) = geometry.range_of(slot) else {
            break;
        };
        if registry.range_is_overlapped(range) || !range.ports().all(|port| probe.is_free(port)) {
            continue;
        }
        let ports = requests
            .iter()
            .zip(range.ports())
            .map(|(request, port)| assigned_port(request, port))
            .collect();
        return Ok(InstanceRecord {
            path: identity.path,
            name: identity.name,
            slot,
            range,
            ports,
        });
    }
    Err(AllocationError::NoFreeSlot {
        requested: requests.len(),
    })
}

fn assigned_port(request: &PortRequest, port: u16) -> AssignedPort {
    AssignedPort {
        variable: request.variable.clone(),
        port,
        detail: request.detail.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::discovery::PortKind;
    use crate::global_config::GlobalConfig;
    use crate::port_range::PortRange;

    struct BusyPorts(HashSet<u16>);

    impl PortProbe for BusyPorts {
        fn is_free(&self, port: u16) -> bool {
            !self.0.contains(&port)
        }
    }

    fn nothing_busy() -> BusyPorts {
        BusyPorts(HashSet::new())
    }

    fn request(variable: &str) -> PortRequest {
        PortRequest {
            variable: variable.to_owned(),
            detail: PortDetail {
                kind: PortKind::Web,
                service: None,
                target: None,
            },
        }
    }

    fn service_request(variable: &str, service: &str, target: u16) -> PortRequest {
        PortRequest {
            variable: variable.to_owned(),
            detail: PortDetail {
                kind: PortKind::Service,
                service: Some(service.to_owned()),
                target: Some(target),
            },
        }
    }

    fn identity(path: &str) -> NewInstanceIdentity {
        NewInstanceIdentity {
            path: PathBuf::from(path),
            name: format!("name{}", path.len()),
        }
    }

    fn geometry(first: u16, last: u16, size: u16) -> SlotGeometry {
        GlobalConfig::from_text(
            &format!("[slots]\nfirst_port = {first}\nlast_port = {last}\nsize = {size}\n"),
            std::path::Path::new("/c"),
        )
        .unwrap()
        .slots
    }

    fn record(path: &str, slot: u32, range: (u16, u16), ports: &[(&str, u16)]) -> InstanceRecord {
        InstanceRecord {
            path: PathBuf::from(path),
            name: format!("name-{slot}-{}", path.len()),
            slot,
            range: PortRange {
                first: range.0,
                last: range.1,
            },
            ports: ports
                .iter()
                .map(|(variable, port)| assigned_port(&request(variable), *port))
                .collect(),
        }
    }

    fn registry_of(instances: Vec<InstanceRecord>) -> Registry {
        Registry {
            instances,
            opted_out: vec![],
        }
    }

    fn variables_and_ports(record: &InstanceRecord) -> Vec<(&str, u16)> {
        record
            .ports
            .iter()
            .map(|assigned| (assigned.variable.as_str(), assigned.port))
            .collect()
    }

    #[test]
    fn should_give_a_new_instance_slot_one_with_ports_in_request_order() {
        let requests = [
            request("PORT"),
            request("VITE_HMR_PORT"),
            service_request("DB_PORT", "postgres", 5432),
        ];

        let record = allocate_new(
            identity("/a"),
            &Registry::default(),
            SlotGeometry::default(),
            &requests,
            &nothing_busy(),
        )
        .unwrap();

        assert_eq!(
            (record.slot, record.range, variables_and_ports(&record)),
            (
                1,
                PortRange {
                    first: 20050,
                    last: 20099
                },
                vec![
                    ("PORT", 20050),
                    ("VITE_HMR_PORT", 20051),
                    ("DB_PORT", 20052)
                ]
            )
        );
    }

    #[test]
    fn should_keep_the_service_details_when_a_new_instance_is_allocated() {
        let record = allocate_new(
            identity("/a"),
            &Registry::default(),
            SlotGeometry::default(),
            &[service_request("DB_PORT", "postgres", 5432)],
            &nothing_busy(),
        )
        .unwrap();

        assert_eq!(
            record.ports[0].detail,
            service_request("DB_PORT", "postgres", 5432).detail
        );
    }

    #[test]
    fn should_never_use_slot_zero_when_allocating_a_new_instance() {
        let record = allocate_new(
            identity("/a"),
            &Registry::default(),
            SlotGeometry::default(),
            &[],
            &nothing_busy(),
        )
        .unwrap();

        assert!(record.range.first >= 20050);
    }

    #[test]
    fn should_take_the_next_slot_when_the_first_range_is_registered() {
        let registry = registry_of(vec![record("/a", 1, (20050, 20099), &[("PORT", 20050)])]);

        let record = allocate_new(
            identity("/b"),
            &registry,
            SlotGeometry::default(),
            &[request("PORT")],
            &nothing_busy(),
        )
        .unwrap();

        assert_eq!((record.slot, record.ports[0].port), (2, 20100));
    }

    #[test]
    fn should_skip_a_slot_when_one_of_its_ports_is_busy() {
        let busy = BusyPorts(HashSet::from([20099]));

        let record = allocate_new(
            identity("/a"),
            &Registry::default(),
            SlotGeometry::default(),
            &[request("PORT")],
            &busy,
        )
        .unwrap();

        assert_eq!(record.slot, 2);
    }

    #[test]
    fn should_skip_a_slot_that_overlaps_a_range_stored_before_a_size_change() {
        let registry = registry_of(vec![record("/a", 1, (20050, 20139), &[("PORT", 20050)])]);

        let record = allocate_new(
            identity("/b"),
            &registry,
            SlotGeometry::default(),
            &[request("PORT")],
            &nothing_busy(),
        )
        .unwrap();

        assert_eq!(record.range.first, 20150);
    }

    #[test]
    fn should_not_reuse_a_slot_number_taken_by_an_older_geometry_when_the_range_is_free() {
        let registry = registry_of(vec![record("/a", 1, (20050, 20099), &[("PORT", 20050)])]);

        let record = allocate_new(
            identity("/b"),
            &registry,
            geometry(20000, 29999, 100),
            &[request("PORT")],
            &nothing_busy(),
        )
        .unwrap();

        assert_eq!(
            (record.slot, record.range),
            (
                1,
                PortRange {
                    first: 20100,
                    last: 20199
                }
            )
        );
    }

    #[test]
    fn should_refuse_a_new_instance_when_it_needs_more_ports_than_a_slot_holds() {
        let requests: Vec<PortRequest> = (0..4).map(|n| request(&format!("V{n}"))).collect();

        let error = allocate_new(
            identity("/a"),
            &Registry::default(),
            geometry(20000, 20999, 3),
            &requests,
            &nothing_busy(),
        )
        .unwrap_err();

        assert_eq!(
            error,
            AllocationError::SlotFull {
                requested: 4,
                capacity: 3
            }
        );
    }

    #[test]
    fn should_refuse_a_new_instance_when_no_slot_is_left() {
        let registry = registry_of(vec![record("/a", 1, (20050, 20099), &[("PORT", 20050)])]);

        let error = allocate_new(
            identity("/b"),
            &registry,
            geometry(20000, 20099, 50),
            &[request("PORT")],
            &nothing_busy(),
        )
        .unwrap_err();

        assert_eq!(error, AllocationError::NoFreeSlot { requested: 1 });
    }

    #[test]
    fn should_keep_every_port_and_the_range_when_the_instance_is_known() {
        let existing = record(
            "/a",
            1,
            (20050, 20099),
            &[("PORT", 20050), ("DB_PORT", 20051)],
        );
        let registry = registry_of(vec![existing.clone()]);

        let record = allocate_known(
            &existing,
            &registry,
            &[request("PORT"), request("DB_PORT")],
            &nothing_busy(),
        )
        .unwrap();

        assert_eq!(record, existing);
    }

    #[test]
    fn should_give_a_new_variable_the_first_free_port_of_the_stored_range_when_the_instance_is_known()
     {
        let existing = record(
            "/a",
            1,
            (20050, 20099),
            &[("PORT", 20050), ("DB_PORT", 20051)],
        );
        let registry = registry_of(vec![existing.clone()]);
        let busy = BusyPorts(HashSet::from([20052]));

        let record = allocate_known(
            &existing,
            &registry,
            &[request("PORT"), request("DB_PORT"), request("MAIL_PORT")],
            &busy,
        )
        .unwrap();

        assert_eq!(record.port_of("MAIL_PORT"), Some(20053));
    }

    #[test]
    fn should_keep_the_port_of_a_variable_that_disappeared() {
        let existing = record(
            "/a",
            1,
            (20050, 20099),
            &[("PORT", 20050), ("OLD_PORT", 20051)],
        );
        let registry = registry_of(vec![existing.clone()]);

        let record =
            allocate_known(&existing, &registry, &[request("PORT")], &nothing_busy()).unwrap();

        assert_eq!(record.port_of("OLD_PORT"), Some(20051));
    }

    #[test]
    fn should_not_hand_out_the_port_of_a_variable_that_disappeared() {
        let existing = record("/a", 1, (20050, 20099), &[("OLD_PORT", 20050)]);
        let registry = registry_of(vec![existing.clone()]);

        let record = allocate_known(
            &existing,
            &registry,
            &[request("NEW_PORT")],
            &nothing_busy(),
        )
        .unwrap();

        assert_eq!(record.port_of("NEW_PORT"), Some(20051));
    }

    #[test]
    fn should_refresh_the_details_of_a_variable_discovered_again() {
        let existing = record("/a", 1, (20050, 20099), &[("DB_PORT", 20050)]);
        let registry = registry_of(vec![existing.clone()]);

        let record = allocate_known(
            &existing,
            &registry,
            &[service_request("DB_PORT", "postgres", 5432)],
            &nothing_busy(),
        )
        .unwrap();

        assert_eq!(record.ports[0].detail.service.as_deref(), Some("postgres"));
    }

    #[test]
    fn should_allocate_inside_the_stored_range_when_the_geometry_has_changed() {
        let existing = record("/a", 1, (20050, 20099), &[("PORT", 20050)]);
        let registry = registry_of(vec![existing.clone()]);

        let record = allocate_known(
            &existing,
            &registry,
            &[request("PORT"), request("DB_PORT")],
            &nothing_busy(),
        )
        .unwrap();

        assert_eq!(record.port_of("DB_PORT"), Some(20051));
    }

    #[test]
    fn should_refuse_a_known_instance_when_its_slot_is_full() {
        let existing = record(
            "/a",
            1,
            (20050, 20051),
            &[("PORT", 20050), ("DB_PORT", 20051)],
        );
        let registry = registry_of(vec![existing.clone()]);

        let error = allocate_known(
            &existing,
            &registry,
            &[request("PORT"), request("DB_PORT"), request("MAIL_PORT")],
            &nothing_busy(),
        )
        .unwrap_err();

        assert_eq!(
            error,
            AllocationError::SlotFull {
                requested: 3,
                capacity: 2
            }
        );
    }

    #[test]
    fn should_refuse_a_known_instance_when_the_remaining_ports_are_busy() {
        let existing = record("/a", 1, (20050, 20052), &[("PORT", 20050)]);
        let registry = registry_of(vec![existing.clone()]);
        let busy = BusyPorts(HashSet::from([20051, 20052]));

        let error = allocate_known(
            &existing,
            &registry,
            &[request("PORT"), request("DB_PORT")],
            &busy,
        )
        .unwrap_err();

        assert_eq!(
            error,
            AllocationError::SlotBusy {
                variable: "DB_PORT".to_owned()
            }
        );
    }

    #[test]
    fn should_never_hand_out_a_port_held_by_another_entry_when_the_registry_is_inconsistent() {
        let existing = record("/a", 1, (20050, 20099), &[("PORT", 20050)]);
        let other = record("/b", 2, (20100, 20149), &[("PORT", 20051)]);
        let registry = registry_of(vec![existing.clone(), other]);

        let record = allocate_known(
            &existing,
            &registry,
            &[request("PORT"), request("DB_PORT")],
            &nothing_busy(),
        )
        .unwrap();

        assert_eq!(record.port_of("DB_PORT"), Some(20052));
    }
}
