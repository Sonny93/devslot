//! `ds hook pre-tool-use`: runs before every Bash command and every file write
//! of an agent. A write tool is only checked by the worktree guard (see
//! `write_guard`). A Bash command is denied when it deletes devslot data, or
//! starts an environment in a main checkout that requires a worktree; then the
//! checkout is registered and the agent is told what it got. It never fails:
//! whatever goes wrong becomes context for the agent, and the exit code is 0.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use super::command_guard::{is_destructive_ds_command, is_environment_ds_command};
use super::input::{INPUT_TIMEOUT, read_input_limited};
use super::write_guard::{GuardOutcome, WRITE_BUDGET, guard_environment, guard_write};
use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::cli::style::Style;
use crate::commands::ensure::{EnsureResult, ensure_directory_within};
use crate::deadline::Deadline;
use crate::error::ToolError;
use crate::registry::record::InstanceRecord;

const HOOK_EVENT_NAME: &str = "PreToolUse";
pub(super) const DENY_REASON: &str = "ds rm, ds prune and ds worktree rm delete data, ds hook is run by Claude Code itself, and other agents work on this machine; ask the user to run it";
/// Claude Code kills the hook after 20 seconds; the work stops before that.
const HOOK_BUDGET: Duration = Duration::from_secs(15);
/// A `Write` or `MultiEdit` carries the whole file in its input.
const MAX_PRE_TOOL_USE_INPUT_BYTES: u64 = 32 * 1024 * 1024;
const PLAIN_STYLE: Style = Style { color: false };

#[derive(Debug, Deserialize)]
pub(super) struct PreToolUseInput {
    pub(super) cwd: PathBuf,
    #[serde(default)]
    pub(super) tool_name: Option<String>,
    #[serde(default)]
    pub(super) agent_id: Option<String>,
    #[serde(default)]
    pub(super) tool_input: Option<Value>,
}

impl PreToolUseInput {
    fn command(&self) -> Option<&str> {
        self.tool_input.as_ref()?.get("command")?.as_str()
    }
}

/// Reads the hook input from stdin and answers on stdout. Always returns 0.
/// The whole run, the read of stdin included, has [`HOOK_BUDGET`] for a Bash
/// command and [`WRITE_BUDGET`] for a write tool; a run that runs out of it
/// says nothing and lets the agent's call go.
pub fn run_pre_tool_use() -> u8 {
    let started = Instant::now();
    let deadline = Deadline::starting_at(started, HOOK_BUDGET);
    let input_timeout = deadline.clamp(INPUT_TIMEOUT).unwrap_or(Duration::ZERO);
    let answer = match read_input_limited::<PreToolUseInput>(
        std::io::stdin(),
        input_timeout,
        MAX_PRE_TOOL_USE_INPUT_BYTES,
    ) {
        Ok(input) => answer_for(&input, started),
        Err(error) => Some(context_answer(&format!(
            "the devslot hook could not read its input ({error}); tell the user"
        ))),
    };
    if let Some(answer) = answer {
        let _ = write_stdout(&answer);
    }
    exit_code::SUCCESS
}

fn answer_for(input: &PreToolUseInput, started: Instant) -> Option<String> {
    if input.is_write_tool() {
        let deadline = Deadline::starting_at(started, WRITE_BUDGET);
        return decide_write(|| guard_write(input, deadline));
    }
    let deadline = Deadline::starting_at(started, HOOK_BUDGET);
    decide(
        input,
        || guard_environment(input, deadline),
        |directory| ensure_directory_within(directory, PLAIN_STYLE, Some(deadline)),
    )
}

fn decide_write(guard: impl FnOnce() -> GuardOutcome) -> Option<String> {
    outcome_answer(guard())
}

fn decide(
    input: &PreToolUseInput,
    guard: impl FnOnce() -> GuardOutcome,
    ensure: impl FnOnce(&Path) -> Result<EnsureResult, ToolError>,
) -> Option<String> {
    let command = input.command();
    if command.is_some_and(is_destructive_ds_command) {
        return Some(deny_answer(DENY_REASON));
    }
    if command.is_some_and(is_environment_ds_command)
        && let Some(answer) = outcome_answer(guard())
    {
        return Some(answer);
    }
    context_for(&ensure(&input.cwd)).map(|context| context_answer(&context))
}

