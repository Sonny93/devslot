//! The check on the session that runs `ds doctor`.

use super::finding::{Check, Finding, Level};
use crate::instance_processes::{CLAUDE_COMM, ProcessSummary};

const SESSION_VARIABLE: &str = "CLAUDE_PID";

/// What was found about the Claude Code session that runs `ds doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRoot {
    /// `CLAUDE_PID` is unset, or the platform cannot tell: nothing to check.
    NotInSession,
    /// The session root is named `claude`.
    Claude,
    /// The session root is named something else.
    Other(ProcessSummary),
    /// `CLAUDE_PID` is set but no ancestor is the session root.
    Missing,
}

/// Warns when the session would get no lease: the removal of a worktree by
/// another session would not see this session's worktrees in use.
pub fn check_session_root(root: &SessionRoot) -> Option<Finding> {
    let cause = match root {
        SessionRoot::NotInSession | SessionRoot::Claude => return None,
        SessionRoot::Other(process) => {
            format!("the session process {process} is not named `{CLAUDE_COMM}`")
        }
        SessionRoot::Missing => format!(
            "no ancestor is the session process (`{SESSION_VARIABLE}` is not an ancestor and none is named `{CLAUDE_COMM}`)"
        ),
    };
    Some(
        Finding::new(
            Level::Warning,
            Check::SessionProcessName,
            format!(
                "{cause}: devslot cannot record session leases, so a worktree removal by another session will not see this session's worktrees in use"
            ),
        )
        .for_key(SESSION_VARIABLE),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_not_warn_when_there_is_no_session() {
        assert_eq!(check_session_root(&SessionRoot::NotInSession), None);
    }

    #[test]
    fn should_not_warn_when_the_session_root_is_named_claude() {
        assert_eq!(check_session_root(&SessionRoot::Claude), None);
    }

    #[test]
    fn should_warn_with_the_pid_and_the_name_when_the_session_root_is_named_otherwise() {
        let root = SessionRoot::Other(ProcessSummary {
            pid: 42,
            comm: "node".to_owned(),
        });

        let finding = check_session_root(&root).unwrap();

        assert_eq!(
            (finding.level, finding.check, finding.key.as_deref()),
            (
                Level::Warning,
                Check::SessionProcessName,
                Some("CLAUDE_PID")
            )
        );
        assert!(finding.message.contains("42 (node)"));
        assert!(finding.message.contains("cannot record session leases"));
    }

    #[test]
    fn should_warn_when_no_session_root_is_found() {
        let finding = check_session_root(&SessionRoot::Missing).unwrap();

        assert_eq!(finding.check, Check::SessionProcessName);
        assert!(finding.message.contains("no ancestor"));
        assert!(finding.message.contains("cannot record session leases"));
    }
}
