#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `ds hook <event>` end to end, as the Claude Code plugin runs them: JSON on
//! stdin, real git in temp folders, the fake docker, and a pretend machine
//! whose config points the worktrees folder into a temp folder.

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use support::{FAKE_ENV, Machine, Project, Run, TEST_SLOTS};

const BLOCK_START: &str = "# >>> devslot";

struct Sandbox {
    main: Project,
    root: tempfile::TempDir,
}

fn sandbox() -> Sandbox {
    let main = Project::from_scenario("template");
    main.initialize_git_and_commit(".env\n");
    let root = tempfile::tempdir().unwrap();
    write_worktrees_config(main.machine(), root.path(), "");
    Sandbox { main, root }
}

fn write_worktrees_config(machine: &Machine, root: &Path, extra: &str) {
    machine.write_global_config(&format!(
        "{TEST_SLOTS}{extra}[worktrees]\nroot = \"{}\"\n",
        root.display()
    ));
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

fn fixtures_with_container_name() -> tempfile::TempDir {
    let fixtures = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/template/docker");
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let text = std::fs::read_to_string(entry.path()).unwrap().replace(
            "\"restart\": \"always\"",
            "\"restart\": \"always\", \"container_name\": \"fixed\"",
        );
        std::fs::write(fixtures.path().join(entry.file_name()), text).unwrap();
    }
    fixtures
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

    fn main_branch(&self) -> String {
        git_output(&self.main_path(), &["symbolic-ref", "--short", "HEAD"])
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

    fn create(&self, name: &str, environment: &[(&str, &str)]) -> Run {
        let input = input_for(
            "WorktreeCreate",
            &json!({"cwd": self.main_path(), "name": name}),
        );
        self.main.run_hook("worktree-create", &input, environment)
    }

    fn remove(&self, path: &Path, environment: &[(&str, &str)]) -> Run {
        let input = input_for("WorktreeRemove", &json!({"worktree_path": path}));
        self.main.run_hook("worktree-remove", &input, environment)
    }

    fn machine(&self) -> &Machine {
        self.main.machine()
    }

    fn created(&self, name: &str) -> PathBuf {
        let run = self.create(name, &[]);
        assert_eq!(run.status, 0, "{}", run.stderr);
        self.expected_path(name)
    }
}

fn first_instance_name(machine: &Machine) -> String {
    machine.instance_names().remove(0)
}

#[test]
fn should_create_the_worktree_on_a_new_branch_and_print_only_its_path_when_creating() {
    let sandbox = sandbox();

    let run = sandbox.create("feature-x", &[]);

    let path = sandbox.expected_path("feature-x");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stdout, format!("{}\n", path.display()));
    assert!(path.join("dev.compose.yml").exists());
    assert_eq!(
        git_output(&path, &["symbolic-ref", "--short", "HEAD"]),
        "feature-x"
    );
    assert_eq!(
        git_output(&path, &["rev-parse", "HEAD"]),
        git_output(&sandbox.main_path(), &["rev-parse", "HEAD"])
    );
}

#[test]
fn should_register_the_worktree_and_copy_the_env_with_the_block_when_creating() {
    let sandbox = sandbox();

    let run = sandbox.create("feature-x", &[]);

    let path = sandbox.expected_path("feature-x");
    let text = std::fs::read_to_string(path.join(".env")).unwrap();
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(text.starts_with(FAKE_ENV));
    assert!(text.contains(BLOCK_START));
    assert!(text.contains("COMPOSE_PROJECT_NAME="));
    assert!(
        sandbox
            .machine()
            .read_registry()
            .contains(path.to_str().unwrap())
    );
    assert_eq!(sandbox.machine().instance_names().len(), 1);
    assert!(run.stderr.contains("ds: registered "));
    assert!(
        !sandbox
            .machine()
            .docker_log()
            .iter()
            .any(|line| line.contains(" up "))
    );
}

