//! `ds hook worktree-create`: creates the git worktree of a Claude Code
//! session in the worktrees folder, registers it, and prints its path as the
//! only line on stdout. Claude Code's own worktree creation is replaced by this
//! hook, so everything else goes to stderr.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::failure::HookFailure;
use super::input::{INPUT_TIMEOUT, read_input_within};
use crate::checkout::{Checkout, CheckoutError};
use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::cli::style::Style;
use crate::commands::ensure::{EnsureResult, PROBE_TIMEOUT, ensure_directory_within};
use crate::commands::machine::Machine;
use crate::commands::registering::{announce, refusal_text, register_checkout_within};
use crate::compose::selection::{DevComposeSelection, select_dev_compose};
use crate::deadline::{BudgetExhausted, Deadline};
use crate::error::ToolError;
use crate::git_worktree::{
    ListedWorktree, WorktreeGitError, add_on_existing_branch, add_on_new_branch, base_reference,
    branch_exists, delete_branch_within, list_worktrees, list_worktrees_within,
    remove_interrupted_worktree,
};
use crate::global_config::{GlobalConfig, home_from_environment};
use crate::project_config::ProjectConfig;
use crate::registration::{Intent, RegistrationOutcome};
use crate::registry::record::normalize_path;

const MAX_NAME_LENGTH: usize = 100;
/// Claude Code kills the hook after 180 seconds; the work stops before that.
const WORKTREE_CREATE_BUDGET: Duration = Duration::from_secs(170);
/// Registration is skipped when less than this is left: the first command of
/// the agent registers the worktree anyway.
const MIN_REGISTRATION_BUDGET: Duration = Duration::from_secs(20);
/// The reason git gives the lock it holds on a worktree while creating it.
const INITIALIZING_LOCK_REASON: &str = "initializing";
/// How long each step of the cleanup of an interrupted creation may take.
const CLEANUP_STEP_TIMEOUT: Duration = Duration::from_secs(10);
const PLAIN_STYLE: Style = Style { color: false };
const REFUSAL_CLOSING_LINE: &str = "the worktree was created but devslot did not register it; fix the errors, then run ds up in it";

#[derive(Debug, Deserialize)]
struct WorktreeCreateInput {
    cwd: PathBuf,
    name: String,
}

/// Reads the hook input from stdin, creates the worktree and prints its path.
/// The whole run has [`WORKTREE_CREATE_BUDGET`]; registration is optional and
/// gives way to printing the path.
pub fn run_worktree_create() -> Result<u8, HookFailure> {
    let deadline = Deadline::starting_now(WORKTREE_CREATE_BUDGET);
    let input_timeout = deadline.clamp(INPUT_TIMEOUT).unwrap_or(Duration::ZERO);
    let input: WorktreeCreateInput = read_input_within(std::io::stdin(), input_timeout)?;
    validate_name(&input.name).map_err(|reason| HookFailure::InvalidName {
        name: input.name.clone(),
        reason,
    })?;
    let machine = Machine::from_process()?;
    let global = GlobalConfig::load(&machine.paths.config_file).map_err(ToolError::from)?;
    let path = create_worktree(&input, &global, deadline)?;
    register_worktree(&path, (&machine, &global), deadline);
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
    deadline: Deadline,
) -> Result<PathBuf, HookFailure> {
    let checkout = Checkout::locate_within(&input.cwd, deadline)?;
    if !checkout.is_git {
        return Err(HookFailure::NotInGit(checkout.root));
    }
    let target = target_path(&checkout, global, &input.name)?;
    let main_root = checkout.main_root.as_ref().unwrap_or(&checkout.root);
    if std::fs::symlink_metadata(&target).is_ok() {
        return reopen_worktree(&target, main_root, deadline);
    }
    prepare_parent(&target)?;
    add_worktree(main_root, &target, &input.name, deadline)?;
    target.canonicalize().map_err(|source| {
        HookFailure::Checkout(CheckoutError::Resolve {
            path: target.clone(),
            source,
        })
    })
}

/// Where the worktree called `name` of the repository of `checkout` lives:
/// `<[worktrees] root>/<repository>/<name>`.
pub fn target_path(
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
    Ok(normalize_path(&root.join(repository).join(name)))
}

