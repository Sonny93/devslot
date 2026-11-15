#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `ds worktree rm` end to end: real git in temp folders, the fake docker, a
//! pretend machine whose config points the worktrees folder into a temp
//! folder, and real child processes that the tests spawn themselves.

mod support;

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;
use support::{Project, Run, TEST_SLOTS};

const TERMINATED: i32 = 15;
const WAIT_LIMIT: Duration = Duration::from_secs(10);

struct Sandbox {
    main: Project,
    root: tempfile::TempDir,
}

fn sandbox() -> Sandbox {
    let main = Project::from_scenario("template");
    main.initialize_git_and_commit(".env\n");
    let root = tempfile::tempdir().unwrap();
    main.machine().write_global_config(&format!(
        "{TEST_SLOTS}[worktrees]\nroot = \"{}\"\n",
        root.path().display()
    ));
    Sandbox { main, root }
}

fn git_output(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn commit_in(directory: &Path) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--no-verify",
            "--allow-empty",
            "-m",
            "work",
        ])
        .current_dir(directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());
}

impl Sandbox {
    fn main_path(&self) -> PathBuf {
        self.main.path().canonicalize().unwrap()
    }

    fn expected_path(&self, name: &str) -> PathBuf {
        let repository = self.main_path().file_name().unwrap().to_owned();
        self.root
            .path()
            .canonicalize()
            .unwrap()
            .join(repository)
            .join(name)
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
        self.expected_path(name)
    }

    fn remove(&self, arguments: &[&str]) -> Run {
        let mut command = vec!["worktree", "rm"];
        command.extend(arguments);
        self.main.run(&command)
    }

    fn instance_name_of(&self, path: &Path) -> String {
        let registry = self.main.machine().read_registry();
        let table = registry
            .split("[[instance]]")
            .find(|table| table.contains(path.to_str().unwrap()))
            .unwrap();
        table
            .lines()
            .find_map(|line| line.trim().strip_prefix("name = \""))
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap()
            .to_owned()
    }
}

fn spawn_sleeper(directory: &Path, instance: Option<&str>) -> Child {
    let mut command = Command::new("sleep");
    command
        .arg("60")
        .current_dir(directory)
        .env_remove("DEVSLOT_INSTANCE")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(instance) = instance {
        command.env("DEVSLOT_INSTANCE", instance);
    }
    command.spawn().unwrap()
}

