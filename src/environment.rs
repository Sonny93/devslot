//! The environment of an instance: its ports, its compose project, and the
//! values `[env]` builds from them. Built from the registry and the files of
//! the checkout only: no Docker call.

use std::fmt::Write;
use std::path::{Path, PathBuf};

use serde::ser::{Serialize, SerializeMap, Serializer};
use thiserror::Error;

use crate::compose::selection::{DevComposeSelection, existing_overrides};
use crate::project_config::{CONFIG_FILE, EnvSetting, is_valid_variable_name};
use crate::references::substitute_references;
use crate::registry::record::InstanceRecord;
use crate::shell_quote::shell_quote;

/// The compose project name variable.
pub const PROJECT_NAME_VARIABLE: &str = "COMPOSE_PROJECT_NAME";
/// The compose files variable.
pub const COMPOSE_FILE_VARIABLE: &str = "COMPOSE_FILE";
/// The instance name variable.
pub const INSTANCE_VARIABLE: &str = "DEVSLOT_INSTANCE";
/// The slot number variable.
pub const SLOT_VARIABLE: &str = "DEVSLOT_SLOT";
/// The local host name variable.
pub const HOST_VARIABLE: &str = "DEVSLOT_HOST";

const COMPOSE_FILE_SEPARATOR: &str = ":";

/// Why the environment cannot be built. These are configuration errors.
#[derive(Debug, Error)]
pub enum EnvironmentError {
    /// `[env]` refers to a variable the instance does not have.
    #[error("[env] key {key} refers to ${{{name}}}, which is not a variable of this instance")]
    UnknownReference {
        /// The `[env]` key whose value holds the reference.
        key: String,
        /// The unknown name.
        name: String,
    },
    /// A variable name cannot be exported safely.
    #[error("refusing to export the variable name {name:?}")]
    InvalidName {
        /// The name.
        name: String,
    },
    /// The committed file sets a managed port variable.
    #[error(
        "[env] key {key} in devslot.toml would replace a managed port; move it to devslot.local.toml"
    )]
    CommittedPortOverride {
        /// The port variable.
        key: String,
    },
}

/// Where a variable comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariableSource {
    /// A port of the instance.
    Port,
    /// The compose project, the compose files, the devslot ids.
    Instance,
    /// A value built by `[env]`.
    Config,
}

/// One variable of the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentVariable {
    /// The name.
    pub name: String,
    /// The value. Never printed outside `ds env`.
    pub value: String,
    /// Where it comes from.
    pub source: VariableSource,
}

/// The variables of an instance, in a stable order: ports, the compose
/// project, the compose files, the devslot ids, then `[env]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceEnvironment {
    /// The variables.
    pub variables: Vec<EnvironmentVariable>,
}

impl InstanceEnvironment {
    /// The `export NAME='value'` lines for a shell. Refuses a name that is
    /// not a plain variable name instead of printing it.
    pub fn to_shell(&self) -> Result<String, EnvironmentError> {
        let mut output = String::new();
        for variable in &self.variables {
            check_exportable(&variable.name)?;
            let _ = writeln!(
                output,
                "export {}={}",
                variable.name,
                shell_quote(&variable.value)
            );
        }
        Ok(output)
    }

    /// The value of `name`.
    pub fn value_of(&self, name: &str) -> Option<&str> {
        self.variables
            .iter()
            .find(|variable| variable.name == name)
            .map(|variable| variable.value.as_str())
    }
}

impl Serialize for InstanceEnvironment {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.variables.len()))?;
        for variable in &self.variables {
            map.serialize_entry(&variable.name, &variable.value)?;
        }
        map.end()
    }
}

fn check_exportable(name: &str) -> Result<(), EnvironmentError> {
    if is_valid_variable_name(name) {
        return Ok(());
    }
    Err(EnvironmentError::InvalidName {
        name: name.to_owned(),
    })
}

/// The dev compose and the overrides merged into it, as absolute paths.
/// `None` when the checkout has no dev compose.
pub fn compose_file_paths(root: &Path, selection: &DevComposeSelection) -> Option<Vec<PathBuf>> {
    let DevComposeSelection::Selected(file) = selection else {
        return None;
    };
    Some(
        std::iter::once(file.clone())
            .chain(existing_overrides(root))
            .map(|name| root.join(name))
            .collect(),
    )
}

/// Builds the environment of `instance`.
pub fn build_environment(
    instance: &InstanceRecord,
    compose_files: Option<&[PathBuf]>,
    settings: &[EnvSetting],
) -> Result<InstanceEnvironment, EnvironmentError> {
    let mut variables = instance_variables(instance, compose_files);
    force_ports(&mut variables, instance, settings)?;
    for setting in settings
        .iter()
        .filter(|setting| instance.port_of(&setting.key).is_none())
    {
        let value = substitute(&setting.value, &variables, &setting.key)?;
        variables.push(EnvironmentVariable {
            name: setting.key.clone(),
            value,
            source: VariableSource::Config,
        });
    }
    for variable in &variables {
        check_exportable(&variable.name)?;
    }
    Ok(InstanceEnvironment { variables })
}

