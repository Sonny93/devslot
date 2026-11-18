//! Checks on the Vite setup.

use std::path::Path;

use super::finding::{Check, Finding, Level};
use super::vite_ports::check_literal_ports;
use crate::discovery::ManagedPort;
use crate::project::vite::{
    HMR_PORT_VARIABLE, ViteDetection, config_reads_hmr_port, config_uses_middleware_mode,
};

const FIRST_WS_MAJOR: u32 = 8;

/// Runs every Vite check on what was detected under `root`.
pub fn check_vite(root: &Path, detection: &ViteDetection, managed: &[ManagedPort]) -> Vec<Finding> {
    let mut findings = Vec::new();
    if !detection.uses_assembler {
        findings.extend(check_middleware_configs_read_hmr_port(root, detection));
    }
    findings.extend(check_single_config(root, detection));
    findings.extend(check_literal_ports(root, detection, managed));
    findings
}

/// A standalone Vite dev server serves HMR on its own `server.port`: only the
/// middleware mode falls back to the fixed HMR port.
fn check_middleware_configs_read_hmr_port(root: &Path, detection: &ViteDetection) -> Vec<Finding> {
    detection
        .configs
        .iter()
        .filter(|config| config_uses_middleware_mode(&root.join(config)))
        .filter(|config| !config_reads_hmr_port(&root.join(config)))
        .map(|config| {
            Finding::new(
                Level::Warning,
                Check::ViteHmrPort,
                format!(
                    "the Vite config runs in middlewareMode and does not read {HMR_PORT_VARIABLE}: every instance would fight over HMR port 24678; {}",
                    suggested_fix(detection.vite_major)
                ),
            )
            .in_file(config.display().to_string())
        })
        .collect()
}

/// What to add to the Vite config: `server.ws.port` from Vite 8 on, where
/// `server.hmr.port` is deprecated, `server.hmr.port` before; both when the
/// version is unknown.
fn suggested_fix(vite_major: Option<u32>) -> String {
    let ws = format!(
        "server: {{ ws: {{ port: Number(process.env.{HMR_PORT_VARIABLE}) || undefined }} }}"
    );
    let hmr = format!(
        "server: {{ hmr: {{ port: Number(process.env.{HMR_PORT_VARIABLE}) || undefined }} }}"
    );
    match vite_major {
        Some(major) if major >= FIRST_WS_MAJOR => format!("add {ws}"),
        Some(_) => format!("add {hmr}"),
        None => format!("add {ws} for Vite {FIRST_WS_MAJOR} and later, or {hmr} before"),
    }
}

