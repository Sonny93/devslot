#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `ds worktree land` end to end: real git in temp folders, a worktree
//! created by the real hook (so the base is recorded), and a pretend machine.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;
use support::{Project, Run, TEST_SLOTS};

const LOCAL_NOTE: &str = "my own notes\n";

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

fn git_command(directory: &Path, arguments: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .current_dir(directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

fn git_output(directory: &Path, arguments: &[&str]) -> String {
    let output = git_command(directory, arguments).output().unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn git_run(directory: &Path, arguments: &[&str]) {
    let status = git_command(directory, arguments).status().unwrap();
    assert!(status.success(), "git {arguments:?}");
}

fn commit_file(directory: &Path, name: &str, content: &str) {
    std::fs::write(directory.join(name), content).unwrap();
    git_run(directory, &["add", name]);
    git_run(directory, &["commit", "-q", "--no-verify", "-m", name]);
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// The commits of both branches, to check a refusal changed neither.
struct Heads {
    main: String,
    worktree: String,
}

impl Sandbox {
    fn main_path(&self) -> PathBuf {
        self.main.path().canonicalize().unwrap()
    }

    fn main_branch(&self) -> String {
        git_output(&self.main_path(), &["symbolic-ref", "--short", "HEAD"])
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
        assert!(self.root.path().exists());
        PathBuf::from(run.stdout.trim())
    }

    fn land_in(&self, directory: &Path, arguments: &[&str]) -> Run {
        let mut command = vec!["worktree", "land"];
        command.extend(arguments);
        self.main.run_in(directory, &command)
    }

    fn heads(&self, worktree: &Path) -> Heads {
        Heads {
            main: git_output(&self.main_path(), &["rev-parse", &self.main_branch()]),
            worktree: git_output(worktree, &["rev-parse", "HEAD"]),
        }
    }

    fn write_local_note(&self) {
        std::fs::write(self.main_path().join("notes.txt"), LOCAL_NOTE).unwrap();
    }

    fn assert_untouched(&self, worktree: &Path, before: &Heads) {
        let after = self.heads(worktree);
        assert_eq!(after.main, before.main, "main branch moved");
        assert_eq!(after.worktree, before.worktree, "worktree branch moved");
        assert_eq!(read(&self.main_path().join("notes.txt")), LOCAL_NOTE);
    }
}

fn assert_refused(run: &Run, message: &str) {
    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains(message), "{}", run.stderr);
    assert_eq!(run.stdout, "");
}

#[test]
fn should_move_the_main_branch_forward_when_the_worktree_has_commits() {
    let sandbox = sandbox();
    let worktree = sandbox.created("feature-x");
    commit_file(&worktree, "one.txt", "1\n");
    commit_file(&worktree, "two.txt", "2\n");
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    let run = sandbox.land_in(&worktree, &[]);

    let after = sandbox.heads(&worktree);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(after.main, before.worktree);
    assert_eq!(after.worktree, before.worktree);
    assert_eq!(
        run.stdout,
        format!(
            "{} moved from {} to {} (2 commits)\n",
            sandbox.main_branch(),
            &before.main[..7],
            &before.worktree[..7]
        )
    );
    assert_eq!(read(&sandbox.main_path().join("two.txt")), "2\n");
    assert_eq!(read(&sandbox.main_path().join("notes.txt")), LOCAL_NOTE);
}

#[test]
fn should_land_from_a_subfolder_when_the_worktree_has_commits() {
    let sandbox = sandbox();
    let worktree = sandbox.created("feature-x");
    std::fs::create_dir_all(worktree.join("deep/er")).unwrap();
    commit_file(&worktree, "one.txt", "1\n");

    let run = sandbox.land_in(&worktree.join("deep/er"), &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        git_output(&sandbox.main_path(), &["rev-parse", &sandbox.main_branch()]),
        git_output(&worktree, &["rev-parse", "HEAD"])
    );
}

#[test]
fn should_refuse_when_run_in_the_main_checkout() {
    let sandbox = sandbox();
    let worktree = sandbox.created("feature-x");
    commit_file(&worktree, "one.txt", "1\n");
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    let run = sandbox.land_in(&sandbox.main_path(), &[]);

    assert_refused(&run, "is the main checkout, not a linked worktree");
    sandbox.assert_untouched(&worktree, &before);
}

#[test]
fn should_refuse_when_run_outside_git() {
    let sandbox = sandbox();
    let outside = tempfile::tempdir().unwrap();

    let run = sandbox.land_in(outside.path(), &[]);

    assert_refused(&run, "is not inside a git repository");
}

#[test]
fn should_refuse_and_change_nothing_when_a_tracked_file_is_modified() {
    let sandbox = sandbox();
    let worktree = sandbox.created("feature-x");
    commit_file(&worktree, "one.txt", "1\n");
    std::fs::write(worktree.join("one.txt"), "edited\n").unwrap();
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    let run = sandbox.land_in(&worktree, &[]);

    assert_refused(
        &run,
        "1 uncommitted change(s) to tracked files; commit them first",
    );
    sandbox.assert_untouched(&worktree, &before);
    assert_eq!(read(&worktree.join("one.txt")), "edited\n");
}

#[test]
fn should_refuse_when_a_change_is_staged_but_not_committed() {
    let sandbox = sandbox();
    let worktree = sandbox.created("feature-x");
    commit_file(&worktree, "one.txt", "1\n");
    std::fs::write(worktree.join("one.txt"), "staged\n").unwrap();
    git_run(&worktree, &["add", "one.txt"]);
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    let run = sandbox.land_in(&worktree, &[]);

    assert_refused(&run, "uncommitted change(s) to tracked files");
    sandbox.assert_untouched(&worktree, &before);
}

#[test]
fn should_warn_and_land_when_only_untracked_files_are_left() {
    let sandbox = sandbox();
    let worktree = sandbox.created("feature-x");
    commit_file(&worktree, "one.txt", "1\n");
    std::fs::write(worktree.join("scratch.txt"), "draft\n").unwrap();

    let run = sandbox.land_in(&worktree, &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("ds: warning: 1 untracked file(s) in the worktree are not landed"),
        "{}",
        run.stderr
    );
    assert!(!sandbox.main_path().join("scratch.txt").exists());
    assert_eq!(
        git_output(&sandbox.main_path(), &["rev-parse", &sandbox.main_branch()]),
        git_output(&worktree, &["rev-parse", "HEAD"])
    );
}

#[test]
fn should_refuse_when_the_main_checkout_is_on_another_branch() {
    let sandbox = sandbox();
    let worktree = sandbox.created("feature-x");
    commit_file(&worktree, "one.txt", "1\n");
    let base = sandbox.main_branch();
    git_run(&sandbox.main_path(), &["checkout", "-q", "-b", "other"]);
    sandbox.write_local_note();
    let main_before = git_output(&sandbox.main_path(), &["rev-parse", &base]);
    let worktree_before = git_output(&worktree, &["rev-parse", "HEAD"]);

    let run = sandbox.land_in(&worktree, &[]);

    assert_refused(&run, &format!("is on other, not on {base}"));
    assert_eq!(
        git_output(&sandbox.main_path(), &["rev-parse", &base]),
        main_before
    );
    assert_eq!(
        git_output(&worktree, &["rev-parse", "HEAD"]),
        worktree_before
    );
    assert_eq!(read(&sandbox.main_path().join("notes.txt")), LOCAL_NOTE);
}

#[test]
fn should_refuse_when_the_main_checkout_is_detached() {
    let sandbox = sandbox();
    let worktree = sandbox.created("feature-x");
    commit_file(&worktree, "one.txt", "1\n");
    let base = sandbox.main_branch();
    let main_before = git_output(&sandbox.main_path(), &["rev-parse", "HEAD"]);
    git_run(&sandbox.main_path(), &["checkout", "-q", "--detach"]);
    sandbox.write_local_note();

    let run = sandbox.land_in(&worktree, &[]);

    assert_refused(&run, &format!("is on a detached HEAD, not on {base}"));
    assert_eq!(
        git_output(&sandbox.main_path(), &["rev-parse", &base]),
        main_before
    );
    assert_eq!(read(&sandbox.main_path().join("notes.txt")), LOCAL_NOTE);
}

#[test]
fn should_refuse_when_a_merge_is_in_progress_in_the_main_checkout() {
    let sandbox = sandbox();
    let worktree = sandbox.created("feature-x");
    commit_file(&worktree, "one.txt", "1\n");
    let main_head = git_output(&sandbox.main_path(), &["rev-parse", "HEAD"]);
    std::fs::write(
        sandbox.main_path().join(".git/MERGE_HEAD"),
        format!("{main_head}\n"),
    )
    .unwrap();
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    let run = sandbox.land_in(&worktree, &[]);

    assert_refused(&run, "a merge is in progress in the main checkout");
    sandbox.assert_untouched(&worktree, &before);
}

#[test]
fn should_refuse_and_change_nothing_when_the_base_moved() {
    let sandbox = sandbox();
    let worktree = sandbox.created("feature-x");
    commit_file(&worktree, "one.txt", "1\n");
    commit_file(&sandbox.main_path(), "moved.txt", "moved\n");
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    let run = sandbox.land_in(&worktree, &[]);

    assert_refused(
        &run,
        &format!(
            "the base moved: run `git rebase {}` here, resolve, commit, then run ds worktree land again",
            sandbox.main_branch()
        ),
    );
    sandbox.assert_untouched(&worktree, &before);
}

#[test]
fn should_refuse_when_no_base_is_recorded_and_onto_is_missing() {
    let sandbox = sandbox();
    let legacy = sandbox.main.add_worktree("legacy");
    commit_file(legacy.path(), "one.txt", "1\n");
    sandbox.write_local_note();
    let before = sandbox.heads(legacy.path());

    let run = sandbox.land_in(legacy.path(), &[]);

    assert_refused(&run, "pass --onto <branch>");
    sandbox.assert_untouched(legacy.path(), &before);
}

#[test]
fn should_land_on_the_branch_given_by_onto_when_no_base_is_recorded() {
    let sandbox = sandbox();
    let legacy = sandbox.main.add_worktree("legacy");
    commit_file(legacy.path(), "one.txt", "1\n");
    let base = sandbox.main_branch();

    let run = sandbox.land_in(legacy.path(), &["--onto", &base]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        git_output(&sandbox.main_path(), &["rev-parse", &base]),
        git_output(legacy.path(), &["rev-parse", "HEAD"])
    );
}

#[test]
fn should_prefer_onto_when_it_differs_from_the_recorded_base() {
    let sandbox = sandbox();
    let worktree = sandbox.created("feature-x");
    commit_file(&worktree, "one.txt", "1\n");
    git_run(
        &worktree,
        &["config", "branch.feature-x.devslotBase", "gone"],
    );
    let base = sandbox.main_branch();

    let run = sandbox.land_in(&worktree, &["--onto", &base]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        git_output(&sandbox.main_path(), &["rev-parse", &base]),
        git_output(&worktree, &["rev-parse", "HEAD"])
    );
}

#[test]
fn should_say_there_is_nothing_to_land_when_the_worktree_is_at_the_target() {
    let sandbox = sandbox();
    let worktree = sandbox.created("feature-x");
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    let run = sandbox.land_in(&worktree, &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stdout.contains("nothing to land"), "{}", run.stdout);
    sandbox.assert_untouched(&worktree, &before);
}

#[test]
fn should_forward_git_refusal_and_keep_local_edits_when_the_main_checkout_would_be_overwritten() {
    let sandbox = sandbox();
    commit_file(&sandbox.main_path(), "shared.txt", "original\n");
    let worktree = sandbox.created("feature-x");
    commit_file(&worktree, "shared.txt", "from the worktree\n");
    std::fs::write(sandbox.main_path().join("shared.txt"), "local edit\n").unwrap();
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    let run = sandbox.land_in(&worktree, &[]);

    assert_refused(&run, "would be overwritten by merge");
    assert!(run.stderr.contains("shared.txt"), "{}", run.stderr);
    sandbox.assert_untouched(&worktree, &before);
    assert_eq!(
        std::fs::read(sandbox.main_path().join("shared.txt")).unwrap(),
        b"local edit\n"
    );
}

#[test]
fn should_refuse_when_the_worktree_is_on_a_detached_head() {
    let sandbox = sandbox();
    let worktree = sandbox.created("feature-x");
    git_run(&worktree, &["checkout", "-q", "--detach"]);
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    let run = sandbox.land_in(&worktree, &[]);

    assert_refused(&run, "the worktree is on a detached HEAD");
    sandbox.assert_untouched(&worktree, &before);
}

#[test]
fn should_refuse_and_keep_the_file_when_the_branch_adds_a_path_ignored_in_the_main_checkout() {
    let sandbox = sandbox();
    commit_file(&sandbox.main_path(), ".gitignore", ".env\nsecrets.txt\n");
    let worktree = sandbox.created("feature-x");
    std::fs::write(worktree.join("secrets.txt"), "from the branch\n").unwrap();
    git_run(&worktree, &["add", "-f", "secrets.txt"]);
    git_run(&worktree, &["commit", "-q", "--no-verify", "-m", "secrets"]);
    std::fs::write(sandbox.main_path().join("secrets.txt"), "my secret\n").unwrap();
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    let run = sandbox.land_in(&worktree, &[]);

    assert_refused(
        &run,
        "the landing would overwrite secrets.txt in the main checkout",
    );
    assert!(!run.stderr.contains("my secret"), "{}", run.stderr);
    sandbox.assert_untouched(&worktree, &before);
    assert_eq!(
        std::fs::read(sandbox.main_path().join("secrets.txt")).unwrap(),
        b"my secret\n"
    );
}

#[test]
fn should_land_when_the_branch_adds_a_path_missing_from_the_main_checkout() {
    let sandbox = sandbox();
    commit_file(&sandbox.main_path(), ".gitignore", ".env\nsecrets.txt\n");
    let worktree = sandbox.created("feature-x");
    std::fs::write(worktree.join("secrets.txt"), "from the branch\n").unwrap();
    git_run(&worktree, &["add", "-f", "secrets.txt"]);
    git_run(&worktree, &["commit", "-q", "--no-verify", "-m", "secrets"]);

    let run = sandbox.land_in(&worktree, &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        read(&sandbox.main_path().join("secrets.txt")),
        "from the branch\n"
    );
}

#[test]
fn should_refuse_and_keep_the_file_when_the_branch_adds_a_path_under_a_file_of_the_main_checkout() {
    let sandbox = sandbox();
    commit_file(&sandbox.main_path(), ".gitignore", ".env\ncache\n");
    let worktree = sandbox.created("feature-x");
    std::fs::write(worktree.join(".gitignore"), ".env\n").unwrap();
    git_run(&worktree, &["add", ".gitignore"]);
    std::fs::create_dir_all(worktree.join("cache")).unwrap();
    std::fs::write(worktree.join("cache/x"), "from the branch\n").unwrap();
    git_run(&worktree, &["add", "cache/x"]);
    git_run(&worktree, &["commit", "-q", "--no-verify", "-m", "cache"]);
    std::fs::write(sandbox.main_path().join("cache"), "my cached data\n").unwrap();
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    let run = sandbox.land_in(&worktree, &[]);

    assert_refused(
        &run,
        "the landing would overwrite cache in the main checkout",
    );
    sandbox.assert_untouched(&worktree, &before);
    assert_eq!(
        std::fs::read(sandbox.main_path().join("cache")).unwrap(),
        b"my cached data\n"
    );
}

fn replace_file_by_folder_in_worktree(worktree: &Path) {
    git_run(worktree, &["rm", "-q", "foo"]);
    std::fs::create_dir_all(worktree.join("foo")).unwrap();
    std::fs::write(worktree.join("foo/x"), "from the branch\n").unwrap();
    git_run(worktree, &["add", "foo/x"]);
    git_run(worktree, &["commit", "-q", "--no-verify", "-m", "folder"]);
}

#[test]
fn should_land_when_the_branch_replaces_a_tracked_file_by_a_folder() {
    let sandbox = sandbox();
    commit_file(&sandbox.main_path(), "foo", "tracked\n");
    let worktree = sandbox.created("feature-x");
    replace_file_by_folder_in_worktree(&worktree);

    let run = sandbox.land_in(&worktree, &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        read(&sandbox.main_path().join("foo/x")),
        "from the branch\n"
    );
}

#[test]
fn should_let_git_refuse_and_keep_the_edit_when_the_replaced_tracked_file_is_modified() {
    let sandbox = sandbox();
    commit_file(&sandbox.main_path(), "foo", "tracked\n");
    let worktree = sandbox.created("feature-x");
    replace_file_by_folder_in_worktree(&worktree);
    std::fs::write(sandbox.main_path().join("foo"), "my edit\n").unwrap();
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    let run = sandbox.land_in(&worktree, &[]);

    assert_refused(&run, "would be overwritten by merge");
    sandbox.assert_untouched(&worktree, &before);
    assert_eq!(read(&sandbox.main_path().join("foo")), "my edit\n");
}

fn commit_added_file(worktree: &Path, relative: &str) {
    let file = worktree.join(relative);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "from the branch\n").unwrap();
    git_run(worktree, &["--literal-pathspecs", "add", relative]);
    git_run(worktree, &["commit", "-q", "--no-verify", "-m", "added"]);
}

fn ignore_in_main(sandbox: &Sandbox, pattern: &str) {
    let exclude = sandbox.main_path().join(".git/info/exclude");
    std::fs::write(exclude, format!("{pattern}\n")).unwrap();
}

fn assert_would_overwrite_and_untouched(
    sandbox: &Sandbox,
    worktree: &Path,
    before: &Heads,
    (file, content): (&str, &[u8]),
) {
    let run = sandbox.land_in(worktree, &[]);

    assert_refused(
        &run,
        &format!("the landing would overwrite {file} in the main checkout"),
    );
    sandbox.assert_untouched(worktree, before);
    assert_eq!(
        std::fs::read(sandbox.main_path().join(file)).unwrap(),
        content
    );
}

#[test]
fn should_refuse_when_the_replaced_file_was_untracked_in_the_main_checkout_and_holds_ignored_data()
{
    let sandbox = sandbox();
    commit_file(&sandbox.main_path(), "foo", "tracked\n");
    let worktree = sandbox.created("feature-x");
    replace_file_by_folder_in_worktree(&worktree);
    git_run(&sandbox.main_path(), &["rm", "-q", "--cached", "foo"]);
    ignore_in_main(&sandbox, "foo");
    std::fs::write(sandbox.main_path().join("foo"), "my ignored data\n").unwrap();
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    assert_would_overwrite_and_untouched(
        &sandbox,
        &worktree,
        &before,
        ("foo", b"my ignored data\n"),
    );
}

#[test]
fn should_refuse_when_an_ignored_file_stands_where_the_target_tracks_a_folder() {
    let sandbox = sandbox();
    commit_added_file(&sandbox.main_path(), "dir/y");
    let worktree = sandbox.created("feature-x");
    commit_added_file(&worktree, "dir/x");
    std::fs::remove_dir_all(sandbox.main_path().join("dir")).unwrap();
    ignore_in_main(&sandbox, "dir");
    std::fs::write(sandbox.main_path().join("dir"), "my ignored data\n").unwrap();
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    assert_would_overwrite_and_untouched(
        &sandbox,
        &worktree,
        &before,
        ("dir", b"my ignored data\n"),
    );
}

#[test]
fn should_refuse_when_a_file_named_like_a_pathspec_magic_is_ignored_in_the_main_checkout() {
    let sandbox = sandbox();
    let worktree = sandbox.created("feature-x");
    commit_added_file(&worktree, ":/x");
    ignore_in_main(&sandbox, ":");
    std::fs::write(sandbox.main_path().join(":"), "my ignored data\n").unwrap();
    sandbox.write_local_note();
    let before = sandbox.heads(&worktree);

    assert_would_overwrite_and_untouched(&sandbox, &worktree, &before, (":", b"my ignored data\n"));
}
