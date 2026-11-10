//! Running `docker compose config`: the only Docker call devslot makes in
//! phase 1, read-only, behind a trait so discovery and checks can be tested
//! against an in-memory fake.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use thiserror::Error;

/// The compose project name every probe uses, so implicit volume and network
/// names come out as `devslot-probe_<key>`.
pub const PROBE_PROJECT: &str = "devslot-probe";
/// The environment variable that replaces the `docker` program.
pub const DOCKER_PROGRAM_VARIABLE: &str = "DEVSLOT_DOCKER";

const DEFAULT_DOCKER_PROGRAM: &str = "docker";
const EMPTY_ENV_FILE: &str = "/dev/null";
const ALL_PROFILES: &str = "*";
const DOCKER_VARIABLE_PREFIX: &str = "DOCKER_";
const PASSED_VARIABLES: [&str; 2] = ["PATH", "HOME"];
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How long a docker call may run.
pub const DOCKER_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a docker call gave nothing usable.
#[derive(Debug, Error)]
pub enum DockerError {
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
    /// Compose ran and rejected the files. Its stderr is never kept: it can
    /// quote values of a `.env` that Compose loaded despite `--env-file`.
    #[error("docker compose rejected the configuration")]
    Rejected,
    /// Compose succeeded but its output is not what was expected. No detail
    /// is kept: parser errors can quote the content.
    #[error("docker compose gave an output that cannot be read ({stage})")]
    UnexpectedOutput {
        /// The call whose output was unreadable.
        stage: &'static str,
    },
}

impl DockerError {
    /// Whether Compose itself refused the files, as opposed to the tool failing.
    pub fn is_rejection(&self) -> bool {
        matches!(self, Self::Rejected)
    }
}

/// The read-only questions devslot asks Compose.
pub trait ComposeCli {
    /// `config --variables`: each variable of the files with its default value
    /// (empty when it has none).
    fn variables(&self, files: &[PathBuf]) -> Result<BTreeMap<String, String>, DockerError>;

    /// `config --format json --no-env-resolution` with `sentinels` as the
    /// whole variable environment.
    fn config_json(
        &self,
        files: &[PathBuf],
        sentinels: &BTreeMap<String, String>,
    ) -> Result<String, DockerError>;
}

#[derive(Deserialize)]
struct VariableDescription {
    #[serde(rename = "DefaultValue", default)]
    default_value: String,
}

/// The real `docker compose`, run from the checkout root.
#[derive(Debug, Clone)]
pub struct DockerCompose {
    program: OsString,
    working_directory: PathBuf,
    timeout: Duration,
}

impl DockerCompose {
    /// Uses `$DEVSLOT_DOCKER`, else `docker`, with the standard timeout.
    pub fn from_environment(working_directory: &Path) -> Self {
        let program = std::env::var_os(DOCKER_PROGRAM_VARIABLE)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| OsString::from(DEFAULT_DOCKER_PROGRAM));
        Self::new(program, working_directory, DOCKER_TIMEOUT)
    }

    /// Uses `program` with an explicit timeout.
    pub fn new(program: impl Into<OsString>, working_directory: &Path, timeout: Duration) -> Self {
        Self {
            program: program.into(),
            working_directory: working_directory.to_path_buf(),
            timeout,
        }
    }

    fn compose_command(
        &self,
        files: &[PathBuf],
        tail: &[&str],
        sentinels: &BTreeMap<String, String>,
    ) -> Command {
        let mut command = Command::new(&self.program);
        command
            .current_dir(&self.working_directory)
            .env_clear()
            .envs(sentinels)
            .envs(passed_environment())
            .process_group(0)
            .args(["compose", "-p", PROBE_PROJECT]);
        for file in files {
            command.arg("-f").arg(file);
        }
        command
            .args([
                "--env-file",
                EMPTY_ENV_FILE,
                "--profile",
                ALL_PROFILES,
                "config",
            ])
            .args(tail);
        command
    }

    fn run(&self, command: Command) -> Result<String, DockerError> {
        let program = self.program.to_string_lossy().into_owned();
        let output = run_with_timeout(command, self.timeout, &program)?;
        if output.success {
            return Ok(output.stdout);
        }
        Err(DockerError::Rejected)
    }
}

