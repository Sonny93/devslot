//! The refusal cache of `ds ensure`: the hook runs before every agent command,
//! so a refusal is reported once and then stays quiet until the checkout
//! changes. Entries live in `<state dir>/ensure/`, one file per checkout.

use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::compose::selection::existing_overrides;
use crate::project_config::{CONFIG_FILE, LOCAL_CONFIG_FILE};

/// How long a Docker failure silences `ds ensure`.
pub const DOCKER_FAILURE_LIFETIME: Duration = Duration::from_mins(10);
/// How long a refusal that depends on other checkouts (no room, a forced port
/// taken by another instance) silences `ds ensure`.
pub const SHARED_STATE_LIFETIME: Duration = Duration::from_mins(5);

const CACHE_DIRECTORY: &str = "ensure";
const KEY_BYTES: usize = 8;
const EXPIRY_PREFIX: &str = "expires_at=";
const MISSING_STAMP: &str = "missing";
const WATCHED_FILES: [&str; 5] = [
    ".env",
    ".env.example",
    CONFIG_FILE,
    LOCAL_CONFIG_FILE,
    ".gitignore",
];

/// The state of the files a registration depends on: for each, its name, size
/// and modification time in nanoseconds, or "missing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    lines: Vec<String>,
}

impl Fingerprint {
    /// Reads the files of the checkout at `root`; `main_root` is the main
    /// checkout of a linked worktree, whose `.env` is a creation source.
    pub fn of(root: &Path, main_root: Option<&Path>, dev_compose: &str) -> Self {
        let mut watched: Vec<(String, PathBuf)> = std::iter::once(dev_compose.to_owned())
            .chain(existing_overrides(root))
            .chain(WATCHED_FILES.iter().map(|name| (*name).to_owned()))
            .map(|name| (name.clone(), root.join(name)))
            .collect();
        if let Some(main) = main_root {
            watched.push(("main:.env".to_owned(), main.join(".env")));
        }
        Self {
            lines: watched
                .iter()
                .map(|(name, path)| format!("{name} {}", stamp_of(path)))
                .collect(),
        }
    }

    #[cfg(test)]
    fn from_lines(lines: &[&str]) -> Self {
        Self {
            lines: lines.iter().map(|line| (*line).to_owned()).collect(),
        }
    }
}

fn stamp_of(path: &Path) -> String {
    let Ok(metadata) = std::fs::metadata(path) else {
        return MISSING_STAMP.to_owned();
    };
    let nanoseconds = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{} {nanoseconds}", metadata.len())
}

/// A refusal that was reported, with what the checkout looked like then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    /// The checkout when the refusal happened.
    pub fingerprint: Fingerprint,
    /// When the entry stops silencing, in seconds since the epoch; `None` for
    /// a refusal that only the files of the checkout can change.
    pub expires_at: Option<u64>,
}

impl CacheEntry {
    /// A refusal by the checks of this checkout.
    pub const fn refusal(fingerprint: Fingerprint) -> Self {
        Self {
            fingerprint,
            expires_at: None,
        }
    }

    /// A Docker failure at `now`.
    pub fn docker_failure(fingerprint: Fingerprint, now: SystemTime) -> Self {
        Self::expiring(fingerprint, now, DOCKER_FAILURE_LIFETIME)
    }

    /// A refusal at `now` that depends on other checkouts, not on the files of
    /// this one.
    pub fn shared_state_refusal(fingerprint: Fingerprint, now: SystemTime) -> Self {
        Self::expiring(fingerprint, now, SHARED_STATE_LIFETIME)
    }

    fn expiring(fingerprint: Fingerprint, now: SystemTime, lifetime: Duration) -> Self {
        Self {
            fingerprint,
            expires_at: Some(unix_seconds(now) + lifetime.as_secs()),
        }
    }

    /// Whether `ds ensure` stays quiet: the checkout still looks the same as
    /// when it was refused, and the entry has not expired.
    pub fn silences(&self, current: &Fingerprint, now: SystemTime) -> bool {
        &self.fingerprint == current
            && self
                .expires_at
                .is_none_or(|expiry| unix_seconds(now) < expiry)
    }

