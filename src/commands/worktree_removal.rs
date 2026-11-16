//! Removing a linked worktree, for `ds hook worktree-remove` and
//! `ds worktree rm`. The order keeps the worst outcomes out of reach:
//!
//! 1. checks with no side effect: the path is the root of a linked worktree
//!    that git lists at exactly that path, the registry entry is read, the
//!    worktree is not locked, no process of somebody else (another session,
//!    an editor) runs in it, and no other Claude Code session holds a lease on
//!    it (see `session_leases`);
//! 2. the instance's processes are stopped (only the ones that belong to it),
//!    then the check for somebody else's processes runs again, because
//!    stopping can take seconds and a person can wait at a prompt;
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
use crate::ensure_cache::remove_checkout_entries;
use crate::error::ToolError;
use crate::git_worktree::{
    WorktreeGitError, current_branch, delete_branch, is_branch_merged, list_worktrees,
    remove_worktree, uncommitted_change_count, unreachable_commit_count,
};
use crate::instance_processes::{
    CLAUDE_COMM, InstanceScope, ProcessControl, ProcessSummary, Requester, SystemProcesses,
    join_summaries,
};
use crate::registry::record::{InstanceRecord, RegistryChange};
use crate::session_leases::{self, SessionLease, join_descriptions};
use crate::worktree_lock::WorktreeKey;

const DETACHED_HEAD_NAME: &str = "HEAD";
const REMOVE_COMMAND: &str = "worktree remove";
/// Why a worktree is kept when another creation or removal of it is running.
pub const BUSY_REASON: &str = "another creation or removal of this worktree was running";

/// Why a worktree was not removed, or not fully.
#[derive(Debug, Error)]
pub enum RemovalFailure {
    /// The path is not the root of a linked worktree, so nothing was removed.
    #[error("{} is not the root of a linked worktree; nothing was removed", .0.display())]
    NotLinkedWorktree(PathBuf),
    /// The worktree is locked: nothing was stopped, deleted or forgotten.
    #[error("{} is locked (git worktree unlock first)", .0.display())]
    Locked(PathBuf),
    /// Another session or program runs in the worktree: nothing was deleted
    /// or forgotten (instance processes may already be stopped).
    #[error("{}", describe_in_use(.path, .processes))]
    InUse {
        /// The worktree.
        path: PathBuf,
        /// The processes of somebody else that run in it.
        processes: Vec<ProcessSummary>,
    },
    /// Another Claude Code session holds a lease on the worktree (it ran
    /// `ds up`, `ds run` or `ds reset` there): nothing was deleted or
    /// forgotten (instance processes may already be stopped).
    #[error("{}", describe_sessions_active(.path, .sessions))]
    SessionsActive {
        /// The worktree.
        path: PathBuf,
        /// The sessions that hold it.
        sessions: Vec<SessionLease>,
    },
    /// Git refused, or could not run.
    #[error(transparent)]
    Git(#[from] WorktreeGitError),
    /// Work appeared in the worktree after it was shown: nothing was deleted
    /// or forgotten (instance processes are stopped).
    #[error("{}", describe_unsaved_work(.path, .work))]
    UnsavedWork {
        /// The worktree.
        path: PathBuf,
        /// What it holds now.
        work: UnsavedWork,
    },
    /// The checkout cannot be examined.
    #[error(transparent)]
    Checkout(#[from] CheckoutError),
    /// The worktree is not the one that was shown: it is gone or was replaced
    /// while `ds worktree rm` waited, so nothing was removed.
    #[error("the worktree {} changed since the preview; nothing was removed, run the command again", .0.display())]
    ChangedSincePreview(PathBuf),
    /// Another creation or removal of the worktree holds its lock.
    #[error("another creation or removal of {} is running; nothing was removed, try again", .0.display())]
    Busy(PathBuf),
}

impl RemovalFailure {
    /// 1 when the state of the folder or git refused the removal, 2 when the
    /// tool itself failed.
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::NotLinkedWorktree(_)
            | Self::Locked(_)
            | Self::InUse { .. }
            | Self::SessionsActive { .. }
            | Self::UnsavedWork { .. }
            | Self::ChangedSincePreview(_)
            | Self::Busy(_)
            | Self::Git(WorktreeGitError::Rejected { .. }) => exit_code::ERROR,
            Self::Git(WorktreeGitError::Tool(_)) | Self::Checkout(_) => exit_code::TOOL_FAILURE,
        }
    }

