//! The Docker commands that act on an instance's compose project: start,
//! stop, remove, list containers and volumes. They show Compose's own output
//! and keep the caller's environment, unlike the read-only probes.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::docker::{DockerError, docker_program, run_with_timeout};
use crate::process::spawn_with_retry;

const COMPOSE_VARIABLE_PREFIX: &str = "COMPOSE_";
const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";
const COMPOSE_VOLUME_LABEL: &str = "com.docker.compose.volume";
const UP_WAIT_TIMEOUT_SECONDS: &str = "120";
const CONTAINERS_TIMEOUT: Duration = Duration::from_secs(5);
const LISTING_TIMEOUT: Duration = Duration::from_secs(30);
const MODEL_TIMEOUT: Duration = Duration::from_secs(30);
const NOT_PUBLISHED: u16 = 0;
const RUNNING_PROJECTS_FORMAT: &str = "{{.Label \"com.docker.compose.project\"}}";

/// Why a command that acts on an instance did not complete. No message quotes
/// Docker's output.
#[derive(Debug, Error)]
pub enum RunnerError {
    /// The program cannot be started.
    #[error("cannot run {program}: {reason}")]
    Unavailable {
        /// The program that was asked for.
        program: String,
        /// Why it could not start.
        reason: String,
    },
    /// The call did not finish in time.
    #[error("{program} did not answer within {seconds} seconds")]
    TimedOut {
        /// The program that was running.
        program: String,
        /// The time limit.
        seconds: u64,
    },
    /// Docker ran and exited with a failure.
    #[error("docker {command} failed{}", exit_code_suffix(*.code))]
    Failed {
        /// The devslot name of the command (`up`, `down`, ...).
        command: &'static str,
        /// The exit code, absent when a signal ended the process.
        code: Option<i32>,
    },
    /// Docker succeeded but its output is not what was expected.
    #[error("docker gave an output that cannot be read ({stage})")]
    UnexpectedOutput {
        /// The call whose output was unreadable.
        stage: &'static str,
    },
}

fn exit_code_suffix(code: Option<i32>) -> String {
    code.map(|code| format!(" (exit code {code})"))
        .unwrap_or_default()
}

/// The compose project a command acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeTarget {
    /// The compose project, always passed as `-p`.
    pub project_name: String,
    /// The compose files, each passed as `-f`; empty for a deleted folder.
    pub files: Vec<PathBuf>,
    /// The working directory of the docker process.
    pub working_directory: PathBuf,
    /// The instance environment, added on top of the caller's.
    pub environment: Vec<(String, String)>,
}

/// One container of a compose project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerState {
    /// The compose service the container belongs to.
    pub service: String,
    /// The container state (`running`, `exited`, ...).
    pub state: String,
    /// The host ports the container publishes, sorted, without duplicates.
    pub published_ports: Vec<u16>,
}

/// The commands that act on an instance's compose project.
pub trait ComposeRunner {
    /// `up -d --wait` for `services` (all of them when empty).
    fn up(&self, target: &ComposeTarget, services: &[String]) -> Result<(), RunnerError>;

    /// `down`: removes the containers and networks, keeps the volumes.
    fn down(&self, target: &ComposeTarget) -> Result<(), RunnerError>;

    /// `config --format json` with the environment and folder `up` will use,
    /// so `.env` is loaded: the model as Compose will really resolve it.
    /// Nothing Compose prints on stderr is kept.
    fn resolved_model(&self, target: &ComposeTarget) -> Result<String, RunnerError>;

    /// `rm -s -f -v`: stops and removes one service's container and its
    /// anonymous volumes.
    fn remove_service(&self, target: &ComposeTarget, service: &str) -> Result<(), RunnerError>;

    /// `ps -a`: every container of the project, stopped ones included.
    fn containers(&self, target: &ComposeTarget) -> Result<Vec<ContainerState>, RunnerError>;

    /// The names of the volumes of `project_name`, only those labelled with
    /// `volume_key` when it is given. Found by label alone: no compose file.
    fn project_volumes(
        &self,
        project_name: &str,
        volume_key: Option<&str>,
    ) -> Result<Vec<String>, RunnerError>;

