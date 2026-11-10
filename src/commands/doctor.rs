//! `ds doctor`: checkout, config, discovery and checks, without writing anything.

use crate::analysis::{CheckoutAnalysis, DOTENV_FILE, ProbeScope, selection_findings};
use crate::checkout::Checkout;
use crate::checks::env::{EnvCheckInput, check_env};
use crate::checks::env_config::check_forced_port_conflicts;
use crate::checks::finding::{Finding, Summary};
use crate::checks::slot::{SlotCheckInput, check_slot_geometry};
use crate::checks::vite::check_vite;
use crate::cli::exit_code;
use crate::cli::output::write_stdout;
use crate::cli::render::render_text;
use crate::cli::style::{ColorChoice, Style};
use crate::commands::machine::{Machine, locate_checkout};
use crate::docker::{ComposeCli, DockerCompose};
use crate::error::ToolError;
use crate::global_config::GlobalConfig;
use crate::registry::record::Registry;
use crate::report::{DoctorReport, REPORT_VERSION};

/// Runs `ds doctor` on the current folder and prints the report.
pub fn run_doctor_command(color: ColorChoice, as_json: bool) -> Result<u8, ToolError> {
    let checkout = locate_checkout()?;
    let machine = Machine::from_process()?;
    let geometry = GlobalConfig::load(&machine.paths.config_file)?.slots;
    let registry = machine.store.read()?;
    let slot_check = registry
        .find(&checkout.root)
        .map(|instance| SlotCheckInput { instance, geometry });
    let compose = DockerCompose::from_environment(&checkout.root);
    let report = run_doctor(&checkout, &compose, slot_check, Some(&registry))?;
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

/// Reviews `checkout`. `slot_check` is given when the checkout is registered.
pub fn run_doctor(
    checkout: &Checkout,
    compose: &dyn ComposeCli,
    slot_check: Option<SlotCheckInput<'_>>,
    registry: Option<&Registry>,
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
    findings.extend(check_vite(&checkout.root, &analysis.vite));
    findings.extend(slot_check.and_then(check_slot_geometry));
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
    registry.map_or_else(Vec::new, |registry| {
        check_forced_port_conflicts(&analysis.config, &analysis.ports, registry, &checkout.root)
    })
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
        main_checkout_has_env: main_checkout_has_env(checkout),
        is_env_ignored,
        managed: &analysis.ports,
        env_config_keys: &analysis.config.env_keys(),
    }))
}

fn main_checkout_has_env(checkout: &Checkout) -> bool {
    checkout
        .main_root
        .as_ref()
        .is_some_and(|main| main.join(DOTENV_FILE).is_file())
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
        run_doctor(&Checkout::locate(root)?, compose, None, None)
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
        )
        .unwrap();

        assert!(checks_of(&report).contains(&Check::SlotGeometryChanged));
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
