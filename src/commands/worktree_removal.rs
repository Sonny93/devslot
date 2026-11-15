//! Removing a linked worktree, for `ds hook worktree-remove` and
//! `ds worktree rm`. The order keeps the worst outcomes out of reach:
//!
//! 1. checks with no side effect: the path is the root of a linked worktree
//!    that git lists at exactly that path, the registry entry is read, the
//!    worktree is not locked;
//! 2. the instance's processes are stopped (only the ones that belong to it);
//! 3. `git worktree remove --force`; when git fails after it forgot the
//!    worktree, the removal goes on and the leftover folder is reported;
//! 4. the instance's Docker data is deleted by label and its entry forgotten;
//! 5. the branch is deleted when it is merged.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::checkout::{Checkout, CheckoutError};
use crate::cli::exit_code;
use crate::commands::machine::Machine;
use crate::commands::registering::LOCK_TIMEOUT;
use crate::compose::runner::{ComposeRunner, DockerRunner, delete_project_data};
use crate::error::ToolError;
use crate::git_worktree::{
    WorktreeGitError, current_branch, delete_branch, is_branch_merged, list_worktrees,
    remove_worktree,
};
use crate::instance_processes::{InstanceScope, find_instance_processes, stop_instance_processes};
use crate::registry::record::{InstanceRecord, RegistryChange};

const DETACHED_HEAD_NAME: &str = "HEAD";
const REMOVE_COMMAND: &str = "worktree remove";

/// Why a worktree was not removed, or not fully.
#[derive(Debug, Error)]
pub enum RemovalFailure {
    /// The path is not the root of a linked worktree, so nothing was removed.
    #[error("{} is not the root of a linked worktree; nothing was removed", .0.display())]
    NotLinkedWorktree(PathBuf),
    /// The worktree is locked: nothing was stopped, deleted or forgotten.
    #[error("{} is locked (git worktree unlock first)", .0.display())]
    Locked(PathBuf),
    /// Git refused, or could not run.
    #[error(transparent)]
    Git(#[from] WorktreeGitError),
    /// The checkout cannot be examined.
    #[error(transparent)]
    Checkout(#[from] CheckoutError),
}

impl RemovalFailure {
    /// 1 when the state of the folder or git refused the removal, 2 when the
    /// tool itself failed.
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::NotLinkedWorktree(_)
            | Self::Locked(_)
            | Self::Git(WorktreeGitError::Rejected { .. }) => exit_code::ERROR,
            Self::Git(WorktreeGitError::Tool(_)) | Self::Checkout(_) => exit_code::TOOL_FAILURE,
        }
    }

    /// Prints the failure on stderr. A lock says what to do about it, so it
    /// is not prefixed with `error:`.
    pub fn announce(&self) {
        match self {
            Self::Locked(_) => eprintln!("ds: {self}"),
            _ => eprintln!("ds: error: {self}"),
        }
    }
}

/// What removing a worktree needs to know before the folder is gone.
#[derive(Debug, Clone)]
pub struct Removal {
    /// The canonical root of the linked worktree.
    pub path: PathBuf,
    /// The root of the main worktree of the repository.
    pub main_root: PathBuf,
    /// The branch checked out in the worktree; `None` when `HEAD` is detached.
    pub branch: Option<String>,
}

/// The registry entry of a worktree, with the machine files it came from.
pub struct RegisteredInstance {
    machine: Machine,
    record: InstanceRecord,
}

impl RegisteredInstance {
    /// The registry entry.
    pub const fn record(&self) -> &InstanceRecord {
        &self.record
    }
}

/// A worktree that passed the checks without side effect.
pub struct PreparedRemoval {
    /// The worktree.
    pub removal: Removal,
    /// Its instance, when one is registered for the path.
    pub instance: Option<RegisteredInstance>,
}

impl PreparedRemoval {
    /// What ties processes to the instance; `None` without an instance.
    pub fn scope(&self) -> Option<InstanceScope> {
        self.instance
            .as_ref()
            .map(|instance| scope_of(&instance.record))
    }
}

