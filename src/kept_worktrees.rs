//! The worktrees Claude Code asked to remove and that are still there: the
//! `WorktreeRemove` hook cannot tell the user anything once the session is
//! gone, so it leaves a small entry that `ds ls` and `ds worktree rm` show.
//! Entries live in `<state dir>/worktrees/kept/`, one file per worktree. They
//! hold a path, a short reason and a date, never an environment value.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::worktree_identity::WorktreeIdentity;
use crate::worktree_lock::{WorktreeKey, worktrees_directory};

const KEPT_DIRECTORY: &str = "kept";
const PATH_PREFIX: &str = "path=";
const REASON_PREFIX: &str = "reason=";
const DATE_PREFIX: &str = "date=";
const IDENTITY_PREFIX: &str = "identity=";
const TEMP_EXTENSION_PREFIX: &str = "tmp-";
const SECONDS_PER_DAY: u64 = 86_400;

/// A worktree that was asked to be removed and is still there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptWorktree {
    /// The worktree.
    pub path: PathBuf,
    /// Why it was kept, in a few words.
    pub reason: String,
    /// The UTC date of the request, `YYYY-MM-DD`.
    pub date: String,
}

/// An entry as stored: what is shown, and which folder it was written for.
struct StoredEntry {
    kept: KeptWorktree,
    identity: WorktreeIdentity,
}

impl StoredEntry {
    fn to_text(&self) -> String {
        format!(
            "{PATH_PREFIX}{}\n{REASON_PREFIX}{}\n{DATE_PREFIX}{}\n{IDENTITY_PREFIX}{}\n",
            self.kept.path.display(),
            single_line(&self.kept.reason),
            self.kept.date,
            self.identity.to_text()
        )
    }

    fn from_text(text: &str) -> Option<Self> {
        let value_of = |prefix: &str| text.lines().find_map(|line| line.strip_prefix(prefix));
        Some(Self {
            kept: KeptWorktree {
                path: PathBuf::from(value_of(PATH_PREFIX)?),
                reason: value_of(REASON_PREFIX)?.to_owned(),
                date: value_of(DATE_PREFIX)?.to_owned(),
            },
            identity: WorktreeIdentity::from_text(value_of(IDENTITY_PREFIX)?)?,
        })
    }

    /// Whether the folder at the path is still the one the entry was written for.
    fn is_current(&self) -> bool {
        WorktreeIdentity::of(&self.kept.path)
            .is_ok_and(|current| current.is_same_folder_as(&self.identity))
    }
}

fn single_line(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn kept_directory(state_directory: &Path) -> PathBuf {
    worktrees_directory(state_directory).join(KEPT_DIRECTORY)
}

fn entry_path(state_directory: &Path, key: &WorktreeKey) -> PathBuf {
    kept_directory(state_directory).join(key.file_stem())
}

/// Records that the worktree of `key` was asked to be removed at `now` and
/// kept for `reason`, tied to the folder that is there now: nothing is written
/// when there is none. Written atomically: a temporary file, then a rename.
pub fn record(
    state_directory: &Path,
    key: &WorktreeKey,
    reason: &str,
    now: SystemTime,
) -> std::io::Result<()> {
    let entry = StoredEntry {
        kept: KeptWorktree {
            path: key.path().to_path_buf(),
            reason: reason.to_owned(),
            date: utc_date(now),
        },
        identity: WorktreeIdentity::of(key.path())?,
    };
    if entry.kept.path.to_string_lossy().contains('\n') {
        return Err(std::io::Error::other("the path holds a line break"));
    }
    let path = entry_path(state_directory, key);
    std::fs::create_dir_all(kept_directory(state_directory))?;
    let temp = path.with_extension(format!("{TEMP_EXTENSION_PREFIX}{}", std::process::id()));
    let mut file = std::fs::File::create(&temp)?;
    file.write_all(entry.to_text().as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temp, path)
}

/// Deletes the entry of `key`; a missing one is fine.
pub fn forget(state_directory: &Path, key: &WorktreeKey) {
    let _ = std::fs::remove_file(entry_path(state_directory, key));
}

/// The entry of `key`. An entry whose worktree is gone or was replaced by
/// another folder is deleted and `None`.
pub fn read(state_directory: &Path, key: &WorktreeKey) -> Option<KeptWorktree> {
    read_entry(&entry_path(state_directory, key))
}

/// Every entry whose worktree is still the one it was written for, by path.
/// The others are deleted.
pub fn list(state_directory: &Path) -> Vec<KeptWorktree> {
    let Ok(entries) = std::fs::read_dir(kept_directory(state_directory)) else {
        return Vec::new();
    };
    let mut kept: Vec<KeptWorktree> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_none())
        .filter_map(|path| read_entry(&path))
        .collect();
    kept.sort_by(|first, second| first.path.cmp(&second.path));
    kept
}

