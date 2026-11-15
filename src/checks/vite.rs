//! Checks on the Vite setup.

use std::path::Path;

use super::finding::{Check, Finding, Level};
use crate::project::vite::{HMR_PORT_VARIABLE, ViteDetection, config_reads_hmr_port};

const FIRST_WS_MAJOR: u32 = 8;

/// Runs every Vite check on what was detected under `root`.
pub fn check_vite(root: &Path, detection: &ViteDetection) -> Vec<Finding> {
    let mut findings = Vec::new();
    if !detection.uses_assembler {
        findings.extend(check_configs_read_hmr_port(root, detection));
        findings.extend(check_manifests_have_a_config(detection));
    }
    findings.extend(check_single_config(detection));
    findings
}

fn check_configs_read_hmr_port(root: &Path, detection: &ViteDetection) -> Vec<Finding> {
    detection
        .configs
        .iter()
        .filter(|config| !config_reads_hmr_port(&root.join(config)))
        .map(|config| {
            Finding::new(
                Level::Warning,
                Check::ViteHmrPort,
                format!(
                    "the Vite config does not read {HMR_PORT_VARIABLE}: every instance would fight over HMR port 24678; {}",
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

fn check_manifests_have_a_config(detection: &ViteDetection) -> Vec<Finding> {
    detection
        .manifests
        .iter()
        .filter(|manifest| {
            let directory = manifest.parent();
            !detection
                .configs
                .iter()
                .any(|config| config.parent() == directory)
        })
        .map(|manifest| {
            Finding::new(
                Level::Warning,
                Check::ViteHmrPort,
                format!(
                    "package.json lists vite but no vite.config.* sits next to it, so {HMR_PORT_VARIABLE} is not read: every instance would fight over HMR port 24678"
                ),
            )
            .in_file(manifest.display().to_string())
        })
        .collect()
}

fn check_single_config(detection: &ViteDetection) -> Option<Finding> {
    if detection.configs.len() < 2 {
        return None;
    }
    let names: Vec<String> = detection
        .configs
        .iter()
        .map(|config| config.display().to_string())
        .collect();
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

    #[test]
    fn should_warn_on_the_config_when_it_does_not_read_the_hmr_variable() {
        let root = root_with(&[("vite.config.ts", "export default {}")]);

        let findings = check_vite(
            root.path(),
            &detection(&["package.json"], &["vite.config.ts"]),
        );

        assert_eq!(
            (
                findings.len(),
                findings[0].check,
                findings[0].file.as_deref()
            ),
            (1, Check::ViteHmrPort, Some("vite.config.ts"))
        );
    }

    fn message_of(detection: &ViteDetection) -> String {
        let root = root_with(&[("vite.config.ts", "export default {}")]);
        check_vite(root.path(), detection)[0].message.clone()
    }

    #[test]
    fn should_suggest_server_ws_when_vite_is_8_or_later() {
        let vite = ViteDetection {
            vite_major: Some(8),
            ..detection(&["package.json"], &["vite.config.ts"])
        };

        let message = message_of(&vite);

        assert!(message.contains("server: { ws: { port: Number(process.env.VITE_HMR_PORT)"));
        assert!(!message.contains("hmr"));
    }

    #[test]
    fn should_suggest_server_hmr_when_vite_is_before_8() {
        let vite = ViteDetection {
            vite_major: Some(5),
            ..detection(&["package.json"], &["vite.config.ts"])
        };

        let message = message_of(&vite);

        assert!(message.contains("server: { hmr: { port:"));
        assert!(!message.contains("ws:"));
    }

    #[test]
    fn should_mention_both_when_the_vite_version_is_unknown() {
        let message = message_of(&detection(&["package.json"], &["vite.config.ts"]));

        assert!(message.contains("ws: { port:") && message.contains("hmr: { port:"));
    }

    #[test]
    fn should_report_nothing_when_the_project_uses_the_adonis_assembler() {
        let root = root_with(&[("vite.config.ts", "export default {}")]);
        let vite = ViteDetection {
            uses_assembler: true,
            ..detection(&["package.json"], &["vite.config.ts"])
        };

        assert_eq!(check_vite(root.path(), &vite), vec![]);
    }

    #[test]
    fn should_report_nothing_when_the_config_reads_the_hmr_variable() {
        let root = root_with(&[("vite.config.ts", "Number(process.env.VITE_HMR_PORT)")]);

        let findings = check_vite(
            root.path(),
            &detection(&["package.json"], &["vite.config.ts"]),
        );

        assert_eq!(findings, vec![]);
    }

    #[test]
    fn should_warn_on_the_manifest_when_vite_is_listed_and_no_config_exists() {
        let root = root_with(&[]);

        let findings = check_vite(root.path(), &detection(&["package.json"], &[]));

        assert_eq!(
            (findings.len(), findings[0].file.as_deref()),
            (1, Some("package.json"))
        );
    }

    #[test]
    fn should_not_warn_on_the_manifest_when_a_config_sits_in_the_same_folder() {
        let root = root_with(&[("apps/web/vite.config.ts", "VITE_HMR_PORT")]);

        let findings = check_vite(
            root.path(),
            &detection(&["apps/web/package.json"], &["apps/web/vite.config.ts"]),
        );

        assert_eq!(findings, vec![]);
    }

    #[test]
    fn should_warn_once_about_the_count_when_several_configs_exist() {
        let root = root_with(&[
            ("a/vite.config.js", "VITE_HMR_PORT"),
            ("b/vite.config.js", "VITE_HMR_PORT"),
        ]);

        let findings = check_vite(
            root.path(),
            &detection(&[], &["a/vite.config.js", "b/vite.config.js"]),
        );

        assert_eq!(
            findings
                .iter()
                .map(|finding| finding.check)
                .collect::<Vec<_>>(),
            vec![Check::ViteMultipleConfigs]
        );
    }

    #[test]
    fn should_report_nothing_when_vite_is_absent() {
        let root = root_with(&[]);

        assert_eq!(check_vite(root.path(), &ViteDetection::default()), vec![]);
    }
}
