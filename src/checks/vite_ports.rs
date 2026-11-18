//! Ports written as literals in a Vite config, a `package.json` script or the
//! justfile: every instance of the checkout would use the same one.

use std::path::Path;

use serde::Deserialize;

use super::finding::{Check, Finding, Level};
use super::local_host::literal_local_ports;
use crate::discovery::ManagedPort;
use crate::project::bounded_read::{BoundedRead, read_bounded};
use crate::project::vite::{HMR_PORT_VARIABLE, ViteDetection, read_text};

const JUSTFILE_NAMES: [&str; 2] = ["justfile", "Justfile"];
const VITE_WORD: &str = "vite";
const PORT_FLAG: &str = "--port";
const PORT_WORD: &str = "port";

const LOAD_ENV_HINT: &str = "Vite only loads the .env of its own folder: to read a root .env, call loadEnv(mode, <repo root>, '') and take only the keys you need";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LiteralPort {
    line: usize,
    port: u16,
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    scripts: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Reports the literal ports of the Vite configs, of the scripts of the
/// manifests that list Vite, and of the root justfile.
pub fn check_literal_ports(
    root: &Path,
    detection: &ViteDetection,
    managed: &[ManagedPort],
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for config in &detection.configs {
        findings.extend(check_config(root, config, managed));
    }
    for manifest in &detection.manifests {
        findings.extend(check_manifest_scripts(root, manifest, managed));
    }
    findings.extend(check_justfile(root, managed));
    findings
}

fn check_config(root: &Path, config: &Path, managed: &[ManagedPort]) -> Vec<Finding> {
    let Some(text) = read_text(&root.join(config)) else {
        return Vec::new();
    };
    if names_a_managed_variable(&text, managed) {
        return Vec::new();
    }
    config_literal_ports(&text)
        .into_iter()
        .map(|literal| {
            Finding::new(
                Level::Warning,
                Check::ViteLiteralPort,
                format!(
                    "line {} hardcodes port {}: {}",
                    literal.line,
                    literal.port,
                    config_advice(literal.port, managed)
                ),
            )
            .in_file(config.display().to_string())
        })
        .collect()
}

fn names_a_managed_variable(text: &str, managed: &[ManagedPort]) -> bool {
    managed
        .iter()
        .filter(|port| port.variable != HMR_PORT_VARIABLE)
        .any(|port| names_variable(text, &port.variable))
}

fn check_manifest_scripts(root: &Path, manifest: &Path, managed: &[ManagedPort]) -> Vec<Finding> {
    let Some(text) = read_text(&root.join(manifest)) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Manifest>(&text) else {
        return Vec::new();
    };
    parsed
        .scripts
        .iter()
        .filter_map(|(name, command)| Some((name, command_port(command.as_str()?)?)))
        .map(|(name, port)| {
            Finding::new(
                Level::Warning,
                Check::ViteLiteralPort,
                format!(
                    "script \"{name}\" passes {PORT_FLAG} {port}: {}",
                    script_advice(port, managed)
                ),
            )
            .in_file(manifest.display().to_string())
        })
        .collect()
}

fn check_justfile(root: &Path, managed: &[ManagedPort]) -> Vec<Finding> {
    let Some((name, text)) = read_justfile(root) else {
        return Vec::new();
    };
    text.lines()
        .enumerate()
        .filter(|(_, line)| line.to_ascii_lowercase().contains(VITE_WORD))
        .filter_map(|(index, line)| Some((index + 1, command_port(line)?)))
        .map(|(line, port)| {
            Finding::new(
                Level::Warning,
                Check::ViteLiteralPort,
                format!(
                    "line {line} passes {PORT_FLAG} {port} to vite: {}",
                    script_advice(port, managed)
                ),
            )
            .in_file(name)
        })
        .collect()
}

fn read_justfile(root: &Path) -> Option<(&'static str, String)> {
    JUSTFILE_NAMES.iter().find_map(|name| {
        let path = root.join(name);
        let Ok(BoundedRead::Bytes(bytes)) = read_bounded(&path) else {
            return None;
        };
        Some((*name, String::from_utf8_lossy(&bytes).into_owned()))
    })
}

fn variable_with_default(port: u16, managed: &[ManagedPort]) -> Option<&str> {
    managed
        .iter()
        .find(|candidate| candidate.default == Some(port))
        .map(|candidate| candidate.variable.as_str())
}

fn config_advice(port: u16, managed: &[ManagedPort]) -> String {
    match variable_with_default(port, managed) {
        Some(variable) => format!(
            "the port is the same for every instance, but {variable} already holds it: use that variable instead, e.g. Number(process.env.{variable}) || {port}. {LOAD_ENV_HINT}"
        ),
        None => format!(
            "the port is the same for every instance: read it from an environment variable with the literal as fallback, e.g. Number(process.env.WEB_PORT) || {port}, and declare that variable in [ports] extra of devslot.toml and in .env.example with {port}. {LOAD_ENV_HINT}"
        ),
    }
}

fn script_advice(port: u16, managed: &[ManagedPort]) -> String {
    match variable_with_default(port, managed) {
        Some(variable) => format!(
            "the port is the same for every instance, but {variable} already holds it: use that variable instead, e.g. {PORT_FLAG} \"${{{variable}:-{port}}}\""
        ),
        None => format!(
            "the port is the same for every instance: read it from an environment variable with the literal as fallback, e.g. {PORT_FLAG} \"${{WEB_PORT:-{port}}}\", and declare that variable in [ports] extra of devslot.toml and in .env.example with {port}"
        ),
    }
}

fn config_literal_ports(text: &str) -> Vec<LiteralPort> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !is_comment(line))
        .filter_map(|(index, line)| {
            Some(LiteralPort {
                line: index + 1,
                port: literal_port_in_line(line)?,
            })
        })
        .collect()
}

