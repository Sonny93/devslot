//! Checks on the `.env` files. No finding ever carries a value.

use super::finding::{Check, Finding, Level};
use super::local_host::{hardcodes_local_port, literal_local_ports};
use crate::discovery::ManagedPort;
use crate::project::dotenv::{EnvEntry, EnvFile, EnvFileError, EnvRead};

const DOTENV_FILE: &str = ".env";
const DOTENV_EXAMPLE_FILE: &str = ".env.example";
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
    /// The usual ports of the `[ports] ignore` variables.
    pub ignored_ports: &'a [u16],
}

/// Runs every `.env` check.
pub fn check_env(input: &EnvCheckInput<'_>) -> Vec<Finding> {
    let mut findings = Vec::new();
    findings.extend(check_readable(DOTENV_FILE, input.env));
    findings.extend(check_readable(DOTENV_EXAMPLE_FILE, input.example));
    findings.extend(check_readable(DOTENV_TEST_FILE, input.test));
    findings.extend(check_env_exists(input));
    findings.extend(check_env_ignored(input));
    findings.extend(check_test_redefinitions(input));
    findings.extend(check_local_host_ports(input));
    findings.extend(check_unmanaged_local_ports(input));
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

fn scanned_entries<'a>(input: &'a EnvCheckInput<'_>) -> Vec<(&'static str, &'a EnvEntry)> {
    [
        (DOTENV_FILE, input.env),
        (DOTENV_EXAMPLE_FILE, input.example),
    ]
    .into_iter()
    .filter_map(|(file, read)| Some((file, parsed(read)?)))
    .flat_map(|(file, env_file)| env_file.entries().iter().map(move |entry| (file, entry)))
    .filter(|(_, entry)| !input.env_config_keys.contains(&entry.key))
    .collect()
}

fn check_local_host_ports(input: &EnvCheckInput<'_>) -> Vec<Finding> {
    let mut findings: Vec<Finding> = Vec::new();
    let mut reported: Vec<(&str, &str)> = Vec::new();
    for (file, entry) in scanned_entries(input) {
        for port in input.managed {
            let Some(default) = port.default else {
                continue;
            };
            let pair = (entry.key.as_str(), port.variable.as_str());
            if reported.contains(&pair) || !hardcodes_local_port(&entry.value, default) {
                continue;
            }
            reported.push(pair);
            findings.push(describe_hardcoded_value(
                file,
                &entry.key,
                &port.variable,
                default,
            ));
        }
    }
    findings
}

fn describe_hardcoded_value(file: &str, key: &str, variable: &str, default: u16) -> Finding {
    Finding::new(
        Level::Warning,
        Check::EnvLocalHostPort,
        format!(
            "{key} points at a local host on port {default}, the default of {variable}: it will not follow the assigned port unless [env] rebuilds it"
        ),
    )
    .in_file(file)
    .for_key(key)
}

fn check_unmanaged_local_ports(input: &EnvCheckInput<'_>) -> Vec<Finding> {
    let mut findings: Vec<Finding> = Vec::new();
    for (file, entry) in scanned_entries(input) {
        if findings
            .iter()
            .any(|finding| finding.key.as_deref() == Some(entry.key.as_str()))
        {
            continue;
        }
        let unmanaged = literal_local_ports(&entry.value)
            .into_iter()
            .find(|port| !is_port_known(input, *port));
        if let Some(port) = unmanaged {
            findings.push(describe_unmanaged_port(file, &entry.key, port));
        }
    }
    findings
}

fn is_port_known(input: &EnvCheckInput<'_>, port: u16) -> bool {
    input.ignored_ports.contains(&port)
        || input
            .managed
            .iter()
            .any(|managed| managed.default == Some(port))
}

