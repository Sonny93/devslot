//! Which compose files a checkout has: the dev compose, the files `ds doctor`
//! scans, and the overrides merged into them.

use std::collections::BTreeSet;
use std::path::Path;

/// The ten names looked up for the dev compose, in order.
pub const DETECTION_NAMES: [&str; 10] = [
    "dev.compose.yaml",
    "dev.compose.yml",
    "compose.dev.yaml",
    "compose.dev.yml",
    "docker-compose.dev.yaml",
    "docker-compose.dev.yml",
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

const DEFAULT_NAMES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

const OVERRIDE_NAMES: [&str; 4] = [
    "compose.override.yaml",
    "compose.override.yml",
    "docker-compose.override.yaml",
    "docker-compose.override.yml",
];

const YAML_EXTENSIONS: [&str; 2] = [".yaml", ".yml"];
const SUFFIX_PATTERN_STEM: &str = ".compose";
const PREFIX_PATTERN_STEMS: [&str; 2] = ["compose.", "docker-compose."];

/// The dev compose the checkout uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevComposeSelection {
    /// The file to use, relative to the root.
    Selected(String),
    /// `[compose] file` names a file that does not exist.
    ConfiguredMissing(String),
    /// No compose file at all.
    NoneFound,
}

/// The dev compose: the configured file, otherwise the first detection name that exists.
pub fn select_dev_compose(root: &Path, configured: Option<&str>) -> DevComposeSelection {
    if let Some(file) = configured {
        return if root.join(file).is_file() {
            DevComposeSelection::Selected(file.to_owned())
        } else {
            DevComposeSelection::ConfiguredMissing(file.to_owned())
        };
    }
    DETECTION_NAMES
        .iter()
        .find(|name| root.join(name).is_file())
        .map_or(DevComposeSelection::NoneFound, |name| {
            DevComposeSelection::Selected((*name).to_owned())
        })
}

/// The compose file a bare `docker compose` picks: the first default name that exists.
pub fn default_named_compose(root: &Path) -> Option<String> {
    DEFAULT_NAMES
        .iter()
        .find(|name| root.join(name).is_file())
        .map(|name| (*name).to_owned())
}

/// The `compose.override.*` files that exist, in lookup order.
pub fn existing_overrides(root: &Path) -> Vec<String> {
    OVERRIDE_NAMES
        .iter()
        .filter(|name| root.join(name).is_file())
        .map(|name| (*name).to_owned())
        .collect()
}

/// The files `ds doctor` scans: the detection names that exist, then the
/// other compose-looking files at the root sorted by name. Overrides are
/// never listed.
pub fn scan_set(root: &Path) -> Vec<String> {
    let mut files: Vec<String> = DETECTION_NAMES
        .iter()
        .filter(|name| root.join(name).is_file())
        .map(|name| (*name).to_owned())
        .collect();
    let Ok(entries) = std::fs::read_dir(root) else {
        return files;
    };
    let extra: BTreeSet<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| is_scannable_name(name) && !files.contains(name))
        .collect();
    files.extend(extra);
    files
}

fn is_scannable_name(name: &str) -> bool {
    if OVERRIDE_NAMES.contains(&name) {
        return false;
    }
    let Some(stem) = strip_yaml_extension(name) else {
        return false;
    };
    let has_suffix_pattern =
        stem.len() > SUFFIX_PATTERN_STEM.len() && stem.ends_with(SUFFIX_PATTERN_STEM);
    let has_prefix_pattern = PREFIX_PATTERN_STEMS
        .iter()
        .any(|prefix| stem.len() > prefix.len() && stem.starts_with(prefix));
    has_suffix_pattern || has_prefix_pattern
}