#[test]
fn should_branch_from_the_branch_of_the_main_worktree_when_the_main_worktree_is_not_on_the_first_branch()
 {
    let sandbox = sandbox();
    sandbox.main.git(&["checkout", "-q", "-b", "develop"]);
    commit_in(&sandbox.main_path());

    let run = sandbox.create("feature-x", &[]);

    let path = sandbox.expected_path("feature-x");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        git_output(&path, &["rev-parse", "HEAD"]),
        git_output(&sandbox.main_path(), &["rev-parse", "develop"])
    );
}

#[test]
fn should_reuse_the_branch_when_a_local_branch_has_the_name() {
    let sandbox = sandbox();
    sandbox.main.git(&["branch", "feature-y"]);

    let run = sandbox.create("feature-y", &[]);

    let path = sandbox.expected_path("feature-y");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        git_output(&path, &["symbolic-ref", "--short", "HEAD"]),
        "feature-y"
    );
}

#[test]
fn should_exit_2_and_create_nothing_when_the_name_is_invalid() {
    let sandbox = sandbox();

    for name in ["../escape", ".hidden", "-flag", "a b", "a/b", "a..b", ""] {
        let run = sandbox.create(name, &[]);

        assert_eq!(run.status, 2, "{name:?}: {}", run.stderr);
        assert_eq!(run.stdout, "", "{name:?}");
        assert!(run.stderr.contains("is not valid"), "{}", run.stderr);
    }
    assert_eq!(std::fs::read_dir(sandbox.root.path()).unwrap().count(), 0);
    assert!(!sandbox.machine().registry_path().exists());
}

#[test]
fn should_exit_2_when_the_name_is_longer_than_100_characters() {
    let sandbox = sandbox();

    let run = sandbox.create(&"a".repeat(101), &[]);

    assert_eq!(run.status, 2, "{}", run.stderr);
    assert_eq!(std::fs::read_dir(sandbox.root.path()).unwrap().count(), 0);
}

#[test]
fn should_exit_2_when_the_folder_is_not_in_a_git_repository() {
    let sandbox = sandbox();
    let outside = tempfile::tempdir().unwrap();
    let input = input_for(
        "WorktreeCreate",
        &json!({"cwd": outside.path(), "name": "feature-x"}),
    );

    let run = sandbox.main.run_hook("worktree-create", &input, &[]);

    assert_eq!(run.status, 2, "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("not inside a git repository"));
}

#[test]
fn should_reopen_the_worktree_when_the_same_name_is_created_again() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let registry = sandbox.machine().read_registry();
    sandbox.machine().clear_docker_log();

    let run = sandbox.create("feature-x", &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stdout, format!("{}\n", path.display()));
    assert_eq!(sandbox.machine().instance_names().len(), 1);
    assert_eq!(sandbox.machine().read_registry(), registry);
    assert_eq!(sandbox.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_recreate_a_missing_env_when_reopening_a_registered_worktree() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    std::fs::remove_file(path.join(".env")).unwrap();

    let run = sandbox.create("feature-x", &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        std::fs::read_to_string(path.join(".env"))
            .unwrap()
            .contains(BLOCK_START)
    );
}

#[test]
fn should_exit_2_and_leave_a_plain_folder_untouched_when_it_is_at_the_target() {
    let sandbox = sandbox();
    let target = sandbox.expected_path("feature-x");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("keep.txt"), "mine").unwrap();

    let run = sandbox.create("feature-x", &[]);

    assert_eq!(run.status, 2, "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("already exists"));
    assert_eq!(
        std::fs::read_to_string(target.join("keep.txt")).unwrap(),
        "mine"
    );
    assert!(!target.join(".git").exists());
}

#[test]
fn should_exit_1_with_nothing_on_stdout_when_git_refuses_to_create_the_worktree() {
    let sandbox = sandbox();
    let checked_out_in_main = sandbox.main_branch();

    let run = sandbox.create(&checked_out_in_main, &[]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("git worktree add failed"));
}

#[test]
fn should_create_the_worktree_and_warn_when_the_registration_is_refused() {
    let sandbox = sandbox();
    let fixtures = fixtures_with_container_name();
    let environment = [("DOCKER_FAKE_FIXTURES", fixtures.path().to_str().unwrap())];

    let run = sandbox.create("feature-x", &environment);

    let path = sandbox.expected_path("feature-x");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stdout, format!("{}\n", path.display()));
    assert!(path.exists());
    assert!(run.stderr.contains("ds: warning: "));
    assert!(run.stderr.contains("container_name"));
    assert!(!sandbox.machine().registry_path().exists());
}

