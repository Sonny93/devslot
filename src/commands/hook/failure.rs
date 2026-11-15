//! Why a hook did not do its job, and the exit code each reason gives.

use std::io;
use std::path::PathBuf;

use thiserror::Error;

use super::input::InputError;
use crate::checkout::CheckoutError;
use crate::cli::exit_code;
use crate::error::ToolError;
use crate::git_worktree::WorktreeGitError;

/// A failure of `worktree-create`, `worktree-remove` or `session-start`.
#[derive(Debug, Error)]
pub enum HookFailure {
    /// The input on stdin is unusable.
    #[error(transparent)]
    Input(#[from] InputError),
    /// The worktree name is not a safe slug.
    #[error("the worktree name {name:?} is not valid: {reason}")]
    InvalidName {
        /// The name the hook was given.
        name: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The folder is not inside a git repository.
    #[error("{} is not inside a git repository", .0.display())]
    NotInGit(PathBuf),
    /// Nothing tells where to put worktrees.
    #[error("cannot place the worktree: set [worktrees] root in the global config, or HOME")]
    NoWorktreesRoot,
    /// The repository folder has no usable name.
    #[error("cannot name the worktrees folder after the repository at {}", .0.display())]
    UnusableRepositoryName(PathBuf),
    /// The target folder is taken.
    #[error("{} already exists", .0.display())]
    TargetExists(PathBuf),
    /// A parent folder cannot be created.
    #[error("cannot create {}: {source}", path.display())]
    CreateDirectory {
        /// The folder.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// The path is not the root of a linked worktree, so nothing was removed.
    #[error("{} is not the root of a linked worktree; nothing was removed", .0.display())]
    NotLinkedWorktree(PathBuf),
    /// Git refused, or could not run.
    #[error(transparent)]
    Git(#[from] WorktreeGitError),
    /// The checkout cannot be examined.
    #[error(transparent)]
    Checkout(#[from] CheckoutError),
    /// A devslot file or Docker failed.
    #[error(transparent)]
    Tool(#[from] ToolError),
}

impl HookFailure {
    /// 1 when git or the state of the folder refused the operation, 2 when
    /// the tool itself failed or the input is invalid.
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Git(WorktreeGitError::Rejected { .. }) | Self::NotLinkedWorktree(_) => {
                exit_code::ERROR
            }
            _ => exit_code::TOOL_FAILURE,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn should_exit_1_when_git_refuses() {
        let failure = HookFailure::Git(WorktreeGitError::Rejected {
            command: "worktree add",
        });

        assert_eq!(failure.exit_code(), 1);
    }

    #[test]
    fn should_exit_1_when_the_path_is_not_a_linked_worktree() {
        let failure = HookFailure::NotLinkedWorktree(PathBuf::from("/x"));

        assert_eq!(failure.exit_code(), 1);
    }

    #[test]
    fn should_exit_2_when_the_name_is_invalid() {
        let failure = HookFailure::InvalidName {
            name: "..".to_owned(),
            reason: "x",
        };

        assert_eq!(failure.exit_code(), 2);
    }
}