/// The name and ports that tie a process to `instance`.
pub fn scope_of(instance: &InstanceRecord) -> InstanceScope {
    InstanceScope {
        name: instance.name.clone(),
        ports: instance
            .ports
            .iter()
            .map(|assigned| assigned.port)
            .collect(),
    }
}

/// Checks that `path` (canonical) is the root of a linked worktree and reads
/// its registry entry. Nothing is changed.
pub fn prepare_removal(path: PathBuf) -> Result<PreparedRemoval, RemovalFailure> {
    let removal = locate_removal(path)?;
    let instance = known_instance(&removal.path);
    Ok(PreparedRemoval { removal, instance })
}

/// Fails unless git lists a worktree at exactly this path (a copied folder
/// is not one), and fails when that worktree is locked.
pub fn ensure_listed_and_unlocked(removal: &Removal) -> Result<(), RemovalFailure> {
    let listed = list_worktrees(&removal.main_root)?;
    let Some(entry) = listed.iter().find(|entry| entry.is_at(&removal.path)) else {
        return Err(RemovalFailure::NotLinkedWorktree(removal.path.clone()));
    };
    if entry.is_locked {
        return Err(RemovalFailure::Locked(removal.path.clone()));
    }
    Ok(())
}

/// Removes the worktree at `path` (canonical): the whole sequence.
pub fn remove_linked_worktree(path: PathBuf) -> Result<(), RemovalFailure> {
    remove_prepared(&prepare_removal(path)?)
}

/// Runs the steps 1 (lock check) to 5 on a prepared worktree.
pub fn remove_prepared(prepared: &PreparedRemoval) -> Result<(), RemovalFailure> {
    let removal = &prepared.removal;
    ensure_listed_and_unlocked(removal)?;
    stop_processes(prepared);
    remove_git_worktree(removal)?;
    if let Some(instance) = &prepared.instance {
        forget_instance(
            &instance.machine,
            &instance.record,
            &DockerRunner::from_environment(),
        );
    }
    delete_branch_when_merged(removal);
    Ok(())
}

fn locate_removal(path: PathBuf) -> Result<Removal, RemovalFailure> {
    let checkout = Checkout::locate(&path)?;
    let Some(main_root) = checkout.main_root.clone() else {
        return Err(RemovalFailure::NotLinkedWorktree(path));
    };
    if !checkout.is_git || checkout.root != path {
        return Err(RemovalFailure::NotLinkedWorktree(path));
    }
    let branch = current_branch(&path)?;
    Ok(Removal {
        path,
        main_root,
        branch,
    })
}

fn known_instance(path: &Path) -> Option<RegisteredInstance> {
    match find_instance(path) {
        Ok(found) => found,
        Err(error) => {
            eprintln!(
                "ds: warning: could not read the registry for {}: {error}; ds prune cleans it later",
                path.display()
            );
            None
        }
    }
}

fn find_instance(path: &Path) -> Result<Option<RegisteredInstance>, ToolError> {
    let machine = Machine::from_process()?;
    let record = machine.store.read()?.find(path).cloned();
    Ok(record.map(|record| RegisteredInstance { machine, record }))
}

fn stop_processes(prepared: &PreparedRemoval) {
    let path = &prepared.removal.path;
    let scope = prepared.scope();
    if scope.is_none() {
        print_still_running(find_instance_processes(path, None).still_running_line(path));
        return;
    }
    let stopped = stop_instance_processes(path, scope.as_ref());
    for line in stopped.report.stopped_lines() {
        eprintln!("{line}");
    }
    for survivor in &stopped.survivors {
        eprintln!("ds: warning: {survivor} did not stop");
    }
    print_still_running(stopped.report.still_running_line(path));
}

fn print_still_running(line: Option<String>) {
    if let Some(line) = line {
        eprintln!("{line}");
    }
}

fn remove_git_worktree(removal: &Removal) -> Result<(), RemovalFailure> {
    match remove_worktree(&removal.main_root, &removal.path) {
        Ok(()) => Ok(()),
        Err(WorktreeGitError::Rejected { .. }) => accept_partial_removal(removal),
        Err(error) => Err(error.into()),
    }
}

