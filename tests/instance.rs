#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `ds run`, `ds env` and `ds info` end to end: the real binary, a fake
//! docker, temp projects and a pretend machine with its own config and
//! registry. The slot range is far from the default one.

mod support;

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;
use support::{Machine, Project};

const TEST_FIRST_PORT: u64 = 14000;
const TEST_LAST_PORT: u64 = 14999;
const TEST_SLOT_SIZE: u64 = 10;

fn json_of(run: &support::Run) -> Value {
    serde_json::from_str(&run.stdout).unwrap()
}

fn environment_of(project: &Project) -> BTreeMap<String, String> {
    let run = project.run_with(&["env", "--format", "json"], &[]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    serde_json::from_str(&run.stdout).unwrap()
}

fn port_of(environment: &BTreeMap<String, String>, variable: &str) -> u64 {
    environment[variable].parse().unwrap()
}

fn files_under(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let name = path.strip_prefix(root).unwrap().display().to_string();
            files.insert(name, std::fs::read(&path).unwrap());
        }
    }
    files
}

#[test]
fn should_run_the_command_with_the_instance_environment_and_return_its_exit_code() {
    let project = Project::from_scenario("template");

    let run = project.run_with(
        &["run", "--", "sh", "-c", "echo $PORT-$DB_PORT; exit 7"],
        &[],
    );

    let environment = environment_of(&project);
    assert_eq!(run.status, 7, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        format!("{}-{}\n", environment["PORT"], environment["DB_PORT"])
    );
}

#[test]
fn should_give_the_template_three_consecutive_ports_in_the_test_slot_range_in_spec_order() {
    let project = Project::from_scenario("template");

    project.run_with(&["run", "--", "true"], &[]);

    let environment = environment_of(&project);
    let port = port_of(&environment, "PORT");
    assert!((TEST_FIRST_PORT + TEST_SLOT_SIZE..=TEST_LAST_PORT).contains(&port));
    assert_eq!(
        (
            port_of(&environment, "VITE_HMR_PORT"),
            port_of(&environment, "DB_PORT")
        ),
        (port + 1, port + 2)
    );
}

#[test]
fn should_set_the_compose_project_the_compose_file_and_the_devslot_ids() {
    let project = Project::from_scenario("template");
    project.run_with(&["run", "--", "true"], &[]);

    let environment = environment_of(&project);

    let name = &environment["COMPOSE_PROJECT_NAME"];
    assert_eq!(&environment["DEVSLOT_INSTANCE"], name);
    assert!(environment["DEVSLOT_SLOT"].parse::<u32>().unwrap() >= 1);
    assert_eq!(
        environment["COMPOSE_FILE"],
        format!(
            "{}/dev.compose.yml",
            project.path().canonicalize().unwrap().display()
        )
    );
}

#[test]
fn should_announce_the_registration_on_stderr_the_first_time_only() {
    let project = Project::from_scenario("template");

    let first = project.run_with(&["run", "--", "true"], &[]);
    let second = project.run_with(&["run", "--", "true"], &[]);

    assert!(first.stderr.contains("ds: registered "));
    assert!(first.stderr.contains(", slot "));
    assert!(first.stderr.contains("keep their old project name"));
    assert!(!second.stderr.contains("registered"));
    assert_eq!(first.stdout, "");
}

#[test]
fn should_keep_the_same_ports_when_run_a_second_time() {
    let project = Project::from_scenario("template");
    project.run_with(&["run", "--", "true"], &[]);
    let first = environment_of(&project);
    let registry_before = project.machine().read_registry();

    project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(environment_of(&project), first);
    assert_eq!(project.machine().read_registry(), registry_before);
}

#[test]
fn should_write_only_the_devslot_block_in_the_project_when_running_and_nothing_when_reading() {
    let project = Project::from_scenario("template");
    let before = files_under(project.path());

    project.run_with(&["run", "--", "true"], &[]);
    let after_run = files_under(project.path());
    project.run_with(&["env"], &[]);
    project.run_with(&["info"], &[]);

    let changed: Vec<&String> = after_run
        .keys()
        .filter(|name| before.get(*name) != after_run.get(*name))
        .collect();
    assert_eq!(changed, [".env"]);
    assert_eq!(files_under(project.path()), after_run);
}

#[test]
fn should_write_the_registry_like_the_spec_example() {
    let project = Project::from_scenario("template");

    project.run_with(&["run", "--", "true"], &[]);

    let registry = project.machine().read_registry();
    let root = project.path().canonicalize().unwrap();
    assert!(registry.starts_with("version = 1\n\n[[instance]]\n"));
    assert!(registry.contains(&format!("path = \"{}\"\n", root.display())));
    assert!(registry.contains("\nslot = "));
    assert!(registry.contains("ports = { PORT = "));
    assert!(registry.contains(", VITE_HMR_PORT = "));
    assert!(
        registry.contains(
            "DB_PORT = { kind = \"service\", service = \"postgres\", target = 5432 } }\n"
        )
    );
}

