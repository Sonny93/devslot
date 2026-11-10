//! The result of `ds doctor`.

use serde::Serialize;

use crate::checks::finding::{Finding, Summary};
use crate::discovery::ManagedPort;

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
