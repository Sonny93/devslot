//! The `[auto]` scope of the global configuration: which checkouts `ds ensure`
//! may register by itself.

use std::path::Path;

use crate::glob_match::{any_pattern_covers, check_pattern};

/// The include and exclude globs of `[auto]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoScope {
    include: Vec<String>,
    exclude: Vec<String>,
}

impl AutoScope {
    /// A scope from the two lists; check each pattern with
    /// [`AutoScope::check_pattern`] first.
    pub const fn new(include: Vec<String>, exclude: Vec<String>) -> Self {
        Self { include, exclude }
    }

    /// Why `pattern` is not a usable glob, when it is not.
    pub fn check_pattern(pattern: &str) -> Result<(), String> {
        check_pattern(pattern)
    }

    /// Whether `ds ensure` may register the checkout at `checkout_root`: it is
    /// in `include` (or `include` is empty) and not in `exclude`. A pattern
    /// covers a path when it matches the path or one of its ancestors.
    pub fn covers(&self, checkout_root: &Path, home: Option<&Path>) -> bool {
        let is_included =
            self.include.is_empty() || any_pattern_covers(&self.include, checkout_root, home);
        is_included && !any_pattern_covers(&self.exclude, checkout_root, home)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn scope(include: &[&str], exclude: &[&str]) -> AutoScope {
        let owned = |patterns: &[&str]| patterns.iter().map(|text| (*text).to_owned()).collect();
        AutoScope::new(owned(include), owned(exclude))
    }

    fn covers(scope: &AutoScope, path: &str) -> bool {
        scope.covers(Path::new(path), Some(Path::new("/home/me")))
    }

    #[test]
    fn should_cover_everything_when_include_is_empty() {
        assert!(covers(&scope(&[], &[]), "/anywhere/at/all"));
    }

    #[test]
    fn should_cover_only_the_included_paths_when_include_is_set() {
        let scope = scope(&["/work/*"], &[]);

        assert!(covers(&scope, "/work/app"));
        assert!(!covers(&scope, "/other/app"));
    }

    #[test]
    fn should_let_exclude_win_over_include() {
        let scope = scope(&["/work/**"], &["/work/vendor"]);

        assert!(covers(&scope, "/work/app"));
        assert!(!covers(&scope, "/work/vendor"));
        assert!(!covers(&scope, "/work/vendor/lib"));
    }

    #[test]
    fn should_exclude_when_include_is_empty() {
        assert!(!covers(&scope(&[], &["/tmp/**"]), "/tmp/x"));
    }

    #[test]
    fn should_cover_a_path_below_the_pattern_with_or_without_a_double_star() {
        assert!(covers(&scope(&["/clones"], &[]), "/clones/x"));
        assert!(covers(&scope(&["/clones/**"], &[]), "/clones/x"));
    }

    #[test]
    fn should_not_let_a_star_cross_a_separator() {
        let scope = scope(&["/clones/*"], &[]);

        assert!(covers(&scope, "/clones/x/y"));
        assert!(!covers(&scope, "/clones"));
    }

    #[test]
    fn should_not_match_a_star_inside_one_component_across_folders() {
        assert!(!covers(&scope(&["/a*b"], &[]), "/a/x/b"));
    }

    #[test]
    fn should_expand_the_home_folder_in_a_pattern() {
        let scope = scope(&["~/clones"], &[]);

        assert!(covers(&scope, "/home/me/clones/x"));
        assert!(!covers(&scope, "/home/you/clones/x"));
    }

    #[test]
    fn should_match_nothing_when_a_home_pattern_has_no_home() {
        let scope = scope(&["~/clones"], &[]);

        assert!(!scope.covers(Path::new("/home/me/clones/x"), None));
    }

    #[test]
    fn should_match_a_symlinked_prefix_through_its_canonical_path() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        std::fs::create_dir_all(base.join("real/clones/x")).unwrap();
        symlink(base.join("real"), base.join("link")).unwrap();
        let pattern = format!("{}/link/clones/*", base.display());

        assert!(scope(&[&pattern], &[]).covers(&base.join("real/clones/x"), None));
    }

    #[test]
    fn should_match_a_symlinked_literal_pattern_through_its_canonical_path() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        std::fs::create_dir_all(base.join("real/x")).unwrap();
        symlink(base.join("real"), base.join("link")).unwrap();
        let pattern = format!("{}/link", base.display());

        assert!(scope(&[&pattern], &[]).covers(&base.join("real/x"), None));
    }

    #[test]
    fn should_keep_the_pattern_as_written_when_its_prefix_does_not_exist() {
        assert!(covers(
            &scope(&["/no/such/folder/*"], &[]),
            "/no/such/folder/x"
        ));
    }

    #[test]
    fn should_refuse_an_invalid_glob_and_an_empty_pattern() {
        assert!(AutoScope::check_pattern("/a/[b").is_err());
        assert!(AutoScope::check_pattern("").is_err());
        assert!(AutoScope::check_pattern("~/clones/**").is_ok());
    }
}
