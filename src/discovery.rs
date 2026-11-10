//! Port discovery: the variables devslot would manage, in the order of the spec.

use serde::Serialize;

use crate::compose::probe::ProbedCompose;
use crate::compose::sentinels::usual_value;
use crate::project::dotenv::{EnvFile, lookup_value};
use crate::project::vite::{DEFAULT_HMR_PORT, HMR_PORT_VARIABLE, ViteDetection};
use crate::project_config::{ProjectConfig, has_port_suffix};

const APP_PORT_VARIABLE: &str = "PORT";
const DOTENV_FILE: &str = ".env";
const DOTENV_EXAMPLE_FILE: &str = ".env.example";

/// Whether the port serves the app or a compose service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PortKind {
    /// `PORT`, extra ports and the Vite HMR port.
    Web,
    /// A port published by a compose service.
    Service,
}

impl PortKind {
    /// The word the registry and the JSON output use.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Service => "service",
        }
    }

    /// The kind written as `word`, when it is one.
    pub fn from_word(word: &str) -> Option<Self> {
        [Self::Web, Self::Service]
            .into_iter()
            .find(|kind| kind.word() == word)
    }
}

/// Where the variable was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PortSource {
    /// `.env` or `.env.example`.
    Dotenv,
    /// `devslot.toml` or `devslot.local.toml`.
    Config,
    /// `package.json` or the Vite config.
    Vite,
    /// The dev compose file.
    Compose,
}

/// A port variable devslot would manage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManagedPort {
    /// The variable name.
    #[serde(rename = "var")]
    pub variable: String,
    /// Web or service.
    pub kind: PortKind,
    /// The port without devslot, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<u16>,
    /// Where the variable was found.
    pub source: PortSource,
    /// The file it was found in, relative to the checkout root.
    pub file: String,
    /// The compose service that publishes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// The container port behind it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<u16>,
}

/// Everything discovery reads.
pub struct DiscoveryInput<'a> {
    /// `.env` without its devslot block, when it could be read.
    pub env: Option<&'a EnvFile>,
    /// `.env.example`, when it could be read.
    pub example: Option<&'a EnvFile>,
    /// The merged project configuration.
    pub config: &'a ProjectConfig,
    /// The Vite files found.
    pub vite: &'a ViteDetection,
    /// The dev compose file name and its probe, when it could be read.
    pub dev_compose: Option<(&'a str, &'a ProbedCompose)>,
}

/// Lists the managed variables, one entry each; the first source wins.
pub fn discover_ports(input: &DiscoveryInput<'_>) -> Vec<ManagedPort> {
    let mut ports = Vec::new();
    add_app_port(input, &mut ports);
    add_extra_ports(input, &mut ports);
    add_vite_port(input, &mut ports);
    add_compose_ports(input, &mut ports);
    ports
}

fn parse_port(text: Option<&str>) -> Option<u16> {
    text.and_then(|value| value.trim().parse().ok())
}

fn add_app_port(input: &DiscoveryInput<'_>, ports: &mut Vec<ManagedPort>) {
    let from_env = input.env.and_then(|file| file.value_of(APP_PORT_VARIABLE));
    let from_example = input
        .example
        .and_then(|file| file.value_of(APP_PORT_VARIABLE));
    let (value, file) = match (from_env, from_example) {
        (Some(value), _) => (value, DOTENV_FILE),
        (None, Some(value)) => (value, DOTENV_EXAMPLE_FILE),
        (None, None) => return,
    };
    ports.push(ManagedPort {
        variable: APP_PORT_VARIABLE.to_owned(),
        kind: PortKind::Web,
        default: parse_port(Some(value)),
        source: PortSource::Dotenv,
        file: file.to_owned(),
        service: None,
        target: None,
    });
}

fn add_extra_ports(input: &DiscoveryInput<'_>, ports: &mut Vec<ManagedPort>) {
    let list = &input.config.extra_ports;
    for name in &list.names {
        if ports.iter().any(|port| &port.variable == name) {
            continue;
        }
        let value = lookup_value(input.env, input.example, name);
        ports.push(ManagedPort {
            variable: name.clone(),
            kind: PortKind::Web,
            default: parse_port(value),
            source: PortSource::Config,
            file: list.file.unwrap_or_default().to_owned(),
            service: None,
            target: None,
        });
    }
}

