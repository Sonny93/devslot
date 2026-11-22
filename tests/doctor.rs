#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `ds doctor` end to end: the real binary, a fake docker, temp projects.

mod support;

use serde_json::Value;
use support::{FAKE_ENV, Project};

const GITIGNORE_WITH_ENV: &str = ".env\n";

fn json_of(run: &support::Run) -> Value {
    serde_json::from_str(&run.stdout).unwrap()
}

fn finding_checks(report: &Value) -> Vec<String> {
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["check"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn should_exit_0_and_list_the_template_ports_and_warnings_when_the_project_has_no_error() {
    let project = Project::from_scenario("template");

    let run = project.doctor(&["doctor", "--color", "never"]);

    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout
            .contains("PORT           .env             3333     -\n")
    );
    assert!(
        run.stdout
            .contains("VITE_HMR_PORT  package.json     24678    -\n")
    );
    assert!(
        run.stdout
            .contains("DB_PORT        dev.compose.yml  5432     postgres:5432\n")
    );
    assert!(run.stdout.contains(
        "compose.yml (app): service loads .env with env_file but does not pin PORT, VITE_HMR_PORT, DB_PORT"
    ));
    assert!(!run.stdout.contains("does not read VITE_HMR_PORT"));
    assert!(run.stdout.ends_with("0 errors, 1 warning\n"));
}

#[test]
fn should_print_the_report_as_json_when_json_is_asked() {
    let project = Project::from_scenario("template");

    let run = project.doctor(&["doctor", "--json"]);

    let report = json_of(&run);
    assert_eq!(run.status, 0);
    assert_eq!(report["version"], 1);
    assert_eq!(report["compose_file"], "dev.compose.yml");
    assert_eq!(report["summary"]["errors"], 0);
    assert_eq!(report["summary"]["warnings"], 1);
    assert_eq!(report["ports"][2]["var"], "DB_PORT");
    assert_eq!(report["ports"][2]["kind"], "service");
    assert_eq!(report["ports"][2]["service"], "postgres");
    assert_eq!(report["ports"][2]["target"], 5432);
    assert_eq!(finding_checks(&report), ["env_file_ports"]);
}

#[test]
fn should_exit_1_and_name_the_container_names_when_the_compose_files_set_them() {
    let project = Project::from_scenario("my_links");

    let run = project.doctor(&["doctor", "--color", "never"]);

    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout
            .contains("error   compose.yml (my-links): service sets container_name")
    );
    assert!(
        run.stdout
            .contains("error   compose.image.yml (my-links): service sets container_name")
    );
    assert!(run.stdout.contains("apps/webapp/package.json"));
}

#[test]
fn should_count_errors_and_warnings_in_json_when_the_project_has_errors() {
    let project = Project::from_scenario("my_links");

    let run = project.doctor(&["doctor", "--json"]);

    let report = json_of(&run);
    assert_eq!(run.status, 1);
    assert_eq!(report["summary"]["errors"], 2);
    assert_eq!(report["ports"][1]["file"], "apps/webapp/package.json");
    assert_eq!(
        finding_checks(&report)
            .iter()
            .filter(|check| *check == "hardcoded_port")
            .count(),
        2
    );
}

#[test]
fn should_exit_2_when_the_docker_program_does_not_exist() {
    let project = Project::from_scenario("template");

    let run = project.run_with(&["doctor"], &[("DEVSLOT_DOCKER", "/nonexistent/docker")]);

    assert_eq!(run.status, 2);
    assert!(
        run.stderr
            .starts_with("ds: error: cannot run /nonexistent/docker"),
        "{}",
        run.stderr
    );
    assert_eq!(run.stdout, "");
}

#[test]
fn should_exit_2_when_the_command_line_is_invalid() {
    let project = Project::from_scenario("template");

    let run = project.doctor(&["doctor", "--nonsense"]);

    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("unexpected argument"));
}

#[test]
fn should_exit_1_when_compose_rejects_the_dev_compose_file() {
    let project = Project::from_scenario("template");

    let run = project.run_with(
        &["doctor", "--json"],
        &[("DOCKER_FAKE_FAIL", "dev.compose.yml")],
    );

    let report = json_of(&run);
    assert_eq!(run.status, 1);
    assert!(finding_checks(&report).contains(&"compose_unreadable".to_owned()));
}

#[test]
fn should_only_warn_when_compose_rejects_a_file_that_is_not_the_dev_compose() {
    let project = Project::from_scenario("template");

    let run = project.run_with(
        &["doctor", "--json"],
        &[("DOCKER_FAKE_FAIL", "compose.yml")],
    );

    let report = json_of(&run);
    assert_eq!(run.status, 0);
    assert!(finding_checks(&report).contains(&"compose_skipped".to_owned()));
}

