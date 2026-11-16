//! `ds worktree rm <name|path>`: removes a linked worktree and everything its
//! instance owns. It shows what would be lost first (processes, uncommitted
//! changes, commits found nowhere else, programs of other sessions in use) and
//! refuses a worktree with something to lose unless `--force` is given.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::hook::worktree_create::{target_path, validate_name};
use super::machine::{Machine, locate_checkout};
use super::worktree_removal::{
    PreparedRemoval, RemovalFailure, UnsavedWork, count_unsaved_work, describe_unsaved_work,
    describe_users, ensure_listed_and_unlocked, prepare_removal, remove_from_terminal,
};
use crate::checkout::CheckoutError;
use crate::cli::args::WorktreeRmArguments;
use crate::cli::confirm::{ConfirmationPolicy, exit_code_unless_confirmed};
use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::error::ToolError;
use crate::global_config::GlobalConfig;
use crate::instance_processes::{
    ProcessReport, ProcessSummary, Requester, find_foreign_processes, find_instance_processes,
    join_summaries,
};
use crate::kept_worktrees::{self, KeptWorktree};
use crate::paths::DevslotPaths;
use crate::worktree_identity::WorktreeIdentity;
use crate::worktree_lock::{LockPurpose, WorktreeKey, WorktreeLock};

const COMMAND_NAME: &str = "ds worktree rm";
const NOTHING: &str = "none";
const DETACHED_HEAD_LABEL: &str = "none (detached HEAD)";
const NOT_REGISTERED: &str = "not registered";
/// How long `ds worktree rm` waits for another creation or removal of the
/// worktree, once the person has confirmed.
const LOCK_WAIT: Duration = Duration::from_secs(10);
const IGNORED_FILES_WARNING: &str =
    "ignored files (.env, local databases, uploads) are deleted too";

/// Where the argument pointed, or why it pointed nowhere.
enum Resolution {
    Found(Target),
    Refused(u8),
}

#[derive(Clone)]
struct Target {
    path: PathBuf,
    repository_main_root: Option<PathBuf>,
}

/// What `ds worktree rm` shows before it acts.
#[derive(Debug, PartialEq, Eq)]
struct Preview {
    path: PathBuf,
    branch: Option<String>,
    instance_name: Option<String>,
    processes: ProcessReport,
    in_use: Vec<ProcessSummary>,
    uncommitted_changes: usize,
    unreachable_commits: usize,
    kept: Option<KeptWorktree>,
}

/// What `ds worktree rm` keeps from the preview to act on once confirmed.
struct Previewed {
    target: Target,
    key: WorktreeKey,
    identity: WorktreeIdentity,
    state_directory: PathBuf,
}

/// Resolves the argument, shows the preview, asks, then removes the worktree.
pub fn run_worktree_rm(arguments: &WorktreeRmArguments) -> Result<u8, ToolError> {
    let target = match resolve_target(&arguments.target)? {
        Resolution::Found(target) => target,
        Resolution::Refused(code) => return Ok(code),
    };
    let paths = DevslotPaths::from_process()?;
    let prepared = match prepare_checked(target.clone()) {
        Ok(prepared) => prepared,
        Err(failure) => return Ok(refuse(&failure)),
    };
    let Some(key) = WorktreeKey::of(&prepared.removal.path) else {
        eprintln!(
            "ds: error: cannot lock {}: its parent folder cannot be resolved",
            prepared.removal.path.display()
        );
        return Ok(exit_code::ERROR);
    };
    let previewed = Previewed {
        identity: identity_of(&prepared.removal.path)?,
        state_directory: paths.state_directory().to_path_buf(),
        target,
        key,
    };
    let kept = kept_worktrees::read(&previewed.state_directory, &previewed.key);
    let preview = build_preview(&prepared, kept)?;
    write_stdout(&render_preview(&preview))?;
    if let Some(code) = refusal_without_force(&preview, arguments.force) {
        return Ok(code);
    }
    let prompt = confirmation_prompt(&preview);
    let policy = ConfirmationPolicy::from(arguments.yes);
    if let Some(code) = exit_code_unless_confirmed(COMMAND_NAME, policy, &prompt) {
        return Ok(code);
    }
    remove_under_lock(&previewed, &preview, arguments.force)
}

fn identity_of(path: &Path) -> Result<WorktreeIdentity, CheckoutError> {
    WorktreeIdentity::of(path).map_err(|source| CheckoutError::Resolve {
        path: path.to_path_buf(),
        source,
    })
}

