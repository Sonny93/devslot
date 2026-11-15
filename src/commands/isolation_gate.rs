//! The last look at the model before services start: it is resolved the way
//! `up` will resolve it, `.env` included, because the probe of registration
//! runs without it and cannot see a name that `.env` chooses.

use crate::checks::isolation::isolation_problems;
use crate::compose::model::ComposeConfig;
use crate::compose::runner::{ComposeRunner, ComposeTarget, RunnerError};
use crate::error::ToolError;

/// Whether the resolved model of `target` keeps the instance isolated. When it
/// does not, or when Compose cannot resolve it, tells the user and gives
/// `false`. Messages name keys only.
pub fn is_model_isolated(
    runner: &dyn ComposeRunner,
    target: &ComposeTarget,
) -> Result<bool, ToolError> {
    let text = match runner.resolved_model(target) {
        Ok(text) => text,
        Err(RunnerError::Failed { .. }) => {
            eprintln!(
                "ds: error: docker compose cannot resolve the dev compose; run docker compose config to see why"
            );
            return Ok(false);
        }
        Err(error) => return Err(error.into()),
    };
    let config = ComposeConfig::from_json(&text)
        .map_err(|_| RunnerError::UnexpectedOutput { stage: "config" })?;
    let problems = isolation_problems(&config, &target.project_name);
    for problem in &problems {
        eprintln!("ds: error: {problem}");
    }
    Ok(problems.is_empty())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::compose::runner::testing::FakeComposeRunner;

    const HOSTILE_MODEL: &str = r#"{"volumes":{"data":{"name":"victim_pgdata"}}}"#;
    const NORMAL_MODEL: &str =
        r#"{"services":{"db":{"image":"x"}},"volumes":{"data":{"name":"app-3f2a_data"}}}"#;

    fn target() -> ComposeTarget {
        ComposeTarget {
            project_name: "app-3f2a".to_owned(),
            files: vec![PathBuf::from("/work/app/dev.compose.yml")],
            working_directory: PathBuf::from("/work/app"),
            environment: vec![],
        }
    }

    #[test]
    fn should_refuse_the_model_whose_volume_name_is_chosen_by_the_env() {
        let runner = FakeComposeRunner::default().with_model(HOSTILE_MODEL);

        assert!(!is_model_isolated(&runner, &target()).unwrap());
    }

    #[test]
    fn should_accept_a_normal_model() {
        let runner = FakeComposeRunner::default().with_model(NORMAL_MODEL);

        assert!(is_model_isolated(&runner, &target()).unwrap());
    }

    #[test]
    fn should_refuse_when_compose_cannot_resolve_the_model() {
        let runner = FakeComposeRunner::default().with_failing_commands(vec!["config"]);

        assert!(!is_model_isolated(&runner, &target()).unwrap());
    }

    #[test]
    fn should_fail_the_tool_when_the_model_is_not_json() {
        let runner = FakeComposeRunner::default().with_model("FAKE-SECRET");

        let error = is_model_isolated(&runner, &target()).unwrap_err();

        assert!(!error.to_string().contains("FAKE-SECRET"));
    }
}
