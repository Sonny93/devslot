//! Checks on the `[env]` table of the project configuration.

use std::path::Path;

use super::finding::{Check, Finding, Level};
use crate::discovery::ManagedPort;
use crate::project::dotenv::EnvFile;
use crate::project_config::{CONFIG_FILE, LOCAL_CONFIG_FILE, ProjectConfig};
use crate::registry::record::{Registry, normalize_path};

/// A managed port variable set in `[env]` of the committed `devslot.toml` is an
/// error: only `devslot.local.toml` may force a port on this machine.
pub fn check_env_forces_port(config: &ProjectConfig, managed: &[ManagedPort]) -> Vec<Finding> {
    config
        .env
        .iter()
        .filter(|setting| setting.file == CONFIG_FILE)
        .filter(|setting| managed.iter().any(|port| port.variable == setting.key))
        .map(|setting| {
            Finding::new(
                Level::Error,
                Check::EnvForcesPort,
                "[env] sets a managed port variable, which would break the isolation of every checkout; move it to devslot.local.toml",
            )
            .in_file(CONFIG_FILE)
            .for_key(&setting.key)
        })
        .collect()
}

/// A key of the committed `[env]` must already be declared in `.env` or
/// `.env.example`: a committed file cannot invent variables for the process.
/// Managed ports are left to [`check_env_forces_port`].
pub fn check_env_keys_declared(
    config: &ProjectConfig,
    env: Option<&EnvFile>,
    example: Option<&EnvFile>,
    managed: &[ManagedPort],
) -> Vec<Finding> {
    let is_declared = |key: &str| {
        [env, example]
            .into_iter()
            .flatten()
            .any(|file| file.value_of(key).is_some())
    };
    config
        .env
        .iter()
        .filter(|setting| setting.file == CONFIG_FILE)
        .filter(|setting| !managed.iter().any(|port| port.variable == setting.key))
        .filter(|setting| !is_declared(&setting.key))
        .map(|setting| {
            Finding::new(
                Level::Error,
                Check::EnvKeyUndeclared,
                "[env] sets a key that neither .env nor .env.example declares; declare it there, or set it in devslot.local.toml",
            )
            .in_file(CONFIG_FILE)
            .for_key(&setting.key)
        })
        .collect()
}

/// A `devslot.local.toml` that git tracks is shared like the committed file,
/// so it is not the user's own.
pub fn check_local_config_tracked(is_tracked: bool) -> Option<Finding> {
    is_tracked.then(|| {
        Finding::new(
            Level::Error,
            Check::LocalConfigTracked,
            "git tracks this file, but it must stay on this machine: untrack it and add it to .gitignore",
        )
        .in_file(LOCAL_CONFIG_FILE)
    })
}