#[test]
fn should_create_the_worktree_without_registering_it_when_it_is_outside_the_auto_scope() {
    let sandbox = sandbox();
    let excluded = format!(
        "[auto]\nexclude = [\"{}\"]\n",
        sandbox.root.path().display()
    );
    write_worktrees_config(sandbox.machine(), sandbox.root.path(), &excluded);

    let run = sandbox.create("feature-x", &[]);

    let path = sandbox.expected_path("feature-x");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(path.exists());
    assert!(!path.join(".env").exists());
    assert!(!sandbox.machine().registry_path().exists());
}

#[test]
fn should_delete_the_data_by_label_forget_the_instance_and_remove_the_worktree_and_merged_branch() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    let name = first_instance_name(sandbox.machine());
    let containers = sandbox
        .machine()
        .write_scratch("containers.txt", "abc123\n");
    sandbox.machine().clear_docker_log();

    let run = sandbox.remove(
        &path,
        &[("DOCKER_FAKE_CONTAINERS", containers.to_str().unwrap())],
    );

    let log = sandbox.machine().docker_log().join("\n");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(log.contains(&format!(
        "ps -aq --filter label=com.docker.compose.project={name}"
    )));
    assert!(log.contains("rm -f -v abc123"));
    assert!(log.contains(&format!(
        "volume ls -q --filter label=com.docker.compose.project={name}"
    )));
    assert!(log.contains(&format!(
        "network ls -q --filter label=com.docker.compose.project={name}"
    )));
    assert!(!path.exists());
    assert_eq!(
        git_output(&sandbox.main_path(), &["branch", "--list", "feature-x"]),
        ""
    );
    assert_eq!(sandbox.machine().instance_names(), Vec::<String>::new());
}

#[test]
fn should_not_opt_the_checkout_out_when_the_worktree_is_removed() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");

    let run = sandbox.remove(&path, &[]);

    let registry = sandbox.machine().read_registry();
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(!registry.contains(path.to_str().unwrap()));
    assert!(!registry.contains("opted_out"));
}

#[test]
fn should_keep_the_branch_and_say_so_when_it_is_not_merged() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    commit_in(&path);

    let run = sandbox.remove(&path, &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(!path.exists());
    assert!(run.stderr.contains(&format!(
        "ds: kept branch feature-x: not merged into {}",
        sandbox.main_branch()
    )));
    assert_eq!(
        git_output(&sandbox.main_path(), &["branch", "--list", "feature-x"]),
        "feature-x"
    );
}

#[test]
fn should_keep_the_entry_and_still_remove_the_worktree_when_the_label_deletion_fails() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");

    let run = sandbox.remove(&path, &[("DOCKER_FAKE_FAIL_COMMANDS", "docker_ps")]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stderr.contains("ds: warning: could not delete"));
    assert!(!path.exists());
    assert_eq!(sandbox.machine().instance_names().len(), 1);
    assert!(
        sandbox
            .machine()
            .read_registry()
            .contains(path.to_str().unwrap())
    );
}

#[test]
fn should_keep_the_data_and_the_entry_when_git_refuses_to_remove_a_locked_worktree() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    sandbox
        .main
        .git(&["worktree", "lock", path.to_str().unwrap()]);
    sandbox.machine().clear_docker_log();

    let run = sandbox.remove(&path, &[]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains(&format!(
        "ds: {} is locked (git worktree unlock first)",
        path.display()
    )));
    assert!(path.exists());
    assert_eq!(sandbox.machine().instance_names().len(), 1);
    assert_eq!(sandbox.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_go_on_and_warn_when_git_forgets_the_worktree_but_leaves_its_folder() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    std::fs::write(sandbox.main_path().join(".git/info/exclude"), "blocked/\n").unwrap();
    let blocked = path.join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    std::fs::write(blocked.join("file"), "x").unwrap();
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o555)).unwrap();
    sandbox.machine().clear_docker_log();

    let run = sandbox.remove(&path, &[]);

    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stderr.contains(&format!(
        "ds: warning: git left {} on disk; remove it by hand",
        path.display()
    )));
    assert!(path.exists());
    assert!(
        !git_output(&sandbox.main_path(), &["worktree", "list", "--porcelain"])
            .contains(path.to_str().unwrap())
    );
    assert!(sandbox.machine().docker_log().join("\n").contains("ps -aq"));
    assert_eq!(sandbox.machine().instance_names(), Vec::<String>::new());
}

