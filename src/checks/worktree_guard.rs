//! The checks on the worktree guard: whether it is on for the checkout, and the
//! keys that cannot switch it off.

use super::finding::{Check, Finding, Level};
use crate::project_config::{CONFIG_FILE, LOCAL_CONFIG_FILE};
use crate::worktree_guard::GuardStatus;

const REQUIRE_KEY: &str = "worktrees.require";

/// What the guard checks need to know about the checkout.
#[derive(Debug, Clone, Copy)]
pub struct GuardCheckInput {
    /// Whether the guard applies to the checkout.
    pub status: GuardStatus,
    /// Whether the checkout has an instance in the registry.
    pub is_registered: bool,
    /// The value of `[worktrees] require` in the committed `devslot.toml`.
    pub committed_key: Option<bool>,
    /// The value of `[worktrees] require` in a `devslot.local.toml` that git tracks.
    pub tracked_local_key: Option<bool>,
}

/// Says whether the guard is on for the checkout, warns about the committed
/// key that is ignored, and notes a main checkout registered before the guard.
pub fn check_worktree_guard(input: GuardCheckInput) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (file, key) in [
        (CONFIG_FILE, input.committed_key),
        (LOCAL_CONFIG_FILE, input.tracked_local_key),
    ] {
        if key.is_some() {
            findings.push(ignored_key_finding(file));
        }
    }
    findings.extend(status_finding(input.status));
    if input.status == GuardStatus::Active && input.is_registered {
        findings.push(Finding::new(
            Level::Info,
            Check::WorktreeGuardRegistered,
            "this main checkout was registered before the worktree guard covered it: ds ensure leaves its slot and its .env block as they are and no longer updates them",
        ));
    }
    findings
}

fn ignored_key_finding(file: &'static str) -> Finding {
    Finding::new(
        Level::Warning,
        Check::WorktreeRequireIgnored,
        format!(
            "[worktrees] require in {file} is ignored: only an untracked {LOCAL_CONFIG_FILE} can switch the worktree guard off for a checkout"
        ),
    )
    .in_file(file)
    .for_key(REQUIRE_KEY)
}

fn status_finding(status: GuardStatus) -> Option<Finding> {
    let message = match status {
        GuardStatus::NotConfigured => return None,
        GuardStatus::NotCovered => {
            "the worktree guard is not active: this checkout is outside [worktrees] require".to_owned()
        }
        GuardStatus::LinkedWorktree => {
            "the worktree guard is on and this is a linked worktree: writes and environments are allowed here".to_owned()
        }
        GuardStatus::DisabledLocally => format!(
            "the worktree guard is not active: {LOCAL_CONFIG_FILE} sets [worktrees] require = false"
        ),
        GuardStatus::Active => format!(
            "the worktree guard is active: agents must enter a worktree before writing files or running ds up, ds run and ds reset in this main checkout; [worktrees] require = false in {LOCAL_CONFIG_FILE} switches it off here"
        ),
    };
    Some(Finding::new(Level::Info, Check::WorktreeGuard, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(status: GuardStatus) -> GuardCheckInput {
        GuardCheckInput {
            status,
            is_registered: false,
            committed_key: None,
            tracked_local_key: None,
        }
    }

    fn checks_of(findings: &[Finding]) -> Vec<Check> {
        findings.iter().map(|finding| finding.check).collect()
    }

    #[test]
    fn should_say_nothing_when_the_guard_is_not_configured() {
        assert_eq!(check_worktree_guard(input(GuardStatus::NotConfigured)), []);
    }

    #[test]
    fn should_say_the_guard_is_active_for_a_covered_main_checkout() {
        let findings = check_worktree_guard(input(GuardStatus::Active));

        assert_eq!(checks_of(&findings), [Check::WorktreeGuard]);
        assert!(
            findings[0]
                .message
                .starts_with("the worktree guard is active")
        );
    }

    #[test]
    fn should_warn_that_the_committed_key_is_ignored() {
        let findings = check_worktree_guard(GuardCheckInput {
            committed_key: Some(false),
            ..input(GuardStatus::Active)
        });

        assert_eq!(
            checks_of(&findings),
            [Check::WorktreeRequireIgnored, Check::WorktreeGuard]
        );
        assert_eq!(findings[0].level, Level::Warning);
        assert_eq!(findings[0].file.as_deref(), Some("devslot.toml"));
    }

    #[test]
    fn should_mention_a_main_checkout_registered_before_the_guard() {
        let findings = check_worktree_guard(GuardCheckInput {
            is_registered: true,
            ..input(GuardStatus::Active)
        });

        assert_eq!(
            checks_of(&findings),
            [Check::WorktreeGuard, Check::WorktreeGuardRegistered]
        );
    }

    #[test]
    fn should_not_mention_the_registration_when_the_guard_is_not_active() {
        let findings = check_worktree_guard(GuardCheckInput {
            is_registered: true,
            ..input(GuardStatus::DisabledLocally)
        });

        assert_eq!(checks_of(&findings), [Check::WorktreeGuard]);
    }

    #[test]
    fn should_warn_that_a_tracked_local_file_key_is_ignored() {
        let findings = check_worktree_guard(GuardCheckInput {
            tracked_local_key: Some(false),
            ..input(GuardStatus::Active)
        });

        assert_eq!(findings[0].check, Check::WorktreeRequireIgnored);
        assert_eq!(findings[0].file.as_deref(), Some("devslot.local.toml"));
    }
}
