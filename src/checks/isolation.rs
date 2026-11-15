//! The isolation checks on the model as Compose will really resolve it (with
//! the checkout's `.env` loaded), run right before services are started.
//! Messages name keys only, never a resolved name or any value.

use crate::compose::model::{ComposeConfig, Resource};

/// What stops the model from being isolated per instance: a service with a
/// `container_name`, and a volume or network that is `external` or whose name
/// does not start with `<instance>_` or `<instance>-`.
pub fn isolation_problems(config: &ComposeConfig, instance_name: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for (key, service) in &config.services {
        if service.container_name.is_some() {
            problems.push(format!(
                "service {key} sets container_name: two instances would want the same container name"
            ));
        }
    }
    problems.extend(resource_problems("volume", &config.volumes, instance_name));
    problems.extend(resource_problems(
        "network",
        &config.networks,
        instance_name,
    ));
    problems
}

fn resource_problems(
    kind: &str,
    resources: &std::collections::BTreeMap<String, Resource>,
    instance_name: &str,
) -> Vec<String> {
    resources
        .iter()
        .filter(|(_, resource)| !follows_project(resource, instance_name))
        .map(|(key, _)| {
            format!(
                "{kind} {key} is external or has a name that does not follow the compose project: it would be shared between instances"
            )
        })
        .collect()
}

fn follows_project(resource: &Resource, instance_name: &str) -> bool {
    if resource.is_external() {
        return false;
    }
    let Some(name) = &resource.name else {
        return true;
    };
    ['_', '-']
        .iter()
        .any(|separator| name.starts_with(&format!("{instance_name}{separator}")))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn problems(json: &str) -> Vec<String> {
        isolation_problems(&ComposeConfig::from_json(json).unwrap(), "app-3f2a")
    }

    #[test]
    fn should_accept_a_model_whose_resources_follow_the_project() {
        let model = r#"{"services":{"db":{"image":"x"}},
            "volumes":{"data":{"name":"app-3f2a_data"}},
            "networks":{"default":{"name":"app-3f2a_default"}}}"#;

        assert_eq!(problems(model), Vec::<String>::new());
    }

    #[test]
    fn should_refuse_a_volume_name_chosen_through_an_env_value_without_printing_it() {
        let model = r#"{"volumes":{"data":{"name":"victim_pgdata"}}}"#;

        let found = problems(model);

        assert_eq!(found.len(), 1);
        assert!(found[0].starts_with("volume data is external or has a name"));
        assert!(!found[0].contains("victim_pgdata"));
    }

    #[test]
    fn should_refuse_an_external_volume_and_network() {
        let model = r#"{"volumes":{"a":{"name":"app-3f2a_a","external":true}},
            "networks":{"b":{"name":"app-3f2a_b","external":{}}}}"#;

        let found = problems(model);

        assert_eq!(found.len(), 2);
        assert!(found[0].starts_with("volume a "));
        assert!(found[1].starts_with("network b "));
    }

    #[test]
    fn should_refuse_a_service_with_a_container_name_without_printing_it() {
        let model = r#"{"services":{"web":{"container_name":"FAKE-fixed"}}}"#;

        let found = problems(model);

        assert_eq!(found.len(), 1);
        assert!(found[0].starts_with("service web sets container_name"));
        assert!(!found[0].contains("FAKE-fixed"));
    }

    #[test]
    fn should_not_accept_a_name_that_only_shares_the_beginning_of_the_instance_name() {
        let model = r#"{"volumes":{"data":{"name":"app-3f2ab_data"}}}"#;

        assert_eq!(problems(model).len(), 1);
    }

    #[test]
    fn should_accept_the_dash_separator_of_a_name_built_from_the_project() {
        let model = r#"{"volumes":{"data":{"name":"app-3f2a-data"}}}"#;

        assert_eq!(problems(model), Vec::<String>::new());
    }
}