fn wait_for_exit(child: &mut Child) -> Option<std::process::ExitStatus> {
    let started = Instant::now();
    while started.elapsed() < WAIT_LIMIT {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

#[test]
fn should_refuse_without_a_terminal_and_without_yes_and_change_nothing() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    sandbox.main.machine().clear_docker_log();

    let run = sandbox.remove(&["feature-x"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("ds: error: ds worktree rm deletes data; run it in a terminal or pass --yes")
    );
    assert!(path.exists());
    assert_eq!(sandbox.main.machine().instance_names().len(), 1);
    assert_eq!(sandbox.main.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_show_what_would_be_removed_before_asking() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let name = sandbox.instance_name_of(&path);

    let run = sandbox.remove(&["feature-x"]);

    assert!(
        run.stdout
            .contains(&format!("worktree:             {}", path.display()))
    );
    assert!(run.stdout.contains("branch:               feature-x"));
    assert!(
        run.stdout
            .contains(&format!("instance:             {name}"))
    );
    assert!(run.stdout.contains("uncommitted changes:  0"));
    assert!(run.stdout.contains("commits found nowhere else: 0"));
    assert!(
        run.stdout
            .contains("ignored files (.env, local databases, uploads) are deleted too")
    );
}

#[test]
fn should_refuse_a_worktree_with_uncommitted_changes_without_force() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    std::fs::write(path.join("notes.txt"), "work in progress").unwrap();
    sandbox.main.machine().clear_docker_log();

    let run = sandbox.remove(&["feature-x", "--yes"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stdout.contains("uncommitted changes:  1"));
    assert!(run.stderr.contains("1 uncommitted change(s)"));
    assert!(run.stderr.contains("pass --force"));
    assert!(path.join("notes.txt").exists());
    assert_eq!(sandbox.main.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_remove_a_worktree_with_uncommitted_changes_when_forced() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    std::fs::write(path.join("notes.txt"), "work in progress").unwrap();

    let run = sandbox.remove(&["feature-x", "--yes", "--force"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(!path.exists());
}

#[test]
fn should_refuse_a_detached_head_with_commits_found_nowhere_else_without_force() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    git_output(&path, &["switch", "-q", "--detach"]);
    commit_in(&path);

    let run = sandbox.remove(&["feature-x", "--yes"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stdout.contains("commits found nowhere else: 1"));
    assert!(run.stderr.contains("1 commit(s) found nowhere else"));
    assert!(path.exists());
}

#[test]
fn should_not_count_the_commits_of_a_branch_that_is_kept() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    commit_in(&path);

    let run = sandbox.remove(&["feature-x", "--yes"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stdout.contains("commits found nowhere else: 0"));
    assert_eq!(
        git_output(&sandbox.main_path(), &["branch", "--list", "feature-x"]),
        "feature-x"
    );
}

#[test]
fn should_remove_a_clean_worktree_by_name_and_keep_the_neighbour_instance() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let neighbour = sandbox.created("feature-y");
    let name = sandbox.instance_name_of(&path);
    let neighbour_name = sandbox.instance_name_of(&neighbour);
    let containers = sandbox
        .main
        .machine()
        .write_scratch("containers.txt", "abc123\n");
    sandbox.main.machine().clear_docker_log();

    let run = sandbox.main.run_with(
        &["worktree", "rm", "feature-x", "--yes"],
        &[("DOCKER_FAKE_CONTAINERS", containers.to_str().unwrap())],
    );

    let log = sandbox.main.machine().docker_log().join("\n");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(log.contains(&format!(
        "ps -aq --filter label=com.docker.compose.project={name}"
    )));
    assert!(log.contains("rm -f -v abc123"));
    assert!(!log.contains(&neighbour_name));
    assert!(!path.exists());
    assert!(neighbour.exists());
    assert_eq!(
        sandbox.main.machine().instance_names(),
        vec![neighbour_name]
    );
    assert_eq!(
        git_output(&sandbox.main_path(), &["branch", "--list", "feature-x"]),
        ""
    );
}

#[test]
fn should_remove_a_worktree_given_by_path() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");

    let run = sandbox.remove(&[path.to_str().unwrap(), "--yes"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(!path.exists());
    assert_eq!(
        sandbox.main.machine().instance_names(),
        Vec::<String>::new()
    );
}

#[test]
fn should_refuse_a_locked_worktree_and_call_neither_docker_nor_the_registry() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    sandbox
        .main
        .git(&["worktree", "lock", path.to_str().unwrap()]);
    let registry = sandbox.main.machine().read_registry();
    sandbox.main.machine().clear_docker_log();

    let run = sandbox.remove(&["feature-x", "--yes", "--force"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains(&format!(
        "ds: {} is locked (git worktree unlock first)",
        path.display()
    )));
    assert!(path.exists());
    assert_eq!(sandbox.main.machine().read_registry(), registry);
    assert_eq!(sandbox.main.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_refuse_a_copied_folder_that_git_does_not_list_and_keep_its_instance() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let copy = path.with_file_name("feature-x-copy");
    let copied = Command::new("cp")
        .arg("-a")
        .arg(&path)
        .arg(&copy)
        .status()
        .unwrap();
    assert!(copied.success());
    let ensured = sandbox.main.run_in(&copy, &["ensure"]);
    assert_eq!(ensured.status, 0, "{}", ensured.stderr);
    assert_eq!(sandbox.main.machine().instance_names().len(), 2);
    sandbox.main.machine().clear_docker_log();

    let run = sandbox.remove(&[copy.to_str().unwrap(), "--yes", "--force"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains("is not the root of a linked worktree"));
    assert!(copy.exists());
    assert_eq!(sandbox.main.machine().instance_names().len(), 2);
    assert_eq!(sandbox.main.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_refuse_the_main_checkout_given_by_path() {
    let sandbox = sandbox();
    sandbox.main.run(&["run", "--", "true"]);
    sandbox.main.machine().clear_docker_log();

    let run = sandbox.remove(&[sandbox.main_path().to_str().unwrap(), "--yes", "--force"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains("is not the root of a linked worktree"));
    assert!(sandbox.main_path().join("dev.compose.yml").exists());
    assert_eq!(sandbox.main.machine().instance_names().len(), 1);
    assert_eq!(sandbox.main.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_refuse_a_folder_inside_a_worktree() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    std::fs::create_dir(path.join("inside")).unwrap();

    let run = sandbox.remove(&[path.join("inside").to_str().unwrap(), "--yes"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(path.join("inside").exists());
}

#[test]
fn should_exit_1_when_no_worktree_has_the_name() {
    let sandbox = sandbox();

    let run = sandbox.remove(&["missing", "--yes"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains("does not exist"));
}

#[test]
fn should_exit_2_when_the_name_is_not_valid() {
    let sandbox = sandbox();

    let run = sandbox.remove(&["a..b", "--yes"]);

    assert_eq!(run.status, 2, "{}", run.stderr);
    assert!(run.stderr.contains("is not valid"));
}

#[test]
fn should_stop_the_processes_of_the_instance_and_leave_the_other_ones_running() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let name = sandbox.instance_name_of(&path);
    let mut owned = spawn_sleeper(&path, Some(&name));
    let mut bystander = spawn_sleeper(&path, None);
    let mut foreign = spawn_sleeper(&path, Some("other-instance-1234"));

    let run = sandbox.remove(&["feature-x", "--yes"]);

    let owned_status = wait_for_exit(&mut owned);
    let bystander_alive = bystander.try_wait().unwrap().is_none();
    let foreign_alive = foreign.try_wait().unwrap().is_none();
    let _ = bystander.kill();
    let _ = foreign.kill();
    let _ = owned.kill();
    let _ = bystander.wait();
    let _ = foreign.wait();
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        owned_status.and_then(|status| status.signal()),
        Some(TERMINATED)
    );
    assert!(bystander_alive);
    assert!(foreign_alive);
    assert!(
        run.stderr
            .contains(&format!("ds: stopped {} (sleep)", owned.id()))
    );
    assert!(run.stderr.contains("ds: still running in "));
    assert!(run.stderr.contains(&format!("{} (sleep)", bystander.id())));
    assert!(
        run.stdout
            .contains(&format!("processes to stop:    {} (sleep)", owned.id()))
    );
}
