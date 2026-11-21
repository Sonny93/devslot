#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `ds ls` end to end: a pretend machine with two instances, the real binary
//! and the fake docker answering the machine-wide `ps` call.

mod support;

use std::sync::Arc;

use serde_json::Value;
use support::{Machine, Project};

struct TwoInstances {
    machine: Arc<Machine>,
    first: Project,
    second: Project,
}

fn registered_project(machine: &Arc<Machine>) -> Project {
    let project = Project::from_scenario_on(machine, "template");
    project.initialize_git(".env\n");
    let run = project.run(&["run", "--", "true"]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    project
}

fn two_instances() -> TwoInstances {
    let machine = Machine::with_test_slots();
    let first = registered_project(&machine);
    let second = registered_project(&machine);
    TwoInstances {
        machine,
        first,
        second,
    }
}

fn name_of(project: &Project) -> String {
    let run = project.run(&["info", "--json"]);
    let info: Value = serde_json::from_str(&run.stdout).unwrap();
    info["name"].as_str().unwrap().to_owned()
}

fn running_file(machine: &Machine, names: &[&str]) -> String {
    let path = machine.write_scratch("running.txt", &names.join("\n"));
    path.display().to_string()
}

fn listing(project: &Project, arguments: &[&str], environment: &[(&str, &str)]) -> support::Run {
    project.run_with(arguments, environment)
}

#[test]
fn should_list_every_instance_with_its_services_state_when_docker_answers() {
    let machines = two_instances();
    let first_name = name_of(&machines.first);
    let second_name = name_of(&machines.second);
    let running = running_file(&machines.machine, &[&first_name]);

    let run = listing(
        &machines.first,
        &["ls", "--json"],
        &[("DOCKER_FAKE_RUNNING_PROJECTS", &running)],
    );

    let report: Value = serde_json::from_str(&run.stdout).unwrap();
    let state_of = |name: &str| {
        report["instances"]
            .as_array()
            .unwrap()
            .iter()
            .find(|instance| instance["name"] == name)
            .unwrap()["services_running"]
            .clone()
    };
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(report["version"], 1);
    assert_eq!(report["shared"], Value::Array(Vec::new()));
    assert_eq!(report["instances"].as_array().unwrap().len(), 2);
    assert_eq!(state_of(&first_name), Value::Bool(true));
    assert_eq!(state_of(&second_name), Value::Bool(false));
}

#[test]
fn should_give_each_instance_its_path_slot_and_ports_with_a_url_on_web_ports_only() {
    let machines = two_instances();

    let run = listing(&machines.first, &["ls", "--json"], &[]);

    let report: Value = serde_json::from_str(&run.stdout).unwrap();
    let instances = report["instances"].as_array().unwrap();
    let paths: Vec<&str> = instances
        .iter()
        .map(|instance| instance["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.contains(
            &machines
                .first
                .path()
                .canonicalize()
                .unwrap()
                .to_str()
                .unwrap()
        )
    );
    assert!(
        paths.contains(
            &machines
                .second
                .path()
                .canonicalize()
                .unwrap()
                .to_str()
                .unwrap()
        )
    );
    for instance in instances {
        assert!(instance["slot"].as_u64().unwrap() >= 1);
        let kinds: Vec<&str> = instance["ports"]
            .as_array()
            .unwrap()
            .iter()
            .map(|port| port["kind"].as_str().unwrap())
            .collect();
        assert!(
            kinds.contains(&"web") && kinds.contains(&"service"),
            "{kinds:?}"
        );
        for port in instance["ports"].as_array().unwrap() {
            let url = port.get("url").and_then(Value::as_str);
            if port["kind"] == "web" {
                assert_eq!(
                    url,
                    Some(format!("http://localhost:{}", port["port"]).as_str())
                );
            } else {
                assert_eq!(url, None);
                assert_eq!(port["service"], "postgres");
                assert_eq!(port["target"], 5432);
            }
        }
    }
}

#[test]
fn should_give_a_null_state_and_exit_0_when_the_docker_call_fails() {
    let machines = two_instances();

    let run = listing(
        &machines.first,
        &["ls", "--json"],
        &[("DOCKER_FAKE_FAIL_COMMANDS", "docker_ps")],
    );

    let report: Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(run.status, 0, "{}", run.stderr);
    for instance in report["instances"].as_array().unwrap() {
        assert_eq!(instance["services_running"], Value::Null);
    }
}

#[test]
fn should_ask_docker_once_for_the_whole_machine_when_listing() {
    let machines = two_instances();
    machines.machine.clear_docker_log();

    listing(&machines.first, &["ls", "--json"], &[]);

    let log = machines.machine.docker_log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].contains("ps --filter label=com.docker.compose.project --format"));
}