impl ComposeCli for DockerCompose {
    fn variables(&self, files: &[PathBuf]) -> Result<BTreeMap<String, String>, DockerError> {
        let command = self.compose_command(
            files,
            &["--variables", "--format", "json"],
            &BTreeMap::new(),
        );
        let text = self.run(command)?;
        let descriptions: BTreeMap<String, VariableDescription> = serde_json::from_str(&text)
            .map_err(|_| DockerError::UnexpectedOutput { stage: "variables" })?;
        Ok(descriptions
            .into_iter()
            .map(|(name, description)| (name, description.default_value))
            .collect())
    }

    fn config_json(
        &self,
        files: &[PathBuf],
        sentinels: &BTreeMap<String, String>,
    ) -> Result<String, DockerError> {
        let command = self.compose_command(
            files,
            &["--format", "json", "--no-env-resolution"],
            sentinels,
        );
        self.run(command)
    }
}

fn passed_environment() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| is_passed_variable(name))
        .collect()
}

fn is_passed_variable(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    PASSED_VARIABLES.contains(&name) || name.starts_with(DOCKER_VARIABLE_PREFIX)
}

struct FinishedCall {
    success: bool,
    stdout: String,
}

fn run_with_timeout(
    mut command: Command,
    timeout: Duration,
    program: &str,
) -> Result<FinishedCall, DockerError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().map_err(|error| DockerError::Unavailable {
        program: program.to_owned(),
        reason: error.to_string(),
    })?;
    let stdout_reader = spawn_reader(child.stdout.take());
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                kill_process_group(&child);
                let _ = child.wait();
                return Err(DockerError::TimedOut {
                    program: program.to_owned(),
                    seconds: timeout.as_secs(),
                });
            }
            Err(error) => {
                return Err(DockerError::Unavailable {
                    program: program.to_owned(),
                    reason: error.to_string(),
                });
            }
        }
    };
    Ok(FinishedCall {
        success: status.success(),
        stdout: join_reader(stdout_reader),
    })
}

fn kill_process_group(child: &std::process::Child) {
    let group = rustix::process::Pid::from_child(child);
    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
}

fn spawn_reader<R: Read + Send + 'static>(stream: Option<R>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut stream) = stream {
            let _ = stream.read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

