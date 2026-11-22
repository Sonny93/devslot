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
use crate::tunneler::{TunnelerHost, TunnelerSetting};
use crate::worktree_guard::{GuardStatus, guard_status};

const GUARD_BUDGET: Duration = Duration::from_secs(5);
const BUILD_COMMIT: &str = match option_env!("DEVSLOT_BUILD_COMMIT") {
    Some(commit) => commit,
    None => "",
};
const PLUGIN_ROOT_VARIABLE: &str = "CLAUDE_PLUGIN_ROOT";
const SHORT_COMMIT_LENGTH: usize = 12;
const FULL_COMMIT_LENGTH: usize = 40;

pub(super) const GUIDANCE: &str = "Use ds info --json for ports and URLs (open app_url when it has one; when you give the user a URL of this instance, also give the tunneler command and link that ds info --json gives, when it has them: they open the instance from the user's computer); ds up --no-run starts the services (plain ds up then runs the app in the foreground and does not return); ds reset resets the data; ds down stops the app and the services, including processes started earlier; devslot never changes the project: when the app hardcodes a port or a URL or needs several processes, the project declares it in devslot.toml (run, [ports] extra, [env], [web] port), ds doctor says what to change, and you never run on the default ports instead; a service's own CLI runs with ds run -- docker compose -f <dev compose> exec <service> <cli>; never run ds rm, ds prune, ds worktree rm or delete containers or volumes by hand. When the devslot worktree guard is on, enter a worktree with the EnterWorktree tool before writing code or starting an environment, and run ds worktree land only after the user's explicit go-ahead (the yes to the closing question, or /devslot:finish; fast-forward only: when the base moved, git rebase it in the worktree, resolve, commit, run ds worktree land again; never push); when the work is committed and verified, offer to close it in one question: land it on the base branch and remove the session's worktree (removing it also stops the app and deletes the instance's containers and volumes, so no ds down first); a yes covers both: run ds worktree land, then, once it succeeded, leave with ExitWorktree, action remove and discard_changes true; the user can also type /devslot:finish, which is the go-ahead; never land or remove without one of the two. To remove this session's worktree when the user asks, leave it with the ExitWorktree tool, action remove and discard_changes true, never keep followed by a removal command; if it says the worktree was kept, ds ls says why.";
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
    let machine = Machine::from_process().map_err(|error| error.to_string());
    let global = load_global_config(&machine);
    let notices = notice_lines(
        guard_line_of(&input.cwd, &machine, &global),
        tunneler_line_of(&global),
        drift_line_of(
            BUILD_COMMIT,
            std::env::var_os(PLUGIN_ROOT_VARIABLE)
                .as_deref()
                .map(Path::new),
        ),
    );
    let tunneler_host = tunneler_host_of(&global);
    let info = machine
        .as_ref()
        .ok()
        .and_then(|machine| registered_info_of(&input.cwd, machine, tunneler_host.as_ref()));
    if let Some(text) = session_text(notices.as_deref(), info.as_ref()) {
        write_stdout(&text)?;
    }
    Ok(exit_code::SUCCESS)
}

fn load_global_config(machine: &Result<Machine, String>) -> Result<GlobalConfig, String> {
    let machine = machine.as_ref().map_err(Clone::clone)?;
    GlobalConfig::load(&machine.paths.config_file).map_err(|error| error.to_string())
}

fn notice_lines(
    guard: Option<String>,
    tunneler: Option<String>,
    drift: Option<String>,
) -> Option<String> {
    let lines: Vec<String> = [guard, tunneler, drift].into_iter().flatten().collect();
    if lines.is_empty() {
        return None;
    }
    Some(lines.join("\n"))
}

/// The line telling that `ds` and the plugin copy Claude Code runs come from
/// different commits; `None` when they match or when either is unknown. The
/// plugin has no version, so Claude Code names its copy after the first 12
/// characters of the commit, and the last component of the plugin root is
/// that name. Only the environment is read: no file, no git call.
fn drift_line_of(build_commit: &str, plugin_root: Option<&Path>) -> Option<String> {
    let plugin_commit = plugin_root?.file_name()?.to_str()?;
    if !is_short_commit(plugin_commit) || !is_full_commit(build_commit) {
        return None;
    }
    if build_commit.starts_with(plugin_commit) {
        return None;
    }
    let build_short = &build_commit[..SHORT_COMMIT_LENGTH];
    Some(format!(
        "devslot is out of step: ds was built from commit {build_short} and Claude Code runs the plugin copy of commit {plugin_commit}; tell the user to run scripts/install.sh in the devslot clone on main, then restart the Claude Code sessions."
    ))
}

fn is_short_commit(text: &str) -> bool {
    text.len() == SHORT_COMMIT_LENGTH && is_lowercase_hex(text)
}

fn is_full_commit(text: &str) -> bool {
    text.len() == FULL_COMMIT_LENGTH && is_lowercase_hex(text)
}

fn is_lowercase_hex(text: &str) -> bool {
    text.chars()
        .all(|character| matches!(character, '0'..='9' | 'a'..='f'))
}

fn tunneler_line_of(global: &Result<GlobalConfig, String>) -> Option<String> {
    match &global.as_ref().ok()?.tunneler {
        TunnelerSetting::Invalid { reason } => Some(format!(
            "the [tunneler] host in the devslot config is invalid ({reason}); tell the user"
        )),
        TunnelerSetting::NotSet | TunnelerSetting::Host(_) => None,
    }
}