    /// The compose projects that have at least one running container, for the
    /// whole machine, found by label alone in one call.
    fn running_projects(&self) -> Result<BTreeSet<String>, RunnerError>;

    /// `volume rm`: no call when `names` is empty.
    fn remove_volumes(&self, names: &[String]) -> Result<(), RunnerError>;

    /// The ids of the containers of `project_name`, stopped ones included,
    /// found by label alone.
    fn project_containers(&self, project_name: &str) -> Result<Vec<String>, RunnerError>;

    /// `rm -f`: no call when `ids` is empty.
    fn remove_containers(&self, ids: &[String]) -> Result<(), RunnerError>;

    /// The ids of the networks of `project_name`, found by label alone.
    fn project_networks(&self, project_name: &str) -> Result<Vec<String>, RunnerError>;

    /// `network rm`: no call when `ids` is empty.
    fn remove_networks(&self, ids: &[String]) -> Result<(), RunnerError>;
}

/// Deletes the containers, volumes and networks that carry the compose project
/// label `project_name`, without reading any compose file, `.env` or folder:
/// nothing a repository writes can choose what is deleted.
pub fn delete_project_data(
    runner: &dyn ComposeRunner,
    project_name: &str,
) -> Result<(), RunnerError> {
    runner.remove_containers(&runner.project_containers(project_name)?)?;
    runner.remove_volumes(&runner.project_volumes(project_name, None)?)?;
    runner.remove_networks(&runner.project_networks(project_name)?)
}

/// The real `docker`.
#[derive(Debug, Clone)]
pub struct DockerRunner {
    program: OsString,
    containers_timeout: Duration,
    listing_timeout: Duration,
}

impl DockerRunner {
    /// Uses `$DEVSLOT_DOCKER`, else `docker`, with the standard timeouts.
    pub fn from_environment() -> Self {
        Self::new(docker_program())
    }

    /// Uses `program` with the standard timeouts.
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            containers_timeout: CONTAINERS_TIMEOUT,
            listing_timeout: LISTING_TIMEOUT,
        }
    }

    fn program_name(&self) -> String {
        self.program.to_string_lossy().into_owned()
    }

    fn compose_command(&self, target: &ComposeTarget, tail: &[&str]) -> Command {
        let mut command = self.plain_command(&target.environment);
        command.current_dir(&target.working_directory).args([
            "compose",
            "-p",
            &target.project_name,
        ]);
        for file in &target.files {
            command.arg("-f").arg(file);
        }
        command.args(tail);
        command
    }

    fn plain_command(&self, environment: &[(String, String)]) -> Command {
        let mut command = Command::new(&self.program);
        for (name, _) in std::env::vars_os().filter(|(name, _)| is_compose_variable_name(name)) {
            command.env_remove(name);
        }
        command.envs(environment.iter().map(|(name, value)| (name, value)));
        command
    }

    fn run_visible(&self, mut command: Command, name: &'static str) -> Result<(), RunnerError> {
        command
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let status = spawn_with_retry(&mut command)
            .and_then(|mut child| child.wait())
            .map_err(|error| RunnerError::Unavailable {
                program: self.program_name(),
                reason: error.to_string(),
            })?;
        if status.success() {
            return Ok(());
        }
        Err(RunnerError::Failed {
            command: name,
            code: status.code(),
        })
    }

    fn run_captured(
        &self,
        mut command: Command,
        timeout: Duration,
        name: &'static str,
    ) -> Result<String, RunnerError> {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        let output = run_with_timeout(command, timeout, &self.program_name())
            .map_err(|error| runner_error(error, name))?;
        if output.success {
            return Ok(output.stdout);
        }
        Err(RunnerError::Failed {
            command: name,
            code: None,
        })
    }
}

fn runner_error(error: DockerError, name: &'static str) -> RunnerError {
    match error {
        DockerError::Unavailable { program, reason } => {
            RunnerError::Unavailable { program, reason }
        }
        DockerError::TimedOut { program, seconds } => RunnerError::TimedOut { program, seconds },
        DockerError::Rejected => RunnerError::Failed {
            command: name,
            code: None,
        },
        DockerError::UnexpectedOutput { stage } => RunnerError::UnexpectedOutput { stage },
    }
}

