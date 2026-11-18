//! The project configuration: `devslot.toml` overridden by `devslot.local.toml`.
//!
//! Only the keys devslot reads are modeled (`[compose]`, `[ports]`, `[env]`,
//! `run`, `[write]` and `[web]`); other keys are accepted and ignored.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::project::bounded_read::{BoundedRead, Unreadable, read_bounded};
use crate::reserved_names::is_reserved;

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
    /// A `[ports]` name cannot be a port variable.
    #[error("invalid [ports] name {name:?} in {file}: {reason}")]
    InvalidPortName {
        /// The file name.
        file: &'static str,
        /// The name at fault.
        name: String,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// An `[env]` value is neither a string nor an integer. The value is never quoted.
    #[error("the value of {key} in {file} must be a string or an integer")]
    EnvValueType {
        /// The file name.
        file: &'static str,
        /// The key at fault.
        key: String,
    },
    /// `run` is empty.
    #[error("run in {file} must not be empty")]
    EmptyRun {
        /// The file name.
        file: &'static str,
    },
    /// `[web] port` is not a variable name. The value is never quoted: it must
    /// never be a host or a URL.
    #[error("[web] port in {file} must be the name of a port variable ([A-Za-z_][A-Za-z0-9_]*)")]
    InvalidWebPort {
        /// The file name.
        file: &'static str,
    },
    /// An `[env]` entry cannot be exported. The value is never quoted.
    #[error("invalid [env] key {key:?} in {file}: {reason}")]
    InvalidEnv {
        /// The file name.
        file: &'static str,
        /// The key at fault.
        key: String,
        /// What is wrong with it.
        reason: &'static str,
    },
}

#[derive(Debug, Default, Deserialize)]
struct RawConfig {
    #[serde(default)]
    compose: RawCompose,
    #[serde(default)]
    ports: RawPorts,
    #[serde(default)]
    env: BTreeMap<String, RawEnvValue>,
    run: Option<String>,
    #[serde(default)]
    write: RawWrite,
    #[serde(default)]
    web: RawWeb,
}

#[derive(Debug, Default, Deserialize)]
struct RawWeb {
    port: Option<RawWebPort>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawWebPort {
    Variable(String),
    Unsupported(serde::de::IgnoredAny),
}

#[derive(Debug, Default, Deserialize)]
struct RawWrite {
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawEnvValue {
    Text(String),
    Integer(i64),
    Unsupported(serde::de::IgnoredAny),
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

/// One `[env]` entry and the file whose value won.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvSetting {
    /// The variable name.
    pub key: String,
    /// The value as written; may hold a secret, never printed.
    pub value: String,
    /// The file that set it: the local file wins over the committed one.
    pub file: &'static str,
}

/// The merged configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectConfig {
    /// `[compose] file`: the dev compose file to use instead of detecting one.
    pub compose_file: Option<String>,
    /// The file that set `[compose] file`.
    pub compose_file_origin: Option<&'static str>,
    /// `[ports] extra`: variables to manage in addition to `PORT`.
    pub extra_ports: VariableList,
    /// `[ports] ignore`: compose variables to leave as they are.
    pub ignored_ports: VariableList,
    /// `[env]`: the values built from the assigned ports, local over committed per key.
    pub env: Vec<EnvSetting>,
    /// `run`: the command `ds run` executes when it is given none.
    pub run: Option<String>,
    /// The file that set `run`.
    pub run_origin: Option<&'static str>,
    /// `[write] enabled`: whether ds may write `.env`; the local file wins.
    pub write_enabled: Option<bool>,
    /// `[web] port`: the variable of the web port opened on the host of the
    /// instance; the local file wins. Always a variable name, never a host.
    pub web_port: Option<String>,
}

impl ProjectConfig {
    /// Reads the two files in `root`; a missing file counts as empty.
    pub fn load(root: &Path) -> Result<Self, ConfigError> {
        let base = read_layer(root, CONFIG_FILE)?;
        let local = read_layer(root, LOCAL_CONFIG_FILE)?;
        Ok(Self::merge(base, local))
    }

