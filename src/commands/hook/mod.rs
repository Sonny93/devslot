//! `ds hook <event>`: what the Claude Code plugin runs. Each hook reads one JSON
//! object on stdin; see the files of the events for what they print and return.

pub mod command_guard;
pub mod failure;
pub mod input;
pub mod pre_tool_use;
pub mod session_start;
pub mod worktree_create;
pub mod worktree_remove;

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