fn instance_variables(
    instance: &InstanceRecord,
    compose_files: Option<&[PathBuf]>,
) -> Vec<EnvironmentVariable> {
    let owned = |name: &str, value: String| EnvironmentVariable {
        name: name.to_owned(),
        value,
        source: VariableSource::Instance,
    };
    let mut variables: Vec<EnvironmentVariable> = instance
        .ports
        .iter()
        .map(|assigned| EnvironmentVariable {
            source: VariableSource::Port,
            ..owned(&assigned.variable, assigned.port.to_string())
        })
        .collect();
    variables.push(owned(PROJECT_NAME_VARIABLE, instance.name.clone()));
    if let Some(files) = compose_files {
        let joined: Vec<String> = files
            .iter()
            .map(|file| file.display().to_string())
            .collect();
        variables.push(owned(
            COMPOSE_FILE_VARIABLE,
            joined.join(COMPOSE_FILE_SEPARATOR),
        ));
    }
    variables.push(owned(INSTANCE_VARIABLE, instance.name.clone()));
    variables.push(owned(SLOT_VARIABLE, instance.slot.to_string()));
    variables.push(owned(HOST_VARIABLE, instance.host.clone()));
    variables
}

fn force_ports(
    variables: &mut [EnvironmentVariable],
    instance: &InstanceRecord,
    settings: &[EnvSetting],
) -> Result<(), EnvironmentError> {
    for setting in settings
        .iter()
        .filter(|setting| instance.port_of(&setting.key).is_some())
    {
        if setting.file == CONFIG_FILE {
            return Err(EnvironmentError::CommittedPortOverride {
                key: setting.key.clone(),
            });
        }
        if let Some(variable) = variables
            .iter_mut()
            .find(|variable| variable.name == setting.key)
        {
            variable.value.clone_from(&setting.value);
        }
    }
    Ok(())
}

