//! `ds doctor`: checkout, config, discovery and checks, without writing anything.

use crate::analysis::{CheckoutAnalysis, ProbeOutcome, ProbeScope, selection_findings};
use crate::checkout::Checkout;
use crate::checks::env::{EnvCheckInput, check_env};
use crate::checks::env_config::check_forced_port_conflicts;
use crate::checks::finding::{Finding, Summary};
use crate::checks::forced_port_shared::{
    OtherForcing, OwnForcedPort, SharedForcingInput, check_forced_port_shared,
};
use crate::checks::session::{SessionRoot, check_session_root};
use crate::checks::slot::{SlotCheckInput, check_slot_geometry};
use crate::checks::tunneler::check_tunneler;
use crate::checks::vite::check_vite;
use crate::checks::web_port::check_web_port;
use crate::checks::worktree_guard::{GuardCheckInput, check_worktree_guard};
use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::cli::render::render_text;
use crate::cli::style::{ColorChoice, Style};
use crate::commands::machine::{Machine, locate_checkout};
use crate::compose::sentinels::usual_value;
use crate::docker::{ComposeCli, DockerCompose};
use crate::effective_port::{effective_ports, forced_port_of, read_forcing_settings};
use crate::env_sync::has_creation_source;
use crate::error::ToolError;
use crate::global_config::{GlobalConfig, home_from_environment};
use crate::instance_processes::{CLAUDE_COMM, claude_pid_from_environment, session_root_process};
use crate::project::dotenv::lookup_value;
use crate::project_config::{CONFIG_FILE, LOCAL_CONFIG_FILE};
use crate::registry::record::{InstanceRecord, Registry, normalize_path};
use crate::report::{DoctorReport, REPORT_VERSION};
use crate::worktree_guard::guard_status;
use crate::worktree_require_override::read_require_key;

/// Runs `ds doctor` on the current folder and prints the report.
pub fn run_doctor_command(color: ColorChoice, as_json: bool) -> Result<u8, ToolError> {
    let checkout = locate_checkout()?;
    let machine = Machine::from_process()?;
    let global = GlobalConfig::load(&machine.paths.config_file)?;
    let geometry = global.slots;
    let registry = machine.store.read()?;
    let slot_check = registry
        .find(&checkout.root)
        .map(|instance| SlotCheckInput { instance, geometry });
    let compose = DockerCompose::from_environment(&checkout.root);
    let mut report = run_doctor(
        &checkout,
        &compose,
        slot_check,
        Some(&registry),
        &observe_session_root(),
    )?;
    let guard_input = GuardCheckInput {
        status: guard_status(
            &global.require,
            home_from_environment().as_deref(),
            &checkout,
        )?,
        is_registered: registry.find(&checkout.root).is_some(),
        committed_key: read_require_key(&checkout.root, CONFIG_FILE).ok().flatten(),
        tracked_local_key: tracked_local_key(&checkout),
    };
    report.findings.extend(check_worktree_guard(guard_input));
    report
        .findings
        .extend(check_tunneler(&global.tunneler, &machine.paths.config_file));
    report.summary = Summary::of(&report.findings);
    let text = if as_json {
        let mut json = serde_json::to_string_pretty(&report)
            .map_err(|error| ToolError::WriteReport(std::io::Error::other(error)))?;
        json.push('\n');
        json
    } else {
        render_text(&report, Style::for_process(color))
    };
    write_stdout(&text)?;
    Ok(if report.has_errors() {
        exit_code::ERROR
    } else {
        exit_code::SUCCESS
    })
}

fn tracked_local_key(checkout: &Checkout) -> Option<bool> {
    if !checkout.is_git || !checkout.is_tracked(LOCAL_CONFIG_FILE).ok()? {
        return None;
    }
    read_require_key(&checkout.root, LOCAL_CONFIG_FILE).ok()?
}

