#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Two sessions of Claude Code (the plugin's hook and the same hook declared in
//! `settings.json`) run the worktree hooks side by side: real git in temp
//! folders, the fake docker, and a lock held by the test to force the overlap.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use devslot::kept_worktrees;
use devslot::worktree_lock::{LockPurpose, WorktreeKey, WorktreeLock};
use serde_json::{Value, json};
use support::{Project, Run, TEST_SLOTS, install_executable};

const BLOCK_START: &str = "# >>> devslot";
const POST_CHECKOUT_SLEEP_SECONDS: u32 = 2;
const IMMEDIATE: Duration = Duration::from_secs(5);

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

fn input_for(event: &str, fields: &Value) -> String {
    let mut input = json!({
        "session_id": "session",
        "transcript_path": "/tmp/transcript.jsonl",
        "hook_event_name": event,
    });
    for (key, value) in fields.as_object().unwrap() {
        input[key] = value.clone();
    }
    input.to_string()
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

    fn state_directory(&self) -> PathBuf {
        self.main.machine().state_directory()
    }

    fn create(&self, name: &str) -> Run {
        let input = input_for(
            "WorktreeCreate",
            &json!({"cwd": self.main_path(), "name": name}),
        );
        self.main.run_hook("worktree-create", &input, &[])
    }

    fn created(&self, name: &str) -> PathBuf {
        let run = self.create(name);
        assert_eq!(run.status, 0, "{}", run.stderr);
        self.expected_path(name)
    }

    fn remove(&self, path: &Path) -> Run {
        let input = input_for("WorktreeRemove", &json!({"worktree_path": path}));
        self.main.run_hook("worktree-remove", &input, &[])
    }

    fn hold_lock(&self, path: &Path) -> WorktreeLock {
        let key = WorktreeKey::of(path).unwrap();
        WorktreeLock::try_acquire(&self.state_directory(), &key, LockPurpose::Remove)
            .unwrap()
            .expect("the lock was free")
    }

    fn kept_entry_count(&self) -> usize {
        std::fs::read_dir(self.state_directory().join("worktrees/kept")).map_or(0, Iterator::count)
    }

    fn install_slow_post_checkout(&self, marker: &Path) {
        let hook = self.main_path().join(".git/hooks/post-checkout");
        install_executable(
            &hook,
            &format!(
                "#!/bin/sh\necho ran >> '{}'\nsleep {POST_CHECKOUT_SLEEP_SECONDS}\n",
                marker.display()
            ),
        );
    }
}

#[test]
fn should_let_the_second_creation_reopen_the_worktree_the_first_one_made() {
    let sandbox = sandbox();
    let marker = sandbox.root.path().join("post-checkout-runs");
    sandbox.install_slow_post_checkout(&marker);

    let (first, second) = std::thread::scope(|scope| {
        let first = scope.spawn(|| sandbox.create("feature-x"));
        let second = scope.spawn(|| sandbox.create("feature-x"));
        (first.join().unwrap(), second.join().unwrap())
    });

    let path = sandbox.expected_path("feature-x");
    let env_text = std::fs::read_to_string(path.join(".env")).unwrap();
    assert_eq!(first.status, 0, "{}", first.stderr);
    assert_eq!(second.status, 0, "{}", second.stderr);
    assert_eq!(first.stdout, format!("{}\n", path.display()));
    assert_eq!(second.stdout, first.stdout);
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
    assert_eq!(
        git_output(&sandbox.main_path(), &["worktree", "list"])
            .lines()
            .count(),
        2
    );
    assert_eq!(
        git_output(
            &sandbox.main_path(),
            &["branch", "--list", "--format=%(refname:short)", "feature-x"]
        ),
        "feature-x"
    );
    assert_eq!(sandbox.main.machine().instance_names().len(), 1);
    assert_eq!(env_text.matches(BLOCK_START).count(), 1);
}

#[test]
fn should_keep_the_worktree_and_note_it_when_another_removal_holds_the_lock() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let lock = sandbox.hold_lock(&path);
    let started = Instant::now();

    let run = sandbox.remove(&path);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(started.elapsed() < IMMEDIATE);
    assert!(path.join(".git").exists());
    assert_eq!(sandbox.main.machine().instance_names().len(), 1);
    assert_eq!(sandbox.kept_entry_count(), 1);
    let listing = sandbox.main.run(&["ls"]);
    assert!(
        listing
            .stdout
            .contains("Worktrees Claude Code asked to remove that are still there:")
    );
    assert!(listing.stdout.contains(path.to_str().unwrap()));
    assert!(listing.stdout.contains("remove with ds worktree rm"));
    drop(lock);
}

