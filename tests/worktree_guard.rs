#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The worktree guard end to end: `ds hook pre-tool-use` on the write tools and
//! on `ds up`, `ds run` and `ds reset`, `ds ensure`, `ds hook session-start`
//! and `ds doctor`, with real git in temp folders and the fake docker.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use support::{FAKE_ENV, Machine, Project, Run, TEST_SLOTS};

const SESSION_START_GUARD_LINE: &str = "devslot worktree guard is on: this is the main checkout";

fn temp_root() -> PathBuf {
    std::env::temp_dir().canonicalize().unwrap()
}

fn write_require(machine: &Machine, extra: &str) {
    machine.write_global_config(&format!(
        "{TEST_SLOTS}[worktrees]\nrequire = [\"{}/**\"]\n{extra}",
        temp_root().display()
    ));
}

fn guarded_project() -> Project {
    let project = Project::from_scenario("template");
    project.initialize_git_and_commit(".env\n");
    write_require(project.machine(), "");
    project
}

fn linked_worktree(main: &Project) -> Project {
    let worktree = main.add_worktree("feature-x");
    worktree.write(".env", FAKE_ENV);
    worktree
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

fn write_tool(project: &Project, cwd: &Path, tool: &str, tool_input: &Value) -> Run {
    write_tool_with(
        project,
        &json!({"cwd": cwd, "tool_name": tool, "tool_input": tool_input}),
    )
}

fn write_tool_with(project: &Project, fields: &Value) -> Run {
    project.run_hook("pre-tool-use", &input_for("PreToolUse", fields), &[])
}

fn edit(project: &Project, cwd: &Path, file_path: &Path) -> Run {
    write_tool(
        project,
        cwd,
        "Edit",
        &json!({"file_path": file_path, "old_string": "a", "new_string": "b"}),
    )
}

fn bash(project: &Project, cwd: &Path, command: &str) -> Run {
    write_tool(project, cwd, "Bash", &json!({"command": command}))
}

fn output_of(run: &Run) -> Value {
    serde_json::from_str::<Value>(&run.stdout).unwrap()["hookSpecificOutput"].clone()
}

fn denial_reason(run: &Run) -> String {
    let output = output_of(run);
    assert_eq!(output["permissionDecision"], "deny", "{}", run.stdout);
    output["permissionDecisionReason"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn assert_silent(run: &Run) {
    assert_eq!((run.status, run.stdout.as_str()), (0, ""), "{}", run.stderr);
}

#[test]
fn should_deny_edit_write_multi_edit_and_notebook_edit_in_a_covered_main_checkout() {
    let project = guarded_project();
    let file = project.path().join("src/app.rs");

    for (tool, key) in [
        ("Edit", "file_path"),
        ("Write", "file_path"),
        ("MultiEdit", "file_path"),
        ("NotebookEdit", "notebook_path"),
    ] {
        let run = write_tool(&project, project.path(), tool, &json!({ key: file }));

        let reason = denial_reason(&run);
        assert_eq!(output_of(&run)["hookEventName"], "PreToolUse", "{tool}");
        assert!(reason.contains("EnterWorktree"), "{tool}");
        assert!(
            reason.contains("Never work around this through Bash"),
            "{tool}"
        );
    }
}

#[test]
fn should_give_the_enter_worktree_instruction_when_a_main_session_writes_in_a_covered_main_checkout()
 {
    let project = guarded_project();

    let run = edit(&project, project.path(), &project.path().join("a.rs"));

    assert_eq!(
        denial_reason(&run),
        "devslot refuses writes in the main checkout of this repository: work in a linked worktree. Enter one with the EnterWorktree tool, with a short new name that describes the task, then retry the edit there. After claude --continue, reuse the same worktree name. If EnterWorktree is not available, ask the user. Never work around this through Bash."
    );
}

#[test]
fn should_deny_a_file_that_does_not_exist_yet_in_a_folder_that_does_not_exist_either() {
    let project = guarded_project();

    let run = edit(
        &project,
        project.path(),
        &project.path().join("new/deeper/file.rs"),
    );

    denial_reason(&run);
}

#[test]
fn should_deny_a_relative_path_resolved_against_the_cwd() {
    let project = guarded_project();
    std::fs::create_dir_all(project.path().join("src")).unwrap();

    let run = write_tool(
        &project,
        &project.path().join("src"),
        "Write",
        &json!({"file_path": "../new/file.rs", "content": "x"}),
    );

    denial_reason(&run);
}

#[test]
fn should_answer_nothing_when_the_file_is_in_a_linked_worktree() {
    let project = guarded_project();
    let worktree = linked_worktree(&project);

    let run = edit(
        &project,
        worktree.path(),
        &worktree.path().join("src/app.rs"),
    );

    assert_silent(&run);
}

#[test]
fn should_answer_nothing_when_the_file_is_outside_a_repository() {
    let project = guarded_project();
    let folder = tempfile::tempdir().unwrap();

    let run = edit(&project, folder.path(), &folder.path().join("notes.txt"));

    assert_silent(&run);
}

#[test]
fn should_answer_nothing_when_the_file_is_outside_the_required_globs() {
    let project = guarded_project();
    let elsewhere = tempfile::tempdir().unwrap();
    project.machine().write_global_config(&format!(
        "{TEST_SLOTS}[worktrees]\nrequire = [\"{}\"]\n",
        elsewhere.path().canonicalize().unwrap().display()
    ));

    let run = edit(&project, project.path(), &project.path().join("a.rs"));

    assert_silent(&run);
}

#[test]
fn should_answer_nothing_when_require_exclude_covers_the_file() {
    let project = guarded_project();
    write_require(
        project.machine(),
        &format!(
            "require_exclude = [\"{}\"]\n",
            project.path().canonicalize().unwrap().display()
        ),
    );

    let run = edit(&project, project.path(), &project.path().join("a.rs"));

    assert_silent(&run);
}

#[test]
fn should_answer_nothing_and_touch_no_registry_when_require_is_empty() {
    let project = Project::from_scenario("template");
    project.initialize_git_and_commit(".env\n");

    let run = edit(&project, project.path(), &project.path().join("a.rs"));

    assert_silent(&run);
    assert!(!project.machine().registry_path().exists());
}

#[test]
fn should_answer_nothing_when_the_local_file_switches_the_guard_off() {
    let project = guarded_project();
    project.write("devslot.local.toml", "[worktrees]\nrequire = false\n");

    let run = edit(&project, project.path(), &project.path().join("a.rs"));

    assert_silent(&run);
}

#[test]
fn should_still_deny_when_only_the_committed_file_switches_the_guard_off() {
    let project = guarded_project();
    project.write("devslot.toml", "[worktrees]\nrequire = false\n");

    let run = edit(&project, project.path(), &project.path().join("a.rs"));

    denial_reason(&run);
}

#[test]
fn should_answer_nothing_when_the_repository_is_bare() {
    let project = guarded_project();
    let folder = tempfile::tempdir().unwrap();
    let bare = folder.path().canonicalize().unwrap().join("bare.git");
    let status = Command::new("git")
        .args(["init", "-q", "--bare", bare.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());

    let run = edit(&project, &bare, &bare.join("description"));

    assert_silent(&run);
}

#[test]
fn should_give_the_subagent_reason_when_the_call_comes_from_a_subagent() {
    let project = guarded_project();

    let run = write_tool_with(
        &project,
        &json!({
            "cwd": project.path(),
            "tool_name": "Edit",
            "agent_id": "agent-1",
            "tool_input": {"file_path": project.path().join("a.rs")},
        }),
    );

    let reason = denial_reason(&run);
    assert!(reason.contains("Do not call EnterWorktree from a subagent"));
    assert!(reason.contains("the main session must enter a worktree"));
}

#[test]
fn should_give_the_equivalent_path_when_the_session_is_in_a_worktree_of_the_same_repository() {
    let project = guarded_project();
    let worktree = linked_worktree(&project);
    let main_file = project.path().canonicalize().unwrap().join("src/app.rs");

    let run = edit(&project, worktree.path(), &main_file);

    let reason = denial_reason(&run);
    assert!(reason.contains(&format!(
        "at {}",
        worktree.path().join("src/app.rs").display()
    )));
    assert!(!reason.contains("Enter one with the EnterWorktree tool"));
}

#[test]
fn should_give_the_context_and_no_decision_when_the_local_file_cannot_be_parsed() {
    let project = guarded_project();
    project.write("devslot.local.toml", "[worktrees\n");

    let run = edit(&project, project.path(), &project.path().join("a.rs"));

    let output = output_of(&run);
    let context = output["additionalContext"].as_str().unwrap();
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(context.starts_with("the devslot worktree guard could not decide ("));
    assert!(context.ends_with("); tell the user"));
    assert_eq!(output.get("permissionDecision"), None);
}

#[test]
fn should_give_the_context_and_no_decision_when_the_global_config_is_unreadable() {
    let project = guarded_project();
    project
        .machine()
        .write_global_config("[worktrees]\nrequire = [\"/a/[b\"]\n");

    let run = edit(&project, project.path(), &project.path().join("a.rs"));

    let output = output_of(&run);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        output["additionalContext"]
            .as_str()
            .unwrap()
            .contains("could not decide")
    );
    assert_eq!(output.get("permissionDecision"), None);
}

#[test]
fn should_never_register_the_checkout_when_a_write_tool_is_allowed() {
    let project = guarded_project();
    let worktree = linked_worktree(&project);

    edit(&project, worktree.path(), &worktree.path().join("a.rs"));

    assert!(!project.machine().registry_path().exists());
}

#[test]
fn should_deny_ds_up_ds_run_and_ds_reset_in_a_covered_main_checkout() {
    let project = guarded_project();

    for command in [
        "ds up --no-run",
        "ds run -- pnpm dev",
        "ds reset postgres",
        "cd src && ds up",
    ] {
        let run = bash(&project, project.path(), command);

        let reason = denial_reason(&run);
        assert!(reason.contains("ds up, ds run and ds reset"), "{command}");
        assert!(reason.contains("EnterWorktree"), "{command}");
    }
    assert!(!project.machine().registry_path().exists());
}

#[test]
fn should_keep_the_deleting_reason_for_ds_rm_even_in_a_covered_main_checkout() {
    let project = guarded_project();

    let run = bash(&project, project.path(), "ds rm --yes");

    assert!(denial_reason(&run).starts_with("ds rm, ds prune and ds worktree rm delete data"));
}

#[test]
fn should_allow_ds_up_ds_run_and_ds_reset_in_a_linked_worktree() {
    let project = guarded_project();
    let worktree = linked_worktree(&project);

    for command in ["ds up --no-run", "ds run -- pnpm dev", "ds reset"] {
        let run = bash(&project, worktree.path(), command);

        assert!(!run.stdout.contains("permissionDecision"), "{command}");
    }
}

#[test]
fn should_allow_ds_info_and_other_commands_in_a_covered_main_checkout() {
    let project = guarded_project();

    for command in ["ds info --json", "ds doctor", "ls", "git status"] {
        let run = bash(&project, project.path(), command);

        assert_silent(&run);
    }
}

#[test]
fn should_give_the_subagent_reason_for_ds_up_when_the_call_comes_from_a_subagent() {
    let project = guarded_project();

    let run = write_tool_with(
        &project,
        &json!({
            "cwd": project.path(),
            "tool_name": "Bash",
            "agent_id": "agent-1",
            "tool_input": {"command": "ds up"},
        }),
    );

    assert!(denial_reason(&run).contains("Do not call EnterWorktree from a subagent"));
}

#[test]
fn should_not_register_a_covered_main_checkout_when_ensure_runs() {
    let project = guarded_project();

    let run = project.run(&["ensure"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!((run.stdout.as_str(), run.stderr.as_str()), ("", ""));
    assert!(!project.machine().registry_path().exists());
    assert!(!project.read(".env").contains("# >>> devslot"));
}

#[test]
fn should_still_register_a_linked_worktree_when_ensure_runs() {
    let project = guarded_project();
    let worktree = linked_worktree(&project);

    let run = worktree.run(&["ensure"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(worktree.machine().instance_names().len(), 1);
    assert!(worktree.read(".env").contains("# >>> devslot"));
}

#[test]
fn should_leave_a_main_checkout_registered_before_the_guard_untouched_when_ensure_runs() {
    let project = Project::from_scenario("template");
    project.initialize_git_and_commit(".env\n");
    project.run(&["ensure"]);
    assert_eq!(project.machine().instance_names().len(), 1);
    project.write(".env", FAKE_ENV);
    write_require(project.machine(), "");

    let run = project.run(&["ensure"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!((run.stdout.as_str(), run.stderr.as_str()), ("", ""));
    assert_eq!(project.read(".env"), FAKE_ENV);
    assert_eq!(project.machine().instance_names().len(), 1);
}

fn session_start(project: &Project, cwd: &Path) -> Run {
    let input = input_for("SessionStart", &json!({"cwd": cwd}));
    project.run_hook("session-start", &input, &[])
}

#[test]
fn should_print_the_guard_line_when_the_session_starts_in_a_covered_main_checkout() {
    let project = guarded_project();

    let run = session_start(&project, project.path());

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stdout.starts_with(SESSION_START_GUARD_LINE));
    assert!(run.stdout.contains("EnterWorktree"));
    assert!(run.stdout.contains("ds worktree land"));
    assert!(
        run.stdout
            .contains("Kept worktrees of this repository: 0 (ds ls lists them).")
    );
    assert_eq!(run.stdout.lines().count(), 1);
}

#[test]
fn should_say_the_guard_is_on_when_the_session_starts_in_a_linked_worktree() {
    let project = guarded_project();
    let worktree = linked_worktree(&project);

    let run = session_start(&project, worktree.path());

    assert!(run.stdout.contains("this is a linked worktree, work here"));
}

#[test]
fn should_say_the_guard_is_off_when_the_local_file_switches_it_off() {
    let project = guarded_project();
    project.write("devslot.local.toml", "[worktrees]\nrequire = false\n");

    let run = session_start(&project, project.path());

    assert!(
        run.stdout
            .contains("devslot worktree guard is off for this checkout")
    );
}

#[test]
fn should_print_nothing_when_the_session_starts_with_the_guard_not_configured() {
    let project = Project::from_scenario("template");
    project.initialize_git_and_commit(".env\n");

    let run = session_start(&project, project.path());

    assert_silent(&run);
}

fn finding_checks(run: &Run) -> Vec<String> {
    let report: Value = serde_json::from_str(&run.stdout).unwrap();
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["check"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn should_say_in_doctor_that_the_guard_is_active_for_a_covered_main_checkout() {
    let project = guarded_project();

    let run = project.doctor(&["doctor", "--json"]);

    assert!(finding_checks(&run).contains(&"worktree_guard".to_owned()));
    assert!(run.stdout.contains("the worktree guard is active"));
}

#[test]
fn should_report_the_committed_require_key_as_ignored_in_doctor() {
    let project = guarded_project();
    project.write("devslot.toml", "[worktrees]\nrequire = false\n");

    let run = project.doctor(&["doctor", "--json"]);

    assert!(finding_checks(&run).contains(&"worktree_require_ignored".to_owned()));
    assert!(run.stdout.contains("is ignored"));
}

#[test]
fn should_mention_in_doctor_a_main_checkout_registered_before_the_guard() {
    let project = Project::from_scenario("template");
    project.initialize_git_and_commit(".env\n");
    project.run(&["ensure"]);
    write_require(project.machine(), "");

    let run = project.doctor(&["doctor", "--json"]);

    assert!(finding_checks(&run).contains(&"worktree_guard_registered".to_owned()));
}

#[test]
fn should_say_nothing_about_the_guard_in_doctor_when_it_is_not_configured() {
    let project = Project::from_scenario("template");
    project.initialize_git_and_commit(".env\n");

    let run = project.doctor(&["doctor", "--json"]);

    assert!(
        finding_checks(&run)
            .iter()
            .all(|check| !check.starts_with("worktree_"))
    );
}

#[test]
fn should_deny_a_write_whose_input_is_larger_than_one_mebibyte() {
    let project = guarded_project();
    let content = "x".repeat(2 * 1024 * 1024);

    let run = write_tool(
        &project,
        project.path(),
        "Write",
        &json!({"file_path": project.path().join("big.txt"), "content": content}),
    );

    denial_reason(&run);
}

#[test]
fn should_give_the_context_when_the_input_is_larger_than_the_limit() {
    let project = guarded_project();
    let content = "x".repeat(33 * 1024 * 1024);

    let run = write_tool(
        &project,
        project.path(),
        "Write",
        &json!({"file_path": project.path().join("big.txt"), "content": content}),
    );

    let output = output_of(&run);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        output["additionalContext"]
            .as_str()
            .unwrap()
            .starts_with("the devslot hook could not read its input (")
    );
    assert_eq!(output.get("permissionDecision"), None);
}

fn commit_local_file(project: &Project, text: &str) {
    project.write("devslot.local.toml", text);
    project.git(&["add", "devslot.local.toml"]);
    project.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.com",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "-m",
        "local",
    ]);
}

#[test]
fn should_still_deny_when_a_committed_local_file_switches_the_guard_off() {
    let project = guarded_project();
    commit_local_file(&project, "[worktrees]\nrequire = false\n");

    let run = edit(&project, project.path(), &project.path().join("a.rs"));

    denial_reason(&run);
}

#[test]
fn should_still_deny_when_a_committed_local_file_is_malformed() {
    let project = guarded_project();
    commit_local_file(&project, "[worktrees\n");

    let run = edit(&project, project.path(), &project.path().join("a.rs"));

    denial_reason(&run);
}

#[test]
fn should_not_register_a_main_checkout_whose_guard_a_committed_file_tries_to_switch_off() {
    let project = guarded_project();
    commit_local_file(&project, "[worktrees]\nrequire = false\n");

    project.run(&["ensure"]);

    assert!(!project.machine().registry_path().exists());
}

#[test]
fn should_report_a_tracked_local_file_key_as_ignored_in_doctor() {
    let project = guarded_project();
    commit_local_file(&project, "[worktrees]\nrequire = false\n");

    let run = project.doctor(&["doctor", "--json"]);

    assert!(finding_checks(&run).contains(&"worktree_require_ignored".to_owned()));
    assert!(run.stdout.contains("devslot.local.toml"));
}