fn add_vite_port(input: &DiscoveryInput<'_>, ports: &mut Vec<ManagedPort>) {
    let Some(source_file) = input.vite.source_file() else {
        return;
    };
    if ports.iter().any(|port| port.variable == HMR_PORT_VARIABLE) {
        return;
    }
    ports.push(ManagedPort {
        variable: HMR_PORT_VARIABLE.to_owned(),
        kind: PortKind::Web,
        default: Some(DEFAULT_HMR_PORT),
        source: PortSource::Vite,
        file: source_file.display().to_string(),
        service: None,
        target: None,
    });
}

fn add_compose_ports(input: &DiscoveryInput<'_>, ports: &mut Vec<ManagedPort>) {
    let Some((file, probed)) = input.dev_compose else {
        return;
    };
    for (service_name, service) in &probed.config.services {
        for mapping in &service.ports {
            let Some(variable) = probed.variable_published_by(mapping) else {
                continue;
            };
            if !has_port_suffix(variable) {
                continue;
            }
            if input
                .config
                .ignored_ports
                .names
                .iter()
                .any(|name| name == variable)
            {
                continue;
            }
            let target = probed.container_port(mapping);
            if let Some(existing) = ports.iter_mut().find(|port| port.variable == variable) {
                fill_compose_details(existing, service_name, target);
                continue;
            }
            let compose_default = probed.defaults.get(variable).map_or("", String::as_str);
            let usual = usual_value(compose_default, || {
                lookup_value(input.env, input.example, variable).map(str::to_owned)
            });
            ports.push(ManagedPort {
                variable: variable.to_owned(),
                kind: PortKind::Service,
                default: parse_port(usual.as_deref()),
                source: PortSource::Compose,
                file: file.to_owned(),
                service: Some(service_name.clone()),
                target,
            });
        }
    }
}