    /// Why the worktree was kept, in a few words, for `ds ls`: process names
    /// and paths only.
    pub fn kept_reason(&self) -> String {
        match self {
            Self::NotLinkedWorktree(_) => "not the root of a linked worktree".to_owned(),
            Self::Locked(_) => "locked by git".to_owned(),
            Self::InUse { processes, .. } => format!("in use by {}", describe_users(processes)),
            Self::SessionsActive { sessions, .. } => format!(
                "in use by another Claude Code session ({})",
                join_descriptions(sessions)
            ),
            Self::Git(_) => "git could not remove it".to_owned(),
            Self::UnsavedWork { .. } => "it holds unsaved work".to_owned(),
            Self::Checkout(_) => "it could not be examined".to_owned(),
            Self::ChangedSincePreview(_) => "it changed while it was being removed".to_owned(),
            Self::Busy(_) => BUSY_REASON.to_owned(),
        }
    }

    /// Prints the failure on stderr. A lock or a process in use says what to
    /// do about it, so they are not prefixed with `error:`.
    pub fn announce(&self) {
        match self {
            Self::Locked(_) | Self::InUse { .. } | Self::SessionsActive { .. } => {
                eprintln!("ds: {self}");
            }
            _ => eprintln!("ds: error: {self}"),
        }
    }
}

fn describe_in_use(path: &Path, processes: &[ProcessSummary]) -> String {
    format!(
        "{path} is in use by {}; nothing was deleted (the worktree, its data and its slot are kept). Close {}, or run ds worktree rm --force {path} from a terminal.",
        describe_users(processes),
        if processes.len() == 1 { "it" } else { "them" },
        path = path.display(),
    )
}

fn describe_sessions_active(path: &Path, sessions: &[SessionLease]) -> String {
    format!(
        "{path} is in use by another Claude Code session ({}); nothing was deleted (the worktree, its data and its slot are kept). Close {}, or run ds worktree rm --force {path} from a terminal: without --force, ds worktree rm is refused too, even typed inside a session.",
        join_descriptions(sessions),
        if sessions.len() == 1 { "it" } else { "them" },
        path = path.display(),
    )
}

/// What a worktree holds that removing it would lose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsavedWork {
    /// Lines of `git status --porcelain`.
    pub uncommitted_changes: usize,
    /// Commits found nowhere else.
    pub unreachable_commits: usize,
}

impl UnsavedWork {
    /// Whether there is anything to lose.
    pub const fn is_empty(&self) -> bool {
        self.uncommitted_changes == 0 && self.unreachable_commits == 0
    }

    /// Whether `self` holds more of either kind than `before`.
    pub const fn has_grown_since(&self, before: &Self) -> bool {
        self.uncommitted_changes > before.uncommitted_changes
            || self.unreachable_commits > before.unreachable_commits
    }
}

/// The refusal message for a worktree that holds `work`.
pub fn describe_unsaved_work(path: &Path, work: &UnsavedWork) -> String {
    format!(
        "{} has {} uncommitted change(s) and {} commit(s) found nowhere else; nothing was removed, pass --force to remove it anyway",
        path.display(),
        work.uncommitted_changes,
        work.unreachable_commits
    )
}