#[test]
fn should_exit_1_with_a_message_when_env_runs_in_an_unregistered_checkout() {
    let project = Project::from_scenario("template");

    let run = project.run_with(&["env"], &[]);

    assert_eq!(run.status, 1);
    assert_eq!(run.stdout, "");
    assert_eq!(
        run.stderr,
        "ds: error: this checkout is not registered; run ds up or ds run\n"
    );
}

#[test]
fn should_exit_0_and_suggest_ds_up_when_info_runs_in_an_unregistered_checkout() {
    let project = Project::from_scenario("template");

    let text = project.run_with(&["info"], &[]);
    let json = project.run_with(&["info", "--json"], &[]);

    assert_eq!(text.status, 0);
    assert!(
        text.stdout
            .contains("This checkout is not registered. Run ds up")
    );
    assert_eq!(json.status, 0);
    assert_eq!(json_of(&json)["registered"], false);
    assert_eq!(json_of(&json)["version"], 1);
}

#[test]
fn should_show_the_instance_in_info_json_with_the_shape_of_ds_ls() {
    let project = Project::from_scenario("template");
    project.run_with(&["run", "--", "true"], &[]);

    let run = project.run_with(&["info", "--json"], &[]);

    let info = json_of(&run);
    assert_eq!(info["registered"], true);
    assert!(info["slot"].as_u64().unwrap() >= 1);
    assert_eq!(info["compose_file"], "dev.compose.yml");
    assert_eq!(info["compose_file_origin"], "detected");
    assert_eq!(info["ports"][0]["var"], "PORT");
    assert_eq!(info["ports"][0]["kind"], "web");
    assert_eq!(info["ports"][2]["var"], "DB_PORT");
    assert_eq!(info["ports"][2]["kind"], "service");
    assert_eq!(info["ports"][2]["service"], "postgres");
    assert_eq!(info["ports"][2]["target"], 5432);
}

#[test]
fn should_give_the_project_and_the_worktree_flag_in_info_json_for_a_main_checkout_and_a_linked_worktree()
 {
    let machine = Machine::with_test_slots();
    let main = Project::from_scenario_on(&machine, "template");
    main.initialize_git_and_commit(".env\n");
    let linked = main.add_worktree("feature");
    for project in [&main, &linked] {
        let run = project.run(&["run", "--", "true"]);
        assert_eq!(run.status, 0, "{}", run.stderr);
    }

    let main_info = json_of(&main.run(&["info", "--json"]));
    let linked_info = json_of(&linked.run(&["info", "--json"]));

    let project_name = main
        .path()
        .canonicalize()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        (
            main_info["project"].as_str(),
            main_info["worktree"].as_bool(),
            linked_info["project"].as_str(),
            linked_info["worktree"].as_bool()
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
fn should_print_the_instance_as_text_in_info() {
    let project = Project::from_scenario("template");
    project.run_with(&["run", "--", "true"], &[]);

    let run = project.run_with(&["info"], &[]);

    assert_eq!(run.status, 0);
    assert!(run.stdout.contains("Slot:          "));
    assert!(
        run.stdout
            .contains("Compose file:  dev.compose.yml (detected)\n")
    );
    assert!(run.stdout.contains("DB_PORT"));
    assert!(run.stdout.contains("postgres:5432"));
}

#[test]
fn should_print_export_lines_in_the_shell_format_by_default() {
    let project = Project::from_scenario("template");
    project.run_with(&["run", "--", "true"], &[]);

    let run = project.run_with(&["env"], &[]);

    assert_eq!(run.status, 0);
    assert!(run.stdout.starts_with("export PORT='"));
    assert!(run.stdout.contains("\nexport COMPOSE_PROJECT_NAME='"));
    assert!(run.stdout.contains("\nexport DEVSLOT_SLOT='"));
}

#[test]
fn should_set_the_run_marker_to_the_instance_name_in_what_run_launches() {
    let project = Project::from_scenario("template");

    let run = project.run_with(&["run", "--", "sh", "-c", "echo $DEVSLOT_RUN"], &[]);

    let environment = environment_of(&project);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stdout, format!("{}\n", environment["DEVSLOT_INSTANCE"]));
}

#[test]
fn should_never_export_the_run_marker_in_env_or_in_the_env_block() {
    let project = Project::from_scenario("template");
    project.run_with(&["run", "--", "true"], &[]);

    let shell = project.run_with(&["env"], &[]);
    let json = project.run_with(&["env", "--format", "json"], &[]);

    assert!(!shell.stdout.contains("DEVSLOT_RUN"));
    assert!(!json.stdout.contains("DEVSLOT_RUN"));
    assert!(!project.read(".env").contains("DEVSLOT_RUN"));
}

#[test]
fn should_never_print_a_value_of_the_env_file_in_env_or_info() {
    let project = Project::from_scenario("template");
    project.run_with(&["run", "--", "true"], &[]);

    let outputs = [
        project.run_with(&["env"], &[]),
        project.run_with(&["env", "--format", "json"], &[]),
        project.run_with(&["info"], &[]),
        project.run_with(&["info", "--json"], &[]),
    ];

    for run in outputs {
        assert!(!run.stdout.contains("FAKE") && !run.stderr.contains("FAKE"));
    }
}

#[test]
fn should_replace_a_port_variable_of_the_environment_and_show_both_values() {
    let project = Project::from_scenario("template");

    let run = project.run_with(&["run", "--", "true"], &[("PORT", "1")]);

    let port = port_of(&environment_of(&project), "PORT");
    assert!(
        run.stderr.contains(&format!(
            "ds: warning: replacing PORT from the environment with {port}\n"
        )),
        "{}",
        run.stderr
    );
}

#[test]
fn should_replace_the_variable_without_a_warning_when_the_environment_already_has_the_value() {
    let project = Project::from_scenario("template");
    project.run_with(&["run", "--", "true"], &[]);
    let port = environment_of(&project)["PORT"].clone();

    let run = project.run_with(&["run", "--", "true"], &[("PORT", &port)]);

    assert!(!run.stderr.contains("replacing"));
}

#[test]
fn should_name_a_replaced_env_key_without_printing_either_value() {
    let project = Project::from_scenario("template");
    project.write(
        "devslot.local.toml",
        "[env]\nAPI_BASE = \"http://localhost:${PORT}\"\n",
    );

    let run = project.run_with(&["run", "--", "true"], &[("API_BASE", "FAKE-SECRET-VALUE")]);

    assert!(
        run.stderr
            .contains("ds: warning: replacing API_BASE from the environment\n")
    );
    assert!(!run.stderr.contains("FAKE-SECRET-VALUE"));
    assert!(!run.stderr.contains("http://localhost"));
}

#[test]
fn should_build_the_env_values_from_the_assigned_ports() {
    let project = Project::from_scenario("template");
    project.write(
        "devslot.local.toml",
        "[env]\nVITE_API_URL = \"http://localhost:${PORT}/api\"\n",
    );
    project.run_with(&["run", "--", "true"], &[]);

    let environment = environment_of(&project);

    assert_eq!(
        environment["VITE_API_URL"],
        format!("http://localhost:{}/api", environment["PORT"])
    );
}

#[test]
fn should_exit_2_naming_the_key_when_env_refers_to_an_unknown_variable() {
    let project = Project::from_scenario("template");
    project.write("devslot.local.toml", "[env]\nURL = \"http://x:${NOPE}\"\n");

    let run = project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("[env] key URL refers to ${NOPE}"));
}

