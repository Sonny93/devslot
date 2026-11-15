//! `ds hook pre-tool-use`: runs before every Bash command of an agent. It
//! denies the commands that delete devslot data, then makes sure the checkout
//! is registered and tells the agent what it got. It never fails: whatever goes
//! wrong becomes context for the agent, and the exit code is 0.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use super::command_guard::is_destructive_ds_command;
use super::input::read_input;
use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::cli::style::Style;
use crate::commands::ensure::{EnsureResult, ensure_directory};
use crate::error::ToolError;
use crate::registry::record::InstanceRecord;

const HOOK_EVENT_NAME: &str = "PreToolUse";
const DENY_REASON: &str =
    "ds rm and ds prune delete data and other agents work on this machine; ask the user to run it";
const PLAIN_STYLE: Style = Style { color: false };

#[derive(Debug, Deserialize)]
struct PreToolUseInput {
    cwd: PathBuf,
    #[serde(default)]
    tool_input: Option<Value>,
}

impl PreToolUseInput {
    fn command(&self) -> Option<&str> {
        self.tool_input.as_ref()?.get("command")?.as_str()
    }
}

/// Reads the hook input from stdin and answers on stdout. Always returns 0.
pub fn run_pre_tool_use() -> u8 {
    match read_input::<PreToolUseInput>(std::io::stdin()) {
        Ok(input) => {
            if let Some(answer) =
                decide(&input, |directory| ensure_directory(directory, PLAIN_STYLE))
            {
                let _ = write_stdout(&answer);
            }
        }
        Err(error) => eprintln!("ds: {error}"),
    }
    exit_code::SUCCESS
}

fn decide(
    input: &PreToolUseInput,
    ensure: impl FnOnce(&Path) -> Result<EnsureResult, ToolError>,
) -> Option<String> {
    if input.command().is_some_and(is_destructive_ds_command) {
        return Some(deny_answer());
    }
    context_for(&ensure(&input.cwd)).map(|context| context_answer(&context))
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

fn deny_answer() -> String {
    answer_of(&json!({
        "hookEventName": HOOK_EVENT_NAME,
        "permissionDecision": "deny",
        "permissionDecisionReason": DENY_REASON,
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
            tool_input: command.map(|command| json!({ "command": command })),
        }
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
        let answer = decide(&input(Some("ds rm --yes")), |_| panic!("ensure ran")).unwrap();

        let output = &parsed(&answer)["hookSpecificOutput"];
        assert_eq!(output["permissionDecision"], "deny");
        assert_eq!(output["permissionDecisionReason"], DENY_REASON);
        assert_eq!(output["hookEventName"], "PreToolUse");
    }

    #[test]
    fn should_answer_nothing_when_ensure_has_nothing_to_report() {
        let answer = decide(&input(Some("ls")), |_| Ok(EnsureResult::Silent));

        assert_eq!(answer, None);
    }

    #[test]
    fn should_run_ensure_when_the_tool_has_no_command() {
        let answer = decide(&input(None), |_| Ok(EnsureResult::Silent));

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
}
