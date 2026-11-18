//! `ds rm`: bring the checkout back to its default configuration. The data of
//! the instance is deleted, the devslot block leaves `.env`, the instance leaves
//! the registry and the checkout is opted out of automatic registration.

use std::path::{Path, PathBuf};

use crate::checkout::Checkout;
use crate::cli::confirm::{ConfirmationPolicy, exit_code_unless_confirmed};
use crate::cli::exit_code;
use crate::commands::docker_step::checked_step;
use crate::commands::machine::{Machine, locate_checkout};
use crate::commands::registering::LOCK_TIMEOUT;
use crate::compose::runner::{ComposeRunner, DockerRunner, delete_project_data};
use crate::ensure_cache::remove_checkout_entries;
use crate::env_sync::EnvSyncError;
use crate::error::ToolError;
use crate::project::dotenv_block::split_block;
use crate::project::env_block_render::remove_block;
use crate::project::env_file::{ENV_FILE, EnvContent, read_env, write_env};
use crate::registry::record::{InstanceRecord, Registry, RegistryChange};
use crate::registry::store::RegistryStore;

const COMMAND_NAME: &str = "ds rm";

/// Removes the instance of the current checkout, or just opts the checkout
/// out when it is not registered.
pub fn run_rm(policy: ConfirmationPolicy) -> Result<u8, ToolError> {
    let checkout = locate_checkout()?;
    let machine = Machine::from_process()?;
    let registry = machine.store.read()?;
    match registry.find(&checkout.root).cloned() {
        Some(instance) => remove_registered(&checkout, &machine, &instance, policy),
        None => remove_unregistered(&checkout, &machine),
    }
}

fn remove_registered(
    checkout: &Checkout,
    machine: &Machine,
    instance: &InstanceRecord,
    policy: ConfirmationPolicy,
) -> Result<u8, ToolError> {
    let prompt = format!(
        "Delete the containers and volumes of {} ({}) and remove the devslot block from .env? [y/N] ",
        instance.name,
        instance.path.display()
    );
    if let Some(code) = exit_code_unless_confirmed(COMMAND_NAME, policy, &prompt) {
        return Ok(code);
    }
    if let Some(problem) = env_problem(&checkout.root) {
        eprintln!("ds: error: {problem}; nothing was changed");
        return Ok(exit_code::ERROR);
    }
    machine
        .store
        .locked(LOCK_TIMEOUT, |_| Ok::<(), ToolError>(()))?;
    if !delete_data(&DockerRunner::from_environment(), &instance.name)? {
        return Ok(exit_code::ERROR);
    }
    Ok(finish_registered(checkout, machine, instance))
}

fn delete_data(runner: &dyn ComposeRunner, name: &str) -> Result<bool, ToolError> {
    eprintln!("ds: deleting the containers, volumes and networks of {name}");
    let message = format!(
        "ds: error: the data of {name} was not fully deleted; nothing else was changed, run ds rm again"
    );
    Ok(checked_step(delete_project_data(runner, name), &message)?.is_some())
}

fn finish_registered(checkout: &Checkout, machine: &Machine, instance: &InstanceRecord) -> u8 {
    let changes = vec![
        RegistryChange::RemoveInstance(instance.path.clone()),
        RegistryChange::OptOut(checkout.root.clone()),
    ];
    match remove_block_then_apply(&machine.store, &checkout.root, |_| changes) {
        Ok(lines) => {
            remove_checkout_entries(machine.paths.state_directory(), &instance.path);
            announce_removal(&lines, &checkout.root);
            exit_code::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "ds: error: the containers and volumes of {} are deleted, but {error}; run ds rm again to finish",
                instance.name
            );
            exit_code::TOOL_FAILURE
        }
    }
}

fn remove_unregistered(checkout: &Checkout, machine: &Machine) -> Result<u8, ToolError> {
    let root = &checkout.root;
    if let Some(problem) = env_problem(root) {
        eprintln!("ds: error: {problem}; nothing was changed");
        return Ok(exit_code::ERROR);
    }
    let lines = remove_block_then_apply(&machine.store, root, |registry| {
        opt_out_changes(registry, root)
    })?;
    announce_removal(&lines, root);
    Ok(exit_code::SUCCESS)
}

fn opt_out_changes(registry: &Registry, root: &Path) -> Vec<RegistryChange> {
    if registry.is_opted_out(root) {
        return Vec::new();
    }
    vec![RegistryChange::OptOut(root.to_path_buf())]
}

fn announce_removal(lines: &[String], root: &Path) {
    for line in lines {
        eprintln!("{line}");
    }
    eprintln!(
        "ds: {} is back to its default configuration and opted out of automatic registration",
        root.display()
    );
}

/// What stops `.env` from being rewritten: it cannot be read, or its block is
/// malformed. `None` when it is missing or fine.
fn env_problem(root: &Path) -> Option<String> {
    match read_env(root) {
        Err(error) => Some(error.to_string()),
        Ok(EnvContent::Missing) => None,
        Ok(EnvContent::Text(text)) => split_block(&text)
            .err()
            .map(|error| format!("{ENV_FILE}: {error}")),
    }
}

/// In one lock hold: removes the devslot block from `.env` when it has one,
/// then applies the registry changes `changes_for` asks for, in one write.
/// Returns the lines announcing what was done to `.env`. Ignores
/// `[write] enabled` and the git ignore check: removing is always allowed.
pub fn remove_block_then_apply(
    store: &RegistryStore,
    root: &Path,
    changes_for: impl FnOnce(&Registry) -> Vec<RegistryChange>,
) -> Result<Vec<String>, ToolError> {
    store.locked(LOCK_TIMEOUT, |locked| {
        let lines = remove_env_block(root)?;
        let changes = changes_for(locked.registry());
        locked.apply(&changes)?;
        Ok(lines)
    })
}