#[test]
fn should_exit_2_naming_the_key_and_file_when_an_env_key_is_reserved() {
    let project = Project::from_scenario("template");
    project.write("devslot.toml", "[env]\nCOMPOSE_PROJECT_NAME = \"x\"\n");

    let run = project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("COMPOSE_PROJECT_NAME"));
    assert!(run.stderr.contains("devslot.toml"));
}

#[test]
fn should_block_run_with_exit_1_when_the_committed_config_forces_a_managed_port() {
    let project = Project::from_scenario("template");
    project.write("devslot.toml", "[env]\nPORT = \"3333\"\n");

    let run = project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 1);
    assert!(run.stderr.contains("[env] sets a managed port variable"));
    assert!(!project.machine().registry_path().exists());
}

#[test]
fn should_force_a_managed_port_when_the_local_config_sets_it() {
    let project = Project::from_scenario("template");
    project.write("devslot.local.toml", "[env]\nPORT = \"3333\"\n");

    let run = project.run_with(&["run", "--", "sh", "-c", "echo $PORT"], &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "3333\n");
    assert!(project.machine().read_registry().contains("PORT = 140"));
}

#[test]
fn should_exit_1_and_register_nothing_when_the_dev_compose_sets_a_container_name() {
    let project = Project::from_scenario("template");
    let fixtures = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/template/docker");
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let text = std::fs::read_to_string(entry.path()).unwrap();
        let text = text.replace(
            "\"restart\": \"always\"",
            "\"restart\": \"always\", \"container_name\": \"fixed\"",
        );
        std::fs::write(fixtures.path().join(entry.file_name()), text).unwrap();
    }

    let run = project.run_with(
        &["run", "--color", "never", "--", "true"],
        &[("DOCKER_FAKE_FIXTURES", fixtures.path().to_str().unwrap())],
    );

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("error   dev.compose.yml (postgres): service sets container_name")
    );
    assert!(!project.machine().registry_path().exists());
}

#[test]
fn should_exit_127_when_the_program_does_not_exist() {
    let project = Project::from_scenario("template");

    let run = project.run_with(&["run", "--", "definitely-not-a-program"], &[]);

    assert_eq!(run.status, 127);
    assert!(
        run.stderr
            .contains("ds: error: cannot run definitely-not-a-program")
    );
}

#[test]
fn should_exit_126_when_the_file_is_not_executable() {
    let project = Project::from_scenario("template");
    project.write("script.sh", "#!/bin/sh\ntrue\n");

    let run = project.run_with(&["run", "--", "./script.sh"], &[]);

    assert_eq!(run.status, 126, "{}", run.stderr);
}

