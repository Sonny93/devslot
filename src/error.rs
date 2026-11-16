//! The failures of the tool itself, as opposed to the findings about a project.

use std::io;

use thiserror::Error;

use crate::checkout::CheckoutError;
use crate::compose::runner::RunnerError;
use crate::deadline::BudgetExhausted;
use crate::docker::DockerError;
use crate::env_sync::EnvSyncError;
use crate::environment::EnvironmentError;
use crate::global_config::GlobalConfigError;
use crate::paths::PathsError;
use crate::project_config::ConfigError;
use crate::registry::RegistryError;
use crate::worktree_guard::GuardError;
use crate::worktree_lock::WorktreeLockError;
use crate::worktree_require_override::OverrideError;

/// Something that stops `ds` from answering. Maps to exit code 2.
#[derive(Debug, Error)]
pub enum ToolError {
    /// The current folder is unknown.
    #[error("cannot determine the current directory: {0}")]
    CurrentDirectory(io::Error),
    /// Git or the folder could not be examined.
    #[error(transparent)]
    Checkout(#[from] CheckoutError),
    /// A devslot config file is unusable.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The global config file is unusable.
    #[error(transparent)]
    GlobalConfig(#[from] GlobalConfigError),
    /// The devslot files cannot be placed.
    #[error(transparent)]
    Paths(#[from] PathsError),
    /// The registry is unreadable, malformed, locked or cannot be written.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// The lock file of a worktree cannot be used.
    #[error(transparent)]
    WorktreeLock(#[from] WorktreeLockError),
    /// The worktree guard switch of a checkout cannot be read.
    #[error(transparent)]
    WorktreeOverride(#[from] OverrideError),
    /// The worktree guard cannot examine the checkout.
    #[error(transparent)]
    WorktreeGuard(#[from] GuardError),
    /// The `[env]` values cannot be built.
    #[error(transparent)]
    Environment(#[from] EnvironmentError),
    /// Docker is unavailable, timed out or answered nonsense.
    #[error(transparent)]
    Docker(#[from] DockerError),
    /// A Docker command that acts on an instance could not run to its end.
    #[error(transparent)]
    Runner(#[from] RunnerError),
    /// `.env` cannot be created or rewritten.
    #[error(transparent)]
    EnvSync(#[from] EnvSyncError),
    /// A project file exists but cannot be read.
    #[error("cannot read {file}: {source}")]
    ReadFile {
        /// The file, relative to the checkout root.
        file: String,
        /// The underlying error.
        source: io::Error,
    },
    /// The time budget of the command ran out.
    #[error(transparent)]
    Budget(#[from] BudgetExhausted),
    /// The report could not be written.
    #[error("cannot write the report: {0}")]
    WriteReport(io::Error),
}

impl ToolError {
    /// Whether the command stopped because its time budget ran out, whichever
    /// step noticed it.
    pub const fn is_budget_exhausted(&self) -> bool {
        matches!(
            self,
            Self::Budget(_)
                | Self::Checkout(CheckoutError::Budget(_))
                | Self::Docker(DockerError::Budget(_))
        )
    }
}