fn remove_env_block(root: &Path) -> Result<Vec<String>, ToolError> {
    let text = match read_env(root).map_err(EnvSyncError::from)? {
        EnvContent::Text(text) => text,
        EnvContent::Missing => return Ok(Vec::new()),
    };
    let without_block = remove_block(&text).map_err(EnvSyncError::from)?;
    if without_block == text {
        return Ok(Vec::new());
    }
    write_env(root, &without_block).map_err(EnvSyncError::from)?;
    let path: PathBuf = root.join(ENV_FILE);
    Ok(vec![format!(
        "ds: removed the devslot block from {}",
        path.display()
    )])
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::discovery::PortKind;
    use crate::paths::{DevslotPaths, PathVariables};
    use crate::port_range::PortRange;
    use crate::project::dotenv_block::{BLOCK_END, BLOCK_START};
    use crate::registry::record::{AssignedPort, PortDetail};

    struct Fixture {
        state: tempfile::TempDir,
        projects: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                state: tempfile::tempdir().unwrap(),
                projects: tempfile::tempdir().unwrap(),
            }
        }

        fn store(&self) -> RegistryStore {
            RegistryStore::new(
                DevslotPaths::from_variables(&PathVariables {
                    config_home: Some(OsString::from(self.state.path().join("config"))),
                    state_home: Some(OsString::from(self.state.path().join("state"))),
                    home: None,
                })
                .unwrap(),
            )
        }

        fn project(&self, name: &str, env: &str) -> PathBuf {
            let root = self.projects.path().join(name);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join(".env"), env).unwrap();
            root
        }
    }

    fn record(path: &Path, name: &str, first: u16) -> InstanceRecord {
        InstanceRecord {
            path: path.to_path_buf(),
            name: name.to_owned(),
            slot: 1,
            range: PortRange {
                first,
                last: first + 49,
            },
            ports: vec![AssignedPort {
                variable: "PORT".to_owned(),
                port: first,
                detail: PortDetail {
                    kind: PortKind::Web,
                    service: None,
                    target: None,
                },
            }],
            host: format!("{name}.localhost"),
            web_port_variable: None,
            is_host_unsaved: false,
        }
    }

    fn block() -> String {
        format!("{BLOCK_START}\nPORT=20050\n{BLOCK_END}\n")
    }

    #[test]
    fn should_remove_the_block_the_instance_and_record_the_opt_out_in_one_step() {
        let machine = Fixture::new();
        let root = machine.project("app", &format!("A=1\n{}B=2\n", block()));
        let other = machine.project("other", "");
        let store = machine.store();
        store
            .locked(LOCK_TIMEOUT, |locked| {
                locked.save_instance(&record(&root, "app-1", 20050))?;
                locked.save_instance(&record(&other, "other-1", 20100))
            })
            .unwrap();

        let lines = remove_block_then_apply(&store, &root, |_| {
            vec![
                RegistryChange::RemoveInstance(root.clone()),
                RegistryChange::OptOut(root.clone()),
            ]
        })
        .unwrap();

        let registry = store.read().unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join(".env")).unwrap(),
            "A=1\nB=2\n"
        );
        assert_eq!(lines.len(), 1);
        assert!(registry.find(&root).is_none());
        assert!(registry.find(&other).is_some());
        assert!(registry.is_opted_out(&root));
    }

    #[test]
    fn should_leave_the_registry_alone_when_the_block_is_malformed() {
        let machine = Fixture::new();
        let root = machine.project("app", &format!("A=1\n{BLOCK_START}\nPORT=1\n"));
        let store = machine.store();
        store
            .locked(LOCK_TIMEOUT, |locked| {
                locked.save_instance(&record(&root, "app-1", 20050))
            })
            .unwrap();

        let outcome = remove_block_then_apply(&store, &root, |_| {
            vec![RegistryChange::RemoveInstance(root.clone())]
        });

        assert!(outcome.is_err());
        assert!(store.read().unwrap().find(&root).is_some());
    }

    #[test]
    fn should_not_touch_the_env_when_it_has_no_block() {
        let machine = Fixture::new();
        let root = machine.project("app", "A=1");
        let store = machine.store();

        let lines =
            remove_block_then_apply(&store, &root, |registry| opt_out_changes(registry, &root))
                .unwrap();

        assert_eq!(lines, Vec::<String>::new());
        assert_eq!(std::fs::read_to_string(root.join(".env")).unwrap(), "A=1");
        assert!(store.read().unwrap().is_opted_out(&root));
    }

    #[test]
    fn should_not_ask_for_a_second_opt_out_when_the_checkout_is_already_opted_out() {
        let registry = Registry {
            opted_out: vec![PathBuf::from("/work/app")],
            ..Registry::default()
        };

        assert_eq!(
            opt_out_changes(&registry, Path::new("/work/app")),
            Vec::new()
        );
    }

    #[test]
    fn should_report_a_malformed_block_as_a_problem_before_anything_is_deleted() {
        let machine = Fixture::new();
        let root = machine.project("app", &format!("{BLOCK_END}\n"));

        assert!(env_problem(&root).unwrap().starts_with(".env: "));
    }

    #[test]
    fn should_report_no_problem_when_the_env_is_missing() {
        let machine = Fixture::new();
        let root = machine.projects.path().join("none");
        std::fs::create_dir_all(&root).unwrap();

        assert_eq!(env_problem(&root), None);
    }
}