fn is_compose_variable_name(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name.starts_with(COMPOSE_VARIABLE_PREFIX))
}

impl ComposeRunner for DockerRunner {
    fn up(&self, target: &ComposeTarget, services: &[String]) -> Result<(), RunnerError> {
        let mut tail = vec![
            "up",
            "-d",
            "--wait",
            "--wait-timeout",
            UP_WAIT_TIMEOUT_SECONDS,
        ];
        tail.extend(services.iter().map(String::as_str));
        self.run_visible(self.compose_command(target, &tail), "up")
    }

    fn down(&self, target: &ComposeTarget) -> Result<(), RunnerError> {
        self.run_visible(self.compose_command(target, &["down"]), "down")
    }

    fn resolved_model(&self, target: &ComposeTarget) -> Result<String, RunnerError> {
        let command = self.compose_command(target, &["config", "--format", "json"]);
        self.run_captured(command, MODEL_TIMEOUT, "config")
    }

    fn remove_service(&self, target: &ComposeTarget, service: &str) -> Result<(), RunnerError> {
        let command = self.compose_command(target, &["rm", "-s", "-f", "-v", service]);
        self.run_visible(command, "rm")
    }

    fn containers(&self, target: &ComposeTarget) -> Result<Vec<ContainerState>, RunnerError> {
        let command = self.compose_command(target, &["ps", "-a", "--format", "json"]);
        let text = self.run_captured(command, self.containers_timeout, "ps")?;
        parse_containers(&text)
    }

    fn project_volumes(
        &self,
        project_name: &str,
        volume_key: Option<&str>,
    ) -> Result<Vec<String>, RunnerError> {
        let mut command = self.plain_command(&[]);
        command
            .args(["volume", "ls", "-q"])
            .args(["--filter".to_owned(), project_filter(project_name)]);
        if let Some(key) = volume_key {
            command.args([
                "--filter".to_owned(),
                format!("label={COMPOSE_VOLUME_LABEL}={key}"),
            ]);
        }
        self.list_names(command, "volume ls")
    }

