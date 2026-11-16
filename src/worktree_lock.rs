//! The per-worktree lock that serializes the creation and the removal of one
//! worktree. Claude Code can run the plugin's hook and the same hook declared
//! in `settings.json` side by side, and `ds worktree rm` can run next to a
//! hook: the lock makes the second one wait for, or give way to, the first.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

use crate::ensure_cache::key_of;

/// The folder of the state directory that holds the locks and the kept
/// entries. It is not under `ensure/`, which the sweep of `ds prune` cleans.
pub const WORKTREES_DIRECTORY: &str = "worktrees";
/// Lock files are never deleted: a process that waits on a lock file holds the
/// old file, so deleting it lets a third process create and lock a new file at
/// the same path, and two holders run at once (a waiting remover would then
/// delete a worktree that was just recreated).
const LOCK_EXTENSION: &str = "lock";
const RETRY_INTERVAL: Duration = Duration::from_millis(25);

/// Why a lock could not be taken.
#[derive(Debug, Error)]
pub enum WorktreeLockError {
    /// The lock file cannot be created, opened or locked.
    #[error("cannot lock {}: {source}", path.display())]
    Io {
        /// The lock file or its folder.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
}

/// What the holder of a lock is doing, written in the lock file while it holds
/// the lock: a waiting creation may reopen the worktree of a holder that
/// creates it, never the one of a holder that removes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockPurpose {
    /// The holder creates or reopens the worktree.
    Create,
    /// The holder removes the worktree.
    Remove,
}

impl LockPurpose {
    const fn as_text(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Remove => "remove",
        }
    }

    fn from_text(text: &str) -> Option<Self> {
        match text.trim() {
            "create" => Some(Self::Create),
            "remove" => Some(Self::Remove),
            _ => None,
        }
    }
}

/// What identifies a worktree: its path with the parent folder resolved, so a
/// symlink in the worktrees root or in `HOME` gives the same key on both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeKey {
    path: PathBuf,
}

impl WorktreeKey {
    /// The key of the worktree at `worktree_path`; `None` when the path has no
    /// file name or its parent folder does not exist (the worktree is absent).
    pub fn of(worktree_path: &Path) -> Option<Self> {
        let name = worktree_path.file_name()?;
        let parent = worktree_path.parent()?.canonicalize().ok()?;
        Some(Self {
            path: parent.join(name),
        })
    }

    /// The worktree path with its parent folder resolved.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file name stem shared by the lock and the kept entry of the key.
    pub fn file_stem(&self) -> String {
        key_of(&self.path)
    }

    fn lock_path(&self, state_directory: &Path) -> PathBuf {
        worktrees_directory(state_directory)
            .join(self.file_stem())
            .with_extension(LOCK_EXTENSION)
    }
}

/// The folder of the locks and the kept entries.
pub fn worktrees_directory(state_directory: &Path) -> PathBuf {
    state_directory.join(WORKTREES_DIRECTORY)
}

/// An exclusive lock on one worktree, released when dropped.
#[derive(Debug)]
pub struct WorktreeLock {
    file: File,
}

impl WorktreeLock {
    /// Takes the lock without waiting; `None` when somebody holds it.
    pub fn try_acquire(
        state_directory: &Path,
        key: &WorktreeKey,
        purpose: LockPurpose,
    ) -> Result<Option<Self>, WorktreeLockError> {
        let path = key.lock_path(state_directory);
        let file = open_lock_file(&path)?;
        let Ok(lock) = try_lock_file(file, &path)? else {
            return Ok(None);
        };
        lock.write_purpose(purpose, &path)?;
        Ok(Some(lock))
    }

    /// Takes the lock, retrying every 25 ms until `wait` has passed; `None`
    /// when it is still held then. The lock is tried at least once.
    pub fn acquire_within(
        state_directory: &Path,
        key: &WorktreeKey,
        wait: Duration,
        purpose: LockPurpose,
    ) -> Result<Option<Self>, WorktreeLockError> {
        let path = key.lock_path(state_directory);
        let mut file = open_lock_file(&path)?;
        let started = Instant::now();
        loop {
            file = match try_lock_file(file, &path)? {
                Ok(lock) => {
                    lock.write_purpose(purpose, &path)?;
                    return Ok(Some(lock));
                }
                Err(file) => file,
            };
            let left = wait.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Ok(None);
            }
            thread::sleep(RETRY_INTERVAL.min(left));
        }
    }

    /// What the current holder said it is doing; `None` when the file is
    /// empty, unreadable or says something else.
    pub fn purpose_of_holder(state_directory: &Path, key: &WorktreeKey) -> Option<LockPurpose> {
        let text = std::fs::read_to_string(key.lock_path(state_directory)).ok()?;
        LockPurpose::from_text(&text)
    }

    fn write_purpose(&self, purpose: LockPurpose, path: &Path) -> Result<(), WorktreeLockError> {
        let mut file = &self.file;
        file.set_len(0)
            .and_then(|()| file.write_all(purpose.as_text().as_bytes()))
            .map_err(|source| WorktreeLockError::Io {
                path: path.to_path_buf(),
                source,
            })
    }
}

