//! `ds hook session-start`: tells the agent which instance its checkout is,
//! when it is registered. No Docker call: the state of the services is left to
//! `ds info`.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::failure::HookFailure;
use super::input::read_input;
use crate::checkout::Checkout;
use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::cli::render::render_registered_info_unchecked;
use crate::commands::info::describe_registered;
use crate::commands::machine::Machine;
use crate::project_config::ProjectConfig;
use crate::report::RegisteredInfo;
use crate::services_state::ServicesState;

const GUIDANCE: &str = "Use ds info --json for ports and URLs; ds up starts the services; ds reset resets the data; never run ds rm, ds prune or delete containers or volumes by hand.";

#[derive(Debug, Deserialize)]
struct SessionStartInput {
    cwd: PathBuf,
}

/// Reads the hook input from stdin and prints what the agent should know.
pub fn run_session_start() -> Result<u8, HookFailure> {
    let input: SessionStartInput = read_input(std::io::stdin())?;
    if let Some(info) = registered_info_of(&input.cwd) {
        write_stdout(&session_text(&info))?;
    }
    Ok(exit_code::SUCCESS)
}

fn registered_info_of(directory: &Path) -> Option<RegisteredInfo> {
    let checkout = Checkout::locate(directory).ok()?;
    let machine = Machine::from_process().ok()?;
    let registry = machine.store.read().ok()?;
    let instance = registry.find(&checkout.root)?;
    let config = ProjectConfig::load(&checkout.root).ok()?;
    describe_registered(&checkout, instance, &config, ServicesState::Unknown).ok()
}

fn session_text(info: &RegisteredInfo) -> String {
    format!("{}\n{GUIDANCE}\n", render_registered_info_unchecked(info))
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

        let text = session_text(&info);

        assert!(text.contains("Services:      run ds info to check\n"));
        assert!(text.ends_with(&format!("{GUIDANCE}\n")));
    }
}
