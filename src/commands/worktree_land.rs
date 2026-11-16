//! `ds worktree land [--onto <branch>]`: fast-forwards the branch checked out
//! in the main checkout onto the branch of the current linked worktree. It
//! only ever fast-forwards: when the base moved, the worktree is rebased by
//! hand first. It never forces, stashes, resets or pushes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::machine::locate_checkout;
use crate::checkout::CheckoutError;
use crate::cli::args::WorktreeLandArguments;
use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::error::ToolError;
use crate::git_worktree::{
    WorktreeGitError, added_paths, base_config_key, commits_between, current_branch,
    fast_forward_onto, is_ancestor, is_tracked_file_in_index, operation_in_progress, read_config,
    short_commit, worktree_status,
};

const HEAD: &str = "HEAD";
const MAX_LISTED_PATHS: usize = 10;

/// Why nothing was landed. Each one is a refusal, exit code 1.
#[derive(Debug, Error)]
enum Refusal {
    #[error("{} is not inside a git repository", .0.display())]
    NotInGit(PathBuf),
    #[error(
        "{} is the main checkout, not a linked worktree; run ds worktree land from a linked worktree",
        .0.display()
    )]
    MainCheckout(PathBuf),
    #[error("the worktree is on a detached HEAD; check out a branch before landing")]
    DetachedWorktree,
    #[error(
        "no base is recorded for the branch {branch} (it was created before ds worktree land existed); pass --onto <branch>"
    )]
    NoTarget { branch: String },
    #[error(
        "the worktree has {count} uncommitted change(s) to tracked files; commit them first, nothing was landed"
    )]
    UncommittedChanges { count: usize },
    #[error(
        "the main checkout {} is on {actual}, not on {target}; nothing was landed",
        .path.display()
    )]
    MainOnOtherBranch {
        path: PathBuf,
        actual: String,
        target: String,
    },
    #[error(
        "{operation} is in progress in the main checkout {}; finish or abort it, nothing was landed",
        .path.display()
    )]
    MainBusy {
        path: PathBuf,
        operation: &'static str,
    },
    #[error(
        "the base moved: run `git rebase {target}` here, resolve, commit, then run ds worktree land again"
    )]
    BaseMoved { target: String },
    #[error(
        "the landing would overwrite {paths} in the main checkout {}, which git does not track there; move them aside or remove them from the branch, nothing was landed",
        .path.display()
    )]
    WouldOverwrite { path: PathBuf, paths: String },
    #[error("git refused the fast-forward; nothing was landed")]
    GitRefused,
}

enum LandFailure {
    Refused(Refusal),
    Tool(ToolError),
}

