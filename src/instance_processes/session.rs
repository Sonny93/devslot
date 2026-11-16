//! Telling which Claude Code session runs `ds`, and whether a session that
//! was seen earlier still runs. A pid alone is not enough: the kernel reuses
//! them, so a session is its pid and its start time.

use super::foreign::session_root;
use super::proc_root::ProcRoot;
use super::{CLAUDE_COMM, SessionProcess};

/// The session that runs `ds` (see [`session_root`] for how it is found) with
/// its start time; `None` when there is none or it cannot be read.
pub fn current_session(
    root: &ProcRoot,
    own_pid: i32,
    claude_pid: Option<i32>,
) -> Option<SessionProcess> {
    let pid = session_root(root, own_pid, claude_pid)?;
    let stat = root.stat(pid)?;
    Some(SessionProcess {
        pid,
        start_time: stat.start_time,
    })
}

/// Whether `session` still runs: the pid exists and is not a zombie, its
/// kernel name is `claude`, and it started when the session did.
pub fn is_session_alive(root: &ProcRoot, session: SessionProcess) -> bool {
    let Some(stat) = root.stat(session.pid) else {
        return false;
    };
    !stat.is_dead()
        && stat.start_time == session.start_time
        && root.comm(session.pid) == CLAUDE_COMM
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance_processes::testing::{DEFAULT_START_TIME, FakeProc};

    const SHELL_PID: i32 = 20;
    const CLAUDE_PID: i32 = 50;
    const OWN_PID: i32 = 100;

    fn session() -> SessionProcess {
        SessionProcess {
            pid: CLAUDE_PID,
            start_time: DEFAULT_START_TIME,
        }
    }

    fn fake_with_session() -> FakeProc {
        let fake = FakeProc::new();
        let main = fake.directory("work/main");
        fake.add_process(1, 0, 1, "systemd", &fake.directory("/"));
        fake.add_process(SHELL_PID, 1, SHELL_PID, "zsh", &main);
        fake.add_process(CLAUDE_PID, SHELL_PID, CLAUDE_PID, "claude", &main);
        fake.add_process(OWN_PID, CLAUDE_PID, CLAUDE_PID, "ds", &main);
        fake
    }

    #[test]
    fn should_find_the_session_that_runs_ds_with_its_start_time() {
        let fake = fake_with_session();
        fake.set_start_time(CLAUDE_PID, 777);

        let found = current_session(&fake.root(), OWN_PID, None);

        assert_eq!(
            found,
            Some(SessionProcess {
                pid: CLAUDE_PID,
                start_time: 777
            })
        );
    }

    #[test]
    fn should_find_no_session_when_no_ancestor_is_named_claude() {
        let fake = fake_with_session();
        fake.remove_process(CLAUDE_PID);
        fake.add_process(
            CLAUDE_PID,
            SHELL_PID,
            CLAUDE_PID,
            "bash",
            &fake.directory("/"),
        );

        assert_eq!(current_session(&fake.root(), OWN_PID, None), None);
    }

    #[test]
    fn should_find_a_running_session_alive() {
        let fake = fake_with_session();

        assert!(is_session_alive(&fake.root(), session()));
    }

    #[test]
    fn should_find_a_session_dead_when_its_pid_is_gone() {
        let fake = fake_with_session();
        fake.remove_process(CLAUDE_PID);

        assert!(!is_session_alive(&fake.root(), session()));
    }

    #[test]
    fn should_find_a_session_dead_when_another_process_reused_its_pid() {
        let fake = fake_with_session();
        fake.set_start_time(CLAUDE_PID, DEFAULT_START_TIME + 1);

        assert!(!is_session_alive(&fake.root(), session()));
    }

    #[test]
    fn should_find_a_session_dead_when_its_pid_is_not_named_claude_any_more() {
        let fake = fake_with_session();
        fake.remove_process(CLAUDE_PID);
        fake.add_process(
            CLAUDE_PID,
            SHELL_PID,
            CLAUDE_PID,
            "sleep",
            &fake.directory("/"),
        );

        assert!(!is_session_alive(&fake.root(), session()));
    }

    #[test]
    fn should_find_a_session_dead_when_it_is_a_zombie() {
        let fake = fake_with_session();
        fake.set_state(CLAUDE_PID, 'Z');

        assert!(!is_session_alive(&fake.root(), session()));
    }
}
