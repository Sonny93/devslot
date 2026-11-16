//! `ds hook worktree-remove`: removes the git worktree of a Claude Code
//! session, stops the processes of its instance, deletes its data, forgets its
//! instance, then deletes the branch when it is merged (see
//! `commands::worktree_removal` for the order). A worktree that holds unsaved
//! work (uncommitted changes, commits found nowhere else) is kept, whatever
//! the user answered to Claude Code: only `ds worktree rm` from a terminal
//! deletes it. Exit 0 means the folder is
//! gone; any other exit tells Claude Code the removal failed and the worktree
//! stays, which is also what happens when another session or program still
//! runs in the worktree. The closing session is recognised through the
//! `CLAUDE_PID` variable Claude Code sets for its hooks.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;

use super::failure::HookFailure;
use super::input::read_input;
use crate::checkout::CheckoutError;
use crate::cli::exit_code;
use crate::commands::worktree_removal::{BUSY_REASON, remove_linked_worktree};
use crate::error::ToolError;
use crate::instance_processes::claude_pid_from_environment;
use crate::kept_worktrees;
use crate::paths::DevslotPaths;
use crate::session_leases;
use crate::worktree_lock::{LockPurpose, WorktreeKey, WorktreeLock};

#[derive(Debug, Deserialize)]
struct WorktreeRemoveInput {
    worktree_path: PathBuf,
}

/// Reads the hook input from stdin and removes the worktree, unless another
/// creation or removal of it holds its lock: then the worktree is kept, noted
/// for `ds ls`, and the hook still exits 0 (the other holder decides).
pub fn run_worktree_remove() -> Result<u8, HookFailure> {
    let input: WorktreeRemoveInput = read_input(std::io::stdin())?;
    let paths = DevslotPaths::from_process().map_err(ToolError::from)?;
    let state_directory = paths.state_directory();
    let Some(key) = WorktreeKey::of(&input.worktree_path) else {
        return Ok(exit_code::SUCCESS);
    };
    let Some(_lock) = WorktreeLock::try_acquire(state_directory, &key, LockPurpose::Remove)? else {
        keep_noted(state_directory, &key, BUSY_REASON);
        return Ok(exit_code::SUCCESS);
    };
    remove_locked(&input.worktree_path, state_directory, &key)
}

fn remove_locked(
    worktree_path: &Path,
    state_directory: &Path,
    key: &WorktreeKey,
) -> Result<u8, HookFailure> {
    let path = match worktree_path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(exit_code::SUCCESS),
        Err(source) => {
            return Err(CheckoutError::Resolve {
                path: worktree_path.to_path_buf(),
                source,
            }
            .into());
        }
    };
    match remove_linked_worktree(path, claude_pid_from_environment(), state_directory) {
        Ok(()) => {
            kept_worktrees::forget(state_directory, key);
            session_leases::clear(state_directory, key);
            Ok(exit_code::SUCCESS)
        }
        Err(failure) => {
            keep_noted(state_directory, key, &failure.kept_reason());
            failure.announce();
            Ok(failure.exit_code())
        }
    }
}

fn keep_noted(state_directory: &Path, key: &WorktreeKey, reason: &str) {
    let Err(error) = kept_worktrees::record(state_directory, key, reason, SystemTime::now()) else {
        return;
    };
    if error.kind() != ErrorKind::NotFound {
        eprintln!(
            "ds: warning: could not note that {} was kept: {error}",
            key.path().display()
        );
    }
}
