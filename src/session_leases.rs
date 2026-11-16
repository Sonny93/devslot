//! Session leases: a Claude Code session that runs `ds up`, `ds run` or
//! `ds reset` in a linked worktree leaves a file there, so another session's
//! removal of the worktree sees it in use even when the session only has
//! containers running (its `claude` process stays in the main checkout).
//! A lease is `<state dir>/leases/<worktree key>/<claude pid>-<start time>`
//! and holds the session id, or nothing. It lives until the session dies:
//! whoever checks it deletes it when the session is gone. Nothing here
//! follows a symlink, reads more than a few bytes or prints a stored value
//! that was not checked first.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use crate::checkout::Checkout;
use crate::instance_processes::{
    CLAUDE_COMM, SessionProcess, claude_pid_from_environment, current_session,
};
use crate::worktree_lock::WorktreeKey;

const LEASES_DIRECTORY: &str = "leases";
const NAME_SEPARATOR: char = '-';
const SESSION_ID_VARIABLE: &str = "CLAUDE_CODE_SESSION_ID";
const MAX_SESSION_ID_LENGTH: usize = 64;
const MAX_CONTENT_BYTES: u64 = 65;
const LEASE_FILE_MODE: u32 = 0o600;
const TEMPORARY_SUFFIX: &str = ".tmp-";

/// A session that holds a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLease {
    /// The `claude` process of the session.
    pub session: SessionProcess,
    /// The session id, when the lease holds a safe one.
    pub session_id: Option<String>,
}

impl SessionLease {
    /// How a message names the session: `session <id>`, else `claude <pid>`.
    pub fn describe(&self) -> String {
        match &self.session_id {
            Some(session_id) => format!("session {session_id}"),
            None => format!("{} {}", CLAUDE_COMM, self.session.pid),
        }
    }
}

/// `session a, claude 12`.
pub fn join_descriptions(leases: &[SessionLease]) -> String {
    leases
        .iter()
        .map(SessionLease::describe)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Records that the session that runs `ds` uses `checkout`, when `checkout` is
/// a linked worktree and `ds` runs under a Claude Code session. Silent: a
/// lease that cannot be written only means the removal will not see it.
pub fn record_current_session(state_directory: &Path, checkout: &Checkout) {
    if checkout.main_root.is_none() {
        return;
    }
    let Some(key) = WorktreeKey::of(&checkout.root) else {
        return;
    };
    let Some(session) = current_session(claude_pid_from_environment()) else {
        return;
    };
    let session_id = std::env::var(SESSION_ID_VARIABLE).ok();
    let _ = write(state_directory, &key, session, session_id.as_deref());
}

/// Writes the lease of `session` for the worktree of `key`. `session_id` is
/// kept only when it is safe to print.
pub fn write(
    state_directory: &Path,
    key: &WorktreeKey,
    session: SessionProcess,
    session_id: Option<&str>,
) -> io::Result<()> {
    let folder = folder_of(state_directory, key);
    let content = session_id.and_then(safe_session_id).unwrap_or_default();
    match write_in_folder(&folder, session, content) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            write_in_folder(&folder, session, content)
        }
        outcome => outcome,
    }
}

/// Creates the folder, writes the content to a temporary file next to the
/// lease, then renames it onto the lease name: a reader sees the old file or
/// the whole new one, never an empty one. The temporary name is rejected by
/// `parse_lease_name`, so nothing lists it.
fn write_in_folder(folder: &Path, session: SessionProcess, content: &str) -> io::Result<()> {
    fs::create_dir_all(folder)?;
    let temporary = folder.join(format!(
        ".{}{TEMPORARY_SUFFIX}{}",
        lease_name(session),
        std::process::id()
    ));
    let _ = fs::remove_file(&temporary);
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(LEASE_FILE_MODE)
        .custom_flags(no_follow_flag())
        .open(&temporary)
        .and_then(|mut file| file.write_all(content.as_bytes()))
        .and_then(|()| fs::rename(&temporary, folder.join(lease_name(session))));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// The leases of the worktree of `key` whose session `is_alive` says runs.
/// The dead ones are deleted on the way. Names that are not exactly
/// `<pid>-<start time>` and anything that is not a plain file are ignored.
pub fn live(
    state_directory: &Path,
    key: &WorktreeKey,
    is_alive: &dyn Fn(SessionProcess) -> bool,
) -> Vec<SessionLease> {
    live_in_folder(&folder_of(state_directory, key), is_alive)
}

/// Deletes every lease of the worktree of `key` and its folder: the worktree
/// is gone or was just made, so no lease can belong to it.
pub fn clear(state_directory: &Path, key: &WorktreeKey) {
    let _ = fs::remove_dir_all(folder_of(state_directory, key));
}

/// Deletes the dead leases of every folder, then the folders left empty. The
/// folder names are hashes, so whether the worktree still exists is unknown:
/// only the sessions decide.
pub fn sweep_dead(state_directory: &Path, is_alive: &dyn Fn(SessionProcess) -> bool) {
    let Ok(entries) = fs::read_dir(leases_directory(state_directory)) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        live_in_folder(&entry.path(), is_alive);
        let _ = fs::remove_dir(entry.path());
    }
}