/// Takes the lock of the worktree after the confirmation (a person deciding
/// must not block a creation of that name), checks that it is still the
/// worktree that was shown, and removes it.
fn remove_under_lock(
    previewed: &Previewed,
    preview: &Preview,
    is_forced: bool,
) -> Result<u8, ToolError> {
    let lock = WorktreeLock::acquire_within(
        &previewed.state_directory,
        &previewed.key,
        LOCK_WAIT,
        LockPurpose::Remove,
    )?;
    let Some(_lock) = lock else {
        return Ok(refuse(&RemovalFailure::Busy(preview.path.clone())));
    };
    let prepared = match prepare_unchanged(previewed) {
        Ok(prepared) => prepared,
        Err(failure) => return Ok(refuse(&failure)),
    };
    match remove_from_terminal(&prepared, is_forced, shown_work(preview)) {
        Ok(()) => {
            kept_worktrees::forget(&previewed.state_directory, &previewed.key);
            eprintln!("ds: removed {}", preview.path.display());
            Ok(exit_code::SUCCESS)
        }
        Err(failure) => Ok(refuse(&failure)),
    }
}

fn prepare_unchanged(previewed: &Previewed) -> Result<PreparedRemoval, RemovalFailure> {
    ensure_unchanged(&previewed.target.path, &previewed.identity)?;
    prepare_checked(previewed.target.clone())
}

/// Fails unless the folder at `path` is the one that was shown.
fn ensure_unchanged(path: &Path, previewed: &WorktreeIdentity) -> Result<(), RemovalFailure> {
    let is_unchanged =
        WorktreeIdentity::of(path).is_ok_and(|current| current.is_same_folder_as(previewed));
    if is_unchanged {
        return Ok(());
    }
    Err(RemovalFailure::ChangedSincePreview(path.to_path_buf()))
}

fn refuse(failure: &RemovalFailure) -> u8 {
    failure.announce();
    failure.exit_code()
}

fn prepare_checked(target: Target) -> Result<PreparedRemoval, RemovalFailure> {
    let prepared = prepare_removal(target.path)?;
    ensure_belongs_to(&prepared, target.repository_main_root.as_deref())?;
    ensure_listed_and_unlocked(&prepared.removal)?;
    Ok(prepared)
}

fn ensure_belongs_to(
    prepared: &PreparedRemoval,
    repository_main_root: Option<&Path>,
) -> Result<(), RemovalFailure> {
    let Some(expected) = repository_main_root else {
        return Ok(());
    };
    if prepared.removal.main_root == expected {
        return Ok(());
    }
    Err(RemovalFailure::NotLinkedWorktree(
        prepared.removal.path.clone(),
    ))
}

fn resolve_target(argument: &str) -> Result<Resolution, ToolError> {
    if is_path_argument(argument) {
        return resolve_path(Path::new(argument), None);
    }
    resolve_name(argument)
}

/// Whether `argument` is a path: it holds a `/` or starts with `.`.
fn is_path_argument(argument: &str) -> bool {
    argument.contains('/') || argument.starts_with('.')
}

fn resolve_name(name: &str) -> Result<Resolution, ToolError> {
    if let Err(reason) = validate_name(name) {
        eprintln!("ds: error: the worktree name {name:?} is not valid: {reason}");
        return Ok(Resolution::Refused(exit_code::USAGE));
    }
    let checkout = locate_checkout()?;
    if !checkout.is_git {
        eprintln!(
            "ds: error: {} is not inside a git repository",
            checkout.root.display()
        );
        return Ok(Resolution::Refused(exit_code::ERROR));
    }
    let machine = Machine::from_process()?;
    let global = GlobalConfig::load(&machine.paths.config_file)?;
    let path = match target_path(&checkout, &global, name) {
        Ok(path) => path,
        Err(failure) => {
            eprintln!("ds: error: {failure}");
            return Ok(Resolution::Refused(failure.exit_code()));
        }
    };
    let main_root = checkout.main_root.unwrap_or(checkout.root);
    resolve_path(&path, Some(main_root))
}

fn resolve_path(
    path: &Path,
    repository_main_root: Option<PathBuf>,
) -> Result<Resolution, ToolError> {
    match path.canonicalize() {
        Ok(path) => Ok(Resolution::Found(Target {
            path,
            repository_main_root,
        })),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            eprintln!(
                "ds: error: {} does not exist; nothing was removed",
                path.display()
            );
            Ok(Resolution::Refused(exit_code::ERROR))
        }
        Err(source) => Err(CheckoutError::Resolve {
            path: path.to_path_buf(),
            source,
        }
        .into()),
    }
}