    /// The keys of `[env]`, in merge order.
    pub fn env_keys(&self) -> Vec<String> {
        self.env.iter().map(|setting| setting.key.clone()).collect()
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
                config.compose_file_origin = Some(file);
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
            if layer.run.is_some() {
                config.run = layer.run;
                config.run_origin = Some(file);
            }
            if layer.write.enabled.is_some() {
                config.write_enabled = layer.write.enabled;
            }
            if let Some(RawWebPort::Variable(name)) = layer.web.port {
                config.web_port = Some(name);
            }
            for (key, value) in layer.env {
                let RawEnvValue::Text(value) = value else {
                    continue;
                };
                config.set_env(EnvSetting { key, value, file });
            }
        }
        config
    }

    fn set_env(&mut self, setting: EnvSetting) {
        match self.env.iter_mut().find(|known| known.key == setting.key) {
            Some(known) => *known = setting,
            None => self.env.push(setting),
        }
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
    let mut raw: RawConfig = toml_edit::de::from_str(text).map_err(|error| ConfigError::Parse {
        file,
        message: error.message().to_owned(),
    })?;
    check_port_names(&raw.ports, file)?;
    check_run(raw.run.as_deref(), file)?;
    check_committed_extra_names(&raw.ports, file)?;
    check_web_port(raw.web.port.as_ref(), file)?;
    for (key, value) in &mut raw.env {
        let text = match value {
            RawEnvValue::Text(text) => text.clone(),
            RawEnvValue::Integer(number) => number.to_string(),
            RawEnvValue::Unsupported(_) => {
                return Err(ConfigError::EnvValueType {
                    file,
                    key: key.clone(),
                });
            }
        };
        check_env_entry(key, &text).map_err(|reason| ConfigError::InvalidEnv {
            file,
            key: key.clone(),
            reason,
        })?;
        *value = RawEnvValue::Text(text);
    }
    Ok(raw)
}

fn check_web_port(port: Option<&RawWebPort>, file: &'static str) -> Result<(), ConfigError> {
    match port {
        None => Ok(()),
        Some(RawWebPort::Variable(name)) if is_valid_variable_name(name) => Ok(()),
        Some(_) => Err(ConfigError::InvalidWebPort { file }),
    }
}

fn check_run(run: Option<&str>, file: &'static str) -> Result<(), ConfigError> {
    match run {
        Some(command) if command.trim().is_empty() => Err(ConfigError::EmptyRun { file }),
        _ => Ok(()),
    }
}

fn check_committed_extra_names(ports: &RawPorts, file: &'static str) -> Result<(), ConfigError> {
    if file != CONFIG_FILE {
        return Ok(());
    }
    let mut names = ports.extra.iter().flatten();
    match names.find(|name| !has_port_suffix(name)) {
        Some(name) => Err(ConfigError::InvalidPortName {
            file,
            name: name.clone(),
            reason: "a port variable of the committed file must end in PORT",
        }),
        None => Ok(()),
    }
}

fn check_port_names(ports: &RawPorts, file: &'static str) -> Result<(), ConfigError> {
    let names = ports.extra.iter().chain(&ports.ignore).flatten();
    for name in names {
        check_variable_name(name).map_err(|reason| ConfigError::InvalidPortName {
            file,
            name: name.clone(),
            reason,
        })?;
    }
    Ok(())
}

const PORT_SUFFIX: &str = "PORT";

/// Whether the name ends in `PORT`, in any case. A managed port variable must:
/// its value is a bare number that tools may read as something else (a config
/// file path, for example) under any other name.
pub fn has_port_suffix(name: &str) -> bool {
    name.len() >= PORT_SUFFIX.len()
        && name.as_bytes()[name.len() - PORT_SUFFIX.len()..]
            .eq_ignore_ascii_case(PORT_SUFFIX.as_bytes())
}

/// Whether `name` matches `[A-Za-z_][A-Za-z0-9_]*`.
pub fn is_valid_variable_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Checks that `name` can be exported and is not a reserved or denied name.
pub fn check_variable_name(name: &str) -> Result<(), &'static str> {
    if !is_valid_variable_name(name) {
        return Err("not a valid variable name ([A-Za-z_][A-Za-z0-9_]*)");
    }
    if is_reserved(name) {
        return Err("the name is reserved (it steers the process, Docker, devslot or a shell)");
    }
    Ok(())
}

