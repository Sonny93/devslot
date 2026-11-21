//! The checkout: its root, whether git manages it, its main worktree, and
//! whether git ignores its `.env`.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use thiserror::Error;

use crate::deadline::{BudgetExhausted, Deadline, timeout_within};
use crate::instance_name::repository_name_from_common_dir;
use crate::process::{CaptureFailure, CapturedOutput, capture_with_timeout};
use crate::registry::record::CheckoutOrigin;

const GIT_PROGRAM: &str = "git";
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
const DISABLE_FSMONITOR_ARGUMENTS: [&str; 2] = ["-c", "core.fsmonitor=false"];
const NOT_A_REPOSITORY_MESSAGE: &str = "not a git repository";
const WORKTREE_PREFIX: &str = "worktree ";
const BARE_MARKER: &str = "bare";
const IGNORED_EXIT_CODE: i32 = 0;
const NOT_IGNORED_EXIT_CODE: i32 = 1;
const TRACKED_EXIT_CODE: i32 = 0;
const NOT_TRACKED_EXIT_CODE: i32 = 1;

/// Why the checkout cannot be examined. These are tool failures.
#[derive(Debug, Error)]
pub enum CheckoutError {
    /// The current folder cannot be resolved.
    #[error("cannot resolve {path}: {source}")]
    Resolve {
        /// The path.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// Git could not be run.
    #[error("cannot run git: {0}")]
    GitUnavailable(io::Error),
    /// Git did not answer in time and was killed.
    #[error("git did not answer within {seconds} seconds")]
    GitTimedOut {
        /// The time limit.
        seconds: u64,
    },
    /// Git ran and failed for another reason than "not a repository".
    #[error("git {command} failed: {reason}")]
    GitFailed {
        /// The git subcommand.
        command: &'static str,
        /// The first line git printed on stderr, or its exit status.
        reason: String,
    },
    /// The time budget of the command ran out.
    #[error(transparent)]
    Budget(#[from] BudgetExhausted),
}

/// A folder that holds a working copy of the repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    /// The canonical root: the git top level, or the canonical current folder.
    pub root: PathBuf,
    /// Whether the root is inside a git repository.
    pub is_git: bool,
    /// The main worktree, when it is a different folder than `root`.
    pub main_root: Option<PathBuf>,
    deadline: Option<Deadline>,
}

impl Checkout {
    /// Finds the checkout that contains `directory`.
    pub fn locate(directory: &Path) -> Result<Self, CheckoutError> {
        Self::locate_with(directory, None)
    }

    /// Like [`Checkout::locate`], and every git call of the checkout, now and
    /// later, stops at `deadline` with [`CheckoutError::Budget`].
    pub fn locate_within(directory: &Path, deadline: Deadline) -> Result<Self, CheckoutError> {
        Self::locate_with(directory, Some(deadline))
    }

    fn locate_with(directory: &Path, deadline: Option<Deadline>) -> Result<Self, CheckoutError> {
        let canonical = canonicalize(directory)?;
        let Some(top_level) = git_top_level(&canonical, deadline)? else {
            return Ok(Self {
                root: canonical,
                is_git: false,
                main_root: None,
                deadline,
            });
        };
        let root = canonicalize(&top_level)?;
        let main_root = main_worktree(&root, deadline)?.filter(|main| main != &root);
        Ok(Self {
            root,
            is_git: true,
            main_root,
            deadline,
        })
    }