#[test]
fn should_not_run_the_arguments_through_a_shell() {
    let project = Project::from_scenario("template");

    let run = project.run_with(&["run", "--", "echo", "$(id) ; `id` && $HOME"], &[]);

    assert_eq!(run.stdout, "$(id) ; `id` && $HOME\n");
}

#[test]
fn should_release_the_registry_lock_before_the_command_starts() {
    let project = Project::from_scenario("template");
    project.run_with(&["run", "--", "true"], &[]);
    let lock = project.machine().lock_path();

    let run = project.run_with(
        &["run", "--", "flock", "-n", lock.to_str().unwrap(), "true"],
        &[],
    );

    assert_eq!(run.status, 0, "{}", run.stderr);
}

#[test]
fn should_give_two_parallel_runs_two_different_slots_and_list_both() {
    let machine = Machine::with_test_slots();
    let first = Project::from_scenario_on(&machine, "template");
    let second = Project::from_scenario_on(&machine, "template");

    std::thread::scope(|scope| {
        scope.spawn(|| first.run_with(&["run", "--", "true"], &[]));
        scope.spawn(|| second.run_with(&["run", "--", "true"], &[]));
    });

    let one = environment_of(&first);
    let two = environment_of(&second);
    assert_ne!(one["DEVSLOT_SLOT"], two["DEVSLOT_SLOT"]);
    assert_ne!(one["PORT"], two["PORT"]);
    assert_eq!(machine.read_registry().matches("[[instance]]").count(), 2);
}

#[test]
fn should_keep_the_first_instance_untouched_when_a_second_one_registers() {
    let machine = Machine::with_test_slots();
    let first = Project::from_scenario_on(&machine, "template");
    let second = Project::from_scenario_on(&machine, "template");
    first.run_with(&["run", "--", "true"], &[]);
    let hand_edited = machine
        .read_registry()
        .replace("version = 1\n", "# hand written\nversion = 1\n");
    std::fs::write(machine.registry_path(), &hand_edited).unwrap();

    second.run_with(&["run", "--", "true"], &[]);

    let after = machine.read_registry();
    assert!(after.starts_with(hand_edited.trim_end()));
}

#[test]
fn should_refuse_with_exit_2_and_leave_a_malformed_registry_untouched() {
    let project = Project::from_scenario("template");
    let registry = project.machine().registry_path();
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    std::fs::write(&registry, "version = 9\n# mine\n").unwrap();

    let run = project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("malformed"));
    assert_eq!(
        std::fs::read_to_string(&registry).unwrap(),
        "version = 9\n# mine\n"
    );
}

#[test]
fn should_exit_2_naming_the_key_when_the_global_config_is_out_of_bounds() {
    let project = Project::from_scenario("template");
    project
        .machine()
        .write_global_config("[slots]\nsize = 500\n");

    let run = project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("[slots] size"));
}

#[test]
fn should_exit_1_with_the_config_path_when_the_slot_is_too_small_for_the_ports() {
    let project = Project::from_scenario("template");
    project
        .machine()
        .write_global_config("[slots]\nfirst_port = 14000\nlast_port = 14999\nsize = 2\n");

    let run = project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 1);
    assert!(
        run.stderr
            .contains("3 ports requested, slots hold 2; raise [slots] size in ")
    );
    assert!(run.stderr.contains("config.toml"));
}

#[test]
fn should_warn_in_doctor_when_the_slot_size_changed_after_registration() {
    let project = Project::from_scenario("template");
    project.run_with(&["run", "--", "true"], &[]);
    project
        .machine()
        .write_global_config("[slots]\nfirst_port = 14000\nlast_port = 14999\nsize = 20\n");

    let run = project.run_with(&["doctor", "--json"], &[]);

    let checks: Vec<String> = json_of(&run)["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["check"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        checks.contains(&"slot_geometry_changed".to_owned()),
        "{checks:?}"
    );
}

#[test]
fn should_not_warn_in_doctor_when_the_slot_geometry_is_unchanged() {
    let project = Project::from_scenario("template");
    project.run_with(&["run", "--", "true"], &[]);

    let run = project.run_with(&["doctor", "--json"], &[]);

    assert!(!run.stdout.contains("slot_geometry_changed"));
}

#[test]
fn should_register_a_second_project_in_another_slot_with_the_same_machine() {
    let machine = Machine::with_test_slots();
    let first = Project::from_scenario_on(&machine, "template");
    let second = Project::from_scenario_on(&machine, "my_links");
    first.run_with(&["run", "--", "true"], &[]);

    let run = second.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    let (one, two) = (environment_of(&first), environment_of(&second));
    assert_ne!(one["DEVSLOT_SLOT"], two["DEVSLOT_SLOT"]);
    assert_ne!(one["PORT"], two["PORT"]);
}

