//! Checks on the merged configuration of one compose file.

use std::path::Path;

use super::finding::{Check, Finding, Level};
use crate::compose::probe::ProbedCompose;
use crate::docker::PROBE_PROJECT;

const DOTENV_FILE: &str = ".env";
const BIND_MOUNT: &str = "bind";

/// What the compose checks need to know about the checkout.
pub struct ComposeCheckContext<'a> {
    /// The checkout root.
    pub root: &'a Path,
    /// The managed port variables, in discovery order.
    pub managed_variables: &'a [String],
}

/// Runs every compose check on `probed`, the configuration of `file`.
pub fn check_compose(
    file: &str,
    probed: &ProbedCompose,
    context: &ComposeCheckContext<'_>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    findings.extend(check_container_names(file, probed));
    findings.extend(check_explicit_names(file, probed));
    findings.extend(check_published_ports(file, probed));
    findings.extend(check_env_file_ports(file, probed, context));
    findings.extend(check_bind_mounts(file, probed, context));
    findings
}

fn check_container_names(file: &str, probed: &ProbedCompose) -> Vec<Finding> {
    probed
        .config
        .services
        .iter()
        .filter(|(_, service)| service.container_name.is_some())
        .map(|(name, _)| {
            Finding::new(
                Level::Error,
                Check::ContainerName,
                "service sets container_name: two instances would want the same container name; remove it",
            )
            .in_file(file)
            .in_service(name)
            .for_key("container_name")
        })
        .collect()
}

fn check_explicit_names(file: &str, probed: &ProbedCompose) -> Vec<Finding> {
    let volumes = probed.config.volumes.iter().map(|entry| ("volume", entry));
    let networks = probed
        .config
        .networks
        .iter()
        .map(|entry| ("network", entry));
    volumes
        .chain(networks)
        .filter_map(|(kind, (key, resource))| {
            let reason = if resource.is_external() {
                "is external"
            } else {
                let name = resource.name.as_deref()?;
                if name.starts_with(PROBE_PROJECT) {
                    return None;
                }
                "has a fixed name"
            };
            Some(
                Finding::new(
                    Level::Error,
                    Check::ExplicitName,
                    format!(
                        "{kind} \"{key}\" {reason}: it would be shared between instances despite the separate compose project"
                    ),
                )
                .in_file(file)
                .for_key(key),
            )
        })
        .collect()
}

fn check_published_ports(file: &str, probed: &ProbedCompose) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (service_name, service) in &probed.config.services {
        for mapping in &service.ports {
            let Some(published) = mapping.published.as_deref().filter(|text| !text.is_empty())
            else {
                continue;
            };
            if probed.variable_published_by(mapping).is_some() {
                continue;
            }
            findings.push(describe_literal_port(
                file,
                service_name,
                published,
                mapping.target,
            ));
        }
    }
    findings
}

fn describe_literal_port(
    file: &str,
    service: &str,
    published: &str,
    target: Option<u64>,
) -> Finding {
    let is_single_port = published
        .chars()
        .all(|character| character.is_ascii_digit());
    if !is_single_port {
        return Finding::new(
            Level::Warning,
            Check::PortRange,
            format!(
                "service publishes the port range {published}: ranges are not managed by devslot"
            ),
        )
        .in_file(file)
        .in_service(service)
        .for_key(published);
    }
    let target_text = target.map_or_else(|| published.to_owned(), |port| port.to_string());
    Finding::new(
        Level::Warning,
        Check::HardcodedPort,
        format!(
            "service publishes host port {published} as a literal: write it as ${{VAR:-{published}}}:{target_text} so devslot can manage it, otherwise it conflicts with a second instance"
        ),
    )
    .in_file(file)
    .in_service(service)
    .for_key(published)
}