fn check_env_entry(key: &str, value: &str) -> Result<(), &'static str> {
    check_variable_name(key)?;
    if value.contains('\0') {
        return Err("the value contains a NUL character");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn write(root: &Path, file: &str, text: &str) {
        std::fs::write(root.join(file), text).unwrap();
    }

    #[test]
    fn should_read_the_run_command_and_its_file() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "run = \"pnpm dev\"\n");

        let config = ProjectConfig::load(root.path()).unwrap();

        assert_eq!(
            (config.run.as_deref(), config.run_origin),
            (Some("pnpm dev"), Some(CONFIG_FILE))
        );
    }

    #[test]
    fn should_let_the_local_run_command_win_over_the_committed_one() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "run = \"pnpm dev\"\n");
        write(root.path(), LOCAL_CONFIG_FILE, "run = \"pnpm start\"\n");

        let config = ProjectConfig::load(root.path()).unwrap();

        assert_eq!(
            (config.run.as_deref(), config.run_origin),
            (Some("pnpm start"), Some(LOCAL_CONFIG_FILE))
        );
    }

    #[test]
    fn should_refuse_an_empty_run_command_naming_the_file() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "run = \"  \"\n");

        assert!(matches!(
            ProjectConfig::load(root.path()),
            Err(ConfigError::EmptyRun { file: CONFIG_FILE })
        ));
    }

    #[test]
    fn should_refuse_a_run_command_that_is_not_a_string() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "run = 3\n");

        assert!(matches!(
            ProjectConfig::load(root.path()),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn should_leave_write_unset_when_no_file_sets_it() {
        let root = tempfile::tempdir().unwrap();

        assert_eq!(
            ProjectConfig::load(root.path()).unwrap().write_enabled,
            None
        );
    }

    #[test]
    fn should_let_the_local_write_setting_win_over_the_committed_one() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "[write]\nenabled = false\n");
        write(root.path(), LOCAL_CONFIG_FILE, "[write]\nenabled = true\n");

        assert_eq!(
            ProjectConfig::load(root.path()).unwrap().write_enabled,
            Some(true)
        );
    }

    #[test]
    fn should_keep_the_committed_write_setting_when_the_local_file_does_not_set_it() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "[write]\nenabled = false\n");
        write(root.path(), LOCAL_CONFIG_FILE, "run = \"x\"\n");

        assert_eq!(
            ProjectConfig::load(root.path()).unwrap().write_enabled,
            Some(false)
        );
    }

    #[test]
    fn should_read_the_web_port_variable() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "[web]\nport = \"PORT\"\n");

        assert_eq!(
            ProjectConfig::load(root.path())
                .unwrap()
                .web_port
                .as_deref(),
            Some("PORT")
        );
    }

    #[test]
    fn should_leave_the_web_port_unset_when_no_file_sets_it() {
        let root = tempfile::tempdir().unwrap();

        assert_eq!(ProjectConfig::load(root.path()).unwrap().web_port, None);
    }

    #[test]
    fn should_let_the_local_web_port_win_over_the_committed_one() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "[web]\nport = \"PORT\"\n");
        write(
            root.path(),
            LOCAL_CONFIG_FILE,
            "[web]\nport = \"WEB_PORT\"\n",
        );

        assert_eq!(
            ProjectConfig::load(root.path())
                .unwrap()
                .web_port
                .as_deref(),
            Some("WEB_PORT")
        );
    }

    #[test]
    fn should_refuse_a_web_port_that_is_a_host_or_a_url_naming_the_key_but_not_the_value() {
        let values = [
            "evil.example.com",
            "http://evil.example.com",
            "app.localhost",
            "PORT:80",
            "1PORT",
            "",
            "PORT ",
        ];
        for value in values {
            let root = tempfile::tempdir().unwrap();
            write(
                root.path(),
                CONFIG_FILE,
                &format!("[web]\nport = \"{value}\"\n"),
            );

            let error = ProjectConfig::load(root.path()).unwrap_err();

            assert!(matches!(
                error,
                ConfigError::InvalidWebPort { file: CONFIG_FILE }
            ));
            assert!(error.to_string().contains("[web] port"));
            assert!(value.is_empty() || !error.to_string().contains(value));
        }
    }

    #[test]
    fn should_refuse_a_web_port_that_is_not_a_string() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), LOCAL_CONFIG_FILE, "[web]\nport = 3000\n");

        assert!(matches!(
            ProjectConfig::load(root.path()),
            Err(ConfigError::InvalidWebPort {
                file: LOCAL_CONFIG_FILE
            })
        ));
    }

    #[test]
    fn should_refuse_a_write_setting_that_is_not_a_boolean() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "[write]\nenabled = \"no\"\n");

        assert!(matches!(
            ProjectConfig::load(root.path()),
            Err(ConfigError::Parse { .. })
        ));
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
        write(root.path(), CONFIG_FILE, "[ports]\nextra = [\"A_PORT\"]\n");
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
        write(root.path(), CONFIG_FILE, "[ports]\nextra = [\"A_PORT\"]\n");
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

        assert_eq!(config.env_keys(), vec!["A", "B", "C"]);
    }

    #[test]
    fn should_keep_the_value_and_the_file_that_won_when_the_local_file_overrides_a_key() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), CONFIG_FILE, "[env]\nA = \"x\"\nB = \"y\"\n");
        write(root.path(), LOCAL_CONFIG_FILE, "[env]\nB = \"z\"\n");

        let config = ProjectConfig::load(root.path()).unwrap();

        assert_eq!(
            config.env,
            vec![
                EnvSetting {
                    key: "A".to_owned(),
                    value: "x".to_owned(),
                    file: CONFIG_FILE
                },
                EnvSetting {
                    key: "B".to_owned(),
                    value: "z".to_owned(),
                    file: LOCAL_CONFIG_FILE
                }
            ]
        );
    }

    #[test]
    fn should_name_the_file_that_set_the_compose_file() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            LOCAL_CONFIG_FILE,
            "[compose]\nfile = \"b.yml\"\n",
        );

        let config = ProjectConfig::load(root.path()).unwrap();

        assert_eq!(config.compose_file_origin, Some(LOCAL_CONFIG_FILE));
    }

    fn invalid_env_reason(file: &str, text: &str) -> String {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), file, text);
        match ProjectConfig::load(root.path()).unwrap_err() {
            ConfigError::InvalidEnv { key, .. } => key,
            other => panic!("unexpected error {other}"),
        }
    }

    #[test]
    fn should_refuse_an_env_key_that_is_not_a_variable_name_naming_the_key() {
        assert_eq!(
            invalid_env_reason(CONFIG_FILE, "[env]\n\"A-B\" = \"x\"\n"),
            "A-B"
        );
    }

    #[test]
    fn should_refuse_an_env_key_that_starts_with_a_digit() {
        assert_eq!(
            invalid_env_reason(CONFIG_FILE, "[env]\n\"1A\" = \"x\"\n"),
            "1A"
        );
    }

    #[test]
    fn should_refuse_a_reserved_env_key_in_the_committed_file() {
        assert_eq!(
            invalid_env_reason(CONFIG_FILE, "[env]\nCOMPOSE_PROJECT_NAME = \"x\"\n"),
            "COMPOSE_PROJECT_NAME"
        );
    }

    #[test]
    fn should_refuse_a_reserved_env_key_in_the_local_file() {
        assert_eq!(
            invalid_env_reason(LOCAL_CONFIG_FILE, "[env]\nPATH = \"x\"\n"),
            "PATH"
        );
    }

    #[test]
    fn should_refuse_an_env_value_with_a_nul_character_without_printing_it() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            CONFIG_FILE,
            "[env]\nA = \"fake-secret\\u0000\"\n",
        );

        let error = ProjectConfig::load(root.path()).unwrap_err();

        assert!(matches!(error, ConfigError::InvalidEnv { .. }));
        assert!(!error.to_string().contains("fake-secret"));
    }

    #[test]
    fn should_ignore_unknown_keys_when_the_file_has_some() {
        let config = ProjectConfig::from_text("[mobile]\nname = \"app\"\n").unwrap();

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

    #[test]
    fn should_refuse_an_extra_port_name_that_carries_shell_code() {
        let error = ProjectConfig::from_text("[ports]\nextra = [\"A;id\"]\n").unwrap_err();

        assert!(matches!(error, ConfigError::InvalidPortName { ref name, .. } if name == "A;id"));
    }

    #[test]
    fn should_refuse_a_reserved_extra_port_name() {
        assert!(matches!(
            ProjectConfig::from_text("[ports]\nextra = [\"PATH\"]\n").unwrap_err(),
            ConfigError::InvalidPortName { .. }
        ));
    }

    #[test]
    fn should_refuse_an_invalid_ignored_port_name_in_the_local_file() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            LOCAL_CONFIG_FILE,
            "[ports]\nignore = [\"A B\"]\n",
        );

        let error = ProjectConfig::load(root.path()).unwrap_err();

        assert!(matches!(
            error,
            ConfigError::InvalidPortName {
                file: LOCAL_CONFIG_FILE,
                ..
            }
        ));
    }

    #[test]
    fn should_refuse_a_denied_env_key_in_both_files() {
        for file in [CONFIG_FILE, LOCAL_CONFIG_FILE] {
            assert_eq!(
                invalid_env_reason(file, "[env]\nGIT_CONFIG_COUNT = \"1\"\n"),
                "GIT_CONFIG_COUNT"
            );
        }
    }

    #[test]
    fn should_accept_an_integer_env_value_as_its_decimal_text() {
        let config = ProjectConfig::from_text("[env]\nPORT_FALLBACK = 3333\n").unwrap();

        assert_eq!(config.env[0].value, "3333");
    }

    #[test]
    fn should_refuse_a_float_env_value_without_quoting_it() {
        let error = ProjectConfig::from_text("[env]\nPIN = 1.5\n").unwrap_err();

        assert!(matches!(error, ConfigError::EnvValueType { .. }));
        assert_eq!(
            error.to_string(),
            "the value of PIN in devslot.toml must be a string or an integer"
        );
    }

    #[test]
    fn should_not_quote_a_table_env_value_in_the_error() {
        let error = ProjectConfig::from_text("[env]\nA = { secret = 98765432 }\n").unwrap_err();

        assert!(!error.to_string().contains("98765432"));
    }

    #[test]
    fn should_refuse_a_committed_extra_port_that_does_not_end_in_port() {
        let error = ProjectConfig::from_text("[ports]\nextra = [\"KUBECONFIG\"]\n").unwrap_err();

        assert!(
            matches!(error, ConfigError::InvalidPortName { ref name, .. } if name == "KUBECONFIG")
        );
    }

    #[test]
    fn should_accept_a_committed_extra_port_that_ends_in_port_in_any_case() {
        assert!(
            ProjectConfig::from_text("[ports]\nextra = [\"front_port\", \"FRONT_PORT\"]\n").is_ok()
        );
    }

    #[test]
    fn should_accept_a_local_extra_port_that_does_not_end_in_port() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            LOCAL_CONFIG_FILE,
            "[ports]\nextra = [\"KUBECONFIG\"]\n",
        );

        assert!(ProjectConfig::load(root.path()).is_ok());
    }

    #[test]
    fn should_still_refuse_a_reserved_local_extra_port() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            LOCAL_CONFIG_FILE,
            "[ports]\nextra = [\"GIT_PORT\"]\n",
        );

        assert!(ProjectConfig::load(root.path()).is_err());
    }

    #[test]
    fn should_detect_the_port_suffix_in_any_case() {
        assert!(has_port_suffix("PORT") && has_port_suffix("db_port"));
        assert!(!has_port_suffix("PSQLRC") && !has_port_suffix("PO"));
    }
}