fn strip_yaml_extension(name: &str) -> Option<&str> {
    YAML_EXTENSIONS
        .iter()
        .find_map(|extension| name.strip_suffix(extension))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn root_with(files: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for file in files {
            std::fs::write(root.path().join(file), "").unwrap();
        }
        root
    }

    fn selected(file: &str) -> DevComposeSelection {
        DevComposeSelection::Selected(file.to_owned())
    }

    #[test]
    fn should_prefer_the_dev_named_file_when_a_default_named_file_also_exists() {
        let root = root_with(&["compose.yml", "dev.compose.yml"]);

        assert_eq!(
            select_dev_compose(root.path(), None),
            selected("dev.compose.yml")
        );
    }

    #[test]
    fn should_follow_the_detection_order_when_several_dev_names_exist() {
        let root = root_with(&[
            "compose.dev.yml",
            "docker-compose.dev.yml",
            "dev.compose.yaml",
        ]);

        assert_eq!(
            select_dev_compose(root.path(), None),
            selected("dev.compose.yaml")
        );
    }

    #[test]
    fn should_fall_back_to_the_default_name_when_no_dev_name_exists() {
        let root = root_with(&["docker-compose.yml"]);

        assert_eq!(
            select_dev_compose(root.path(), None),
            selected("docker-compose.yml")
        );
    }

    #[test]
    fn should_report_none_found_when_there_is_no_compose_file() {
        let root = root_with(&["README.md"]);

        assert_eq!(
            select_dev_compose(root.path(), None),
            DevComposeSelection::NoneFound
        );
    }

    #[test]
    fn should_use_the_configured_file_when_it_exists() {
        let root = root_with(&["dev.compose.yml", "custom.yml"]);

        assert_eq!(
            select_dev_compose(root.path(), Some("custom.yml")),
            selected("custom.yml")
        );
    }

    #[test]
    fn should_report_the_configured_file_missing_when_it_does_not_exist() {
        let root = root_with(&["dev.compose.yml"]);

        assert_eq!(
            select_dev_compose(root.path(), Some("custom.yml")),
            DevComposeSelection::ConfiguredMissing("custom.yml".to_owned())
        );
    }

    #[test]
    fn should_pick_the_first_default_name_when_several_exist() {
        let root = root_with(&["docker-compose.yml", "compose.yml"]);

        assert_eq!(
            default_named_compose(root.path()),
            Some("compose.yml".to_owned())
        );
    }

    #[test]
    fn should_not_pick_a_dev_named_file_when_looking_for_the_default_named_compose() {
        let root = root_with(&["dev.compose.yml"]);

        assert_eq!(default_named_compose(root.path()), None);
    }

    #[test]
    fn should_list_detection_names_first_then_pattern_matches_sorted_when_scanning() {
        let root = root_with(&[
            "compose.yml",
            "dev.compose.yml",
            "prod.compose.yaml",
            "compose.image.yml",
            "docker-compose.ci.yml",
            "package.json",
        ]);

        assert_eq!(
            scan_set(root.path()),
            [
                "dev.compose.yml",
                "compose.yml",
                "compose.image.yml",
                "docker-compose.ci.yml",
                "prod.compose.yaml"
            ]
        );
    }

    #[test]
    fn should_never_scan_an_override_file_on_its_own() {
        let root = root_with(&[
            "compose.yml",
            "compose.override.yml",
            "docker-compose.override.yaml",
        ]);

        assert_eq!(scan_set(root.path()), ["compose.yml"]);
    }

    #[test]
    fn should_not_match_a_bare_pattern_stem_when_the_name_has_nothing_around_it() {
        let root = root_with(&[
            ".compose.yml",
            "compose..yml",
            "notcompose.yml",
            "compose.yml.bak",
        ]);

        assert_eq!(scan_set(root.path()).len(), 0);
    }

    #[test]
    fn should_list_existing_overrides_in_lookup_order() {
        let root = root_with(&["docker-compose.override.yml", "compose.override.yaml"]);

        assert_eq!(
            existing_overrides(root.path()),
            ["compose.override.yaml", "docker-compose.override.yml"]
        );
    }
}
