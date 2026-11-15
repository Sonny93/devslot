//! The git operations behind the worktree hooks. Each runs from the root of
//! the main worktree, so it works whatever the current folder is.

use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use thiserror::Error;

use crate::checkout::{CheckoutError, git_failure, run_git_within};
use crate::process::CapturedOutput;

const WORKTREE_TIMEOUT: Duration = Duration::from_secs(120);
const QUICK_TIMEOUT: Duration = Duration::from_secs(10);
const BRANCH_FOUND_EXIT_CODE: i32 = 0;
const BRANCH_MISSING_EXIT_CODE: i32 = 1;
const ANCESTOR_EXIT_CODE: i32 = 0;
const NOT_ANCESTOR_EXIT_CODE: i32 = 1;

/// Why a git operation of a worktree hook did not complete.
#[derive(Debug, Error)]
pub enum WorktreeGitError {
    /// Git could not be run, or did not answer in time.
    #[error(transparent)]
    Tool(#[from] CheckoutError),
    /// Git ran and refused; its own output was already shown.
    #[error("git {command} failed")]
    Rejected {
        /// The git subcommand.
        command: &'static str,
    },
}

/// The branch checked out in `directory`; `None` when `HEAD` is detached.
pub fn current_branch(directory: &Path) -> Result<Option<String>, CheckoutError> {
    let output = run_git_within(
        directory,
        ["symbolic-ref", "--short", "HEAD"],
        QUICK_TIMEOUT,
    )?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(trimmed_stdout(&output)))
}

/// What a new branch starts from: the branch checked out in the main
/// worktree, or its `HEAD` commit when that is detached.
pub fn base_reference(main_root: &Path) -> Result<String, WorktreeGitError> {
    if let Some(branch) = current_branch(main_root)? {
        return Ok(branch);
    }
    let output = run_git_within(main_root, ["rev-parse", "HEAD"], QUICK_TIMEOUT)?;
    if !output.status.success() {
        forward_output(&output);
        return Err(WorktreeGitError::Rejected {
            command: "rev-parse",
        });
    }
    Ok(trimmed_stdout(&output))
}

/// Whether the local branch `name` exists.
pub fn branch_exists(main_root: &Path, name: &str) -> Result<bool, CheckoutError> {
    let reference = format!("refs/heads/{name}");
    let output = run_git_within(
        main_root,
        ["show-ref", "--verify", "--quiet", reference.as_str()],
        QUICK_TIMEOUT,
    )?;
    match output.status.code() {
        Some(BRANCH_FOUND_EXIT_CODE) => Ok(true),
        Some(BRANCH_MISSING_EXIT_CODE) => Ok(false),
        _ => Err(git_failure("show-ref", &output)),
    }
}

/// `git worktree add <path> <branch>`: checks out the existing `branch`.
pub fn add_on_existing_branch(
    main_root: &Path,
    path: &Path,
    branch: &str,
) -> Result<(), WorktreeGitError> {
    run_forwarded(
        main_root,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            path.as_os_str(),
            OsStr::new(branch),
        ],
        "worktree add",
    )
}

/// `git worktree add -b <branch> <path> <base>`: creates `branch` from `base`.
pub fn add_on_new_branch(
    main_root: &Path,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<(), WorktreeGitError> {
    run_forwarded(
        main_root,
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new(branch),
            path.as_os_str(),
            OsStr::new(base),
        ],
        "worktree add",
    )
}

/// `git worktree remove --force <path>`: deletes the folder and forgets the
/// worktree, even with local changes.
pub fn remove_worktree(main_root: &Path, path: &Path) -> Result<(), WorktreeGitError> {
    run_forwarded(
        main_root,
        [
            OsStr::new("worktree"),
            OsStr::new("remove"),
            OsStr::new("--force"),
            path.as_os_str(),
        ],
        "worktree remove",
    )
}

/// Whether the local branch `branch` is an ancestor of the `HEAD` of the
/// main worktree, that is merged into it (not into an upstream).
pub fn is_branch_merged(main_root: &Path, branch: &str) -> Result<bool, CheckoutError> {
    let reference = format!("refs/heads/{branch}");
    let output = run_git_within(
        main_root,
        ["merge-base", "--is-ancestor", reference.as_str(), "HEAD"],
        QUICK_TIMEOUT,
    )?;
    match output.status.code() {
        Some(ANCESTOR_EXIT_CODE) => Ok(true),
        Some(NOT_ANCESTOR_EXIT_CODE) => Ok(false),
        _ => Err(git_failure("merge-base", &output)),
    }
}

/// `git branch -D --end-of-options <branch>`. The caller has checked that the
/// branch is merged, which `-d` would check against the upstream too.
pub fn delete_branch(main_root: &Path, branch: &str) -> Result<(), CheckoutError> {
    let output = run_git_within(
        main_root,
        ["branch", "-D", "--end-of-options", branch],
        QUICK_TIMEOUT,
    )?;
    if output.status.success() {
        return Ok(());
    }
    Err(git_failure("branch", &output))
}

fn run_forwarded<const COUNT: usize>(
    main_root: &Path,
    arguments: [&OsStr; COUNT],
    command: &'static str,
) -> Result<(), WorktreeGitError> {
    let output = run_git_within(main_root, arguments, WORKTREE_TIMEOUT)?;
    forward_output(&output);
    if output.status.success() {
        return Ok(());
    }
    Err(WorktreeGitError::Rejected { command })
}

fn forward_output(output: &CapturedOutput) {
    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(&output.stdout);
    let _ = stderr.write_all(&output.stderr);
}

fn trimmed_stdout(output: &CapturedOutput) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}