fn leases_directory(state_directory: &Path) -> PathBuf {
    state_directory.join(LEASES_DIRECTORY)
}

fn folder_of(state_directory: &Path, key: &WorktreeKey) -> PathBuf {
    leases_directory(state_directory).join(key.file_stem())
}

fn lease_name(session: SessionProcess) -> String {
    format!("{}{NAME_SEPARATOR}{}", session.pid, session.start_time)
}

fn parse_lease_name(name: &str) -> Option<SessionProcess> {
    let (pid, start_time) = name.split_once(NAME_SEPARATOR)?;
    let session = SessionProcess {
        pid: pid.parse().ok().filter(|pid| *pid > 0)?,
        start_time: start_time.parse().ok()?,
    };
    (lease_name(session) == name).then_some(session)
}

fn live_in_folder(folder: &Path, is_alive: &dyn Fn(SessionProcess) -> bool) -> Vec<SessionLease> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut leases = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(session) = entry.file_name().to_str().and_then(parse_lease_name) else {
            continue;
        };
        if !is_alive(session) {
            let _ = fs::remove_file(entry.path());
            continue;
        }
        leases.push(SessionLease {
            session,
            session_id: read_session_id(&entry.path()),
        });
    }
    leases.sort_by_key(|lease| (lease.session.pid, lease.session.start_time));
    leases
}

fn read_session_id(path: &Path) -> Option<String> {
    let file = open_without_following(path).ok()?;
    let mut content = String::new();
    file.take(MAX_CONTENT_BYTES)
        .read_to_string(&mut content)
        .ok()?;
    safe_session_id(&content).map(str::to_owned)
}

fn open_without_following(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(no_follow_flag())
        .open(path)
}

fn no_follow_flag() -> i32 {
    i32::try_from(rustix::fs::OFlags::NOFOLLOW.bits()).unwrap_or_default()
}