#[test]
fn should_exit_1_when_git_does_not_ignore_env() {
    let project = Project::from_scenario("template");
    project.initialize_git("node_modules\n");

    let run = project.doctor(&["doctor", "--json"]);

    assert_eq!(run.status, 1);
    assert!(finding_checks(&json_of(&run)).contains(&"env_not_ignored".to_owned()));
}

#[test]
fn should_not_report_env_as_unignored_when_gitignore_lists_it() {
    let project = Project::from_scenario("template");
    project.initialize_git(GITIGNORE_WITH_ENV);

    let run = project.doctor(&["doctor", "--json"]);

    assert_eq!(run.status, 0);
    assert!(!finding_checks(&json_of(&run)).contains(&"env_not_ignored".to_owned()));
}

#[test]
fn should_report_an_info_line_and_exit_0_when_there_is_no_compose_file() {
    let project = Project::empty();
    project.write(".env", FAKE_ENV);

    let run = project.doctor(&["doctor", "--json"]);

    let report = json_of(&run);
    assert_eq!(run.status, 0);
    assert_eq!(report["compose_file"], Value::Null);
    assert_eq!(finding_checks(&report), ["compose_missing"]);
}

#[test]
fn should_exit_1_when_there_is_no_env_and_nothing_to_create_it_from() {
    let project = Project::empty();

    let run = project.doctor(&["doctor", "--json"]);

    assert_eq!(run.status, 1);
    assert!(finding_checks(&json_of(&run)).contains(&"env_missing".to_owned()));
}

#[test]
fn should_never_print_an_env_value_when_the_env_holds_secrets_in_every_form() {
    let project = Project::from_scenario("template");
    project.write(
        ".env",
        &format!(
            "{FAKE_ENV}DB_SECRET=FAKE-SECRET-1\nDATABASE_URL=postgres://u:FAKE-SECRET-2@localhost:5432/app\nBROKEN LINE FAKE-SECRET-3\n"
        ),
    );

    let text = project.doctor(&["doctor"]);
    let json = project.doctor(&["doctor", "--json"]);

    assert_eq!(text.status, 1);
    assert!(
        text.stdout
            .contains(".env: line 9 is not valid dotenv syntax")
    );
    for output in [&text.stdout, &text.stderr, &json.stdout, &json.stderr] {
        assert!(!output.contains("FAKE-SECRET"), "{output}");
    }
}

#[test]
fn should_warn_without_the_value_when_a_secret_url_hardcodes_the_default_port() {
    let project = Project::from_scenario("template");
    project.write(
        ".env",
        &format!("{FAKE_ENV}DATABASE_URL=postgres://u:FAKE-SECRET@localhost:5432/app\n"),
    );

    let run = project.doctor(&["doctor"]);

    assert!(run.stdout.contains(
        ".env: DATABASE_URL points at a local host on port 5432, the default of DB_PORT"
    ));
    assert!(!run.stdout.contains("FAKE-SECRET"));
}

#[test]
fn should_color_the_level_words_when_color_always_is_asked() {
    let project = Project::from_scenario("my_links");

    let run = project.doctor(&["doctor", "--color", "always"]);

    assert!(run.stdout.contains("\x1b[31merror  \x1b[0m"));
}

#[test]
fn should_not_color_when_stdout_is_not_a_terminal_and_no_color_is_set() {
    let project = Project::from_scenario("my_links");

    let run = project.run_with(&["doctor"], &[("NO_COLOR", "1")]);

    assert!(!run.stdout.contains('\x1b'));
}

#[test]
fn should_document_exit_codes_and_environment_variables_when_help_is_asked() {
    let project = Project::empty();

    let top_level = project.doctor(&["--help"]);
    let doctor = project.doctor(&["doctor", "--help"]);

    assert_eq!(top_level.status, 0);
    for help in [&top_level.stdout, &doctor.stdout] {
        assert!(help.contains("Exit codes:"));
        assert!(help.contains("NO_COLOR"));
        assert!(help.contains("DEVSLOT_DOCKER"));
    }
    assert!(doctor.stdout.contains("  1  at least one error was found"));
}

#[test]
fn should_not_relay_compose_stderr_when_compose_rejects_a_file_with_a_secret_in_its_message() {
    let project = Project::from_scenario("template");
    let stderr = "failed to read /x/.env: line 3: unterminated quoted value \"FAKE-SECRET";

    let text = project.run_with(
        &["doctor"],
        &[
            ("DOCKER_FAKE_FAIL", "dev.compose.yml compose.yml"),
            ("DOCKER_FAKE_STDERR", stderr),
        ],
    );
    let json = project.run_with(
        &["doctor", "--json"],
        &[
            ("DOCKER_FAKE_FAIL", "dev.compose.yml compose.yml"),
            ("DOCKER_FAKE_STDERR", stderr),
        ],
    );

    assert_eq!(text.status, 1);
    assert!(
        text.stdout
            .contains("run docker compose -f 'dev.compose.yml' config --quiet to see why")
    );
    for output in [&text.stdout, &text.stderr, &json.stdout, &json.stderr] {
        assert!(!output.contains("FAKE-SECRET"), "{output}");
    }
}

