//! `ds info`: the name, slot and ports of the instance of this checkout.

use crate::checkout::Checkout;
use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::cli::render::render_info_text;
use crate::commands::machine::{Machine, locate_checkout};
use crate::compose::selection::{DevComposeSelection, select_dev_compose};
use crate::error::ToolError;
use crate::project_config::ProjectConfig;
use crate::registry::record::InstanceRecord;
use crate::report::{InfoPort, InfoReport, REPORT_VERSION, RegisteredInfo, UnregisteredInfo};

const DETECTED_ORIGIN: &str = "detected";

/// Prints what is known about the instance, or that there is none.
pub fn run_info(as_json: bool) -> Result<u8, ToolError> {
    let checkout = locate_checkout()?;
    let machine = Machine::from_process()?;
    let registry = machine.store.read()?;
    let report = match registry.find(&checkout.root) {
        Some(instance) => InfoReport::Registered(registered_info(&checkout, instance)?),
        None => InfoReport::Unregistered(UnregisteredInfo {
            version: REPORT_VERSION,
            registered: false,
            path: checkout.root.display().to_string(),
        }),
    };
    write_stdout(&render(&report, as_json)?)?;
    Ok(exit_code::SUCCESS)
}

fn render(report: &InfoReport, as_json: bool) -> Result<String, ToolError> {
    if !as_json {
        return Ok(render_info_text(report));
    }
    let mut json = serde_json::to_string_pretty(report)
        .map_err(|error| ToolError::WriteReport(std::io::Error::other(error)))?;
    json.push('\n');
    Ok(json)
}

fn registered_info(
    checkout: &Checkout,
    instance: &InstanceRecord,
) -> Result<RegisteredInfo, ToolError> {
    let config = ProjectConfig::load(&checkout.root)?;
    let selection = select_dev_compose(&checkout.root, config.compose_file.as_deref());
    let (compose_file, compose_file_origin) = match selection {
        DevComposeSelection::Selected(file) => {
            let origin = config.compose_file_origin.unwrap_or(DETECTED_ORIGIN);
            (Some(file), Some(origin.to_owned()))
        }
        DevComposeSelection::ConfiguredMissing(_) | DevComposeSelection::NoneFound => (None, None),
    };
    Ok(RegisteredInfo {
        version: REPORT_VERSION,
        registered: true,
        name: instance.name.clone(),
        path: instance.path.display().to_string(),
        slot: instance.slot,
        range: [instance.range.first, instance.range.last],
        compose_file,
        compose_file_origin,
        ports: instance.ports.iter().map(info_port).collect(),
    })
}

fn info_port(assigned: &crate::registry::record::AssignedPort) -> InfoPort {
    InfoPort {
        variable: assigned.variable.clone(),
        port: assigned.port,
        kind: assigned.detail.kind,
        service: assigned.detail.service.clone(),
        target: assigned.detail.target,
    }
}