/// Counts what removing the worktree would lose.
pub fn count_unsaved_work(removal: &Removal) -> Result<UnsavedWork, CheckoutError> {
    let branch_to_delete = if is_branch_deletable(removal) {
        removal.branch.as_deref()
    } else {
        None
    };
    Ok(UnsavedWork {
        uncommitted_changes: uncommitted_change_count(&removal.path)?,
        unreachable_commits: unreachable_commit_count(&removal.path, branch_to_delete)?,
    })
}

/// The processes that use a worktree, for a message: names the other Claude
/// Code session when one of them is its `claude` process.
pub fn describe_users(processes: &[ProcessSummary]) -> String {
    let list = join_summaries(processes);
    if processes.iter().any(|process| process.comm == CLAUDE_COMM) {
        return format!("another Claude Code session ({list})");
    }
    list
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

/// What a removal acts through: Docker, the processes of the machine, the
/// state directory (where the session leases are), who asks, and whether the
/// check for somebody else's processes and sessions is skipped.
pub struct RemovalContext<'a> {
    /// Deletes the instance's containers, volumes and networks.
    pub runner: &'a dyn ComposeRunner,
    /// Finds and stops processes, and tells which sessions run.
    pub processes: &'a dyn ProcessControl,
    /// Where the session leases of the worktrees are kept.
    pub state_directory: &'a Path,
    /// Whoever asked for the removal.
    pub requester: Requester,
    /// Skips the checks for somebody else's processes and sessions
    /// (`ds worktree rm --force`).
    pub is_forced: bool,
    /// What the worktree held when it was shown, for a removal that must not
    /// delete work saved since; `None` when nothing was shown (the hook).
    pub shown_work: Option<UnsavedWork>,
}

/// Removes the worktree at `path` (canonical) for the `WorktreeRemove` hook:
/// the whole sequence. `claude_pid` is `CLAUDE_PID` from the hook environment;
/// `state_directory` holds the session leases.
pub fn remove_linked_worktree(
    path: PathBuf,
    claude_pid: Option<i32>,
    state_directory: &Path,
) -> Result<(), RemovalFailure> {
    let prepared = prepare_removal(path)?;
    let runner = DockerRunner::from_environment();
    let context = RemovalContext {
        runner: &runner,
        processes: &SystemProcesses,
        state_directory,
        requester: Requester::Session { claude_pid },
        is_forced: false,
        shown_work: None,
    };
    remove_prepared(&prepared, &context)
}

/// Removes a prepared worktree for `ds worktree rm`.
pub fn remove_from_terminal(
    prepared: &PreparedRemoval,
    is_forced: bool,
    shown_work: UnsavedWork,
    state_directory: &Path,
) -> Result<(), RemovalFailure> {
    let runner = DockerRunner::from_environment();
    let context = RemovalContext {
        runner: &runner,
        processes: &SystemProcesses,
        state_directory,
        requester: Requester::Terminal,
        is_forced,
        shown_work: Some(shown_work),
    };
    remove_prepared(prepared, &context)
}

/// Runs the steps 1 (lock and use checks) to 5 on a prepared worktree.
pub fn remove_prepared(
    prepared: &PreparedRemoval,
    context: &RemovalContext,
) -> Result<(), RemovalFailure> {
    let removal = &prepared.removal;
    ensure_listed_and_unlocked(removal)?;
    ensure_not_in_use(prepared, context)?;
    stop_processes(prepared, context.processes);
    ensure_not_in_use(prepared, context)?;
    ensure_no_new_work(removal, context)?;
    remove_git_worktree(removal)?;
    if let Some(instance) = &prepared.instance {
        forget_instance(&instance.machine, &instance.record, context.runner);
    }
    delete_branch_when_merged(removal);
    Ok(())
}

/// Counts again right before the deletion: stopping the processes takes
/// seconds, and work saved meanwhile must not be deleted unseen.
fn ensure_no_new_work(removal: &Removal, context: &RemovalContext) -> Result<(), RemovalFailure> {
    let Some(shown) = context.shown_work.filter(|_| !context.is_forced) else {
        return Ok(());
    };
    let work = count_unsaved_work(removal)?;
    if !work.has_grown_since(&shown) {
        return Ok(());
    }
    Err(RemovalFailure::UnsavedWork {
        path: removal.path.clone(),
        work,
    })
}