#[test]
fn should_remove_the_worktree_and_its_kept_entry_once_the_lock_is_free() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let neighbour = sandbox.created("feature-y");
    let lock = sandbox.hold_lock(&path);
    assert_eq!(sandbox.remove(&path).status, 0);
    drop(lock);

    let run = sandbox.remove(&path);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(!path.exists());
    assert_eq!(sandbox.kept_entry_count(), 0);
    assert!(neighbour.join(".git").exists());
    assert_eq!(sandbox.main.machine().instance_names().len(), 1);
    assert!(
        sandbox
            .main
            .machine()
            .read_registry()
            .contains(neighbour.to_str().unwrap())
    );
    assert!(!sandbox.main.run(&["ls"]).stdout.contains("still there"));
}

#[test]
fn should_note_the_reason_when_the_removal_is_refused() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    sandbox
        .main
        .git(&["worktree", "lock", path.to_str().unwrap()]);

    let run = sandbox.remove(&path);

    assert_eq!(run.status, 1, "{}", run.stderr);
    let listing = sandbox.main.run(&["ls"]);
    assert!(listing.stdout.contains(": locked by git ("));
}

#[test]
fn should_show_when_claude_code_asked_to_remove_the_worktree_in_the_preview() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let key = WorktreeKey::of(&path).unwrap();
    kept_worktrees::record(
        &sandbox.state_directory(),
        &key,
        "in use by 12 (node)",
        std::time::SystemTime::now(),
    )
    .unwrap();

    let preview = sandbox.main.run(&["worktree", "rm", "feature-x"]);

    assert!(
        preview
            .stdout
            .contains("Claude Code asked to remove it on ")
    );
    assert!(preview.stdout.contains(": in use by 12 (node)\n"));
}

#[test]
fn should_delete_the_kept_entry_when_ds_worktree_rm_removes_the_worktree() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let neighbour = sandbox.created("feature-y");
    let moment = std::time::SystemTime::now();
    for kept in [&path, &neighbour] {
        let key = WorktreeKey::of(kept).unwrap();
        kept_worktrees::record(&sandbox.state_directory(), &key, "locked", moment).unwrap();
    }

    let run = sandbox.main.run(&["worktree", "rm", "feature-x", "--yes"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(!path.exists());
    assert_eq!(sandbox.kept_entry_count(), 1);
    assert!(neighbour.join(".git").exists());
}

#[test]
fn should_ignore_and_delete_a_kept_entry_whose_worktree_no_longer_exists() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let key = WorktreeKey::of(&path).unwrap();
    kept_worktrees::record(
        &sandbox.state_directory(),
        &key,
        "locked by git",
        std::time::SystemTime::now(),
    )
    .unwrap();
    std::fs::remove_dir_all(&path).unwrap();
    assert_eq!(sandbox.kept_entry_count(), 1);

    let listing = sandbox.main.run(&["ls"]);

    assert_eq!(listing.status, 0, "{}", listing.stderr);
    assert!(!listing.stdout.contains("still there"));
    assert_eq!(sandbox.kept_entry_count(), 0);
}

#[test]
fn should_leave_the_json_listing_without_the_kept_worktrees() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let lock = sandbox.hold_lock(&path);
    sandbox.remove(&path);

    let listing = sandbox.main.run(&["ls", "--json"]);

    assert_eq!(listing.status, 0, "{}", listing.stderr);
    assert!(!listing.stdout.contains("still there"));
    assert!(serde_json::from_str::<Value>(&listing.stdout).is_ok());
    drop(lock);
}

#[test]
fn should_not_recommend_removing_a_worktree_recreated_at_the_same_path() {
    let sandbox = sandbox();
    let path = sandbox.created("feat");
    let key = WorktreeKey::of(&path).unwrap();
    kept_worktrees::record(
        &sandbox.state_directory(),
        &key,
        "locked by git",
        std::time::SystemTime::now(),
    )
    .unwrap();
    sandbox
        .main
        .git(&["worktree", "remove", "--force", path.to_str().unwrap()]);

    let recreated = sandbox.create("feat");

    assert_eq!(recreated.status, 0, "{}", recreated.stderr);
    assert!(!sandbox.main.run(&["ls"]).stdout.contains("still there"));
    let preview = sandbox.main.run(&["worktree", "rm", "feat"]);
    assert!(!preview.stdout.contains("Claude Code asked"));
    assert_eq!(sandbox.kept_entry_count(), 0);
}