fn join_reader(reader: thread::JoinHandle<String>) -> String {
    reader.join().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use pretty_assertions::assert_eq;

    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    fn script(directory: &Path, body: &str) -> PathBuf {
        let path = directory.join("fake-docker");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn docker(program: &Path, directory: &Path, timeout: Duration) -> DockerCompose {
        DockerCompose::new(program, directory, timeout)
    }

    #[test]
    fn should_pass_the_probe_flags_in_order_when_asking_for_the_config() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(directory.path(), "printf '%s\\n' \"$@\"");
        let files = [
            PathBuf::from("dev.compose.yml"),
            PathBuf::from("compose.override.yml"),
        ];

        let output = docker(&program, directory.path(), TEST_TIMEOUT)
            .config_json(&files, &BTreeMap::new())
            .unwrap();

        assert_eq!(
            output.lines().collect::<Vec<_>>(),
            [
                "compose",
                "-p",
                "devslot-probe",
                "-f",
                "dev.compose.yml",
                "-f",
                "compose.override.yml",
                "--env-file",
                "/dev/null",
                "--profile",
                "*",
                "config",
                "--format",
                "json",
                "--no-env-resolution"
            ]
        );
    }

    #[test]
    fn should_run_in_the_working_directory_when_the_program_prints_it() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(directory.path(), "pwd -P");

        let output = docker(&program, directory.path(), TEST_TIMEOUT)
            .config_json(&[], &BTreeMap::new())
            .unwrap();

        assert_eq!(
            output.trim(),
            directory.path().canonicalize().unwrap().to_str().unwrap()
        );
    }

    #[test]
    fn should_clear_the_environment_and_pass_only_the_sentinels_when_the_program_prints_it() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(directory.path(), "env | cut -d= -f1 | sort");
        let sentinels = BTreeMap::from([("DB_PORT".to_owned(), "61000".to_owned())]);

        let output = docker(&program, directory.path(), TEST_TIMEOUT)
            .config_json(&[], &sentinels)
            .unwrap();

        let names: Vec<&str> = output.lines().collect();
        assert!(names.contains(&"DB_PORT"));
        assert!(
            names.iter().all(|name| matches!(
                *name,
                "DB_PORT" | "PATH" | "HOME" | "PWD" | "SHLVL" | "_" | "OLDPWD"
            ) || name.starts_with(DOCKER_VARIABLE_PREFIX)),
            "unexpected variables: {names:?}"
        );
    }

    const LEAKING_STDERR: &str = "echo 'failed to read /x/.env: line 3: unterminated quoted value \"FAKE-SECRET' >&2; exit 1";

    #[test]
    fn should_not_keep_any_stderr_text_when_the_program_fails() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(directory.path(), LEAKING_STDERR);

        let error = docker(&program, directory.path(), TEST_TIMEOUT)
            .variables(&[])
            .unwrap_err();

        assert!(error.is_rejection());
        assert!(!error.to_string().contains("FAKE-SECRET"));
        assert!(!format!("{error:?}").contains("FAKE-SECRET"));
    }

    #[test]
    fn should_give_the_passed_variables_priority_when_a_sentinel_has_the_same_name() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(directory.path(), "echo \"$HOME\"");
        let sentinels = BTreeMap::from([("HOME".to_owned(), "61000".to_owned())]);

        let output = docker(&program, directory.path(), TEST_TIMEOUT)
            .config_json(&[], &sentinels)
            .unwrap();

        assert_eq!(output.trim(), std::env::var("HOME").unwrap());
    }

    #[test]
    fn should_kill_the_whole_process_group_when_the_call_times_out() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("grandchild.pid");
        let program = script(
            directory.path(),
            &format!("sleep 30 &\necho $! > '{}'\nsleep 30", pid_file.display()),
        );

        let error = docker(&program, directory.path(), Duration::from_millis(500))
            .variables(&[])
            .unwrap_err();

        assert!(matches!(error, DockerError::TimedOut { .. }));
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        let process_directory = PathBuf::from(format!("/proc/{}", pid.trim()));
        let is_gone = (0..100).any(|_| {
            thread::sleep(Duration::from_millis(50));
            !process_directory.exists() || is_zombie(&process_directory)
        });
        assert!(is_gone, "the grandchild is still running");
    }

    fn is_zombie(process_directory: &Path) -> bool {
        std::fs::read_to_string(process_directory.join("stat"))
            .is_ok_and(|stat| stat.contains(") Z "))
    }

    #[test]
    fn should_report_unavailable_when_the_program_does_not_exist() {
        let directory = tempfile::tempdir().unwrap();

        let error = docker(
            Path::new("/nonexistent/docker"),
            directory.path(),
            TEST_TIMEOUT,
        )
        .variables(&[])
        .unwrap_err();

        assert!(matches!(error, DockerError::Unavailable { .. }));
    }

    #[test]
    fn should_report_a_timeout_when_the_program_runs_too_long() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(directory.path(), "exec sleep 5");

        let error = docker(&program, directory.path(), Duration::from_millis(100))
            .variables(&[])
            .unwrap_err();

        assert!(matches!(error, DockerError::TimedOut { .. }));
    }

    #[test]
    fn should_map_each_variable_to_its_default_when_parsing_the_variables_output() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(
            directory.path(),
            r#"echo '{"DB_PORT":{"Name":"DB_PORT","DefaultValue":"5432"},"DB_USER":{"Name":"DB_USER","DefaultValue":""}}'"#,
        );

        let variables = docker(&program, directory.path(), TEST_TIMEOUT)
            .variables(&[])
            .unwrap();

        assert_eq!(
            variables,
            BTreeMap::from([
                ("DB_PORT".to_owned(), "5432".to_owned()),
                ("DB_USER".to_owned(), String::new())
            ])
        );
    }

    #[test]
    fn should_report_unexpected_output_when_the_variables_output_is_not_json() {
        let directory = tempfile::tempdir().unwrap();
        let program = script(directory.path(), "echo not-json");

        let error = docker(&program, directory.path(), TEST_TIMEOUT)
            .variables(&[])
            .unwrap_err();

        assert!(matches!(error, DockerError::UnexpectedOutput { .. }));
        assert!(!error.to_string().contains("not-json"));
    }
}
