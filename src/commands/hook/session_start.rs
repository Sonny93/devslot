//! `ds hook session-start`: tells the agent which instance its checkout is,
//! when it is registered, and whether the worktree guard is on for it. No
//! Docker call: the state of the services is left to `ds info`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use super::failure::HookFailure;
use super::input::read_input;
use crate::checkout::Checkout;
use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::cli::render::render_registered_info_unchecked;
use crate::commands::info::describe_registered;
use crate::commands::machine::Machine;
use crate::deadline::Deadline;
use crate::git_worktree::list_worktrees;
use crate::global_config::{GlobalConfig, home_from_environment};
use crate::kept_worktrees;
use crate::project_config::ProjectConfig;
use crate::report::RegisteredInfo;
use crate::services_state::ServicesState;
use crate::worktree_guard::{GuardStatus, guard_status};

const GUARD_BUDGET: Duration = Duration::from_secs(5);

pub(super) const GUIDANCE: &str = "Use ds info --json for ports and URLs; ds up --no-run starts the services (plain ds up then runs the app in the foreground and does not return); ds reset resets the data; never run ds rm, ds prune, ds worktree rm or delete containers or volumes by hand. When the devslot worktree guard is on, enter a worktree with the EnterWorktree tool before writing code or starting an environment, and run ds worktree land only after the user's explicit go-ahead (fast-forward only: when the base moved, git rebase it in the worktree, resolve, commit, run ds worktree land again; never push). To remove this session's worktree when the user asks, leave it with the ExitWorktree tool, action remove and discard_changes true, never keep followed by a removal command; if it says the worktree was kept, ds ls says why.";
const MAIN_CHECKOUT_LINE: &str = "devslot worktree guard is on: this is the main checkout, read and search here; before editing files or starting an environment (ds up, ds run, ds reset) enter a worktree with the EnterWorktree tool, with a short name taken from the task; when the user approves the committed work, run ds worktree land.";
const LINKED_WORKTREE_LINE: &str = "devslot worktree guard is on: this is a linked worktree, work here; when the user approves the committed work, run ds worktree land.";
const DISABLED_LINE: &str = "devslot worktree guard is off for this checkout: devslot.local.toml sets [worktrees] require = false.";

#[derive(Debug, Deserialize)]
struct SessionStartInput {
    cwd: PathBuf,
}

/// Reads the hook input from stdin and prints what the agent should know.
pub fn run_session_start() -> Result<u8, HookFailure> {
    let input: SessionStartInput = read_input(std::io::stdin())?;
    let guard = guard_line_of(&input.cwd);
    let info = registered_info_of(&input.cwd);
    if let Some(text) = session_text(guard.as_deref(), info.as_ref()) {
        write_stdout(&text)?;
    }
    Ok(exit_code::SUCCESS)
}

/// The line about the worktree guard for the checkout of `directory`; `None`
/// when the guard is not on for it. A failure to find out is a line too.
fn guard_line_of(directory: &Path) -> Option<String> {
    match guard_line_within(directory) {
        Ok(line) => line,
        Err(message) => Some(format!(
            "the devslot worktree guard could not be checked ({message}); tell the user"
        )),
    }
}

fn guard_line_within(directory: &Path) -> Result<Option<String>, String> {
    let machine = Machine::from_process().map_err(|error| error.to_string())?;
    let global =
        GlobalConfig::load(&machine.paths.config_file).map_err(|error| error.to_string())?;
    if global.require.is_empty() {
        return Ok(None);
    }
    let checkout = Checkout::locate_within(directory, Deadline::starting_now(GUARD_BUDGET))
        .map_err(|error| error.to_string())?;
    let status = guard_status(
        &global.require,
        home_from_environment().as_deref(),
        &checkout,
    )
    .map_err(|error| error.to_string())?;
    Ok(match status {
        GuardStatus::Active => Some(main_checkout_line(kept_worktree_count(
            &checkout,
            machine.paths.state_directory(),
        ))),
        GuardStatus::LinkedWorktree => Some(LINKED_WORKTREE_LINE.to_owned()),
        GuardStatus::DisabledLocally => Some(DISABLED_LINE.to_owned()),
        GuardStatus::NotConfigured | GuardStatus::NotCovered => None,
    })
}

fn main_checkout_line(kept_count: Option<usize>) -> String {
    match kept_count {
        Some(count) => {
            format!(
                "{MAIN_CHECKOUT_LINE} Kept worktrees of this repository: {count} (ds ls lists them)."
            )
        }
        None => MAIN_CHECKOUT_LINE.to_owned(),
    }
}

/// How many worktrees of this repository `ds ls` lists as kept; `None` when
/// git cannot list the worktrees.
fn kept_worktree_count(checkout: &Checkout, state_directory: &Path) -> Option<usize> {
    let listed = list_worktrees(&checkout.root).ok()?;
    let count = kept_worktrees::list(state_directory)
        .iter()
        .filter(|kept| listed.iter().any(|entry| entry.is_at(&kept.path)))
        .count();
    Some(count)
}

fn registered_info_of(directory: &Path) -> Option<RegisteredInfo> {
    let checkout = Checkout::locate(directory).ok()?;
    let machine = Machine::from_process().ok()?;
    let registry = machine.store.read().ok()?;
    let instance = registry.find(&checkout.root)?;
    let config = ProjectConfig::load(&checkout.root).ok()?;
    describe_registered(&checkout, instance, &config, ServicesState::Unknown).ok()
}

fn session_text(guard: Option<&str>, info: Option<&RegisteredInfo>) -> Option<String> {
    let mut text = String::new();
    if let Some(guard) = guard {
        text.push_str(guard);
        text.push('\n');
    }
    if let Some(info) = info {
        text.push_str(&render_registered_info_unchecked(info));
        text.push('\n');
        text.push_str(GUIDANCE);
        text.push('\n');
    }
    if text.is_empty() { None } else { Some(text) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_say_how_to_check_the_services_and_end_with_the_guidance() {
        let info = RegisteredInfo {
            version: 1,
            registered: true,
            name: "app-3f2a".to_owned(),
            path: "/work/app".to_owned(),
            slot: 1,
            range: [20050, 20099],
            compose_file: None,
            compose_file_origin: None,
            ports: Vec::new(),
            services: ServicesState::Unknown,
        };

        let text = session_text(None, Some(&info)).unwrap();

        assert!(text.contains("Services:      run ds info to check\n"));
        assert!(text.ends_with(&format!("{GUIDANCE}\n")));
    }

    #[test]
    fn should_print_the_guard_line_first_and_alone_when_the_checkout_is_not_registered() {
        let text = session_text(Some("guard line"), None);

        assert_eq!(text.as_deref(), Some("guard line\n"));
    }

    #[test]
    fn should_print_nothing_when_there_is_no_guard_line_and_no_instance() {
        assert_eq!(session_text(None, None), None);
    }

    #[test]
    fn should_add_the_kept_worktree_count_to_the_main_checkout_line() {
        let line = main_checkout_line(Some(3));

        assert!(line.contains("EnterWorktree"));
        assert!(line.contains("ds worktree land"));
        assert!(line.ends_with("Kept worktrees of this repository: 3 (ds ls lists them)."));
        assert_eq!(main_checkout_line(None), MAIN_CHECKOUT_LINE);
    }
}