#[test]
fn should_document_exit_codes_and_environment_variables_in_the_help_of_each_command() {
    let project = Project::empty();

    for command in ["run", "env", "info"] {
        let run = project.run_with(&[command, "--help"], &[]);

        assert_eq!(run.status, 0);
        assert!(run.stdout.contains("Exit codes:"), "{command}");
        assert!(run.stdout.contains("XDG_STATE_HOME"), "{command}");
    }
    let run = project.run_with(&["run", "--help"], &[]);
    assert!(run.stdout.contains("126"));
    assert!(run.stdout.contains("127"));
    assert!(run.stdout.contains("DEVSLOT_DOCKER"));
}

#[test]
fn should_exit_2_when_run_has_no_command() {
    let project = Project::empty();

    let run = project.run_with(&["run"], &[]);

    assert_eq!(run.status, 2);
}

fn git_in(project: &Project, arguments: &[&str]) {
    let status = std::process::Command::new("git")
        .args(arguments)
        .current_dir(project.path())
        .status()
        .unwrap();
    assert!(status.success());
}

fn checks_of(run: &support::Run) -> Vec<String> {
    json_of(run)["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["check"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn should_refuse_an_extra_port_name_that_carries_shell_code_with_exit_2() {
    let project = Project::from_scenario("template");
    project.write(
        "devslot.toml",
        "[ports]\nextra = [\"X=1; echo PWNED >&2; Y\"]\n",
    );

    let run = project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("[ports] name"));
    assert!(!project.machine().registry_path().exists());
}

#[test]
fn should_refuse_path_as_an_extra_port_name() {
    let project = Project::from_scenario("template");
    project.write("devslot.toml", "[ports]\nextra = [\"PATH\"]\n");

    let run = project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 2);
}

#[test]
fn should_refuse_a_denied_env_key_from_the_committed_file() {
    let project = Project::from_scenario("template");
    project.write("devslot.toml", "[env]\nGIT_CONFIG_COUNT = \"1\"\n");

    let run = project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("GIT_CONFIG_COUNT"));
}

#[test]
fn should_block_run_with_exit_1_when_the_committed_env_key_is_declared_nowhere() {
    let project = Project::from_scenario("template");
    project.write("devslot.toml", "[env]\nFOO = \"x\"\n");

    let run = project.run_with(&["run", "--color", "never", "--", "true"], &[]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("devslot.toml: [env] sets a key that neither .env nor .env.example declares")
    );
    assert!(!project.machine().registry_path().exists());
}

#[test]
fn should_report_the_undeclared_key_in_doctor_as_an_error_without_its_value() {
    let project = Project::from_scenario("template");
    project.write("devslot.toml", "[env]\nFOO = \"FAKE-VALUE\"\n");

    let run = project.run_with(&["doctor", "--json"], &[]);

    assert_eq!(run.status, 1);
    assert!(checks_of(&run).contains(&"env_key_undeclared".to_owned()));
    assert!(!run.stdout.contains("FAKE-VALUE"));
}

#[test]
fn should_accept_a_committed_env_key_declared_in_the_example() {
    let project = Project::from_scenario("template");
    project.write(
        ".env.example",
        "DATABASE_URL=postgres://localhost:5432/app\n",
    );
    project.write(
        "devslot.toml",
        "[env]\nDATABASE_URL = \"postgres://localhost:${DB_PORT}/app\"\n",
    );

    let run = project.run_with(&["run", "--", "sh", "-c", "echo $DATABASE_URL"], &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    let port = environment_of(&project)["DB_PORT"].clone();
    assert_eq!(run.stdout, format!("postgres://localhost:{port}/app\n"));
}

#[test]
fn should_accept_a_local_env_key_that_no_env_file_declares() {
    let project = Project::from_scenario("template");
    project.write("devslot.local.toml", "[env]\nFOO = \"x\"\n");

    let run = project.run_with(&["run", "--", "sh", "-c", "echo $FOO"], &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "x\n");
}

#[test]
fn should_accept_an_integer_env_value() {
    let project = Project::from_scenario("template");
    project.write("devslot.local.toml", "[env]\nPORT = 3333\n");

    let run = project.run_with(&["run", "--", "sh", "-c", "echo $PORT"], &[]);

    assert_eq!(run.stdout, "3333\n");
}

#[test]
fn should_not_echo_a_non_string_env_value_in_the_error() {
    let project = Project::from_scenario("template");
    project.write("devslot.toml", "[env]\nPIN = 1.5\n");

    let run = project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 2);
    assert!(
        run.stderr
            .contains("the value of PIN in devslot.toml must be a string or an integer")
    );
    assert!(!run.stderr.contains("1.5"));
}

#[test]
fn should_not_leak_the_caller_value_of_a_repository_chosen_port_variable() {
    let project = Project::from_scenario("template");
    project.write(
        "devslot.local.toml",
        "[ports]\nextra = [\"FAKE_API_TOKEN\"]\n",
    );

    let run = project.run_with(
        &["run", "--", "true"],
        &[("FAKE_API_TOKEN", "sk-fake-not-real-42")],
    );

    assert!(
        run.stderr
            .contains("ds: warning: replacing FAKE_API_TOKEN from the environment with ")
    );
    assert!(!run.stderr.contains("sk-fake-not-real-42"));
}