fn safe_session_id(candidate: &str) -> Option<&str> {
    let is_safe = !candidate.is_empty()
        && candidate.len() <= MAX_SESSION_ID_LENGTH
        && candidate
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'));
    is_safe.then_some(candidate)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    const ALWAYS_ALIVE: &dyn Fn(SessionProcess) -> bool = &|_| true;
    const NEVER_ALIVE: &dyn Fn(SessionProcess) -> bool = &|_| false;

    struct Fixture {
        _base: tempfile::TempDir,
        state: tempfile::TempDir,
        key: WorktreeKey,
        other_key: WorktreeKey,
    }

    fn fixture() -> Fixture {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().canonicalize().unwrap();
        let key = WorktreeKey::of(&root.join("feat")).unwrap();
        let other_key = WorktreeKey::of(&root.join("other")).unwrap();
        Fixture {
            _base: base,
            state: tempfile::tempdir().unwrap(),
            key,
            other_key,
        }
    }

    fn session(pid: i32, start_time: u64) -> SessionProcess {
        SessionProcess { pid, start_time }
    }

    impl Fixture {
        fn folder(&self) -> PathBuf {
            folder_of(self.state.path(), &self.key)
        }

        fn write(&self, pid: i32, start_time: u64, session_id: Option<&str>) {
            write(
                self.state.path(),
                &self.key,
                session(pid, start_time),
                session_id,
            )
            .unwrap();
        }

        fn live(&self, is_alive: &dyn Fn(SessionProcess) -> bool) -> Vec<SessionLease> {
            live(self.state.path(), &self.key, is_alive)
        }
    }

    #[test]
    fn should_read_back_the_session_and_its_id_when_the_lease_is_alive() {
        let fixture = fixture();
        fixture.write(42, 9_001, Some("3f2a-b_C"));

        assert_eq!(
            fixture.live(ALWAYS_ALIVE),
            vec![SessionLease {
                session: session(42, 9_001),
                session_id: Some("3f2a-b_C".to_owned()),
            }]
        );
    }

    #[test]
    fn should_delete_the_lease_when_its_session_is_dead() {
        let fixture = fixture();
        fixture.write(42, 9_001, None);

        assert_eq!(fixture.live(NEVER_ALIVE), Vec::new());
        assert!(!fixture.folder().join("42-9001").exists());
    }

    #[test]
    fn should_ignore_the_names_that_are_not_a_pid_and_a_start_time() {
        let fixture = fixture();
        fixture.write(42, 9_001, None);
        for name in [
            "42", "-42-1", "+42-1", "042-1", "42-01", "a-1", "42-b", "0-1", "1-2-3",
        ] {
            fs::write(fixture.folder().join(name), "").unwrap();
        }
        fs::create_dir(fixture.folder().join("43-1")).unwrap();

        let found = fixture.live(ALWAYS_ALIVE);

        assert_eq!(
            found.iter().map(|lease| lease.session).collect::<Vec<_>>(),
            vec![session(42, 9_001)]
        );
        assert!(fixture.folder().join("042-1").exists());
    }

    #[test]
    fn should_not_print_a_session_id_that_is_unsafe() {
        let fixture = fixture();
        fixture.write(1, 1, Some("ok-id"));
        for (pid, content) in [
            (2, "bad id"),
            (3, "esc\u{1b}[31m"),
            (4, "line\nbreak"),
            (5, &"a".repeat(MAX_SESSION_ID_LENGTH + 1)),
            (6, "accentu\u{e9}"),
        ] {
            fs::write(fixture.folder().join(format!("{pid}-1")), content).unwrap();
        }

        let ids: Vec<Option<String>> = fixture
            .live(ALWAYS_ALIVE)
            .into_iter()
            .map(|lease| lease.session_id)
            .collect();

        assert_eq!(
            ids,
            vec![Some("ok-id".to_owned()), None, None, None, None, None]
        );
    }

    #[test]
    fn should_leave_only_the_lease_file_in_the_folder_after_a_write() {
        let fixture = fixture();
        fixture.write(7, 1, Some("abc"));
        fixture.write(7, 1, Some("def"));

        let names: Vec<_> = fs::read_dir(fixture.folder())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();

        assert_eq!(names, vec![std::ffi::OsString::from("7-1")]);
        assert_eq!(
            fs::read_to_string(fixture.folder().join("7-1")).unwrap(),
            "def"
        );
    }

    #[test]
    fn should_recreate_the_folder_when_it_is_missing_and_ignore_a_temporary_file() {
        let fixture = fixture();
        fixture.write(7, 1, None);
        fs::write(fixture.folder().join(".8-1.tmp-99"), "x").unwrap();
        fs::remove_file(fixture.folder().join("7-1")).unwrap();
        fs::remove_file(fixture.folder().join(".8-1.tmp-99")).unwrap();
        fs::remove_dir(fixture.folder()).unwrap();

        write_in_folder(&fixture.folder(), session(9, 2), "ok").unwrap();

        assert_eq!(fixture.live(ALWAYS_ALIVE).len(), 1);
    }

    #[test]
    fn should_not_count_a_temporary_file_as_a_lease_or_remove_its_folder_when_sweeping() {
        let fixture = fixture();
        fs::create_dir_all(fixture.folder()).unwrap();
        fs::write(fixture.folder().join(".8-1.tmp-99"), "x").unwrap();

        sweep_dead(fixture.state.path(), NEVER_ALIVE);

        assert_eq!(fixture.live(ALWAYS_ALIVE), Vec::new());
        assert!(fixture.folder().join(".8-1.tmp-99").exists());
    }

    #[test]
    fn should_not_write_an_unsafe_session_id() {
        let fixture = fixture();
        fixture.write(7, 1, Some("bad\nid"));

        assert_eq!(
            fs::read_to_string(fixture.folder().join("7-1")).unwrap(),
            ""
        );
    }

    #[test]
    fn should_read_nothing_through_a_symlink() {
        let fixture = fixture();
        fixture.write(1, 1, None);
        let target = fixture.state.path().join("secret");
        fs::write(&target, "hunter2").unwrap();
        std::os::unix::fs::symlink(&target, fixture.folder().join("2-1")).unwrap();

        let found = fixture.live(ALWAYS_ALIVE);

        assert_eq!(found.len(), 1);
        assert!(target.exists());
    }

    #[test]
    fn should_clear_only_the_folder_of_its_own_key() {
        let fixture = fixture();
        fixture.write(1, 1, None);
        write(
            fixture.state.path(),
            &fixture.other_key,
            session(2, 2),
            None,
        )
        .unwrap();

        clear(fixture.state.path(), &fixture.key);

        assert!(!fixture.folder().exists());
        assert_eq!(
            live(fixture.state.path(), &fixture.other_key, ALWAYS_ALIVE).len(),
            1
        );
    }

    #[test]
    fn should_sweep_only_the_dead_leases_and_the_folders_left_empty() {
        let fixture = fixture();
        fixture.write(1, 1, None);
        write(
            fixture.state.path(),
            &fixture.other_key,
            session(2, 2),
            None,
        )
        .unwrap();
        write(
            fixture.state.path(),
            &fixture.other_key,
            session(3, 3),
            None,
        )
        .unwrap();

        sweep_dead(fixture.state.path(), &|session| {
            session.pid != 1 && session.pid != 3
        });

        assert!(!fixture.folder().exists());
        assert_eq!(
            live(fixture.state.path(), &fixture.other_key, ALWAYS_ALIVE),
            vec![SessionLease {
                session: session(2, 2),
                session_id: None
            }]
        );
    }

    #[test]
    fn should_name_a_session_by_its_id_or_else_by_its_pid() {
        let with_id = SessionLease {
            session: session(12, 1),
            session_id: Some("abc".to_owned()),
        };
        let without_id = SessionLease {
            session: session(13, 1),
            session_id: None,
        };

        assert_eq!(
            join_descriptions(&[with_id, without_id]),
            "session abc, claude 13"
        );
    }
}
