//! The exclusive lock on `registry.lock`. The file is never deleted or
//! renamed: another process may be holding a lock on it.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use super::RegistryError;

const RETRY_INTERVAL: Duration = Duration::from_millis(25);

/// An exclusive `flock` on the lock file, released when dropped.
#[derive(Debug)]
pub struct RegistryLock {
    file: File,
}

impl RegistryLock {
    /// Takes the lock, retrying until `timeout` has passed.
    pub fn acquire(path: &Path, timeout: Duration) -> Result<Self, RegistryError> {
        let lock_error = |source| RegistryError::Lock {
            path: path.to_path_buf(),
            source,
        };
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)
            .map_err(lock_error)?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { file }),
                Err(TryLockError::Error(source)) => return Err(lock_error(source)),
                Err(TryLockError::WouldBlock) if started.elapsed() >= timeout => {
                    return Err(RegistryError::LockTimeout { waited: timeout });
                }
                Err(TryLockError::WouldBlock) => thread::sleep(RETRY_INTERVAL),
            }
        }
    }
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_time_out_when_another_handle_holds_the_lock() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("registry.lock");
        let _held = RegistryLock::acquire(&path, Duration::from_secs(1)).unwrap();

        let error = RegistryLock::acquire(&path, Duration::from_millis(100)).unwrap_err();

        assert!(matches!(error, RegistryError::LockTimeout { .. }));
    }

    #[test]
    fn should_take_the_lock_again_when_the_first_holder_is_dropped() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("registry.lock");
        drop(RegistryLock::acquire(&path, Duration::from_secs(1)).unwrap());

        assert!(RegistryLock::acquire(&path, Duration::from_millis(100)).is_ok());
    }

    #[test]
    fn should_keep_the_lock_file_when_the_lock_is_released() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("registry.lock");

        drop(RegistryLock::acquire(&path, Duration::from_secs(1)).unwrap());

        assert!(path.is_file());
    }
}