fn accept_partial_removal(removal: &Removal) -> Result<(), RemovalFailure> {
    let listed = list_worktrees(&removal.main_root)?;
    if listed.iter().any(|entry| entry.is_at(&removal.path)) {
        return Err(WorktreeGitError::Rejected {
            command: REMOVE_COMMAND,
        }
        .into());
    }
    if removal.path.exists() {
        eprintln!(
            "ds: warning: git left {} on disk; remove it by hand",
            removal.path.display()
        );
    }
    Ok(())
}

fn forget_instance(machine: &Machine, instance: &InstanceRecord, runner: &dyn ComposeRunner) {
    if let Err(error) = delete_project_data(runner, &instance.name) {
        eprintln!(
            "ds: warning: could not delete the containers and volumes of {} ({error}); its registry entry stays, ds prune cleans it later",
            instance.name
        );
        return;
    }
    let forgotten = machine.store.locked(LOCK_TIMEOUT, |locked| {
        locked.apply(&[RegistryChange::RemoveInstance(instance.path.clone())])
    });
    if let Err(error) = forgotten {
        eprintln!(
            "ds: warning: could not forget {}: {error}; ds prune cleans it later",
            instance.name
        );
    }
}

/// Whether `delete_branch_when_merged` would delete the branch of `removal`.
pub fn is_branch_deletable(removal: &Removal) -> bool {
    let Some(branch) = &removal.branch else {
        return false;
    };
    is_named_like_folder(removal, branch)
        && is_branch_merged(&removal.main_root, branch).unwrap_or(false)
}

fn is_named_like_folder(removal: &Removal, branch: &str) -> bool {
    removal.path.file_name().and_then(|name| name.to_str()) == Some(branch)
}

fn delete_branch_when_merged(removal: &Removal) {
    let Some(branch) = &removal.branch else {
        return;
    };
    if !is_named_like_folder(removal, branch) {
        eprintln!("ds: kept branch {branch}: not the branch of this worktree");
        return;
    }
    match is_branch_merged(&removal.main_root, branch) {
        Ok(true) => delete_merged(removal, branch),
        Ok(false) => {
            let main_branch = current_branch(&removal.main_root)
                .ok()
                .flatten()
                .unwrap_or_else(|| DETACHED_HEAD_NAME.to_owned());
            eprintln!("ds: kept branch {branch}: not merged into {main_branch}");
        }
        Err(error) => eprintln!("ds: warning: could not check branch {branch}: {error}"),
    }
}

fn delete_merged(removal: &Removal, branch: &str) {
    if let Err(error) = delete_branch(&removal.main_root, branch) {
        eprintln!("ds: warning: could not delete branch {branch}: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_exit_1_when_the_worktree_is_locked_or_not_a_linked_worktree() {
        let locked = RemovalFailure::Locked(PathBuf::from("/x"));
        let not_linked = RemovalFailure::NotLinkedWorktree(PathBuf::from("/x"));

        assert_eq!(locked.exit_code(), exit_code::ERROR);
        assert_eq!(not_linked.exit_code(), exit_code::ERROR);
    }

    #[test]
    fn should_exit_1_when_git_refuses_and_2_when_git_cannot_run() {
        let refused = RemovalFailure::Git(WorktreeGitError::Rejected {
            command: REMOVE_COMMAND,
        });
        let broken = RemovalFailure::Checkout(CheckoutError::GitTimedOut { seconds: 1 });

        assert_eq!(refused.exit_code(), exit_code::ERROR);
        assert_eq!(broken.exit_code(), exit_code::TOOL_FAILURE);
    }

    #[test]
    fn should_say_how_to_unlock_when_the_worktree_is_locked() {
        let failure = RemovalFailure::Locked(PathBuf::from("/x/feat"));

        assert_eq!(
            failure.to_string(),
            "/x/feat is locked (git worktree unlock first)"
        );
    }
}