impl From<Refusal> for LandFailure {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<CheckoutError> for LandFailure {
    fn from(error: CheckoutError) -> Self {
        Self::Tool(error.into())
    }
}

impl From<WorktreeGitError> for LandFailure {
    fn from(error: WorktreeGitError) -> Self {
        match error {
            WorktreeGitError::Tool(error) => error.into(),
            WorktreeGitError::Rejected { .. } => Refusal::GitRefused.into(),
        }
    }
}

struct LinkedWorktree {
    root: PathBuf,
    main_root: PathBuf,
    branch: String,
}

#[derive(Debug, PartialEq, Eq)]
enum Landing {
    NothingToLand {
        target: String,
    },
    Landed {
        target: String,
        old_commit: String,
        new_commit: String,
        commit_count: usize,
    },
}

/// Lands the branch of the worktree the current folder belongs to.
pub fn run_worktree_land(arguments: &WorktreeLandArguments) -> Result<u8, ToolError> {
    match land(arguments.onto.as_deref()) {
        Ok(landing) => {
            write_stdout(&render_landing(&landing))?;
            Ok(exit_code::SUCCESS)
        }
        Err(LandFailure::Refused(refusal)) => {
            eprintln!("ds: error: {refusal}");
            Ok(exit_code::ERROR)
        }
        Err(LandFailure::Tool(error)) => Err(error),
    }
}

fn land(onto: Option<&str>) -> Result<Landing, LandFailure> {
    let worktree = locate_linked_worktree()?;
    let target = resolve_target(&worktree, onto)?;
    ensure_committed(&worktree)?;
    ensure_main_ready(&worktree.main_root, &target)?;
    let target_reference = format!("refs/heads/{target}");
    if !is_ancestor(&worktree.root, &target_reference, HEAD)? {
        return Err(Refusal::BaseMoved { target }.into());
    }
    let commit_count = commits_between(&worktree.root, (&target_reference, HEAD), None)?;
    if commit_count == 0 {
        return Ok(Landing::NothingToLand { target });
    }
    ensure_no_untracked_overwrite(&worktree, &target_reference)?;
    let old_commit = short_commit(&worktree.main_root, &target_reference)?;
    fast_forward_onto(&worktree.main_root, &worktree.branch)?;
    let new_commit = short_commit(&worktree.main_root, &target_reference)?;
    Ok(Landing::Landed {
        target,
        old_commit,
        new_commit,
        commit_count,
    })
}

fn locate_linked_worktree() -> Result<LinkedWorktree, LandFailure> {
    let checkout = locate_checkout().map_err(LandFailure::Tool)?;
    if !checkout.is_git {
        return Err(Refusal::NotInGit(checkout.root).into());
    }
    let Some(main_root) = checkout.main_root else {
        return Err(Refusal::MainCheckout(checkout.root).into());
    };
    let Some(branch) = current_branch(&checkout.root)? else {
        return Err(Refusal::DetachedWorktree.into());
    };
    Ok(LinkedWorktree {
        root: checkout.root,
        main_root,
        branch,
    })
}

fn resolve_target(worktree: &LinkedWorktree, onto: Option<&str>) -> Result<String, LandFailure> {
    if let Some(target) = onto {
        return Ok(target.to_owned());
    }
    let key = base_config_key(&worktree.branch);
    match read_config(&worktree.root, &key)? {
        Some(target) => Ok(target),
        None => Err(Refusal::NoTarget {
            branch: worktree.branch.clone(),
        }
        .into()),
    }
}

fn ensure_committed(worktree: &LinkedWorktree) -> Result<(), LandFailure> {
    let status = worktree_status(&worktree.root)?;
    if status.tracked_changes > 0 {
        return Err(Refusal::UncommittedChanges {
            count: status.tracked_changes,
        }
        .into());
    }
    if status.untracked_files > 0 {
        eprintln!(
            "ds: warning: {} untracked file(s) in the worktree are not landed",
            status.untracked_files
        );
    }
    Ok(())
}

fn ensure_main_ready(main_root: &Path, target: &str) -> Result<(), LandFailure> {
    let actual = current_branch(main_root)?.unwrap_or_else(|| "a detached HEAD".to_owned());
    if actual != target {
        return Err(Refusal::MainOnOtherBranch {
            path: main_root.to_path_buf(),
            actual,
            target: target.to_owned(),
        }
        .into());
    }
    if let Some(operation) = operation_in_progress(main_root)? {
        return Err(Refusal::MainBusy {
            path: main_root.to_path_buf(),
            operation,
        }
        .into());
    }
    Ok(())
}

/// `git merge` replaces ignored files without a word: the paths the branch
/// adds must not exist in the main checkout.
fn ensure_no_untracked_overwrite(
    worktree: &LinkedWorktree,
    target_reference: &str,
) -> Result<(), LandFailure> {
    let branch_reference = format!("refs/heads/{}", worktree.branch);
    let added = added_paths(&worktree.main_root, (target_reference, &branch_reference))?;
    let mut tracked_parents: HashMap<PathBuf, bool> = HashMap::new();
    let mut existing: Vec<PathBuf> = Vec::new();
    for path in &added {
        let blocker = first_blocker(&worktree.main_root, path, &mut tracked_parents)?;
        if let Some(blocker) = blocker.filter(|blocker| !existing.contains(blocker)) {
            existing.push(blocker);
        }
    }
    if existing.is_empty() {
        return Ok(());
    }
    Err(Refusal::WouldOverwrite {
        path: worktree.main_root.clone(),
        paths: list_paths(&existing),
    }
    .into())
}

/// What stops `added` from being written in `checkout_root` without losing
/// something: a parent that exists as a file or a symlink and is not a
/// tracked file of the main checkout's index (git replaces a tracked one, and
/// refuses when it is modified), or the path itself when anything is there.
/// `tracked_parents` remembers the answer of git per parent.
fn first_blocker(
    checkout_root: &Path,
    added: &Path,
    tracked_parents: &mut HashMap<PathBuf, bool>,
) -> Result<Option<PathBuf>, CheckoutError> {
    if let Some(parent) = blocking_parent(checkout_root, added) {
        if is_tracked_file(checkout_root, parent, tracked_parents)? {
            return Ok(None);
        }
        return Ok(Some(parent.to_path_buf()));
    }
    let exists = std::fs::symlink_metadata(checkout_root.join(added)).is_ok();
    Ok(exists.then(|| added.to_path_buf()))
}

fn blocking_parent<'a>(checkout_root: &Path, added: &'a Path) -> Option<&'a Path> {
    let mut ancestors: Vec<&Path> = added
        .ancestors()
        .skip(1)
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .collect();
    ancestors.reverse();
    ancestors.into_iter().find(|ancestor| {
        std::fs::symlink_metadata(checkout_root.join(ancestor))
            .is_ok_and(|metadata| !metadata.is_dir())
    })
}

