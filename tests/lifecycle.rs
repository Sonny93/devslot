#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The commands of phase 3 end to end: the `.env` block, `ds ensure`, `ds up`,
//! `ds down`, `ds reset`, `ds rm`, `ds prune` and the services state of
//! `ds info`. The real binary runs against the fake docker, in git-initialised
//! temp projects and a pretend machine with its own config, registry and state.

mod support;

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use devslot::ensure_cache::{CacheEntry, Fingerprint, entry_path, write_entry};
use serde_json::Value;
use support::{FAKE_ENV, Machine, Project, TEST_SLOTS};

const BLOCK_START: &str = "# >>> devslot";
const BLOCK_END: &str = "# <<< devslot";
const UP_ARGUMENTS: &str = "up -d --wait --wait-timeout 120";
const PROJECT_LABEL: &str = "label=com.docker.compose.project";

fn json_of(run: &support::Run) -> Value {
    serde_json::from_str(&run.stdout).unwrap()
}

fn environment_of(project: &Project) -> BTreeMap<String, String> {
    let run = project.run(&["env", "--format", "json"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    serde_json::from_str(&run.stdout).unwrap()
}

fn name_of(project: &Project) -> String {
    let run = project.run(&["info", "--json"]);
    json_of(&run)["name"].as_str().unwrap().to_owned()
}

fn outside_block(text: &str) -> String {
    let mut is_inside = false;
    let mut kept = String::new();
    for line in text.split_inclusive('\n') {
        if line.starts_with(BLOCK_START) {
            is_inside = true;
        }
        if !is_inside {
            kept.push_str(line);
        }
        if line.starts_with(BLOCK_END) {
            is_inside = false;
        }
    }
    kept
}

fn git_project() -> Project {
    let project = Project::from_scenario("template");
    project.initialize_git(".env\n");
    project
}

fn canonical(project: &Project) -> PathBuf {
    project.path().canonicalize().unwrap()
}

fn dev_compose_of(project: &Project) -> String {
    canonical(project)
        .join("dev.compose.yml")
        .display()
        .to_string()
}

fn instance_count(project: &Project) -> usize {
    project
        .machine()
        .read_registry()
        .matches("[[instance]]")
        .count()
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

fn log_has_line_ending_with(project: &Project, suffix: &str) -> bool {
    project
        .machine()
        .docker_log()
        .iter()
        .any(|line| line.ends_with(suffix))
}

#[test]
fn should_write_the_block_and_keep_every_byte_outside_it_when_running() {
    let project = git_project();

    let run = project.run(&["run", "--", "true"]);

    let text = project.read(".env");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(text.contains(&format!("{BLOCK_START}: generated automatically")));
    assert!(text.contains("COMPOSE_PROJECT_NAME="));
    assert_eq!(outside_block(&text), FAKE_ENV);
    assert!(run.stderr.contains(&format!(
        "ds: wrote the devslot block in {}/.env",
        canonical(&project).display()
    )));
}

#[test]
fn should_not_rewrite_the_env_when_running_a_second_time() {
    let project = git_project();
    project.run(&["run", "--", "true"]);
    let content = project.read(".env");
    let modified = project.modified(".env");

    let second = project.run(&["run", "--", "true"]);

    assert_eq!(project.read(".env"), content);
    assert_eq!(project.modified(".env"), modified);
    assert!(!second.stderr.contains("wrote the devslot block"));
}

#[test]
fn should_create_the_env_of_a_worktree_from_the_main_checkout_without_its_block() {
    let main = Project::from_scenario("template");
    main.initialize_git_and_commit(".env\n");
    main.run(&["run", "--", "true"]);
    let worktree = main.add_worktree("feature");
    assert!(!worktree.exists(".env"));

    let run = worktree.run(&["run", "--", "true"]);

    let text = worktree.read(".env");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stderr.contains("ds: created "));
    assert!(
        run.stderr
            .contains(&format!("{}/.env", canonical(&main).display()))
    );
    assert_eq!(outside_block(&text), FAKE_ENV);
    assert_eq!(text.matches(BLOCK_START).count(), 1);
    assert!(!text.contains(&format!("COMPOSE_PROJECT_NAME={}\n", name_of(&main))));
    assert!(environment_of(&worktree).contains_key("PORT"));
}

#[test]
fn should_copy_a_main_env_that_is_a_symlink_into_a_new_worktree_without_its_block() {
    let main = Project::from_scenario("template");
    main.initialize_git_and_commit(".env\n");
    main.run(&["run", "--", "true"]);
    let managed = main
        .machine()
        .write_scratch("managed.env", &main.read(".env"));
    main.remove(".env");
    std::os::unix::fs::symlink(&managed, main.path().join(".env")).unwrap();
    let worktree = main.add_worktree("feature");

    let run = worktree.run(&["run", "--", "true"]);

    let text = worktree.read(".env");
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(outside_block(&text), FAKE_ENV);
    assert_eq!(text.matches(BLOCK_START).count(), 1);
    assert!(
        !worktree
            .path()
            .join(".env")
            .symlink_metadata()
            .unwrap()
            .is_symlink()
    );
}

#[test]
fn should_agree_in_doctor_run_and_ensure_that_a_symlinked_example_is_no_source() {
    let project = git_project();
    project.run(&["ensure"]);
    project.remove(".env");
    let outside = project.machine().write_scratch("secret.env", "PORT=3333\n");
    std::os::unix::fs::symlink(&outside, project.path().join(".env.example")).unwrap();

    let doctor = project.run(&["doctor", "--json"]);
    let first = project.run(&["ensure"]);
    let second = project.run(&["ensure"]);

    assert_eq!(doctor.status, 1, "{}", doctor.stdout);
    assert!(doctor.stdout.contains("env_missing"));
    assert_eq!(first.status, 1, "{}", first.stderr);
    assert!(first.stderr.contains("there is no .env"));
    assert_eq!((second.status, second.stderr.as_str()), (0, ""));
    assert!(!project.exists(".env"));
}

#[test]
fn should_refuse_to_run_and_leave_the_env_untouched_when_git_does_not_ignore_it() {
    let project = Project::from_scenario("template");
    project.initialize_git("node_modules\n");

    let run = project.run(&["run", "--", "true"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains("git does not ignore .env"));
    assert_eq!(project.read(".env"), FAKE_ENV);
}

#[test]
fn should_refuse_to_ensure_and_leave_the_env_untouched_when_git_does_not_ignore_it() {
    let project = Project::from_scenario("template");
    project.initialize_git("node_modules\n");

    let run = project.run(&["ensure"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains("git does not ignore .env"));
    assert_eq!(project.read(".env"), FAKE_ENV);
    assert!(!project.machine().registry_path().exists());
}

#[test]
fn should_register_and_write_the_block_on_the_first_ensure() {
    let project = git_project();

    let run = project.run(&["ensure"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(instance_count(&project), 1);
    assert!(project.read(".env").contains("COMPOSE_PROJECT_NAME="));
    assert!(run.stderr.contains("ds: registered "));
}

#[test]
fn should_make_no_docker_call_on_the_second_ensure() {
    let project = git_project();
    project.run(&["ensure"]);
    project.machine().clear_docker_log();

    let second = project.run_with(&["ensure"], &[("DEVSLOT_DOCKER", "/does/not/exist")]);

    assert_eq!(second.status, 0, "{}", second.stderr);
    assert_eq!(second.stderr, "");
    assert_eq!(project.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_do_nothing_when_ensure_runs_outside_git() {
    let project = Project::from_scenario("template");

    let run = project.run(&["ensure"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(project.read(".env"), FAKE_ENV);
    assert!(!project.machine().registry_path().exists());
}

#[test]
fn should_do_nothing_when_ensure_runs_outside_the_auto_scope() {
    let project = git_project();
    project.machine().write_global_config(&format!(
        "{TEST_SLOTS}\n[auto]\nexclude = [\"{}\"]\n",
        canonical(&project).display()
    ));

    let run = project.run_with(&["ensure"], &[("DEVSLOT_DOCKER", "/does/not/exist")]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(project.read(".env"), FAKE_ENV);
    assert!(!project.machine().registry_path().exists());
}

#[test]
fn should_do_nothing_when_ensure_runs_without_a_compose_file() {
    let project = git_project();
    project.remove("dev.compose.yml");
    project.remove("compose.yml");

    let run = project.run(&["ensure"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(project.read(".env"), FAKE_ENV);
    assert!(!project.machine().registry_path().exists());
}

#[test]
fn should_write_the_block_again_when_it_was_deleted_by_hand() {
    let project = git_project();
    project.run(&["ensure"]);
    project.write(".env", FAKE_ENV);

    let run = project.run(&["ensure"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(project.read(".env").contains("COMPOSE_PROJECT_NAME="));
    assert!(run.stderr.contains("ds: wrote the devslot block in "));
    assert_eq!(instance_count(&project), 1);
}

#[test]
fn should_report_a_refusal_once_then_stay_quiet_until_the_compose_file_changes() {
    let project = git_project();
    let fixtures = fixtures_with_container_name();
    let environment = [("DOCKER_FAKE_FIXTURES", fixtures.path().to_str().unwrap())];

    let first = project.run_with(&["ensure"], &environment);
    let second = project.run_with(&["ensure"], &environment);
    project.write(
        "dev.compose.yml",
        &format!("{}\n# changed\n", project.read("dev.compose.yml")),
    );
    let third = project.run_with(&["ensure"], &environment);

    assert_eq!(first.status, 1, "{}", first.stderr);
    assert!(first.stderr.contains("service sets container_name"));
    assert_eq!((second.status, second.stderr.as_str()), (0, ""));
    assert_eq!(third.status, 1, "{}", third.stderr);
    assert!(!project.machine().registry_path().exists());
}

#[test]
fn should_report_the_refusal_of_a_registered_checkout_once_until_it_changes() {
    let project = git_project();
    project.run(&["ensure"]);
    project.write(".gitignore", "node_modules\n");
    project.write(".env", FAKE_ENV);

    let first = project.run(&["ensure"]);
    let second = project.run(&["ensure"]);
    project.write(".gitignore", "node_modules\n# changed\n");
    let third = project.run(&["ensure"]);
    project.write(".gitignore", ".env\n");
    let fixed = project.run(&["ensure"]);

    assert_eq!(first.status, 1, "{}", first.stderr);
    assert!(first.stderr.contains("git does not ignore .env"));
    assert_eq!((second.status, second.stderr.as_str()), (0, ""));
    assert_eq!(third.status, 1, "{}", third.stderr);
    assert_eq!(fixed.status, 0, "{}", fixed.stderr);
    assert!(project.read(".env").contains("COMPOSE_PROJECT_NAME="));
}

#[test]
fn should_not_create_the_env_from_an_example_that_is_a_symlink_to_a_file_outside() {
    let project = git_project();
    project.remove(".env");
    let outside = project
        .machine()
        .write_scratch("secret.env", "SECRET=FAKE\nPORT=3333\n");
    std::os::unix::fs::symlink(&outside, project.path().join(".env.example")).unwrap();

    let run = project.run(&["run", "--", "true"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains("there is no .env"));
    assert!(!project.exists(".env"));
}

#[test]
fn should_stay_quiet_for_ten_minutes_after_a_docker_failure_unless_the_checkout_changes() {
    let project = git_project();
    let unavailable = [("DEVSLOT_DOCKER", "/does/not/exist")];

    let first = project.run_with(&["ensure"], &unavailable);
    let second = project.run_with(&["ensure"], &unavailable);
    project.write(".gitignore", ".env\n# changed\n");
    let third = project.run_with(&["ensure"], &unavailable);

    assert_eq!(first.status, 2, "{}", first.stderr);
    assert_eq!((second.status, second.stderr.as_str()), (0, ""));
    assert_eq!(third.status, 2, "{}", third.stderr);
}

#[test]
fn should_register_the_checkout_once_and_write_one_block_when_four_ensures_run_in_parallel() {
    let project = git_project();

    let runs: Vec<support::Run> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| project.run(&["ensure"])))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    });

    let text = project.read(".env");
    assert!(
        runs.iter().all(|run| run.status == 0),
        "{runs:?}",
        runs = runs
            .iter()
            .map(|run| run.stderr.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(instance_count(&project), 1);
    assert_eq!(text.matches(BLOCK_START).count(), 1);
    assert_eq!(outside_block(&text), FAKE_ENV);
    assert_eq!(
        runs.iter()
            .filter(|run| run.stderr.contains("ds: registered "))
            .count(),
        1
    );
}

#[test]
fn should_start_the_services_with_the_instance_environment_and_without_the_inherited_compose_file()
{
    let project = git_project();

    let run = project.run_with(
        &["up"],
        &[
            ("COMPOSE_FILE", "/elsewhere.yml"),
            ("DOCKER_FAKE_LOG_ENV", "COMPOSE_FILE"),
        ],
    );

    let name = name_of(&project);
    let dev_compose = dev_compose_of(&project);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr
            .contains(&format!("ds: the services of {name} are up"))
    );
    assert!(project.machine().docker_log().contains(&format!(
        "COMPOSE_PROJECT_NAME={name} COMPOSE_FILE={dev_compose} compose -p {name} -f {dev_compose} {UP_ARGUMENTS}"
    )));
}

#[test]
fn should_run_the_run_key_with_the_instance_environment_after_the_services() {
    let project = git_project();
    project.write("devslot.toml", "run = \"echo up-$PORT\"\n");

    let run = project.run(&["up"]);

    let port = environment_of(&project)["PORT"].clone();
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stdout, format!("up-{port}\n"));
    assert!(
        run.stderr
            .contains("ds: running echo up-$PORT (from devslot.toml)")
    );
    assert!(log_has_line_ending_with(&project, UP_ARGUMENTS));
}

#[test]
fn should_start_the_services_without_running_the_run_key_when_up_has_no_run() {
    let project = git_project();
    project.write("devslot.toml", "run = \"touch ran-the-app\"\n");

    let run = project.run(&["up", "--no-run"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stderr
            .contains(&format!("ds: the services of {} are up", name_of(&project)))
    );
    assert!(log_has_line_ending_with(&project, UP_ARGUMENTS));
    assert!(!project.exists("ran-the-app"));
}

#[test]
fn should_refuse_to_start_when_another_program_holds_a_service_port() {
    let project = git_project();
    project
        .machine()
        .write_global_config("[slots]\nfirst_port = 16000\nlast_port = 16999\nsize = 10\n");
    project.run(&["run", "--", "true"]);
    let port: u16 = environment_of(&project)["DB_PORT"].parse().unwrap();
    let _listener = TcpListener::bind(("127.0.0.1", port)).unwrap();

    let run = project.run(&["up"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains(&format!(
        "ds: error: port {port} (DB_PORT) is used by another program"
    )));
    assert!(
        !project
            .machine()
            .docker_log()
            .iter()
            .any(|line| line.contains(UP_ARGUMENTS))
    );
}

#[test]
fn should_exit_1_when_compose_fails_to_start_the_services() {
    let project = git_project();

    let run = project.run_with(&["up"], &[("DOCKER_FAKE_FAIL_COMMANDS", "up")]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("did not start or did not become healthy; see the Compose output above")
    );
}

#[test]
fn should_refuse_to_start_when_the_checks_find_an_error() {
    let project = git_project();
    let fixtures = fixtures_with_container_name();

    let run = project.run_with(
        &["up"],
        &[("DOCKER_FAKE_FIXTURES", fixtures.path().to_str().unwrap())],
    );

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains("ds up does not start the services"));
    assert_eq!(project.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_say_there_is_no_service_to_start_when_the_checkout_has_no_compose_file() {
    let project = git_project();
    project.remove("dev.compose.yml");
    project.remove("compose.yml");

    let run = project.run(&["up"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("ds: no compose file: no service to start")
    );
    assert_eq!(project.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_stop_the_services_of_a_registered_checkout() {
    let project = git_project();
    project.run(&["run", "--", "true"]);

    let run = project.run(&["down"]);

    let name = name_of(&project);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(log_has_line_ending_with(
        &project,
        &format!("compose -p {name} -f {} down", dev_compose_of(&project))
    ));
}

#[test]
fn should_refuse_to_stop_an_unregistered_checkout() {
    let project = git_project();

    let run = project.run(&["down"]);

    assert_eq!(run.status, 1);
    assert!(
        run.stderr
            .contains("ds: error: this checkout is not registered; nothing to stop")
    );
    assert_eq!(project.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_delete_the_volumes_then_start_the_services_when_resetting() {
    let project = git_project();
    project.run(&["run", "--", "true"]);
    let name = name_of(&project);
    project.machine().clear_docker_log();

    let run = project.run(&["reset"]);

    let log = project.machine().docker_log();
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stderr
            .contains(&format!("ds: resetting the data of {name} ("))
    );
    assert_eq!(log.len(), 4, "{log:?}");
    assert!(log[0].ends_with(&format!("ps -aq --filter {PROJECT_LABEL}={name}")));
    assert!(log[1].starts_with("COMPOSE_PROJECT_NAME= volume ls -q"));
    assert!(log[2].starts_with("COMPOSE_PROJECT_NAME= network ls -q"));
    assert!(log[3].ends_with(UP_ARGUMENTS));
    assert!(!log.iter().any(|line| line.contains(" down")));
}

#[test]
fn should_reset_only_the_service_when_one_is_named() {
    let project = git_project();
    project.run(&["run", "--", "true"]);
    let name = name_of(&project);
    let volumes = project
        .machine()
        .write_scratch("volumes", "app_postgres-volume\nother_postgres-volume\n");
    project.machine().clear_docker_log();

    let run = project.run_with(
        &["reset", "postgres"],
        &[("DOCKER_FAKE_VOLUMES", volumes.to_str().unwrap())],
    );

    let log = project.machine().docker_log();
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(log.len(), 4, "{log:?}");
    assert!(log[0].ends_with("rm -s -f -v postgres"));
    assert!(log[1].ends_with(&format!(
        "volume ls -q --filter label=com.docker.compose.project={name} --filter label=com.docker.compose.volume=postgres-volume"
    )));
    assert!(log[2].ends_with("volume rm app_postgres-volume other_postgres-volume"));
    assert!(log[3].ends_with(&format!("{UP_ARGUMENTS} postgres")));
}

#[test]
fn should_refuse_to_reset_a_service_the_compose_does_not_have() {
    let project = git_project();
    project.run(&["run", "--", "true"]);

    let run = project.run(&["reset", "mysql"]);

    assert_eq!(run.status, 1);
    assert!(run.stderr.contains("no service named mysql"));
    assert_eq!(project.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_refuse_to_reset_an_unregistered_checkout() {
    let project = git_project();

    let run = project.run(&["reset"]);

    assert_eq!(run.status, 1);
    assert!(run.stderr.contains("not registered; nothing to reset"));
}

#[test]
fn should_bring_the_checkout_back_to_its_default_configuration_and_opt_it_out() {
    let project = git_project();
    let before = project.read(".env");
    project.run(&["run", "--", "true"]);
    let name = name_of(&project);
    let machine = project.machine();
    let containers = machine.write_scratch("containers", "c1\nc2\n");
    let volumes = machine.write_scratch("volumes", "v1\n");
    let networks = machine.write_scratch("networks", "n1\n");
    machine.clear_docker_log();

    let run = project.run_with(
        &["rm", "--yes"],
        &[
            ("DOCKER_FAKE_CONTAINERS", containers.to_str().unwrap()),
            ("DOCKER_FAKE_VOLUMES", volumes.to_str().unwrap()),
            ("DOCKER_FAKE_NETWORKS", networks.to_str().unwrap()),
        ],
    );

    let registry = machine.read_registry();
    let path = canonical(&project).display().to_string();
    let log = machine.docker_log();
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(log.len(), 6, "{log:?}");
    assert!(log[0].ends_with(&format!("ps -aq --filter {PROJECT_LABEL}={name}")));
    assert!(log[1].ends_with("rm -f -v c1 c2"));
    assert!(log[2].ends_with(&format!("volume ls -q --filter {PROJECT_LABEL}={name}")));
    assert!(log[3].ends_with("volume rm v1"));
    assert!(log[4].ends_with(&format!("network ls -q --filter {PROJECT_LABEL}={name}")));
    assert!(log[5].ends_with("network rm n1"));
    assert!(!log.iter().any(|line| line.contains(" compose ")));
    assert_eq!(project.read(".env"), before);
    assert_eq!(instance_count(&project), 0);
    assert!(registry.contains(&format!("opted_out = [\"{path}\"]")));
    assert!(run.stderr.contains(&format!(
        "ds: {path} is back to its default configuration and opted out of automatic registration"
    )));
}

#[test]
fn should_never_let_the_compose_model_choose_what_rm_reset_and_prune_delete() {
    let (alive, gone) = two_registered_projects();
    let model = alive.machine().write_scratch(
        "model.json",
        r#"{"volumes":{"data":{"name":"victim_pgdata"}}}"#,
    );
    let environment = [("DOCKER_FAKE_MODEL", model.to_str().unwrap())];
    std::fs::remove_dir_all(gone.path()).unwrap();
    alive.machine().clear_docker_log();

    let reset = alive.run_with(&["reset"], &environment);
    let prune = alive.run_with(&["prune", "--yes"], &environment);
    let rm = alive.run_with(&["rm", "--yes"], &environment);

    let log = alive.machine().docker_log();
    assert_eq!(reset.status, 1, "{}", reset.stderr);
    assert_eq!(
        (prune.status, rm.status),
        (0, 0),
        "{}{}",
        prune.stderr,
        rm.stderr
    );
    assert!(!log.iter().any(|line| line.contains(" down")), "{log:?}");
    assert!(!log.iter().any(|line| line.contains("victim_pgdata")));
}

#[test]
fn should_refuse_to_start_when_the_resolved_model_names_a_volume_outside_the_project() {
    let project = git_project();
    let model = project.machine().write_scratch(
        "model.json",
        r#"{"volumes":{"postgres-volume":{"name":"victim_pgdata"}}}"#,
    );

    let run = project.run_with(&["up"], &[("DOCKER_FAKE_MODEL", model.to_str().unwrap())]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("volume postgres-volume is external or has a name")
    );
    assert!(!run.stderr.contains("victim_pgdata"));
    assert!(
        !project
            .machine()
            .docker_log()
            .iter()
            .any(|line| line.contains(UP_ARGUMENTS))
    );
}

#[test]
fn should_leave_an_opted_out_checkout_alone_until_an_explicit_run_registers_it_again() {
    let project = git_project();
    let before = project.read(".env");
    project.run(&["run", "--", "true"]);
    project.run(&["rm", "--yes"]);
    let registry_after_rm = project.machine().read_registry();

    let ensure = project.run(&["ensure"]);
    let explicit = project.run(&["run", "--", "true"]);

    assert_eq!((ensure.status, ensure.stderr.as_str()), (0, ""));
    assert_eq!(explicit.status, 0, "{}", explicit.stderr);
    assert!(registry_after_rm.contains("opted_out"));
    assert!(
        explicit
            .stderr
            .contains("was opted out by ds rm; registering it again")
    );
    assert_eq!(instance_count(&project), 1);
    assert!(!project.machine().read_registry().contains(&format!(
        "opted_out = [\"{}\"]",
        canonical(&project).display()
    )));
    assert_eq!(outside_block(&project.read(".env")), before);
}

#[test]
fn should_do_nothing_on_ensure_while_the_checkout_stays_opted_out() {
    let project = git_project();
    project.run(&["run", "--", "true"]);
    project.run(&["rm", "--yes"]);
    let env = project.read(".env");
    let registry = project.machine().read_registry();

    let run = project.run(&["ensure"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(project.read(".env"), env);
    assert_eq!(project.machine().read_registry(), registry);
}

#[test]
fn should_change_nothing_when_rm_has_no_terminal_and_no_yes() {
    let project = git_project();
    project.run(&["run", "--", "true"]);
    let env = project.read(".env");
    let registry = project.machine().read_registry();
    project.machine().clear_docker_log();

    let run = project.run(&["rm"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("ds: error: ds rm deletes data; run it in a terminal or pass --yes")
    );
    assert_eq!(project.read(".env"), env);
    assert_eq!(project.machine().read_registry(), registry);
    assert_eq!(project.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_only_record_the_opt_out_when_rm_runs_on_an_unregistered_checkout() {
    let project = git_project();

    let run = project.run(&["rm"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(project.machine().read_registry().contains(&format!(
        "opted_out = [\"{}\"]",
        canonical(&project).display()
    )));
    assert_eq!(project.machine().docker_log(), Vec::<String>::new());
    assert_eq!(project.read(".env"), FAKE_ENV);
}

#[test]
fn should_remove_a_foreign_block_without_any_docker_call_when_rm_runs_on_an_unregistered_checkout()
{
    let project = git_project();
    project.write(
        ".env",
        &format!(
            "{FAKE_ENV}{BLOCK_START}: generated automatically, do not edit by hand >>>\nCOMPOSE_PROJECT_NAME=someone-elses\n# <<< devslot <<<\n"
        ),
    );

    let run = project.run(&["rm"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(project.read(".env"), FAKE_ENV);
    assert_eq!(project.machine().docker_log(), Vec::<String>::new());
    assert!(run.stderr.contains("ds: removed the devslot block from "));
}

#[test]
fn should_keep_the_instance_and_the_block_when_docker_fails_to_delete_the_data() {
    let project = git_project();
    project.run(&["run", "--", "true"]);
    let env = project.read(".env");
    let registry = project.machine().read_registry();

    let containers = project.machine().write_scratch("containers", "c1\n");

    let run = project.run_with(
        &["rm", "--yes"],
        &[
            ("DOCKER_FAKE_CONTAINERS", containers.to_str().unwrap()),
            ("DOCKER_FAKE_FAIL_COMMANDS", "docker_rm"),
        ],
    );

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("nothing else was changed, run ds rm again")
    );
    assert_eq!(project.read(".env"), env);
    assert_eq!(project.machine().read_registry(), registry);
}

#[test]
fn should_refuse_to_remove_when_the_block_of_the_env_is_malformed_and_delete_nothing() {
    let project = git_project();
    project.run(&["run", "--", "true"]);
    let broken = format!("{}\n{BLOCK_START}: broken\n", project.read(".env"));
    project.write(".env", &broken);
    project.machine().clear_docker_log();

    let run = project.run(&["rm", "--yes"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert_eq!(project.read(".env"), broken);
    assert_eq!(project.machine().docker_log(), Vec::<String>::new());
    assert_eq!(instance_count(&project), 1);
}

fn two_registered_projects() -> (Project, Project) {
    let machine = Machine::with_test_slots();
    let alive = Project::from_scenario_on(&machine, "template");
    let gone = Project::from_scenario_on(&machine, "template");
    alive.run(&["run", "--", "true"]);
    gone.run(&["run", "--", "true"]);
    (alive, gone)
}

fn entry_of(registry: &str, path: &Path) -> String {
    registry
        .split("[[instance]]")
        .find(|entry| entry.contains(&format!("path = \"{}\"", path.display())))
        .unwrap()
        .trim()
        .to_owned()
}

#[test]
fn should_forget_only_the_gone_instance_and_leave_the_other_entry_untouched() {
    let (alive, gone) = two_registered_projects();
    let gone_name = name_of(&gone);
    let alive_name = name_of(&alive);
    let gone_path = canonical(&gone);
    let before = alive.machine().read_registry();
    std::fs::remove_dir_all(gone.path()).unwrap();
    alive.machine().clear_docker_log();

    let run = alive.run(&["prune", "--yes"]);

    let after = alive.machine().read_registry();
    let log = alive.machine().docker_log();
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(instance_count(&alive), 1);
    assert!(!after.contains(&gone_path.display().to_string()));
    assert_eq!(
        entry_of(&after, &canonical(&alive)),
        entry_of(&before, &canonical(&alive))
    );
    assert_eq!(log.len(), 3, "{log:?}");
    assert!(log[0].ends_with(&format!("ps -aq --filter {PROJECT_LABEL}={gone_name}")));
    assert!(log[1].ends_with(&format!(
        "volume ls -q --filter {PROJECT_LABEL}={gone_name}"
    )));
    assert!(log[2].ends_with(&format!(
        "network ls -q --filter {PROJECT_LABEL}={gone_name}"
    )));
    assert!(
        !log.iter()
            .any(|line| line.contains(&alive_name) || line.contains(" down"))
    );
}

fn write_ensure_entry(machine: &Machine, root: &Path) -> PathBuf {
    let path = entry_path(&machine.state_directory(), root);
    write_entry(&path, &CacheEntry::refusal(Fingerprint::none())).unwrap();
    path
}

#[test]
fn should_delete_the_ensure_entry_of_a_pruned_checkout_and_keep_the_other() {
    let (alive, gone) = two_registered_projects();
    let gone_entry = write_ensure_entry(alive.machine(), &canonical(&gone));
    let alive_entry = write_ensure_entry(alive.machine(), &canonical(&alive));
    std::fs::remove_dir_all(gone.path()).unwrap();

    let run = alive.run(&["prune", "--yes"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(!gone_entry.exists());
    assert!(alive_entry.exists());
}

#[test]
fn should_delete_the_ensure_entry_of_a_removed_checkout_and_keep_the_other() {
    let (alive, removed) = two_registered_projects();
    let removed_entry = write_ensure_entry(alive.machine(), &canonical(&removed));
    let alive_entry = write_ensure_entry(alive.machine(), &canonical(&alive));

    let run = removed.run(&["rm", "--yes"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(!removed_entry.exists());
    assert!(alive_entry.exists());
}

#[test]
fn should_keep_the_entry_and_exit_1_when_the_cleanup_of_a_gone_instance_fails() {
    let (alive, gone) = two_registered_projects();
    std::fs::remove_dir_all(gone.path()).unwrap();
    let before = alive.machine().read_registry();

    let run = alive.run_with(
        &["prune", "--yes"],
        &[("DOCKER_FAKE_FAIL_COMMANDS", "docker_ps")],
    );

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains("it stays in the registry"));
    assert_eq!(alive.machine().read_registry(), before);
}

#[test]
fn should_change_nothing_when_prune_has_no_terminal_and_no_yes() {
    let (alive, gone) = two_registered_projects();
    let gone_name = name_of(&gone);
    std::fs::remove_dir_all(gone.path()).unwrap();
    let before = alive.machine().read_registry();
    alive.machine().clear_docker_log();

    let run = alive.run(&["prune"]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains(&gone_name));
    assert!(
        run.stderr
            .contains("ds prune deletes data; run it in a terminal or pass --yes")
    );
    assert_eq!(alive.machine().read_registry(), before);
    assert_eq!(alive.machine().docker_log(), Vec::<String>::new());
}

#[test]
fn should_say_there_is_nothing_to_prune_when_every_folder_exists() {
    let (alive, _gone) = two_registered_projects();

    let run = alive.run(&["prune"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stderr.contains("ds: nothing to prune"));
}

#[test]
fn should_neither_create_nor_modify_the_env_when_writing_is_disabled() {
    let project = git_project();
    project
        .machine()
        .write_global_config(&format!("{TEST_SLOTS}\n[write]\nenabled = false\n"));

    let first = project.run(&["run", "--", "true"]);
    project.remove(".env");
    project.write(".env.example", "PORT=3333\n");
    let second = project.run(&["run", "--", "true"]);

    assert_eq!(first.status, 0, "{}", first.stderr);
    assert_eq!(second.status, 0, "{}", second.stderr);
    assert!(!project.exists(".env"));
}

#[test]
fn should_leave_the_env_bytes_alone_when_writing_is_disabled() {
    let project = git_project();
    project
        .machine()
        .write_global_config(&format!("{TEST_SLOTS}\n[write]\nenabled = false\n"));

    project.run(&["run", "--", "true"]);

    assert_eq!(project.read(".env"), FAKE_ENV);
}

#[test]
fn should_give_the_services_state_and_the_urls_of_the_web_ports_in_info() {
    let project = git_project();
    project.run(&["run", "--", "true"]);
    let ps = project.machine().write_scratch(
        "ps.json",
        r#"[{"Service":"postgres","State":"running","Publishers":[{"PublishedPort":14999}]}]"#,
    );

    let running = project.run_with(
        &["info", "--json"],
        &[("DOCKER_FAKE_PS", ps.to_str().unwrap())],
    );
    let none = project.run(&["info", "--json"]);
    let unknown = project.run_with(&["info", "--json"], &[("DOCKER_FAKE_FAIL_COMMANDS", "ps")]);
    let text = project.run_with(&["info"], &[("DOCKER_FAKE_PS", ps.to_str().unwrap())]);

    let info = json_of(&running);
    let ports = info["ports"].as_array().unwrap();
    let port_of = |name: &str| ports.iter().find(|port| port["var"] == name).unwrap();
    assert_eq!(info["services"], "running");
    assert_eq!(json_of(&none)["services"], "none");
    assert_eq!(json_of(&unknown)["services"], "unknown");
    assert_eq!(
        port_of("PORT")["url"],
        format!("http://localhost:{}", port_of("PORT")["port"])
    );
    assert!(port_of("DB_PORT").get("url").is_none());
    assert!(text.stdout.contains("Services:      running\n"));
    assert!(text.stdout.contains("http://localhost:"));
}

#[test]
fn should_still_show_the_info_when_the_env_section_is_invalid() {
    let project = git_project();
    project.run(&["run", "--", "true"]);
    project.write("devslot.local.toml", "[env]\nURL = \"http://x:${NOPE}\"\n");

    let run = project.run(&["info", "--json"]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(json_of(&run)["registered"], true);
}

#[test]
fn should_say_stopped_when_the_containers_exist_and_none_runs() {
    let project = git_project();
    project.run(&["run", "--", "true"]);
    let ps = project.machine().write_scratch(
        "ps.json",
        r#"[{"Service":"postgres","State":"exited","Publishers":[]}]"#,
    );

    let run = project.run_with(
        &["info", "--json"],
        &[("DOCKER_FAKE_PS", ps.to_str().unwrap())],
    );

    assert_eq!(json_of(&run)["services"], "stopped");
}

#[test]
fn should_say_the_checkout_is_opted_out_in_info_after_rm() {
    let project = git_project();
    project.run(&["run", "--", "true"]);
    project.run(&["rm", "--yes"]);

    let json = project.run(&["info", "--json"]);
    let text = project.run(&["info"]);

    assert_eq!(json_of(&json)["registered"], false);
    assert_eq!(json_of(&json)["opted_out"], true);
    assert!(
        text.stdout
            .contains("opted out by ds rm; ds up or ds run registers it again")
    );
}

#[test]
fn should_not_say_opted_out_in_info_for_a_checkout_that_never_was() {
    let project = git_project();

    let json = project.run(&["info", "--json"]);

    assert_eq!(json_of(&json)["opted_out"], false);
}

#[test]
fn should_document_exit_codes_in_the_help_of_each_new_command() {
    let project = Project::empty();

    for command in ["up", "down", "reset", "rm", "prune", "ensure"] {
        let run = project.run(&[command, "--help"]);

        assert_eq!(run.status, 0, "{command}");
        assert!(run.stdout.contains("Exit codes:"), "{command}");
        assert!(run.stdout.contains("XDG_STATE_HOME"), "{command}");
    }
}