fn ensure_not_in_use(
    prepared: &PreparedRemoval,
    context: &RemovalContext,
) -> Result<(), RemovalFailure> {
    if context.is_forced {
        return Ok(());
    }
    ensure_no_foreign_process(prepared, context)?;
    ensure_no_other_session(prepared, context)
}

fn ensure_no_foreign_process(
    prepared: &PreparedRemoval,
    context: &RemovalContext,
) -> Result<(), RemovalFailure> {
    let path = &prepared.removal.path;
    let processes = context
        .processes
        .foreign(path, prepared.scope().as_ref(), context.requester);
    if processes.is_empty() {
        return Ok(());
    }
    Err(RemovalFailure::InUse {
        path: path.clone(),
        processes,
    })
}

/// The live leases of sessions other than the one that closes (none of them
/// is "own" for a terminal) refuse the removal. Dead leases are deleted.
fn ensure_no_other_session(
    prepared: &PreparedRemoval,
    context: &RemovalContext,
) -> Result<(), RemovalFailure> {
    let path = &prepared.removal.path;
    let Some(key) = WorktreeKey::of(path) else {
        return Ok(());
    };
    let own = context.processes.own_session(context.requester);
    let is_alive = |session| context.processes.is_session_alive(session);
    let sessions: Vec<SessionLease> =
        session_leases::live(context.state_directory, &key, &is_alive)
            .into_iter()
            .filter(|lease| Some(lease.session) != own)
            .collect();
    if sessions.is_empty() {
        return Ok(());
    }
    Err(RemovalFailure::SessionsActive {
        path: path.clone(),
        sessions,
    })
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

fn stop_processes(prepared: &PreparedRemoval, processes: &dyn ProcessControl) {
    let path = &prepared.removal.path;
    let scope = prepared.scope();
    if scope.is_none() {
        print_still_running(processes.find(path, None).still_running_line(path));
        return;
    }
    let stopped = processes.stop(path, scope.as_ref());
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
        return;
    }
    remove_checkout_entries(machine.paths.state_directory(), &instance.path);
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
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::process::Command;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::compose::runner::testing::FakeComposeRunner;
    use crate::discovery::PortKind;
    use crate::instance_processes::{ProcessReport, SessionProcess, StopReport};
    use crate::paths::{DevslotPaths, PathVariables};
    use crate::port_range::PortRange;
    use crate::registry::record::{AssignedPort, PortDetail};
    use crate::registry::store::RegistryStore;

    const INSTANCE_NAME: &str = "app-feat-3f2a";
    const NEIGHBOUR_NAME: &str = "app-other-9c1d";

    struct FakeProcesses {
        foreign_answers: RefCell<VecDeque<Vec<ProcessSummary>>>,
        stop_calls: Cell<u32>,
        file_created_on_stop: Option<PathBuf>,
        live_sessions: Vec<SessionProcess>,
        own_session: Option<SessionProcess>,
    }

    impl FakeProcesses {
        fn answering(answers: Vec<Vec<ProcessSummary>>) -> Self {
            Self {
                foreign_answers: RefCell::new(answers.into()),
                stop_calls: Cell::new(0),
                file_created_on_stop: None,
                live_sessions: Vec::new(),
                own_session: None,
            }
        }

        fn with_live_sessions(mut self, sessions: Vec<SessionProcess>) -> Self {
            self.live_sessions = sessions;
            self
        }

        fn running_as(mut self, session: SessionProcess) -> Self {
            self.own_session = Some(session);
            self
        }

        fn creating_on_stop(mut self, file: PathBuf) -> Self {
            self.file_created_on_stop = Some(file);
            self
        }
    }

    impl ProcessControl for FakeProcesses {
        fn find(&self, _worktree: &Path, _scope: Option<&InstanceScope>) -> ProcessReport {
            ProcessReport::default()
        }

        fn stop(&self, _worktree: &Path, _scope: Option<&InstanceScope>) -> StopReport {
            self.stop_calls.set(self.stop_calls.get() + 1);
            if let Some(file) = &self.file_created_on_stop {
                std::fs::write(file, "saved while stopping").unwrap();
            }
            StopReport::default()
        }

        fn foreign(
            &self,
            _worktree: &Path,
            _scope: Option<&InstanceScope>,
            _requester: Requester,
        ) -> Vec<ProcessSummary> {
            self.foreign_answers
                .borrow_mut()
                .pop_front()
                .unwrap_or_default()
        }

        fn is_session_alive(&self, session: SessionProcess) -> bool {
            self.live_sessions.contains(&session)
        }

        fn own_session(&self, requester: Requester) -> Option<SessionProcess> {
            self.own_session
                .filter(|_| matches!(requester, Requester::Session { .. }))
        }
    }

    const fn session_process(pid: i32) -> SessionProcess {
        SessionProcess {
            pid,
            start_time: 70,
        }
    }

    fn other_session() -> Vec<ProcessSummary> {
        vec![ProcessSummary {
            pid: 1234,
            comm: "claude".to_owned(),
        }]
    }

    fn git(directory: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(["-c", "commit.gpgsign=false"])
            .args(arguments)
            .current_dir(directory)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?} failed");
    }

    fn record(path: &Path, name: &str, slot: u32, port: u16) -> InstanceRecord {
        InstanceRecord {
            path: path.to_path_buf(),
            name: name.to_owned(),
            slot,
            range: PortRange {
                first: port,
                last: port + 9,
            },
            ports: vec![AssignedPort {
                variable: "PORT".to_owned(),
                port,
                detail: PortDetail {
                    kind: PortKind::Web,
                    service: None,
                    target: None,
                },
            }],
        }
    }

    struct Fixture {
        _folder: tempfile::TempDir,
        main: PathBuf,
        prepared: PreparedRemoval,
    }

    fn fixture() -> Fixture {
        let folder = tempfile::tempdir().unwrap();
        let base = folder.path().canonicalize().unwrap();
        let main = base.join("main");
        let worktree = base.join("worktrees/feat");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "first"]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat",
                worktree.to_str().unwrap(),
            ],
        );
        let home = base.join("home");
        let paths = DevslotPaths::from_variables(&PathVariables {
            config_home: Some(home.join("config").into()),
            state_home: Some(home.join("state").into()),
            home: Some(home.into()),
        })
        .unwrap();
        let machine = Machine {
            store: RegistryStore::new(paths.clone()),
            paths,
        };
        let instance = record(&worktree, INSTANCE_NAME, 1, 20_050);
        let neighbour = record(&base.join("other"), NEIGHBOUR_NAME, 2, 20_060);
        machine
            .store
            .locked(LOCK_TIMEOUT, |locked| {
                locked.apply(&[
                    RegistryChange::SaveInstance(instance.clone()),
                    RegistryChange::SaveInstance(neighbour),
                ])
            })
            .unwrap();
        let prepared = PreparedRemoval {
            removal: Removal {
                path: worktree,
                main_root: main.clone(),
                branch: Some("feat".to_owned()),
            },
            instance: Some(RegisteredInstance {
                machine,
                record: instance,
            }),
        };
        Fixture {
            _folder: folder,
            main,
            prepared,
        }
    }

    impl Fixture {
        fn state_directory(&self) -> &Path {
            self.prepared
                .instance
                .as_ref()
                .unwrap()
                .machine
                .paths
                .state_directory()
        }

        fn key(&self) -> WorktreeKey {
            WorktreeKey::of(&self.prepared.removal.path).unwrap()
        }

        fn write_lease(&self, session: SessionProcess, session_id: Option<&str>) {
            session_leases::write(self.state_directory(), &self.key(), session, session_id)
                .unwrap();
        }

        fn lease_count(&self) -> usize {
            session_leases::live(self.state_directory(), &self.key(), &|_| true).len()
        }

        fn is_listed(&self) -> bool {
            list_worktrees(&self.main)
                .unwrap()
                .iter()
                .any(|entry| entry.is_at(&self.prepared.removal.path))
        }

        fn registered_names(&self) -> Vec<String> {
            let machine = &self.prepared.instance.as_ref().unwrap().machine;
            machine
                .store
                .read()
                .unwrap()
                .instances
                .iter()
                .map(|instance| instance.name.clone())
                .collect()
        }
    }

    fn runner() -> FakeComposeRunner {
        FakeComposeRunner::default()
            .with_project_containers(vec!["container-1".to_owned()])
            .with_volumes(vec!["volume-1".to_owned()])
    }

    fn deletion_commands(runner: &FakeComposeRunner) -> Vec<&'static str> {
        runner
            .calls()
            .iter()
            .map(|call| call.command)
            .filter(|command| matches!(*command, "docker rm" | "volume rm" | "network rm"))
            .collect()
    }

    fn removal_context<'a>(
        runner: &'a FakeComposeRunner,
        processes: &'a FakeProcesses,
        requester: Requester,
        is_forced: bool,
        state_directory: &'a Path,
    ) -> RemovalContext<'a> {
        RemovalContext {
            runner,
            processes,
            state_directory,
            requester,
            is_forced,
            shown_work: None,
        }
    }

    const HOOK: Requester = Requester::Session { claude_pid: None };

    #[test]
    fn should_refuse_and_change_nothing_when_another_session_uses_the_worktree() {
        let fixture = fixture();
        let runner = runner();
        let processes = FakeProcesses::answering(vec![other_session()]);

        let failure = remove_prepared(
            &fixture.prepared,
            &removal_context(&runner, &processes, HOOK, false, fixture.state_directory()),
        )
        .unwrap_err();

        assert!(matches!(failure, RemovalFailure::InUse { .. }));
        assert_eq!(failure.exit_code(), exit_code::ERROR);
        assert_eq!(processes.stop_calls.get(), 0);
        assert_eq!(deletion_commands(&runner), Vec::<&str>::new());
        assert_eq!(
            fixture.registered_names(),
            vec![INSTANCE_NAME, NEIGHBOUR_NAME]
        );
        assert!(fixture.is_listed());
    }

    #[test]
    fn should_refuse_and_change_nothing_when_a_process_appears_while_the_targets_stop() {
        let fixture = fixture();
        let runner = runner();
        let processes = FakeProcesses::answering(vec![Vec::new(), other_session()]);

        let failure = remove_prepared(
            &fixture.prepared,
            &removal_context(&runner, &processes, HOOK, false, fixture.state_directory()),
        )
        .unwrap_err();

        assert!(matches!(failure, RemovalFailure::InUse { .. }));
        assert_eq!(processes.stop_calls.get(), 1);
        assert_eq!(deletion_commands(&runner), Vec::<&str>::new());
        assert_eq!(
            fixture.registered_names(),
            vec![INSTANCE_NAME, NEIGHBOUR_NAME]
        );
        assert!(fixture.is_listed());
    }

    #[test]
    fn should_remove_the_worktree_and_keep_the_neighbour_instance_when_nobody_else_uses_it() {
        let fixture = fixture();
        let runner = runner();
        let processes = FakeProcesses::answering(Vec::new());

        remove_prepared(
            &fixture.prepared,
            &removal_context(&runner, &processes, HOOK, false, fixture.state_directory()),
        )
        .unwrap();

        assert_eq!(deletion_commands(&runner), vec!["docker rm", "volume rm"]);
        assert!(
            runner
                .calls()
                .iter()
                .all(|call| call.project != NEIGHBOUR_NAME)
        );
        assert_eq!(fixture.registered_names(), vec![NEIGHBOUR_NAME]);
        assert!(!fixture.is_listed());
    }

    #[test]
    fn should_refuse_and_delete_nothing_when_work_is_saved_while_the_processes_stop() {
        let fixture = fixture();
        let runner = runner();
        let file = fixture.prepared.removal.path.join("notes.txt");
        let processes = FakeProcesses::answering(Vec::new()).creating_on_stop(file.clone());
        let context = RemovalContext {
            shown_work: Some(UnsavedWork {
                uncommitted_changes: 0,
                unreachable_commits: 0,
            }),
            ..removal_context(
                &runner,
                &processes,
                Requester::Terminal,
                false,
                fixture.state_directory(),
            )
        };

        let failure = remove_prepared(&fixture.prepared, &context).unwrap_err();

        assert!(matches!(failure, RemovalFailure::UnsavedWork { .. }));
        assert_eq!(failure.exit_code(), exit_code::ERROR);
        assert!(failure.to_string().contains("1 uncommitted change(s)"));
        assert!(file.exists());
        assert!(fixture.is_listed());
        assert_eq!(deletion_commands(&runner), Vec::<&str>::new());
        assert_eq!(
            fixture.registered_names(),
            vec![INSTANCE_NAME, NEIGHBOUR_NAME]
        );
    }

    #[test]
    fn should_remove_the_worktree_when_work_is_saved_while_the_processes_stop_and_forced() {
        let fixture = fixture();
        let runner = runner();
        let file = fixture.prepared.removal.path.join("notes.txt");
        let processes = FakeProcesses::answering(Vec::new()).creating_on_stop(file);
        let context = RemovalContext {
            shown_work: Some(UnsavedWork {
                uncommitted_changes: 0,
                unreachable_commits: 0,
            }),
            ..removal_context(
                &runner,
                &processes,
                Requester::Terminal,
                true,
                fixture.state_directory(),
            )
        };

        remove_prepared(&fixture.prepared, &context).unwrap();

        assert!(!fixture.is_listed());
    }

    #[test]
    fn should_see_work_that_grew_in_either_count() {
        let shown = UnsavedWork {
            uncommitted_changes: 2,
            unreachable_commits: 1,
        };
        let with = |uncommitted_changes, unreachable_commits| UnsavedWork {
            uncommitted_changes,
            unreachable_commits,
        };

        assert!(with(3, 1).has_grown_since(&shown));
        assert!(with(2, 2).has_grown_since(&shown));
        assert!(!with(2, 1).has_grown_since(&shown));
        assert!(!with(0, 0).has_grown_since(&shown));
    }

    #[test]
    fn should_refuse_a_hook_when_another_live_session_holds_a_lease_and_change_nothing() {
        let fixture = fixture();
        fixture.write_lease(session_process(7), Some("abc-1"));
        let runner = runner();
        let processes = FakeProcesses::answering(Vec::new())
            .with_live_sessions(vec![session_process(7), session_process(5)])
            .running_as(session_process(5));

        let failure = remove_prepared(
            &fixture.prepared,
            &removal_context(&runner, &processes, HOOK, false, fixture.state_directory()),
        )
        .unwrap_err();

        assert!(matches!(failure, RemovalFailure::SessionsActive { .. }));
        assert_eq!(failure.exit_code(), exit_code::ERROR);
        assert_eq!(
            failure.kept_reason(),
            "in use by another Claude Code session (session abc-1)"
        );
        assert_eq!(processes.stop_calls.get(), 0);
        assert_eq!(deletion_commands(&runner), Vec::<&str>::new());
        assert_eq!(fixture.lease_count(), 1);
        assert!(fixture.is_listed());
    }

    #[test]
    fn should_not_block_a_hook_on_the_lease_of_the_closing_session() {
        let fixture = fixture();
        fixture.write_lease(session_process(5), None);
        let runner = runner();
        let processes = FakeProcesses::answering(Vec::new())
            .with_live_sessions(vec![session_process(5)])
            .running_as(session_process(5));

        remove_prepared(
            &fixture.prepared,
            &removal_context(&runner, &processes, HOOK, false, fixture.state_directory()),
        )
        .unwrap();

        assert!(!fixture.is_listed());
    }

    #[test]
    fn should_block_a_terminal_on_any_live_lease_and_say_so_by_pid_without_a_session_id() {
        let fixture = fixture();
        fixture.write_lease(session_process(5), None);
        let runner = runner();
        let processes = FakeProcesses::answering(Vec::new())
            .with_live_sessions(vec![session_process(5)])
            .running_as(session_process(5));

        let failure = remove_prepared(
            &fixture.prepared,
            &removal_context(
                &runner,
                &processes,
                Requester::Terminal,
                false,
                fixture.state_directory(),
            ),
        )
        .unwrap_err();

        let message = failure.to_string();
        assert!(matches!(failure, RemovalFailure::SessionsActive { .. }));
        assert!(message.contains("another Claude Code session (claude 5)"));
        assert!(message.contains("without --force, ds worktree rm is refused too"));
        assert!(fixture.is_listed());
    }

    #[test]
    fn should_remove_the_worktree_and_delete_the_dead_lease_when_its_session_is_gone() {
        let fixture = fixture();
        fixture.write_lease(session_process(9), None);
        let runner = runner();
        let processes = FakeProcesses::answering(Vec::new());

        remove_prepared(
            &fixture.prepared,
            &removal_context(&runner, &processes, HOOK, false, fixture.state_directory()),
        )
        .unwrap();

        assert_eq!(fixture.lease_count(), 0);
        assert!(!fixture.is_listed());
    }

    #[test]
    fn should_ignore_the_leases_when_forced() {
        let fixture = fixture();
        fixture.write_lease(session_process(7), None);
        let runner = runner();
        let processes =
            FakeProcesses::answering(Vec::new()).with_live_sessions(vec![session_process(7)]);

        remove_prepared(
            &fixture.prepared,
            &removal_context(
                &runner,
                &processes,
                Requester::Terminal,
                true,
                fixture.state_directory(),
            ),
        )
        .unwrap();

        assert!(!fixture.is_listed());
    }

    #[test]
    fn should_remove_the_worktree_despite_another_session_when_forced() {
        let fixture = fixture();
        let runner = runner();
        let processes = FakeProcesses::answering(vec![other_session(), other_session()]);

        remove_prepared(
            &fixture.prepared,
            &removal_context(
                &runner,
                &processes,
                Requester::Terminal,
                true,
                fixture.state_directory(),
            ),
        )
        .unwrap();

        assert_eq!(fixture.registered_names(), vec![NEIGHBOUR_NAME]);
        assert!(!fixture.is_listed());
    }

    #[test]
    fn should_list_the_users_and_say_how_to_force_when_the_worktree_is_in_use() {
        let failure = RemovalFailure::InUse {
            path: PathBuf::from("/x/feat"),
            processes: vec![
                ProcessSummary {
                    pid: 1234,
                    comm: "claude".to_owned(),
                },
                ProcessSummary {
                    pid: 5678,
                    comm: "nvim".to_owned(),
                },
            ],
        };

        assert_eq!(
            failure.to_string(),
            "/x/feat is in use by another Claude Code session (1234 (claude), 5678 (nvim)); nothing was deleted (the worktree, its data and its slot are kept). Close them, or run ds worktree rm --force /x/feat from a terminal."
        );
    }

    #[test]
    fn should_not_name_a_claude_session_when_only_other_programs_use_the_worktree() {
        let failure = RemovalFailure::InUse {
            path: PathBuf::from("/x/feat"),
            processes: vec![ProcessSummary {
                pid: 5678,
                comm: "nvim".to_owned(),
            }],
        };

        assert!(
            failure
                .to_string()
                .starts_with("/x/feat is in use by 5678 (nvim); nothing")
        );
    }

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