#[test]
fn should_keep_a_branch_that_is_merged_only_into_its_upstream() {
    let sandbox = sandbox();
    let remote = tempfile::tempdir().unwrap();
    git_output(remote.path(), &["init", "-q", "--bare"]);
    sandbox
        .main
        .git(&["remote", "add", "origin", remote.path().to_str().unwrap()]);
    let path = sandbox.created("feature-x");
    commit_in(&path);
    git_output(&path, &["push", "-q", "-u", "origin", "feature-x"]);

    let run = sandbox.remove(&path, &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("ds: kept branch feature-x: not merged into ")
    );
    assert_eq!(
        git_output(&sandbox.main_path(), &["branch", "--list", "feature-x"]),
        "feature-x"
    );
}

#[test]
fn should_keep_a_branch_that_is_not_the_one_of_the_worktree() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    git_output(&path, &["switch", "-q", "-c", "other"]);

    let run = sandbox.remove(&path, &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("ds: kept branch other: not the branch of this worktree")
    );
    assert_eq!(
        git_output(&sandbox.main_path(), &["branch", "--list", "other"]),
        "other"
    );
}

#[test]
fn should_exit_0_when_the_pre_tool_use_input_never_ends() {
    let project = git_project();

    let run = project.run_hook_with_open_stdin("pre-tool-use");

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stdout
            .contains("the devslot hook could not read its input")
    );
    assert!(!run.stdout.contains("permissionDecision"));
}

#[test]
fn should_exit_2_when_the_session_start_input_never_ends() {
    let project = git_project();

    let run = project.run_hook_with_open_stdin("session-start");

    assert_eq!(run.status, 2, "{}", run.stderr);
}

