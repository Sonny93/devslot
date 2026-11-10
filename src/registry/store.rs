//! The registry file: read without a lock, changed only under the lock and by
//! an atomic replace.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::time::Duration;

use toml_edit::DocumentMut;

use super::RegistryError;
use super::document::{EMPTY_REGISTRY, apply_record, parse_registry};
use super::lock::RegistryLock;
use super::record::{InstanceRecord, Registry};
use crate::paths::DevslotPaths;
use crate::project::bounded_read::{BoundedRead, Unreadable, read_bounded};

const STATE_DIRECTORY_MODE: u32 = 0o700;

/// The registry on disk.
#[derive(Debug, Clone)]
pub struct RegistryStore {
    paths: DevslotPaths,
}

/// The registry while this process holds its lock. Every change goes through
/// [`LockedRegistry::save_instance`].
#[derive(Debug)]
pub struct LockedRegistry<'store> {
    store: &'store RegistryStore,
    document: DocumentMut,
    registry: Registry,
    _lock: RegistryLock,
}

impl RegistryStore {
    /// The registry at the locations of `paths`.
    pub const fn new(paths: DevslotPaths) -> Self {
        Self { paths }
    }

    /// Reads the registry without taking the lock: the rename of a write makes
    /// every read see a whole file. A missing file is an empty registry.
    pub fn read(&self) -> Result<Registry, RegistryError> {
        let text = self.read_text()?;
        let (_, registry) = parse_registry(
            text.as_deref().unwrap_or(EMPTY_REGISTRY),
            &self.paths.registry_file,
        )?;
        Ok(registry)
    }

    /// Takes the lock, reads the registry again and hands it to `change`.
    /// The lock is released when `change` returns.
    pub fn locked<T, E: From<RegistryError>>(
        &self,
        timeout: Duration,
        change: impl FnOnce(&mut LockedRegistry<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        create_state_directory(self.paths.state_directory())?;
        let lock = RegistryLock::acquire(&self.paths.lock_file, timeout)?;
        let text = self.read_text()?;
        let (document, registry) = parse_registry(
            text.as_deref().unwrap_or(EMPTY_REGISTRY),
            &self.paths.registry_file,
        )?;
        let mut locked = LockedRegistry {
            store: self,
            document,
            registry,
            _lock: lock,
        };
        change(&mut locked)
    }

    fn read_text(&self) -> Result<Option<String>, RegistryError> {
        let path = &self.paths.registry_file;
        let bytes = match read_bounded(path) {
            Ok(BoundedRead::Bytes(bytes)) => bytes,
            Ok(BoundedRead::Missing) => return Ok(None),
            Ok(BoundedRead::Unusable(Unreadable::NotRegularFile)) => {
                return Err(unusable(path, "not a regular file"));
            }
            Ok(BoundedRead::Unusable(Unreadable::TooLarge)) => {
                return Err(unusable(path, "larger than 1 MiB"));
            }
            Err(source) => {
                return Err(RegistryError::Read {
                    path: path.clone(),
                    source,
                });
            }
        };
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| unusable(path, "not valid UTF-8"))
    }
}

impl LockedRegistry<'_> {
    /// The registry as it is on disk, read after the lock was taken.
    pub const fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Adds `record` or updates the entry of its path, then replaces the file.
    /// Every other entry, comment and unknown key stays as it was.
    pub fn save_instance(&mut self, record: &InstanceRecord) -> Result<(), RegistryError> {
        let file = &self.store.paths.registry_file;
        let mut document = self.document.clone();
        apply_record(&mut document, record, file)?;
        let text = document.to_string();
        let (checked_document, checked_registry) = parse_registry(&text, file)?;
        write_atomically(&self.store.paths, &text)?;
        self.document = checked_document;
        self.registry = checked_registry;
        Ok(())
    }
}

fn unusable(path: &Path, reason: &'static str) -> RegistryError {
    RegistryError::Unusable {
        path: path.to_path_buf(),
        reason,
    }
}

fn create_state_directory(directory: &Path) -> Result<(), RegistryError> {
    DirBuilder::new()
        .recursive(true)
        .mode(STATE_DIRECTORY_MODE)
        .create(directory)
        .map_err(|source| RegistryError::Write {
            path: directory.to_path_buf(),
            source,
        })
}