    fn to_text(&self) -> String {
        let mut text = String::new();
        if let Some(expiry) = self.expires_at {
            let _ = writeln!(text, "{EXPIRY_PREFIX}{expiry}");
        }
        for line in &self.fingerprint.lines {
            let _ = writeln!(text, "{line}");
        }
        text
    }

    fn from_text(text: &str) -> Self {
        let mut expires_at = None;
        let mut lines = Vec::new();
        for line in text.lines() {
            match line.strip_prefix(EXPIRY_PREFIX) {
                Some(seconds) => expires_at = seconds.parse().ok(),
                None => lines.push(line.to_owned()),
            }
        }
        Self {
            fingerprint: Fingerprint { lines },
            expires_at,
        }
    }
}

fn unix_seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Who holds the right to register a checkout right now.
#[derive(Debug)]
pub enum Flight {
    /// This process does; the lock is released when the value is dropped.
    Mine(std::fs::File),
    /// Another `ds ensure` is already registering this checkout.
    Busy,
    /// The lock file could not be used: carry on without it.
    Unavailable,
}

/// Takes, without waiting, the lock of the checkout at `root`, so that parallel
/// hooks do not all probe Docker for the same checkout.
pub fn try_take_flight(state_directory: &Path, root: &Path) -> Flight {
    let lock_path = entry_path(state_directory, root).with_extension("lock");
    let Some(directory) = lock_path.parent() else {
        return Flight::Unavailable;
    };
    if std::fs::create_dir_all(directory).is_err() {
        return Flight::Unavailable;
    }
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
    else {
        return Flight::Unavailable;
    };
    match file.try_lock() {
        Ok(()) => Flight::Mine(file),
        Err(std::fs::TryLockError::WouldBlock) => Flight::Busy,
        Err(std::fs::TryLockError::Error(_)) => Flight::Unavailable,
    }
}

/// Where the entry of the checkout at `root` lives.
pub fn entry_path(state_directory: &Path, root: &Path) -> PathBuf {
    let digest = Sha256::digest(root.as_os_str().as_bytes());
    let mut key = String::with_capacity(KEY_BYTES * 2);
    for byte in digest.iter().take(KEY_BYTES) {
        let _ = write!(key, "{byte:02x}");
    }
    state_directory.join(CACHE_DIRECTORY).join(key)
}

/// The entry at `path`; `None` when it is missing or cannot be read.
pub fn read_entry(path: &Path) -> Option<CacheEntry> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| CacheEntry::from_text(&text))
}

/// Writes the entry atomically: a temporary file, then a rename.
pub fn write_entry(path: &Path, entry: &CacheEntry) -> std::io::Result<()> {
    let directory = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(directory)?;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = std::fs::File::create(&temp)?;
    file.write_all(entry.to_text().as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temp, path)
}