fn tunneler_host_of(global: &Result<GlobalConfig, String>) -> Option<TunnelerHost> {
    match &global.as_ref().ok()?.tunneler {
        TunnelerSetting::Host(host) => Some(host.clone()),
        TunnelerSetting::NotSet | TunnelerSetting::Invalid { .. } => None,
    }
}

/// The line about the worktree guard for the checkout of `directory`; `None`
/// when the guard is not on for it. A failure to find out is a line too.
fn guard_line_of(
    directory: &Path,
    machine: &Result<Machine, String>,
    global: &Result<GlobalConfig, String>,
) -> Option<String> {
    let outcome = match (machine, global) {
        (Ok(machine), Ok(global)) => guard_line_within(directory, machine, global),
        (Err(message), _) | (_, Err(message)) => Err(message.clone()),
    };
    match outcome {
        Ok(line) => line,
        Err(message) => Some(format!(
            "the devslot worktree guard could not be checked ({message}); tell the user"
        )),
    }
}

fn guard_line_within(
    directory: &Path,
    machine: &Machine,
    global: &GlobalConfig,
) -> Result<Option<String>, String> {
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

fn registered_info_of(
    directory: &Path,
    machine: &Machine,
    tunneler_host: Option<&TunnelerHost>,
) -> Option<RegisteredInfo> {
    let checkout = Checkout::locate(directory).ok()?;
    let registry = machine.store.read().ok()?;
    let instance = registry.find(&checkout.root)?;
    let config = ProjectConfig::load(&checkout.root).ok()?;
    describe_registered(
        &checkout,
        instance,
        &config,
        tunneler_host,
        ServicesState::Unknown,
    )
    .ok()
}

fn session_text(notices: Option<&str>, info: Option<&RegisteredInfo>) -> Option<String> {
    let mut text = String::new();
    if let Some(notices) = notices {
        text.push_str(notices);
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
            project: None,
            host: "app-3f2a.localhost".to_owned(),
            app_url: None,
            path: "/work/app".to_owned(),
            worktree: None,
            slot: 1,
            range: [20050, 20099],
            compose_file: None,
            compose_file_origin: None,
            ports: Vec::new(),
            services: ServicesState::Unknown,
            tunneler: None,
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
    fn should_put_the_tunneler_line_after_the_guard_line() {
        let notices = notice_lines(Some("guard".to_owned()), Some("tunneler".to_owned()), None);

        assert_eq!(notices.as_deref(), Some("guard\ntunneler"));
    }

    #[test]
    fn should_give_no_notice_when_there_is_neither_guard_nor_tunneler_line() {
        assert_eq!(notice_lines(None, None, None), None);
    }

    #[test]
    fn should_add_the_kept_worktree_count_to_the_main_checkout_line() {
        let line = main_checkout_line(Some(3));

        assert!(line.contains("EnterWorktree"));
        assert!(line.contains("ds worktree land"));
        assert!(line.ends_with("Kept worktrees of this repository: 3 (ds ls lists them)."));
        assert_eq!(main_checkout_line(None), MAIN_CHECKOUT_LINE);
    }

    const BUILD: &str = "36294cdd320a1111111111111111111111111111";

    fn drift_for(build: &str, root: &str) -> Option<String> {
        drift_line_of(build, Some(Path::new(root)))
    }

    #[test]
    fn should_put_the_drift_line_after_the_tunneler_line() {
        let notices = notice_lines(None, Some("tunneler".to_owned()), Some("drift".to_owned()));

        assert_eq!(notices.as_deref(), Some("tunneler\ndrift"));
    }

    #[test]
    fn should_say_both_short_commits_when_the_plugin_copy_is_another_commit() {
        let line = drift_for(
            BUILD,
            "/home/u/.claude/plugins/cache/devslot/devslot/cc52b387482e",
        );

        assert_eq!(
            line.as_deref(),
            Some(
                "devslot is out of step: ds was built from commit 36294cdd320a and Claude Code runs the plugin copy of commit cc52b387482e; tell the user to run scripts/install.sh in the devslot clone on main, then restart the Claude Code sessions."
            )
        );
    }

    #[test]
    fn should_give_no_drift_line_when_the_build_commit_starts_with_the_plugin_copy_name() {
        assert_eq!(
            drift_for(BUILD, "/cache/devslot/devslot/36294cdd320a"),
            None
        );
    }

    #[test]
    fn should_give_no_drift_line_when_the_build_commit_is_unknown() {
        assert_eq!(drift_for("", "/cache/devslot/devslot/cc52b387482e"), None);
        assert_eq!(
            drift_for("not-a-commit", "/cache/devslot/devslot/cc52b387482e"),
            None
        );
    }

    #[test]
    fn should_give_no_drift_line_when_the_plugin_root_is_unknown() {
        assert_eq!(drift_line_of(BUILD, None), None);
        assert_eq!(drift_for(BUILD, ""), None);
    }

    #[test]
    fn should_give_no_drift_line_when_the_plugin_root_is_not_named_after_a_short_commit() {
        for root in [
            "/cache/devslot/devslot/0.2.0",
            "/cache/devslot/devslot/CC52B387482E",
            "/cache/devslot/devslot/cc52b387482",
            "/cache/devslot/devslot/cc52b387482ef",
            "/cache/devslot/devslot/cc52b387482z",
        ] {
            assert_eq!(drift_for(BUILD, root), None, "{root}");
        }
    }
}
