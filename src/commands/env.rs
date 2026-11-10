//! `ds env`: print the environment of the instance of this checkout.

use crate::cli::args::EnvFormat;
use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::commands::machine::{Machine, load_printable_environment, locate_checkout};
use crate::error::ToolError;

const NOT_REGISTERED_MESSAGE: &str =
    "ds: error: this checkout is not registered; run ds up or ds run";

/// Prints the environment, or says the checkout is not registered.
pub fn run_env(format: EnvFormat) -> Result<u8, ToolError> {
    let checkout = locate_checkout()?;
    let machine = Machine::from_process()?;
    let registry = machine.store.read()?;
    let Some(instance) = registry.find(&checkout.root) else {
        eprintln!("{NOT_REGISTERED_MESSAGE}");
        return Ok(exit_code::ERROR);
    };
    let environment = load_printable_environment(&checkout, instance)?;
    let text = match format {
        EnvFormat::Sh => environment.to_shell()?,
        EnvFormat::Json => {
            let mut json = serde_json::to_string_pretty(&environment)
                .map_err(|error| ToolError::WriteReport(std::io::Error::other(error)))?;
            json.push('\n');
            json
        }
    };
    write_stdout(&text)?;
    Ok(exit_code::SUCCESS)
}