/// Deletes the entry; a missing one is fine.
pub fn remove_entry(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn moment(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn fingerprint() -> Fingerprint {
        Fingerprint::from_lines(&["dev.compose.yml 120 1000", ".env missing"])
    }

    #[test]
    fn should_silence_a_refusal_while_the_fingerprint_is_the_same() {
        let entry = CacheEntry::refusal(fingerprint());

        assert!(entry.silences(&fingerprint(), moment(10_000_000)));
    }

    #[test]
    fn should_not_silence_when_a_file_changed() {
        let entry = CacheEntry::refusal(fingerprint());
        let changed = Fingerprint::from_lines(&["dev.compose.yml 121 1001", ".env missing"]);

        assert!(!entry.silences(&changed, moment(0)));
    }

    #[test]
    fn should_silence_a_docker_failure_for_less_than_ten_minutes() {
        let entry = CacheEntry::docker_failure(fingerprint(), moment(1_000));

        assert!(entry.silences(&fingerprint(), moment(1_599)));
        assert!(!entry.silences(&fingerprint(), moment(1_600)));
    }

    #[test]
    fn should_silence_a_refusal_that_depends_on_other_checkouts_for_less_than_five_minutes() {
        let entry = CacheEntry::shared_state_refusal(fingerprint(), moment(1_000));

        assert!(entry.silences(&fingerprint(), moment(1_299)));
        assert!(!entry.silences(&fingerprint(), moment(1_300)));
    }

    #[test]
    fn should_let_only_one_process_take_the_flight_of_a_checkout_at_a_time() {
        let state = tempfile::tempdir().unwrap();
        let root = Path::new("/work/app");

        let first = try_take_flight(state.path(), root);
        let second = try_take_flight(state.path(), root);
        let other_checkout = try_take_flight(state.path(), Path::new("/work/other"));
        drop(first);
        let third = try_take_flight(state.path(), root);

        assert!(matches!(second, Flight::Busy));
        assert!(matches!(other_checkout, Flight::Mine(_)));
        assert!(matches!(third, Flight::Mine(_)));
    }

    #[test]
    fn should_round_trip_a_docker_failure_through_its_text() {
        let entry = CacheEntry::docker_failure(fingerprint(), moment(1_000));

        assert_eq!(CacheEntry::from_text(&entry.to_text()), entry);
    }

    #[test]
    fn should_round_trip_a_refusal_through_its_text() {
        let entry = CacheEntry::refusal(fingerprint());

        assert_eq!(CacheEntry::from_text(&entry.to_text()), entry);
    }

    #[test]
    fn should_name_the_entry_after_sixteen_hex_characters_of_the_path_hash() {
        let path = entry_path(Path::new("/state"), Path::new("/work/app"));

        let key = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(path.parent().unwrap(), Path::new("/state/ensure"));
        assert_eq!(key.len(), 16);
        assert!(key.chars().all(|character| character.is_ascii_hexdigit()));
    }

    #[test]
    fn should_give_two_checkouts_two_entries() {
        let first = entry_path(Path::new("/state"), Path::new("/work/a"));
        let second = entry_path(Path::new("/state"), Path::new("/work/b"));

        assert_ne!(first, second);
    }

    #[test]
    fn should_write_the_entry_atomically_and_read_it_back() {
        let state = tempfile::tempdir().unwrap();
        let path = entry_path(state.path(), Path::new("/work/app"));
        let entry = CacheEntry::refusal(fingerprint());

        write_entry(&path, &entry).unwrap();

        assert_eq!(read_entry(&path), Some(entry));
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|file| file.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![path.file_name().unwrap().to_owned()]);
    }

    #[test]
    fn should_read_nothing_after_the_entry_is_removed() {
        let state = tempfile::tempdir().unwrap();
        let path = entry_path(state.path(), Path::new("/work/app"));
        write_entry(&path, &CacheEntry::refusal(fingerprint())).unwrap();

        remove_entry(&path);

        assert_eq!(read_entry(&path), None);
    }

    #[test]
    fn should_see_a_changed_file_in_the_fingerprint_of_a_checkout() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("dev.compose.yml"), "a").unwrap();
        let before = Fingerprint::of(root.path(), None, "dev.compose.yml");

        std::fs::write(root.path().join("dev.compose.yml"), "ab").unwrap();
        let after = Fingerprint::of(root.path(), None, "dev.compose.yml");

        assert_ne!(before, after);
    }

    #[test]
    fn should_see_a_file_appear_in_the_fingerprint_of_a_checkout() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("dev.compose.yml"), "a").unwrap();
        let before = Fingerprint::of(root.path(), None, "dev.compose.yml");

        std::fs::write(root.path().join(".env"), "A=1\n").unwrap();

        assert_ne!(
            before,
            Fingerprint::of(root.path(), None, "dev.compose.yml")
        );
    }

    #[test]
    fn should_see_the_env_of_the_main_checkout_in_the_fingerprint() {
        let root = tempfile::tempdir().unwrap();
        let main = tempfile::tempdir().unwrap();
        let before = Fingerprint::of(root.path(), Some(main.path()), "dev.compose.yml");

        std::fs::write(main.path().join(".env"), "A=1\n").unwrap();

        assert_ne!(
            before,
            Fingerprint::of(root.path(), Some(main.path()), "dev.compose.yml")
        );
    }
}