    /// The name of the repository this checkout belongs to: the same for every
    /// worktree. Outside git, the name of the folder.
    pub fn repository_name(&self) -> Result<String, CheckoutError> {
        if !self.is_git {
            return Ok(folder_name(&self.root));
        }
        let output = run_git(
            &self.root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            self.deadline,
        )?;
        if !output.status.success() {
            return Err(git_failure("rev-parse", &output));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(repository_name_from_common_dir(Path::new(
            text.trim_end_matches('\n'),
        )))
    }

    /// Where the checkout comes from: the repository name and whether git
    /// counts it as a linked worktree, from one git call. Outside git, the
    /// name of the folder and not a worktree.
    pub fn origin(&self) -> Result<CheckoutOrigin, CheckoutError> {
        if !self.is_git {
            return Ok(CheckoutOrigin {
                project: folder_name(&self.root),
                is_worktree: false,
            });
        }
        let output = run_git(
            &self.root,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-dir",
                "--git-common-dir",
            ],
            self.deadline,
        )?;
        if !output.status.success() {
            return Err(git_failure("rev-parse", &output));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut lines = text.lines();
        let (Some(git_dir), Some(common_dir)) = (lines.next(), lines.next()) else {
            return Err(CheckoutError::GitFailed {
                command: "rev-parse",
                reason: "no git directory in its output".to_owned(),
            });
        };
        Ok(CheckoutOrigin {
            project: repository_name_from_common_dir(Path::new(common_dir)),
            is_worktree: git_dir != common_dir,
        })
    }

    /// Whether git tracks `file`, given relative to the root. Only meaningful
    /// inside git.
    pub fn is_tracked(&self, file: &str) -> Result<bool, CheckoutError> {
        let output = run_git(
            &self.root,
            &["ls-files", "--error-unmatch", "--", file],
            self.deadline,
        )?;
        match output.status.code() {
            Some(TRACKED_EXIT_CODE) => Ok(true),
            Some(NOT_TRACKED_EXIT_CODE) => Ok(false),
            _ => Err(git_failure("ls-files", &output)),
        }
    }

    /// Whether git ignores `.env` at the root. Only meaningful inside git.
    pub fn is_env_ignored(&self) -> Result<bool, CheckoutError> {
        let output = run_git(
            &self.root,
            &["check-ignore", "-q", "--", ".env"],
            self.deadline,
        )?;
        match output.status.code() {
            Some(IGNORED_EXIT_CODE) => Ok(true),
            Some(NOT_IGNORED_EXIT_CODE) => Ok(false),
            _ => Err(git_failure("check-ignore", &output)),
        }
    }
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map_or_default(|name| name.to_string_lossy().into_owned())
}

fn canonicalize(path: &Path) -> Result<PathBuf, CheckoutError> {
    path.canonicalize()
        .map_err(|source| CheckoutError::Resolve {
            path: path.to_path_buf(),
            source,
        })
}

fn run_git(
    directory: &Path,
    arguments: &[&str],
    deadline: Option<Deadline>,
) -> Result<CapturedOutput, CheckoutError> {
    let timeout = timeout_within(deadline, GIT_TIMEOUT)?;
    run_git_within(directory, arguments, timeout)
        .map_err(|error| timeout_as_budget(error, deadline))
}

/// Under a deadline, a git that timed out is a budget that ran out: the
/// command must stop and not pay the same wait again and again.
fn timeout_as_budget(error: CheckoutError, deadline: Option<Deadline>) -> CheckoutError {
    match error {
        CheckoutError::GitTimedOut { .. } if deadline.is_some() => {
            CheckoutError::Budget(BudgetExhausted)
        }
        other => other,
    }
}

/// Runs git in `directory` without the file system monitor, killed after
/// `timeout`. The output is captured.
pub(crate) fn run_git_within<I, S>(
    directory: &Path,
    arguments: I,
    timeout: Duration,
) -> Result<CapturedOutput, CheckoutError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(GIT_PROGRAM);
    command
        .args(DISABLE_FSMONITOR_ARGUMENTS)
        .args(arguments)
        .current_dir(directory)
        .env("LC_ALL", "C");
    capture_with_timeout(command, timeout).map_err(|failure| match failure {
        CaptureFailure::Io(error) => CheckoutError::GitUnavailable(error),
        CaptureFailure::TimedOut => CheckoutError::GitTimedOut {
            seconds: timeout.as_secs(),
        },
    })
}

pub(crate) fn git_failure(command: &'static str, output: &CapturedOutput) -> CheckoutError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = stderr
        .lines()
        .next()
        .map_or_else(|| output.status.to_string(), str::to_owned);
    CheckoutError::GitFailed { command, reason }
}

fn git_top_level(
    directory: &Path,
    deadline: Option<Deadline>,
) -> Result<Option<PathBuf>, CheckoutError> {
    let output = match run_git(directory, &["rev-parse", "--show-toplevel"], deadline) {
        Ok(output) => output,
        Err(CheckoutError::GitUnavailable(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout);
        return Ok(Some(PathBuf::from(text.trim_end_matches('\n'))));
    }
    if String::from_utf8_lossy(&output.stderr).contains(NOT_A_REPOSITORY_MESSAGE) {
        return Ok(None);
    }
    Err(git_failure("rev-parse", &output))
}

fn main_worktree(
    root: &Path,
    deadline: Option<Deadline>,
) -> Result<Option<PathBuf>, CheckoutError> {
    let output = run_git(root, &["worktree", "list", "--porcelain"], deadline)?;
    if !output.status.success() {
        return Err(git_failure("worktree", &output));
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    Ok(first_non_bare_worktree(&listing))
}

/// The path of the first entry of `git worktree list --porcelain` that is not bare.
fn first_non_bare_worktree(listing: &str) -> Option<PathBuf> {
    listing
        .split("\n\n")
        .find(|entry| !entry.lines().any(|line| line == BARE_MARKER))
        .and_then(|entry| {
            entry
                .lines()
                .find_map(|line| line.strip_prefix(WORKTREE_PREFIX))
        })
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn git(directory: &Path, arguments: &[&str]) {
        let status = Command::new(GIT_PROGRAM)
            .args(arguments)
            .current_dir(directory)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn initialized_repository() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        git(directory.path(), &["init", "-q"]);
        directory
    }

    #[test]
    fn should_pick_the_first_entry_when_the_listing_has_no_bare_repository() {
        let listing =
            "worktree /a/main\nHEAD abc\nbranch refs/heads/main\n\nworktree /a/feature\nHEAD def\n";

        assert_eq!(
            first_non_bare_worktree(listing),
            Some(PathBuf::from("/a/main"))
        );
    }

    #[test]
    fn should_skip_the_bare_entry_when_the_listing_starts_with_one() {
        let listing = "worktree /a/repo.git\nbare\n\nworktree /a/main\nHEAD abc\n";

        assert_eq!(
            first_non_bare_worktree(listing),
            Some(PathBuf::from("/a/main"))
        );
    }

    #[test]
    fn should_return_none_when_the_listing_is_empty() {
        assert_eq!(first_non_bare_worktree(""), None);
    }

    #[test]
    fn should_use_the_canonical_current_folder_when_it_is_not_in_a_repository() {
        let directory = tempfile::tempdir().unwrap();

        let checkout = Checkout::locate(directory.path()).unwrap();

        assert_eq!(
            checkout,
            Checkout {
                root: directory.path().canonicalize().unwrap(),
                is_git: false,
                main_root: None,
                deadline: None,
            }
        );
    }

    #[test]
    fn should_use_the_top_level_when_the_folder_is_inside_a_repository() {
        let repository = initialized_repository();
        let nested = repository.path().join("src");
        std::fs::create_dir(&nested).unwrap();

        let checkout = Checkout::locate(&nested).unwrap();

        assert_eq!(
            (checkout.root, checkout.is_git),
            (repository.path().canonicalize().unwrap(), true)
        );
    }

    #[test]
    fn should_have_no_main_root_when_the_checkout_is_the_main_worktree() {
        let repository = initialized_repository();

        assert_eq!(Checkout::locate(repository.path()).unwrap().main_root, None);
    }

    #[test]
    fn should_report_ignored_when_gitignore_lists_env() {
        let repository = initialized_repository();
        std::fs::write(repository.path().join(".gitignore"), ".env\n").unwrap();
        let checkout = Checkout::locate(repository.path()).unwrap();

        assert!(checkout.is_env_ignored().unwrap());
    }

    #[test]
    fn should_report_not_ignored_when_gitignore_does_not_list_env() {
        let repository = initialized_repository();
        let checkout = Checkout::locate(repository.path()).unwrap();

        assert!(!checkout.is_env_ignored().unwrap());
    }

    #[test]
    fn should_find_the_main_worktree_when_the_checkout_is_a_linked_worktree() {
        let repository = initialized_repository();
        git(
            repository.path(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ],
        );
        let linked = repository.path().join("linked");
        git(
            repository.path(),
            &["worktree", "add", "-q", linked.to_str().unwrap()],
        );

        let checkout = Checkout::locate(&linked).unwrap();

        assert_eq!(
            checkout.main_root,
            Some(repository.path().canonicalize().unwrap())
        );
    }

    #[test]
    fn should_name_the_repository_after_the_folder_when_the_checkout_is_not_in_git() {
        let directory = tempfile::tempdir().unwrap();
        let checkout = Checkout::locate(directory.path()).unwrap();

        assert_eq!(
            checkout.repository_name().unwrap(),
            folder_name(&directory.path().canonicalize().unwrap())
        );
    }

    #[test]
    fn should_name_the_repository_after_the_main_folder_when_the_checkout_is_a_linked_worktree() {
        let repository = initialized_repository();
        git(
            repository.path(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ],
        );
        let linked = repository.path().join("linked");
        git(
            repository.path(),
            &["worktree", "add", "-q", linked.to_str().unwrap()],
        );

        let checkout = Checkout::locate(&linked).unwrap();

        assert_eq!(
            checkout.repository_name().unwrap(),
            folder_name(&repository.path().canonicalize().unwrap())
        );
    }

    #[test]
    fn should_give_the_folder_name_and_no_worktree_when_the_checkout_is_not_in_git() {
        let directory = tempfile::tempdir().unwrap();
        let checkout = Checkout::locate(directory.path()).unwrap();

        assert_eq!(
            checkout.origin().unwrap(),
            CheckoutOrigin {
                project: folder_name(&directory.path().canonicalize().unwrap()),
                is_worktree: false,
            }
        );
    }

    #[test]
    fn should_give_the_repository_name_and_no_worktree_when_the_checkout_is_the_main_one() {
        let repository = initialized_repository();
        let checkout = Checkout::locate(repository.path()).unwrap();

        assert_eq!(
            checkout.origin().unwrap(),
            CheckoutOrigin {
                project: folder_name(&repository.path().canonicalize().unwrap()),
                is_worktree: false,
            }
        );
    }

    #[test]
    fn should_give_the_repository_name_and_a_worktree_when_the_checkout_is_a_linked_worktree() {
        let repository = initialized_repository();
        git(
            repository.path(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ],
        );
        let linked = repository.path().join("linked");
        git(
            repository.path(),
            &["worktree", "add", "-q", linked.to_str().unwrap()],
        );
        let checkout = Checkout::locate(&linked).unwrap();

        assert_eq!(
            checkout.origin().unwrap(),
            CheckoutOrigin {
                project: folder_name(&repository.path().canonicalize().unwrap()),
                is_worktree: true,
            }
        );
    }

    #[test]
    fn should_report_tracked_when_the_file_is_added_to_the_index() {
        let repository = initialized_repository();
        std::fs::write(repository.path().join("devslot.local.toml"), "").unwrap();
        git(repository.path(), &["add", "devslot.local.toml"]);
        let checkout = Checkout::locate(repository.path()).unwrap();

        assert!(checkout.is_tracked("devslot.local.toml").unwrap());
    }

    #[test]
    fn should_report_not_tracked_when_the_file_is_only_in_the_folder() {
        let repository = initialized_repository();
        std::fs::write(repository.path().join("devslot.local.toml"), "").unwrap();
        let checkout = Checkout::locate(repository.path()).unwrap();

        assert!(!checkout.is_tracked("devslot.local.toml").unwrap());
    }

    #[test]
    fn should_see_a_git_timeout_as_the_budget_only_under_a_deadline() {
        let timed_out = || CheckoutError::GitTimedOut { seconds: 10 };
        let deadline = Some(Deadline::starting_now(Duration::from_secs(60)));

        assert!(matches!(
            timeout_as_budget(timed_out(), deadline),
            CheckoutError::Budget(_)
        ));
        assert!(matches!(
            timeout_as_budget(timed_out(), None),
            CheckoutError::GitTimedOut { .. }
        ));
    }

    #[test]
    fn should_fail_with_the_budget_when_the_deadline_is_exhausted_before_git_runs() {
        let repository = initialized_repository();
        let deadline = Deadline::starting_now(Duration::ZERO);

        let error = Checkout::locate_within(repository.path(), deadline).unwrap_err();

        assert!(matches!(error, CheckoutError::Budget(_)));
    }

    #[test]
    fn should_fail_with_the_budget_when_the_deadline_runs_out_after_the_checkout_was_located() {
        let repository = initialized_repository();
        let deadline = Deadline::starting_now(Duration::from_secs(60));
        let checkout = Checkout::locate_within(repository.path(), deadline).unwrap();
        let exhausted = Checkout {
            deadline: Some(Deadline::starting_now(Duration::ZERO)),
            ..checkout.clone()
        };

        let error = exhausted.is_env_ignored().unwrap_err();

        assert!(matches!(error, CheckoutError::Budget(_)));
        assert_eq!(checkout.main_root, None);
    }
}