#[test]
fn should_exit_0_and_touch_nothing_when_the_worktree_path_does_not_exist() {
    let sandbox = sandbox();
    sandbox.machine().clear_docker_log();

    let run = sandbox.remove(&sandbox.root.path().join("missing"), &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert_eq!(sandbox.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_exit_1_and_remove_nothing_when_the_path_is_the_main_worktree() {
    let sandbox = sandbox();
    sandbox.main.run(&["run", "--", "true"]);
    sandbox.machine().clear_docker_log();

    let run = sandbox.remove(&sandbox.main_path(), &[]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(sandbox.main_path().join("dev.compose.yml").exists());
    assert_eq!(sandbox.machine().instance_names().len(), 1);
    assert_eq!(sandbox.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_exit_1_and_remove_nothing_when_the_path_is_a_folder_inside_a_worktree() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    std::fs::create_dir(path.join("inside")).unwrap();

    let run = sandbox.remove(&path.join("inside"), &[]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(path.join("inside").exists());
    assert_eq!(sandbox.machine().instance_names().len(), 1);
}

fn pre_tool_use(project: &Project, command: Option<&str>, environment: &[(&str, &str)]) -> Run {
    let fields = match command {
        Some(command) => json!({
            "cwd": project.path(),
            "tool_name": "Bash",
            "tool_input": {"command": command},
        }),
        None => json!({"cwd": project.path(), "tool_name": "Read", "tool_input": {}}),
    };
    project.run_hook(
        "pre-tool-use",
        &input_for("PreToolUse", &fields),
        environment,
    )
}

fn git_project() -> Project {
    let project = Project::from_scenario("template");
    project.initialize_git(".env\n");
    project
}

fn answer_of(run: &Run) -> Value {
    serde_json::from_str(&run.stdout).unwrap()
}

#[test]
fn should_deny_the_command_when_it_runs_ds_rm_ds_prune_ds_worktree_rm_or_a_hook() {
    let project = git_project();
    for command in [
        "ds rm",
        "ds rm --yes",
        "ds prune",
        "cd x && ds prune --yes",
        "/usr/local/bin/ds rm",
        "ds 'rm'",
        "ds run -- ds rm --yes",
        "timeout 5 ds prune",
        "ds worktree rm feature-x --yes",
        "ds hook worktree-remove",
    ] {
        let run = pre_tool_use(&project, Some(command), &[]);

        let output = &answer_of(&run)["hookSpecificOutput"];
        assert_eq!(run.status, 0, "{command}: {}", run.stderr);
        assert_eq!(output["hookEventName"], "PreToolUse", "{command}");
        assert_eq!(output["permissionDecision"], "deny", "{command}");
        assert_eq!(
            output["permissionDecisionReason"],
            "ds rm, ds prune and ds worktree rm delete data, ds hook is run by Claude Code itself, and other agents work on this machine; ask the user to run it",
            "{command}"
        );
    }
    assert!(!project.machine().registry_path().exists());
}

#[test]
fn should_not_deny_the_commands_that_only_look_like_ds_rm() {
    let project = git_project();
    for command in [
        "ds reset",
        "ds run -- rm -rf build",
        "dsrm",
        "ds rmx",
        "ls",
        "ds worktree --help",
    ] {
        let run = pre_tool_use(&project, Some(command), &[]);

        assert_eq!(run.status, 0, "{command}: {}", run.stderr);
        assert!(!run.stdout.contains("permissionDecision"), "{command}");
    }
}

#[test]
fn should_give_the_summary_on_the_first_call_and_nothing_on_the_second() {
    let project = git_project();

    let first = pre_tool_use(&project, Some("ls"), &[]);
    let second = pre_tool_use(&project, Some("ls"), &[]);

    let output = &answer_of(&first)["hookSpecificOutput"];
    let context = output["additionalContext"].as_str().unwrap();
    assert_eq!(first.status, 0, "{}", first.stderr);
    assert_eq!(output["hookEventName"], "PreToolUse");
    assert!(context.contains("ds: registered "));
    assert!(context.contains("devslot: this checkout is "));
    assert!(context.contains(", slot "));
    assert!(context.contains("DB_PORT="));
    assert!(context.contains("(postgres)"));
    assert!(context.ends_with("run ds info --json for URLs"));
    assert_eq!((second.status, second.stdout.as_str()), (0, ""));
}

#[test]
fn should_run_ensure_when_the_tool_has_no_command() {
    let project = git_project();

    let run = pre_tool_use(&project, None, &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stdout.contains("devslot: this checkout is "));
}

#[test]
fn should_report_a_refusal_once_as_context_when_the_checkout_cannot_be_registered() {
    let project = git_project();
    let fixtures = fixtures_with_container_name();
    let environment = [("DOCKER_FAKE_FIXTURES", fixtures.path().to_str().unwrap())];

    let first = pre_tool_use(&project, Some("ls"), &environment);
    let second = pre_tool_use(&project, Some("ls"), &environment);

    let context = answer_of(&first)["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(first.status, 0, "{}", first.stderr);
    assert!(context.starts_with("devslot could not register this checkout: "));
    assert!(context.contains("container_name"));
    assert_eq!((second.status, second.stdout.as_str()), (0, ""));
}

#[test]
fn should_turn_a_tool_failure_into_context_and_exit_0() {
    let project = git_project();
    project.machine().write_global_config("[slots\n");

    let run = pre_tool_use(&project, Some("ls"), &[]);

    let context = answer_of(&run)["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(context.starts_with("devslot ensure failed: "));
}

#[test]
fn should_exit_0_with_the_context_and_no_decision_when_the_input_is_not_json() {
    let project = git_project();

    for input in ["not json", "", "{}", "[1]"] {
        let run = project.run_hook("pre-tool-use", input, &[]);

        assert_eq!(run.status, 0, "{input:?}: {}", run.stderr);
        assert!(
            run.stdout
                .contains("the devslot hook could not read its input ("),
            "{input:?}"
        );
        assert!(!run.stdout.contains("permissionDecision"), "{input:?}");
    }
}

fn session_start(project: &Project) -> Run {
    let input = input_for("SessionStart", &json!({"cwd": project.path()}));
    project.run_hook("session-start", &input, &[])
}

#[test]
fn should_print_the_info_and_the_guidance_when_the_checkout_is_registered() {
    let project = git_project();
    project.run(&["run", "--", "true"]);
    let name = first_instance_name(project.machine());
    project.machine().clear_docker_log();

    let run = session_start(&project);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stdout.contains(&format!("Name:          {name}\n")));
    assert!(run.stdout.contains("Services:      run ds info to check\n"));
    assert!(run.stdout.contains("DB_PORT"));
    assert!(run.stdout.ends_with(
        "Use ds info --json for ports and URLs; ds up --no-run starts the services (plain ds up then runs the app in the foreground and does not return); ds reset resets the data; never run ds rm, ds prune, ds worktree rm or delete containers or volumes by hand. When the devslot worktree guard is on, enter a worktree with the EnterWorktree tool before writing code or starting an environment, and run ds worktree land only after the user's explicit go-ahead (fast-forward only: when the base moved, git rebase it in the worktree, resolve, commit, run ds worktree land again; never push).\n"
    ));
    assert_eq!(project.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_print_nothing_when_the_checkout_is_not_registered() {
    let project = git_project();

    let run = session_start(&project);

    assert_eq!((run.status, run.stdout.as_str()), (0, ""));
}

#[test]
fn should_exit_2_when_the_session_start_input_is_invalid() {
    let project = git_project();

    let run = project.run_hook("session-start", "{}", &[]);

    assert_eq!(run.status, 2, "{}", run.stderr);
    assert_eq!(run.stdout, "");
}

#[test]
fn should_exit_2_when_the_worktree_hook_input_is_invalid() {
    let project = git_project();

    let create = project.run_hook("worktree-create", "{\"cwd\": \"/tmp\"}", &[]);
    let remove = project.run_hook("worktree-remove", "not json", &[]);

    assert_eq!(create.status, 2, "{}", create.stderr);
    assert_eq!(remove.status, 2, "{}", remove.stderr);
}

const HOOK_BUDGET_CEILING: Duration = Duration::from_secs(19);
const SLOW_PROBE_SECONDS: &str = "3.0";

fn hold_registry_lock(machine: &Machine) -> std::fs::File {
    let path = machine.lock_path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .unwrap();
    file.lock().unwrap();
    file
}

fn timed_pre_tool_use(project: &Project, environment: &[(&str, &str)]) -> (Run, Duration) {
    let started = Instant::now();
    let run = pre_tool_use(project, Some("ls"), environment);
    (run, started.elapsed())
}

#[test]
fn should_exit_0_silently_and_stay_silent_when_the_registry_lock_stays_taken() {
    let project = git_project();
    project.run(&["run", "--", "true"]);
    project.write(".env", FAKE_ENV);
    let lock = hold_registry_lock(project.machine());

    let (first, elapsed) = timed_pre_tool_use(&project, &[]);
    drop(lock);
    let second = pre_tool_use(&project, Some("ls"), &[]);

    assert_eq!(
        (first.status, first.stdout.as_str()),
        (0, ""),
        "{}",
        first.stderr
    );
    assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");
    assert_eq!(
        (second.status, second.stdout.as_str()),
        (0, ""),
        "{}",
        second.stderr
    );
    assert!(!project.read(".env").contains(BLOCK_START));
}

#[test]
fn should_exit_0_silently_under_the_kill_delay_when_a_slow_docker_and_a_taken_lock_use_the_budget()
{
    let project = git_project();
    let slow = [("DOCKER_FAKE_SLEEP", SLOW_PROBE_SECONDS)];
    let lock = hold_registry_lock(project.machine());

    let (first, elapsed) = timed_pre_tool_use(&project, &slow);
    drop(lock);
    let second = pre_tool_use(&project, Some("ls"), &[]);

    assert_eq!(
        (first.status, first.stdout.as_str()),
        (0, ""),
        "{}",
        first.stderr
    );
    assert!(elapsed < HOOK_BUDGET_CEILING, "{elapsed:?}");
    assert_eq!(
        (second.status, second.stdout.as_str()),
        (0, ""),
        "{}",
        second.stderr
    );
    assert_eq!(project.machine().instance_names(), Vec::<String>::new());
}

fn ls_output(sandbox: &Sandbox) -> String {
    sandbox.main.run(&["ls"]).stdout
}

#[test]
fn should_refuse_and_keep_everything_when_the_worktree_has_uncommitted_changes() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    std::fs::write(path.join("notes.txt"), "unsaved").unwrap();
    sandbox.machine().clear_docker_log();

    let run = sandbox.remove(&path, &[]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("1 uncommitted change(s) and 0 commit(s) found nowhere else")
    );
    assert!(run.stderr.contains(&format!(
        "ds worktree rm {} from a terminal",
        path.display()
    )));
    assert!(!run.stderr.contains("--force"));
    assert!(path.join("notes.txt").exists());
    assert_eq!(sandbox.machine().instance_names().len(), 1);
    assert_eq!(sandbox.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_refuse_when_a_detached_head_holds_a_commit_found_nowhere_else() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    git_output(&path, &["checkout", "-q", "--detach"]);
    commit_in(&path);
    sandbox.machine().clear_docker_log();

    let run = sandbox.remove(&path, &[]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("0 uncommitted change(s) and 1 commit(s) found nowhere else")
    );
    assert!(path.exists());
    assert_eq!(sandbox.machine().instance_names().len(), 1);
    assert_eq!(sandbox.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_list_the_unsaved_work_reason_in_ds_ls_when_the_hook_refuses() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    std::fs::write(path.join("notes.txt"), "unsaved").unwrap();

    sandbox.remove(&path, &[]);

    let listing = ls_output(&sandbox);
    assert!(listing.contains(&format!("{}: it holds unsaved work", path.display())));
}

#[test]
fn should_count_the_kept_worktrees_of_the_repository_in_the_session_start_line() {
    let sandbox = sandbox();
    let path = sandbox.created("feature-x");
    std::fs::write(path.join("notes.txt"), "unsaved").unwrap();
    sandbox.remove(&path, &[]);
    sandbox.machine().write_global_config(&format!(
        "{TEST_SLOTS}[worktrees]\nroot = \"{}\"\nrequire = [\"{}\"]\n",
        sandbox.root.path().display(),
        sandbox.main_path().display()
    ));

    let run = session_start(&sandbox.main);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stdout
            .contains("Kept worktrees of this repository: 1 (ds ls lists them).")
    );
}

#[test]
fn should_record_the_base_branch_in_the_git_config_when_creating_a_new_branch() {
    let sandbox = sandbox();

    let run = sandbox.create("feature-x", &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        git_output(
            &sandbox.main_path(),
            &["config", "--get", "branch.feature-x.devslotBase"]
        ),
        sandbox.main_branch()
    );
}

#[test]
fn should_not_record_a_base_when_the_main_worktree_is_detached() {
    let sandbox = sandbox();
    sandbox.main.git(&["checkout", "-q", "--detach"]);

    let run = sandbox.create("feature-x", &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        git_output(
            &sandbox.main_path(),
            &["config", "--get", "branch.feature-x.devslotBase"]
        ),
        ""
    );
}

#[test]
fn should_announce_the_reused_branch_and_print_only_the_path_when_the_branch_has_commits() {
    let sandbox = sandbox();
    let base = sandbox.main_branch();
    sandbox.main.git(&["checkout", "-q", "-b", "feature-y"]);
    commit_in(&sandbox.main_path());
    commit_in(&sandbox.main_path());
    sandbox.main.git(&["checkout", "-q", &base]);

    let run = sandbox.create("feature-y", &[]);

    let path = sandbox.expected_path("feature-y");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stdout, format!("{}\n", path.display()));
    assert!(
        run.stderr.contains(&format!(
            "ds: reusing the existing branch feature-y: 2 commit(s) not in {base}"
        )),
        "{}",
        run.stderr
    );
}