fn describe_unmanaged_port(file: &str, key: &str, port: u16) -> Finding {
    Finding::new(
        Level::Warning,
        Check::EnvUnmanagedLocalPort,
        format!(
            "{key} points at a local host on port {port}, which no devslot variable manages: if this service belongs to this checkout, declare a variable such as WEB_PORT in [ports] extra of devslot.toml and in .env.example with {port}, then rebuild {key} in [env], e.g. \"http://localhost:${{WEB_PORT}}\""
        ),
    )
    .in_file(file)
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
        ignored_ports: Vec<u16>,
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
                ignored_ports: vec![],
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
                ignored_ports: &self.ignored_ports,
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
    fn should_not_report_a_managed_default_when_the_managed_variable_has_no_known_default() {
        let scenario = Scenario {
            env: parsed_env("URL=localhost:5432\n"),
            managed: vec![ManagedPort {
                default: None,
                ..managed_port("DB_PORT", 0)
            }],
            ..Scenario::healthy()
        };

        assert!(!checks_of(&scenario.findings()).contains(&Check::EnvLocalHostPort));
    }

    #[test]
    fn should_warn_on_the_example_when_it_hardcodes_a_local_default_port() {
        let scenario = Scenario {
            example: parsed_env("DATABASE_URL=postgres://localhost:5432/app\n"),
            ..Scenario::healthy()
        };

        let findings = scenario.findings();

        assert_eq!(
            (
                checks_of(&findings),
                findings[0].file.as_deref(),
                findings[0].key.as_deref()
            ),
            (
                vec![Check::EnvLocalHostPort],
                Some(".env.example"),
                Some("DATABASE_URL")
            )
        );
    }

    #[test]
    fn should_warn_about_an_unmanaged_port_without_printing_the_value() {
        let scenario = Scenario {
            env: parsed_env("PORT=3333\nAPP_BASE_URL=http://FAKE:secret@localhost:3334\n"),
            ..Scenario::healthy()
        };

        let findings = scenario.findings();

        assert_eq!(
            (
                checks_of(&findings),
                findings[0].file.as_deref(),
                findings[0].key.as_deref()
            ),
            (
                vec![Check::EnvUnmanagedLocalPort],
                Some(".env"),
                Some("APP_BASE_URL")
            )
        );
        assert!(findings[0].message.contains("port 3334"));
        assert!(!findings[0].message.contains("FAKE"));
        assert!(!findings[0].message.contains("secret"));
    }

    #[test]
    fn should_report_the_unmanaged_port_once_when_the_key_is_in_both_files_and_prefer_env() {
        let scenario = Scenario {
            env: parsed_env("APP_BASE_URL=http://localhost:3334\n"),
            example: parsed_env("APP_BASE_URL=http://localhost:3335\n"),
            ..Scenario::healthy()
        };

        let findings = scenario.findings();

        assert_eq!(
            (
                checks_of(&findings),
                findings[0].file.as_deref(),
                findings[0].message.contains("port 3334")
            ),
            (vec![Check::EnvUnmanagedLocalPort], Some(".env"), true)
        );
    }

    #[test]
    fn should_report_an_unmanaged_port_from_the_example_when_env_lacks_the_key() {
        let scenario = Scenario {
            example: parsed_env("APP_BASE_URL=http://localhost:3334\n"),
            ..Scenario::healthy()
        };

        assert_eq!(scenario.findings()[0].file.as_deref(), Some(".env.example"));
    }

    #[test]
    fn should_not_report_an_unmanaged_port_when_it_is_the_usual_port_of_an_ignored_variable() {
        let scenario = Scenario {
            env: parsed_env("MAIL_URL=smtp://localhost:1025\n"),
            ignored_ports: vec![1025],
            ..Scenario::healthy()
        };

        assert_eq!(scenario.findings(), vec![]);
    }

    #[test]
    fn should_not_report_an_unmanaged_port_when_the_key_is_rebuilt_by_the_env_section() {
        let scenario = Scenario {
            env: parsed_env("APP_BASE_URL=http://localhost:3334\n"),
            env_config_keys: vec!["APP_BASE_URL".to_owned()],
            ..Scenario::healthy()
        };

        assert_eq!(scenario.findings(), vec![]);
    }

    #[test]
    fn should_not_scan_the_devslot_block_for_unmanaged_ports() {
        let scenario = Scenario {
            env: parsed_env(&format!(
                "PORT=3333\n{BLOCK_START}\nAPP_BASE_URL=http://localhost:3334\n{BLOCK_END}\n"
            )),
            ..Scenario::healthy()
        };

        assert_eq!(scenario.findings(), vec![]);
    }
}
