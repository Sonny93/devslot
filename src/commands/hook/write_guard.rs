//! The worktree guard of `ds hook pre-tool-use`: it refuses the write tools,
//! and `ds up`, `ds run` and `ds reset` in Bash, in a main checkout covered by
//! `[worktrees] require`. It refuses on a positive finding only: when it cannot
//! decide, it tells the agent so and lets the call go. It never registers a
//! checkout and never calls Docker.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::pre_tool_use::PreToolUseInput;
use crate::deadline::Deadline;
use crate::global_config::{GlobalConfig, home_from_environment};
use crate::paths::DevslotPaths;
use crate::worktree_guard::{
    GuardContext, GuardVerdict, MainCheckout, locate_required_main_checkout,
    nearest_existing_directory, read_layout, resolve_target,
};

/// The time the guard has for a write tool, which waits for every edit.
pub(super) const WRITE_BUDGET: Duration = Duration::from_secs(5);

pub(super) const WRITE_DENY_REASON: &str = "devslot refuses writes in the main checkout of this repository: work in a linked worktree. Enter one with the EnterWorktree tool, with a short new name that describes the task, then retry the edit there. After claude --continue, reuse the same worktree name. If EnterWorktree is not available, ask the user. Never work around this through Bash.";
pub(super) const SUBAGENT_WRITE_DENY_REASON: &str = "devslot refuses writes in the main checkout of this repository: work in a linked worktree. Do not call EnterWorktree from a subagent: stop and report that the main session must enter a worktree with EnterWorktree before it delegates write work. Never work around this through Bash.";
pub(super) const ENVIRONMENT_DENY_REASON: &str = "devslot refuses ds up, ds run and ds reset in the main checkout of this repository: start environments from a linked worktree. Enter one with the EnterWorktree tool, with a short new name that describes the task, then run the command there. Reading and searching here is fine. After claude --continue, reuse the same worktree name. If EnterWorktree is not available, ask the user.";
pub(super) const SUBAGENT_ENVIRONMENT_DENY_REASON: &str = "devslot refuses ds up, ds run and ds reset in the main checkout of this repository: start environments from a linked worktree. Do not call EnterWorktree from a subagent: stop and report that the main session must enter a worktree with EnterWorktree before it delegates this work.";
const WORKTREE_WRITE_DENY_REASON_PREFIX: &str = "devslot refuses writes in the main checkout of this repository, and this session already works in a linked worktree of it: write the same file in that worktree instead, at ";
const WORKTREE_WRITE_DENY_REASON_SUFFIX: &str = ". Never work around this through Bash.";
const WRITE_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// What the guard decided about a tool call.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum GuardOutcome {
    /// Nothing to say: the call goes.
    Clear,
    /// A positive finding: the call is refused for this reason.
    Deny(String),
    /// The guard could not decide: the agent is told, the call goes.
    Undecided(String),
}

impl PreToolUseInput {
    /// Whether the tool call writes a file.
    pub(super) fn is_write_tool(&self) -> bool {
        self.tool_name
            .as_deref()
            .is_some_and(|name| WRITE_TOOLS.contains(&name))
    }

    fn written_path(&self) -> Option<PathBuf> {
        let tool_input = self.tool_input.as_ref()?;
        let path = tool_input
            .get("file_path")
            .or_else(|| tool_input.get("notebook_path"))?;
        path.as_str().map(PathBuf::from)
    }

    fn is_from_subagent(&self) -> bool {
        self.agent_id.is_some()
    }
}

/// Decides a call of a write tool, from the file it writes.
pub(super) fn guard_write(input: &PreToolUseInput, deadline: Deadline) -> GuardOutcome {
    let Some(path) = input.written_path() else {
        return GuardOutcome::Clear;
    };
    match locate(&path, &input.cwd, deadline) {
        Ok(GuardVerdict::NotRequired) => GuardOutcome::Clear,
        Ok(GuardVerdict::MainCheckout(main)) => {
            GuardOutcome::Deny(write_reason(input, &main, &path, deadline))
        }
        Err(message) => GuardOutcome::Undecided(message),
    }
}

/// Decides a Bash call that runs `ds up`, `ds run` or `ds reset`, from the
/// folder it runs in.
pub(super) fn guard_environment(input: &PreToolUseInput, deadline: Deadline) -> GuardOutcome {
    match locate(&input.cwd, &input.cwd, deadline) {
        Ok(GuardVerdict::NotRequired) => GuardOutcome::Clear,
        Ok(GuardVerdict::MainCheckout(_)) => GuardOutcome::Deny(
            if input.is_from_subagent() {
                SUBAGENT_ENVIRONMENT_DENY_REASON
            } else {
                ENVIRONMENT_DENY_REASON
            }
            .to_owned(),
        ),
        Err(message) => GuardOutcome::Undecided(message),
    }
}

fn locate(path: &Path, base: &Path, deadline: Deadline) -> Result<GuardVerdict, String> {
    let paths = DevslotPaths::from_process().map_err(undecided)?;
    let global = GlobalConfig::load(&paths.config_file).map_err(undecided)?;
    let home = home_from_environment();
    let context = GuardContext {
        scope: &global.require,
        home: home.as_deref(),
        deadline,
    };
    locate_required_main_checkout(&context, path, base).map_err(undecided)
}

fn undecided(error: impl std::fmt::Display) -> String {
    format!("the devslot worktree guard could not decide ({error}); tell the user")
}

fn write_reason(
    input: &PreToolUseInput,
    main: &MainCheckout,
    path: &Path,
    deadline: Deadline,
) -> String {
    if let Some(equivalent) = equivalent_in_session_worktree(main, path, &input.cwd, deadline) {
        return format!(
            "{WORKTREE_WRITE_DENY_REASON_PREFIX}{}{WORKTREE_WRITE_DENY_REASON_SUFFIX}",
            equivalent.display()
        );
    }
    if input.is_from_subagent() {
        return SUBAGENT_WRITE_DENY_REASON.to_owned();
    }
    WRITE_DENY_REASON.to_owned()
}

/// The path of the target inside the linked worktree the session is in, when
/// that worktree belongs to the repository of the target. A second git call,
/// made only on the way to a refusal; any failure just gives no path.
fn equivalent_in_session_worktree(
    main: &MainCheckout,
    path: &Path,
    base: &Path,
    deadline: Deadline,
) -> Option<PathBuf> {
    let session_directory = nearest_existing_directory(base)?;
    let layout = read_layout(session_directory, deadline).ok()??;
    if layout.is_main_checkout() || layout.common_dir != main.common_dir {
        return None;
    }
    let target = resolve_target(path, base).ok()?;
    let relative = target.strip_prefix(&main.toplevel).ok()?;
    Some(layout.toplevel.join(relative))
}