#[test]
fn should_block_run_and_doctor_when_git_tracks_the_local_config() {
    let project = Project::from_scenario("template");
    project.initialize_git(".env\n");
    project.write("devslot.local.toml", "[env]\nFOO = \"x\"\n");
    git_in(&project, &["add", "devslot.local.toml"]);

    let run = project.run_with(&["run", "--", "true"], &[]);
    let doctor = project.run_with(&["doctor", "--json"], &[]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("devslot.local.toml: git tracks this file")
    );
    assert_eq!(doctor.status, 1);
    assert!(checks_of(&doctor).contains(&"local_config_tracked".to_owned()));
}

#[test]
fn should_accept_an_untracked_local_config_inside_git() {
    let project = Project::from_scenario("template");
    project.initialize_git(".env\n");
    project.write("devslot.local.toml", "[env]\nFOO = \"x\"\n");

    let run = project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
}

#[test]
fn should_block_when_a_local_forced_port_belongs_to_another_instance() {
    let machine = Machine::with_test_slots();
    let first = Project::from_scenario_on(&machine, "template");
    let second = Project::from_scenario_on(&machine, "template");
    first.run_with(&["run", "--", "true"], &[]);
    let taken = environment_of(&first)["DB_PORT"].clone();
    second.write(
        "devslot.local.toml",
        &format!("[env]\nDB_PORT = \"{taken}\"\n"),
    );

    let run = second.run_with(&["run", "--", "true"], &[]);
    let doctor = second.run_with(&["doctor", "--color", "never"], &[]);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(
        run.stderr
            .contains(&format!("DB_PORT is forced to port {taken}"))
    );
    assert_eq!(doctor.status, 1);
    assert!(doctor.stdout.contains("DB_PORT is forced to port"));
}

fn fixtures_with_volume_variant() -> tempfile::TempDir {
    let fixtures = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/template/docker");
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), fixtures.path().join(entry.file_name())).unwrap();
    }
    let plain =
        std::fs::read_to_string(fixtures.path().join("dev.compose.yml.config.json")).unwrap();
    let variant = plain.replace(
        "\"name\": \"devslot-probe_postgres-volume\"",
        "\"name\": \"other_pgdata\"",
    );
    assert_ne!(plain, variant);
    std::fs::write(
        fixtures
            .path()
            .join("dev.compose.yml.config.other_pgdata.json"),
        variant,
    )
    .unwrap();
    fixtures
}

#[test]
fn should_see_the_env_values_in_the_checks_when_they_make_a_volume_name_explicit() {
    let project = Project::from_scenario("template");
    project.write(
        "devslot.local.toml",
        "[env]\nDB_VOLUME = \"other_pgdata\"\n",
    );
    let fixtures = fixtures_with_volume_variant();
    let override_fixtures = [("DOCKER_FAKE_FIXTURES", fixtures.path().to_str().unwrap())];

    let run = project.run_with(
        &["run", "--color", "never", "--", "true"],
        &override_fixtures,
    );
    let doctor = project.run_with(&["doctor", "--json"], &override_fixtures);

    assert_eq!(run.status, 1, "{}", run.stderr);
    assert!(run.stderr.contains("has a fixed name"));
    assert!(!run.stderr.contains("other_pgdata"));
    assert!(checks_of(&doctor).contains(&"explicit_name".to_owned()));
    assert!(!doctor.stdout.contains("other_pgdata"));
}

#[test]
fn should_not_leak_a_numeric_caller_value_of_a_repository_chosen_port_variable() {
    let project = Project::from_scenario("template");
    project.write(
        "devslot.local.toml",
        "[ports]\nextra = [\"FAKE_API_TOKEN\"]\n",
    );

    let run = project.run_with(&["run", "--", "true"], &[("FAKE_API_TOKEN", "12345")]);

    assert!(
        run.stderr
            .contains("replacing FAKE_API_TOKEN from the environment with ")
    );
    assert!(!run.stderr.contains("12345"));
}

#[test]
fn should_pass_committed_env_values_to_the_command_but_not_print_them_in_env() {
    let project = Project::from_scenario("template");
    project.write(".env.example", "DATABASE_URL=x\n");
    project.write(
        "devslot.toml",
        "[env]\nDATABASE_URL = \"postgres://localhost:${DB_PORT}/app\"\n",
    );
    project.write("devslot.local.toml", "[env]\nMY_OWN = \"mine\"\n");

    let run = project.run_with(&["run", "--", "sh", "-c", "echo $DATABASE_URL"], &[]);
    let shell = project.run_with(&["env"], &[]);
    let json = project.run_with(&["env", "--format", "json"], &[]);
    let info = project.run_with(&["info", "--json"], &[]);

    assert!(run.stdout.starts_with("postgres://localhost:"));
    for output in [&shell.stdout, &json.stdout, &info.stdout] {
        assert!(!output.contains("DATABASE_URL"));
        assert!(!output.contains("postgres://"));
    }
    assert!(shell.stdout.contains("export MY_OWN='mine'\n"));
    assert_eq!(json_of(&json)["MY_OWN"], "mine");
}

