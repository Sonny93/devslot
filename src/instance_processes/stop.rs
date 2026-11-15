//! Stopping the targets of a scan: SIGTERM, a grace period, a rescan for
//! respawned children, at most a few rounds, then SIGKILL for what is left.

use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{Pid, PidfdFlags, Signal};

use super::ProcessSummary;
use super::scan::{Scan, ScanContext, is_target, scan};

const GRACE_PERIOD: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const KILL_WAIT: Duration = Duration::from_secs(2);
const TERM_ROUNDS: u32 = 3;

/// How long and how often to wait.
#[derive(Debug, Clone, Copy)]
pub struct StopPolicy {
    /// How long a SIGTERM round waits for the targets to exit.
    pub grace_period: Duration,
    /// The delay between two checks.
    pub poll_interval: Duration,
    /// How long to wait after SIGKILL.
    pub kill_wait: Duration,
    /// How many SIGTERM rounds before SIGKILL.
    pub term_rounds: u32,
}

impl Default for StopPolicy {
    fn default() -> Self {
        Self {
            grace_period: GRACE_PERIOD,
            poll_interval: POLL_INTERVAL,
            kill_wait: KILL_WAIT,
            term_rounds: TERM_ROUNDS,
        }
    }
}

/// Which signal to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSignal {
    /// SIGTERM: ask to exit.
    Terminate,
    /// SIGKILL: no choice.
    Kill,
}

/// Sends signals; the kernel in production, a recorder in tests.
pub trait Signaller {
    /// Sends `signal` to the process `pid`. A process that is gone is not an error.
    fn send(&self, pid: i32, signal: StopSignal);
}

/// What stopping did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StopOutcome {
    /// The processes that were signalled and are gone.
    pub stopped: Vec<ProcessSummary>,
    /// The targets that are still there after SIGKILL.
    pub survivors: Vec<ProcessSummary>,
    /// The other processes of the worktree, never signalled.
    pub others: Vec<ProcessSummary>,
}

/// Signals through the kernel, one process at a time and never a group. It
/// opens a pidfd, checks again that the process is one `ds` may stop, then
/// signals through the pidfd, so a pid reused in between is never hit.
pub struct KernelSignaller<'a> {
    context: &'a ScanContext<'a>,
}

impl<'a> KernelSignaller<'a> {
    /// A signaller that applies the rules of `context`.
    pub const fn new(context: &'a ScanContext<'a>) -> Self {
        Self { context }
    }
}

impl Signaller for KernelSignaller<'_> {
    fn send(&self, pid: i32, signal: StopSignal) {
        let signal = match signal {
            StopSignal::Terminate => Signal::TERM,
            StopSignal::Kill => Signal::KILL,
        };
        let Some(pid) = Pid::from_raw(pid) else {
            return;
        };
        let Ok(pidfd) = rustix::process::pidfd_open(pid, PidfdFlags::empty()) else {
            return;
        };
        if !is_target(self.context, pid.as_raw_pid()) {
            return;
        }
        let _ = rustix::process::pidfd_send_signal(&pidfd, signal);
    }
}

/// Stops the instance's processes of the worktree described by `context`.
pub fn stop(context: &ScanContext, signaller: &dyn Signaller, policy: &StopPolicy) -> StopOutcome {
    let mut signalled_by_pid: BTreeMap<i32, ProcessSummary> = BTreeMap::new();
    let mut current = scan(context);
    for _ in 0..policy.term_rounds {
        if current.targets.is_empty() {
            break;
        }
        signal_round(signaller, &current, StopSignal::Terminate);
        remember(&mut signalled_by_pid, &current);
        wait_until_gone(context, &current, policy.grace_period, policy);
        current = scan(context);
    }
    if !current.targets.is_empty() {
        signal_round(signaller, &current, StopSignal::Kill);
        remember(&mut signalled_by_pid, &current);
        wait_until_gone(context, &current, policy.kill_wait, policy);
        current = scan(context);
    }
    outcome_of(signalled_by_pid, current)
}

fn signal_round(signaller: &dyn Signaller, current: &Scan, signal: StopSignal) {
    for target in &current.targets {
        signaller.send(target.pid, signal);
    }
}

fn remember(signalled_by_pid: &mut BTreeMap<i32, ProcessSummary>, current: &Scan) {
    for target in &current.targets {
        signalled_by_pid
            .entry(target.pid)
            .or_insert_with(|| target.clone());
    }
}

