//! Probing a set of compose files: list the variables, give the candidates
//! sentinels, read the merged configuration back.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::model::{ComposeConfig, PortMapping};
use super::sentinels::{SentinelPlan, plan_for};
use crate::docker::{ComposeCli, DockerError};

/// The merged configuration of a probe and what is needed to interpret it.
#[derive(Debug)]
pub struct ProbedCompose {
    /// The merged configuration, with sentinels where candidate variables were.
    pub config: ComposeConfig,
    /// The sentinel of each candidate variable.
    pub plan: SentinelPlan,
    /// The default of each variable in the compose files (empty when none).
    pub defaults: BTreeMap<String, String>,
}

impl ProbedCompose {
    /// The variable whose sentinel a port publishes, when it publishes one.
    pub fn variable_published_by(&self, port: &PortMapping) -> Option<&str> {
        port.published
            .as_deref()
            .and_then(|published| self.plan.variable_for_text(published))
    }

    /// The container port of a mapping; a sentinel target stands for its
    /// variable's default.
    pub fn container_port(&self, port: &PortMapping) -> Option<u16> {
        let target = port.target?;
        let Some(variable) = self.plan.variable_for(target) else {
            return u16::try_from(target).ok();
        };
        self.defaults.get(variable)?.parse().ok()
    }
}

/// Runs the two probes on `files`. `from_env_files` gives a variable's value
/// in `.env` (devslot block stripped), else `.env.example`.
pub fn probe(
    cli: &dyn ComposeCli,
    files: &[PathBuf],
    from_env_files: &dyn Fn(&str) -> Option<String>,
) -> Result<ProbedCompose, DockerError> {
    let defaults = cli.variables(files)?;
    let plan = plan_for(&defaults, from_env_files);
    let text = cli.config_json(files, &plan.environment())?;
    let config = ComposeConfig::from_json(&text)
        .map_err(|_| DockerError::UnexpectedOutput { stage: "config" })?;
    Ok(ProbedCompose {
        config,
        plan,
        defaults,
    })
}

#[cfg(test)]
pub(crate) mod testing {
    //! An in-memory `ComposeCli` for unit tests.

    use std::cell::RefCell;

    use super::*;

    /// Answers from canned data and records the sentinels it was given.
    pub struct FakeComposeCli {
        pub variables: Option<BTreeMap<String, String>>,
        pub config: String,
        pub received_sentinels: RefCell<BTreeMap<String, String>>,
    }

    impl FakeComposeCli {
        pub fn new(variables: &[(&str, &str)], config: &str) -> Self {
            Self {
                variables: Some(
                    variables
                        .iter()
                        .map(|(name, default)| ((*name).to_owned(), (*default).to_owned()))
                        .collect(),
                ),
                config: config.to_owned(),
                received_sentinels: RefCell::default(),
            }
        }

        pub fn rejecting() -> Self {
            Self {
                variables: None,
                config: String::new(),
                received_sentinels: RefCell::default(),
            }
        }
    }

    impl ComposeCli for FakeComposeCli {
        fn variables(&self, _: &[PathBuf]) -> Result<BTreeMap<String, String>, DockerError> {
            self.variables.clone().ok_or(DockerError::Rejected)
        }

        fn config_json(
            &self,
            _: &[PathBuf],
            sentinels: &BTreeMap<String, String>,
        ) -> Result<String, DockerError> {
            *self.received_sentinels.borrow_mut() = sentinels.clone();
            Ok(self.config.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::testing::FakeComposeCli;
    use super::*;

    const POSTGRES_CONFIG: &str =
        r#"{"services":{"postgres":{"ports":[{"target":5432,"published":"61000"}]}}}"#;

    fn no_env_files(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn should_hand_the_sentinels_to_compose_when_probing() {
        let cli = FakeComposeCli::new(&[("DB_PORT", ""), ("DB_USER", "")], POSTGRES_CONFIG);

        probe(&cli, &[PathBuf::from("dev.compose.yml")], &|name| {
            (name == "DB_USER").then(|| "root".to_owned())
        })
        .unwrap();

        assert_eq!(
            *cli.received_sentinels.borrow(),
            BTreeMap::from([("DB_PORT".to_owned(), "61000".to_owned())])
        );
    }

    #[test]
    fn should_map_a_published_sentinel_to_its_variable_when_the_port_carries_one() {
        let cli = FakeComposeCli::new(&[("DB_PORT", "")], POSTGRES_CONFIG);

        let probed = probe(&cli, &[], &no_env_files).unwrap();

        let port = &probed.config.services["postgres"].ports[0];
        assert_eq!(probed.variable_published_by(port), Some("DB_PORT"));
    }

    #[test]
    fn should_not_map_a_hardcoded_port_to_a_variable() {
        let config = r#"{"services":{"postgres":{"ports":[{"target":5432,"published":"5432"}]}}}"#;
        let cli = FakeComposeCli::new(&[("DB_PORT", "")], config);

        let probed = probe(&cli, &[], &no_env_files).unwrap();

        let port = &probed.config.services["postgres"].ports[0];
        assert_eq!(probed.variable_published_by(port), None);
    }

    #[test]
    fn should_map_a_sentinel_target_back_to_the_variable_default() {
        let config = r#"{"services":{"web":{"ports":[{"target":61000,"published":"8080"}]}}}"#;
        let cli = FakeComposeCli::new(&[("INNER_PORT", "3000")], config);

        let probed = probe(&cli, &[], &no_env_files).unwrap();

        let port = &probed.config.services["web"].ports[0];
        assert_eq!(probed.container_port(port), Some(3000));
    }

    #[test]
    fn should_return_the_target_unchanged_when_it_is_not_a_sentinel() {
        let cli = FakeComposeCli::new(&[("DB_PORT", "")], POSTGRES_CONFIG);

        let probed = probe(&cli, &[], &no_env_files).unwrap();

        let port = &probed.config.services["postgres"].ports[0];
        assert_eq!(probed.container_port(port), Some(5432));
    }

    #[test]
    fn should_report_a_rejection_when_compose_refuses_the_files() {
        let cli = FakeComposeCli::rejecting();

        let error = probe(&cli, &[], &no_env_files).unwrap_err();

        assert!(error.is_rejection());
    }

    #[test]
    fn should_report_unexpected_output_when_the_config_is_not_json() {
        let cli = FakeComposeCli::new(&[], "not json");

        let error = probe(&cli, &[], &no_env_files).unwrap_err();

        assert!(matches!(error, DockerError::UnexpectedOutput { .. }));
    }
}