fn outcome_answer(outcome: GuardOutcome) -> Option<String> {
    match outcome {
        GuardOutcome::Clear => None,
        GuardOutcome::Deny(reason) => Some(deny_answer(&reason)),
        GuardOutcome::Undecided(message) => Some(context_answer(&message)),
    }
}

fn context_for(result: &Result<EnsureResult, ToolError>) -> Option<String> {
    match result {
        Ok(EnsureResult::Silent) => None,
        Ok(EnsureResult::Done { instance, lines }) => {
            let mut context = lines.join("\n");
            context.push('\n');
            context.push_str(&summary_of(instance));
            Some(context)
        }
        Ok(EnsureResult::Refused { lines }) => Some(format!(
            "devslot could not register this checkout: {}",
            lines.join("\n")
        )),
        Err(error) => Some(format!("devslot ensure failed: {error}")),
    }
}

fn summary_of(instance: &InstanceRecord) -> String {
    let ports: Vec<String> = instance
        .ports
        .iter()
        .map(|assigned| match &assigned.detail.service {
            Some(service) => format!("{}={} ({service})", assigned.variable, assigned.port),
            None => format!("{}={}", assigned.variable, assigned.port),
        })
        .collect();
    let ports = if ports.is_empty() {
        "none".to_owned()
    } else {
        ports.join(", ")
    };
    format!(
        "devslot: this checkout is {}, slot {}; ports: {ports}; run ds info --json for URLs",
        instance.name, instance.slot
    )
}

fn deny_answer(reason: &str) -> String {
    answer_of(&json!({
        "hookEventName": HOOK_EVENT_NAME,
        "permissionDecision": "deny",
        "permissionDecisionReason": reason,
    }))
}

fn context_answer(context: &str) -> String {
    answer_of(&json!({
        "hookEventName": HOOK_EVENT_NAME,
        "additionalContext": context,
    }))
}