/// What `/proc` says about the session that runs `ds doctor`; nothing outside
/// Linux or without `CLAUDE_PID`.
fn observe_session_root() -> SessionRoot {
    if !cfg!(target_os = "linux") || std::env::var_os("CLAUDE_PID").is_none() {
        return SessionRoot::NotInSession;
    }
    match session_root_process(claude_pid_from_environment()) {
        None => SessionRoot::Missing,
        Some(process) if process.comm == CLAUDE_COMM => SessionRoot::Claude,
        Some(process) => SessionRoot::Other(process),
    }
}

/// Reviews `checkout`. `slot_check` is given when the checkout is registered;
/// `session_root` is what was found about the session that runs `ds`.
pub fn run_doctor(
    checkout: &Checkout,
    compose: &dyn ComposeCli,
    slot_check: Option<SlotCheckInput<'_>>,
    registry: Option<&Registry>,
    session_root: &SessionRoot,
) -> Result<DoctorReport, ToolError> {
    let analysis = CheckoutAnalysis::analyze(checkout, compose, ProbeScope::AllScannedFiles)?;
    let mut findings = Vec::new();
    findings.extend(selection_findings(&analysis.selection));
    for file in &analysis.probed_files {
        findings.extend(analysis.compose_file_findings(&checkout.root, file));
    }
    findings.extend(env_findings(checkout, &analysis)?);
    findings.extend(analysis.config_findings());
    findings.extend(forced_port_findings(checkout, &analysis, registry));
    findings.extend(check_vite(&checkout.root, &analysis.vite, &analysis.ports));
    findings.extend(slot_check.and_then(check_slot_geometry));
    findings.extend(check_session_root(session_root));
    findings.extend(check_web_port(
        analysis.config.web_port.as_deref(),
        &analysis.ports,
    ));
    Ok(DoctorReport {
        version: REPORT_VERSION,
        checkout: checkout.root.display().to_string(),
        compose_file: analysis.dev_file().map(str::to_owned),
        summary: Summary::of(&findings),
        ports: analysis.ports,
        findings,
    })
}

fn forced_port_findings(
    checkout: &Checkout,
    analysis: &CheckoutAnalysis,
    registry: Option<&Registry>,
) -> Vec<Finding> {
    let Some(registry) = registry else {
        return Vec::new();
    };
    let mut findings =
        check_forced_port_conflicts(&analysis.config, &analysis.ports, registry, &checkout.root);
    findings.extend(shared_port_findings(checkout, analysis, registry));
    findings
}

fn shared_port_findings(
    checkout: &Checkout,
    analysis: &CheckoutAnalysis,
    registry: &Registry,
) -> Vec<Finding> {
    let own = normalize_path(&checkout.root);
    let forced_by_others: Vec<(&InstanceRecord, Vec<u16>)> = registry
        .instances
        .iter()
        .filter(|instance| normalize_path(&instance.path) != own)
        .map(|instance| (instance, forced_ports_of(instance)))
        .filter(|(_, ports)| !ports.is_empty())
        .collect();
    if forced_by_others.is_empty() {
        return Vec::new();
    }
    let others: Vec<OtherForcing<'_>> = forced_by_others
        .iter()
        .map(|(instance, ports)| OtherForcing { instance, ports })
        .collect();
    check_forced_port_shared(&SharedForcingInput {
        own_path: &checkout.root,
        own_range: registry.find(&checkout.root).map(|instance| instance.range),
        own_forced: &own_forced_ports(checkout, analysis),
        others: &others,
    })
}

fn forced_ports_of(instance: &InstanceRecord) -> Vec<u16> {
    effective_ports(instance, &read_forcing_settings(instance))
        .into_iter()
        .filter(|port| port.is_forced)
        .map(|port| port.port)
        .collect()
}

