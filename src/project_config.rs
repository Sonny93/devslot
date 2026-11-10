//! The project configuration: `devslot.toml` overridden by `devslot.local.toml`.
//!
//! Only the keys phase 1 reads are modeled; other keys are accepted and ignored.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::project::bounded_read::{BoundedRead, Unreadable, read_bounded};

/// The committed configuration file.
pub const CONFIG_FILE: &str = "devslot.toml";
/// The machine-local override, ignored by git.
pub const LOCAL_CONFIG_FILE: &str = "devslot.local.toml";

/// Why the configuration cannot be read.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// The file exists but cannot be read.
    #[error("cannot read {file}: {source}")]
    Read {
        /// The file name.
        file: &'static str,
        /// The underlying error.
        source: io::Error,
    },
    /// The file is not a regular file or is larger than 1 MiB.
    #[error("cannot read {file}: {reason}")]
    Unusable {
        /// The file name.
        file: &'static str,
        /// "not a regular file" or "larger than 1 MiB".
        reason: &'static str,
    },
    /// The file is not valid TOML or has a value of the wrong type.
    #[error("cannot parse {file}: {message}")]
    Parse {
        /// The file name.
        file: &'static str,
        /// What the parser reports.
        message: String,
    },
}

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    compose: RawCompose,
    #[serde(default)]
    ports: RawPorts,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawCompose {
    file: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawPorts {
    extra: Option<Vec<String>>,
    ignore: Option<Vec<String>>,
}

/// A list of variable names and the file that defined it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VariableList {
    /// The variable names, in file order.
    pub names: Vec<String>,
    /// The file that gave the list, when there is one.
    pub file: Option<&'static str>,
}

/// The merged configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectConfig {
    /// `[compose] file`: the dev compose file to use instead of detecting one.
    pub compose_file: Option<String>,
    /// `[ports] extra`: variables to manage in addition to `PORT`.
    pub extra_ports: VariableList,
    /// `[ports] ignore`: compose variables to leave as they are.
    pub ignored_ports: VariableList,
    /// `[env]`: the keys rebuilt from the assigned ports.
    pub env_keys: Vec<String>,
}

impl ProjectConfig {
    /// Reads the two files in `root`; a missing file counts as empty.
    pub fn load(root: &Path) -> Result<Self, ConfigError> {
        let base = read_layer(root, CONFIG_FILE)?;
        let local = read_layer(root, LOCAL_CONFIG_FILE)?;
        Ok(Self::merge(base, local))
    }

    /// Parses the text of a `devslot.toml`.
    pub fn from_text(text: &str) -> Result<Self, ConfigError> {
        let raw = parse(text, CONFIG_FILE)?;
        Ok(Self::merge(Some((CONFIG_FILE, raw)), None))
    }