    fn running_projects(&self) -> Result<BTreeSet<String>, RunnerError> {
        let mut command = self.plain_command(&[]);
        command
            .args(["ps", "--filter", "label=com.docker.compose.project"])
            .args(["--format", RUNNING_PROJECTS_FORMAT]);
        let text = self.run_captured(command, self.containers_timeout, "ps")?;
        Ok(text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn remove_volumes(&self, names: &[String]) -> Result<(), RunnerError> {
        self.remove_all(&["volume", "rm"], names, "volume rm")
    }

    fn project_containers(&self, project_name: &str) -> Result<Vec<String>, RunnerError> {
        let mut command = self.plain_command(&[]);
        command
            .args(["ps", "-aq"])
            .args(["--filter".to_owned(), project_filter(project_name)]);
        self.list_names(command, "ps")
    }

    fn remove_containers(&self, ids: &[String]) -> Result<(), RunnerError> {
        self.remove_all(&["rm", "-f", "-v"], ids, "rm")
    }

    fn project_networks(&self, project_name: &str) -> Result<Vec<String>, RunnerError> {
        let mut command = self.plain_command(&[]);
        command
            .args(["network", "ls", "-q"])
            .args(["--filter".to_owned(), project_filter(project_name)]);
        self.list_names(command, "network ls")
    }

    fn remove_networks(&self, ids: &[String]) -> Result<(), RunnerError> {
        self.remove_all(&["network", "rm"], ids, "network rm")
    }
}

fn project_filter(project_name: &str) -> String {
    format!("label={COMPOSE_PROJECT_LABEL}={project_name}")
}

impl DockerRunner {
    fn list_names(&self, command: Command, name: &'static str) -> Result<Vec<String>, RunnerError> {
        let text = self.run_captured(command, self.listing_timeout, name)?;
        Ok(text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn remove_all(
        &self,
        base: &[&str],
        names: &[String],
        name: &'static str,
    ) -> Result<(), RunnerError> {
        if names.is_empty() {
            return Ok(());
        }
        let mut command = self.plain_command(&[]);
        command.args(base).args(names);
        self.run_visible(command, name)
    }
}

#[derive(Deserialize)]
struct RawContainer {
    #[serde(rename = "Service")]
    service: String,
    #[serde(rename = "State")]
    state: String,
    #[serde(rename = "Publishers", default)]
    publishers: Option<Vec<RawPublisher>>,
}

#[derive(Deserialize)]
struct RawPublisher {
    #[serde(rename = "PublishedPort", default)]
    published_port: u16,
}

fn parse_containers(text: &str) -> Result<Vec<ContainerState>, RunnerError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let raw_containers = if trimmed.starts_with('[') {
        serde_json::from_str::<Vec<RawContainer>>(trimmed)
    } else {
        trimmed
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str::<RawContainer>)
            .collect()
    }
    .map_err(|_| RunnerError::UnexpectedOutput { stage: "ps" })?;
    Ok(raw_containers
        .into_iter()
        .map(into_container_state)
        .collect())
}

fn into_container_state(raw: RawContainer) -> ContainerState {
    let mut published_ports: Vec<u16> = raw
        .publishers
        .unwrap_or_default()
        .iter()
        .map(|publisher| publisher.published_port)
        .filter(|port| *port != NOT_PUBLISHED)
        .collect();
    published_ports.sort_unstable();
    published_ports.dedup();
    ContainerState {
        service: raw.service,
        state: raw.state,
        published_ports,
    }
}

/// An in-memory [`ComposeRunner`] for tests of the code that drives it.
#[cfg(test)]
pub mod testing {
    use std::cell::RefCell;

    use super::*;

    /// One call the fake received.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RecordedCall {
        /// The command name: `up`, `down`, `config`, `rm`, `ps`, `volume ls`,
        /// `volume rm`, `docker ps`, `docker rm`, `network ls` or `network rm`.
        pub command: &'static str,
        /// The compose project, empty for `volume rm`.
        pub project: String,
        /// The compose files of the target.
        pub files: Vec<PathBuf>,
        /// The services (`up`, `rm`), the volume key (`volume ls`) or the
        /// volume names (`volume rm`).
        pub services: Vec<String>,
        /// The instance environment of the target.
        pub environment: Vec<(String, String)>,
    }

    /// A runner that records its calls and answers from its fields.
    #[derive(Debug, Default)]
    pub struct FakeComposeRunner {
        containers: Vec<ContainerState>,
        volumes: Vec<String>,
        project_containers: Vec<String>,
        networks: Vec<String>,
        running_projects: Vec<String>,
        model: Option<String>,
        failing_commands: Vec<&'static str>,
        calls: RefCell<Vec<RecordedCall>>,
    }

    impl FakeComposeRunner {
        /// Answers `containers` with `containers`.
        #[must_use]
        pub fn with_containers(mut self, containers: Vec<ContainerState>) -> Self {
            self.containers = containers;
            self
        }

        /// Answers `project_volumes` with `volumes`.
        #[must_use]
        pub fn with_volumes(mut self, volumes: Vec<String>) -> Self {
            self.volumes = volumes;
            self
        }

        /// Answers `project_containers` with `ids`.
        #[must_use]
        pub fn with_project_containers(mut self, ids: Vec<String>) -> Self {
            self.project_containers = ids;
            self
        }

        /// Answers `project_networks` with `ids`.
        #[must_use]
        pub fn with_networks(mut self, ids: Vec<String>) -> Self {
            self.networks = ids;
            self
        }

        /// Answers `running_projects` with `names`.
        #[must_use]
        pub fn with_running_projects(mut self, names: Vec<String>) -> Self {
            self.running_projects = names;
            self
        }

        /// Answers `resolved_model` with `json`.
        #[must_use]
        pub fn with_model(mut self, json: &str) -> Self {
            self.model = Some(json.to_owned());
            self
        }

        /// Makes the commands named `commands` (as in [`RecordedCall::command`])
        /// fail.
        #[must_use]
        pub fn with_failing_commands(mut self, commands: Vec<&'static str>) -> Self {
            self.failing_commands = commands;
            self
        }

        /// Every call received so far, in order.
        pub fn calls(&self) -> Vec<RecordedCall> {
            self.calls.borrow().clone()
        }

        fn record(
            &self,
            command: &'static str,
            target: Option<&ComposeTarget>,
            services: Vec<String>,
        ) -> Result<(), RunnerError> {
            self.calls.borrow_mut().push(RecordedCall {
                command,
                project: target
                    .map(|target| target.project_name.clone())
                    .unwrap_or_default(),
                files: target
                    .map(|target| target.files.clone())
                    .unwrap_or_default(),
                services,
                environment: target
                    .map(|target| target.environment.clone())
                    .unwrap_or_default(),
            });
            if self.failing_commands.contains(&command) {
                return Err(RunnerError::Failed {
                    command,
                    code: Some(1),
                });
            }
            Ok(())
        }
    }

    impl ComposeRunner for FakeComposeRunner {
        fn up(&self, target: &ComposeTarget, services: &[String]) -> Result<(), RunnerError> {
            self.record("up", Some(target), services.to_vec())
        }

        fn down(&self, target: &ComposeTarget) -> Result<(), RunnerError> {
            self.record("down", Some(target), Vec::new())
        }

        fn resolved_model(&self, target: &ComposeTarget) -> Result<String, RunnerError> {
            self.record("config", Some(target), Vec::new())?;
            Ok(self.model.clone().unwrap_or_else(|| "{}".to_owned()))
        }

        fn remove_service(&self, target: &ComposeTarget, service: &str) -> Result<(), RunnerError> {
            self.record("rm", Some(target), vec![service.to_owned()])
        }

        fn containers(&self, target: &ComposeTarget) -> Result<Vec<ContainerState>, RunnerError> {
            self.record("ps", Some(target), Vec::new())?;
            Ok(self.containers.clone())
        }

        fn project_volumes(
            &self,
            project_name: &str,
            volume_key: Option<&str>,
        ) -> Result<Vec<String>, RunnerError> {
            self.record_project("volume ls", project_name, volume_key.map(str::to_owned))?;
            Ok(self.volumes.clone())
        }

        fn running_projects(&self) -> Result<BTreeSet<String>, RunnerError> {
            self.record("docker ps", None, Vec::new())?;
            Ok(self.running_projects.iter().cloned().collect())
        }

        fn remove_volumes(&self, names: &[String]) -> Result<(), RunnerError> {
            self.record_removal("volume rm", names)
        }

        fn project_containers(&self, project_name: &str) -> Result<Vec<String>, RunnerError> {
            self.record_project("docker ps", project_name, None)?;
            Ok(self.project_containers.clone())
        }

        fn remove_containers(&self, ids: &[String]) -> Result<(), RunnerError> {
            self.record_removal("docker rm", ids)
        }

        fn project_networks(&self, project_name: &str) -> Result<Vec<String>, RunnerError> {
            self.record_project("network ls", project_name, None)?;
            Ok(self.networks.clone())
        }

        fn remove_networks(&self, ids: &[String]) -> Result<(), RunnerError> {
            self.record_removal("network rm", ids)
        }
    }

    impl FakeComposeRunner {
        fn record_project(
            &self,
            command: &'static str,
            project_name: &str,
            key: Option<String>,
        ) -> Result<(), RunnerError> {
            let target = ComposeTarget {
                project_name: project_name.to_owned(),
                files: Vec::new(),
                working_directory: PathBuf::new(),
                environment: Vec::new(),
            };
            self.record(command, Some(&target), key.into_iter().collect())
        }

        fn record_removal(
            &self,
            command: &'static str,
            names: &[String],
        ) -> Result<(), RunnerError> {
            if names.is_empty() {
                return Ok(());
            }
            self.record(command, None, names.to_vec())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use pretty_assertions::assert_eq;

    use super::*;

    fn script(directory: &Path, body: &str) -> PathBuf {
        let path = directory.join("fake-docker");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn target(directory: &Path) -> ComposeTarget {
        ComposeTarget {
            project_name: "web-1".to_owned(),
            files: vec![PathBuf::from("dev.compose.yml"), PathBuf::from("o.yml")],
            working_directory: directory.to_path_buf(),
            environment: vec![("DB_PORT".to_owned(), "61000".to_owned())],
        }
    }

    fn logging_runner(directory: &Path) -> (DockerRunner, PathBuf) {
        let log = directory.join("log");
        let program = script(
            directory,
            &format!("printf '%s\\n' \"$*\" >> '{}'", log.display()),
        );
        (DockerRunner::new(program), log)
    }

    fn logged(log: &Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn should_pass_the_up_flags_and_services_when_starting() {
        let directory = tempfile::tempdir().unwrap();
        let (runner, log) = logging_runner(directory.path());

        runner
            .up(&target(directory.path()), &["db".to_owned()])
            .unwrap();

        assert_eq!(
            logged(&log),
            ["compose -p web-1 -f dev.compose.yml -f o.yml up -d --wait --wait-timeout 120 db"]
        );
    }

    #[test]
    fn should_list_the_running_projects_once_each_when_asked() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("log");
        let program = script(
            directory.path(),
            &format!(
                "printf '%s\\n' \"$*\" >> '{}'\nprintf 'app-1\\napp-2\\napp-1\\n'",
                log.display()
            ),
        );

        let projects = DockerRunner::new(program).running_projects().unwrap();

        assert_eq!(projects.into_iter().collect::<Vec<_>>(), ["app-1", "app-2"]);
        assert_eq!(
            logged(&log),
            [
                r#"ps --filter label=com.docker.compose.project --format {{.Label "com.docker.compose.project"}}"#
            ]
        );
    }

    #[test]
    fn should_pass_the_down_flags_when_stopping() {
        let directory = tempfile::tempdir().unwrap();
        let (runner, log) = logging_runner(directory.path());
        let target = target(directory.path());

        runner.down(&target).unwrap();

        assert_eq!(
            logged(&log),
            ["compose -p web-1 -f dev.compose.yml -f o.yml down"]
        );
    }

    #[test]
    fn should_pass_the_remove_flags_when_removing_a_service() {
        let directory = tempfile::tempdir().unwrap();
        let (runner, log) = logging_runner(directory.path());

        runner
            .remove_service(&target(directory.path()), "db")
            .unwrap();

        assert_eq!(
            logged(&log),
            ["compose -p web-1 -f dev.compose.yml -f o.yml rm -s -f -v db"]
        );
    }

    #[test]
    fn should_omit_the_file_flags_when_the_target_has_no_files() {
        let directory = tempfile::tempdir().unwrap();
        let (runner, log) = logging_runner(directory.path());
        let target = ComposeTarget {
            files: Vec::new(),
            ..target(directory.path())
        };

        runner.down(&target).unwrap();

        assert_eq!(logged(&log), ["compose -p web-1 down"]);
    }

    #[test]
    fn should_add_the_instance_environment_and_run_in_the_working_directory_when_acting() {
        let directory = tempfile::tempdir().unwrap();
        let out = directory.path().join("out");
        let program = script(
            directory.path(),
            &format!("echo \"$DB_PORT $(pwd -P)\" > '{}'", out.display()),
        );

        DockerRunner::new(program)
            .down(&target(directory.path()))
            .unwrap();

        let expected = format!(
            "61000 {}",
            directory.path().canonicalize().unwrap().display()
        );
        assert_eq!(std::fs::read_to_string(out).unwrap().trim(), expected);
    }

    #[test]
    fn should_recognise_only_compose_variables_when_filtering_the_environment() {
        assert!(is_compose_variable_name(OsStr::new("COMPOSE_FILE")));
        assert!(is_compose_variable_name(OsStr::new("COMPOSE_PROJECT_NAME")));
        assert!(!is_compose_variable_name(OsStr::new("DOCKER_HOST")));
        assert!(!is_compose_variable_name(OsStr::new("PATH")));
    }

    #[test]
    fn should_report_the_exit_code_when_a_visible_command_fails() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(directory.path(), "exit 3");

        let error = DockerRunner::new(program)
            .up(&target(directory.path()), &[])
            .unwrap_err();

        assert!(matches!(
            error,
            RunnerError::Failed {
                command: "up",
                code: Some(3)
            }
        ));
        assert_eq!(error.to_string(), "docker up failed (exit code 3)");
    }

    #[test]
    fn should_report_unavailable_when_the_program_does_not_exist() {
        let directory = tempfile::tempdir().unwrap();

        let error = DockerRunner::new("/nonexistent/docker")
            .down(&target(directory.path()))
            .unwrap_err();

        assert!(matches!(error, RunnerError::Unavailable { .. }));
    }

    #[test]
    fn should_parse_the_containers_when_compose_prints_an_array() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(
            directory.path(),
            r#"echo '[{"Service":"db","State":"running","Publishers":[{"PublishedPort":61000,"TargetPort":5432},{"PublishedPort":61000,"TargetPort":5432},{"PublishedPort":0,"TargetPort":80}]},{"Service":"app","State":"exited","Publishers":null}]'"#,
        );

        let containers = DockerRunner::new(program)
            .containers(&target(directory.path()))
            .unwrap();

        assert_eq!(
            containers,
            [
                ContainerState {
                    service: "db".to_owned(),
                    state: "running".to_owned(),
                    published_ports: vec![61000],
                },
                ContainerState {
                    service: "app".to_owned(),
                    state: "exited".to_owned(),
                    published_ports: Vec::new(),
                }
            ]
        );
    }

    #[test]
    fn should_parse_the_containers_when_compose_prints_one_object_per_line() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(
            directory.path(),
            r#"printf '%s\n%s\n' '{"Service":"db","State":"running"}' '{"Service":"app","State":"running","Publishers":[{"PublishedPort":3000}]}'"#,
        );

        let containers = DockerRunner::new(program)
            .containers(&target(directory.path()))
            .unwrap();

        assert_eq!(containers.len(), 2);
        assert_eq!(containers[1].published_ports, [3000]);
    }

    #[test]
    fn should_return_no_containers_when_the_output_is_empty() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(directory.path(), "true");

        let containers = DockerRunner::new(program)
            .containers(&target(directory.path()))
            .unwrap();

        assert_eq!(containers, []);
    }