fn is_comment(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with('*')
}

fn literal_port_in_line(line: &str) -> Option<u16> {
    property_port(line)
        .or_else(|| assigned_port(line))
        .or_else(|| literal_local_ports(line).into_iter().next())
}

fn continues_an_identifier(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '$')
}

fn leading_port(text: &str) -> Option<u16> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn property_port(line: &str) -> Option<u16> {
    let lowercase = line.to_ascii_lowercase();
    lowercase.match_indices(PORT_WORD).find_map(|(start, _)| {
        let before = lowercase[..start].chars().next_back();
        if before.is_some_and(continues_an_identifier) {
            return None;
        }
        let after = lowercase[start + PORT_WORD.len()..]
            .trim_start_matches(['\'', '"', '`'])
            .trim_start()
            .strip_prefix(':')?;
        leading_port(after.trim_start())
    })
}

fn assigned_port(line: &str) -> Option<u16> {
    line.match_indices('=').find_map(|(index, _)| {
        let before = &line[..index];
        let after = &line[index + 1..];
        let is_plain_assignment = !before.ends_with([
            '=', '!', '<', '>', '+', '-', '*', '/', '%', '&', '|', '^', '?',
        ]) && !after.starts_with(['=', '>']);
        if !is_plain_assignment || !ends_with_a_port_name(before.trim_end()) {
            return None;
        }
        leading_port(after.trim_start())
    })
}

fn ends_with_a_port_name(text: &str) -> bool {
    let name: String = text
        .chars()
        .rev()
        .take_while(|character| continues_an_identifier(*character))
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    name.eq_ignore_ascii_case(PORT_WORD)
        || ["PORT", "Port", "_port"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn names_variable(text: &str, variable: &str) -> bool {
    text.match_indices(variable).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + variable.len()..].chars().next();
        !before.is_some_and(continues_an_identifier) && !after.is_some_and(continues_an_identifier)
    })
}

