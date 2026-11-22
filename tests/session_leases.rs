#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Session leases end to end: a live `claude` process (a copy of `sleep` or of
//! a shell named `claude`) holds a worktree, and the removal and the cleanup
//! of `ds` see it. Real git in temp folders, the fake docker, a pretend machine.

mod support;

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use devslot::worktree_lock::WorktreeKey;
use serde_json::json;
use support::{Project, Run, TEST_SLOTS};

const SESSION_SCRIPT: &str = "while :; do sleep 1; done";
const COMM_WAIT_LIMIT: Duration = Duration::from_secs(5);
const COMM_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SHELL_PROGRAM: &str = "/bin/sh";
const START_TIME_TOKEN_AFTER_NAME: usize = 19;
const DEAD_SESSION: (i32, u64) = (2_000_000_000, 1);

struct Sandbox {
    main: Project,
    root: tempfile::TempDir,
    programs: tempfile::TempDir,
}

fn sandbox() -> Sandbox {
    let main = Project::from_scenario("template");
    main.initialize_git_and_commit(".env\n");
    let root = tempfile::tempdir().unwrap();
    main.machine().write_global_config(&format!(
        "{TEST_SLOTS}[worktrees]\nroot = \"{}\"\n",
        root.path().display()
    ));
    Sandbox {
        main,
        root,
        programs: tempfile::tempdir().unwrap(),
    }
}

impl Sandbox {
    fn main_path(&self) -> PathBuf {
        self.main.path().canonicalize().unwrap()
    }

    fn created(&self, name: &str) -> PathBuf {
        let input = json!({
            "session_id": "session",
            "transcript_path": "/tmp/transcript.jsonl",
            "hook_event_name": "WorktreeCreate",
            "cwd": self.main_path(),
            "name": name,
        })
        .to_string();
        let run = self.main.run_hook("worktree-create", &input, &[]);
        assert_eq!(run.status, 0, "{}", run.stderr);
        let repository = self.main_path().file_name().unwrap().to_owned();
        self.root
            .path()
            .canonicalize()
            .unwrap()
            .join(repository)
            .join(name)
    }

    fn remove(&self, name: &str) -> Run {
        self.main.run(&["worktree", "rm", name, "--yes"])
    }

    fn fake_program(&self, source: &str) -> PathBuf {
        let folder = self.programs.path().join(source.replace('/', "_"));
        std::fs::create_dir_all(&folder).unwrap();
        let program = folder.join("claude");
        std::os::unix::fs::symlink(source, &program).unwrap();
        program
    }

    /// A shell named `claude` looping over short sleeps: it never depends on
    /// how a multicall coreutils handles argv[0], and a killed session leaves
    /// at most a one second sleep behind.
    fn spawn_session(&self) -> Child {
        let session = Command::new(self.fake_program(SHELL_PROGRAM))
            .args(["-c", SESSION_SCRIPT])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        assert_comm_is_claude(session.id());
        session
    }

    fn lease_folder(&self, worktree: &Path) -> PathBuf {
        let key = WorktreeKey::of(worktree).unwrap();
        self.main
            .machine()
            .state_directory()
            .join("leases")
            .join(key.file_stem())
    }

    fn write_lease(&self, worktree: &Path, pid: i32, start_time: u64, content: &str) {
        let folder = self.lease_folder(worktree);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(format!("{pid}-{start_time}")), content).unwrap();
    }

    fn lease_names(&self, worktree: &Path) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.lease_folder(worktree)) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .map(|entry| entry.file_name().to_str().unwrap().to_owned())
            .collect();
        names.sort();
        names
    }
}

fn start_time_of(pid: u32) -> u64 {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let after_name = &text[text.rfind(')').unwrap() + 1..];
    after_name
        .split_whitespace()
        .nth(START_TIME_TOKEN_AFTER_NAME)
        .unwrap()
        .parse()
        .unwrap()
}

fn assert_comm_is_claude(pid: u32) {
    let started = Instant::now();
    let mut comm = String::new();
    while started.elapsed() < COMM_WAIT_LIMIT {
        comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        if comm.trim() == "claude" {
            return;
        }
        std::thread::sleep(COMM_POLL_INTERVAL);
    }
    panic!("the fake session {pid} is not a live process named claude (comm: {comm:?})");
}