fn read_entry(path: &Path) -> Option<KeptWorktree> {
    let text = std::fs::read_to_string(path).ok()?;
    let Some(entry) = StoredEntry::from_text(&text).filter(StoredEntry::is_current) else {
        let _ = std::fs::remove_file(path);
        return None;
    };
    Some(entry.kept)
}

/// The UTC date of `moment` as `YYYY-MM-DD`.
pub fn utc_date(moment: SystemTime) -> String {
    let seconds = moment
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (year, month, day) = civil_from_days(seconds / SECONDS_PER_DAY);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The proleptic Gregorian date of a number of days since 1970-01-01.
fn civil_from_days(days_since_epoch: u64) -> (u64, u64, u64) {
    const DAYS_PER_ERA: u64 = 146_097;
    const DAYS_FROM_0000_TO_1970: u64 = 719_468;
    let days = days_since_epoch + DAYS_FROM_0000_TO_1970;
    let era = days / DAYS_PER_ERA;
    let day_of_era = days % DAYS_PER_ERA;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pretty_assertions::assert_eq;

    use super::*;

    fn moment(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn existing_worktree(parent: &Path, name: &str) -> WorktreeKey {
        std::fs::create_dir_all(parent.join(name)).unwrap();
        std::fs::write(parent.join(name).join(".git"), "gitdir: x").unwrap();
        WorktreeKey::of(&parent.join(name)).unwrap()
    }

    #[test]
    fn should_format_the_utc_date_of_a_moment() {
        assert_eq!(utc_date(moment(0)), "1970-01-01");
        assert_eq!(utc_date(moment(1_700_000_000)), "2023-11-14");
        assert_eq!(utc_date(moment(1_709_164_800)), "2024-02-29");
    }

    #[test]
    fn should_read_back_what_was_recorded() {
        let state = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let key = existing_worktree(worktrees.path(), "feat");

        record(
            state.path(),
            &key,
            "in use by 12 (node)",
            moment(1_700_000_000),
        )
        .unwrap();

        assert_eq!(
            read(state.path(), &key),
            Some(KeptWorktree {
                path: key.path().to_path_buf(),
                reason: "in use by 12 (node)".to_owned(),
                date: "2023-11-14".to_owned(),
            })
        );
    }

    #[test]
    fn should_ignore_and_delete_an_entry_whose_worktree_is_gone() {
        let state = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let key = existing_worktree(worktrees.path(), "feat");
        record(state.path(), &key, "locked", moment(0)).unwrap();
        std::fs::remove_dir_all(key.path()).unwrap();

        assert_eq!(list(state.path()), Vec::new());

        assert!(!entry_path(state.path(), &key).exists());
    }

    #[test]
    fn should_ignore_and_delete_an_entry_whose_folder_was_replaced_by_another() {
        let state = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let key = existing_worktree(worktrees.path(), "feat");
        record(state.path(), &key, "locked", moment(0)).unwrap();
        std::fs::rename(key.path(), worktrees.path().join("feat-old")).unwrap();
        existing_worktree(worktrees.path(), "feat");

        assert_eq!(read(state.path(), &key), None);

        assert!(!entry_path(state.path(), &key).exists());
    }

    #[test]
    fn should_write_nothing_when_the_worktree_does_not_exist() {
        let state = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let key = existing_worktree(worktrees.path(), "feat");
        std::fs::remove_dir_all(key.path()).unwrap();

        let result = record(state.path(), &key, "locked", moment(0));

        assert!(result.is_err());
        assert!(!entry_path(state.path(), &key).exists());
    }

    #[test]
    fn should_forget_only_the_entry_of_the_key() {
        let state = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let key = existing_worktree(worktrees.path(), "feat");
        let neighbour = existing_worktree(worktrees.path(), "other");
        record(state.path(), &key, "locked", moment(0)).unwrap();
        record(state.path(), &neighbour, "locked", moment(0)).unwrap();

        forget(state.path(), &key);

        assert_eq!(read(state.path(), &key), None);
        assert!(read(state.path(), &neighbour).is_some());
    }

    #[test]
    fn should_list_the_entries_by_path_and_skip_temporary_files() {
        let state = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let second = existing_worktree(worktrees.path(), "b");
        let first = existing_worktree(worktrees.path(), "a");
        record(state.path(), &second, "locked", moment(0)).unwrap();
        record(state.path(), &first, "locked", moment(0)).unwrap();
        std::fs::write(kept_directory(state.path()).join("abcd.tmp-1"), "path=/x").unwrap();

        let paths: Vec<PathBuf> = list(state.path())
            .into_iter()
            .map(|kept| kept.path)
            .collect();

        assert_eq!(
            paths,
            vec![first.path().to_path_buf(), second.path().to_path_buf()]
        );
    }

    #[test]
    fn should_keep_the_reason_on_one_line() {
        let state = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let key = existing_worktree(worktrees.path(), "feat");

        record(state.path(), &key, "first\nsecond", moment(0)).unwrap();

        assert_eq!(read(state.path(), &key).unwrap().reason, "first second");
    }
}
