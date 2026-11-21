//! What `ds run`, `ds up` and `ds ensure` share around a registration: building
//! it for this process, telling the user about a refusal, and announcing what
//! was done.

use std::time::Duration;

use crate::checkout::Checkout;
use crate::cli::exit_code;
use crate::cli::render::render_findings;
use crate::cli::style::Style;
use crate::commands::machine::Machine;
use crate::compose::selection::{DevComposeSelection, select_dev_compose};
use crate::deadline::Deadline;
use crate::docker::{DOCKER_TIMEOUT, DockerCompose, docker_program};
use crate::ensure_cache::{Fingerprint, discovery_stamp_path, write_discovery_stamp};
use crate::error::ToolError;
use crate::global_config::GlobalConfig;
use crate::port_probe::SystemPortProbe;
use crate::project_config::ProjectConfig;
use crate::registration::{
    Intent, Refusal, Registration, RegistrationContext, RegistrationOutcome, register,
};

/// How long the commands wait for the registry lock.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// Registers `checkout` with the real Docker and the real ports of this
/// machine, probing the compose files with the standard timeout.
pub fn register_checkout(
    checkout: &Checkout,
    machine: &Machine,
    global: &GlobalConfig,
    intent: Intent,
) -> Result<RegistrationOutcome, ToolError> {
    register_checkout_within(checkout, machine, global, intent, (DOCKER_TIMEOUT, None))
}

/// Like [`register_checkout`], with `probe_timeout` for each compose probe,
/// both clamped to the `deadline` when there is one. A successful registration
/// stamps what port discovery read, so `ds ensure` knows the registry is not
/// behind those files; the stamp is taken before the registration, so a file
/// edited during the probe triggers another refresh instead of being missed.
pub fn register_checkout_within(
    checkout: &Checkout,
    machine: &Machine,
    global: &GlobalConfig,
    intent: Intent,
    (probe_timeout, deadline): (Duration, Option<Deadline>),
) -> Result<RegistrationOutcome, ToolError> {
    let stamp = discovery_fingerprint_of(checkout);
    let compose =
        DockerCompose::new(docker_program(), &checkout.root, probe_timeout).with_deadline(deadline);
    let outcome = register(&RegistrationContext {
        checkout,
        compose: &compose,
        store: &machine.store,
        global,
        probe: &SystemPortProbe,
        lock_timeout: LOCK_TIMEOUT,
        deadline,
        intent,
    })?;
    if let (RegistrationOutcome::Registered(_), Some(stamp)) = (&outcome, &stamp) {
        let file = discovery_stamp_path(machine.paths.state_directory(), &checkout.root);
        let _ = write_discovery_stamp(&file, stamp);
    }
    Ok(outcome)
}

/// What port discovery reads in `checkout`, or `None` when it has no dev
/// compose file or its configuration cannot be read (registration refuses
/// then, and nothing is stamped).
fn discovery_fingerprint_of(checkout: &Checkout) -> Option<Fingerprint> {
    let config = ProjectConfig::load(&checkout.root).ok()?;
    let DevComposeSelection::Selected(dev_file) =
        select_dev_compose(&checkout.root, config.compose_file.as_deref())
    else {
        return None;
    };
    Some(Fingerprint::of_discovery(&checkout.root, &dev_file))
}

/// The registration, or the exit code to return: a refusal is told to the
/// user and gives 1, and a skipped automatic registration gives 0.
pub fn registration_or_exit_code(
    outcome: RegistrationOutcome,
    machine: &Machine,
    style: Style,
    closing_line: &str,
) -> Result<Registration, u8> {
    match outcome {
        RegistrationOutcome::Registered(registration) => Ok(registration),
        RegistrationOutcome::SkippedOptedOut => Err(exit_code::SUCCESS),
        RegistrationOutcome::Refused(refusal) => {
            Err(report_refusal(&refusal, machine, style, closing_line))
        }
    }
}

/// Tells the user why the checkout was not registered; returns the exit code.
pub fn report_refusal(
    refusal: &Refusal,
    machine: &Machine,
    style: Style,
    closing_line: &str,
) -> u8 {
    eprint!("{}", refusal_text(refusal, machine, style, closing_line));
    exit_code::ERROR
}

/// What to tell the user about a refusal, one line each, ending with a newline.
pub fn refusal_text(
    refusal: &Refusal,
    machine: &Machine,
    style: Style,
    closing_line: &str,
) -> String {
    match refusal {
        Refusal::Blocked(findings) => format!(
            "ds: the checks found problems in this checkout:\n{}ds: error: {closing_line}\n",
            render_findings(findings, style)
        ),
        Refusal::NoRoom(error) => {
            let message = Refusal::no_room_message(error, &machine.paths.config_file);
            format!("ds: error: {message}\n")
        }
    }
}

/// Prints the warnings of the checks and what registration did.
pub fn announce(registration: &Registration, style: Style) {
    for line in announcement_lines(registration, style) {
        eprintln!("{line}");
    }
}

/// The warnings of the checks, then what registration did, one entry per line.
pub fn announcement_lines(registration: &Registration, style: Style) -> Vec<String> {
    let mut lines = Vec::new();
    if !registration.warnings.is_empty() {
        lines.push("ds: the checks report warnings:".to_owned());
        lines.extend(
            render_findings(&registration.warnings, style)
                .lines()
                .map(str::to_owned),
        );
    }
    lines.extend(registration.notices());
    lines
}
