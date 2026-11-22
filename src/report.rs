//! The result of `ds doctor`.

use serde::Serialize;

use crate::checks::finding::{Finding, Summary};
use crate::discovery::{ManagedPort, PortKind};
use crate::services_state::ServicesState;
use crate::tunneler::TunnelerHint;

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
/// `ds ls --json` uses the same shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InfoPort {
    /// The variable name.
    #[serde(rename = "var")]
    pub variable: String,
    /// The port the app listens on: the forced one, else the allocated one.
    pub port: u16,
    /// Web or service.
    pub kind: PortKind,
    /// The compose service that publishes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// The container port behind it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<u16>,
    /// Where a web port can be opened; absent for a service port.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Whether `devslot.local.toml` forces the port: it then lies outside the
    /// slot range and may be shared with another instance. Absent otherwise.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub forced: bool,
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
    /// The raw repository name (the folder name outside git): the same for
    /// every worktree of a repository. `null` when the entry predates it.
    pub project: Option<String>,
    /// The checkout root.
    pub path: String,
    /// Whether git counts the checkout as a linked worktree. `null` when the
    /// entry predates it.
    pub worktree: Option<bool>,
    /// The slot number.
    pub slot: u32,
    /// The local host name of the instance, `<label>.localhost`.
    pub host: String,
    /// Where the app opens: the URL of the web port `[web] port` names, on
    /// the host of the instance; `null` without that setting.
    pub app_url: Option<String>,
    /// The first and last port of the slot.
    pub range: [u16; 2],
    /// The dev compose file, relative to the checkout root.
    pub compose_file: Option<String>,
    /// Where the compose file choice comes from: a config file or detection.
    pub compose_file_origin: Option<String>,
    /// The ports of the instance.
    pub ports: Vec<InfoPort>,
    /// Whether the compose services are up.
    pub services: ServicesState,
    /// The command and the link that open the instance from the user's
    /// computer through Tunneler. Absent, not `null`, when `[tunneler] host`
    /// is not configured or invalid, or the name is outside the bounds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tunneler: Option<TunnelerHint>,
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
    /// Whether `ds rm` opted the checkout out of automatic registration.
    pub opted_out: bool,
}

/// The answer of `ds info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum InfoReport {
    /// The checkout is registered.
    Registered(Box<RegisteredInfo>),
    /// The checkout is not registered.
    Unregistered(UnregisteredInfo),
}

/// A service shared between instances. v1 has none, so no value exists; a
/// later version adds objects to the `shared` array, and a client ignores the
/// keys it does not know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SharedService {}

/// One instance in the shape `ds ls --json` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListedInstance {
    /// The instance name, also the compose project name.
    pub name: String,
    /// The raw repository name, the same for every worktree of a repository;
    /// `null` for an entry saved before devslot stored it.
    pub project: Option<String>,
    /// The checkout root.
    pub path: String,
    /// Whether git counts the checkout as a linked worktree; `null` for an
    /// entry saved before devslot stored it.
    pub worktree: Option<bool>,
    /// The slot number.
    pub slot: u32,
    /// The local host name of the instance, `<label>.localhost`.
    pub host: String,
    /// Where the app opens, as in `ds info --json`; `null` without `[web] port`.
    pub app_url: Option<String>,
    /// Whether a container of the compose project runs; `null` when Docker
    /// could not tell.
    pub services_running: Option<bool>,
    /// The ports of the instance.
    pub ports: Vec<InfoPort>,
}

/// What `ds ls --json` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListReport {
    /// The version of this contract.
    pub version: u8,
    /// Services shared between instances: always empty in v1.
    pub shared: Vec<SharedService>,
    /// Every instance of the machine.
    pub instances: Vec<ListedInstance>,
}