#[test]
fn should_report_the_env_as_unreadable_without_hanging_when_it_is_a_fifo() {
    let project = Project::from_scenario("template");
    std::fs::remove_file(project.path().join(".env")).unwrap();
    let status = std::process::Command::new("mkfifo")
        .arg(project.path().join(".env"))
        .status()
        .unwrap();
    assert!(status.success());

    let run = project.doctor(&["doctor", "--json"]);

    assert_eq!(run.status, 1);
    assert!(finding_checks(&json_of(&run)).contains(&"env_unreadable".to_owned()));
}

#[test]
fn should_report_the_env_as_unreadable_when_it_is_larger_than_1_mib() {
    let project = Project::from_scenario("template");
    project.write(".env", &"A=1\n".repeat(400_000));

    let run = project.doctor(&["doctor"]);

    assert_eq!(run.status, 1);
    assert!(
        run.stdout
            .contains(".env: .env cannot be read: larger than 1 MiB"),
        "{}",
        run.stdout
    );
}

#[test]
fn should_report_the_line_where_the_quote_opens_when_the_env_has_an_unterminated_value() {
    let project = Project::from_scenario("template");
    project.write(".env", "A=1\nC=2\nB=\"unterminated\nD=3\n");

    let run = project.doctor(&["doctor"]);

    assert!(
        run.stdout
            .contains("the quoted value starting at line 3 is never closed"),
        "{}",
        run.stdout
    );
}

#[test]
fn should_exit_2_when_the_config_file_is_not_a_regular_file() {
    let project = Project::from_scenario("template");
    std::fs::create_dir(project.path().join("devslot.toml")).unwrap();

    let run = project.doctor(&["doctor"]);

    assert_eq!(run.status, 2);
    assert!(
        run.stderr.contains("devslot.toml: not a regular file"),
        "{}",
        run.stderr
    );
}

const FORCE_SHARED_PORT: &str = "[env]\nPORT = \"3333\"\n";

fn registered_checkout(machine: &std::sync::Arc<support::Machine>) -> Project {
    let project = Project::from_scenario_on(machine, "template");
    project.initialize_git(".env\n");
    let run = project.run(&["run", "--", "true"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    project
}

fn two_registered_checkouts() -> (Project, Project) {
    let machine = support::Machine::with_test_slots();
    let first = registered_checkout(&machine);
    let second = registered_checkout(&machine);
    (first, second)
}

#[test]
fn should_warn_when_another_registered_checkout_forces_the_same_port() {
    let (first, second) = two_registered_checkouts();
    first.write("devslot.local.toml", FORCE_SHARED_PORT);
    second.write("devslot.local.toml", FORCE_SHARED_PORT);

    let run = first.doctor(&["doctor", "--json"]);

    let report = json_of(&run);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    let finding = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["check"] == "forced_port_shared")
        .unwrap();
    assert_eq!(finding["level"], "warning");
    assert_eq!(finding["key"], "PORT");
    assert!(
        finding["message"]
            .as_str()
            .unwrap()
            .starts_with("port 3333 (PORT) is also forced by instance ")
    );
}

#[test]
fn should_not_warn_when_the_other_checkout_tracks_its_forcing_file() {
    let (first, second) = two_registered_checkouts();
    first.write("devslot.local.toml", FORCE_SHARED_PORT);
    second.write("devslot.local.toml", FORCE_SHARED_PORT);
    second.git(&["add", "devslot.local.toml"]);

    let run = first.doctor(&["doctor", "--json"]);

    assert!(!finding_checks(&json_of(&run)).contains(&"forced_port_shared".to_owned()));
}

#[test]
fn should_report_an_error_finding_when_the_tunneler_host_is_invalid() {
    let project = Project::from_scenario("template");
    project
        .machine()
        .write_global_config(&format!("{}[tunneler]\nhost = \"\"\n", support::TEST_SLOTS));

    let run = project.doctor(&["doctor", "--json"]);

    let report = json_of(&run);
    let finding = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["check"] == "tunneler_host_invalid")
        .unwrap();
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert_eq!(finding["level"], "error");
    assert_eq!(finding["key"], "tunneler.host");
    assert!(
        finding["message"]
            .as_str()
            .unwrap()
            .starts_with("invalid [tunneler] host in ")
    );
}
