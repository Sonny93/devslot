//! The `[worktrees] require` scope of the global configuration: the checkouts
//! where an agent must work in a linked worktree. Unlike `[auto]`, an empty
//! list covers nothing: the guard is off until the user names what it covers.

use std::path::Path;

use crate::glob_match::{any_pattern_covers, check_pattern};

/// The `require` and `require_exclude` globs of `[worktrees]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequireScope {
    require: Vec<String>,
    exclude: Vec<String>,
}

impl RequireScope {
    /// A scope from the two lists; check each pattern with
    /// [`RequireScope::check_pattern`] first.
    pub const fn new(require: Vec<String>, exclude: Vec<String>) -> Self {
        Self { require, exclude }
    }

    /// Why `pattern` is not a usable glob, when it is not.
    pub fn check_pattern(pattern: &str) -> Result<(), String> {
        check_pattern(pattern)
    }

    /// Whether no pattern asks for worktrees: the guard is off everywhere.
    pub fn is_empty(&self) -> bool {
        self.require.is_empty()
    }

    /// Whether work at `path` must happen in a linked worktree: `path` is in
    /// `require` and not in `require_exclude`. A pattern covers a path when it
    /// matches the path or one of its ancestors.
    pub fn covers(&self, path: &Path, home: Option<&Path>) -> bool {
        any_pattern_covers(&self.require, path, home)
            && !any_pattern_covers(&self.exclude, path, home)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(require: &[&str], exclude: &[&str]) -> RequireScope {
        let owned = |patterns: &[&str]| patterns.iter().map(|text| (*text).to_owned()).collect();
        RequireScope::new(owned(require), owned(exclude))
    }

    fn covers(scope: &RequireScope, path: &str) -> bool {
        scope.covers(Path::new(path), Some(Path::new("/home/me")))
    }

    #[test]
    fn should_cover_nothing_when_require_is_empty() {
        let scope = scope(&[], &[]);

        assert!(scope.is_empty());
        assert!(!covers(&scope, "/anywhere/at/all"));
    }

    #[test]
    fn should_cover_only_the_required_paths() {
        let scope = scope(&["~/dev/*"], &[]);

        assert!(covers(&scope, "/home/me/dev/app"));
        assert!(!covers(&scope, "/home/me/other/app"));
    }

    #[test]
    fn should_let_require_exclude_win_over_require() {
        let scope = scope(&["/work/**"], &["/work/scratch"]);

        assert!(covers(&scope, "/work/app"));
        assert!(!covers(&scope, "/work/scratch/app"));
    }

    #[test]
    fn should_refuse_an_invalid_glob_and_an_empty_pattern() {
        assert!(RequireScope::check_pattern("/a/[b").is_err());
        assert!(RequireScope::check_pattern("").is_err());
        assert!(RequireScope::check_pattern("~/dev/*").is_ok());
    }
}
