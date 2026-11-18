//! The processes an instance runs in its worktree. Removing a worktree stops
//! the ones that belong to the instance (they listen on one of its ports, or
//! carry its `DEVSLOT_INSTANCE` marker) and lists the others without touching
//! them: a tmux server, an ssh master, an editor or another session can have
//! its working directory in a worktree. It also tells which processes belong
//! to somebody else than whoever asks for the removal, so the removal can be
//! refused. Linux only: elsewhere nothing is found and nothing is stopped.

use std::fmt;
use std::path::Path;

const CLAUDE_PID_VARIABLE: &str = "CLAUDE_PID";

#[cfg(target_os = "linux")]
mod foreign;
#[cfg(target_os = "linux")]
mod proc_root;
#[cfg(target_os = "linux")]
mod scan;
#[cfg(target_os = "linux")]
mod session;
#[cfg(target_os = "linux")]
mod stop;
#[cfg(test)]
mod testing;

/// The variable that only the processes devslot launches carry
/// (`ds run`, the `run` key of `ds up`), never `ds env` nor the `.env` block.
pub const RUN_VARIABLE: &str = "DEVSLOT_RUN";
const INSTANCE_VARIABLE: &str = "DEVSLOT_INSTANCE";

/// The environment entry that ties a process to an instance, besides its ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessMarker {
    /// `DEVSLOT_INSTANCE=<name>`, also exported by `ds env` and the `.env`
    /// block, so an editor started from such a shell carries it too.
    Instance,
    /// `DEVSLOT_RUN=<name>`, set only on what devslot launches.
    Run,
}

impl ProcessMarker {
    /// The name of the environment variable.
    pub const fn variable(self) -> &'static str {
        match self {
            Self::Instance => INSTANCE_VARIABLE,
            Self::Run => RUN_VARIABLE,
        }
    }
}

/// The instance registered for a worktree: what ties a process to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceScope {
    /// The instance name, the value of the marker variable.
    pub name: String,
    /// The ports registered for the instance.
    pub ports: Vec<u16>,
    /// The environment entry that marks a process of the instance.
    pub marker: ProcessMarker,
}

/// A process, as shown to the user: its id and its kernel name, never its
/// command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSummary {
    /// The process id.
    pub pid: i32,
    /// `/proc/<pid>/comm`.
    pub comm: String,
}

impl fmt::Display for ProcessSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.pid, self.comm)
    }
}

/// The processes found in a worktree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessReport {
    /// The processes that belong to the instance: to stop, or stopped.
    pub targets: Vec<ProcessSummary>,
    /// The processes that are still there: never signalled, or targets that
    /// survived SIGKILL.
    pub others: Vec<ProcessSummary>,
}

impl ProcessReport {
    /// The `ds: still running in <path>: ...` line, when there is something to say.
    pub fn still_running_line(&self, worktree: &Path) -> Option<String> {
        if self.others.is_empty() {
            return None;
        }
        Some(format!(
            "ds: still running in {}: {}",
            worktree.display(),
            join_summaries(&self.others)
        ))
    }

    /// One `ds: stopped <pid> (<comm>)` line per stopped process.
    pub fn stopped_lines(&self) -> Vec<String> {
        self.targets
            .iter()
            .map(|summary| format!("ds: stopped {summary}"))
            .collect()
    }
}

/// The kernel name of a Claude Code session's process.
pub const CLAUDE_COMM: &str = "claude";

/// A Claude Code session, told apart from any later process that reuses its
/// pid by the time the process started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionProcess {
    /// The pid of the `claude` process.
    pub pid: i32,
    /// When it started, in clock ticks since boot (`/proc/<pid>/stat`).
    pub start_time: u64,
}

/// `CLAUDE_PID` from the environment: the `claude` process Claude Code names
/// for its hooks, to be trusted only when it is an ancestor of `ds`.
pub fn claude_pid_from_environment() -> Option<i32> {
    std::env::var(CLAUDE_PID_VARIABLE).ok()?.trim().parse().ok()
}

