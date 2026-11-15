//! Checks on the `.env` files. No finding ever carries a value.

use super::finding::{Check, Finding, Level};
use super::local_host::hardcodes_local_port;
use crate::discovery::ManagedPort;
use crate::project::dotenv::{EnvFile, EnvFileError, EnvRead};

const DOTENV_FILE: &str = ".env";
const DOTENV_TEST_FILE: &str = ".env.test";

/// The state of the `.env` files and of git, as the checks see it.
pub struct EnvCheckInput<'a> {
    /// What reading `.env` gave.
    pub env: &'a EnvRead,
    /// What reading `.env.example` gave.
    pub example: &'a EnvRead,
    /// What reading `.env.test` gave.
    pub test: &'a EnvRead,
    /// Whether `.env` could be created: the main checkout's `.env`, or a
    /// `.env.example` that is not a symbolic link.
    pub has_creation_source: bool,
    /// Whether git ignores `.env`; `None` outside git.
    pub is_env_ignored: Option<bool>,
    /// The managed port variables.
    pub managed: &'a [ManagedPort],
    /// The keys of `[env]`, rebuilt by devslot.
    pub env_config_keys: &'a [String],
}

/// Runs every `.env` check.
pub fn check_env(input: &EnvCheckInput<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();
    findings.extend(check_readable(DOTENV_FILE, input.env));
    findings.extend(check_readable(".env.example", input.example));
    findings.extend(check_readable(DOTENV_TEST_FILE, input.test));
    findings.extend(check_env_exists(input));
    findings.extend(check_env_ignored(input));
    findings.extend(check_test_redefinitions(input));
    findings.extend(check_local_host_ports(input));
    findings
}

fn check_readable(file: &str, read: &EnvRead) -> Option<Finding> {
    let EnvRead::Invalid(problem) = read else {
        return None;
    };
    Some(
        match problem {
            EnvFileError::Unparsable { .. } | EnvFileError::UnterminatedQuote { .. } => {
                Finding::new(Level::Error, Check::EnvUnparsable, problem.to_string())
            }
            EnvFileError::NotRegularFile | EnvFileError::TooLarge => Finding::new(
                Level::Error,
                Check::EnvUnreadable,
                format!("{file} cannot be read: {problem}"),
            ),
            EnvFileError::Block(block) => Finding::new(
                Level::Error,
                Check::EnvBlockMalformed,
                format!("{block}: fix or remove the devslot block by hand"),
            ),
        }
        .in_file(file),
    )
}

fn check_env_exists(input: &EnvCheckInput<'_>) -> Option<Finding> {
    if *input.env != EnvRead::Missing || input.has_creation_source {
        return None;
    }
    Some(
        Finding::new(
            Level::Error,
            Check::EnvMissing,
            "there is no .env, and neither the main checkout's .env nor a .env.example to create it from",
        )
        .in_file(DOTENV_FILE),
    )
}

fn check_env_ignored(input: &EnvCheckInput<'_>) -> Option<Finding> {
    if input.is_env_ignored != Some(false) {
        return None;
    }
    Some(
        Finding::new(
            Level::Error,
            Check::EnvNotIgnored,
            "git does not ignore .env: the devslot block would end up in a commit; add .env to .gitignore",
        )
        .in_file(DOTENV_FILE),
    )
}

fn parsed(read: &EnvRead) -> Option<&EnvFile> {
    match read {
        EnvRead::Parsed(file) => Some(file),
        EnvRead::Missing | EnvRead::Invalid(_) => None,
    }
}

fn check_test_redefinitions(input: &EnvCheckInput<'_>) -> Vec<Finding> {
    let Some(test_file) = parsed(input.test) else {
        return Vec::new();
    };
    let mut reported: Vec<&str> = Vec::new();
    for entry in test_file.entries() {
        let is_managed = input.managed.iter().any(|port| port.variable == entry.key);
        if !is_managed || reported.contains(&entry.key.as_str()) {
            continue;
        }
        reported.push(&entry.key);
    }
    reported
        .into_iter()
        .map(|key| {
            Finding::new(
                Level::Warning,
                Check::EnvTestRedefines,
                format!("{DOTENV_TEST_FILE} redefines {key}: tests would target another port than the app"),
            )
            .in_file(DOTENV_TEST_FILE)
            .for_key(key)
        })
        .collect()
}