fn substitute(
    text: &str,
    variables: &[EnvironmentVariable],
    key: &str,
) -> Result<String, EnvironmentError> {
    let resolve = |name: &str| {
        variables
            .iter()
            .find(|variable| variable.name == name)
            .map(|variable| variable.value.clone())
    };
    substitute_references(text, &resolve).map_err(|name| EnvironmentError::UnknownReference {
        key: key.to_owned(),
        name,
    })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::discovery::PortKind;
    use crate::port_range::PortRange;
    use crate::project_config::LOCAL_CONFIG_FILE;
    use crate::registry::record::{AssignedPort, PortDetail};

    fn instance() -> InstanceRecord {
        let port = |variable: &str, port: u16| AssignedPort {
            variable: variable.to_owned(),
            port,
            detail: PortDetail {
                kind: PortKind::Web,
                service: None,
                target: None,
            },
        };
        InstanceRecord {
            path: PathBuf::from("/work/app"),
            name: "app-3f2a".to_owned(),
            slot: 1,
            range: PortRange {
                first: 20050,
                last: 20099,
            },
            ports: vec![port("PORT", 20050), port("DB_PORT", 20052)],
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

    fn pairs(environment: &InstanceEnvironment) -> Vec<(&str, &str)> {
        environment
            .variables
            .iter()
            .map(|variable| (variable.name.as_str(), variable.value.as_str()))
            .collect()
    }

    fn dev_compose() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/work/app/dev.compose.yml"),
            PathBuf::from("/work/app/compose.override.yml"),
        ]
    }

    #[test]
    fn should_list_ports_then_compose_then_devslot_variables_in_order() {
        let environment = build_environment(&instance(), Some(&dev_compose()), &[]).unwrap();

        assert_eq!(
            pairs(&environment),
            vec![
                ("PORT", "20050"),
                ("DB_PORT", "20052"),
                ("COMPOSE_PROJECT_NAME", "app-3f2a"),
                (
                    "COMPOSE_FILE",
                    "/work/app/dev.compose.yml:/work/app/compose.override.yml"
                ),
                ("DEVSLOT_INSTANCE", "app-3f2a"),
                ("DEVSLOT_SLOT", "1"),
                ("DEVSLOT_HOST", "app.localhost"),
            ]
        );
    }

    #[test]
    fn should_leave_out_compose_file_when_there_is_no_dev_compose() {
        let environment = build_environment(&instance(), None, &[]).unwrap();

        assert_eq!(environment.value_of(COMPOSE_FILE_VARIABLE), None);
    }

    #[test]
    fn should_replace_a_reference_with_the_value_of_an_instance_variable() {
        let settings = [setting(
            "VITE_API_URL",
            "http://localhost:${PORT}/api",
            CONFIG_FILE,
        )];

        let environment = build_environment(&instance(), None, &settings).unwrap();

        assert_eq!(
            environment.value_of("VITE_API_URL"),
            Some("http://localhost:20050/api")
        );
    }

    #[test]
    fn should_replace_a_reference_to_the_host_with_the_host_of_the_instance() {
        let settings = [setting(
            "APP_URL",
            "http://${DEVSLOT_HOST}:${PORT}",
            CONFIG_FILE,
        )];

        let environment = build_environment(&instance(), None, &settings).unwrap();

        assert_eq!(
            environment.value_of("APP_URL"),
            Some("http://app.localhost:20050")
        );
    }

    #[test]
    fn should_leave_other_dollar_signs_as_they_are() {
        let settings = [setting("X", "$HOME ${A:-b} $$ ${", CONFIG_FILE)];

        let environment = build_environment(&instance(), None, &settings).unwrap();

        assert_eq!(environment.value_of("X"), Some("$HOME ${A:-b} $$ ${"));
    }

    #[test]
    fn should_fail_naming_the_key_when_a_reference_is_unknown() {
        let settings = [setting("X", "${NOPE}", CONFIG_FILE)];

        let error = build_environment(&instance(), None, &settings).unwrap_err();

        assert!(matches!(
            error,
            EnvironmentError::UnknownReference { ref key, ref name } if key == "X" && name == "NOPE"
        ));
    }

    #[test]
    fn should_not_print_the_value_when_a_reference_is_unknown() {
        let settings = [setting("X", "FAKE-SECRET ${NOPE}", CONFIG_FILE)];

        let error = build_environment(&instance(), None, &settings).unwrap_err();

        assert!(!error.to_string().contains("FAKE-SECRET"));
    }

    #[test]
    fn should_force_a_managed_port_when_the_local_file_sets_it() {
        let settings = [
            setting("PORT", "3333", LOCAL_CONFIG_FILE),
            setting("CALLBACK", "http://localhost:${PORT}", CONFIG_FILE),
        ];

        let environment = build_environment(&instance(), None, &settings).unwrap();

        assert_eq!(
            (
                environment.value_of("PORT"),
                environment.value_of("CALLBACK")
            ),
            (Some("3333"), Some("http://localhost:3333"))
        );
    }

    #[test]
    fn should_keep_the_forced_port_in_its_place_in_the_order() {
        let settings = [setting("PORT", "3333", LOCAL_CONFIG_FILE)];

        let environment = build_environment(&instance(), None, &settings).unwrap();

        assert_eq!(pairs(&environment)[0], ("PORT", "3333"));
    }

    #[test]
    fn should_refuse_a_managed_port_set_by_the_committed_file() {
        let settings = [setting("PORT", "3333", CONFIG_FILE)];

        let error = build_environment(&instance(), None, &settings).unwrap_err();

        assert!(matches!(
            error,
            EnvironmentError::CommittedPortOverride { .. }
        ));
    }

    #[test]
    fn should_print_export_lines_with_single_quoted_values_when_rendering_for_a_shell() {
        let settings = [setting("GREETING", "it's $(id) `x`", CONFIG_FILE)];

        let shell = build_environment(&instance(), None, &settings)
            .unwrap()
            .to_shell()
            .unwrap();

        assert!(shell.contains("export PORT='20050'\n"));
        assert!(shell.contains("export GREETING='it'\\''s $(id) `x`'\n"));
    }

    #[test]
    fn should_refuse_to_print_a_variable_name_that_carries_shell_code() {
        let environment = InstanceEnvironment {
            variables: vec![EnvironmentVariable {
                name: "X=1; echo PWNED >&2; Y".to_owned(),
                value: "1".to_owned(),
                source: VariableSource::Instance,
            }],
        };

        assert!(matches!(
            environment.to_shell(),
            Err(EnvironmentError::InvalidName { .. })
        ));
    }

    #[test]
    fn should_refuse_to_build_the_environment_when_a_registered_variable_name_is_invalid() {
        let mut bad = instance();
        bad.ports[0].variable = "A;id".to_owned();

        assert!(matches!(
            build_environment(&bad, None, &[]),
            Err(EnvironmentError::InvalidName { .. })
        ));
    }

    #[test]
    fn should_print_a_json_object_in_variable_order() {
        let json =
            serde_json::to_string(&build_environment(&instance(), None, &[]).unwrap()).unwrap();

        assert_eq!(
            json,
            r#"{"PORT":"20050","DB_PORT":"20052","COMPOSE_PROJECT_NAME":"app-3f2a","DEVSLOT_INSTANCE":"app-3f2a","DEVSLOT_SLOT":"1","DEVSLOT_HOST":"app.localhost"}"#
        );
    }

    #[test]
    fn should_list_the_dev_compose_then_the_existing_overrides_as_absolute_paths() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("compose.override.yml"), "").unwrap();
        let selection = DevComposeSelection::Selected("dev.compose.yml".to_owned());

        let files = compose_file_paths(root.path(), &selection).unwrap();

        assert_eq!(
            files,
            vec![
                root.path().join("dev.compose.yml"),
                root.path().join("compose.override.yml")
            ]
        );
    }

    #[test]
    fn should_list_no_compose_file_when_none_is_selected() {
        assert_eq!(
            compose_file_paths(Path::new("/x"), &DevComposeSelection::NoneFound),
            None
        );
    }
}