/// A port forced by `devslot.local.toml` must not be one that another
/// registered instance holds, or lies in its range.
pub fn check_forced_port_conflicts(
    config: &ProjectConfig,
    managed: &[ManagedPort],
    registry: &Registry,
    own_path: &Path,
) -> Vec<Finding> {
    let own = normalize_path(own_path);
    let mut findings = Vec::new();
    for setting in config
        .env
        .iter()
        .filter(|setting| setting.file == LOCAL_CONFIG_FILE)
    {
        let Some(port) = setting.value.trim().parse::<u16>().ok() else {
            continue;
        };
        if !managed
            .iter()
            .any(|managed| managed.variable == setting.key)
        {
            continue;
        }
        let holder = registry
            .instances
            .iter()
            .filter(|instance| normalize_path(&instance.path) != own)
            .find(|instance| instance.range.contains(port));
        if let Some(instance) = holder {
            findings.push(
                Finding::new(
                    Level::Error,
                    Check::ForcedPortConflict,
                    format!(
                        "{} is forced to port {port}, which belongs to the slot of instance {}",
                        setting.key, instance.name
                    ),
                )
                .in_file(LOCAL_CONFIG_FILE)
                .for_key(&setting.key),
            );
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{PortKind, PortSource};
    use crate::project_config::{EnvSetting, LOCAL_CONFIG_FILE};

    fn managed(variable: &str) -> ManagedPort {
        ManagedPort {
            variable: variable.to_owned(),
            kind: PortKind::Web,
            default: None,
            source: PortSource::Dotenv,
            file: ".env".to_owned(),
            service: None,
            target: None,
        }
    }

    fn config_with(key: &str, file: &'static str) -> ProjectConfig {
        ProjectConfig {
            env: vec![EnvSetting {
                key: key.to_owned(),
                value: "FAKE".to_owned(),
                file,
            }],
            ..ProjectConfig::default()
        }
    }

    #[test]
    fn should_report_an_error_naming_the_key_when_the_committed_file_sets_a_managed_port() {
        let findings = check_env_forces_port(&config_with("PORT", CONFIG_FILE), &[managed("PORT")]);

        assert_eq!(
            findings
                .iter()
                .map(|finding| (finding.level, finding.check, finding.key.as_deref()))
                .collect::<Vec<_>>(),
            [(Level::Error, Check::EnvForcesPort, Some("PORT"))]
        );
    }

    #[test]
    fn should_allow_the_local_file_to_set_a_managed_port() {
        let config = config_with("PORT", LOCAL_CONFIG_FILE);

        assert_eq!(
            check_env_forces_port(&config, &[managed("PORT")]),
            Vec::new()
        );
    }

    #[test]
    fn should_not_report_a_key_that_is_not_a_managed_port() {
        let config = config_with("VITE_API_URL", CONFIG_FILE);

        assert_eq!(
            check_env_forces_port(&config, &[managed("PORT")]),
            Vec::new()
        );
    }

    #[test]
    fn should_not_print_the_value_when_reporting() {
        let findings = check_env_forces_port(&config_with("PORT", CONFIG_FILE), &[managed("PORT")]);

        assert!(!findings[0].message.contains("FAKE"));
    }

    fn declared(text: &str) -> EnvFile {
        EnvFile::parse(text).unwrap()
    }

    #[test]
    fn should_report_a_committed_key_that_no_env_file_declares() {
        let config = config_with("FOO", CONFIG_FILE);

        let findings = check_env_keys_declared(&config, Some(&declared("A=1\n")), None, &[]);

        assert_eq!(
            findings
                .iter()
                .map(|finding| (
                    finding.check,
                    finding.key.as_deref(),
                    finding.file.as_deref()
                ))
                .collect::<Vec<_>>(),
            [(Check::EnvKeyUndeclared, Some("FOO"), Some(CONFIG_FILE))]
        );
    }

    #[test]
    fn should_accept_a_committed_key_declared_in_the_example_only() {
        let config = config_with("DATABASE_URL", CONFIG_FILE);

        let findings =
            check_env_keys_declared(&config, None, Some(&declared("DATABASE_URL=x\n")), &[]);

        assert_eq!(findings, Vec::new());
    }

    #[test]
    fn should_accept_a_local_key_that_no_env_file_declares() {
        let config = config_with("FOO", LOCAL_CONFIG_FILE);

        assert_eq!(
            check_env_keys_declared(&config, None, None, &[]),
            Vec::new()
        );
    }

    #[test]
    fn should_not_print_the_value_of_an_undeclared_key() {
        let findings = check_env_keys_declared(&config_with("FOO", CONFIG_FILE), None, None, &[]);

        assert!(!findings[0].message.contains("FAKE"));
    }

    #[test]
    fn should_report_a_tracked_local_config_and_nothing_otherwise() {
        assert_eq!(
            check_local_config_tracked(true).map(|finding| finding.check),
            Some(Check::LocalConfigTracked)
        );
        assert_eq!(check_local_config_tracked(false), None);
    }

    fn registry_with_other_instance() -> Registry {
        Registry {
            instances: vec![crate::registry::record::InstanceRecord {
                path: "/work/other".into(),
                name: "other-1234".to_owned(),
                slot: 1,
                range: crate::port_range::PortRange {
                    first: 20050,
                    last: 20099,
                },
                ports: vec![],
            }],
            opted_out: vec![],
        }
    }

    #[test]
    fn should_report_a_forced_port_inside_the_range_of_another_instance() {
        let mut config = config_with("DB_PORT", LOCAL_CONFIG_FILE);
        config.env[0].value = "20062".to_owned();

        let findings = check_forced_port_conflicts(
            &config,
            &[managed("DB_PORT")],
            &registry_with_other_instance(),
            Path::new("/work/mine"),
        );

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].check, Check::ForcedPortConflict);
        assert!(findings[0].message.contains("DB_PORT"));
        assert!(findings[0].message.contains("20062"));
        assert!(findings[0].message.contains("other-1234"));
    }

    #[test]
    fn should_accept_a_forced_port_outside_every_other_range() {
        let mut config = config_with("PORT", LOCAL_CONFIG_FILE);
        config.env[0].value = "3333".to_owned();

        let findings = check_forced_port_conflicts(
            &config,
            &[managed("PORT")],
            &registry_with_other_instance(),
            Path::new("/work/mine"),
        );

        assert_eq!(findings, Vec::new());
    }

    #[test]
    fn should_accept_a_forced_port_inside_the_range_of_the_instance_itself() {
        let mut config = config_with("PORT", LOCAL_CONFIG_FILE);
        config.env[0].value = "20060".to_owned();

        let findings = check_forced_port_conflicts(
            &config,
            &[managed("PORT")],
            &registry_with_other_instance(),
            Path::new("/work/other"),
        );

        assert_eq!(findings, Vec::new());
    }
}