fn check_env_file_ports(
    file: &str,
    probed: &ProbedCompose,
    context: &ComposeCheckContext<'_>,
) -> Vec<Finding> {
    let dotenv_path = context.root.join(DOTENV_FILE);
    probed
        .config
        .services
        .iter()
        .filter(|(_, service)| {
            service
                .env_file
                .iter()
                .any(|entry| is_same_file(Path::new(entry.path()), &dotenv_path))
        })
        .filter_map(|(name, service)| {
            let unpinned: Vec<&str> = context
                .managed_variables
                .iter()
                .map(String::as_str)
                .filter(|variable| !service.pins(variable))
                .collect();
            if unpinned.is_empty() {
                return None;
            }
            Some(
                Finding::new(
                    Level::Warning,
                    Check::EnvFilePorts,
                    format!(
                        "service loads .env with env_file but does not pin {} in environment: the container would receive host-side ports",
                        unpinned.join(", ")
                    ),
                )
                .in_file(file)
                .in_service(name),
            )
        })
        .collect()
}

fn is_same_file(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    matches!((left.canonicalize(), right.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

fn check_bind_mounts(
    file: &str,
    probed: &ProbedCompose,
    context: &ComposeCheckContext<'_>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (service_name, service) in &probed.config.services {
        for mount in service
            .volumes
            .iter()
            .filter(|mount| mount.kind == BIND_MOUNT)
        {
            let Some(source) = mount.source.as_deref() else {
                continue;
            };
            if Path::new(source).starts_with(context.root) {
                continue;
            }
            findings.push(
                Finding::new(
                    Level::Warning,
                    Check::BindMount,
                    format!("service mounts {source}, outside the checkout: instances would share the same files"),
                )
                .in_file(file)
                .in_service(service_name)
                .for_key(source),
            );
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::compose::probe::probe;
    use crate::compose::probe::testing::FakeComposeCli;

    const ROOT: &str = "/work/app";

    fn probed(variables: &[(&str, &str)], config: &str) -> ProbedCompose {
        probe(&FakeComposeCli::new(variables, config), &[], &|_| None).unwrap()
    }

    fn run(variables: &[(&str, &str)], config: &str, managed: &[&str]) -> Vec<Finding> {
        let managed: Vec<String> = managed.iter().map(|name| (*name).to_owned()).collect();
        check_compose(
            "compose.yml",
            &probed(variables, config),
            &ComposeCheckContext {
                root: Path::new(ROOT),
                managed_variables: &managed,
            },
        )
    }

    fn checks_of(findings: &[Finding]) -> Vec<Check> {
        findings.iter().map(|finding| finding.check).collect()
    }

    #[test]
    fn should_report_an_error_naming_the_service_when_it_sets_container_name() {
        let findings = run(
            &[],
            r#"{"services":{"my-links":{"container_name":"my-links"}}}"#,
            &[],
        );

        assert_eq!(
            (
                findings[0].level,
                findings[0].check,
                findings[0].service.as_deref(),
                findings[0].key.as_deref()
            ),
            (
                Level::Error,
                Check::ContainerName,
                Some("my-links"),
                Some("container_name")
            )
        );
    }

    #[test]
    fn should_report_nothing_when_no_service_sets_container_name() {
        assert_eq!(
            run(&[], r#"{"services":{"db":{"container_name":null}}}"#, &[]),
            vec![]
        );
    }

    #[test]
    fn should_accept_a_volume_whose_name_follows_the_project() {
        let config = r#"{"volumes":{"data":{"name":"devslot-probe_data"},"other":{"name":"devslot-probe-data"}}}"#;

        assert_eq!(run(&[], config, &[]), vec![]);
    }

    #[test]
    fn should_report_an_error_when_a_volume_has_an_explicit_name() {
        let findings = run(&[], r#"{"volumes":{"data":{"name":"shared-data"}}}"#, &[]);

        assert_eq!(
            (checks_of(&findings), findings[0].key.as_deref()),
            (vec![Check::ExplicitName], Some("data"))
        );
    }

    #[test]
    fn should_report_one_error_when_a_network_is_external() {
        let findings = run(
            &[],
            r#"{"networks":{"proxy":{"name":"proxy","external":true}}}"#,
            &[],
        );

        assert_eq!((findings.len(), findings[0].level), (1, Level::Error));
    }

    #[test]
    fn should_report_a_warning_when_a_port_is_published_as_a_literal() {
        let config = r#"{"services":{"db":{"ports":[{"target":5432,"published":"5432"}]}}}"#;

        let findings = run(&[("DB_PORT", "")], config, &[]);

        assert_eq!(
            (
                checks_of(&findings),
                findings[0].level,
                findings[0].key.as_deref()
            ),
            (vec![Check::HardcodedPort], Level::Warning, Some("5432"))
        );
    }

    #[test]
    fn should_report_nothing_when_the_published_port_comes_from_a_variable() {
        let config = r#"{"services":{"db":{"ports":[{"target":5432,"published":"61000"}]}}}"#;

        assert_eq!(run(&[("DB_PORT", "")], config, &[]), vec![]);
    }

    #[test]
    fn should_report_nothing_when_the_published_port_is_left_to_docker() {
        let config = r#"{"services":{"db":{"ports":[{"target":5432}]}}}"#;

        assert_eq!(run(&[], config, &[]), vec![]);
    }

    #[test]
    fn should_report_a_range_warning_when_a_port_range_is_published() {
        let config = r#"{"services":{"db":{"ports":[{"target":5432,"published":"8000-8010"}]}}}"#;

        assert_eq!(checks_of(&run(&[], config, &[])), vec![Check::PortRange]);
    }

    fn env_file_service(environment: &str) -> String {
        format!(
            r#"{{"services":{{"app":{{"env_file":[{{"path":"{ROOT}/.env"}}],"environment":{environment}}}}}}}"#
        )
    }

    #[test]
    fn should_warn_once_naming_every_unpinned_variable_when_the_service_loads_env() {
        let findings = run(
            &[],
            &env_file_service(r#"{"DB_HOST":"postgres"}"#),
            &["PORT", "DB_PORT"],
        );

        assert_eq!(checks_of(&findings), vec![Check::EnvFilePorts]);
        assert!(
            findings[0].message.contains("PORT, DB_PORT"),
            "{}",
            findings[0].message
        );
    }

    #[test]
    fn should_name_only_the_unpinned_variables_when_some_are_pinned() {
        let findings = run(
            &[],
            &env_file_service(r#"{"PORT":"3333"}"#),
            &["PORT", "DB_PORT"],
        );

        assert!(
            findings[0].message.contains("does not pin DB_PORT in"),
            "{}",
            findings[0].message
        );
    }

    #[test]
    fn should_report_nothing_when_every_managed_variable_is_pinned() {
        let findings = run(
            &[],
            &env_file_service(r#"{"PORT":"3333","DB_PORT":"5432"}"#),
            &["PORT", "DB_PORT"],
        );

        assert_eq!(findings, vec![]);
    }

    #[test]
    fn should_report_nothing_when_the_service_loads_another_env_file() {
        let config = r#"{"services":{"app":{"env_file":[{"path":"/work/app/.env.prod"}]}}}"#;

        assert_eq!(run(&[], config, &["PORT"]), vec![]);
    }

    #[test]
    fn should_warn_when_a_bind_mount_source_is_outside_the_checkout() {
        let config = r#"{"services":{"app":{"volumes":[{"type":"bind","source":"/var/data","target":"/d"}]}}}"#;

        let findings = run(&[], config, &[]);

        assert_eq!(
            (checks_of(&findings), findings[0].key.as_deref()),
            (vec![Check::BindMount], Some("/var/data"))
        );
    }

    #[test]
    fn should_not_warn_when_a_bind_mount_source_is_inside_the_checkout() {
        let config = r#"{"services":{"app":{"volumes":[{"type":"bind","source":"/work/app/src","target":"/s"}]}}}"#;

        assert_eq!(run(&[], config, &[]), vec![]);
    }

    #[test]
    fn should_not_warn_when_a_named_volume_is_mounted() {
        let config =
            r#"{"services":{"app":{"volumes":[{"type":"volume","source":"data","target":"/d"}]}}}"#;

        assert_eq!(run(&[], config, &[]), vec![]);
    }

    #[test]
    fn should_warn_for_a_sibling_folder_that_shares_the_root_prefix() {
        let config = r#"{"services":{"app":{"volumes":[{"type":"bind","source":"/work/app-other","target":"/d"}]}}}"#;

        assert_eq!(checks_of(&run(&[], config, &[])), vec![Check::BindMount]);
    }
}
