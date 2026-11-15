//! `ds prune`: forget the instances whose folder no longer exists, after
//! deleting their containers and volumes, and the opted-out paths whose folder
//! is gone too.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::cli::confirm::{ConfirmationPolicy, exit_code_unless_confirmed};
use crate::cli::exit_code;
use crate::commands::machine::Machine;
use crate::commands::registering::LOCK_TIMEOUT;
use crate::compose::runner::{ComposeRunner, DockerRunner, delete_project_data};
use crate::error::ToolError;
use crate::registry::record::{InstanceRecord, Registry, RegistryChange};
use crate::registry::store::RegistryStore;

const COMMAND_NAME: &str = "ds prune";

/// What the cleanup of the gone instances gave.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cleanup {
    /// The paths whose containers and volumes are deleted.
    pub cleaned: Vec<PathBuf>,
    /// How many instances could not be cleaned.
    pub failures: usize,
}

/// Whether the folder at `path` is gone: only a `NotFound` answer counts, so
/// an unreadable parent never makes a live checkout look deleted.
pub fn is_gone(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path),
        Err(error) if error.kind() == ErrorKind::NotFound
    )
}

/// Deletes the data of the instances whose folder is gone and forgets them.
pub fn run_prune(policy: ConfirmationPolicy) -> Result<u8, ToolError> {
    let machine = Machine::from_process()?;
    let registry = machine.store.read()?;
    let gone_instances: Vec<InstanceRecord> = registry
        .instances
        .iter()
        .filter(|instance| is_gone(&instance.path))
        .cloned()
        .collect();
    let gone_paths: Vec<&PathBuf> = registry
        .opted_out
        .iter()
        .filter(|path| is_gone(path))
        .collect();
    if gone_instances.is_empty() && gone_paths.is_empty() {
        eprintln!("ds: nothing to prune");
        return Ok(exit_code::SUCCESS);
    }
    list_gone(&gone_instances, &gone_paths);
    if let Some(code) = refuse_unless_confirmed(&gone_instances, policy) {
        return Ok(code);
    }
    let runner = DockerRunner::from_environment();
    let cleanup = clean_instances(&runner, &gone_instances, &|instance| {
        machine
            .store
            .read()
            .is_ok_and(|registry| is_still_prunable(&registry, instance))
    });
    forget(&machine.store, &cleanup.cleaned)?;
    Ok(if cleanup.failures == 0 {
        exit_code::SUCCESS
    } else {
        exit_code::ERROR
    })
}

fn list_gone(instances: &[InstanceRecord], opted_out: &[&PathBuf]) {
    eprintln!("ds: these checkouts no longer exist:");
    for instance in instances {
        eprintln!("  {}  {}", instance.name, instance.path.display());
    }
    for path in opted_out {
        eprintln!("  (opted out)  {}", path.display());
    }
}

fn refuse_unless_confirmed(instances: &[InstanceRecord], policy: ConfirmationPolicy) -> Option<u8> {
    if instances.is_empty() {
        return None;
    }
    let prompt = format!(
        "Delete the containers and volumes of these {} instances and forget them? [y/N] ",
        instances.len()
    );
    exit_code_unless_confirmed(COMMAND_NAME, policy, &prompt)
}

/// Whether the registry still holds `instance` under the same name and its
/// folder is still gone: a worktree recreated at the same path while the
/// prompt waited must keep its data.
pub fn is_still_prunable(registry: &Registry, instance: &InstanceRecord) -> bool {
    is_gone(&instance.path)
        && registry
            .find(&instance.path)
            .is_some_and(|current| current.name == instance.name)
}

/// Deletes the containers, volumes and networks of each instance by label,
/// under the name the registry holds. Right before each one, `is_prunable`
/// must still say yes; otherwise it is skipped. An instance that fails stays
/// out of `cleaned`.
pub fn clean_instances(
    runner: &dyn ComposeRunner,
    instances: &[InstanceRecord],
    is_prunable: &dyn Fn(&InstanceRecord) -> bool,
) -> Cleanup {
    let mut cleanup = Cleanup::default();
    for instance in instances {
        if !is_prunable(instance) {
            eprintln!(
                "ds: skipping {}: its folder is back or its registry entry changed",
                instance.name
            );
            continue;
        }
        match delete_project_data(runner, &instance.name) {
            Ok(()) => cleanup.cleaned.push(instance.path.clone()),
            Err(error) => {
                eprintln!(
                    "ds: warning: could not delete the containers and volumes of {} ({error}); it stays in the registry",
                    instance.name
                );
                cleanup.failures += 1;
            }
        }
    }
    cleanup
}