fn build_preview(
    prepared: &PreparedRemoval,
    kept: Option<KeptWorktree>,
) -> Result<Preview, ToolError> {
    let removal = &prepared.removal;
    let scope = prepared.scope();
    let work = count_unsaved_work(removal)?;
    Ok(Preview {
        path: removal.path.clone(),
        branch: removal.branch.clone(),
        instance_name: scope.as_ref().map(|scope| scope.name.clone()),
        processes: find_instance_processes(&removal.path, scope.as_ref()),
        in_use: find_foreign_processes(&removal.path, scope.as_ref(), Requester::Terminal),
        uncommitted_changes: work.uncommitted_changes,
        unreachable_commits: work.unreachable_commits,
        kept,
    })
}

fn shown_work(preview: &Preview) -> UnsavedWork {
    UnsavedWork {
        uncommitted_changes: preview.uncommitted_changes,
        unreachable_commits: preview.unreachable_commits,
    }
}

fn render_preview(preview: &Preview) -> String {
    let branch = preview.branch.as_deref().unwrap_or(DETACHED_HEAD_LABEL);
    let instance = preview.instance_name.as_deref().unwrap_or(NOT_REGISTERED);
    format!(
        "worktree:             {}\nbranch:               {branch}\ninstance:             {instance}\nprocesses to stop:    {}\nother processes here: {} (left running)\n{}{}uncommitted changes:  {}\ncommits found nowhere else: {}\n{IGNORED_FILES_WARNING}\n",
        preview.path.display(),
        listed_or_none(&preview.processes.targets),
        listed_or_none(&preview.processes.others),
        in_use_line(&preview.in_use),
        kept_line(preview.kept.as_ref()),
        preview.uncommitted_changes,
        preview.unreachable_commits,
    )
}

fn in_use_line(in_use: &[ProcessSummary]) -> String {
    if in_use.is_empty() {
        return String::new();
    }
    format!(
        "In use (blocks removal without --force): {}\n",
        join_summaries(in_use)
    )
}

fn kept_line(kept: Option<&KeptWorktree>) -> String {
    let Some(kept) = kept else {
        return String::new();
    };
    format!(
        "Claude Code asked to remove it on {}: {}\n",
        kept.date, kept.reason
    )
}

fn listed_or_none(summaries: &[ProcessSummary]) -> String {
    if summaries.is_empty() {
        return NOTHING.to_owned();
    }
    join_summaries(summaries)
}

fn refusal_without_force(preview: &Preview, is_forced: bool) -> Option<u8> {
    if is_forced {
        return None;
    }
    let reasons: Vec<String> = [in_use_reason(preview), unsaved_work_reason(preview)]
        .into_iter()
        .flatten()
        .collect();
    if reasons.is_empty() {
        return None;
    }
    for reason in reasons {
        eprintln!("ds: error: {reason}");
    }
    Some(exit_code::ERROR)
}

fn in_use_reason(preview: &Preview) -> Option<String> {
    if preview.in_use.is_empty() {
        return None;
    }
    Some(format!(
        "{} is in use by {}; nothing was removed, close them or pass --force to remove it anyway",
        preview.path.display(),
        describe_users(&preview.in_use)
    ))
}

fn unsaved_work_reason(preview: &Preview) -> Option<String> {
    let work = shown_work(preview);
    if work.is_empty() {
        return None;
    }
    Some(describe_unsaved_work(&preview.path, &work))
}