fn wait_until_gone(context: &ScanContext, current: &Scan, limit: Duration, policy: &StopPolicy) {
    let started = Instant::now();
    while started.elapsed() < limit && is_any_alive(context, current) {
        thread::sleep(policy.poll_interval);
    }
}

fn is_any_alive(context: &ScanContext, current: &Scan) -> bool {
    current.targets.iter().any(|target| {
        context
            .root
            .stat(target.pid)
            .is_some_and(|stat| !stat.is_dead())
    })
}

fn outcome_of(signalled_by_pid: BTreeMap<i32, ProcessSummary>, last: Scan) -> StopOutcome {
    let survivors: Vec<ProcessSummary> = last.targets.into_iter().collect();
    let stopped = signalled_by_pid
        .into_values()
        .filter(|summary| survivors.iter().all(|survivor| survivor.pid != summary.pid))
        .collect();
    StopOutcome {
        stopped,
        survivors,
        others: last.others,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::instance_processes::InstanceScope;
    use crate::instance_processes::testing::FakeProc;

    const OWN_PID: i32 = 100;
    const INSTANCE: &str = "app-feat-3f2a";
    const QUICK_POLICY: StopPolicy = StopPolicy {
        grace_period: Duration::from_millis(30),
        poll_interval: Duration::from_millis(5),
        kill_wait: Duration::from_millis(30),
        term_rounds: 3,
    };

    struct Recorder<'a> {
        fake: &'a FakeProc,
        exits_on_terminate: bool,
        sent: RefCell<Vec<(i32, StopSignal)>>,
    }

    impl Signaller for Recorder<'_> {
        fn send(&self, pid: i32, signal: StopSignal) {
            self.sent.borrow_mut().push((pid, signal));
            if signal == StopSignal::Kill || self.exits_on_terminate {
                self.fake.remove_process(pid);
            }
        }
    }

    fn stop_in_fake(exits_on_terminate: bool) -> (StopOutcome, Vec<(i32, StopSignal)>) {
        let fake = FakeProc::new();
        let worktree = fake.directory("work/feat");
        fake.add_process(1, 0, 1, "systemd", &fake.directory("/"));
        fake.add_process(OWN_PID, 1, OWN_PID, "ds", &fake.directory("/"));
        fake.add_process(200, 1, 1, "node", &worktree);
        fake.set_environment(200, &[&format!("DEVSLOT_INSTANCE={INSTANCE}")]);
        fake.add_process(201, 1, 1, "tmux", &worktree);
        let root = fake.root();
        let scope = InstanceScope {
            name: INSTANCE.to_owned(),
            ports: Vec::new(),
        };
        let context = ScanContext {
            root: &root,
            own_pid: OWN_PID,
            own_uid: 1000,
            worktree: &worktree,
            scope: Some(&scope),
        };
        let recorder = Recorder {
            fake: &fake,
            exits_on_terminate,
            sent: RefCell::new(Vec::new()),
        };

        let outcome = stop(&context, &recorder, &QUICK_POLICY);

        (outcome, recorder.sent.into_inner())
    }

    #[test]
    fn should_only_terminate_a_target_that_exits_and_leave_the_other_processes_alone() {
        let (outcome, sent) = stop_in_fake(true);

        assert_eq!(sent, vec![(200, StopSignal::Terminate)]);
        assert_eq!(
            outcome.stopped,
            vec![ProcessSummary {
                pid: 200,
                comm: "node".to_owned()
            }]
        );
        assert_eq!(outcome.survivors, Vec::new());
        assert_eq!(
            outcome.others,
            vec![ProcessSummary {
                pid: 201,
                comm: "tmux".to_owned()
            }]
        );
    }

    #[test]
    fn should_kill_a_target_that_ignores_sigterm_after_the_rounds() {
        let (outcome, sent) = stop_in_fake(false);

        let terminations = sent
            .iter()
            .filter(|(_, signal)| *signal == StopSignal::Terminate)
            .count();
        assert_eq!(terminations, 3);
        assert_eq!(sent.last(), Some(&(200, StopSignal::Kill)));
        assert_eq!(outcome.stopped.len(), 1);
        assert_eq!(outcome.survivors, Vec::new());
    }
}