impl Drop for WorktreeLock {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
        let _ = self.file.unlock();
    }
}

/// The lock when taken, the file back when somebody else holds it.
fn try_lock_file(file: File, path: &Path) -> Result<Result<WorktreeLock, File>, WorktreeLockError> {
    match file.try_lock() {
        Ok(()) => Ok(Ok(WorktreeLock { file })),
        Err(TryLockError::WouldBlock) => Ok(Err(file)),
        Err(TryLockError::Error(source)) => Err(WorktreeLockError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn open_lock_file(path: &Path) -> Result<File, WorktreeLockError> {
    let lock_error = |source| WorktreeLockError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory).map_err(lock_error)?;
    }
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(lock_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_in(directory: &Path, name: &str) -> WorktreeKey {
        WorktreeKey::of(&directory.join(name)).unwrap()
    }

    #[test]
    fn should_give_the_same_key_when_the_parent_is_reached_through_a_symlink() {
        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let through_link = WorktreeKey::of(&link.join("feat")).unwrap();
        let through_real = WorktreeKey::of(&real.join("feat")).unwrap();

        assert_eq!(through_link, through_real);
        assert_eq!(through_link.file_stem(), through_real.file_stem());
    }

    #[test]
    fn should_give_no_key_when_the_parent_folder_is_missing() {
        let directory = tempfile::tempdir().unwrap();

        assert_eq!(WorktreeKey::of(&directory.path().join("gone/feat")), None);
    }

    #[test]
    fn should_refuse_a_second_holder_until_the_first_is_dropped() {
        let state = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let key = key_in(worktrees.path(), "feat");

        let first = WorktreeLock::try_acquire(state.path(), &key, LockPurpose::Create).unwrap();
        let while_held =
            WorktreeLock::try_acquire(state.path(), &key, LockPurpose::Create).unwrap();
        drop(first);
        let after_drop =
            WorktreeLock::try_acquire(state.path(), &key, LockPurpose::Create).unwrap();

        assert!(while_held.is_none());
        assert!(after_drop.is_some());
    }

    #[test]
    fn should_time_out_when_the_lock_stays_held() {
        let state = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let key = key_in(worktrees.path(), "feat");
        let _held = WorktreeLock::try_acquire(state.path(), &key, LockPurpose::Create).unwrap();
        let started = Instant::now();

        let waited = WorktreeLock::acquire_within(
            state.path(),
            &key,
            Duration::from_millis(100),
            LockPurpose::Create,
        )
        .unwrap();

        assert!(waited.is_none());
        assert!(started.elapsed() >= Duration::from_millis(100));
    }

    #[test]
    fn should_take_the_lock_when_the_holder_releases_it_during_the_wait() {
        let state = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let key = key_in(worktrees.path(), "feat");
        let held = WorktreeLock::try_acquire(state.path(), &key, LockPurpose::Create).unwrap();
        let releaser = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            drop(held);
        });

        let waited = WorktreeLock::acquire_within(
            state.path(),
            &key,
            Duration::from_secs(5),
            LockPurpose::Create,
        )
        .unwrap();

        releaser.join().unwrap();
        assert!(waited.is_some());
    }

    #[test]
    fn should_not_block_a_neighbour_worktree() {
        let state = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let _held = WorktreeLock::try_acquire(
            state.path(),
            &key_in(worktrees.path(), "feat"),
            LockPurpose::Create,
        )
        .unwrap();

        let neighbour = WorktreeLock::try_acquire(
            state.path(),
            &key_in(worktrees.path(), "other"),
            LockPurpose::Create,
        )
        .unwrap();

        assert!(neighbour.is_some());
    }

    #[test]
    fn should_keep_the_lock_file_when_the_lock_is_released() {
        let state = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let key = key_in(worktrees.path(), "feat");

        drop(WorktreeLock::try_acquire(state.path(), &key, LockPurpose::Create).unwrap());

        assert!(key.lock_path(state.path()).is_file());
    }

    #[test]
    fn should_tell_what_the_holder_does_while_it_holds_the_lock() {
        let state = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let key = key_in(worktrees.path(), "feat");

        let held = WorktreeLock::try_acquire(state.path(), &key, LockPurpose::Remove).unwrap();
        let while_held = WorktreeLock::purpose_of_holder(state.path(), &key);
        drop(held);
        let after_drop = WorktreeLock::purpose_of_holder(state.path(), &key);

        assert_eq!(while_held, Some(LockPurpose::Remove));
        assert_eq!(after_drop, None);
    }
}
