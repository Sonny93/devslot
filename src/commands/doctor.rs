//! `ds doctor`: checkout, config, discovery and checks, without writing anything.

use std::path::{Path, PathBuf};

use crate::checkout::Checkout;
use crate::checks::compose::{ComposeCheckContext, check_compose};
use crate::checks::env::{EnvCheckInput, check_env};
use crate::checks::finding::{Check, Finding, Level, Summary};
use crate::checks::vite::check_vite;
use crate::compose::probe::{ProbedCompose, probe};
use crate::compose::selection::{
    DevComposeSelection, default_named_compose, existing_overrides, scan_set, select_dev_compose,
};
use crate::discovery::{DiscoveryInput, ManagedPort, discover_ports};
use crate::docker::{ComposeCli, DockerError};
use crate::error::ToolError;
use crate::project::dotenv::{EnvFile, EnvRead, lookup_value, read_env_file};
use crate::project::vite::detect_vite;
use crate::project_config::ProjectConfig;
use crate::report::{DoctorReport, REPORT_VERSION};

const DOTENV_FILE: &str = ".env";
const DOTENV_EXAMPLE_FILE: &str = ".env.example";
const DOTENV_TEST_FILE: &str = ".env.test";

enum ProbeOutcome {
    Read(ProbedCompose),
    Rejected,
}

struct ProbedFile {
    name: String,
    outcome: ProbeOutcome,
}

struct EnvFiles {
    env: EnvRead,
    example: EnvRead,
    test: EnvRead,
}

impl EnvFiles {
    fn read(root: &Path) -> Result<Self, ToolError> {
        Ok(Self {
            env: read_named(root, DOTENV_FILE)?,
            example: read_named(root, DOTENV_EXAMPLE_FILE)?,
            test: read_named(root, DOTENV_TEST_FILE)?,
        })
    }

    fn env_file(&self) -> Option<&EnvFile> {
        parsed(&self.env)
    }

    fn example_file(&self) -> Option<&EnvFile> {
        parsed(&self.example)
    }
}

fn parsed(read: &EnvRead) -> Option<&EnvFile> {
    match read {
        EnvRead::Parsed(file) => Some(file),
        EnvRead::Missing | EnvRead::Invalid(_) => None,
    }
}

fn read_named(root: &Path, name: &str) -> Result<EnvRead, ToolError> {
    read_env_file(&root.join(name)).map_err(|source| ToolError::ReadFile {
        file: name.to_owned(),
        source,
    })
}

/// Reviews `checkout`.
pub fn run_doctor(
    checkout: &Checkout,
    compose: &dyn ComposeCli,
) -> Result<DoctorReport, ToolError> {
    let config = ProjectConfig::load(&checkout.root)?;
    let env_files = EnvFiles::read(&checkout.root)?;
    let vite = detect_vite(&checkout.root);
    let selection = select_dev_compose(&checkout.root, config.compose_file.as_deref());
    let dev_file = match &selection {
        DevComposeSelection::Selected(file) => Some(file.as_str()),
        DevComposeSelection::ConfiguredMissing(_) | DevComposeSelection::NoneFound => None,
    };
    let probed_files = probe_scanned_files(&checkout.root, dev_file, &env_files, compose)?;
    let dev_probe = dev_file.and_then(|file| read_probe(&probed_files, file));
    let ports = discover_ports(&DiscoveryInput {
        env: env_files.env_file(),
        example: env_files.example_file(),
        config: &config,
        vite: &vite,
        dev_compose: dev_file.zip(dev_probe),
    });

    let mut findings = Vec::new();
    findings.extend(selection_findings(&selection));
    findings.extend(compose_findings(
        &checkout.root,
        dev_file,
        &probed_files,
        &ports,
    ));
    findings.extend(check_env(&EnvCheckInput {
        env: &env_files.env,
        example: &env_files.example,
        test: &env_files.test,
        main_checkout_has_env: main_checkout_has_env(checkout),
        is_env_ignored: if checkout.is_git {
            Some(checkout.is_env_ignored()?)
        } else {
            None
        },
        managed: &ports,
        env_config_keys: &config.env_keys,
    }));
    findings.extend(check_vite(&checkout.root, &vite));

    Ok(DoctorReport {
        version: REPORT_VERSION,
        checkout: checkout.root.display().to_string(),
        compose_file: dev_file.map(str::to_owned),
        summary: Summary::of(&findings),
        ports,
        findings,
    })
}

fn main_checkout_has_env(checkout: &Checkout) -> bool {
    checkout
        .main_root
        .as_ref()
        .is_some_and(|main| main.join(DOTENV_FILE).is_file())
}

fn read_probe<'a>(files: &'a [ProbedFile], name: &str) -> Option<&'a ProbedCompose> {
    files
        .iter()
        .find(|file| file.name == name)
        .and_then(|file| match &file.outcome {
            ProbeOutcome::Read(probed) => Some(probed),
            ProbeOutcome::Rejected => None,
        })
}

