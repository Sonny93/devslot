//! `ds hook <event>`: what the Claude Code plugin runs. Each hook reads one JSON
//! object on stdin; see the files of the events for what they print and return.

pub mod command_guard;
pub mod failure;
pub mod input;
pub mod pre_tool_use;
pub mod session_start;
pub mod worktree_create;
pub mod worktree_remove;
pub mod write_guard;

use failure::HookFailure;

use crate::cli::args::HookEvent;

/// Runs the hook for `event` and returns its exit code.
pub fn run_hook(event: HookEvent) -> u8 {
    let result = match event {
        HookEvent::PreToolUse => return pre_tool_use::run_pre_tool_use(),
        HookEvent::SessionStart => session_start::run_session_start(),
        HookEvent::WorktreeCreate => worktree_create::run_worktree_create(),
        HookEvent::WorktreeRemove => worktree_remove::run_worktree_remove(),
    };
    exit_code_of(result)
}

fn exit_code_of(result: Result<u8, HookFailure>) -> u8 {
    match result {
        Ok(code) => code,
        Err(failure) => {
            eprintln!("ds: error: {failure}");
            failure.exit_code()
        }
    }
}

#[cfg(test)]
mod agent_rules_tests {
    use super::pre_tool_use::DENY_REASON;
    use super::session_start::GUIDANCE;

    const SKILL: &str = include_str!("../../../plugin/skills/devslot/SKILL.md");
    const README: &str = include_str!("../../../README.md");
    const RULES_HEADING: &str = "\n## Rules for agents";
    const NEXT_SECTION_HEADING: &str = "\n```\n\n## ";
    const DELETING_COMMANDS: [&str; 3] = ["ds rm", "ds prune", "ds worktree rm"];
    const STARTING_COMMAND: &str = "ds up --no-run";
    const LANDING_COMMAND: &str = "ds worktree land";

    fn readme_rules() -> &'static str {
        let from_heading = README
            .split_once(RULES_HEADING)
            .map_or("", |(_, rest)| rest);
        from_heading
            .split_once(NEXT_SECTION_HEADING)
            .map_or(from_heading, |(rules, _)| rules)
    }

    fn missing(copies: &[(&str, &str)], commands: &[&str]) -> Vec<String> {
        copies
            .iter()
            .flat_map(|(copy, text)| {
                commands
                    .iter()
                    .filter(|command| !text.contains(**command))
                    .map(move |command| format!("{command} is missing from {copy}"))
            })
            .collect()
    }

    #[test]
    fn should_name_the_same_commands_in_the_four_copies_of_the_agent_rules() {
        let all_commands = [STARTING_COMMAND, LANDING_COMMAND]
            .into_iter()
            .chain(DELETING_COMMANDS)
            .collect::<Vec<_>>();
        let mut problems = missing(
            &[
                ("SKILL.md", SKILL),
                ("the README rules for agents", readme_rules()),
                ("GUIDANCE", GUIDANCE),
            ],
            &all_commands,
        );
        problems.extend(missing(&[("DENY_REASON", DENY_REASON)], &DELETING_COMMANDS));

        assert_eq!(problems, Vec::<String>::new());
    }

    #[test]
    fn should_not_name_the_landing_command_in_the_deny_reason() {
        assert!(!DENY_REASON.contains(LANDING_COMMAND));
    }
}
