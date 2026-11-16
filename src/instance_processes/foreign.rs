//! Telling the processes that belong to somebody else from the ones of whoever
//! asks for the removal of a worktree. A session that closes still has its own
//! tree in the worktree (its MCP servers, its subagents), which must not block;
//! another session that works there must. Orphaned daemons (an `adb` server,
//! an ssh master) are not anybody's and never block.

use std::collections::HashSet;

use super::scan::{ScanContext, scan, with_descendants};
use super::{CLAUDE_COMM, ProcessSummary, Requester};

const INIT_PID: i32 = 1;
const SUBREAPER_COMM: &str = "systemd";

/// The candidates of the worktree that are not the requester's and not
/// orphans, sorted by pid. A session's candidates include the instance's
/// processes (they may be another session's servers); a terminal's do not,
/// since it stops them after showing them.
pub fn foreign_processes(context: &ScanContext, requester: Requester) -> Vec<ProcessSummary> {
    let found = scan(context);
    let candidates: Vec<ProcessSummary> = match requester {
        Requester::Session { .. } => found.targets.into_iter().chain(found.others).collect(),
        Requester::Terminal => found.others,
    };
    let own = own_pids(context, requester);
    let mut foreign: Vec<ProcessSummary> = candidates
        .into_iter()
        .filter(|candidate| !own.contains(&candidate.pid) && !is_orphan(context, candidate.pid))
        .collect();
    foreign.sort_by_key(|candidate| candidate.pid);
    foreign
}

fn own_pids(context: &ScanContext, requester: Requester) -> HashSet<i32> {
    let Requester::Session { claude_pid } = requester else {
        return HashSet::new();
    };
    session_root(context, claude_pid).map_or_default(|root| with_descendants(context, &[root]))
}

/// The `claude` process of the closing session: the hinted pid when it is an
/// ancestor of `ds`, otherwise the nearest ancestor named `claude`. Without
/// either there is no root and nothing is the session's own, so the removal
/// is refused when anything else runs in the worktree.
fn session_root(context: &ScanContext, claude_pid: Option<i32>) -> Option<i32> {
    let ancestors = ancestors_nearest_first(context);
    if let Some(hinted) = claude_pid.filter(|pid| ancestors.contains(pid)) {
        return Some(hinted);
    }
    ancestors
        .into_iter()
        .find(|pid| context.root.comm(*pid) == CLAUDE_COMM)
}

/// The proper ancestors of `ds`, parent first, without init and the user's
/// systemd instance: they are never the root of a session, and taking one
/// would make every process of the machine the session's own.
fn ancestors_nearest_first(context: &ScanContext) -> Vec<i32> {
    let mut ancestors = Vec::new();
    let mut next = context.root.stat(context.own_pid).map(|stat| stat.parent);
    while let Some(pid) = next.filter(|pid| *pid > 0 && !ancestors.contains(pid)) {
        ancestors.push(pid);
        next = context.root.stat(pid).map(|stat| stat.parent);
    }
    ancestors.retain(|pid| *pid != INIT_PID && !is_subreaper(context, *pid));
    ancestors
}

fn is_subreaper(context: &ScanContext, pid: i32) -> bool {
    context.root.comm(pid) == SUBREAPER_COMM
}