fn fill_compose_details(existing: &mut ManagedPort, service: &str, target: Option<u16>) {
    if existing.service.is_some() {
        return;
    }
    existing.service = Some(service.to_owned());
    existing.target = target;
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::compose::probe::probe;
    use crate::compose::probe::testing::FakeComposeCli;
    use crate::project_config::VariableList;

    fn env(text: &str) -> EnvFile {
        EnvFile::parse(text).unwrap()
    }

    fn vite_at(file: &str) -> ViteDetection {
        ViteDetection {
            manifests: vec![file.into()],
            configs: vec![],
        }
    }

    fn probed(variables: &[(&str, &str)], config: &str) -> ProbedCompose {
        let cli = FakeComposeCli::new(variables, config);
        probe(&cli, &[], &|_| None, &[]).unwrap()
    }

    const POSTGRES: &str =
        r#"{"services":{"postgres":{"ports":[{"target":5432,"published":"61000"}]}}}"#;

    fn variables_of(ports: &[ManagedPort]) -> Vec<&str> {
        ports.iter().map(|port| port.variable.as_str()).collect()
    }

    fn discover(
        env_file: Option<&EnvFile>,
        example: Option<&EnvFile>,
        config: &ProjectConfig,
        vite: &ViteDetection,
        dev_compose: Option<(&str, &ProbedCompose)>,
    ) -> Vec<ManagedPort> {
        discover_ports(&DiscoveryInput {
            env: env_file,
            example,
            config,
            vite,
            dev_compose,
        })
    }

    #[test]
    fn should_find_the_template_ports_in_the_spec_order() {
        let env_file = env("PORT=3333\nDB_PORT=5432\n");
        let compose = probed(&[("DB_PORT", "")], POSTGRES);

        let ports = discover(
            Some(&env_file),
            None,
            &ProjectConfig::default(),
            &vite_at("package.json"),
            Some(("dev.compose.yml", &compose)),
        );

        assert_eq!(
            ports,
            vec![
                ManagedPort {
                    variable: "PORT".into(),
                    kind: PortKind::Web,
                    default: Some(3333),
                    source: PortSource::Dotenv,
                    file: ".env".into(),
                    service: None,
                    target: None
                },
                ManagedPort {
                    variable: "VITE_HMR_PORT".into(),
                    kind: PortKind::Web,
                    default: Some(24678),
                    source: PortSource::Vite,
                    file: "package.json".into(),
                    service: None,
                    target: None
                },
                ManagedPort {
                    variable: "DB_PORT".into(),
                    kind: PortKind::Service,
                    default: Some(5432),
                    source: PortSource::Compose,
                    file: "dev.compose.yml".into(),
                    service: Some("postgres".into()),
                    target: Some(5432)
                },
            ]
        );
    }

    #[test]
    fn should_read_the_app_port_from_the_example_when_env_lacks_it() {
        let example = env("PORT=3000\n");

        let ports = discover(
            Some(&env("A=1\n")),
            Some(&example),
            &ProjectConfig::default(),
            &ViteDetection::default(),
            None,
        );

        assert_eq!(
            (ports[0].default, ports[0].file.as_str()),
            (Some(3000), ".env.example")
        );
    }

    #[test]
    fn should_not_manage_the_app_port_when_neither_file_defines_it() {
        let ports = discover(
            None,
            None,
            &ProjectConfig::default(),
            &ViteDetection::default(),
            None,
        );

        assert_eq!(ports, vec![]);
    }

    #[test]
    fn should_add_the_extra_ports_with_their_env_default_and_config_source() {
        let config = ProjectConfig {
            extra_ports: VariableList {
                names: vec!["FRONT_PORT".into()],
                file: Some("devslot.toml"),
            },
            ..ProjectConfig::default()
        };

        let ports = discover(
            Some(&env("FRONT_PORT=4000\n")),
            None,
            &config,
            &ViteDetection::default(),
            None,
        );

        assert_eq!(
            (ports[0].default, ports[0].source, ports[0].file.as_str()),
            (Some(4000), PortSource::Config, "devslot.toml")
        );
    }

    #[test]
    fn should_use_the_vite_config_as_source_when_no_manifest_lists_vite() {
        let vite = ViteDetection {
            manifests: vec![],
            configs: vec!["apps/web/vite.config.ts".into()],
        };

        let ports = discover(None, None, &ProjectConfig::default(), &vite, None);

        assert_eq!(ports[0].file, "apps/web/vite.config.ts");
    }

    #[test]
    fn should_skip_an_ignored_compose_variable() {
        let compose = probed(&[("DB_PORT", "5432")], POSTGRES);
        let config = ProjectConfig {
            ignored_ports: VariableList {
                names: vec!["DB_PORT".into()],
                file: Some("devslot.toml"),
            },
            ..ProjectConfig::default()
        };

        let ports = discover(
            None,
            None,
            &config,
            &ViteDetection::default(),
            Some(("c.yml", &compose)),
        );

        assert_eq!(ports, vec![]);
    }

    #[test]
    fn should_keep_one_entry_and_add_the_service_when_the_compose_also_publishes_the_variable() {
        let config = r#"{"services":{"app":{"ports":[{"target":3333,"published":"61000"}]}}}"#;
        let compose = probed(&[("PORT", "")], config);

        let ports = discover(
            Some(&env("PORT=3333\n")),
            None,
            &ProjectConfig::default(),
            &ViteDetection::default(),
            Some(("c.yml", &compose)),
        );

        assert_eq!(
            (
                variables_of(&ports),
                ports[0].kind,
                ports[0].source,
                ports[0].service.clone(),
                ports[0].target
            ),
            (
                vec!["PORT"],
                PortKind::Web,
                PortSource::Dotenv,
                Some("app".to_owned()),
                Some(3333)
            )
        );
    }

    #[test]
    fn should_use_the_env_value_as_default_when_the_compose_has_no_default() {
        let compose = probed(&[("DB_PORT", "")], POSTGRES);

        let ports = discover(
            Some(&env("DB_PORT=5433\n")),
            None,
            &ProjectConfig::default(),
            &ViteDetection::default(),
            Some(("c.yml", &compose)),
        );

        assert_eq!(ports[0].default, Some(5433));
    }

    #[test]
    fn should_have_no_default_when_no_source_knows_the_value() {
        let compose = probed(&[("DB_PORT", "")], POSTGRES);

        let ports = discover(
            None,
            None,
            &ProjectConfig::default(),
            &ViteDetection::default(),
            Some(("c.yml", &compose)),
        );

        assert_eq!(ports[0].default, None);
    }

    #[test]
    fn should_not_list_a_hardcoded_published_port() {
        let config = r#"{"services":{"db":{"ports":[{"target":5432,"published":"5432"}]}}}"#;
        let compose = probed(&[("DB_PORT", "")], config);

        let ports = discover(
            None,
            None,
            &ProjectConfig::default(),
            &ViteDetection::default(),
            Some(("c.yml", &compose)),
        );

        assert_eq!(ports, vec![]);
    }
}