/// Who asks for the removal of a worktree, which decides what counts as
/// somebody else's process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requester {
    /// A Claude Code session closing: its own process tree is not foreign.
    Session {
        /// `CLAUDE_PID` from the hook environment: the session's `claude`
        /// process, used only when it is an ancestor of `ds`.
        claude_pid: Option<i32>,
    },
    /// A person at a terminal: the instance's processes are shown in the
    /// preview and stopped after confirmation, so they are not foreign.
    Terminal,
}

/// What the removal of a worktree asks of the processes of the machine.
pub trait ProcessControl {
    /// See [`find_instance_processes`].
    fn find(&self, worktree: &Path, scope: Option<&InstanceScope>) -> ProcessReport;

    /// See [`stop_instance_processes`].
    fn stop(&self, worktree: &Path, scope: Option<&InstanceScope>) -> StopReport;

    /// See [`find_foreign_processes`].
    fn foreign(
        &self,
        worktree: &Path,
        scope: Option<&InstanceScope>,
        requester: Requester,
    ) -> Vec<ProcessSummary>;

    /// See [`is_session_alive`].
    fn is_session_alive(&self, session: SessionProcess) -> bool;

    /// The session `requester` is: the one that closes, none for a terminal.
    fn own_session(&self, requester: Requester) -> Option<SessionProcess>;
}

/// The processes of this machine, read from `/proc`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemProcesses;

impl ProcessControl for SystemProcesses {
    fn find(&self, worktree: &Path, scope: Option<&InstanceScope>) -> ProcessReport {
        find_instance_processes(worktree, scope)
    }

    fn stop(&self, worktree: &Path, scope: Option<&InstanceScope>) -> StopReport {
        stop_instance_processes(worktree, scope)
    }

    fn foreign(
        &self,
        worktree: &Path,
        scope: Option<&InstanceScope>,
        requester: Requester,
    ) -> Vec<ProcessSummary> {
        find_foreign_processes(worktree, scope, requester)
    }

    fn is_session_alive(&self, session: SessionProcess) -> bool {
        is_session_alive(session)
    }

    fn own_session(&self, requester: Requester) -> Option<SessionProcess> {
        match requester {
            Requester::Session { claude_pid } => current_session(claude_pid),
            Requester::Terminal => None,
        }
    }
}

/// `1 (a), 2 (b)`.
pub fn join_summaries(summaries: &[ProcessSummary]) -> String {
    summaries
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// What stopping the instance's processes did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StopReport {
    /// What was stopped and what was only found.
    pub report: ProcessReport,
    /// Targets that did not exit even after SIGKILL.
    pub survivors: Vec<ProcessSummary>,
}

/// Lists, without signalling, what `stop_instance_processes` would stop
/// (`targets`) and what it would leave alone (`others`). `worktree` is the
/// canonical root of the worktree; `scope` is `None` when no instance is
/// registered for it, so nothing is a target.
#[cfg(target_os = "linux")]
pub fn find_instance_processes(worktree: &Path, scope: Option<&InstanceScope>) -> ProcessReport {
    let root = proc_root::ProcRoot::system();
    let found = scan::scan(&system_context(&root, worktree, scope));
    ProcessReport {
        targets: found.targets,
        others: found.others,
    }
}

/// Stops the processes of the instance in `worktree` (same arguments as
/// [`find_instance_processes`]): SIGTERM, a wait of up to 5 seconds, a rescan
/// for respawned children, at most 3 rounds, then SIGKILL.
#[cfg(target_os = "linux")]
pub fn stop_instance_processes(worktree: &Path, scope: Option<&InstanceScope>) -> StopReport {
    let root = proc_root::ProcRoot::system();
    let context = system_context(&root, worktree, scope);
    let signaller = stop::KernelSignaller::new(&context);
    let outcome = stop::stop(&context, &signaller, &stop::StopPolicy::default());
    StopReport {
        report: ProcessReport {
            targets: outcome.stopped,
            others: outcome.others,
        },
        survivors: outcome.survivors,
    }
}

/// The processes in `worktree` that belong to somebody else than `requester`
/// and are still running, sorted by pid. Orphaned daemons (parent pid 1 or
/// `systemd`) are never counted. Nothing is signalled.
#[cfg(target_os = "linux")]
pub fn find_foreign_processes(
    worktree: &Path,
    scope: Option<&InstanceScope>,
    requester: Requester,
) -> Vec<ProcessSummary> {
    let root = proc_root::ProcRoot::system();
    foreign::foreign_processes(&system_context(&root, worktree, scope), requester)
}

