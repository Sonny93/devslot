//! What `ds run`, `ds up` and `ds ensure` share around a registration: building
//! it for this process, telling the user about a refusal, and announcing what
//! was done.

use std::time::Duration;

use crate::checkout::Checkout;
use crate::cli::exit_code;
use crate::cli::render::render_findings;
use crate::cli::style::Style;
use crate::commands::machine::Machine;
use crate::docker::{DOCKER_TIMEOUT, DockerCompose, docker_program};
use crate::error::ToolError;
use crate::global_config::GlobalConfig;
use crate::port_probe::SystemPortProbe;
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
    register_checkout_within(checkout, machine, global, intent, DOCKER_TIMEOUT)
}

/// Like [`register_checkout`], with `probe_timeout` for each compose probe.
pub fn register_checkout_within(
    checkout: &Checkout,
    machine: &Machine,
    global: &GlobalConfig,
    intent: Intent,
    probe_timeout: Duration,
) -> Result<RegistrationOutcome, ToolError> {
    let compose = DockerCompose::new(docker_program(), &checkout.root, probe_timeout);
    register(&RegistrationContext {
        checkout,
        compose: &compose,
        store: &machine.store,
        global,
        probe: &SystemPortProbe,
        lock_timeout: LOCK_TIMEOUT,
        intent,
    })
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
    match refusal {
        Refusal::Blocked(findings) => {
            eprintln!("ds: the checks found problems in this checkout:");
            eprint!("{}", render_findings(findings, style));
            eprintln!("ds: error: {closing_line}");
        }
        Refusal::NoRoom(error) => {
            let message = Refusal::no_room_message(error, &machine.paths.config_file);
            eprintln!("ds: error: {message}");
        }
    }
    exit_code::ERROR
}

/// Prints the warnings of the checks and what registration did.
pub fn announce(registration: &Registration, style: Style) {
    if !registration.warnings.is_empty() {
        eprintln!("ds: the checks report warnings:");
        eprint!("{}", render_findings(&registration.warnings, style));
    }
    for notice in registration.notices() {
        eprintln!("{notice}");
    }
}
