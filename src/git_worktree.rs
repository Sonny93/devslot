//! The git operations behind the worktree hooks. Each runs from the root of
//! the main worktree, so it works whatever the current folder is.

use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

use crate::checkout::{CheckoutError, git_failure, run_git_within};
use crate::deadline::{BudgetExhausted, Deadline, has_run_out, timeout_within};
use crate::process::CapturedOutput;

const WORKTREE_TIMEOUT: Duration = Duration::from_secs(120);
const QUICK_TIMEOUT: Duration = Duration::from_secs(10);
const BRANCH_FOUND_EXIT_CODE: i32 = 0;
const BRANCH_MISSING_EXIT_CODE: i32 = 1;
const ANCESTOR_EXIT_CODE: i32 = 0;
const NOT_ANCESTOR_EXIT_CODE: i32 = 1;
const MINIMUM_REMOVE_TIME: Duration = Duration::from_secs(10);
const STATUS_TIMEOUT: Duration = Duration::from_secs(60);
const WORKTREE_LINE_PREFIX: &str = "worktree ";
const LOCKED_MARKER: &str = "locked";
const UNTRACKED_PREFIX: &[u8] = b"?? ";
const CONFIG_KEY_MISSING_EXIT_CODE: i32 = 1;
const BASE_CONFIG_NAME: &str = "devslotBase";
const FILE_ENTRY_MODES: [&[u8]; 3] = [b"100644", b"100755", b"120000"];
const SHORT_COMMIT_LENGTH: &str = "--short=7";
const OPERATION_MARKERS: [(&str, &str); 7] = [
    ("MERGE_HEAD", "a merge"),
    ("rebase-merge", "a rebase"),
    ("rebase-apply", "a rebase"),
    ("CHERRY_PICK_HEAD", "a cherry-pick"),
    ("REVERT_HEAD", "a revert"),
    ("BISECT_LOG", "a bisect"),
    ("sequencer", "a cherry-pick or revert sequence"),
];

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

impl WorktreeGitError {
    /// Whether git was cut short, by its timeout or by the budget, rather than
    /// refusing: what it was doing may be half done.
    pub const fn is_interruption(&self) -> bool {
        matches!(
            self,
            Self::Tool(CheckoutError::GitTimedOut { .. } | CheckoutError::Budget(_))
        )
    }
}

/// The branch checked out in `directory`; `None` when `HEAD` is detached.
pub fn current_branch(directory: &Path) -> Result<Option<String>, CheckoutError> {
    current_branch_within(directory, None)
}

fn current_branch_within(
    directory: &Path,
    deadline: Option<Deadline>,
) -> Result<Option<String>, CheckoutError> {
    let output = run_git_within(
        directory,
        ["symbolic-ref", "--short", "HEAD"],
        quick_timeout(deadline)?,
    )?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(trimmed_stdout(&output)))
}

/// What a new branch starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseReference {
    /// The branch checked out in the main worktree.
    Branch(String),
    /// The `HEAD` commit of the main worktree, when it is detached.
    Commit(String),
}

impl BaseReference {
    /// The branch name or the commit id, as git takes it.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Branch(reference) | Self::Commit(reference) => reference,
        }
    }
}

/// What a new branch starts from: the branch checked out in the main
/// worktree, or its `HEAD` commit when that is detached. Every git call stops
/// at `deadline` when there is one.
pub fn base_reference(
    main_root: &Path,
    deadline: Option<Deadline>,
) -> Result<BaseReference, WorktreeGitError> {
    if let Some(branch) = current_branch_within(main_root, deadline)? {
        return Ok(BaseReference::Branch(branch));
    }
    let output = run_git_within(main_root, ["rev-parse", "HEAD"], quick_timeout(deadline)?)?;
    if !output.status.success() {
        forward_output(&output);
        return Err(WorktreeGitError::Rejected {
            command: "rev-parse",
        });
    }
    Ok(BaseReference::Commit(trimmed_stdout(&output)))
}