    fn merge(
        base: Option<(&'static str, RawConfig)>,
        local: Option<(&'static str, RawConfig)>,
    ) -> Self {
        let layers: Vec<(&'static str, RawConfig)> = base.into_iter().chain(local).collect();
        let mut config = Self::default();
        for (file, layer) in layers {
            if layer.compose.file.is_some() {
                config.compose_file = layer.compose.file;
            }
            if let Some(names) = layer.ports.extra {
                config.extra_ports = VariableList {
                    names,
                    file: Some(file),
                };
            }
            if let Some(names) = layer.ports.ignore {
                config.ignored_ports = VariableList {
                    names,
                    file: Some(file),
                };
            }
            for key in layer.env.into_keys() {
                if !config.env_keys.contains(&key) {
                    config.env_keys.push(key);
                }
            }
        }
        config
    }
}

fn read_layer(
    root: &Path,
    file: &'static str,
) -> Result<Option<(&'static str, RawConfig)>, ConfigError> {
    let bytes = match read_bounded(&root.join(file)) {
        Ok(BoundedRead::Bytes(bytes)) => bytes,
        Ok(BoundedRead::Missing) => return Ok(None),
        Ok(BoundedRead::Unusable(Unreadable::NotRegularFile)) => {
            return Err(ConfigError::Unusable {
                file,
                reason: "not a regular file",
            });
        }
        Ok(BoundedRead::Unusable(Unreadable::TooLarge)) => {
            return Err(ConfigError::Unusable {
                file,
                reason: "larger than 1 MiB",
            });
        }
        Err(source) => return Err(ConfigError::Read { file, source }),
    };
    let text = String::from_utf8_lossy(&bytes);
    parse(&text, file).map(|raw| Some((file, raw)))
}

fn parse(text: &str, file: &'static str) -> Result<RawConfig, ConfigError> {
    toml_edit::de::from_str(text).map_err(|error| ConfigError::Parse {
        file,
        message: error.message().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn write(root: &Path, file: &str, text: &str) {
        std::fs::write(root.join(file), text).unwrap();
    }

    #[test]
    fn should_return_the_defaults_when_no_file_exists() {
        let root = tempfile::tempdir().unwrap();

        assert_eq!(
            ProjectConfig::load(root.path()).unwrap(),
            ProjectConfig::default()
        );
    }

    #[test]
    fn should_read_the_compose_file_and_port_lists_when_the_file_sets_them() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            CONFIG_FILE,
            "[compose]\nfile = \"dev.compose.yml\"\n[ports]\nextra = [\"FRONT_PORT\"]\nignore = [\"MAILPIT_PORT\"]\n",
        );

        let config = ProjectConfig::load(root.path()).unwrap();

        assert_eq!(
            (
                config.compose_file.as_deref(),
                config.extra_ports.names,
                config.ignored_ports.names
            ),
            (
                Some("dev.compose.yml"),
                vec!["FRONT_PORT".to_owned()],
                vec!["MAILPIT_PORT".to_owned()]
            )
        );
    }

    #[test]
    fn should_let_the_local_file_override_the_compose_file_when_both_set_it() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "[compose]\nfile = \"a.yml\"\n");
        write(
            root.path(),
            LOCAL_CONFIG_FILE,
            "[compose]\nfile = \"b.yml\"\n",
        );

        let config = ProjectConfig::load(root.path()).unwrap();

        assert_eq!(config.compose_file.as_deref(), Some("b.yml"));
    }

    #[test]
    fn should_keep_the_base_list_and_name_its_file_when_the_local_file_does_not_set_it() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "[ports]\nextra = [\"A\"]\n");
        write(
            root.path(),
            LOCAL_CONFIG_FILE,
            "[ports]\nignore = [\"B\"]\n",
        );

        let config = ProjectConfig::load(root.path()).unwrap();

        assert_eq!(
            (config.extra_ports.file, config.ignored_ports.file),
            (Some(CONFIG_FILE), Some(LOCAL_CONFIG_FILE))
        );
    }

    #[test]
    fn should_replace_the_list_when_the_local_file_sets_it() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "[ports]\nextra = [\"A\"]\n");
        write(root.path(), LOCAL_CONFIG_FILE, "[ports]\nextra = [\"B\"]\n");

        let config = ProjectConfig::load(root.path()).unwrap();

        assert_eq!(config.extra_ports.names, vec!["B".to_owned()]);
    }

    #[test]
    fn should_merge_the_env_keys_when_both_files_define_some() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "[env]\nA = \"x\"\nB = \"y\"\n");
        write(
            root.path(),
            LOCAL_CONFIG_FILE,
            "[env]\nB = \"z\"\nC = \"w\"\n",
        );

        let config = ProjectConfig::load(root.path()).unwrap();

        assert_eq!(config.env_keys, vec!["A", "B", "C"]);
    }

    #[test]
    fn should_ignore_unknown_keys_when_the_file_has_the_run_key() {
        let config = ProjectConfig::from_text("run = \"node ace serve\"\n").unwrap();

        assert_eq!(config, ProjectConfig::default());
    }

    #[test]
    fn should_fail_when_the_config_is_not_a_regular_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(CONFIG_FILE)).unwrap();

        let error = ProjectConfig::load(root.path()).unwrap_err();

        assert!(matches!(
            error,
            ConfigError::Unusable {
                file: CONFIG_FILE,
                ..
            }
        ));
    }

    #[test]
    fn should_report_a_parse_error_naming_the_file_when_the_toml_is_invalid() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "[ports\n");

        let error = ProjectConfig::load(root.path()).unwrap_err();

        assert!(matches!(
            error,
            ConfigError::Parse {
                file: CONFIG_FILE,
                ..
            }
        ));
    }
}
