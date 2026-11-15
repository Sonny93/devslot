//! `ds hook worktree-create`: creates the git worktree of a Claude Code
//! session in the worktrees folder, registers it, and prints its path as the
//! only line on stdout. Claude Code's own worktree creation is replaced by this
//! hook, so everything else goes to stderr.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::failure::HookFailure;
use super::input::read_input;
use crate::checkout::{Checkout, CheckoutError};
use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::cli::style::Style;
use crate::commands::machine::Machine;
use crate::commands::registering::{announce, refusal_text, register_checkout};
use crate::compose::selection::{DevComposeSelection, select_dev_compose};
use crate::error::ToolError;
use crate::git_worktree::{
    add_on_existing_branch, add_on_new_branch, base_reference, branch_exists,
};
use crate::global_config::{GlobalConfig, home_from_environment};
use crate::project_config::ProjectConfig;
use crate::registration::{Intent, RegistrationOutcome};
use crate::registry::record::normalize_path;

const MAX_NAME_LENGTH: usize = 100;
const PLAIN_STYLE: Style = Style { color: false };
const REFUSAL_CLOSING_LINE: &str = "the worktree was created but devslot did not register it; fix the errors, then run ds up in it";

#[derive(Debug, Deserialize)]
struct WorktreeCreateInput {
    cwd: PathBuf,
    name: String,
}

/// Reads the hook input from stdin, creates the worktree and prints its path.
pub fn run_worktree_create() -> Result<u8, HookFailure> {
    let input: WorktreeCreateInput = read_input(std::io::stdin())?;
    validate_name(&input.name).map_err(|reason| HookFailure::InvalidName {
        name: input.name.clone(),
        reason,
    })?;
    let machine = Machine::from_process()?;
    let global = GlobalConfig::load(&machine.paths.config_file).map_err(ToolError::from)?;
    let path = create_worktree(&input, &global)?;
    register_worktree(&path, &machine, &global);
    write_stdout(&format!("{}\n", path.display()))?;
    Ok(exit_code::SUCCESS)
}

/// Why `name` cannot be a worktree and branch name: letters, digits, `.`, `_`
/// and `-` only, not starting with `.` or `-`, no `..`, at most 100 characters.
pub fn validate_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("it is empty");
    }
    if name.len() > MAX_NAME_LENGTH {
        return Err("it is longer than 100 characters");
    }
    if name.starts_with(['.', '-']) {
        return Err("it starts with . or -");
    }
    if name.contains("..") {
        return Err("it contains ..");
    }
    if !name.chars().all(is_name_character) {
        return Err("it holds a character other than a letter, a digit, . _ or -");
    }
    Ok(())
}

fn is_name_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
}

fn create_worktree(
    input: &WorktreeCreateInput,
    global: &GlobalConfig,
) -> Result<PathBuf, HookFailure> {
    let checkout = Checkout::locate(&input.cwd)?;
    if !checkout.is_git {
        return Err(HookFailure::NotInGit(checkout.root));
    }
    let target = target_path(&checkout, global, &input.name)?;
    prepare_parent(&target)?;
    let main_root = checkout.main_root.as_ref().unwrap_or(&checkout.root);
    add_worktree(main_root, &target, &input.name)?;
    target.canonicalize().map_err(|source| {
        HookFailure::Checkout(CheckoutError::Resolve {
            path: target.clone(),
            source,
        })
    })
}

fn target_path(
    checkout: &Checkout,
    global: &GlobalConfig,
    name: &str,
) -> Result<PathBuf, HookFailure> {
    let root = global
        .worktrees_root(home_from_environment().as_deref())
        .ok_or(HookFailure::NoWorktreesRoot)?;
    let repository = checkout.repository_name()?;
    if matches!(repository.as_str(), "" | "." | "..") {
        return Err(HookFailure::UnusableRepositoryName(checkout.root.clone()));
    }
    let target = normalize_path(&root.join(repository).join(name));
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err(HookFailure::TargetExists(target));
    }
    Ok(target)
}

fn prepare_parent(target: &Path) -> Result<(), HookFailure> {
    let Some(parent) = target.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|source| HookFailure::CreateDirectory {
        path: parent.to_path_buf(),
        source,
    })
}

fn add_worktree(main_root: &Path, target: &Path, name: &str) -> Result<(), HookFailure> {
    if branch_exists(main_root, name)? {
        return Ok(add_on_existing_branch(main_root, target, name)?);
    }
    let base = base_reference(main_root)?;
    Ok(add_on_new_branch(main_root, target, name, &base)?)
}

fn register_worktree(path: &Path, machine: &Machine, global: &GlobalConfig) {
    if let Err(error) = try_register(path, machine, global) {
        eprintln!(
            "ds: warning: could not register {}: {error}; the worktree was created, run ds up in it to try again",
            path.display()
        );
    }
}

fn try_register(path: &Path, machine: &Machine, global: &GlobalConfig) -> Result<(), ToolError> {
    let checkout = Checkout::locate(path)?;
    if !global
        .auto
        .covers(&checkout.root, home_from_environment().as_deref())
    {
        return Ok(());
    }
    let config = ProjectConfig::load(&checkout.root)?;
    let selection = select_dev_compose(&checkout.root, config.compose_file.as_deref());
    if !matches!(selection, DevComposeSelection::Selected(_)) {
        return Ok(());
    }
    match register_checkout(&checkout, machine, global, Intent::Explicit)? {
        RegistrationOutcome::Registered(registration) => announce(&registration, PLAIN_STYLE),
        RegistrationOutcome::SkippedOptedOut => {}
        RegistrationOutcome::Refused(refusal) => {
            eprintln!("ds: warning: {} is not registered", checkout.root.display());
            eprint!(
                "{}",
                refusal_text(&refusal, machine, PLAIN_STYLE, REFUSAL_CLOSING_LINE)
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_accept_a_slug_when_validating_the_name() {
        for name in ["feature-x", "fix_1.2", "A", "a.b-c_d", &"a".repeat(100)] {
            assert_eq!(validate_name(name), Ok(()), "{name}");
        }
    }

    #[test]
    fn should_reject_the_name_when_it_is_unsafe() {
        let too_long = "a".repeat(101);
        for name in [
            "",
            ".hidden",
            "-flag",
            "..",
            "a..b",
            "a/b",
            "a b",
            "a\nb",
            "é",
            "a:b",
            too_long.as_str(),
        ] {
            assert!(validate_name(name).is_err(), "{name:?}");
        }
    }
}
