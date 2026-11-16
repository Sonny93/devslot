//! A time budget for a command that something else kills after a fixed delay.
//! The steps that already have their own timeout clamp it to what is left, so
//! the command ends by itself before it is killed.

use std::time::{Duration, Instant};

use thiserror::Error;

/// The budget ran out before the work was done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("the time budget of this command ran out")]
pub struct BudgetExhausted;

/// A start and a budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deadline {
    started: Instant,
    budget: Duration,
}

impl Deadline {
    /// A budget that starts now.
    pub fn starting_now(budget: Duration) -> Self {
        Self::starting_at(Instant::now(), budget)
    }

    /// A budget that started at `started`.
    pub const fn starting_at(started: Instant, budget: Duration) -> Self {
        Self { started, budget }
    }

    /// What is left of the budget.
    pub fn remaining(&self) -> Duration {
        self.remaining_at(Instant::now())
    }

    /// What is left of the budget at `now`.
    pub fn remaining_at(&self, now: Instant) -> Duration {
        self.budget
            .saturating_sub(now.saturating_duration_since(self.started))
    }

    /// Whether nothing is left.
    pub fn is_exhausted(&self) -> bool {
        self.remaining().is_zero()
    }

    /// `default`, or what is left when that is shorter; `None` when nothing is
    /// left.
    pub fn clamp(&self, default: Duration) -> Option<Duration> {
        self.clamp_leaving(default, Duration::ZERO)
    }

    /// Like [`Deadline::clamp`], keeping `margin` of the budget untouched;
    /// `None` when no more than `margin` is left.
    pub fn clamp_leaving(&self, default: Duration, margin: Duration) -> Option<Duration> {
        self.clamp_leaving_at(Instant::now(), default, margin)
    }

    /// Like [`Deadline::clamp_leaving`], at `now`.
    pub fn clamp_leaving_at(
        &self,
        now: Instant,
        default: Duration,
        margin: Duration,
    ) -> Option<Duration> {
        let usable = self.remaining_at(now).saturating_sub(margin);
        if usable.is_zero() {
            return None;
        }
        Some(default.min(usable))
    }
}

/// The timeout of a step whose own limit is `default`: that limit without a
/// deadline, the clamped one with it.
pub fn timeout_within(
    deadline: Option<Deadline>,
    default: Duration,
) -> Result<Duration, BudgetExhausted> {
    let Some(deadline) = deadline else {
        return Ok(default);
    };
    deadline.clamp(default).ok_or(BudgetExhausted)
}

/// Whether there is a deadline and it has passed: a step that timed out then
/// ran out of budget rather than out of its own limit.
pub fn has_run_out(deadline: Option<Deadline>) -> bool {
    deadline.is_some_and(|deadline| deadline.is_exhausted())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    const BUDGET: Duration = Duration::from_secs(15);

    fn seconds(count: u64) -> Duration {
        Duration::from_secs(count)
    }

    #[test]
    fn should_clamp_to_the_default_when_plenty_of_time_remains() {
        let start = Instant::now();
        let deadline = Deadline::starting_at(start, BUDGET);

        let clamped = deadline.clamp_leaving_at(start + seconds(1), seconds(5), Duration::ZERO);

        assert_eq!(clamped, Some(seconds(5)));
    }

    #[test]
    fn should_clamp_to_the_remaining_time_when_it_is_shorter_than_the_default() {
        let start = Instant::now();
        let deadline = Deadline::starting_at(start, BUDGET);

        let clamped = deadline.clamp_leaving_at(start + seconds(12), seconds(5), Duration::ZERO);

        assert_eq!(clamped, Some(seconds(3)));
    }

    #[test]
    fn should_clamp_to_nothing_when_the_budget_is_exhausted() {
        let start = Instant::now();
        let deadline = Deadline::starting_at(start, BUDGET);

        let clamped = deadline.clamp_leaving_at(start + seconds(15), seconds(5), Duration::ZERO);

        assert_eq!(clamped, None);
    }

    #[test]
    fn should_keep_the_margin_when_clamping() {
        let start = Instant::now();
        let deadline = Deadline::starting_at(start, BUDGET);

        let clamped = deadline.clamp_leaving_at(start + seconds(12), seconds(5), seconds(1));
        let at_margin = deadline.clamp_leaving_at(start + seconds(14), seconds(5), seconds(1));

        assert_eq!(clamped, Some(seconds(2)));
        assert_eq!(at_margin, None);
    }

    #[test]
    fn should_report_nothing_remaining_after_the_budget() {
        let start = Instant::now();
        let deadline = Deadline::starting_at(start, BUDGET);

        assert_eq!(deadline.remaining_at(start + seconds(20)), Duration::ZERO);
        assert_eq!(deadline.remaining_at(start + seconds(4)), seconds(11));
    }

    #[test]
    fn should_keep_the_default_timeout_when_there_is_no_deadline() {
        assert_eq!(timeout_within(None, seconds(5)), Ok(seconds(5)));
        assert!(!has_run_out(None));
    }

    #[test]
    fn should_fail_the_timeout_when_the_deadline_is_exhausted() {
        let deadline = Deadline::starting_now(Duration::ZERO);

        assert_eq!(
            timeout_within(Some(deadline), seconds(5)),
            Err(BudgetExhausted)
        );
        assert!(has_run_out(Some(deadline)));
    }
}