fn write_atomically(paths: &DevslotPaths, text: &str) -> Result<(), RegistryError> {
    let temp = &paths.registry_temp_file;
    let write_error = |path: &Path| {
        let path = path.to_path_buf();
        move |source| RegistryError::Write { path, source }
    };
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(temp)
        .map_err(write_error(temp))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(write_error(temp))?;
    std::fs::rename(temp, &paths.registry_file).map_err(write_error(&paths.registry_file))?;
    let directory = paths.state_directory();
    File::open(directory)
        .and_then(|handle| handle.sync_all())
        .map_err(write_error(directory))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::thread;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::discovery::PortKind;
    use crate::paths::PathVariables;
    use crate::port_range::PortRange;
    use crate::registry::record::{AssignedPort, PortDetail};

    const TIMEOUT: Duration = Duration::from_secs(5);

    fn store_in(directory: &Path) -> RegistryStore {
        let paths = DevslotPaths::from_variables(&PathVariables {
            config_home: Some(OsString::from(directory.join("config"))),
            state_home: Some(OsString::from(directory.join("state"))),
            home: None,
        })
        .unwrap();
        RegistryStore::new(paths)
    }

    fn record(path: &str, name: &str, first: u16) -> InstanceRecord {
        InstanceRecord {
            path: PathBuf::from(path),
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

    fn save(store: &RegistryStore, record: &InstanceRecord) {
        store
            .locked(TIMEOUT, |locked| locked.save_instance(record))
            .unwrap();
    }

    fn read_text(store: &RegistryStore) -> String {
        std::fs::read_to_string(&store.paths.registry_file).unwrap()
    }

    #[test]
    fn should_read_an_empty_registry_when_the_file_does_not_exist() {
        let directory = tempfile::tempdir().unwrap();

        let registry = store_in(directory.path()).read().unwrap();

        assert_eq!(registry, Registry::default());
    }

    #[test]
    fn should_create_the_state_folder_with_private_permissions_on_the_first_write() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_in(directory.path());

        save(&store, &record("/work/a", "a", 20050));

        let mode = std::fs::metadata(store.paths.state_directory())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn should_leave_no_temporary_file_and_keep_the_lock_file_after_a_write() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_in(directory.path());

        save(&store, &record("/work/a", "a", 20050));

        assert_eq!(
            (
                store.paths.registry_temp_file.exists(),
                store.paths.lock_file.exists()
            ),
            (false, true)
        );
    }

    #[test]
    fn should_keep_every_other_entry_byte_for_byte_when_one_instance_is_updated() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_in(directory.path());
        save(&store, &record("/work/a", "a", 20050));
        save(&store, &record("/work/b", "b", 20100));
        let hand_edited = read_text(&store).replace("version = 1\n", "# my notes\nversion = 1\n");
        std::fs::write(&store.paths.registry_file, &hand_edited).unwrap();
        let mut updated = record("/work/a", "a", 20050);
        updated.ports.push(AssignedPort {
            variable: "DB_PORT".to_owned(),
            port: 20051,
            detail: PortDetail {
                kind: PortKind::Web,
                service: None,
                target: None,
            },
        });

        save(&store, &updated);

        let after = read_text(&store);
        let untouched_second = hand_edited.split("[[instance]]").nth(2).unwrap();
        assert!(after.starts_with("# my notes\nversion = 1\n"));
        assert!(after.contains(untouched_second));
        assert!(after.contains("DB_PORT = 20051"));
    }

    #[test]
    fn should_refuse_to_change_a_malformed_registry_and_leave_it_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_in(directory.path());
        std::fs::create_dir_all(store.paths.state_directory()).unwrap();
        let malformed = "version = 2\n# keep me\n";
        std::fs::write(&store.paths.registry_file, malformed).unwrap();

        let error = store
            .locked(TIMEOUT, |locked| {
                locked.save_instance(&record("/work/a", "a", 20050))
            })
            .unwrap_err();

        assert!(matches!(error, RegistryError::Malformed { .. }));
        assert_eq!(read_text(&store), malformed);
    }

    #[test]
    fn should_refuse_to_write_an_instance_that_conflicts_with_a_registered_one() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_in(directory.path());
        save(&store, &record("/work/a", "a", 20050));
        let before = read_text(&store);

        let error = store
            .locked(TIMEOUT, |locked| {
                locked.save_instance(&record("/work/b", "b", 20060))
            })
            .unwrap_err();

        assert!(matches!(error, RegistryError::Malformed { .. }));
        assert_eq!(read_text(&store), before);
    }

    #[test]
    fn should_give_up_when_another_holder_keeps_the_lock_past_the_timeout() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_in(directory.path());

        let outcome = store.locked(TIMEOUT, |_| {
            let waiting = store.clone();
            thread::spawn(move || {
                waiting.locked(Duration::from_millis(100), |_| Ok::<(), RegistryError>(()))
            })
            .join()
            .unwrap()
        });

        assert!(matches!(
            outcome.unwrap_err(),
            RegistryError::LockTimeout { .. }
        ));
    }

    #[test]
    fn should_read_without_waiting_when_another_holder_has_the_lock() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_in(directory.path());
        save(&store, &record("/work/a", "a", 20050));

        let registry = store
            .locked(TIMEOUT, |_| {
                let reading = store.clone();
                Ok::<_, RegistryError>(thread::spawn(move || reading.read()).join().unwrap())
            })
            .unwrap()
            .unwrap();

        assert_eq!(registry.instances.len(), 1);
    }

    #[test]
    fn should_keep_both_instances_when_two_threads_register_at_the_same_time() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_in(directory.path());

        thread::scope(|scope| {
            for (path, name, first) in [("/work/a", "a", 20050), ("/work/b", "b", 20100)] {
                let store = &store;
                scope.spawn(move || save(store, &record(path, name, first)));
            }
        });

        assert_eq!(store.read().unwrap().instances.len(), 2);
    }

    #[test]
    fn should_see_the_entry_written_by_another_process_when_the_lock_is_taken_after_it() {
        let directory = tempfile::tempdir().unwrap();
        let store = store_in(directory.path());
        save(&store, &record("/work/a", "a", 20050));

        let known = store
            .locked(TIMEOUT, |locked| {
                Ok::<_, RegistryError>(locked.registry().find(Path::new("/work/a")).is_some())
            })
            .unwrap();

        assert!(known);
    }
}
