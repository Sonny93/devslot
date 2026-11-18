//! The variables an instance writes into the devslot block of its `.env`.

use std::path::Path;

use thiserror::Error;

use crate::compose::selection::{DevComposeSelection, default_named_compose, existing_overrides};
use crate::environment::{
    COMPOSE_FILE_VARIABLE, EnvironmentError, PROJECT_NAME_VARIABLE, build_environment,
};
use crate::project_config::EnvSetting;
use crate::registry::record::InstanceRecord;

const COMPOSE_FILE_SEPARATOR: char = ':';

/// Why the entries of the block cannot be built.
#[derive(Debug, Error)]
pub enum BlockEntriesError {
    /// The environment of the instance cannot be built.
    #[error(transparent)]
    Environment(#[from] EnvironmentError),
    /// A compose file name holds the separator of `COMPOSE_FILE`.
    #[error("the compose file name {file:?} contains ':', which COMPOSE_FILE cannot hold")]
    SeparatorInFileName {
        /// The file name.
        file: String,
    },
}

/// The `(name, value)` lines of the block of `instance`: its ports, the
/// compose project, `COMPOSE_FILE` when a bare `docker compose` would not find
/// the dev compose by itself, the devslot ids, then `[env]`.
pub fn block_entries(
    instance: &InstanceRecord,
    root: &Path,
    selection: &DevComposeSelection,
    settings: &[EnvSetting],
) -> Result<Vec<(String, String)>, BlockEntriesError> {
    let environment = build_environment(instance, None, settings)?;
    let mut entries: Vec<(String, String)> = environment
        .variables
        .into_iter()
        .map(|variable| (variable.name, variable.value))
        .collect();
    if let Some(value) = compose_file_value(root, selection)? {
        let after_project = entries
            .iter()
            .position(|(name, _)| name == PROJECT_NAME_VARIABLE)
            .map_or(entries.len(), |position| position + 1);
        entries.insert(after_project, (COMPOSE_FILE_VARIABLE.to_owned(), value));
    }
    Ok(entries)
}

fn compose_file_value(
    root: &Path,
    selection: &DevComposeSelection,
) -> Result<Option<String>, BlockEntriesError> {
    let DevComposeSelection::Selected(file) = selection else {
        return Ok(None);
    };
    if default_named_compose(root).is_some() {
        return Ok(None);
    }
    let files: Vec<String> = std::iter::once(file.clone())
        .chain(existing_overrides(root))
        .collect();
    if let Some(file) = files
        .iter()
        .find(|file| file.contains(COMPOSE_FILE_SEPARATOR))
    {
        return Err(BlockEntriesError::SeparatorInFileName { file: file.clone() });
    }
    Ok(Some(files.join(&COMPOSE_FILE_SEPARATOR.to_string())))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::discovery::PortKind;
    use crate::port_range::PortRange;
    use crate::project_config::{CONFIG_FILE, LOCAL_CONFIG_FILE};
    use crate::registry::record::{AssignedPort, PortDetail};

    fn instance() -> InstanceRecord {
        InstanceRecord {
            path: PathBuf::from("/work/app"),
            name: "app-3f2a".to_owned(),
            slot: 1,
            range: PortRange {
                first: 20050,
                last: 20099,
            },
            ports: vec![AssignedPort {
                variable: "PORT".to_owned(),
                port: 20050,
                detail: PortDetail {
                    kind: PortKind::Web,
                    service: None,
                    target: None,
                },
            }],
            host: "app.localhost".to_owned(),
            web_port_variable: None,
            is_host_unsaved: false,
        }
    }

    fn setting(key: &str, value: &str, file: &'static str) -> EnvSetting {
        EnvSetting {
            key: key.to_owned(),
            value: value.to_owned(),
            file,
        }
    }

    fn selected(file: &str) -> DevComposeSelection {
        DevComposeSelection::Selected(file.to_owned())
    }

    fn names(entries: &[(String, String)]) -> Vec<&str> {
        entries.iter().map(|(name, _)| name.as_str()).collect()
    }

    fn value_of<'a>(entries: &'a [(String, String)], name: &str) -> Option<&'a str> {
        entries
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn should_not_add_compose_file_when_a_default_named_compose_exists() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("compose.yaml"), "").unwrap();

        let entries =
            block_entries(&instance(), root.path(), &selected("compose.yaml"), &[]).unwrap();

        assert_eq!(
            names(&entries),
            [
                "PORT",
                "COMPOSE_PROJECT_NAME",
                "DEVSLOT_INSTANCE",
                "DEVSLOT_SLOT",
                "DEVSLOT_HOST"
            ]
        );
    }

    #[test]
    fn should_not_add_compose_file_when_no_dev_compose_is_selected() {
        let root = tempfile::tempdir().unwrap();

        let entries = block_entries(
            &instance(),
            root.path(),
            &DevComposeSelection::NoneFound,
            &[],
        )
        .unwrap();

        assert_eq!(value_of(&entries, "COMPOSE_FILE"), None);
    }

    #[test]
    fn should_list_the_dev_compose_then_the_override_relative_to_the_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("dev.compose.yml"), "").unwrap();
        std::fs::write(root.path().join("compose.override.yml"), "").unwrap();

        let entries =
            block_entries(&instance(), root.path(), &selected("dev.compose.yml"), &[]).unwrap();

        assert_eq!(
            (names(&entries), value_of(&entries, "COMPOSE_FILE")),
            (
                vec![
                    "PORT",
                    "COMPOSE_PROJECT_NAME",
                    "COMPOSE_FILE",
                    "DEVSLOT_INSTANCE",
                    "DEVSLOT_SLOT",
                    "DEVSLOT_HOST"
                ],
                Some("dev.compose.yml:compose.override.yml")
            )
        );
    }

    #[test]
    fn should_list_only_the_dev_compose_when_there_is_no_override() {
        let root = tempfile::tempdir().unwrap();

        let entries =
            block_entries(&instance(), root.path(), &selected("dev.compose.yml"), &[]).unwrap();

        assert_eq!(value_of(&entries, "COMPOSE_FILE"), Some("dev.compose.yml"));
    }

    #[test]
    fn should_refuse_a_dev_compose_name_with_a_colon() {
        let root = tempfile::tempdir().unwrap();

        let outcome = block_entries(&instance(), root.path(), &selected("a:b.yml"), &[]);

        assert!(matches!(
            outcome,
            Err(BlockEntriesError::SeparatorInFileName { .. })
        ));
    }

    #[test]
    fn should_write_the_host_of_the_instance_after_its_ids() {
        let root = tempfile::tempdir().unwrap();

        let entries = block_entries(
            &instance(),
            root.path(),
            &DevComposeSelection::NoneFound,
            &[],
        )
        .unwrap();

        assert_eq!(value_of(&entries, "DEVSLOT_HOST"), Some("app.localhost"));
    }

    #[test]
    fn should_write_the_forced_port_of_the_local_file_in_place_of_the_assigned_one() {
        let root = tempfile::tempdir().unwrap();
        let settings = [setting("PORT", "3000", LOCAL_CONFIG_FILE)];

        let entries = block_entries(
            &instance(),
            root.path(),
            &DevComposeSelection::NoneFound,
            &settings,
        )
        .unwrap();

        assert_eq!(value_of(&entries, "PORT"), Some("3000"));
    }

    #[test]
    fn should_refuse_a_forced_port_in_the_committed_file() {
        let root = tempfile::tempdir().unwrap();
        let settings = [setting("PORT", "3000", CONFIG_FILE)];

        let outcome = block_entries(
            &instance(),
            root.path(),
            &DevComposeSelection::NoneFound,
            &settings,
        );

        assert!(matches!(
            outcome,
            Err(BlockEntriesError::Environment(
                EnvironmentError::CommittedPortOverride { .. }
            ))
        ));
    }

    #[test]
    fn should_substitute_references_and_put_the_env_values_last() {
        let root = tempfile::tempdir().unwrap();
        let settings = [setting("API_URL", "http://localhost:${PORT}", CONFIG_FILE)];

        let entries = block_entries(
            &instance(),
            root.path(),
            &DevComposeSelection::NoneFound,
            &settings,
        )
        .unwrap();

        assert_eq!(
            (
                names(&entries).last().copied(),
                value_of(&entries, "API_URL")
            ),
            (Some("API_URL"), Some("http://localhost:20050"))
        );
    }
}
