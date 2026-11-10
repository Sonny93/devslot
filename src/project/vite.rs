//! Vite detection: a recursive walk from the checkout root that honors
//! `.gitignore` and skips hidden directories and `node_modules`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use serde::Deserialize;

const NODE_MODULES: &str = "node_modules";
const PACKAGE_MANIFEST: &str = "package.json";
const VITE_PACKAGE: &str = "vite";
const VITE_CONFIG_STEM: &str = "vite.config.";
const VITE_CONFIG_EXTENSIONS: [&str; 6] = ["js", "mjs", "cjs", "ts", "mts", "cts"];

/// The environment variable the Vite config must read for the HMR port.
pub const HMR_PORT_VARIABLE: &str = "VITE_HMR_PORT";
/// The HMR port Vite uses without configuration.
pub const DEFAULT_HMR_PORT: u16 = 24678;

/// The Vite files found in a checkout, paths relative to its root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViteDetection {
    /// The `package.json` files that list `vite`.
    pub manifests: Vec<PathBuf>,
    /// The `vite.config.*` files.
    pub configs: Vec<PathBuf>,
}

impl ViteDetection {
    /// Whether anything Vite was found.
    pub fn is_detected(&self) -> bool {
        !self.manifests.is_empty() || !self.configs.is_empty()
    }

    /// The file that makes Vite known: the first manifest, else the first config.
    pub fn source_file(&self) -> Option<&Path> {
        self.manifests
            .first()
            .or_else(|| self.configs.first())
            .map(PathBuf::as_path)
    }
}

#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    dependencies: BTreeMap<String, serde_json::Value>,
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: BTreeMap<String, serde_json::Value>,
}

/// Walks `root` and lists its Vite manifests and configs, sorted by path.
pub fn detect_vite(root: &Path) -> ViteDetection {
    let mut detection = ViteDetection::default();
    let walker = WalkBuilder::new(root)
        .hidden(true)
        .require_git(false)
        .git_global(false)
        .follow_links(false)
        .sort_by_file_path(Ord::cmp)
        .filter_entry(|entry| entry.file_name() != NODE_MODULES)
        .build();
    for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(root) else {
            continue;
        };
        classify(entry.path(), relative, &mut detection);
    }
    detection
}

fn classify(absolute: &Path, relative: &Path, detection: &mut ViteDetection) {
    let Some(name) = absolute.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    if name == PACKAGE_MANIFEST && manifest_lists_vite(absolute) {
        detection.manifests.push(relative.to_path_buf());
    } else if is_vite_config_name(name) {
        detection.configs.push(relative.to_path_buf());
    }
}

fn is_vite_config_name(name: &str) -> bool {
    name.strip_prefix(VITE_CONFIG_STEM)
        .is_some_and(|extension| VITE_CONFIG_EXTENSIONS.contains(&extension))
}

fn manifest_lists_vite(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<Manifest>(&text) else {
        return false;
    };
    manifest.dependencies.contains_key(VITE_PACKAGE)
        || manifest.dev_dependencies.contains_key(VITE_PACKAGE)
}

/// Whether the config file mentions the HMR port variable.
pub fn config_reads_hmr_port(path: &Path) -> bool {
    std::fs::read(path)
        .is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains(HMR_PORT_VARIABLE))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn write(root: &Path, relative: &str, content: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn vite_manifest() -> &'static str {
        r#"{"devDependencies": {"vite": "^8"}}"#
    }

    #[test]
    fn should_find_the_manifest_and_config_when_they_sit_at_the_root() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "package.json", vite_manifest());
        write(root.path(), "vite.config.ts", "");

        let detection = detect_vite(root.path());

        assert_eq!(
            detection,
            ViteDetection {
                manifests: vec![PathBuf::from("package.json")],
                configs: vec![PathBuf::from("vite.config.ts")],
            }
        );
    }

    #[test]
    fn should_find_vite_in_a_nested_app_when_the_checkout_is_a_monorepo() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "package.json", r#"{"name": "root"}"#);
        write(root.path(), "apps/webapp/package.json", vite_manifest());
        write(root.path(), "apps/webapp/vite.config.mts", "");

        let detection = detect_vite(root.path());

        assert_eq!(
            detection.manifests,
            vec![PathBuf::from("apps/webapp/package.json")]
        );
    }

    #[test]
    fn should_find_vite_in_dependencies_when_it_is_not_a_dev_dependency() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            "package.json",
            r#"{"dependencies": {"vite": "1"}}"#,
        );

        assert_eq!(detect_vite(root.path()).manifests.len(), 1);
    }

    #[test]
    fn should_not_list_the_manifest_when_vite_is_absent() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            "package.json",
            r#"{"dependencies": {"react": "1"}}"#,
        );

        assert!(!detect_vite(root.path()).is_detected());
    }

    #[test]
    fn should_skip_node_modules_when_walking() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "node_modules/pkg/vite.config.js", "");

        assert!(!detect_vite(root.path()).is_detected());
    }

    #[test]
    fn should_skip_hidden_directories_when_walking() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), ".cache/vite.config.js", "");

        assert!(!detect_vite(root.path()).is_detected());
    }

    #[test]
    fn should_skip_gitignored_paths_when_the_root_has_a_gitignore() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), ".gitignore", "dist/\n");
        write(root.path(), "dist/vite.config.js", "");

        assert!(!detect_vite(root.path()).is_detected());
    }

    #[test]
    fn should_list_every_config_when_there_are_several() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "a/vite.config.js", "");
        write(root.path(), "b/vite.config.ts", "");

        assert_eq!(detect_vite(root.path()).configs.len(), 2);
    }

    #[test]
    fn should_prefer_the_manifest_as_source_when_both_are_found() {
        let detection = ViteDetection {
            manifests: vec![PathBuf::from("package.json")],
            configs: vec![PathBuf::from("vite.config.ts")],
        };

        assert_eq!(detection.source_file(), Some(Path::new("package.json")));
    }

    #[test]
    fn should_fall_back_to_the_config_as_source_when_no_manifest_lists_vite() {
        let detection = ViteDetection {
            manifests: vec![],
            configs: vec![PathBuf::from("vite.config.ts")],
        };

        assert_eq!(detection.source_file(), Some(Path::new("vite.config.ts")));
    }

    #[test]
    fn should_detect_the_hmr_variable_when_the_config_mentions_it() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            "vite.config.ts",
            "port: Number(process.env.VITE_HMR_PORT)",
        );

        assert!(config_reads_hmr_port(&root.path().join("vite.config.ts")));
    }

    #[test]
    fn should_not_detect_the_hmr_variable_when_the_config_ignores_it() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "vite.config.ts", "export default {}");

        assert!(!config_reads_hmr_port(&root.path().join("vite.config.ts")));
    }
}