fn reopen_worktree(
    target: &Path,
    main_root: &Path,
    deadline: Deadline,
) -> Result<PathBuf, HookFailure> {
    if !target.is_dir() {
        return Err(HookFailure::TargetExists(target.to_path_buf()));
    }
    let canonical = target
        .canonicalize()
        .map_err(|source| CheckoutError::Resolve {
            path: target.to_path_buf(),
            source,
        })?;
    refuse_locked(main_root, &canonical, target)?;
    let existing = Checkout::locate_within(&canonical, deadline)?;
    let is_linked_worktree_of_repository = existing.is_git
        && existing.root == canonical
        && existing.main_root.as_deref() == Some(main_root);
    if !is_linked_worktree_of_repository {
        return Err(HookFailure::TargetExists(target.to_path_buf()));
    }
    Ok(canonical)
}

/// Refuses a worktree git keeps locked: with the reason git writes during a
/// creation, an interrupted one; with any other, one someone locked.
fn refuse_locked(main_root: &Path, canonical: &Path, target: &Path) -> Result<(), HookFailure> {
    let listed = list_worktrees(main_root)?;
    let Some(worktree) = listed
        .iter()
        .find(|listed| listed.is_locked && listed.is_at(canonical))
    else {
        return Ok(());
    };
    if worktree.lock_reason.as_deref() == Some(INITIALIZING_LOCK_REASON) {
        return Err(HookFailure::HalfCreated(target.to_path_buf()));
    }
    Err(HookFailure::Locked(target.to_path_buf()))
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

fn add_worktree(
    main_root: &Path,
    target: &Path,
    name: &str,
    deadline: Deadline,
) -> Result<(), HookFailure> {
    let some_deadline = Some(deadline);
    let is_new_branch = !branch_exists(main_root, name, some_deadline)?;
    let base = if is_new_branch {
        Some(base_reference(main_root, some_deadline)?)
    } else {
        None
    };
    let timeout =
        add_timeout(deadline.remaining()).ok_or(CheckoutError::Budget(BudgetExhausted))?;
    let added = match &base {
        Some(base) => add_on_new_branch(main_root, target, name, base, timeout),
        None => add_on_existing_branch(main_root, target, name, timeout),
    };
    let Err(error) = &added else {
        return Ok(());
    };
    if !error.is_interruption() {
        return Ok(added?);
    }
    match resolve_interruption(main_root, target, deadline) {
        InterruptionAction::Keep => {
            eprintln!(
                "ds: warning: the post-checkout hook of the repository did not finish in time; the worktree {} was created",
                target.display()
            );
            return Ok(());
        }
        InterruptionAction::CleanUp => {
            clean_up_interrupted(main_root, target, is_new_branch.then_some(name), deadline);
        }
        InterruptionAction::LeaveAndFail => {}
    }
    Ok(added?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InterruptionAction {
    Keep,
    CleanUp,
    LeaveAndFail,
}

/// Git unlocks a worktree once its checkout is done, before `post-checkout`
/// runs: a worktree that is listed and unlocked is complete. When the listing
/// failed (`Err`), nothing is known: nothing is deleted and the add fails.
fn after_interruption(listing: Result<Option<&ListedWorktree>, ()>) -> InterruptionAction {
    match listing {
        Err(()) => InterruptionAction::LeaveAndFail,
        Ok(Some(worktree)) if !worktree.is_locked => InterruptionAction::Keep,
        Ok(_) => InterruptionAction::CleanUp,
    }
}

/// What to do with the worktree after an interrupted add.
fn resolve_interruption(main_root: &Path, target: &Path, deadline: Deadline) -> InterruptionAction {
    let Some(timeout) = deadline.clamp(CLEANUP_STEP_TIMEOUT) else {
        return after_interruption(Err(()));
    };
    let Ok(listed) = list_worktrees_within(main_root, timeout) else {
        return after_interruption(Err(()));
    };
    let canonical = target.canonicalize().ok();
    let entry = canonical
        .as_deref()
        .and_then(|path| listed.iter().find(|worktree| worktree.is_at(path)));
    after_interruption(Ok(entry))
}

/// How long `git worktree add` may run: what is left of the budget minus
/// [`MIN_REGISTRATION_BUDGET`], which stays for the cleanup and the
/// registration; `None` when nothing is left.
fn add_timeout(remaining: Duration) -> Option<Duration> {
    let timeout = remaining.saturating_sub(MIN_REGISTRATION_BUDGET);
    (!timeout.is_zero()).then_some(timeout)
}

/// Best effort: removes the folder and the git record of a worktree whose
/// creation was cut short, and the branch when that creation made it. Failures
/// are noted on stderr; the caller returns the original failure.
fn clean_up_interrupted(
    main_root: &Path,
    target: &Path,
    created_branch: Option<&str>,
    deadline: Deadline,
) {
    let step_timeout = |deadline: Deadline| deadline.clamp(CLEANUP_STEP_TIMEOUT);
    let removed = step_timeout(deadline).map_or(
        Err(WorktreeGitError::from(CheckoutError::Budget(
            BudgetExhausted,
        ))),
        |timeout| remove_interrupted_worktree(main_root, target, timeout),
    );
    if let Err(error) = removed {
        eprintln!(
            "ds: warning: could not remove the half-created worktree {}: {error}",
            target.display()
        );
    }
    let Some(branch) = created_branch else {
        return;
    };
    let deleted = step_timeout(deadline)
        .map_or(Err(CheckoutError::Budget(BudgetExhausted)), |timeout| {
            delete_branch_within(main_root, branch, timeout)
        });
    if let Err(error) = deleted {
        eprintln!("ds: warning: could not delete the branch {branch}: {error}");
    }
}

/// Whether enough of the budget is left to register the worktree.
fn has_budget_to_register(remaining: Duration) -> bool {
    remaining >= MIN_REGISTRATION_BUDGET
}

fn register_worktree(
    path: &Path,
    (machine, global): (&Machine, &GlobalConfig),
    deadline: Deadline,
) {
    if !has_budget_to_register(deadline.remaining()) {
        eprintln!(
            "ds: warning: not enough time left to register {}; the first agent command will register it",
            path.display()
        );
        return;
    }
    let result = if is_registered(path, machine) {
        refresh_registered(path, deadline)
    } else {
        try_register(path, (machine, global), deadline)
    };
    if let Err(error) = result {
        eprintln!(
            "ds: warning: could not register {}: {error}; the worktree was created, run ds up in it to try again",
            path.display()
        );
    }
}

fn is_registered(path: &Path, machine: &Machine) -> bool {
    machine
        .store
        .read()
        .is_ok_and(|registry| registry.find(path).is_some())
}

fn refresh_registered(path: &Path, deadline: Deadline) -> Result<(), ToolError> {
    match ensure_directory_within(path, PLAIN_STYLE, Some(deadline))? {
        EnsureResult::Silent => {}
        EnsureResult::Done { lines, .. } | EnsureResult::Refused { lines } => {
            for line in lines {
                eprintln!("{line}");
            }
        }
    }
    Ok(())
}

fn try_register(
    path: &Path,
    (machine, global): (&Machine, &GlobalConfig),
    deadline: Deadline,
) -> Result<(), ToolError> {
    let checkout = Checkout::locate_within(path, deadline)?;
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
    let outcome = register_checkout_within(
        &checkout,
        machine,
        global,
        Intent::Explicit,
        (PROBE_TIMEOUT, Some(deadline)),
    )?;
    match outcome {
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
    fn should_register_when_the_remaining_budget_is_at_least_the_minimum() {
        assert!(has_budget_to_register(MIN_REGISTRATION_BUDGET));
        assert!(has_budget_to_register(WORKTREE_CREATE_BUDGET));
    }

    #[test]
    fn should_skip_registration_when_the_remaining_budget_is_below_the_minimum() {
        let just_below = Duration::from_secs(19);

        assert!(!has_budget_to_register(just_below));
        assert!(!has_budget_to_register(Duration::ZERO));
    }

    fn git(directory: &Path, arguments: &[&str]) {
        let status = std::process::Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?}");
    }

    fn repository_with_commit() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        git(directory.path(), &["init", "-q", "-b", "main"]);
        git(
            directory.path(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "first",
            ],
        );
        directory
    }

    fn branch_listed(main_root: &Path, name: &str) -> bool {
        let output = std::process::Command::new("git")
            .args(["branch", "--list", name])
            .current_dir(main_root)
            .output()
            .unwrap();
        !output.stdout.is_empty()
    }

    #[test]
    fn should_leave_the_budget_minus_the_registration_minimum_to_the_add() {
        assert_eq!(
            add_timeout(Duration::from_secs(150)),
            Some(Duration::from_secs(130))
        );
        assert_eq!(add_timeout(MIN_REGISTRATION_BUDGET), None);
        assert_eq!(add_timeout(Duration::from_secs(5)), None);
        assert_eq!(add_timeout(Duration::ZERO), None);
    }

    fn add_locked_worktree(main_root: &Path, target: &Path, reason: &str) {
        let path = target.to_str().unwrap();
        git(main_root, &["worktree", "add", "-q", "-b", "feature", path]);
        git(main_root, &["worktree", "lock", "--reason", reason, path]);
    }

    #[test]
    fn should_refuse_as_half_created_when_reopening_a_worktree_locked_while_initializing() {
        let repository = repository_with_commit();
        let main_root = repository.path().canonicalize().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let target = worktrees.path().join("feature");
        add_locked_worktree(&main_root, &target, INITIALIZING_LOCK_REASON);
        let deadline = Deadline::starting_now(WORKTREE_CREATE_BUDGET);

        let result = reopen_worktree(&target, &main_root, deadline);

        assert!(matches!(result, Err(HookFailure::HalfCreated(_))));
    }

    #[test]
    fn should_refuse_as_locked_when_reopening_a_worktree_locked_for_another_reason() {
        let repository = repository_with_commit();
        let main_root = repository.path().canonicalize().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let target = worktrees.path().join("feature");
        add_locked_worktree(&main_root, &target, "on a usb drive");
        let deadline = Deadline::starting_now(WORKTREE_CREATE_BUDGET);

        let result = reopen_worktree(&target, &main_root, deadline);

        assert!(matches!(result, Err(HookFailure::Locked(_))));
    }

    #[test]
    fn should_reopen_an_unlocked_worktree() {
        let repository = repository_with_commit();
        let main_root = repository.path().canonicalize().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let target = worktrees.path().join("feature");
        git(
            &main_root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature",
                target.to_str().unwrap(),
            ],
        );
        let deadline = Deadline::starting_now(WORKTREE_CREATE_BUDGET);

        let reopened = reopen_worktree(&target, &main_root, deadline).unwrap();

        assert_eq!(reopened, target.canonicalize().unwrap());
    }

    #[test]
    fn should_keep_the_worktree_when_the_add_is_cut_short_in_the_post_checkout_hook() {
        use std::os::unix::fs::PermissionsExt;
        let repository = repository_with_commit();
        let main_root = repository.path().canonicalize().unwrap();
        let hook = main_root.join(".git/hooks/post-checkout");
        std::fs::write(&hook, "#!/bin/sh\nexec sleep 3\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let target = worktrees.path().join("feature");
        let deadline = Deadline::starting_now(MIN_REGISTRATION_BUDGET + Duration::from_secs(1));

        let result = add_worktree(&main_root, &target, "feature", deadline);

        assert!(result.is_ok());
        assert!(target.is_dir());
        assert!(branch_listed(&main_root, "feature"));
    }

    #[test]
    fn should_remove_the_worktree_and_the_new_branch_when_cleaning_up() {
        let repository = repository_with_commit();
        let main_root = repository.path().canonicalize().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let target = worktrees.path().join("feature");
        add_locked_worktree(&main_root, &target, INITIALIZING_LOCK_REASON);
        let deadline = Deadline::starting_now(WORKTREE_CREATE_BUDGET);

        clean_up_interrupted(&main_root, &target, Some("feature"), deadline);

        assert!(!target.exists());
        assert!(!branch_listed(&main_root, "feature"));
    }

    fn listed(is_locked: bool) -> ListedWorktree {
        ListedWorktree {
            path: PathBuf::from("/wt/feature"),
            is_locked,
            lock_reason: is_locked.then(|| INITIALIZING_LOCK_REASON.to_owned()),
        }
    }

    #[test]
    fn should_keep_the_worktree_when_it_is_listed_and_unlocked() {
        assert_eq!(
            after_interruption(Ok(Some(&listed(false)))),
            InterruptionAction::Keep
        );
    }

    #[test]
    fn should_leave_everything_and_fail_when_the_listing_failed() {
        assert_eq!(
            after_interruption(Err(())),
            InterruptionAction::LeaveAndFail
        );
    }

    #[test]
    fn should_clean_up_when_the_worktree_is_locked_or_not_listed() {
        assert_eq!(
            after_interruption(Ok(Some(&listed(true)))),
            InterruptionAction::CleanUp
        );
        assert_eq!(after_interruption(Ok(None)), InterruptionAction::CleanUp);
    }

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