/// In one lock hold: forgets the cleaned instances that are still gone and the
/// opted-out paths that are gone.
pub fn forget(store: &RegistryStore, cleaned: &[PathBuf]) -> Result<(), ToolError> {
    store.locked(LOCK_TIMEOUT, |locked| {
        let removed: Vec<PathBuf> = cleaned
            .iter()
            .filter(|path| is_gone(path))
            .cloned()
            .collect();
        let forgotten: Vec<PathBuf> = locked
            .registry()
            .opted_out
            .iter()
            .filter(|path| is_gone(path))
            .cloned()
            .collect();
        let changes: Vec<RegistryChange> = removed
            .iter()
            .cloned()
            .map(RegistryChange::RemoveInstance)
            .chain(forgotten.iter().cloned().map(RegistryChange::ForgetOptOut))
            .collect();
        locked.apply(&changes)?;
        for path in removed.iter().chain(&forgotten) {
            eprintln!("ds: forgot {}", path.display());
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::compose::runner::testing::FakeComposeRunner;
    use crate::discovery::PortKind;
    use crate::paths::{DevslotPaths, PathVariables};
    use crate::port_range::PortRange;
    use crate::registry::record::{AssignedPort, PortDetail};

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
        }
    }

    fn store_in(directory: &Path) -> RegistryStore {
        RegistryStore::new(
            DevslotPaths::from_variables(&PathVariables {
                config_home: Some(OsString::from(directory.join("config"))),
                state_home: Some(OsString::from(directory.join("state"))),
                home: None,
            })
            .unwrap(),
        )
    }

    #[test]
    fn should_tell_a_deleted_folder_from_one_that_exists() {
        let directory = tempfile::tempdir().unwrap();

        assert!(!is_gone(directory.path()));
        assert!(is_gone(&directory.path().join("deleted")));
    }

    #[test]
    fn should_not_call_a_dangling_symlink_gone() {
        let directory = tempfile::tempdir().unwrap();
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(directory.path().join("nowhere"), &link).unwrap();

        assert!(!is_gone(&link));
    }

    #[test]
    fn should_not_call_a_path_gone_when_its_parent_cannot_be_read() {
        let file = tempfile::NamedTempFile::new().unwrap();

        assert!(!is_gone(&file.path().join("inside")));
    }

    #[test]
    fn should_clean_each_instance_by_the_project_label_of_its_registry_name() {
        let runner = FakeComposeRunner::default();
        let instances = [record(Path::new("/gone/a"), "a-1", 20050)];

        let cleanup = clean_instances(&runner, &instances, &|_| true);

        let calls = runner.calls();
        assert_eq!(cleanup.cleaned, vec![PathBuf::from("/gone/a")]);
        assert_eq!(
            (calls[0].command, calls[0].project.as_str()),
            ("docker ps", "a-1")
        );
        assert_eq!(calls[0].files, Vec::<PathBuf>::new());
    }

    #[test]
    fn should_keep_the_instance_and_count_a_failure_when_the_cleanup_fails() {
        let runner = FakeComposeRunner::default().with_failing_commands(vec!["docker ps"]);
        let instances = [record(Path::new("/gone/a"), "a-1", 20050)];

        let cleanup = clean_instances(&runner, &instances, &|_| true);

        assert_eq!(
            cleanup,
            Cleanup {
                cleaned: vec![],
                failures: 1
            }
        );
    }

    #[test]
    fn should_skip_an_instance_that_is_no_longer_prunable_and_delete_nothing() {
        let runner = FakeComposeRunner::default();
        let instances = [record(Path::new("/gone/a"), "a-1", 20050)];

        let cleanup = clean_instances(&runner, &instances, &|_| false);

        assert_eq!(cleanup, Cleanup::default());
        assert_eq!(runner.calls(), vec![]);
    }

    #[test]
    fn should_not_prune_when_the_folder_came_back_or_the_name_changed() {
        let directory = tempfile::tempdir().unwrap();
        let gone = directory.path().join("gone");
        let registry = Registry {
            instances: vec![record(&gone, "a-1", 20050)],
            opted_out: vec![],
        };

        assert!(is_still_prunable(&registry, &record(&gone, "a-1", 20050)));
        assert!(!is_still_prunable(
            &registry,
            &record(&gone, "other-1", 20050)
        ));
        std::fs::create_dir(&gone).unwrap();
        assert!(!is_still_prunable(&registry, &record(&gone, "a-1", 20050)));
    }

    #[test]
    fn should_forget_only_the_cleaned_instance_and_leave_the_other_entry_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_in(directory.path());
        let alive = directory.path().to_path_buf();
        let gone_cleaned = directory.path().join("gone-cleaned");
        let gone_kept = directory.path().join("gone-kept");
        store
            .locked(LOCK_TIMEOUT, |locked| {
                locked.save_instance(&record(&alive, "alive-1", 20050))?;
                locked.save_instance(&record(&gone_cleaned, "cleaned-1", 20100))?;
                locked.save_instance(&record(&gone_kept, "kept-1", 20150))
            })
            .unwrap();

        forget(&store, std::slice::from_ref(&gone_cleaned)).unwrap();

        let registry = store.read().unwrap();
        assert!(registry.find(&alive).is_some());
        assert!(registry.find(&gone_cleaned).is_none());
        assert!(registry.find(&gone_kept).is_some());
    }

    #[test]
    fn should_keep_an_instance_whose_folder_came_back_before_the_forgetting() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_in(directory.path());
        let back = directory.path().join("back");
        std::fs::create_dir(&back).unwrap();
        store
            .locked(LOCK_TIMEOUT, |locked| {
                locked.save_instance(&record(&back, "back-1", 20050))
            })
            .unwrap();

        forget(&store, std::slice::from_ref(&back)).unwrap();

        assert!(store.read().unwrap().find(&back).is_some());
    }

    #[test]
    fn should_forget_the_opted_out_paths_whose_folder_is_gone_only() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_in(directory.path());
        let alive = directory.path().to_path_buf();
        let gone = directory.path().join("gone");
        store
            .locked(LOCK_TIMEOUT, |locked| {
                locked.apply(&[
                    RegistryChange::OptOut(alive.clone()),
                    RegistryChange::OptOut(gone.clone()),
                ])
            })
            .unwrap();

        forget(&store, &[]).unwrap();

        let registry = store.read().unwrap();
        assert!(registry.is_opted_out(&alive));
        assert!(!registry.is_opted_out(&gone));
    }
}
