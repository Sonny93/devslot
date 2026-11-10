//! The checkout: its root, whether git manages it, its main worktree, and
//! whether git ignores its `.env`.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use thiserror::Error;

const GIT_PROGRAM: &str = "git";
const NOT_A_REPOSITORY_MESSAGE: &str = "not a git repository";
const WORKTREE_PREFIX: &str = "worktree ";
const BARE_MARKER: &str = "bare";
const IGNORED_EXIT_CODE: i32 = 0;
const NOT_IGNORED_EXIT_CODE: i32 = 1;

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
    /// Git ran and failed for another reason than "not a repository".
    #[error("git {command} failed: {reason}")]
    GitFailed {
        /// The git subcommand.
        command: &'static str,
        /// The first line git printed on stderr, or its exit status.
        reason: String,
    },
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
}

impl Checkout {
    /// Finds the checkout that contains `directory`.
    pub fn locate(directory: &Path) -> Result<Self, CheckoutError> {
        let canonical = canonicalize(directory)?;
        let Some(top_level) = git_top_level(&canonical)? else {
            return Ok(Self {
                root: canonical,
                is_git: false,
                main_root: None,
            });
        };
        let root = canonicalize(&top_level)?;
        let main_root = main_worktree(&root)?.filter(|main| main != &root);
        Ok(Self {
            root,
            is_git: true,
            main_root,
        })
    }

    /// Whether git ignores `.env` at the root. Only meaningful inside git.
    pub fn is_env_ignored(&self) -> Result<bool, CheckoutError> {
        let output = run_git(&self.root, &["check-ignore", "-q", "--", ".env"])?;
        match output.status.code() {
            Some(IGNORED_EXIT_CODE) => Ok(true),
            Some(NOT_IGNORED_EXIT_CODE) => Ok(false),
            _ => Err(git_failure("check-ignore", &output)),
        }
    }
}

fn canonicalize(path: &Path) -> Result<PathBuf, CheckoutError> {
    path.canonicalize()
        .map_err(|source| CheckoutError::Resolve {
            path: path.to_path_buf(),
            source,
        })
}

fn run_git(directory: &Path, arguments: &[&str]) -> Result<Output, CheckoutError> {
    Command::new(GIT_PROGRAM)
        .args(arguments)
        .current_dir(directory)
        .env("LC_ALL", "C")
        .output()
        .map_err(CheckoutError::GitUnavailable)
}

fn git_failure(command: &'static str, output: &Output) -> CheckoutError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = stderr
        .lines()
        .next()
        .map_or_else(|| output.status.to_string(), str::to_owned);
    CheckoutError::GitFailed { command, reason }
}

fn git_top_level(directory: &Path) -> Result<Option<PathBuf>, CheckoutError> {
    let output = match run_git(directory, &["rev-parse", "--show-toplevel"]) {
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

fn main_worktree(root: &Path) -> Result<Option<PathBuf>, CheckoutError> {
    let output = run_git(root, &["worktree", "list", "--porcelain"])?;
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
                main_root: None
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
}
