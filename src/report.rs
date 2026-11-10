//! The result of `ds doctor`.

use serde::Serialize;

use crate::checks::finding::{Finding, Summary};
use crate::discovery::{ManagedPort, PortKind};

/// The version of the `--json` contract.
pub const REPORT_VERSION: u8 = 1;

/// Everything `ds doctor` found, in the shape `--json` prints.
#[derive(Debug, Serialize)]
pub struct DoctorReport {
    /// The version of this contract.
    pub version: u8,
    /// The checkout root.
    pub checkout: String,
    /// The dev compose file, when there is one.
    pub compose_file: Option<String>,
    /// The managed port variables.
    pub ports: Vec<ManagedPort>,
    /// The problems found.
    pub findings: Vec<Finding>,
    /// The counts by level.
    pub summary: Summary,
}

impl DoctorReport {
    /// Whether at least one finding is an error.
    pub fn has_errors(&self) -> bool {
        self.summary.errors > 0
    }
}

/// One port of a registered instance, in the shape `ds info --json` prints.
/// `ds ls --json` will use the same shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InfoPort {
    /// The variable name.
    #[serde(rename = "var")]
    pub variable: String,
    /// The port.
    pub port: u16,
    /// Web or service.
    pub kind: PortKind,
    /// The compose service that publishes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// The container port behind it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<u16>,
}

/// What `ds info` knows about a registered checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RegisteredInfo {
    /// The version of this contract.
    pub version: u8,
    /// Always true.
    pub registered: bool,
    /// The instance name, also the compose project name.
    pub name: String,
    /// The checkout root.
    pub path: String,
    /// The slot number.
    pub slot: u32,
    /// The first and last port of the slot.
    pub range: [u16; 2],
    /// The dev compose file, relative to the checkout root.
    pub compose_file: Option<String>,
    /// Where the compose file choice comes from: a config file or detection.
    pub compose_file_origin: Option<String>,
    /// The ports of the instance.
    pub ports: Vec<InfoPort>,
}

/// What `ds info` says about a checkout that is not registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnregisteredInfo {
    /// The version of this contract.
    pub version: u8,
    /// Always false.
    pub registered: bool,
    /// The checkout root.
    pub path: String,
}

/// The answer of `ds info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum InfoReport {
    /// The checkout is registered.
    Registered(RegisteredInfo),
    /// The checkout is not registered.
    Unregistered(UnregisteredInfo),
}