fn answer_of(specific_output: &Value) -> String {
    let mut text = json!({ "hookSpecificOutput": specific_output }).to_string();
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::discovery::PortKind;
    use crate::port_range::PortRange;
    use crate::registry::record::{AssignedPort, PortDetail};

    fn input(command: Option<&str>) -> PreToolUseInput {
        PreToolUseInput {
            cwd: PathBuf::from("/work/app"),
            tool_name: Some("Bash".to_owned()),
            agent_id: None,
            tool_input: command.map(|command| json!({ "command": command })),
        }
    }

    fn clear() -> GuardOutcome {
        GuardOutcome::Clear
    }

    fn instance() -> InstanceRecord {
        let port = |variable: &str, number: u16, service: Option<&str>| AssignedPort {
            variable: variable.to_owned(),
            port: number,
            detail: PortDetail {
                kind: PortKind::Service,
                service: service.map(str::to_owned),
                target: None,
            },
        };
        InstanceRecord {
            path: PathBuf::from("/work/app"),
            name: "app-3f2a".to_owned(),
            slot: 1,
            range: PortRange {
                first: 20050,
                last: 20099,
            },
            ports: vec![
                port("PORT", 20050, None),
                port("DB_PORT", 20052, Some("postgres")),
            ],
        }
    }

    fn parsed(answer: &str) -> Value {
        serde_json::from_str(answer).unwrap()
    }

    #[test]
    fn should_deny_the_command_without_running_ensure_when_it_is_ds_rm() {
        let answer = decide(
            &input(Some("ds rm --yes")),
            || panic!("guard ran"),
            |_| panic!("ensure ran"),
        )
        .unwrap();

        let output = &parsed(&answer)["hookSpecificOutput"];
        assert_eq!(output["permissionDecision"], "deny");
        assert_eq!(output["permissionDecisionReason"], DENY_REASON);
        assert_eq!(output["hookEventName"], "PreToolUse");
    }

    #[test]
    fn should_answer_nothing_when_ensure_has_nothing_to_report() {
        let answer = decide(
            &input(Some("ls")),
            || panic!("guard ran"),
            |_| Ok(EnsureResult::Silent),
        );

        assert_eq!(answer, None);
    }

    #[test]
    fn should_run_ensure_when_the_tool_has_no_command() {
        let answer = decide(
            &input(None),
            || panic!("guard ran"),
            |_| Ok(EnsureResult::Silent),
        );

        assert_eq!(answer, None);
    }

    #[test]
    fn should_give_the_announcement_and_the_summary_when_the_checkout_was_registered() {
        let result = Ok(EnsureResult::Done {
            instance: instance(),
            lines: vec!["ds: registered /work/app as app-3f2a".to_owned()],
        });

        let context = context_for(&result).unwrap();

        assert_eq!(
            context,
            "ds: registered /work/app as app-3f2a\n\
             devslot: this checkout is app-3f2a, slot 1; ports: PORT=20050, DB_PORT=20052 (postgres); run ds info --json for URLs"
        );
    }

    #[test]
    fn should_give_the_refusal_lines_when_the_checkout_could_not_be_registered() {
        let result = Ok(EnsureResult::Refused {
            lines: vec!["first".to_owned(), "second".to_owned()],
        });

        assert_eq!(
            context_for(&result).unwrap(),
            "devslot could not register this checkout: first\nsecond"
        );
    }

    #[test]
    fn should_give_the_error_when_ensure_failed() {
        let result = Err(ToolError::WriteReport(std::io::Error::other("disk")));

        assert_eq!(
            context_for(&result).unwrap(),
            "devslot ensure failed: cannot write the report: disk"
        );
    }

    #[test]
    fn should_put_the_context_inside_hook_specific_output() {
        let answer = context_answer("hello");

        assert_eq!(
            parsed(&answer),
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "additionalContext": "hello"}})
        );
    }

    #[test]
    fn should_say_none_when_the_instance_has_no_port() {
        let mut empty = instance();
        empty.ports.clear();

        assert!(summary_of(&empty).contains("ports: none;"));
    }

    #[test]
    fn should_deny_without_running_ensure_when_the_guard_denies_ds_up() {
        let answer = decide(
            &input(Some("ds up --no-run")),
            || GuardOutcome::Deny("enter a worktree".to_owned()),
            |_| panic!("ensure ran"),
        )
        .unwrap();

        let output = &parsed(&answer)["hookSpecificOutput"];
        assert_eq!(output["permissionDecision"], "deny");
        assert_eq!(output["permissionDecisionReason"], "enter a worktree");
    }

    #[test]
    fn should_run_ensure_when_the_guard_is_clear_for_ds_up() {
        let answer = decide(&input(Some("ds up")), clear, |_| Ok(EnsureResult::Silent));

        assert_eq!(answer, None);
    }

    #[test]
    fn should_give_context_and_run_nothing_else_when_the_guard_cannot_decide_for_ds_run() {
        let answer = decide(
            &input(Some("ds run -- pnpm dev")),
            || GuardOutcome::Undecided("could not decide".to_owned()),
            |_| panic!("ensure ran"),
        )
        .unwrap();

        let output = &parsed(&answer)["hookSpecificOutput"];
        assert_eq!(output["additionalContext"], "could not decide");
        assert_eq!(output.get("permissionDecision"), None);
    }

    #[test]
    fn should_not_ask_the_guard_when_the_command_starts_no_environment() {
        let answer = decide(
            &input(Some("ds info --json")),
            || panic!("guard ran"),
            |_| Ok(EnsureResult::Silent),
        );

        assert_eq!(answer, None);
    }

    #[test]
    fn should_answer_nothing_when_the_write_guard_is_clear() {
        assert_eq!(decide_write(clear), None);
    }

    #[test]
    fn should_deny_a_write_with_the_reason_of_the_guard() {
        let answer = decide_write(|| GuardOutcome::Deny("enter a worktree".to_owned())).unwrap();

        let output = &parsed(&answer)["hookSpecificOutput"];
        assert_eq!(output["permissionDecision"], "deny");
        assert_eq!(output["permissionDecisionReason"], "enter a worktree");
    }

    #[test]
    fn should_give_context_and_no_decision_when_the_write_guard_cannot_decide() {
        let answer = decide_write(|| GuardOutcome::Undecided("boom".to_owned())).unwrap();

        assert_eq!(
            parsed(&answer),
            json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "additionalContext": "boom"}})
        );
    }

    #[test]
    fn should_know_the_write_tools_by_name() {
        let named = |name: &str| PreToolUseInput {
            tool_name: Some(name.to_owned()),
            ..input(None)
        };

        for tool in ["Edit", "Write", "MultiEdit", "NotebookEdit"] {
            assert!(named(tool).is_write_tool(), "{tool}");
        }
        for tool in ["Bash", "Read", "Glob"] {
            assert!(!named(tool).is_write_tool(), "{tool}");
        }
    }
}