fn selection_findings(selection: &DevComposeSelection) -> Option<Finding> {
    match selection {
        DevComposeSelection::Selected(_) => None,
        DevComposeSelection::ConfiguredMissing(file) => Some(
            Finding::new(
                Level::Error,
                Check::ComposeFileMissing,
                "the compose file named by [compose] file does not exist",
            )
            .in_file(file),
        ),
        DevComposeSelection::NoneFound => Some(Finding::new(
            Level::Info,
            Check::ComposeMissing,
            "no compose file found at the checkout root: the compose checks are skipped",
        )),
    }
}

fn files_to_probe(
    root: &Path,
    file: &str,
    dev_file: Option<&str>,
    overrides: &[String],
) -> Vec<PathBuf> {
    let merges_overrides =
        dev_file == Some(file) || default_named_compose(root).as_deref() == Some(file);
    let merged = if merges_overrides { overrides } else { &[] };
    std::iter::once(file)
        .chain(merged.iter().map(String::as_str))
        .map(PathBuf::from)
        .collect()
}

fn files_to_scan(root: &Path, dev_file: Option<&str>) -> Vec<String> {
    let mut files = scan_set(root);
    if let Some(dev) = dev_file.filter(|dev| !files.iter().any(|file| file == dev)) {
        files.insert(0, dev.to_owned());
    }
    files
}

fn probe_scanned_files(
    root: &Path,
    dev_file: Option<&str>,
    env_files: &EnvFiles,
    compose: &dyn ComposeCli,
) -> Result<Vec<ProbedFile>, ToolError> {
    let overrides = existing_overrides(root);
    let from_env_files = |name: &str| {
        lookup_value(env_files.env_file(), env_files.example_file(), name).map(str::to_owned)
    };
    let mut probed_files = Vec::new();
    for name in files_to_scan(root, dev_file) {
        let files = files_to_probe(root, &name, dev_file, &overrides);
        let outcome = match probe(compose, &files, &from_env_files) {
            Ok(probed) => ProbeOutcome::Read(probed),
            Err(DockerError::Rejected) => ProbeOutcome::Rejected,
            Err(error) => return Err(error.into()),
        };
        probed_files.push(ProbedFile { name, outcome });
    }
    Ok(probed_files)
}

fn compose_findings(
    root: &Path,
    dev_file: Option<&str>,
    probed_files: &[ProbedFile],
    ports: &[ManagedPort],
) -> Vec<Finding> {
    let managed_variables: Vec<String> = ports.iter().map(|port| port.variable.clone()).collect();
    let context = ComposeCheckContext {
        root,
        managed_variables: &managed_variables,
    };
    let mut findings = Vec::new();
    for file in probed_files {
        match &file.outcome {
            ProbeOutcome::Read(probed) => {
                findings.extend(check_compose(&file.name, probed, &context));
            }
            ProbeOutcome::Rejected => {
                findings.push(rejection_finding(&file.name, dev_file));
            }
        }
    }
    findings
}

/// Wraps `text` in single quotes so a shell reads it as one literal word.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn rejection_finding(file: &str, dev_file: Option<&str>) -> Finding {
    let hint = format!(
        "run docker compose -f {} config --quiet to see why",
        shell_quote(file)
    );
    if dev_file == Some(file) {
        return Finding::new(
            Level::Error,
            Check::ComposeUnreadable,
            format!("docker compose cannot read the dev compose file; {hint}"),
        )
        .in_file(file);
    }
    Finding::new(
        Level::Warning,
        Check::ComposeSkipped,
        format!("skipped, docker compose cannot read it on its own; {hint}"),
    )
    .in_file(file)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use pretty_assertions::assert_eq;

    use super::*;

    fn run_in(root: &Path, compose: &dyn ComposeCli) -> Result<DoctorReport, ToolError> {
        run_doctor(&Checkout::locate(root)?, compose)
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
    fn should_wrap_a_plain_name_in_single_quotes() {
        assert_eq!(shell_quote("dev.compose.yml"), "'dev.compose.yml'");
    }

    #[test]
    fn should_keep_a_space_inside_the_quotes() {
        assert_eq!(shell_quote("my file.yml"), "'my file.yml'");
    }

    #[test]
    fn should_leave_a_command_substitution_inert() {
        assert_eq!(
            shell_quote("x$(id>pwned).compose.yml"),
            "'x$(id>pwned).compose.yml'"
        );
    }

    #[test]
    fn should_escape_a_single_quote_inside_the_name() {
        assert_eq!(shell_quote("it's.yml"), "'it'\\''s.yml'");
    }

    #[test]
    fn should_quote_the_file_name_in_the_hint_of_a_rejection() {
        let finding = rejection_finding("a$(id).yml", None);

        assert!(finding.message.contains("-f 'a$(id).yml' config --quiet"));
    }
}