#[test]
fn should_print_a_table_with_the_states_and_the_ports_when_listing_as_text() {
    let machines = two_instances();
    let first_name = name_of(&machines.first);
    let second_name = name_of(&machines.second);
    let running = running_file(&machines.machine, &[&first_name]);

    let run = listing(
        &machines.first,
        &["ls"],
        &[("DOCKER_FAKE_RUNNING_PROJECTS", &running)],
    );

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stdout.starts_with("  NAME"), "{}", run.stdout);
    let row_of = |name: &str| {
        run.stdout
            .lines()
            .find(|line| line.trim_start().starts_with(name))
            .unwrap()
            .to_owned()
    };
    assert!(row_of(&first_name).contains("running"));
    assert!(!row_of(&first_name).contains("not running"));
    assert!(row_of(&second_name).contains("not running"));
    assert!(run.stdout.contains(&format!("Ports of {first_name}")));
    assert!(run.stdout.contains("DB_PORT"));
}

#[test]
fn should_say_unknown_in_text_when_the_docker_call_fails() {
    let machines = two_instances();

    let run = listing(
        &machines.first,
        &["ls"],
        &[("DOCKER_FAKE_FAIL_COMMANDS", "docker_ps")],
    );

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(run.stdout.contains("unknown"));
}

#[test]
fn should_list_the_opted_out_checkouts_in_text_only() {
    let machines = two_instances();
    let rm = machines.second.run(&["rm", "--yes"]);
    assert_eq!(rm.status, 0, "{}", rm.stderr);
    let opted_out = machines.second.path().canonicalize().unwrap();

    let text = listing(&machines.first, &["ls"], &[]);
    let json = listing(&machines.first, &["ls", "--json"], &[]);

    assert!(text.stdout.contains("Opted out"));
    assert!(text.stdout.contains(opted_out.to_str().unwrap()));
    assert!(!json.stdout.contains("Opted out"));
    assert!(!json.stdout.contains("opted_out"));
}

#[test]
fn should_list_no_instance_when_the_registry_is_empty() {
    let project = Project::from_scenario("template");

    let json = project.run(&["ls", "--json"]);
    let text = project.run(&["ls"]);

    let report: Value = serde_json::from_str(&json.stdout).unwrap();
    assert_eq!(report["instances"], Value::Array(Vec::new()));
    assert_eq!(report["shared"], Value::Array(Vec::new()));
    assert_eq!(text.stdout, "No instance is registered on this machine.\n");
}

const FORCE_PORT: &str = "[env]\nPORT = \"3333\"\n";

fn listed<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["instances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|instance| instance["name"] == name)
        .unwrap()
}

fn port_of<'a>(instance: &'a Value, variable: &str) -> &'a Value {
    instance["ports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|port| port["var"] == variable)
        .unwrap()
}