fn is_tracked_file(
    checkout_root: &Path,
    parent: &Path,
    tracked_parents: &mut HashMap<PathBuf, bool>,
) -> Result<bool, CheckoutError> {
    if let Some(is_tracked) = tracked_parents.get(parent) {
        return Ok(*is_tracked);
    }
    let is_tracked = is_tracked_file_in_index(checkout_root, parent)?;
    tracked_parents.insert(parent.to_path_buf(), is_tracked);
    Ok(is_tracked)
}

fn list_paths(paths: &[PathBuf]) -> String {
    let listed: Vec<String> = paths
        .iter()
        .take(MAX_LISTED_PATHS)
        .map(|path| path.display().to_string())
        .collect();
    let hidden = paths.len().saturating_sub(MAX_LISTED_PATHS);
    if hidden == 0 {
        return listed.join(", ");
    }
    format!("{} and {hidden} more", listed.join(", "))
}

fn render_landing(landing: &Landing) -> String {
    match landing {
        Landing::NothingToLand { target } => {
            format!("nothing to land: this branch is already at {target}\n")
        }
        Landing::Landed {
            target,
            old_commit,
            new_commit,
            commit_count,
        } => {
            let noun = if *commit_count == 1 {
                "commit"
            } else {
                "commits"
            };
            format!("{target} moved from {old_commit} to {new_commit} ({commit_count} {noun})\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn landed(commit_count: usize) -> Landing {
        Landing::Landed {
            target: "main".to_owned(),
            old_commit: "abc1234".to_owned(),
            new_commit: "def5678".to_owned(),
            commit_count,
        }
    }

    #[test]
    fn should_say_how_far_the_target_moved_when_rendering_a_landing() {
        assert_eq!(
            render_landing(&landed(3)),
            "main moved from abc1234 to def5678 (3 commits)\n"
        );
    }

    #[test]
    fn should_use_the_singular_when_one_commit_landed() {
        assert_eq!(
            render_landing(&landed(1)),
            "main moved from abc1234 to def5678 (1 commit)\n"
        );
    }

    #[test]
    fn should_say_there_is_nothing_to_land_when_rendering_an_empty_landing() {
        let landing = Landing::NothingToLand {
            target: "main".to_owned(),
        };

        assert_eq!(
            render_landing(&landing),
            "nothing to land: this branch is already at main\n"
        );
    }
}