/// The canonical ports this checkout forces for its managed variables; none
/// when git tracks `devslot.local.toml` or cannot tell.
fn own_forced_ports(checkout: &Checkout, analysis: &CheckoutAnalysis) -> Vec<OwnForcedPort> {
    let forced: Vec<OwnForcedPort> = analysis
        .config
        .env
        .iter()
        .filter(|setting| {
            analysis
                .ports
                .iter()
                .any(|managed| managed.variable == setting.key)
        })
        .filter_map(|setting| {
            forced_port_of(setting).map(|port| OwnForcedPort {
                variable: setting.key.clone(),
                port,
            })
        })
        .collect();
    if forced.is_empty() {
        return forced;
    }
    let is_trusted = checkout.is_git && !checkout.is_tracked(LOCAL_CONFIG_FILE).unwrap_or(true);
    if !is_trusted {
        return Vec::new();
    }
    forced
}

fn env_findings(
    checkout: &Checkout,
    analysis: &CheckoutAnalysis,
) -> Result<Vec<Finding>, ToolError> {
    let is_env_ignored = if checkout.is_git {
        Some(checkout.is_env_ignored()?)
    } else {
        None
    };
    Ok(check_env(&EnvCheckInput {
        env: &analysis.env_files.env,
        example: &analysis.env_files.example,
        test: &analysis.env_files.test,
        has_creation_source: has_creation_source(checkout),
        is_env_ignored,
        managed: &analysis.ports,
        env_config_keys: &analysis.config.env_keys(),
        ignored_ports: &ignored_usual_ports(analysis),
    }))
}

fn ignored_usual_ports(analysis: &CheckoutAnalysis) -> Vec<u16> {
    analysis
        .config
        .ignored_ports
        .names
        .iter()
        .filter_map(|name| {
            usual_value(&compose_default(analysis, name), || {
                env_value(analysis, name)
            })
        })
        .filter_map(|value| value.trim().parse().ok())
        .collect()
}

fn compose_default(analysis: &CheckoutAnalysis, name: &str) -> String {
    analysis
        .probed_files
        .iter()
        .filter_map(|file| match &file.outcome {
            ProbeOutcome::Read(probed) => probed.defaults.get(name),
            ProbeOutcome::Rejected => None,
        })
        .find(|value| !value.is_empty())
        .cloned()
        .unwrap_or_default()
}