fn command_port(command: &str) -> Option<u16> {
    command.match_indices(PORT_FLAG).find_map(|(start, _)| {
        let after = &command[start + PORT_FLAG.len()..];
        if let Some(assigned) = after.strip_prefix('=') {
            return leading_port(assigned);
        }
        let separated = after.trim_start();
        if separated.len() == after.len() {
            return None;
        }
        leading_port(separated)
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::discovery::{PortKind, PortSource};

    fn root_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (name, content) in files {
            let path = root.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        root
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

    fn config_detection(config: &str) -> ViteDetection {
        ViteDetection {
            configs: vec![PathBuf::from(config)],
            ..ViteDetection::default()
        }
    }

    fn findings_for_config(text: &str, managed: &[ManagedPort]) -> Vec<Finding> {
        let root = root_with(&[("vite.config.ts", text)]);
        check_literal_ports(root.path(), &config_detection("vite.config.ts"), managed)
    }

    #[test]
    fn should_report_a_port_property_with_its_line_and_port() {
        let findings = findings_for_config("export default {\n  server: { port: 3333 },\n}\n", &[]);

        assert_eq!(
            (
                findings.len(),
                findings[0].check,
                findings[0].file.as_deref(),
                findings[0]
                    .message
                    .starts_with("line 2 hardcodes port 3333:")
            ),
            (1, Check::ViteLiteralPort, Some("vite.config.ts"), true)
        );
    }

    #[test]
    fn should_report_a_port_property_in_any_case_and_with_spaces() {
        let findings = findings_for_config("PORT :   4000\n'port': 4001\n", &[]);

        assert_eq!(findings.len(), 2);
    }

    #[test]
    fn should_report_an_identifier_ending_in_port_when_it_is_assigned_a_number() {
        let findings = findings_for_config("const DEV_SERVER_PORT = 3333\n", &[]);

        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn should_report_an_identifier_when_it_ends_like_a_port_name_in_camel_snake_or_exact_case() {
        let findings =
            findings_for_config("devServerPort = 3000\ndev_port = 3000\nport = 3000\n", &[]);

        assert_eq!(findings.len(), 3);
    }

    #[test]
    fn should_not_report_an_ordinary_word_that_ends_in_port_when_it_is_assigned_a_number() {
        let findings = findings_for_config(
            "report = 5\ntransport = 2\nviewport = 1\nsupport = 3\n",
            &[],
        );

        assert_eq!(findings, vec![]);
    }

    #[test]
    fn should_not_report_a_comparison_or_an_arrow_after_a_port_name() {
        let findings = findings_for_config("if (port == 3333) {}\nconst port => 3333\n", &[]);

        assert_eq!(findings, vec![]);
    }

    #[test]
    fn should_report_a_local_host_with_a_literal_port_as_a_proxy_target() {
        let findings = findings_for_config("proxy: { '/api': 'http://localhost:3334' }\n", &[]);

        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn should_not_report_a_port_read_from_the_environment() {
        let findings = findings_for_config(
            "port: Number(process.env.WEB_PORT) || 3333\nconst a = `http://localhost:${port}`\n",
            &[],
        );

        assert_eq!(findings, vec![]);
    }

    #[test]
    fn should_not_report_comment_lines() {
        let findings = findings_for_config("// port: 3333\n  * port: 3333\n/* x */\n", &[]);

        assert_eq!(findings, vec![]);
    }

    #[test]
    fn should_report_one_finding_per_line_even_when_a_line_has_two_ports() {
        let findings = findings_for_config("server: { port: 3333, hmr: { port: 3334 } }\n", &[]);

        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn should_not_match_port_inside_a_longer_word_when_it_is_a_property_key() {
        let findings = findings_for_config("import: 3333\nsupport: 3333\n", &[]);

        assert_eq!(findings, vec![]);
    }

    #[test]
    fn should_report_nothing_when_the_config_names_a_managed_variable() {
        let findings = findings_for_config(
            "const port = 3333\nprocess.env.WEB_PORT\n",
            &[managed_port("WEB_PORT", 3333)],
        );

        assert_eq!(findings, vec![]);
    }

    #[test]
    fn should_not_take_db_port_for_the_port_variable_when_matching_names() {
        let findings = findings_for_config(
            "const DB_PORT = 5432\nport: 3333\n",
            &[managed_port("PORT", 3334)],
        );

        assert_eq!(findings.len(), 2);
    }

    #[test]
    fn should_still_report_when_the_config_names_only_the_hmr_variable() {
        let findings = findings_for_config(
            "port: 3333\nhmr: { port: Number(process.env.VITE_HMR_PORT) }\n",
            &[managed_port("VITE_HMR_PORT", 24678)],
        );

        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn should_tell_to_use_the_managed_variable_when_the_port_is_its_default() {
        let findings = findings_for_config(
            "proxy: 'http://localhost:3334'\n",
            &[managed_port("PORT", 3334)],
        );

        assert!(findings[0].message.contains("use that variable instead"));
        assert!(findings[0].message.contains("process.env.PORT"));
    }

    #[test]
    fn should_suggest_a_new_variable_and_load_env_when_the_port_is_unknown() {
        let findings = findings_for_config("port: 3333\n", &[]);

        assert!(
            findings[0]
                .message
                .contains("Number(process.env.WEB_PORT) || 3333")
        );
        assert!(findings[0].message.contains("[ports] extra"));
        assert!(
            findings[0]
                .message
                .contains("loadEnv(mode, <repo root>, '')")
        );
    }

    #[test]
    fn should_report_a_port_flag_in_a_script_of_a_vite_manifest() {
        let root = root_with(&[(
            "package.json",
            r#"{"scripts": {"dev": "vite --port 5173", "build": "vite build", "alt": "vite --port=5174"}}"#,
        )]);
        let detection = ViteDetection {
            manifests: vec![PathBuf::from("package.json")],
            ..ViteDetection::default()
        };

        let findings = check_literal_ports(root.path(), &detection, &[]);

        assert_eq!(
            findings
                .iter()
                .map(|finding| (
                    finding.file.as_deref(),
                    finding.message.contains("script \"alt\""),
                    finding.message.contains("5174")
                ))
                .collect::<Vec<_>>(),
            vec![
                (Some("package.json"), true, true),
                (Some("package.json"), false, false)
            ]
        );
    }

    #[test]
    fn should_not_report_a_port_flag_when_it_is_followed_by_a_variable_or_a_longer_flag() {
        let root = root_with(&[(
            "package.json",
            r#"{"scripts": {"dev": "vite --port $WEB_PORT", "other": "vite --portal 3000"}}"#,
        )]);
        let detection = ViteDetection {
            manifests: vec![PathBuf::from("package.json")],
            ..ViteDetection::default()
        };

        assert_eq!(check_literal_ports(root.path(), &detection, &[]), vec![]);
    }

    #[test]
    fn should_report_the_justfile_line_that_runs_vite_with_a_port() {
        let root = root_with(&[(
            "justfile",
            "dev:\n    pnpm vite --port 5173\nserve:\n    node server --port 8080\n",
        )]);

        let findings = check_literal_ports(root.path(), &ViteDetection::default(), &[]);

        assert_eq!(
            (
                findings.len(),
                findings[0].file.as_deref(),
                findings[0].message.starts_with("line 2 passes --port 5173")
            ),
            (1, Some("justfile"), true)
        );
    }

    #[test]
    fn should_read_the_port_variable_in_the_script_advice_when_a_variable_holds_the_port() {
        let root = root_with(&[("justfile", "dev:\n    vite --port 3334\n")]);

        let findings = check_literal_ports(
            root.path(),
            &ViteDetection::default(),
            &[managed_port("PORT", 3334)],
        );

        assert!(findings[0].message.contains("\"${PORT:-3334}\""));
    }

    #[test]
    fn should_report_nothing_when_no_vite_file_exists() {
        let root = root_with(&[]);

        assert_eq!(
            check_literal_ports(root.path(), &ViteDetection::default(), &[]),
            vec![]
        );
    }
}
