//! What a check reports.

use serde::Serialize;

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Blocks `ds up`; makes `ds doctor` exit with 1.
    Error,
    /// Displayed, never blocking.
    Warning,
    /// For information only.
    Info,
}

impl Level {
    /// The word printed in the text output.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

/// The stable identifier of a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    /// `container_name` on a service.
    ContainerName,
    /// A volume or network with an explicit name or `external: true`.
    ExplicitName,
    /// A hardcoded published port.
    HardcodedPort,
    /// A published port range.
    PortRange,
    /// `env_file` loading `.env` without pinning every managed port variable.
    EnvFilePorts,
    /// A bind mount outside the checkout.
    BindMount,
    /// `.env` missing with nothing to create it from.
    EnvMissing,
    /// `.env` not ignored by git.
    EnvNotIgnored,
    /// A `.env` file that cannot be parsed.
    EnvUnparsable,
    /// A `.env` file that is not a regular file or is too large to read.
    EnvUnreadable,
    /// A malformed devslot block.
    EnvBlockMalformed,
    /// The block cannot be written with the current configuration.
    EnvBlockUnwritable,
    /// `.env.test` redefining a managed variable.
    EnvTestRedefines,
    /// A value that hardcodes the default port of a managed variable.
    EnvLocalHostPort,
    /// A value that points at a local host on a port no devslot variable manages.
    EnvUnmanagedLocalPort,
    /// Vite found but `VITE_HMR_PORT` is not read by its config.
    ViteHmrPort,
    /// More than one Vite config.
    ViteMultipleConfigs,
    /// A port written as a literal in a Vite config, a package script or the justfile.
    ViteLiteralPort,
    /// A compose file that Compose rejects (the dev compose).
    ComposeUnreadable,
    /// A scanned compose file that cannot be read on its own.
    ComposeSkipped,
    /// No compose file at the root.
    ComposeMissing,
    /// `[compose] file` names a file that does not exist.
    ComposeFileMissing,
    /// `devslot.toml` sets a managed port variable in `[env]`.
    EnvForcesPort,
    /// A compose port published through a variable whose name does not end in `PORT`.
    PortVariableUnmanaged,
    /// A committed `[env]` key that `.env` and `.env.example` do not declare.
    EnvKeyUndeclared,
    /// `devslot.local.toml` is tracked by git.
    LocalConfigTracked,
    /// A forced port belongs to another instance.
    ForcedPortConflict,
    /// The registered range no longer matches the slot of the current settings.
    SlotGeometryChanged,
    /// The session runs under a process not named `claude`, or none is found: no lease is written.
    SessionProcessName,
    /// Whether the worktree guard is on for the checkout.
    WorktreeGuard,
    /// `devslot.toml` sets `[worktrees] require`, which only `devslot.local.toml` may.
    WorktreeRequireIgnored,
    /// A main checkout registered before the worktree guard covered it.
    WorktreeGuardRegistered,
    /// `[web] port` names a variable that is not a managed web port.
    WebPortUnmanaged,
    /// A `DEVSLOT_*` key in `.env` or `.env.example` outside the devslot block.
    EnvDevslotKeyOutsideBlock,
}

/// One problem found in the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// How serious it is.
    pub level: Level,
    /// Which check found it.
    pub check: Check,
    /// The file at fault, relative to the checkout root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The compose service at fault.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// The key at fault: a compose key, an env variable name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// What is wrong and what to do. Never holds a `.env` value.
    pub message: String,
}

impl Finding {
    /// A finding without location.
    pub fn new(level: Level, check: Check, message: impl Into<String>) -> Self {
        Self {
            level,
            check,
            file: None,
            service: None,
            key: None,
            message: message.into(),
        }
    }

    /// Names the file at fault.
    #[must_use]
    pub fn in_file(mut self, file: impl Into<String>) -> Self {
        self.file = Some(file.into());
        self
    }

    /// Names the service at fault.
    #[must_use]
    pub fn in_service(mut self, service: impl Into<String>) -> Self {
        self.service = Some(service.into());
        self
    }

    /// Names the key at fault.
    #[must_use]
    pub fn for_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
}

/// The counts `ds doctor` ends with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// How many errors.
    pub errors: usize,
    /// How many warnings.
    pub warnings: usize,
}

impl Summary {
    /// Counts the findings by level.
    pub fn of(findings: &[Finding]) -> Self {
        let count = |level| {
            findings
                .iter()
                .filter(|finding| finding.level == level)
                .count()
        };
        Self {
            errors: count(Level::Error),
            warnings: count(Level::Warning),
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn should_count_errors_and_warnings_apart_when_summarizing() {
        let findings = [
            Finding::new(Level::Error, Check::ContainerName, "a"),
            Finding::new(Level::Warning, Check::BindMount, "b"),
            Finding::new(Level::Warning, Check::BindMount, "c"),
            Finding::new(Level::Info, Check::ComposeMissing, "d"),
        ];

        assert_eq!(
            Summary::of(&findings),
            Summary {
                errors: 1,
                warnings: 2
            }
        );
    }

    #[test]
    fn should_serialize_the_check_as_a_snake_case_id_and_skip_empty_fields() {
        let finding = Finding::new(Level::Error, Check::ContainerName, "m").in_file("compose.yml");

        assert_eq!(
            serde_json::to_string(&finding).unwrap(),
            r#"{"level":"error","check":"container_name","file":"compose.yml","message":"m"}"#
        );
    }
}