#[test]
fn should_refuse_prompt_and_compiler_variables_in_env() {
    let project = Project::from_scenario("template");
    for key in [
        "npm_config_script_shell",
        "PROMPT",
        "RUSTC_WRAPPER",
        "CC",
        "cargo_home",
    ] {
        project.write("devslot.local.toml", &format!("[env]\n{key} = \"x\"\n"));

        let run = project.run_with(&["run", "--", "true"], &[]);

        assert_eq!(run.status, 2, "{key}");
    }
}

#[test]
fn should_refuse_a_committed_extra_port_that_does_not_end_in_port() {
    let project = Project::from_scenario("template");
    project.write("devslot.toml", "[ports]\nextra = [\"KUBECONFIG\"]\n");

    let run = project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("KUBECONFIG") && run.stderr.contains("devslot.toml"));
}

#[test]
fn should_accept_a_local_extra_port_without_the_suffix_and_a_committed_lowercase_one() {
    let project = Project::from_scenario("template");
    project.write("devslot.toml", "[ports]\nextra = [\"front_port\"]\n");
    project.write("devslot.local.toml", "[ports]\nextra = [\"KUBECONFIG\"]\n");

    let run = project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 0, "{}", run.stderr);
}

#[test]
fn should_leave_a_compose_variable_without_the_suffix_unmanaged_and_warn() {
    let project = Project::from_scenario("template");
    let fixtures = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/template/docker");
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let text = std::fs::read_to_string(entry.path()).unwrap();
        std::fs::write(
            fixtures.path().join(entry.file_name()),
            text.replace("DB_PORT", "PSQLRC"),
        )
        .unwrap();
    }
    let overrides = [("DOCKER_FAKE_FIXTURES", fixtures.path().to_str().unwrap())];

    let run = project.run_with(&["run", "--color", "never", "--", "true"], &overrides);
    let doctor = project.run_with(&["doctor", "--json"], &overrides);
    let environment = environment_of(&project);

    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stderr.contains(
            "dev.compose.yml (postgres): PSQLRC is not managed: rename it to end in _PORT"
        )
    );
    assert!(checks_of(&doctor).contains(&"port_variable_unmanaged".to_owned()));
    assert!(!environment.contains_key("PSQLRC"));
    assert!(!project.machine().read_registry().contains("PSQLRC"));
}

#[test]
fn should_give_the_instance_a_localhost_subdomain_host_in_env_run_and_info() {
    let project = Project::from_scenario("template");
    let run = project.run_with(&["run", "--", "sh", "-c", "echo $DEVSLOT_HOST"], &[]);

    let environment = environment_of(&project);
    let shell = project.run_with(&["env"], &[]);
    let info = json_of(&project.run_with(&["info", "--json"], &[]));

    let host = &environment["DEVSLOT_HOST"];
    assert!(host.ends_with(".localhost"));
    assert!(host.chars().all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || character == '-'
            || character == '.'
    }));
    assert_eq!(run.stdout, format!("{host}\n"));
    assert!(
        shell
            .stdout
            .contains(&format!("export DEVSLOT_HOST='{host}'\n"))
    );
    assert_eq!(info["host"], host.as_str());
}

#[test]
fn should_substitute_the_host_in_an_env_value() {
    let project = Project::from_scenario("template");
    project.write(
        "devslot.local.toml",
        "[env]\nAPP_URL = \"http://${DEVSLOT_HOST}:${PORT}\"\n",
    );
    project.run_with(&["run", "--", "true"], &[]);

    let environment = environment_of(&project);

    assert_eq!(
        environment["APP_URL"],
        format!(
            "http://{}:{}",
            environment["DEVSLOT_HOST"], environment["PORT"]
        )
    );
}

#[test]
fn should_keep_every_url_on_localhost_and_give_no_app_url_without_a_web_port_setting() {
    let project = Project::from_scenario("template");
    project.run_with(&["run", "--", "true"], &[]);

    let info = json_of(&project.run_with(&["info", "--json"], &[]));

    assert_eq!(info["app_url"], Value::Null);
    for port in info["ports"].as_array().unwrap() {
        if let Some(url) = port["url"].as_str() {
            assert!(url.starts_with("http://localhost:"), "{url}");
        }
    }
}

#[test]
fn should_open_only_the_named_web_port_on_the_host_when_web_port_is_set() {
    let project = Project::from_scenario("template");
    project.write("devslot.toml", "[web]\nport = \"PORT\"\n");
    project.run_with(&["run", "--", "true"], &[]);
    let environment = environment_of(&project);

    let info = json_of(&project.run_with(&["info", "--json"], &[]));

    let app_url = format!(
        "http://{}:{}",
        environment["DEVSLOT_HOST"], environment["PORT"]
    );
    let urls: BTreeMap<&str, Option<&str>> = info["ports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|port| (port["var"].as_str().unwrap(), port["url"].as_str()))
        .collect();
    assert_eq!(info["app_url"], app_url.as_str());
    assert_eq!(urls["PORT"], Some(app_url.as_str()));
    assert!(urls["VITE_HMR_PORT"].is_none_or(|url| url.starts_with("http://localhost:")));
}

#[test]
fn should_exit_2_naming_the_key_when_web_port_is_a_host_or_a_url() {
    let project = Project::from_scenario("template");
    project.write(
        "devslot.local.toml",
        "[web]\nport = \"http://evil.example\"\n",
    );

    let run = project.run_with(&["run", "--", "true"], &[]);

    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("[web] port"));
    assert!(!run.stderr.contains("evil.example"));
}