/// Whether the local branch `name` exists. The git call stops at `deadline`
/// when there is one.
pub fn branch_exists(
    main_root: &Path,
    name: &str,
    deadline: Option<Deadline>,
) -> Result<bool, CheckoutError> {
    let reference = format!("refs/heads/{name}");
    let output = run_git_within(
        main_root,
        ["show-ref", "--verify", "--quiet", reference.as_str()],
        quick_timeout(deadline)?,
    )?;
    match output.status.code() {
        Some(BRANCH_FOUND_EXIT_CODE) => Ok(true),
        Some(BRANCH_MISSING_EXIT_CODE) => Ok(false),
        _ => Err(git_failure("show-ref", &output)),
    }
}

/// `git worktree add <path> <branch>`: checks out the existing `branch`. The
/// git call is killed after `timeout`.
pub fn add_on_existing_branch(
    main_root: &Path,
    path: &Path,
    branch: &str,
    timeout: Duration,
) -> Result<(), WorktreeGitError> {
    run_forwarded(
        main_root,
        timeout,
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
/// The git call is killed after `timeout`.
pub fn add_on_new_branch(
    main_root: &Path,
    path: &Path,
    branch: &str,
    base: &str,
    timeout: Duration,
) -> Result<(), WorktreeGitError> {
    run_forwarded(
        main_root,
        timeout,
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
/// worktree, even with local changes. With a `deadline`, the call is clamped to
/// what is left of it, and nothing is run when no more than
/// [`MINIMUM_REMOVE_TIME`] is left: git killed half way leaves a half-removed
/// worktree.
pub fn remove_worktree(
    main_root: &Path,
    path: &Path,
    deadline: Option<Deadline>,
) -> Result<(), WorktreeGitError> {
    let timeout = match deadline {
        None => WORKTREE_TIMEOUT,
        Some(deadline) => deadline
            .clamp_leaving(WORKTREE_TIMEOUT, MINIMUM_REMOVE_TIME)
            .ok_or(CheckoutError::Budget(BudgetExhausted))?,
    };
    run_forwarded(
        main_root,
        timeout,
        [
            OsStr::new("worktree"),
            OsStr::new("remove"),
            OsStr::new("--force"),
            path.as_os_str(),
        ],
        "worktree remove",
    )
}

/// `git worktree remove --force --force <path>`: like [`remove_worktree`],
/// and it removes a locked worktree too, such as one whose creation was
/// interrupted. The git call is killed after `timeout`.
pub fn remove_interrupted_worktree(
    main_root: &Path,
    path: &Path,
    timeout: Duration,
) -> Result<(), WorktreeGitError> {
    run_forwarded(
        main_root,
        timeout,
        [
            OsStr::new("worktree"),
            OsStr::new("remove"),
            OsStr::new("--force"),
            OsStr::new("--force"),
            path.as_os_str(),
        ],
        "worktree remove",
    )
}

/// A worktree as `git worktree list --porcelain` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedWorktree {
    /// The path git has recorded.
    pub path: PathBuf,
    /// Whether the worktree is locked.
    pub is_locked: bool,
    /// Why it is locked, when the lock has a reason. Git runs here with
    /// `LC_ALL=C`, so the reason git writes itself is not translated.
    pub lock_reason: Option<String>,
}

impl ListedWorktree {
    /// Whether this entry is the worktree at the canonical `path`.
    pub fn is_at(&self, path: &Path) -> bool {
        self.path == path
            || self
                .path
                .canonicalize()
                .is_ok_and(|canonical| canonical == path)
    }
}

/// The worktrees of the repository of `main_root`, as git lists them now.
pub fn list_worktrees(main_root: &Path) -> Result<Vec<ListedWorktree>, CheckoutError> {
    list_worktrees_within(main_root, QUICK_TIMEOUT)
}

/// Like [`list_worktrees`], with the git call killed after `timeout`.
pub fn list_worktrees_within(
    main_root: &Path,
    timeout: Duration,
) -> Result<Vec<ListedWorktree>, CheckoutError> {
    let output = run_git_within(main_root, ["worktree", "list", "--porcelain"], timeout)?;
    if !output.status.success() {
        return Err(git_failure("worktree", &output));
    }
    Ok(parse_worktree_listing(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn parse_worktree_listing(listing: &str) -> Vec<ListedWorktree> {
    listing
        .split("\n\n")
        .filter_map(|entry| {
            let path = entry
                .lines()
                .find_map(|line| line.strip_prefix(WORKTREE_LINE_PREFIX))?;
            let lock_line = entry
                .lines()
                .find(|line| *line == LOCKED_MARKER || line.starts_with("locked "));
            let lock_reason = lock_line
                .and_then(|line| line.strip_prefix("locked "))
                .map(str::to_owned);
            Some(ListedWorktree {
                path: PathBuf::from(path),
                is_locked: lock_line.is_some(),
                lock_reason,
            })
        })
        .collect()
}

/// How many lines `git status --porcelain` prints in `worktree`: the changed
/// and the untracked files, ignored ones not counted. With a `deadline`, the
/// call stops at it with [`CheckoutError::Budget`].
pub fn uncommitted_change_count(
    worktree: &Path,
    deadline: Option<Deadline>,
) -> Result<usize, CheckoutError> {
    let timeout = timeout_within(deadline, STATUS_TIMEOUT)?;
    let output = run_git_within(
        worktree,
        ["--no-optional-locks", "status", "--porcelain"],
        timeout,
    )
    .map_err(|error| timeout_as_budget(error, deadline))?;
    if !output.status.success() {
        return Err(git_failure("status", &output));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .count())
}

/// How many commits are reachable from the `HEAD` of `worktree` and from no
/// other branch, tag or remote branch. With `branch_to_delete`, that branch
/// does not count as a place the commits are kept, since it is deleted too.
/// With a `deadline`, the call stops at it with [`CheckoutError::Budget`].
pub fn unreachable_commit_count(
    worktree: &Path,
    branch_to_delete: Option<&str>,
    deadline: Option<Deadline>,
) -> Result<usize, CheckoutError> {
    let exclusion = branch_to_delete.map(|branch| format!("--exclude={branch}"));
    let mut arguments = vec!["rev-list", "--count", "HEAD", "--not"];
    arguments.extend(exclusion.as_deref());
    arguments.extend(["--branches", "--tags", "--remotes"]);
    let output = run_git_within(worktree, arguments, quick_timeout(deadline)?)
        .map_err(|error| timeout_as_budget(error, deadline))?;
    if !output.status.success() {
        return Err(git_failure("rev-list", &output));
    }
    trimmed_stdout(&output)
        .parse()
        .map_err(|_| git_failure("rev-list", &output))
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
    delete_branch_within(main_root, branch, QUICK_TIMEOUT)
}

/// Like [`delete_branch`], with the git call killed after `timeout`.
pub fn delete_branch_within(
    main_root: &Path,
    branch: &str,
    timeout: Duration,
) -> Result<(), CheckoutError> {
    let output = run_git_within(
        main_root,
        ["branch", "-D", "--end-of-options", branch],
        timeout,
    )?;
    if output.status.success() {
        return Ok(());
    }
    Err(git_failure("branch", &output))
}

/// What `git status --porcelain` finds in a worktree, ignored files left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorktreeStatus {
    /// Files with staged or unstaged changes to tracked content.
    pub tracked_changes: usize,
    /// Files git does not track and does not ignore.
    pub untracked_files: usize,
}

/// The changed and the untracked files of `worktree`, counted apart.
pub fn worktree_status(worktree: &Path) -> Result<WorktreeStatus, CheckoutError> {
    let output = run_git_within(
        worktree,
        ["--no-optional-locks", "status", "--porcelain"],
        STATUS_TIMEOUT,
    )?;
    if !output.status.success() {
        return Err(git_failure("status", &output));
    }
    let lines = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty());
    let (untracked, tracked): (Vec<_>, Vec<_>) =
        lines.partition(|line| line.starts_with(UNTRACKED_PREFIX));
    Ok(WorktreeStatus {
        tracked_changes: tracked.len(),
        untracked_files: untracked.len(),
    })
}

/// The value of the git config `key` seen from `directory`; `None` when unset.
pub fn read_config(directory: &Path, key: &str) -> Result<Option<String>, CheckoutError> {
    let output = run_git_within(
        directory,
        ["config", "--get", "--end-of-options", key],
        QUICK_TIMEOUT,
    )?;
    if output.status.success() {
        return Ok(Some(trimmed_stdout(&output)));
    }
    match output.status.code() {
        Some(CONFIG_KEY_MISSING_EXIT_CODE) => Ok(None),
        _ => Err(git_failure("config", &output)),
    }
}

/// The git config key that holds the branch `branch` was created from.
pub fn base_config_key(branch: &str) -> String {
    format!("branch.{branch}.{BASE_CONFIG_NAME}")
}

/// Records in the git config that `branch` was created from `base`. The git
/// call stops at `deadline` when there is one.
pub fn record_branch_base(
    main_root: &Path,
    (branch, base): (&str, &str),
    deadline: Option<Deadline>,
) -> Result<(), CheckoutError> {
    let key = base_config_key(branch);
    let output = run_git_within(
        main_root,
        ["config", "--end-of-options", key.as_str(), base],
        quick_timeout(deadline)?,
    )?;
    if output.status.success() {
        return Ok(());
    }
    Err(git_failure("config", &output))
}

/// How many commits are reachable from `to` and not from `from`. The git call
/// stops at `deadline` when there is one.
pub fn commits_between(
    directory: &Path,
    (from, to): (&str, &str),
    deadline: Option<Deadline>,
) -> Result<usize, CheckoutError> {
    let range = format!("{from}..{to}");
    let output = run_git_within(
        directory,
        ["rev-list", "--count", range.as_str(), "--"],
        quick_timeout(deadline)?,
    )?;
    if !output.status.success() {
        return Err(git_failure("rev-list", &output));
    }
    trimmed_stdout(&output)
        .parse()
        .map_err(|_| git_failure("rev-list", &output))
}

/// The paths, relative to the root of `checkout_root`, that `to` adds to
/// `from` (no renames: a renamed file counts as added at its new path).
pub fn added_paths(
    checkout_root: &Path,
    (from, to): (&str, &str),
) -> Result<Vec<PathBuf>, CheckoutError> {
    let output = run_git_within(
        checkout_root,
        [
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            "--diff-filter=A",
            from,
            to,
            "--",
        ],
        QUICK_TIMEOUT,
    )?;
    if !output.status.success() {
        return Err(git_failure("diff", &output));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| PathBuf::from(OsStr::from_bytes(name)))
        .collect())
}

/// Whether the index of `checkout_root` holds `path` (relative to its root)
/// as exactly one regular file or symlink entry: git then checks that file
/// for local changes itself before replacing it. A folder, a missing path, a
/// gitlink or a conflicted entry is not.
pub fn is_tracked_file_in_index(checkout_root: &Path, path: &Path) -> Result<bool, CheckoutError> {
    let output = run_git_within(
        checkout_root,
        [
            OsStr::new("--literal-pathspecs"),
            OsStr::new("ls-files"),
            OsStr::new("-z"),
            OsStr::new("-s"),
            OsStr::new("--"),
            path.as_os_str(),
        ],
        QUICK_TIMEOUT,
    )?;
    if !output.status.success() {
        return Err(git_failure("ls-files", &output));
    }
    let mut entries = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty());
    let (Some(entry), None) = (entries.next(), entries.next()) else {
        return Ok(false);
    };
    Ok(is_file_entry(entry, path))
}

fn is_file_entry(entry: &[u8], path: &Path) -> bool {
    let Some(tab) = entry.iter().position(|byte| *byte == b'\t') else {
        return false;
    };
    let (fields, listed_path) = entry.split_at(tab);
    let mode = fields
        .split(|byte| *byte == b' ')
        .next()
        .unwrap_or_default();
    FILE_ENTRY_MODES.contains(&mode) && &listed_path[1..] == path.as_os_str().as_bytes()
}

/// Whether the commit `ancestor` is reachable from `descendant` (or equal).
pub fn is_ancestor(
    directory: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, CheckoutError> {
    let output = run_git_within(
        directory,
        ["merge-base", "--is-ancestor", ancestor, descendant],
        QUICK_TIMEOUT,
    )?;
    match output.status.code() {
        Some(ANCESTOR_EXIT_CODE) => Ok(true),
        Some(NOT_ANCESTOR_EXIT_CODE) => Ok(false),
        _ => Err(git_failure("merge-base", &output)),
    }
}

/// The abbreviated commit id `reference` points to.
pub fn short_commit(directory: &Path, reference: &str) -> Result<String, CheckoutError> {
    let output = run_git_within(
        directory,
        ["rev-parse", SHORT_COMMIT_LENGTH, "--verify", reference],
        QUICK_TIMEOUT,
    )?;
    if !output.status.success() {
        return Err(git_failure("rev-parse", &output));
    }
    Ok(trimmed_stdout(&output))
}

/// What the operation in progress in `checkout_root` is (a merge, a rebase,
/// ...); `None` when there is none.
pub fn operation_in_progress(checkout_root: &Path) -> Result<Option<&'static str>, CheckoutError> {
    let output = run_git_within(
        checkout_root,
        ["rev-parse", "--absolute-git-dir"],
        QUICK_TIMEOUT,
    )?;
    if !output.status.success() {
        return Err(git_failure("rev-parse", &output));
    }
    let git_directory = PathBuf::from(trimmed_stdout(&output));
    Ok(OPERATION_MARKERS
        .iter()
        .find(|(marker, _)| git_directory.join(marker).exists())
        .map(|(_, description)| *description))
}

/// `git merge --ff-only <branch>` in `checkout_root`: moves its branch
/// forward, never creates a merge commit. Git's own output goes to stderr.
pub fn fast_forward_onto(checkout_root: &Path, branch: &str) -> Result<(), WorktreeGitError> {
    let reference = format!("refs/heads/{branch}");
    run_forwarded(
        checkout_root,
        WORKTREE_TIMEOUT,
        [
            OsStr::new("merge"),
            OsStr::new("--ff-only"),
            OsStr::new("--quiet"),
            OsStr::new(reference.as_str()),
        ],
        "merge",
    )
}

fn timeout_as_budget(error: CheckoutError, deadline: Option<Deadline>) -> CheckoutError {
    match error {
        CheckoutError::GitTimedOut { .. } if has_run_out(deadline) => {
            CheckoutError::Budget(BudgetExhausted)
        }
        other => other,
    }
}

fn quick_timeout(deadline: Option<Deadline>) -> Result<Duration, CheckoutError> {
    Ok(timeout_within(deadline, QUICK_TIMEOUT)?)
}

fn run_forwarded<const COUNT: usize>(
    main_root: &Path,
    timeout: Duration,
    arguments: [&OsStr; COUNT],
    command: &'static str,
) -> Result<(), WorktreeGitError> {
    let output = run_git_within(main_root, arguments, timeout)?;
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

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    const LISTING: &str = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\nworktree /wt/a\nHEAD def\nbranch refs/heads/a\nlocked\n\nworktree /wt/b\nHEAD 123\ndetached\nlocked in use by a session\n\nworktree /wt/c\nHEAD 456\nbranch refs/heads/c\nprunable gitdir file points to non-existent location\n";

    #[test]
    fn should_read_the_path_and_the_lock_of_every_worktree() {
        let listed = parse_worktree_listing(LISTING);

        let summary: Vec<(String, bool)> = listed
            .iter()
            .map(|entry| (entry.path.display().to_string(), entry.is_locked))
            .collect();
        assert_eq!(
            summary,
            [
                ("/repo".to_owned(), false),
                ("/wt/a".to_owned(), true),
                ("/wt/b".to_owned(), true),
                ("/wt/c".to_owned(), false),
            ]
        );
    }

    #[test]
    fn should_read_the_reason_of_a_lock_when_there_is_one() {
        let listed = parse_worktree_listing(LISTING);

        let reasons: Vec<Option<&str>> = listed
            .iter()
            .map(|entry| entry.lock_reason.as_deref())
            .collect();
        assert_eq!(reasons, [None, None, Some("in use by a session"), None]);
    }

    #[test]
    fn should_not_read_a_lock_into_a_word_that_only_starts_like_it() {
        let listed =
            parse_worktree_listing("worktree /wt/a\nHEAD d\nbranch refs/heads/lockedness\n");

        assert_eq!(listed[0].is_locked, false);
    }

    #[test]
    fn should_match_an_entry_by_its_recorded_path_or_its_canonical_path() {
        let folder = tempfile::tempdir().unwrap();
        let canonical = folder.path().canonicalize().unwrap();
        let entry = ListedWorktree {
            path: folder.path().join("."),
            is_locked: false,
            lock_reason: None,
        };

        assert!(entry.is_at(&canonical));
        assert!(!entry.is_at(&canonical.join("other")));
    }
}
