//! What can be learned about a checkout without writing anything: its
//! configuration, its `.env` files, Vite, the compose files and the managed
//! ports. Shared by `ds doctor` and by registration.

use std::path::{Path, PathBuf};

use crate::checkout::Checkout;
use crate::checks::compose::{ComposeCheckContext, check_compose};
use crate::checks::env_config::{
    check_env_forces_port, check_env_keys_declared, check_local_config_tracked,
};
use crate::checks::finding::{Check, Finding, Level};
use crate::compose::probe::{ProbedCompose, probe};
use crate::compose::selection::{
    DevComposeSelection, default_named_compose, existing_overrides, scan_set, select_dev_compose,
};
use crate::discovery::{DiscoveryInput, ManagedPort, discover_ports};
use crate::docker::{ComposeCli, DockerError};
use crate::error::ToolError;
use crate::project::dotenv::{EnvFile, EnvRead, lookup_value, read_env_file};
use crate::project::vite::{ViteDetection, detect_vite};
use crate::project_config::{EnvSetting, LOCAL_CONFIG_FILE, ProjectConfig};
use crate::shell_quote::shell_quote;

/// The `.env` file read at the checkout root.
pub const DOTENV_FILE: &str = ".env";
/// The example the `.env` is created from.
pub const DOTENV_EXAMPLE_FILE: &str = ".env.example";
/// The `.env` the tests load.
pub const DOTENV_TEST_FILE: &str = ".env.test";

/// Which compose files are probed with Docker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeScope {
    /// Every file `ds doctor` scans, and the dev compose.
    AllScannedFiles,
    /// The dev compose only, which is all registration needs.
    DevComposeOnly,
}

/// What probing one compose file gave.
pub enum ProbeOutcome {
    /// Compose read the file.
    Read(ProbedCompose),
    /// Compose rejected it.
    Rejected,
}

/// A compose file and what probing it gave.
pub struct ProbedFile {
    /// The file name, relative to the checkout root.
    pub name: String,
    /// What probing it gave.
    pub outcome: ProbeOutcome,
}

/// The `.env` files of a checkout.
pub struct EnvFiles {
    /// `.env`, as it is on disk.
    pub env: EnvRead,
    /// `.env.example`.
    pub example: EnvRead,
    /// `.env.test`.
    pub test: EnvRead,
    /// The file `.env` would be created from, read in memory only; `Missing`
    /// unless `.env` is missing and a creation source was given.
    pub creation_source: EnvRead,
}

impl EnvFiles {
    /// Reads the three files at `root`. When `.env` is missing and
    /// `creation_source` is given, that file is read too, so discovery can see
    /// the values `.env` is about to be created with.
    pub fn read(root: &Path, creation_source: Option<&Path>) -> Result<Self, ToolError> {
        let env = read_named(root, DOTENV_FILE)?;
        let creation_source = match creation_source {
            Some(source) if env == EnvRead::Missing => {
                read_env_file(source).map_err(|error| ToolError::ReadFile {
                    file: source.display().to_string(),
                    source: error,
                })?
            }
            _ => EnvRead::Missing,
        };
        Ok(Self {
            env,
            example: read_named(root, DOTENV_EXAMPLE_FILE)?,
            test: read_named(root, DOTENV_TEST_FILE)?,
            creation_source,
        })
    }

    /// `.env` when it could be parsed.
    pub fn env_file(&self) -> Option<&EnvFile> {
        parsed(&self.env)
    }

    /// `.env`, else the file it is about to be created from, when parsed.
    pub fn effective_env_file(&self) -> Option<&EnvFile> {
        parsed(&self.env).or_else(|| parsed(&self.creation_source))
    }