fn check_single_config(root: &Path, detection: &ViteDetection) -> Option<Finding> {
    let names: Vec<String> = detection
        .configs
        .iter()
        .filter(|config| {
            config_reads_hmr_port(&root.join(config))
                || config_uses_middleware_mode(&root.join(config))
        })
        .map(|config| config.display().to_string())
        .collect();
    if names.len() < 2 {
        return None;
    }
    Some(Finding::new(
        Level::Warning,
        Check::ViteMultipleConfigs,
        format!(
            "{} Vite configs found ({}): a single {HMR_PORT_VARIABLE} would be shared by all of them",
            names.len(),
            names.join(", ")
        ),
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;

    const MIDDLEWARE_CONFIG: &str = "export default { server: { middlewareMode: true } }";

    fn root_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (name, content) in files {
            let path = root.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        root
    }

    fn detection(manifests: &[&str], configs: &[&str]) -> ViteDetection {
        ViteDetection {
            manifests: manifests.iter().map(PathBuf::from).collect(),
            configs: configs.iter().map(PathBuf::from).collect(),
            ..ViteDetection::default()
        }
    }

    fn findings_of(root: &tempfile::TempDir, detection: &ViteDetection) -> Vec<Finding> {
        check_vite(root.path(), detection, &[])
    }

    fn checks_of(findings: &[Finding]) -> Vec<Check> {
        findings.iter().map(|finding| finding.check).collect()
    }

    #[test]
    fn should_warn_on_the_config_when_it_uses_middleware_mode_without_reading_the_hmr_variable() {
        let root = root_with(&[("vite.config.ts", MIDDLEWARE_CONFIG)]);

        let findings = findings_of(&root, &detection(&["package.json"], &["vite.config.ts"]));

        assert_eq!(
            (
                findings.len(),
                findings[0].check,
                findings[0].file.as_deref()
            ),
            (1, Check::ViteHmrPort, Some("vite.config.ts"))
        );
    }

    #[test]
    fn should_not_warn_when_a_standalone_config_does_not_read_the_hmr_variable() {
        let root = root_with(&[("vite.config.ts", "export default {}")]);

        let findings = findings_of(&root, &detection(&["package.json"], &["vite.config.ts"]));

        assert_eq!(findings, vec![]);
    }

    fn middleware_message(detection: &ViteDetection) -> String {
        let root = root_with(&[("vite.config.ts", MIDDLEWARE_CONFIG)]);
        findings_of(&root, detection)[0].message.clone()
    }

    #[test]
    fn should_suggest_server_ws_when_vite_is_8_or_later() {
        let vite = ViteDetection {
            vite_major: Some(8),
            ..detection(&["package.json"], &["vite.config.ts"])
        };

        let message = middleware_message(&vite);

        assert!(message.contains("server: { ws: { port: Number(process.env.VITE_HMR_PORT)"));
        assert!(!message.contains("hmr: {"));
    }

    #[test]
    fn should_suggest_server_hmr_when_vite_is_before_8() {
        let vite = ViteDetection {
            vite_major: Some(5),
            ..detection(&["package.json"], &["vite.config.ts"])
        };

        let message = middleware_message(&vite);

        assert!(message.contains("server: { hmr: { port:"));
        assert!(!message.contains("ws:"));
    }

    #[test]
    fn should_mention_both_when_the_vite_version_is_unknown() {
        let message = middleware_message(&detection(&["package.json"], &["vite.config.ts"]));

        assert!(message.contains("ws: { port:") && message.contains("hmr: { port:"));
    }

    #[test]
    fn should_report_nothing_when_the_project_uses_the_adonis_assembler() {
        let root = root_with(&[("vite.config.ts", MIDDLEWARE_CONFIG)]);
        let vite = ViteDetection {
            uses_assembler: true,
            ..detection(&["package.json"], &["vite.config.ts"])
        };

        assert_eq!(findings_of(&root, &vite), vec![]);
    }

    #[test]
    fn should_report_nothing_when_the_middleware_config_reads_the_hmr_variable() {
        let root = root_with(&[(
            "vite.config.ts",
            "middlewareMode: true, hmr: Number(process.env.VITE_HMR_PORT)",
        )]);

        let findings = findings_of(&root, &detection(&["package.json"], &["vite.config.ts"]));

        assert_eq!(findings, vec![]);
    }

    #[test]
    fn should_not_warn_on_the_manifest_when_vite_is_listed_and_no_config_exists() {
        let root = root_with(&[]);

        let findings = findings_of(&root, &detection(&["package.json"], &[]));

        assert_eq!(findings, vec![]);
    }

    #[test]
    fn should_warn_once_about_the_count_when_several_configs_read_the_hmr_variable() {
        let root = root_with(&[
            ("a/vite.config.js", "VITE_HMR_PORT"),
            ("b/vite.config.js", "VITE_HMR_PORT"),
        ]);

        let findings = findings_of(
            &root,
            &detection(&[], &["a/vite.config.js", "b/vite.config.js"]),
        );

        assert_eq!(checks_of(&findings), vec![Check::ViteMultipleConfigs]);
    }

    #[test]
    fn should_count_the_configs_in_middleware_mode_when_several_configs_exist() {
        let root = root_with(&[
            ("a/vite.config.js", "VITE_HMR_PORT"),
            ("b/vite.config.js", MIDDLEWARE_CONFIG),
        ]);

        let findings = findings_of(
            &root,
            &detection(&[], &["a/vite.config.js", "b/vite.config.js"]),
        );

        assert_eq!(
            checks_of(&findings),
            vec![Check::ViteHmrPort, Check::ViteMultipleConfigs]
        );
    }

    #[test]
    fn should_not_warn_about_the_count_when_only_one_of_several_configs_reads_the_hmr_variable() {
        let root = root_with(&[
            ("a/vite.config.js", "VITE_HMR_PORT"),
            ("b/vite.config.js", "export default {}"),
        ]);

        let findings = findings_of(
            &root,
            &detection(&[], &["a/vite.config.js", "b/vite.config.js"]),
        );

        assert_eq!(findings, vec![]);
    }

    #[test]
    fn should_report_the_literal_ports_of_the_configs() {
        let root = root_with(&[(
            "vite.config.ts",
            "export default { server: { port: 3333 } }",
        )]);

        let findings = findings_of(&root, &detection(&["package.json"], &["vite.config.ts"]));

        assert_eq!(checks_of(&findings), vec![Check::ViteLiteralPort]);
    }

    #[test]
    fn should_report_nothing_when_vite_is_absent() {
        let root = root_with(&[]);

        assert_eq!(findings_of(&root, &ViteDetection::default()), vec![]);
    }
}