fn env_value(analysis: &CheckoutAnalysis, name: &str) -> Option<String> {
    lookup_value(
        analysis.env_files.effective_env_file(),
        analysis.env_files.example_file(),
        name,
    )
    .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::checks::finding::{Check, Level};
    use crate::docker::DockerError;

    fn run_in(root: &Path, compose: &dyn ComposeCli) -> Result<DoctorReport, ToolError> {
        run_doctor(
            &Checkout::locate(root)?,
            compose,
            None,
            None,
            &SessionRoot::NotInSession,
        )
    }

    struct ScriptedCompose {
        rejected_files: Vec<&'static str>,
        config: &'static str,
        seen_files: RefCell<Vec<Vec<PathBuf>>>,
    }

    impl ScriptedCompose {
        fn answering(config: &'static str) -> Self {
            Self {
                rejected_files: vec![],
                config,
                seen_files: RefCell::default(),
            }
        }
    }

    impl ComposeCli for ScriptedCompose {
        fn variables(&self, files: &[PathBuf]) -> Result<BTreeMap<String, String>, DockerError> {
            self.seen_files.borrow_mut().push(files.to_vec());
            let first = files[0].to_str().unwrap_or_default();
            if self.rejected_files.contains(&first) {
                return Err(DockerError::Rejected);
            }
            Ok(BTreeMap::from([("DB_PORT".to_owned(), String::new())]))
        }

        fn config_json(
            &self,
            _: &[PathBuf],
            _: &BTreeMap<String, String>,
        ) -> Result<String, DockerError> {
            Ok(self.config.to_owned())
        }
    }

    const POSTGRES: &str =
        r#"{"services":{"postgres":{"ports":[{"target":5432,"published":"61000"}]}}}"#;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (name, content) in files {
            std::fs::write(root.path().join(name), content).unwrap();
        }
        root
    }

    fn checks_of(report: &DoctorReport) -> Vec<Check> {
        report
            .findings
            .iter()
            .map(|finding| finding.check)
            .collect()
    }

    #[test]
    fn should_report_an_info_line_and_still_run_the_other_checks_when_there_is_no_compose_file() {
        let root = project(&[(".env", "PORT=3333\n")]);

        let report = run_in(root.path(), &ScriptedCompose::answering("{}")).unwrap();

        assert_eq!(
            (checks_of(&report), report.compose_file, report.ports.len()),
            (vec![Check::ComposeMissing], None, 1)
        );
    }

    #[test]
    fn should_discover_the_compose_port_when_the_dev_compose_publishes_a_sentinel() {
        let root = project(&[(".env", "PORT=3333\n"), ("dev.compose.yml", "")]);

        let report = run_in(root.path(), &ScriptedCompose::answering(POSTGRES)).unwrap();

        assert_eq!(
            report
                .ports
                .iter()
                .map(|port| port.variable.as_str())
                .collect::<Vec<_>>(),
            ["PORT", "DB_PORT"]
        );
    }

    #[test]
    fn should_warn_about_a_scanned_file_when_compose_cannot_read_it_on_its_own() {
        let root = project(&[
            (".env", "A=1\n"),
            ("dev.compose.yml", ""),
            ("compose.extra.yml", ""),
        ]);
        let compose = ScriptedCompose {
            rejected_files: vec!["compose.extra.yml"],
            ..ScriptedCompose::answering(POSTGRES)
        };

        let report = run_in(root.path(), &compose).unwrap();

        assert_eq!(
            (
                report.findings[0].level,
                report.findings[0].check,
                report.summary.errors
            ),
            (Level::Warning, Check::ComposeSkipped, 0)
        );
    }

    #[test]
    fn should_report_an_error_when_compose_rejects_the_dev_compose() {
        let root = project(&[(".env", "A=1\n"), ("dev.compose.yml", "")]);
        let compose = ScriptedCompose {
            rejected_files: vec!["dev.compose.yml"],
            ..ScriptedCompose::answering(POSTGRES)
        };

        let report = run_in(root.path(), &compose).unwrap();

        assert_eq!(
            (report.findings[0].check, report.summary.errors),
            (Check::ComposeUnreadable, 1)
        );
    }

    #[test]
    fn should_report_an_error_when_the_configured_compose_file_does_not_exist() {
        let root = project(&[
            (".env", "A=1\n"),
            ("devslot.toml", "[compose]\nfile = \"nope.yml\"\n"),
        ]);

        let report = run_in(root.path(), &ScriptedCompose::answering("{}")).unwrap();

        assert_eq!(checks_of(&report), vec![Check::ComposeFileMissing]);
    }

    #[test]
    fn should_merge_the_overrides_into_the_dev_compose_only() {
        let root = project(&[
            (".env", "A=1\n"),
            ("dev.compose.yml", ""),
            ("compose.image.yml", ""),
            ("compose.override.yml", ""),
        ]);
        let compose = ScriptedCompose::answering("{}");

        run_in(root.path(), &compose).unwrap();

        let seen: Vec<Vec<String>> = compose
            .seen_files
            .borrow()
            .iter()
            .map(|files| {
                files
                    .iter()
                    .map(|file| file.display().to_string())
                    .collect()
            })
            .collect();
        assert_eq!(
            seen,
            [
                vec![
                    "dev.compose.yml".to_owned(),
                    "compose.override.yml".to_owned()
                ],
                vec!["compose.image.yml".to_owned()]
            ]
        );
    }

    #[test]
    fn should_report_an_error_without_the_line_when_env_cannot_be_parsed() {
        let root = project(&[(".env", "THIS IS FAKE-SECRET\n")]);

        let report = run_in(root.path(), &ScriptedCompose::answering("{}")).unwrap();

        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("env_unparsable"));
        assert!(!json.contains("FAKE-SECRET"));
    }

    #[test]
    fn should_fail_as_a_tool_error_when_docker_is_unavailable() {
        struct MissingDocker;
        impl ComposeCli for MissingDocker {
            fn variables(&self, _: &[PathBuf]) -> Result<BTreeMap<String, String>, DockerError> {
                Err(DockerError::Unavailable {
                    program: "docker".to_owned(),
                    reason: "not found".to_owned(),
                })
            }
            fn config_json(
                &self,
                _: &[PathBuf],
                _: &BTreeMap<String, String>,
            ) -> Result<String, DockerError> {
                unreachable!()
            }
        }
        let root = project(&[(".env", "A=1\n"), ("dev.compose.yml", "")]);

        let error = run_in(root.path(), &MissingDocker).unwrap_err();

        assert!(matches!(
            error,
            ToolError::Docker(DockerError::Unavailable { .. })
        ));
    }

    #[test]
    fn should_print_no_resolved_compose_string_when_the_model_holds_interpolated_names() {
        let root = project(&[(".env", "A=1\n"), ("dev.compose.yml", "")]);
        let config = r#"{"services":{"app":{"container_name":"x-FAKE"}},"volumes":{"data":{"name":"y-FAKE"}}}"#;

        let report = run_in(root.path(), &ScriptedCompose::answering(config)).unwrap();

        let json = serde_json::to_string(&report).unwrap();
        let text =
            crate::cli::render::render_text(&report, crate::cli::style::Style { color: false });
        assert!(json.contains("container_name") && json.contains("explicit_name"));
        assert!(!json.contains("FAKE") && !text.contains("FAKE"));
    }

    #[test]
    fn should_report_the_geometry_warning_when_the_registered_range_differs_from_its_slot() {
        let root = project(&[(".env", "A=1\n")]);
        let checkout = Checkout::locate(root.path()).unwrap();
        let instance = crate::registry::record::InstanceRecord {
            path: checkout.root.clone(),
            name: "x".to_owned(),
            slot: 1,
            range: crate::port_range::PortRange {
                first: 30000,
                last: 30049,
            },
            ports: vec![],
            host: "app.localhost".to_owned(),
            web_port_variable: None,
            origin: None,
            is_host_unsaved: false,
        };
        let slot_check = SlotCheckInput {
            instance: &instance,
            geometry: crate::global_config::SlotGeometry::default(),
        };

        let report = run_doctor(
            &checkout,
            &ScriptedCompose::answering("{}"),
            Some(slot_check),
            None,
            &SessionRoot::NotInSession,
        )
        .unwrap();

        assert!(checks_of(&report).contains(&Check::SlotGeometryChanged));
    }

    #[test]
    fn should_report_the_session_warning_when_the_session_root_is_not_named_claude() {
        let root = project(&[(".env", "A=1\n")]);
        let session_root = SessionRoot::Other(crate::instance_processes::ProcessSummary {
            pid: 42,
            comm: "node".to_owned(),
        });

        let report = run_doctor(
            &Checkout::locate(root.path()).unwrap(),
            &ScriptedCompose::answering("{}"),
            None,
            None,
            &session_root,
        )
        .unwrap();

        assert_eq!(
            (
                checks_of(&report).contains(&Check::SessionProcessName),
                report.summary.warnings
            ),
            (true, 1)
        );
    }

    #[test]
    fn should_report_an_error_when_the_committed_config_forces_a_managed_port() {
        let root = project(&[
            (".env", "PORT=3333\n"),
            ("devslot.toml", "[env]\nPORT = \"3333\"\n"),
        ]);

        let report = run_in(root.path(), &ScriptedCompose::answering("{}")).unwrap();

        assert_eq!(
            (
                checks_of(&report).contains(&Check::EnvForcesPort),
                report.summary.errors
            ),
            (true, 1)
        );
    }

    #[test]
    fn should_not_report_the_forced_port_when_it_comes_from_the_local_config() {
        let root = project(&[
            (".env", "PORT=3333\n"),
            ("devslot.local.toml", "[env]\nPORT = \"3333\"\n"),
        ]);

        let report = run_in(root.path(), &ScriptedCompose::answering("{}")).unwrap();

        assert!(!checks_of(&report).contains(&Check::EnvForcesPort));
    }
}