    /// `.env.example` when it could be parsed.
    pub fn example_file(&self) -> Option<&EnvFile> {
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

/// Everything discovery learned about a checkout.
pub struct CheckoutAnalysis {
    /// The merged project configuration.
    pub config: ProjectConfig,
    /// The `.env` files.
    pub env_files: EnvFiles,
    /// The Vite files found.
    pub vite: ViteDetection,
    /// The dev compose that was selected.
    pub selection: DevComposeSelection,
    /// The compose files that were probed.
    pub probed_files: Vec<ProbedFile>,
    /// The managed port variables, in discovery order.
    pub ports: Vec<ManagedPort>,
    /// Whether git tracks `devslot.local.toml`.
    pub is_local_config_tracked: bool,
}

impl CheckoutAnalysis {
    /// Reads the checkout and probes the compose files `scope` asks for.
    pub fn analyze(
        checkout: &Checkout,
        compose: &dyn ComposeCli,
        scope: ProbeScope,
    ) -> Result<Self, ToolError> {
        Self::analyze_with_creation_source(checkout, compose, scope, None)
    }

    /// Like [`CheckoutAnalysis::analyze`]; when `.env` is missing, discovery
    /// reads `creation_source` in its place.
    pub fn analyze_with_creation_source(
        checkout: &Checkout,
        compose: &dyn ComposeCli,
        scope: ProbeScope,
        creation_source: Option<&Path>,
    ) -> Result<Self, ToolError> {
        let config = ProjectConfig::load(&checkout.root)?;
        let env_files = EnvFiles::read(&checkout.root, creation_source)?;
        let vite = detect_vite(&checkout.root);
        let selection = select_dev_compose(&checkout.root, config.compose_file.as_deref());
        let dev_file = selected_file(&selection);
        let probed_files = probe_files(
            &checkout.root,
            dev_file,
            &env_files,
            compose,
            scope,
            &config.env,
        )?;
        let is_local_config_tracked = is_local_config_tracked(checkout)?;
        let dev_probe = dev_file.and_then(|file| read_probe(&probed_files, file));
        let ports = discover_ports(&DiscoveryInput {
            env: env_files.effective_env_file(),
            example: env_files.example_file(),
            config: &config,
            vite: &vite,
            dev_compose: dev_file.zip(dev_probe),
        });
        Ok(Self {
            config,
            env_files,
            vite,
            selection,
            probed_files,
            ports,
            is_local_config_tracked,
        })
    }

    /// The dev compose file, relative to the checkout root.
    pub fn dev_file(&self) -> Option<&str> {
        selected_file(&self.selection)
    }

    /// The findings that block `ds up` and `ds run`, and the warnings shown
    /// with them: the checks on the dev compose and on `[env]`.
    pub fn up_findings(&self, root: &Path) -> Vec<Finding> {
        let mut findings = Vec::new();
        findings.extend(selection_findings(&self.selection));
        if let Some(file) = self
            .probed_files
            .iter()
            .find(|file| Some(file.name.as_str()) == self.dev_file())
        {
            findings.extend(self.compose_file_findings(root, file));
        }
        findings.extend(self.config_findings());
        findings
    }

    /// The findings about `devslot.toml`, `devslot.local.toml` and `[env]`.
    pub fn config_findings(&self) -> Vec<Finding> {
        let mut findings = check_env_forces_port(&self.config, &self.ports);
        findings.extend(check_env_keys_declared(
            &self.config,
            self.env_files.effective_env_file(),
            self.env_files.example_file(),
            &self.ports,
        ));
        findings.extend(check_local_config_tracked(self.is_local_config_tracked));
        findings
    }

    /// The findings of one probed compose file.
    pub fn compose_file_findings(&self, root: &Path, file: &ProbedFile) -> Vec<Finding> {
        match &file.outcome {
            ProbeOutcome::Read(probed) => {
                let managed_variables: Vec<String> = self
                    .ports
                    .iter()
                    .map(|port| port.variable.clone())
                    .collect();
                let context = ComposeCheckContext {
                    root,
                    managed_variables: &managed_variables,
                    ignored_variables: &self.config.ignored_ports.names,
                };
                check_compose(&file.name, probed, &context)
            }
            ProbeOutcome::Rejected => vec![rejection_finding(&file.name, self.dev_file())],
        }
    }
}

fn is_local_config_tracked(checkout: &Checkout) -> Result<bool, ToolError> {
    if !checkout.is_git || !checkout.root.join(LOCAL_CONFIG_FILE).exists() {
        return Ok(false);
    }
    Ok(checkout.is_tracked(LOCAL_CONFIG_FILE)?)
}

fn selected_file(selection: &DevComposeSelection) -> Option<&str> {
    match selection {
        DevComposeSelection::Selected(file) => Some(file.as_str()),
        DevComposeSelection::ConfiguredMissing(_) | DevComposeSelection::NoneFound => None,
    }
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

/// The finding about the dev compose selection, when there is one.
pub fn selection_findings(selection: &DevComposeSelection) -> Option<Finding> {
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

fn files_to_scan(root: &Path, dev_file: Option<&str>, scope: ProbeScope) -> Vec<String> {
    if scope == ProbeScope::DevComposeOnly {
        return dev_file.map(str::to_owned).into_iter().collect();
    }
    let mut files = scan_set(root);
    if let Some(dev) = dev_file.filter(|dev| !files.iter().any(|file| file == dev)) {
        files.insert(0, dev.to_owned());
    }
    files
}

fn probe_files(
    root: &Path,
    dev_file: Option<&str>,
    env_files: &EnvFiles,
    compose: &dyn ComposeCli,
    scope: ProbeScope,
    configured: &[EnvSetting],
) -> Result<Vec<ProbedFile>, ToolError> {
    let overrides = existing_overrides(root);
    let from_env_files = |name: &str| {
        lookup_value(
            env_files.effective_env_file(),
            env_files.example_file(),
            name,
        )
        .map(str::to_owned)
    };
    let mut probed_files = Vec::new();
    for name in files_to_scan(root, dev_file, scope) {
        let files = files_to_probe(root, &name, dev_file, &overrides);
        let outcome = match probe(compose, &files, &from_env_files, configured) {
            Ok(probed) => ProbeOutcome::Read(probed),
            Err(DockerError::Rejected) => ProbeOutcome::Rejected,
            Err(error) => return Err(error.into()),
        };
        probed_files.push(ProbedFile { name, outcome });
    }
    Ok(probed_files)
}

/// The finding for a compose file Compose rejected: an error for the dev
/// compose, a warning for any other file.
pub fn rejection_finding(file: &str, dev_file: Option<&str>) -> Finding {
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
    use super::*;

    #[test]
    fn should_quote_the_file_name_in_the_hint_of_a_rejection() {
        let finding = rejection_finding("a$(id).yml", None);

        assert!(finding.message.contains("-f 'a$(id).yml' config --quiet"));
    }

    #[test]
    fn should_make_the_rejection_of_the_dev_compose_an_error() {
        let finding = rejection_finding("dev.compose.yml", Some("dev.compose.yml"));

        assert_eq!(
            (finding.level, finding.check),
            (Level::Error, Check::ComposeUnreadable)
        );
    }
}
