//! Sentinels: unique port-like values given to candidate variables so the
//! published ports they drive can be recognized in the merged configuration.

use std::collections::BTreeMap;

use crate::reserved_names::is_reserved;

/// The first sentinel: outside the slot range and above the Linux ephemeral range.
pub const SENTINEL_BASE: u16 = 61000;

/// The sentinel given to each candidate variable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SentinelPlan {
    by_variable: BTreeMap<String, u16>,
}

impl SentinelPlan {
    /// Numbers the candidates in name order: the first gets [`SENTINEL_BASE`].
    pub fn assign(candidates: impl IntoIterator<Item = String>) -> Self {
        let mut names: Vec<String> = candidates.into_iter().collect();
        names.sort();
        names.dedup();
        let by_variable = names.into_iter().zip(SENTINEL_BASE..=u16::MAX).collect();
        Self { by_variable }
    }

    /// The variable environment to run Compose with.
    pub fn environment(&self) -> BTreeMap<String, String> {
        self.by_variable
            .iter()
            .map(|(name, sentinel)| (name.clone(), sentinel.to_string()))
            .collect()
    }

    /// The variable that carries `sentinel`, when there is one.
    pub fn variable_for(&self, sentinel: u64) -> Option<&str> {
        self.by_variable
            .iter()
            .find(|(_, assigned)| u64::from(**assigned) == sentinel)
            .map(|(name, _)| name.as_str())
    }

    /// The variable that carries the sentinel written as text.
    pub fn variable_for_text(&self, sentinel: &str) -> Option<&str> {
        sentinel
            .parse::<u64>()
            .ok()
            .and_then(|number| self.variable_for(number))
    }
}

/// The value a variable usually has: the compose default when it has one,
/// otherwise what the `.env` files say.
pub fn usual_value(
    compose_default: &str,
    from_env_files: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if compose_default.is_empty() {
        return from_env_files();
    }
    Some(compose_default.to_owned())
}

/// A variable is a candidate when its usual value is a number or empty.
pub fn is_candidate(usual: Option<&str>) -> bool {
    usual.is_none_or(|value| value.chars().all(|character| character.is_ascii_digit()))
}

/// The plan for the variables Compose listed, given their compose defaults.
pub fn plan_for(
    variables: &BTreeMap<String, String>,
    from_env_files: &dyn Fn(&str) -> Option<String>,
) -> SentinelPlan {
    let candidates = variables.iter().filter(|(name, default)| {
        if is_reserved(name) {
            return false;
        }
        let usual = usual_value(default, || from_env_files(name));
        is_candidate(usual.as_deref())
    });
    SentinelPlan::assign(candidates.map(|(name, _)| name.clone()))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn variables(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, default)| ((*name).to_owned(), (*default).to_owned()))
            .collect()
    }

    fn no_env_files(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn should_number_candidates_in_name_order_from_the_base() {
        let plan = SentinelPlan::assign(["PORT", "DB_PORT", "A"].map(String::from));

        assert_eq!(
            plan.environment(),
            BTreeMap::from([
                ("A".to_owned(), "61000".to_owned()),
                ("DB_PORT".to_owned(), "61001".to_owned()),
                ("PORT".to_owned(), "61002".to_owned()),
            ])
        );
    }

    #[test]
    fn should_find_the_variable_when_given_its_sentinel_as_text() {
        let plan = SentinelPlan::assign(["A", "B"].map(String::from));

        assert_eq!(plan.variable_for_text("61001"), Some("B"));
    }

    #[test]
    fn should_find_nothing_when_the_text_is_not_a_sentinel() {
        let plan = SentinelPlan::assign(["A"].map(String::from));

        assert_eq!(
            (
                plan.variable_for_text("5432"),
                plan.variable_for_text("8000-8010")
            ),
            (None, None)
        );
    }

    #[test]
    fn should_give_a_sentinel_when_the_compose_default_is_a_number() {
        let plan = plan_for(&variables(&[("DB_PORT", "5432")]), &no_env_files);

        assert_eq!(plan.variable_for(61000), Some("DB_PORT"));
    }

    #[test]
    fn should_give_a_sentinel_when_there_is_no_default_and_no_env_value() {
        let plan = plan_for(&variables(&[("DB_PORT", "")]), &no_env_files);

        assert_eq!(plan.variable_for(61000), Some("DB_PORT"));
    }

    #[test]
    fn should_give_a_sentinel_when_the_env_value_is_a_number() {
        let plan = plan_for(&variables(&[("DB_PORT", "")]), &|_| Some("5432".to_owned()));

        assert_eq!(plan.variable_for(61000), Some("DB_PORT"));
    }

    #[test]
    fn should_give_a_sentinel_when_the_env_value_is_empty() {
        let plan = plan_for(&variables(&[("DB_PORT", "")]), &|_| Some(String::new()));

        assert_eq!(plan.variable_for(61000), Some("DB_PORT"));
    }

    #[test]
    fn should_give_no_sentinel_when_the_env_value_is_not_a_number() {
        let plan = plan_for(&variables(&[("DB_USER", ""), ("BIND_IP", "")]), &|name| {
            Some(
                if name == "DB_USER" {
                    "root"
                } else {
                    "127.0.0.1"
                }
                .to_owned(),
            )
        });

        assert_eq!(plan, SentinelPlan::default());
    }

    #[test]
    fn should_give_no_sentinel_when_the_compose_default_is_not_a_number() {
        let plan = plan_for(&variables(&[("HC", "30s")]), &no_env_files);

        assert_eq!(plan, SentinelPlan::default());
    }

    #[test]
    fn should_prefer_the_compose_default_over_the_env_value_when_both_exist() {
        let plan = plan_for(&variables(&[("DB_PORT", "5432")]), &|_| {
            Some("root".to_owned())
        });

        assert_eq!(plan.variable_for(61000), Some("DB_PORT"));
    }

    #[test]
    fn should_give_a_sentinel_only_to_the_port_when_reserved_variables_have_no_default() {
        let plan = plan_for(
            &variables(&[
                ("HOME", ""),
                ("DOCKER_CONFIG", ""),
                ("LD_LIBRARY_PATH", ""),
                ("XDG_RUNTIME_DIR", ""),
                ("COMPOSE_FILE", ""),
                ("BUILDX_CONFIG", ""),
                ("PATH", ""),
                ("PWD", ""),
                ("SHELL", ""),
                ("USER", ""),
                ("TMPDIR", ""),
                ("DB_PORT", ""),
            ]),
            &no_env_files,
        );

        assert_eq!(
            plan.environment(),
            BTreeMap::from([("DB_PORT".to_owned(), "61000".to_owned())])
        );
    }

    #[test]
    fn should_stop_numbering_when_the_port_space_runs_out() {
        let names = (0..5000).map(|index| format!("V{index:04}"));

        let plan = SentinelPlan::assign(names);

        assert_eq!(
            plan.environment().len(),
            usize::from(u16::MAX - SENTINEL_BASE) + 1
        );
    }
}
