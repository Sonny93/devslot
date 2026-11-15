#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]
//! Helpers shared by the integration tests: temp projects and the real binary
//! run against the fake docker script.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
const FAKE_DOCKER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/fake-docker.sh");

pub const FAKE_ENV: &str =
    "PORT=3333\nDB_HOST=localhost\nDB_PORT=5432\nDB_USER=root\nDB_PASSWORD=FAKE\nDB_DATABASE=app\n";

pub const TEST_SLOTS: &str = "[slots]\nfirst_port = 14000\nlast_port = 14999\nsize = 10\n";

/// The config and state folders of one pretend machine: every run of `ds`
/// is given these, never the real ones.
pub struct Machine {
    config_home: tempfile::TempDir,
    state_home: tempfile::TempDir,
    scratch: tempfile::TempDir,
}

impl Machine {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            config_home: tempfile::tempdir().unwrap(),
            state_home: tempfile::tempdir().unwrap(),
            scratch: tempfile::tempdir().unwrap(),
        })
    }

    pub fn state_directory(&self) -> PathBuf {
        self.state_home.path().join("devslot")
    }

    /// The file the fake docker logs its calls to.
    pub fn docker_log_path(&self) -> PathBuf {
        self.scratch.path().join("docker.log")
    }

    /// The calls the fake docker logged so far, one line each.
    pub fn docker_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.docker_log_path())
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub fn clear_docker_log(&self) {
        let _ = std::fs::remove_file(self.docker_log_path());
    }

    /// Writes a file the fake docker can be pointed at, returns its path.
    pub fn write_scratch(&self, name: &str, content: &str) -> PathBuf {
        let path = self.scratch.path().join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    pub fn with_test_slots() -> Arc<Self> {
        let machine = Self::new();
        machine.write_global_config(TEST_SLOTS);
        machine
    }

    pub fn write_global_config(&self, text: &str) {
        let directory = self.config_home.path().join("devslot");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("config.toml"), text).unwrap();
    }

    pub fn registry_path(&self) -> PathBuf {
        self.state_home.path().join("devslot").join("registry.toml")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.state_home.path().join("devslot").join("registry.lock")
    }

    pub fn read_registry(&self) -> String {
        std::fs::read_to_string(self.registry_path()).unwrap()
    }
}

pub struct Project {
    directory: tempfile::TempDir,
    root: PathBuf,
    scenario: &'static str,
    machine: Arc<Machine>,
}

pub struct Run {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Project {
    pub fn from_scenario(scenario: &'static str) -> Self {
        Self::from_scenario_on(&Machine::with_test_slots(), scenario)
    }

    pub fn from_scenario_on(machine: &Arc<Machine>, scenario: &'static str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        copy_tree(
            &Path::new(FIXTURES).join(scenario).join("project"),
            directory.path(),
        );
        let project = Self {
            root: directory.path().to_path_buf(),
            directory,
            scenario,
            machine: Arc::clone(machine),
        };
        project.write(".env", FAKE_ENV);
        project
    }

    pub fn empty() -> Self {
        let directory = tempfile::tempdir().unwrap();
        Self {
            root: directory.path().to_path_buf(),
            directory,
            scenario: "template",
            machine: Machine::with_test_slots(),
        }
    }

    pub fn machine(&self) -> &Arc<Machine> {
        &self.machine
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.path().join(relative)).unwrap()
    }

    pub fn exists(&self, relative: &str) -> bool {
        self.path().join(relative).exists()
    }

    pub fn remove(&self, relative: &str) {
        std::fs::remove_file(self.path().join(relative)).unwrap();
    }

    /// The modification time of a file of the project.
    pub fn modified(&self, relative: &str) -> std::time::SystemTime {
        std::fs::metadata(self.path().join(relative))
            .unwrap()
            .modified()
            .unwrap()
    }

    pub fn write(&self, relative: &str, content: &str) {
        let path = self.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    pub fn initialize_git(&self, gitignore: &str) {
        self.write(".gitignore", gitignore);
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(self.path())
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Runs `git` in the project without the user's git configuration.
    pub fn git(&self, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(self.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?}");
    }

    /// Initialises git, then commits every file git does not ignore.
    pub fn initialize_git_and_commit(&self, gitignore: &str) {
        self.initialize_git(gitignore);
        self.git(&["add", "-A"]);
        self.git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--no-verify",
            "-m",
            "initial",
        ]);
    }

    /// A linked worktree of this project, on a new branch, in its own folder.
    /// The project needs a commit.
    pub fn add_worktree(&self, branch: &str) -> Project {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("worktree");
        self.git(&[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            root.to_str().unwrap(),
        ]);
        Project {
            root: root.canonicalize().unwrap(),
            directory,
            scenario: self.scenario,
            machine: Arc::clone(&self.machine),
        }
    }

    pub fn doctor(&self, arguments: &[&str]) -> Run {
        self.run_with(arguments, &[])
    }

    pub fn run(&self, arguments: &[&str]) -> Run {
        self.run_with(arguments, &[])
    }

    pub fn run_with(&self, arguments: &[&str], environment: &[(&str, &str)]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ds"));
        command
            .args(arguments)
            .current_dir(self.path())
            .env("DEVSLOT_DOCKER", FAKE_DOCKER)
            .env("DOCKER_FAKE_LOG", self.machine.docker_log_path())
            .env("XDG_CONFIG_HOME", self.machine.config_home.path())
            .env("XDG_STATE_HOME", self.machine.state_home.path())
            .env(
                "DOCKER_FAKE_FIXTURES",
                fixture_docker_directory(self.scenario),
            )
            .env_remove("NO_COLOR");
        for (name, value) in environment {
            command.env(name, value);
        }
        Run::from(command.output().unwrap())
    }
}

impl From<Output> for Run {
    fn from(output: Output) -> Self {
        Self {
            status: output.status.code().unwrap(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

fn fixture_docker_directory(scenario: &str) -> PathBuf {
    Path::new(FIXTURES).join(scenario).join("docker")
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_tree(&entry.path(), &target);
            continue;
        }
        std::fs::copy(entry.path(), target).unwrap();
    }
}