fn kill(mut session: Child) {
    session.kill().unwrap();
    session.wait().unwrap();
}

#[test]
fn should_refuse_worktree_rm_and_keep_everything_when_a_live_session_holds_the_lease() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let session = sandbox.spawn_session();
    let start_time = start_time_of(session.id());
    sandbox.write_lease(
        &path,
        session.id().try_into().unwrap(),
        start_time,
        "abc-123",
    );
    sandbox.main.machine().clear_docker_log();

    let run = sandbox.remove("feature-x");
    kill(session);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("another Claude Code session (session abc-123)")
    );
    assert!(run.stderr.contains("pass --force"));
    assert!(
        run.stdout
            .contains("In use (blocks removal without --force): session abc-123")
    );
    assert!(path.exists());
    assert_eq!(sandbox.main.machine().instance_names().len(), 1);
    assert_eq!(sandbox.lease_names(&path).len(), 1);
    assert_eq!(sandbox.main.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_name_force_and_yes_and_keep_the_worktree_when_no_option_is_given_without_a_terminal() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let session = sandbox.spawn_session();
    let start_time = start_time_of(session.id());
    sandbox.write_lease(
        &path,
        session.id().try_into().unwrap(),
        start_time,
        "abc-123",
    );

    let run = sandbox.main.run(&["worktree", "rm", "feature-x"]);
    kill(session);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains("pass --force --yes"));
    assert!(path.exists());
}

#[test]
fn should_remove_the_worktree_and_only_its_own_lease_folder_when_the_lease_is_dead() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let neighbour = sandbox.created("feature-y");
    let session = sandbox.spawn_session();
    let start_time = start_time_of(session.id());
    let pid: i32 = session.id().try_into().unwrap();
    sandbox.write_lease(&path, DEAD_SESSION.0, DEAD_SESSION.1, "");
    sandbox.write_lease(&neighbour, pid, start_time, "");

    let run = sandbox.remove("feature-x");
    kill(session);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(!path.exists());
    assert!(!sandbox.lease_folder(&path).exists());
    assert_eq!(
        sandbox.lease_names(&neighbour),
        vec![format!("{pid}-{start_time}")]
    );
}

#[test]
fn should_delete_only_the_dead_leases_and_the_empty_folders_when_pruning() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let neighbour = sandbox.created("feature-y");
    let session = sandbox.spawn_session();
    let start_time = start_time_of(session.id());
    let pid: i32 = session.id().try_into().unwrap();
    sandbox.write_lease(&path, DEAD_SESSION.0, DEAD_SESSION.1, "");
    sandbox.write_lease(&neighbour, DEAD_SESSION.0, DEAD_SESSION.1, "");
    sandbox.write_lease(&neighbour, pid, start_time, "");

    let run = sandbox.main.run(&["prune", "--yes"]);
    kill(session);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(!sandbox.lease_folder(&path).exists());
    assert_eq!(
        sandbox.lease_names(&neighbour),
        vec![format!("{pid}-{start_time}")]
    );
}

#[test]
fn should_write_a_lease_when_ds_run_is_started_by_a_session_in_a_linked_worktree() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let linked = Project::linked_at(&sandbox.main, &path);
    let shell = sandbox.fake_program(SHELL_PROGRAM);

    let run = linked.run_under_session(
        &shell,
        &["run", "--", "true"],
        &[("CLAUDE_CODE_SESSION_ID", "sess_42")],
    );

    assert_eq!(run.status, 0, "{}", run.stderr);
    let names = sandbox.lease_names(&path);
    assert_eq!(names.len(), 1, "{names:?}");
    let content = std::fs::read_to_string(sandbox.lease_folder(&path).join(&names[0])).unwrap();
    assert_eq!(content, "sess_42");
}

#[test]
fn should_write_no_lease_when_ds_run_is_started_by_a_session_in_the_main_checkout() {
    let sandbox = sandbox();
    let shell = sandbox.fake_program(SHELL_PROGRAM);

    let run = sandbox
        .main
        .run_under_session(&shell, &["run", "--", "true"], &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        !sandbox
            .main
            .machine()
            .state_directory()
            .join("leases")
            .exists()
    );
}