/// Whether nobody owns the process any more: its parent is init or a
/// subreaper. A process that vanished since the scan counts too. A `claude`
/// process never does: a session started from a systemd unit, `nohup` or
/// `setsid` is not a daemon.
fn is_orphan(context: &ScanContext, pid: i32) -> bool {
    if context.root.comm(pid) == CLAUDE_COMM {
        return false;
    }
    let Some(stat) = context.root.stat(pid) else {
        return true;
    };
    stat.parent == INIT_PID || is_subreaper(context, stat.parent)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::instance_processes::InstanceScope;
    use crate::instance_processes::testing::FakeProc;

    const OWN_PID: i32 = 100;
    const SHELL_PID: i32 = 20;
    const CLOSING_CLAUDE_PID: i32 = 50;
    const CLOSING_SH_PID: i32 = 60;
    const OTHER_CLAUDE_PID: i32 = 30;
    const INSTANCE: &str = "app-feat-3f2a";

    struct Setup {
        fake: FakeProc,
        worktree: PathBuf,
        scope: InstanceScope,
    }

    fn setup() -> Setup {
        let fake = FakeProc::new();
        let worktree = fake.directory("work/feat");
        let main = fake.directory("work/main");
        fake.add_process(1, 0, 1, "systemd", &fake.directory("/"));
        fake.add_process(SHELL_PID, 1, SHELL_PID, "zsh", &main);
        fake.add_process(CLOSING_CLAUDE_PID, SHELL_PID, 50, "claude", &main);
        fake.add_process(CLOSING_SH_PID, CLOSING_CLAUDE_PID, 50, "sh", &worktree);
        fake.add_process(OWN_PID, CLOSING_SH_PID, 50, "ds", &worktree);
        Setup {
            fake,
            worktree,
            scope: InstanceScope {
                name: INSTANCE.to_owned(),
                ports: Vec::new(),
            },
        }
    }

    impl Setup {
        fn foreign_pids(&self, requester: Requester) -> Vec<i32> {
            let root = self.fake.root();
            let context = ScanContext {
                root: &root,
                own_pid: OWN_PID,
                own_uid: 1000,
                worktree: &self.worktree,
                scope: Some(&self.scope),
            };
            foreign_processes(&context, requester)
                .iter()
                .map(|process| process.pid)
                .collect()
        }

        fn add_closing_session_mcp(&self) {
            self.fake.add_process(
                51,
                CLOSING_CLAUDE_PID,
                50,
                "node",
                &self.fake.directory("work/feat"),
            );
            self.fake
                .add_process(52, 51, 50, "bun", &self.fake.directory("work/feat"));
        }

        fn add_other_session(&self) {
            self.fake.add_process(
                OTHER_CLAUDE_PID,
                SHELL_PID,
                OTHER_CLAUDE_PID,
                "claude",
                &self.fake.directory("work/feat"),
            );
            self.fake.add_process(
                31,
                OTHER_CLAUDE_PID,
                OTHER_CLAUDE_PID,
                "node",
                &self.fake.directory("work/feat"),
            );
        }
    }

    const fn session(claude_pid: Option<i32>) -> Requester {
        Requester::Session { claude_pid }
    }

    #[test]
    fn should_not_block_on_the_mcp_server_of_the_closing_session() {
        let setup = setup();
        setup.add_closing_session_mcp();

        assert_eq!(
            setup.foreign_pids(session(Some(CLOSING_CLAUDE_PID))),
            Vec::<i32>::new()
        );
    }

    #[test]
    fn should_find_the_session_root_through_sh_when_there_is_no_claude_pid() {
        let setup = setup();
        setup.add_closing_session_mcp();

        assert_eq!(setup.foreign_pids(session(None)), Vec::<i32>::new());
    }

    #[test]
    fn should_ignore_a_claude_pid_that_is_not_an_ancestor_of_ds() {
        let setup = setup();
        setup.add_closing_session_mcp();
        setup.add_other_session();

        assert_eq!(
            setup.foreign_pids(session(Some(OTHER_CLAUDE_PID))),
            vec![OTHER_CLAUDE_PID, 31]
        );
    }

    #[test]
    fn should_block_on_another_claude_session_and_its_child() {
        let setup = setup();
        setup.add_closing_session_mcp();
        setup.add_other_session();

        assert_eq!(
            setup.foreign_pids(session(Some(CLOSING_CLAUDE_PID))),
            vec![OTHER_CLAUDE_PID, 31]
        );
    }

    #[test]
    fn should_block_on_a_server_of_the_instance_that_another_session_started() {
        let setup = setup();
        setup.fake.add_process(
            OTHER_CLAUDE_PID,
            SHELL_PID,
            OTHER_CLAUDE_PID,
            "claude",
            &setup.fake.directory("work/main"),
        );
        setup.fake.add_process(
            40,
            OTHER_CLAUDE_PID,
            OTHER_CLAUDE_PID,
            "node",
            &setup.worktree,
        );
        setup
            .fake
            .set_environment(40, &[&format!("DEVSLOT_INSTANCE={INSTANCE}")]);

        assert_eq!(setup.foreign_pids(session(None)), vec![40]);
    }

    #[test]
    fn should_not_block_on_an_orphaned_daemon_or_a_child_of_systemd() {
        let setup = setup();
        setup.fake.add_process(70, 1, 70, "adb", &setup.worktree);
        setup
            .fake
            .add_process(5, 1, 5, "systemd", &setup.fake.directory("/"));
        setup.fake.add_process(71, 5, 71, "ssh", &setup.worktree);

        assert_eq!(setup.foreign_pids(session(None)), Vec::<i32>::new());
        assert_eq!(setup.foreign_pids(Requester::Terminal), Vec::<i32>::new());
    }

    #[test]
    fn should_block_on_another_claude_session_from_a_terminal() {
        let setup = setup();
        setup.add_other_session();

        assert_eq!(
            setup.foreign_pids(Requester::Terminal),
            vec![OTHER_CLAUDE_PID, 31]
        );
    }

    #[test]
    fn should_not_block_on_a_server_of_the_instance_from_a_terminal() {
        let setup = setup();
        setup
            .fake
            .add_process(40, SHELL_PID, 40, "node", &setup.worktree);
        setup
            .fake
            .set_environment(40, &[&format!("DEVSLOT_INSTANCE={INSTANCE}")]);

        assert_eq!(setup.foreign_pids(Requester::Terminal), Vec::<i32>::new());
    }

    #[test]
    fn should_not_take_a_shared_ancestor_for_the_session_when_no_claude_is_an_ancestor() {
        let setup = setup();
        setup.fake.remove_process(SHELL_PID);
        setup.fake.remove_process(CLOSING_CLAUDE_PID);
        let main = setup.fake.directory("work/main");
        setup.fake.add_process(10, 1, 10, "tmux: server", &main);
        setup
            .fake
            .add_process(SHELL_PID, 10, SHELL_PID, "zsh", &setup.worktree);
        setup
            .fake
            .add_process(CLOSING_CLAUDE_PID, SHELL_PID, 50, "bash", &setup.worktree);
        setup.fake.add_process(21, 10, 21, "zsh", &main);
        setup.fake.add_process(
            OTHER_CLAUDE_PID,
            21,
            OTHER_CLAUDE_PID,
            "claude",
            &setup.worktree,
        );

        assert_eq!(setup.foreign_pids(session(None)), vec![OTHER_CLAUDE_PID]);
    }

    #[test]
    fn should_block_on_a_claude_session_whose_parent_is_init_or_systemd() {
        let setup = setup();
        setup.fake.add_process(
            OTHER_CLAUDE_PID,
            1,
            OTHER_CLAUDE_PID,
            "claude",
            &setup.worktree,
        );

        assert_eq!(
            setup.foreign_pids(session(Some(CLOSING_CLAUDE_PID))),
            vec![OTHER_CLAUDE_PID]
        );
        assert_eq!(
            setup.foreign_pids(Requester::Terminal),
            vec![OTHER_CLAUDE_PID]
        );
    }
}