    #[test]
    fn should_report_unexpected_output_without_quoting_it_when_the_containers_are_not_json() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(directory.path(), "echo FAKE-SECRET-not-json");

        let error = DockerRunner::new(program)
            .containers(&target(directory.path()))
            .unwrap_err();

        assert!(matches!(
            error,
            RunnerError::UnexpectedOutput { stage: "ps" }
        ));
        assert!(!error.to_string().contains("FAKE-SECRET"));
        assert!(!format!("{error:?}").contains("FAKE-SECRET"));
    }

    #[test]
    fn should_time_out_when_the_containers_call_runs_too_long() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(directory.path(), "exec sleep 5");
        let runner = DockerRunner {
            containers_timeout: Duration::from_millis(100),
            ..DockerRunner::new(program)
        };

        let error = runner.containers(&target(directory.path())).unwrap_err();

        assert!(matches!(error, RunnerError::TimedOut { .. }));
    }

    #[test]
    fn should_list_the_volume_names_when_docker_prints_them() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(directory.path(), "printf 'a\\n\\nb\\n'");

        let names = DockerRunner::new(program)
            .project_volumes("web-1", Some("db"))
            .unwrap();

        assert_eq!(names, ["a", "b"]);
    }

    #[test]
    fn should_pass_the_label_filters_when_listing_volumes() {
        let directory = tempfile::tempdir().unwrap();
        let (runner, log) = logging_runner(directory.path());

        runner.project_volumes("web-1", Some("db")).unwrap();

        assert_eq!(
            logged(&log),
            [
                "volume ls -q --filter label=com.docker.compose.project=web-1 --filter label=com.docker.compose.volume=db"
            ]
        );
    }

    #[test]
    fn should_filter_on_the_project_label_alone_when_listing_every_volume() {
        let directory = tempfile::tempdir().unwrap();
        let (runner, log) = logging_runner(directory.path());

        runner.project_volumes("web-1", None).unwrap();

        assert_eq!(
            logged(&log),
            ["volume ls -q --filter label=com.docker.compose.project=web-1"]
        );
    }

    #[test]
    fn should_list_and_remove_containers_and_networks_by_label_without_compose() {
        let directory = tempfile::tempdir().unwrap();
        let (runner, log) = logging_runner(directory.path());

        runner.project_containers("web-1").unwrap();
        runner
            .remove_containers(&["c1".to_owned(), "c2".to_owned()])
            .unwrap();
        runner.project_networks("web-1").unwrap();
        runner.remove_networks(&["n1".to_owned()]).unwrap();

        assert_eq!(
            logged(&log),
            [
                "ps -aq --filter label=com.docker.compose.project=web-1",
                "rm -f -v c1 c2",
                "network ls -q --filter label=com.docker.compose.project=web-1",
                "network rm n1"
            ]
        );
    }

    #[test]
    fn should_not_call_docker_when_there_is_no_container_or_network_to_remove() {
        let directory = tempfile::tempdir().unwrap();
        let (runner, log) = logging_runner(directory.path());

        runner.remove_containers(&[]).unwrap();
        runner.remove_networks(&[]).unwrap();

        assert_eq!(logged(&log), Vec::<String>::new());
    }

    #[test]
    fn should_ask_for_the_resolved_model_with_the_files_and_keep_nothing_of_stderr() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(directory.path(), "echo FAKE-SECRET >&2; echo \"$*\"");

        let model = DockerRunner::new(program)
            .resolved_model(&target(directory.path()))
            .unwrap();

        assert_eq!(
            model.trim(),
            "compose -p web-1 -f dev.compose.yml -f o.yml config --format json"
        );
    }

    #[test]
    fn should_delete_the_containers_then_the_volumes_then_the_networks_of_a_project() {
        use testing::FakeComposeRunner;

        let fake = FakeComposeRunner::default()
            .with_project_containers(vec!["c1".to_owned()])
            .with_volumes(vec!["v1".to_owned()])
            .with_networks(vec!["n1".to_owned()]);

        delete_project_data(&fake, "web-1").unwrap();

        let calls = fake.calls();
        let steps: Vec<(&str, Vec<String>)> = calls
            .iter()
            .map(|call| (call.command, call.services.clone()))
            .collect();
        assert_eq!(
            steps,
            [
                ("docker ps", vec![]),
                ("docker rm", vec!["c1".to_owned()]),
                ("volume ls", vec![]),
                ("volume rm", vec!["v1".to_owned()]),
                ("network ls", vec![]),
                ("network rm", vec!["n1".to_owned()]),
            ]
        );
    }

    #[test]
    fn should_remove_nothing_when_the_project_has_no_container_volume_or_network() {
        use testing::FakeComposeRunner;

        let fake = FakeComposeRunner::default();

        delete_project_data(&fake, "web-1").unwrap();

        assert_eq!(fake.calls().len(), 3);
    }

    #[test]
    fn should_pass_every_name_when_removing_volumes() {
        let directory = tempfile::tempdir().unwrap();
        let (runner, log) = logging_runner(directory.path());

        runner
            .remove_volumes(&["a".to_owned(), "b".to_owned()])
            .unwrap();

        assert_eq!(logged(&log), ["volume rm a b"]);
    }

    #[test]
    fn should_not_call_docker_when_there_is_no_volume_to_remove() {
        let directory = tempfile::tempdir().unwrap();
        let (runner, log) = logging_runner(directory.path());

        runner.remove_volumes(&[]).unwrap();

        assert_eq!(logged(&log), Vec::<String>::new());
    }

    #[test]
    fn should_record_the_calls_and_fail_the_chosen_commands_when_using_the_fake() {
        use testing::FakeComposeRunner;

        let directory = tempfile::tempdir().unwrap();
        let fake = FakeComposeRunner::default().with_failing_commands(vec!["down"]);
        let target = target(directory.path());

        fake.up(&target, &["db".to_owned()]).unwrap();
        let error = fake.down(&target).unwrap_err();

        assert!(matches!(
            error,
            RunnerError::Failed {
                command: "down",
                ..
            }
        ));
        let calls = fake.calls();
        assert_eq!(calls[0].command, "up");
        assert_eq!(calls[0].project, "web-1");
        assert_eq!(calls[0].services, ["db"]);
        assert_eq!(calls[1].command, "down");
    }
}
