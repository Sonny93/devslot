//! What `ds info` says about the services of an instance.

use serde::Serialize;

use crate::compose::runner::{ContainerState, RunnerError};

const RUNNING_STATE: &str = "running";

/// Whether the compose services of an instance are up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServicesState {
    /// At least one container runs.
    Running,
    /// Containers exist and none runs.
    Stopped,
    /// There is no container.
    #[serde(rename = "none")]
    Absent,
    /// Docker could not tell.
    Unknown,
}

impl ServicesState {
    /// The state `containers` shows; `Unknown` when the call failed.
    pub fn of(containers: &Result<Vec<ContainerState>, RunnerError>) -> Self {
        let Ok(containers) = containers else {
            return Self::Unknown;
        };
        if containers.is_empty() {
            return Self::Absent;
        }
        if containers
            .iter()
            .any(|container| container.state == RUNNING_STATE)
        {
            return Self::Running;
        }
        Self::Stopped
    }

    /// The word the text and JSON output use.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Absent => "none",
            Self::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(state: &str) -> ContainerState {
        ContainerState {
            service: "postgres".to_owned(),
            state: state.to_owned(),
            published_ports: Vec::new(),
        }
    }

    #[test]
    fn should_say_running_when_one_container_runs_and_another_exited() {
        let containers = Ok(vec![container("exited"), container("running")]);

        assert_eq!(ServicesState::of(&containers), ServicesState::Running);
    }

    #[test]
    fn should_say_stopped_when_containers_exist_and_none_runs() {
        let containers = Ok(vec![container("exited"), container("created")]);

        assert_eq!(ServicesState::of(&containers), ServicesState::Stopped);
    }

    #[test]
    fn should_say_none_when_there_is_no_container() {
        assert_eq!(ServicesState::of(&Ok(Vec::new())), ServicesState::Absent);
    }

    #[test]
    fn should_say_unknown_when_the_call_failed() {
        let failed = Err(RunnerError::Failed {
            command: "ps",
            code: Some(1),
        });

        assert_eq!(ServicesState::of(&failed), ServicesState::Unknown);
    }

    #[test]
    fn should_serialize_the_absent_state_as_none() {
        assert_eq!(
            serde_json::to_string(&ServicesState::Absent).unwrap(),
            "\"none\""
        );
    }
}