/// The Claude Code session that runs `ds`: `claude_pid` when it is an
/// ancestor of `ds`, otherwise the nearest ancestor named `claude`; `None`
/// when there is none, or when that process is not named `claude` (a lease
/// for it would be seen dead at every check).
#[cfg(target_os = "linux")]
pub fn current_session(claude_pid: Option<i32>) -> Option<SessionProcess> {
    let root = proc_root::ProcRoot::system();
    session::current_session(&root, rustix::process::getpid().as_raw_pid(), claude_pid)
}

/// The root of the session that runs `ds` (same rule as [`current_session`],
/// without requiring the name `claude`) with its kernel name; `None` when
/// there is no root.
#[cfg(target_os = "linux")]
pub fn session_root_process(claude_pid: Option<i32>) -> Option<ProcessSummary> {
    let root = proc_root::ProcRoot::system();
    session::session_root_process(&root, rustix::process::getpid().as_raw_pid(), claude_pid)
}

/// Whether `session` still runs: its pid exists, is not a zombie, is named
/// `claude` and started when the lease says.
#[cfg(target_os = "linux")]
pub fn is_session_alive(session: SessionProcess) -> bool {
    session::is_session_alive(&proc_root::ProcRoot::system(), session)
}

#[cfg(target_os = "linux")]
fn system_context<'a>(
    root: &'a proc_root::ProcRoot,
    worktree: &'a Path,
    scope: Option<&'a InstanceScope>,
) -> scan::ScanContext<'a> {
    scan::ScanContext {
        root,
        own_pid: rustix::process::getpid().as_raw_pid(),
        own_uid: rustix::process::getuid().as_raw(),
        worktree,
        scope,
    }
}

/// Nothing is found outside Linux.
#[cfg(not(target_os = "linux"))]
pub fn find_instance_processes(_worktree: &Path, _scope: Option<&InstanceScope>) -> ProcessReport {
    ProcessReport::default()
}

/// Nothing is found outside Linux.
#[cfg(not(target_os = "linux"))]
pub fn find_foreign_processes(
    _worktree: &Path,
    _scope: Option<&InstanceScope>,
    _requester: Requester,
) -> Vec<ProcessSummary> {
    Vec::new()
}

/// No session is found outside Linux.
#[cfg(not(target_os = "linux"))]
pub fn current_session(_claude_pid: Option<i32>) -> Option<SessionProcess> {
    None
}

/// No session root is found outside Linux.
#[cfg(not(target_os = "linux"))]
pub fn session_root_process(_claude_pid: Option<i32>) -> Option<ProcessSummary> {
    None
}

/// No session is known to run outside Linux.
#[cfg(not(target_os = "linux"))]
pub fn is_session_alive(_session: SessionProcess) -> bool {
    false
}

/// Nothing is stopped outside Linux.
#[cfg(not(target_os = "linux"))]
pub fn stop_instance_processes(_worktree: &Path, _scope: Option<&InstanceScope>) -> StopReport {
    StopReport::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(pid: i32, comm: &str) -> ProcessSummary {
        ProcessSummary {
            pid,
            comm: comm.to_owned(),
        }
    }

    #[test]
    fn should_say_nothing_when_no_other_process_is_left() {
        let report = ProcessReport::default();

        assert_eq!(report.still_running_line(Path::new("/w")), None);
    }

    #[test]
    fn should_list_the_other_processes_by_pid_and_name() {
        let report = ProcessReport {
            targets: Vec::new(),
            others: vec![summary(7, "tmux: server"), summary(9, "ssh")],
        };

        assert_eq!(
            report.still_running_line(Path::new("/w")).unwrap(),
            "ds: still running in /w: 7 (tmux: server), 9 (ssh)"
        );
    }

    #[test]
    fn should_give_one_line_per_stopped_process() {
        let report = ProcessReport {
            targets: vec![summary(3, "node")],
            others: Vec::new(),
        };

        assert_eq!(report.stopped_lines(), vec!["ds: stopped 3 (node)"]);
    }
}
