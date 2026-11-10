//! What the commands that know the registry share: where the files live, the
//! current checkout, and the environment of a registered instance.

use crate::checkout::Checkout;
use crate::compose::selection::select_dev_compose;
use crate::environment::{InstanceEnvironment, build_environment, compose_file_paths};
use crate::error::ToolError;
use crate::paths::DevslotPaths;
use crate::project_config::{EnvSetting, LOCAL_CONFIG_FILE, ProjectConfig};
use crate::registry::record::InstanceRecord;
use crate::registry::store::RegistryStore;

/// The devslot files of this machine.
#[derive(Debug, Clone)]
pub struct Machine {
    /// The locations of the config and the registry.
    pub paths: DevslotPaths,
    /// The registry.
    pub store: RegistryStore,
}

impl Machine {
    /// The locations for this process.
    pub fn from_process() -> Result<Self, ToolError> {
        let paths = DevslotPaths::from_process()?;
        let store = RegistryStore::new(paths.clone());
        Ok(Self { paths, store })
    }
}

/// The checkout that holds the current folder.
pub fn locate_checkout() -> Result<Checkout, ToolError> {
    let directory = std::env::current_dir().map_err(ToolError::CurrentDirectory)?;
    Ok(Checkout::locate(&directory)?)
}

/// The environment of `instance` with every `[env]` value, for the processes
/// `ds run` starts.
pub fn load_environment(
    checkout: &Checkout,
    instance: &InstanceRecord,
) -> Result<InstanceEnvironment, ToolError> {
    build_for(checkout, instance, |_| true)
}

/// The environment `ds env` prints: the `[env]` values of the committed
/// `devslot.toml` are left out, because the output is meant for `eval` and
/// that file may come from a repository the user has not reviewed.
pub fn load_printable_environment(
    checkout: &Checkout,
    instance: &InstanceRecord,
) -> Result<InstanceEnvironment, ToolError> {
    build_for(checkout, instance, |setting| {
        setting.file == LOCAL_CONFIG_FILE
    })
}

fn build_for(
    checkout: &Checkout,
    instance: &InstanceRecord,
    keeps: impl Fn(&EnvSetting) -> bool,
) -> Result<InstanceEnvironment, ToolError> {
    let config = ProjectConfig::load(&checkout.root)?;
    let selection = select_dev_compose(&checkout.root, config.compose_file.as_deref());
    let compose_files = compose_file_paths(&checkout.root, &selection);
    let settings: Vec<EnvSetting> = config.env.into_iter().filter(keeps).collect();
    Ok(build_environment(
        instance,
        compose_files.as_deref(),
        &settings,
    )?)
}
