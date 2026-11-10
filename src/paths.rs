//! Where devslot keeps its global configuration and its registry.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use thiserror::Error;

const APPLICATION_DIRECTORY: &str = "devslot";
const CONFIG_FILE_NAME: &str = "config.toml";
const REGISTRY_FILE_NAME: &str = "registry.toml";
const REGISTRY_TEMP_FILE_NAME: &str = "registry.toml.tmp";
const LOCK_FILE_NAME: &str = "registry.lock";
const DEFAULT_CONFIG_SUBPATH: &str = ".config";
const DEFAULT_STATE_SUBPATH: &str = ".local/state";
const CONFIG_HOME_VARIABLE: &str = "XDG_CONFIG_HOME";
const STATE_HOME_VARIABLE: &str = "XDG_STATE_HOME";
const HOME_VARIABLE: &str = "HOME";

/// Why the locations cannot be decided.
#[derive(Debug, Error)]
pub enum PathsError {
    /// Neither an XDG variable nor an absolute `HOME` gives a base folder.
    #[error("cannot find where to keep devslot files: set HOME or {variable} to an absolute path")]
    NoBaseDirectory {
        /// The XDG variable that was not usable.
        variable: &'static str,
    },
}

/// The files devslot keeps outside the projects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevslotPaths {
    /// The global configuration, which may not exist.
    pub config_file: PathBuf,
    /// The registry of instances.
    pub registry_file: PathBuf,
    /// The temporary file the registry is written to before the rename.
    pub registry_temp_file: PathBuf,
    /// The lock file, never deleted or renamed.
    pub lock_file: PathBuf,
}

/// The variables the locations are decided from.
#[derive(Debug, Clone, Default)]
pub struct PathVariables {
    /// `XDG_CONFIG_HOME`.
    pub config_home: Option<OsString>,
    /// `XDG_STATE_HOME`.
    pub state_home: Option<OsString>,
    /// `HOME`.
    pub home: Option<OsString>,
}

impl PathVariables {
    /// Reads the variables of this process.
    pub fn from_process() -> Self {
        Self {
            config_home: std::env::var_os(CONFIG_HOME_VARIABLE),
            state_home: std::env::var_os(STATE_HOME_VARIABLE),
            home: std::env::var_os(HOME_VARIABLE),
        }
    }
}

impl DevslotPaths {
    /// The locations for this process.
    pub fn from_process() -> Result<Self, PathsError> {
        Self::from_variables(&PathVariables::from_process())
    }

    /// The locations an XDG variable gives when it is an absolute path,
    /// otherwise the default under `HOME`.
    pub fn from_variables(variables: &PathVariables) -> Result<Self, PathsError> {
        let config_directory = base_directory(
            variables.config_home.as_ref(),
            variables.home.as_ref(),
            DEFAULT_CONFIG_SUBPATH,
            CONFIG_HOME_VARIABLE,
        )?
        .join(APPLICATION_DIRECTORY);
        let state_directory = base_directory(
            variables.state_home.as_ref(),
            variables.home.as_ref(),
            DEFAULT_STATE_SUBPATH,
            STATE_HOME_VARIABLE,
        )?
        .join(APPLICATION_DIRECTORY);
        Ok(Self {
            config_file: config_directory.join(CONFIG_FILE_NAME),
            registry_file: state_directory.join(REGISTRY_FILE_NAME),
            registry_temp_file: state_directory.join(REGISTRY_TEMP_FILE_NAME),
            lock_file: state_directory.join(LOCK_FILE_NAME),
        })
    }

    /// The folder that holds the registry, its lock and its temporary file.
    pub fn state_directory(&self) -> &Path {
        self.registry_file.parent().unwrap_or(Path::new("/"))
    }
}

fn absolute(value: Option<&OsString>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|path| path.is_absolute())
}

fn base_directory(
    xdg_value: Option<&OsString>,
    home: Option<&OsString>,
    default_subpath: &str,
    variable: &'static str,
) -> Result<PathBuf, PathsError> {
    if let Some(directory) = absolute(xdg_value) {
        return Ok(directory);
    }
    absolute(home)
        .map(|directory| directory.join(default_subpath))
        .ok_or(PathsError::NoBaseDirectory { variable })
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn variables(config: Option<&str>, state: Option<&str>, home: Option<&str>) -> PathVariables {
        PathVariables {
            config_home: config.map(OsString::from),
            state_home: state.map(OsString::from),
            home: home.map(OsString::from),
        }
    }

    #[test]
    fn should_use_the_xdg_variables_when_they_are_absolute() {
        let paths =
            DevslotPaths::from_variables(&variables(Some("/c"), Some("/s"), Some("/h"))).unwrap();

        assert_eq!(
            (paths.config_file, paths.registry_file, paths.lock_file),
            (
                PathBuf::from("/c/devslot/config.toml"),
                PathBuf::from("/s/devslot/registry.toml"),
                PathBuf::from("/s/devslot/registry.lock")
            )
        );
    }

    #[test]
    fn should_fall_back_to_home_when_the_xdg_variables_are_unset() {
        let paths = DevslotPaths::from_variables(&variables(None, None, Some("/h"))).unwrap();

        assert_eq!(
            (paths.config_file, paths.registry_file),
            (
                PathBuf::from("/h/.config/devslot/config.toml"),
                PathBuf::from("/h/.local/state/devslot/registry.toml")
            )
        );
    }

    #[test]
    fn should_ignore_a_relative_xdg_variable_when_choosing_the_folder() {
        let paths =
            DevslotPaths::from_variables(&variables(Some("rel"), Some(""), Some("/h"))).unwrap();

        assert_eq!(
            paths.registry_file,
            PathBuf::from("/h/.local/state/devslot/registry.toml")
        );
    }

    #[test]
    fn should_fail_when_no_absolute_base_folder_exists() {
        let error =
            DevslotPaths::from_variables(&variables(None, None, Some("relative"))).unwrap_err();

        assert!(matches!(error, PathsError::NoBaseDirectory { .. }));
    }

    #[test]
    fn should_keep_the_temporary_file_next_to_the_registry() {
        let paths = DevslotPaths::from_variables(&variables(Some("/c"), Some("/s"), None)).unwrap();

        assert_eq!(
            paths.registry_temp_file,
            PathBuf::from("/s/devslot/registry.toml.tmp")
        );
    }
}