#[test]
fn should_warn_in_doctor_and_keep_urls_on_localhost_when_web_port_is_not_a_managed_port() {
    let project = Project::from_scenario("template");
    project.write("devslot.toml", "[web]\nport = \"NOPE_PORT\"\n");

    let doctor = project.run_with(&["doctor", "--json"], &[]);
    project.run_with(&["run", "--", "true"], &[]);
    let info = json_of(&project.run_with(&["info", "--json"], &[]));

    let report: Value = serde_json::from_str(&doctor.stdout).unwrap();
    let finding = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["check"] == "web_port_unmanaged")
        .unwrap();
    assert_eq!(finding["level"], "warning");
    assert_eq!(finding["key"], "[web] port");
    assert_eq!(info["app_url"], Value::Null);
}

const FORCE_WEB_PORT: &str = "[web]\nport = \"PORT\"\n\n[env]\nPORT = \"3333\"\n";

fn port_entry<'a>(info: &'a Value, variable: &str) -> &'a Value {
    info["ports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|port| port["var"] == variable)
        .unwrap()
}

#[test]
fn should_report_the_forced_port_with_its_urls_in_info_json_when_the_local_file_forces_it() {
    let project = Project::from_scenario("template");
    project.initialize_git(".env\n");
    project.write("devslot.local.toml", "[web]\nport = \"PORT\"\n");
    project.run_with(&["run", "--", "true"], &[]);
    let allocated = json_of(&project.run_with(&["info", "--json"], &[]));
    project.write("devslot.local.toml", FORCE_WEB_PORT);

    let info = json_of(&project.run_with(&["info", "--json"], &[]));

    let host = info["host"].as_str().unwrap();
    let forced = port_entry(&info, "PORT");
    assert_eq!(forced["port"], 3333);
    assert_eq!(forced["forced"], true);
    assert_eq!(forced["url"], format!("http://{host}:3333"));
    assert_eq!(info["app_url"], format!("http://{host}:3333"));
    for variable in ["VITE_HMR_PORT", "DB_PORT"] {
        assert_eq!(
            port_entry(&info, variable),
            port_entry(&allocated, variable)
        );
        assert!(port_entry(&info, variable).get("forced").is_none());
    }
}

#[test]
fn should_leave_the_registry_untouched_when_info_reports_a_forced_port() {
    let project = Project::from_scenario("template");
    project.run_with(&["run", "--", "true"], &[]);
    project.write("devslot.local.toml", FORCE_WEB_PORT);
    let registry_before = project.machine().read_registry();

    project.run_with(&["info", "--json"], &[]);

    assert_eq!(project.machine().read_registry(), registry_before);
}

#[test]
fn should_mark_the_forced_port_in_words_in_info_text_when_the_local_file_forces_it() {
    let project = Project::from_scenario("template");
    project.initialize_git(".env\n");
    project.run_with(&["run", "--", "true"], &[]);
    project.write("devslot.local.toml", FORCE_WEB_PORT);

    let run = project.run_with(&["info"], &[]);

    assert!(
        run.stdout.contains("3333 (forced in devslot.local.toml)"),
        "{}",
        run.stdout
    );
}

#[test]
fn should_keep_the_allocated_port_in_info_json_when_the_forced_value_is_not_canonical() {
    let project = Project::from_scenario("template");
    project.run_with(&["run", "--", "true"], &[]);
    let allocated = json_of(&project.run_with(&["info", "--json"], &[]));
    for value in ["0", "03333", "abc"] {
        project.write(
            "devslot.local.toml",
            &format!("[env]\nPORT = \"{value}\"\n"),
        );

        let info = json_of(&project.run_with(&["info", "--json"], &[]));

        assert_eq!(
            port_entry(&info, "PORT"),
            port_entry(&allocated, "PORT"),
            "value {value}"
        );
    }
}

#[test]
fn should_keep_the_allocated_port_in_info_json_when_git_tracks_the_local_file() {
    let project = Project::from_scenario("template");
    project.initialize_git(".env\n");
    project.run_with(&["run", "--", "true"], &[]);
    let allocated = json_of(&project.run_with(&["info", "--json"], &[]));
    project.write("devslot.local.toml", FORCE_WEB_PORT);
    project.git(&["add", "devslot.local.toml"]);

    let info = json_of(&project.run_with(&["info", "--json"], &[]));

    assert_eq!(port_entry(&info, "PORT"), port_entry(&allocated, "PORT"));
    assert!(port_entry(&info, "PORT").get("forced").is_none());
}