fn confirmation_prompt(preview: &Preview) -> String {
    match &preview.instance_name {
        Some(name) => format!(
            "Stop its processes, delete the containers and volumes of {name} and remove {}? [y/N] ",
            preview.path.display()
        ),
        None => format!("Remove {}? [y/N] ", preview.path.display()),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    fn preview() -> Preview {
        Preview {
            path: PathBuf::from("/wt/app/feat"),
            branch: Some("feat".to_owned()),
            instance_name: Some("app-feat-3f2a".to_owned()),
            processes: ProcessReport {
                targets: vec![ProcessSummary {
                    pid: 10,
                    comm: "node".to_owned(),
                }],
                others: vec![ProcessSummary {
                    pid: 11,
                    comm: "tmux".to_owned(),
                }],
            },
            in_use: Vec::new(),
            uncommitted_changes: 0,
            unreachable_commits: 0,
            kept: None,
        }
    }

    fn in_use_by_claude() -> Preview {
        let mut busy = preview();
        busy.in_use = vec![ProcessSummary {
            pid: 1234,
            comm: "claude".to_owned(),
        }];
        busy
    }

    #[test]
    fn should_tell_a_path_from_a_name_by_a_slash_or_a_leading_dot() {
        assert!(is_path_argument("./feat"));
        assert!(is_path_argument("../feat"));
        assert!(is_path_argument("a/b"));
        assert!(is_path_argument(".hidden"));
        assert!(!is_path_argument("feat-x"));
    }

    #[test]
    fn should_show_everything_that_would_be_lost_before_acting() {
        let text = render_preview(&preview());

        assert!(text.contains("/wt/app/feat"));
        assert!(text.contains("branch:               feat"));
        assert!(text.contains("app-feat-3f2a"));
        assert!(text.contains("processes to stop:    10 (node)"));
        assert!(text.contains("other processes here: 11 (tmux) (left running)"));
        assert!(text.contains("uncommitted changes:  0"));
        assert!(text.contains("commits found nowhere else: 0"));
        assert!(text.ends_with("ignored files (.env, local databases, uploads) are deleted too\n"));
    }

    #[test]
    fn should_say_so_when_there_is_no_instance_no_branch_and_no_process() {
        let mut bare = preview();
        bare.branch = None;
        bare.instance_name = None;
        bare.processes = ProcessReport::default();

        let text = render_preview(&bare);

        assert!(text.contains("none (detached HEAD)"));
        assert!(text.contains("not registered"));
        assert!(text.contains("processes to stop:    none"));
    }

    #[test]
    fn should_require_force_when_there_are_changes_or_lost_commits() {
        let mut dirty = preview();
        dirty.uncommitted_changes = 2;
        let mut ahead = preview();
        ahead.unreachable_commits = 1;

        assert_eq!(refusal_without_force(&dirty, false), Some(exit_code::ERROR));
        assert_eq!(refusal_without_force(&ahead, false), Some(exit_code::ERROR));
        assert_eq!(refusal_without_force(&dirty, true), None);
        assert_eq!(refusal_without_force(&preview(), false), None);
    }

    #[test]
    fn should_name_the_instance_in_the_question_when_one_is_registered() {
        assert!(confirmation_prompt(&preview()).contains("app-feat-3f2a"));
    }

    #[test]
    fn should_show_what_is_in_use_and_that_it_blocks_without_force() {
        let text = render_preview(&in_use_by_claude());

        assert!(text.contains("In use (blocks removal without --force): 1234 (claude)\n"));
        assert!(!render_preview(&preview()).contains("In use"));
    }

    #[test]
    fn should_require_force_when_a_process_is_in_use() {
        assert_eq!(
            refusal_without_force(&in_use_by_claude(), false),
            Some(exit_code::ERROR)
        );
        assert_eq!(refusal_without_force(&in_use_by_claude(), true), None);
    }

    #[test]
    fn should_say_when_claude_code_asked_to_remove_the_worktree() {
        let mut kept = preview();
        kept.kept = Some(KeptWorktree {
            path: PathBuf::from("/wt/app/feat"),
            reason: "in use by 12 (node)".to_owned(),
            date: "2026-11-16".to_owned(),
        });

        let text = render_preview(&kept);

        assert!(
            text.contains("Claude Code asked to remove it on 2026-11-16: in use by 12 (node)\n")
        );
        assert!(!render_preview(&preview()).contains("Claude Code asked"));
    }

    fn worktree_at(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join(".git"), "gitdir: x").unwrap();
    }

    #[test]
    fn should_accept_the_folder_that_was_shown() {
        let directory = tempfile::tempdir().unwrap();
        worktree_at(directory.path());
        let shown = WorktreeIdentity::of(directory.path()).unwrap();

        assert!(ensure_unchanged(directory.path(), &shown).is_ok());
    }

    #[test]
    fn should_refuse_when_another_folder_took_the_place_of_the_one_shown() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("feat");
        worktree_at(&path);
        let shown = WorktreeIdentity::of(&path).unwrap();
        std::fs::rename(&path, directory.path().join("feat-old")).unwrap();
        worktree_at(&path);

        let result = ensure_unchanged(&path, &shown);

        assert!(matches!(
            result,
            Err(RemovalFailure::ChangedSincePreview(_))
        ));
    }

    #[test]
    fn should_refuse_when_the_folder_that_was_shown_is_gone() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("feat");
        worktree_at(&path);
        let shown = WorktreeIdentity::of(&path).unwrap();
        std::fs::remove_dir_all(&path).unwrap();

        let result = ensure_unchanged(&path, &shown);

        assert!(matches!(
            result,
            Err(RemovalFailure::ChangedSincePreview(_))
        ));
    }

    #[test]
    fn should_name_the_other_claude_session_when_it_is_what_blocks() {
        let reason = in_use_reason(&in_use_by_claude()).unwrap();

        assert!(reason.contains("another Claude Code session (1234 (claude))"));
        assert!(reason.contains("--force"));
    }
}