fn list_json(project: &Project) -> Value {
    let run = listing(project, &["ls", "--json"], &[]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    serde_json::from_str(&run.stdout).unwrap()
}

#[test]
fn should_list_the_forced_port_of_an_instance_and_leave_its_neighbour_unchanged() {
    let machines = two_instances();
    let first_name = name_of(&machines.first);
    let second_name = name_of(&machines.second);
    let before = list_json(&machines.first);
    machines.first.write("devslot.local.toml", FORCE_PORT);

    let report = list_json(&machines.first);

    let forced = port_of(listed(&report, &first_name), "PORT");
    assert_eq!(forced["port"], 3333);
    assert_eq!(forced["forced"], true);
    assert_eq!(forced["url"], "http://localhost:3333");
    assert_eq!(listed(&report, &second_name), listed(&before, &second_name));
}

#[test]
fn should_list_the_allocated_ports_when_the_local_file_is_malformed() {
    let machines = two_instances();
    let first_name = name_of(&machines.first);
    let before = list_json(&machines.first);
    machines.first.write("devslot.local.toml", "[env\nPORT = ");

    let report = list_json(&machines.first);

    assert_eq!(listed(&report, &first_name), listed(&before, &first_name));
}

#[test]
fn should_list_the_allocated_ports_when_the_folder_of_an_instance_was_deleted() {
    let machines = two_instances();
    let second_name = name_of(&machines.second);
    let before = list_json(&machines.first);
    std::fs::remove_dir_all(machines.second.path()).unwrap();

    let report = list_json(&machines.first);

    assert_eq!(listed(&report, &second_name), listed(&before, &second_name));
}

#[test]
fn should_list_the_forced_port_when_the_committed_file_is_malformed() {
    let machines = two_instances();
    let first_name = name_of(&machines.first);
    machines.first.write("devslot.local.toml", FORCE_PORT);
    machines.first.write("devslot.toml", "[env\nbroken = ");

    let report = list_json(&machines.first);

    assert_eq!(port_of(listed(&report, &first_name), "PORT")["port"], 3333);
}

#[test]
fn should_list_the_allocated_port_when_the_forced_value_is_not_canonical() {
    let machines = two_instances();
    let first_name = name_of(&machines.first);
    let before = list_json(&machines.first);
    for value in ["0", "03333", "abc"] {
        machines.first.write(
            "devslot.local.toml",
            &format!("[env]\nPORT = \"{value}\"\n"),
        );

        let report = list_json(&machines.first);

        assert_eq!(
            port_of(listed(&report, &first_name), "PORT"),
            port_of(listed(&before, &first_name), "PORT"),
            "value {value}"
        );
    }
}

#[test]
fn should_mark_the_forced_port_in_words_when_listing_as_text() {
    let machines = two_instances();
    machines.first.write("devslot.local.toml", FORCE_PORT);

    let run = listing(&machines.first, &["ls"], &[]);

    assert!(
        run.stdout.contains("3333 (forced in devslot.local.toml)"),
        "{}",
        run.stdout
    );
}

#[test]
fn should_list_the_allocated_port_when_git_tracks_the_local_file() {
    let machines = two_instances();
    let first_name = name_of(&machines.first);
    let before = list_json(&machines.first);
    machines.first.write("devslot.local.toml", FORCE_PORT);
    machines.first.git(&["add", "devslot.local.toml"]);

    let report = list_json(&machines.first);

    assert_eq!(listed(&report, &first_name), listed(&before, &first_name));
}

fn listed_instance(project: &Project, report: &Value) -> Value {
    let path = project.path().canonicalize().unwrap();
    report["instances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|instance| instance["path"] == path.to_str().unwrap())
        .unwrap()
        .clone()
}

#[test]
fn should_give_the_project_and_the_worktree_flag_when_listing_a_main_checkout_and_a_linked_worktree()
 {
    let machine = Machine::with_test_slots();
    let main = Project::from_scenario_on(&machine, "template");
    main.initialize_git_and_commit(".env\n");
    let linked = main.add_worktree("feature");
    for project in [&main, &linked] {
        let run = project.run(&["run", "--", "true"]);
        assert_eq!(run.status, 0, "{}", run.stderr);
    }

    let run = main.run(&["ls", "--json"]);

    let report: Value = serde_json::from_str(&run.stdout).unwrap();
    let project_name = main
        .path()
        .canonicalize()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let main_entry = listed_instance(&main, &report);
    let linked_entry = listed_instance(&linked, &report);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(
        (
            main_entry["project"].as_str(),
            main_entry["worktree"].as_bool(),
            linked_entry["project"].as_str(),
            linked_entry["worktree"].as_bool()
        ),
        (
            Some(project_name.as_str()),
            Some(false),
            Some(project_name.as_str()),
            Some(true)
        )
    );
}

#[test]
fn should_give_null_project_and_worktree_when_the_entry_has_no_origin_keys() {
    let machines = two_instances();
    machines.machine.strip_origin_from_registry();

    let run = listing(&machines.first, &["ls", "--json"], &[]);

    let report: Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(run.status, 0, "{}", run.stderr);
    for instance in report["instances"].as_array().unwrap() {
        assert_eq!(instance["project"], Value::Null);
        assert_eq!(instance["worktree"], Value::Null);
        assert_eq!(instance["app_url"], Value::Null);
    }
}