fn check_local_host_ports(input: &EnvCheckInput<'_>) -> Vec<Finding> {
    let Some(env_file) = parsed(input.env) else {
        return Vec::new();
    };
    let mut findings = Vec::new();
    for entry in env_file.entries() {
        if input.env_config_keys.contains(&entry.key) {
            continue;
        }
        for port in input.managed {
            let Some(default) = port.default else {
                continue;
            };
            if hardcodes_local_port(&entry.value, default) {
                findings.push(describe_hardcoded_value(
                    &entry.key,
                    &port.variable,
                    default,
                ));
            }
        }
    }
    findings
}

fn describe_hardcoded_value(key: &str, variable: &str, default: u16) -> Finding {
    Finding::new(
        Level::Warning,
        Check::EnvLocalHostPort,
        format!(
            "{key} points at a local host on port {default}, the default of {variable}: it will not follow the assigned port unless [env] rebuilds it"
        ),
    )
    .in_file(DOTENV_FILE)
    .for_key(key)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::discovery::{PortKind, PortSource};
    use crate::project::dotenv_block::{BLOCK_END, BLOCK_START};

    fn parsed_env(text: &str) -> EnvRead {
        EnvRead::Parsed(EnvFile::parse(text).unwrap())
    }

    fn managed_port(variable: &str, default: u16) -> ManagedPort {
        ManagedPort {
            variable: variable.to_owned(),
            kind: PortKind::Web,
            default: Some(default),
            source: PortSource::Dotenv,
            file: ".env".to_owned(),
            service: None,
            target: None,
        }
    }

    struct Scenario {
        env: EnvRead,
        example: EnvRead,
        test: EnvRead,
        has_creation_source: bool,
        is_env_ignored: Option<bool>,
        managed: Vec<ManagedPort>,
        env_config_keys: Vec<String>,
    }

    impl Scenario {
        fn healthy() -> Self {
            Self {
                env: parsed_env("PORT=3333\n"),
                example: EnvRead::Missing,
                test: EnvRead::Missing,
                has_creation_source: false,
                is_env_ignored: Some(true),
                managed: vec![managed_port("PORT", 3333), managed_port("DB_PORT", 5432)],
                env_config_keys: vec![],
            }
        }

        fn findings(&self) -> Vec<Finding> {
            check_env(&EnvCheckInput {
                env: &self.env,
                example: &self.example,
                test: &self.test,
                has_creation_source: self.has_creation_source,
                is_env_ignored: self.is_env_ignored,
                managed: &self.managed,
                env_config_keys: &self.env_config_keys,
            })
        }
    }

    fn checks_of(findings: &[Finding]) -> Vec<Check> {
        findings.iter().map(|finding| finding.check).collect()
    }

    #[test]
    fn should_report_nothing_when_the_project_is_healthy() {
        assert_eq!(Scenario::healthy().findings(), vec![]);
    }

    #[test]
    fn should_report_an_error_when_env_is_missing_and_nothing_can_create_it() {
        let scenario = Scenario {
            env: EnvRead::Missing,
            ..Scenario::healthy()
        };

        assert_eq!(checks_of(&scenario.findings()), vec![Check::EnvMissing]);
    }

    #[test]
    fn should_not_report_a_missing_env_when_a_creation_source_exists() {
        let scenario = Scenario {
            env: EnvRead::Missing,
            example: parsed_env("PORT=3333\n"),
            has_creation_source: true,
            ..Scenario::healthy()
        };

        assert_eq!(scenario.findings(), vec![]);
    }

    #[test]
    fn should_report_a_missing_env_when_an_example_exists_but_is_not_a_creation_source() {
        let scenario = Scenario {
            env: EnvRead::Missing,
            example: parsed_env("PORT=3333\n"),
            has_creation_source: false,
            ..Scenario::healthy()
        };

        assert_eq!(checks_of(&scenario.findings()), vec![Check::EnvMissing]);
    }

    #[test]
    fn should_not_report_a_missing_env_when_the_main_checkout_has_one() {
        let scenario = Scenario {
            env: EnvRead::Missing,
            has_creation_source: true,
            ..Scenario::healthy()
        };

        assert_eq!(scenario.findings(), vec![]);
    }

    #[test]
    fn should_report_an_error_when_git_does_not_ignore_env() {
        let scenario = Scenario {
            is_env_ignored: Some(false),
            ..Scenario::healthy()
        };

        assert_eq!(checks_of(&scenario.findings()), vec![Check::EnvNotIgnored]);
    }

    #[test]
    fn should_not_report_the_ignore_check_when_outside_git() {
        let scenario = Scenario {
            is_env_ignored: None,
            ..Scenario::healthy()
        };

        assert_eq!(scenario.findings(), vec![]);
    }

    #[test]
    fn should_report_the_line_number_without_the_line_when_env_is_unparsable() {
        let scenario = Scenario {
            env: EnvRead::Invalid(EnvFileError::Unparsable { line: 7 }),
            ..Scenario::healthy()
        };

        let findings = scenario.findings();

        assert_eq!(
            (
                findings[0].check,
                findings[0].message.as_str(),
                findings[0].file.as_deref()
            ),
            (
                Check::EnvUnparsable,
                "line 7 is not valid dotenv syntax",
                Some(".env")
            )
        );
    }

    #[test]
    fn should_report_an_unreadable_file_when_env_is_not_a_regular_file() {
        let scenario = Scenario {
            env: EnvRead::Invalid(EnvFileError::NotRegularFile),
            ..Scenario::healthy()
        };

        let findings = scenario.findings();

        assert_eq!(
            (findings[0].check, findings[0].message.as_str()),
            (
                Check::EnvUnreadable,
                ".env cannot be read: not a regular file"
            )
        );
    }

    #[test]
    fn should_report_an_error_when_the_devslot_block_is_malformed() {
        let text = format!("{BLOCK_START}\nPORT=1\n");
        let scenario = Scenario {
            env: EnvRead::Invalid(EnvFile::parse(&text).unwrap_err()),
            ..Scenario::healthy()
        };

        let findings = scenario.findings();

        assert_eq!(
            (findings[0].level, findings[0].check),
            (Level::Error, Check::EnvBlockMalformed)
        );
        assert!(findings[0].message.contains("line 1"));
    }

    #[test]
    fn should_report_an_error_when_two_blocks_exist() {
        let block = format!("{BLOCK_START}\nA=1\n{BLOCK_END}\n");
        let text = format!("{block}{block}");
        let scenario = Scenario {
            env: EnvRead::Invalid(EnvFile::parse(&text).unwrap_err()),
            ..Scenario::healthy()
        };

        assert_eq!(
            checks_of(&scenario.findings()),
            vec![Check::EnvBlockMalformed]
        );
    }

    #[test]
    fn should_warn_once_per_key_when_env_test_redefines_a_managed_variable() {
        let scenario = Scenario {
            test: parsed_env("PORT=4000\nPORT=4001\nOTHER=1\n"),
            ..Scenario::healthy()
        };

        let findings = scenario.findings();

        assert_eq!(
            (checks_of(&findings), findings[0].key.as_deref()),
            (vec![Check::EnvTestRedefines], Some("PORT"))
        );
    }

    #[test]
    fn should_not_warn_when_env_test_defines_only_unmanaged_variables() {
        let scenario = Scenario {
            test: parsed_env("DB_DATABASE=test\n"),
            ..Scenario::healthy()
        };

        assert_eq!(scenario.findings(), vec![]);
    }

    #[test]
    fn should_warn_without_printing_the_value_when_a_value_hardcodes_a_local_default_port() {
        let scenario = Scenario {
            env: parsed_env("DATABASE_URL=postgres://u:FAKE@localhost:5432/app\n"),
            ..Scenario::healthy()
        };

        let findings = scenario.findings();

        assert_eq!(
            (checks_of(&findings), findings[0].key.as_deref()),
            (vec![Check::EnvLocalHostPort], Some("DATABASE_URL"))
        );
        assert!(!findings[0].message.contains("FAKE"));
    }

    #[test]
    fn should_not_warn_when_the_host_is_not_local() {
        let scenario = Scenario {
            env: parsed_env("SMTP_URL=smtp://mail.example.com:5432\n"),
            ..Scenario::healthy()
        };

        assert_eq!(scenario.findings(), vec![]);
    }

    #[test]
    fn should_not_warn_when_the_port_is_a_substitution() {
        let scenario = Scenario {
            env: parsed_env("DB_PORT=5432\nDATABASE_URL=postgres://localhost:${DB_PORT}/app\n"),
            ..Scenario::healthy()
        };

        assert_eq!(scenario.findings(), vec![]);
    }

    #[test]
    fn should_not_warn_when_the_key_is_rebuilt_by_the_env_section() {
        let scenario = Scenario {
            env: parsed_env("DATABASE_URL=postgres://localhost:5432/app\n"),
            env_config_keys: vec!["DATABASE_URL".to_owned()],
            ..Scenario::healthy()
        };

        assert_eq!(scenario.findings(), vec![]);
    }

    #[test]
    fn should_not_warn_when_the_managed_variable_has_no_known_default() {
        let scenario = Scenario {
            env: parsed_env("URL=localhost:5432\n"),
            managed: vec![ManagedPort {
                default: None,
                ..managed_port("DB_PORT", 0)
            }],
            ..Scenario::healthy()
        };

        assert_eq!(scenario.findings(), vec![]);
    }
}
