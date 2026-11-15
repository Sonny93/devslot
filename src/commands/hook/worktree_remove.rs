//! `ds hook worktree-remove`: removes the git worktree of a Claude Code
//! session, deletes its data, forgets its instance, then deletes the branch
//! when it is merged. Exit 0 means the folder is gone; any other exit tells
//! Claude Code the removal failed and the worktree stays. Git goes first: when
//! it refuses, no data is deleted and the registry is not touched.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::failure::HookFailure;
use super::input::read_input;
use crate::checkout::{Checkout, CheckoutError};
use crate::cli::exit_code;
use crate::commands::machine::Machine;
use crate::commands::registering::LOCK_TIMEOUT;
use crate::compose::runner::{ComposeRunner, DockerRunner, delete_project_data};
use crate::error::ToolError;
use crate::git_worktree::{current_branch, delete_branch, is_branch_merged, remove_worktree};
use crate::registry::record::{InstanceRecord, RegistryChange};

const DETACHED_HEAD_NAME: &str = "HEAD";

#[derive(Debug, Deserialize)]
struct WorktreeRemoveInput {
    worktree_path: PathBuf,
}

/// What removing a worktree needs to know before the folder is gone.
struct Removal {
    path: PathBuf,
    main_root: PathBuf,
    branch: Option<String>,
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
    let removal = locate_removal(path)?;
    let known = known_instance(&removal.path);
    remove_worktree(&removal.main_root, &removal.path)?;
    if let Some((machine, instance)) = &known {
        forget_instance(machine, instance, &DockerRunner::from_environment());
    }
    delete_branch_when_merged(&removal);
    Ok(exit_code::SUCCESS)
}

fn locate_removal(path: PathBuf) -> Result<Removal, HookFailure> {
    let checkout = Checkout::locate(&path)?;
    let Some(main_root) = checkout.main_root.clone() else {
        return Err(HookFailure::NotLinkedWorktree(path));
    };
    if !checkout.is_git || checkout.root != path {
        return Err(HookFailure::NotLinkedWorktree(path));
    }
    let branch = current_branch(&path)?;
    Ok(Removal {
        path,
        main_root,
        branch,
    })
}

fn known_instance(path: &Path) -> Option<(Machine, InstanceRecord)> {
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

fn find_instance(path: &Path) -> Result<Option<(Machine, InstanceRecord)>, ToolError> {
    let machine = Machine::from_process()?;
    let instance = machine.store.read()?.find(path).cloned();
    Ok(instance.map(|instance| (machine, instance)))
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

fn delete_branch_when_merged(removal: &Removal) {
    let Some(branch) = &removal.branch else {
        return;
    };
    let folder_name = removal.path.file_name().and_then(|name| name.to_str());
    if folder_name != Some(branch.as_str()) {
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
