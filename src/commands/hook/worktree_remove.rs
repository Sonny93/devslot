//! `ds hook worktree-remove`: removes the git worktree of a Claude Code
//! session, stops the processes of its instance, deletes its data, forgets its
//! instance, then deletes the branch when it is merged (see
//! `commands::worktree_removal` for the order). Exit 0 means the folder is
//! gone; any other exit tells Claude Code the removal failed and the worktree
//! stays.

use std::io::ErrorKind;

use serde::Deserialize;

use super::failure::HookFailure;
use super::input::read_input;
use crate::checkout::CheckoutError;
use crate::cli::exit_code;
use crate::commands::worktree_removal::remove_linked_worktree;

#[derive(Debug, Deserialize)]
struct WorktreeRemoveInput {
    worktree_path: std::path::PathBuf,
}

/// Reads the hook input from stdin and removes the worktree.
pub fn run_worktree_remove() -> Result<u8, HookFailure> {
    let input: WorktreeRemoveInput = read_input(std::io::stdin())?;
    let path = match input.worktree_path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(exit_code::SUCCESS),
        Err(source) => {
            return Err(CheckoutError::Resolve {
                path: input.worktree_path,
                source,
            }
            .into());
        }
    };
    match remove_linked_worktree(path) {
        Ok(()) => Ok(exit_code::SUCCESS),
        Err(failure) => {
            failure.announce();
            Ok(failure.exit_code())
        }
    }
}
