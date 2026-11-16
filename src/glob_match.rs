//! Matching a path against the glob patterns of the global configuration, for
//! `[auto]` and `[worktrees] require`: `~` expansion, the canonical form of the
//! literal prefix, and a match against the path or any of its ancestors.

use std::path::{Path, PathBuf};

use globset::GlobBuilder;

const HOME_ALONE: &str = "~";
const HOME_PREFIX: &str = "~/";
const GLOB_METACHARACTERS: [char; 7] = ['*', '?', '[', ']', '{', '}', '\\'];
const SEPARATOR: char = '/';

/// Why `pattern` is not a usable glob, when it is not.
pub fn check_pattern(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() {
        return Err("a pattern must not be empty".to_owned());
    }
    compile(pattern).map(|_| ())
}

/// Whether one of `patterns` matches `path` or one of its ancestors.
pub fn any_pattern_covers(patterns: &[String], path: &Path, home: Option<&Path>) -> bool {
    patterns
        .iter()
        .any(|pattern| pattern_covers(pattern, path, home))
}

fn compile(pattern: &str) -> Result<globset::GlobMatcher, String> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|error| error.kind().to_string())
}

fn pattern_covers(pattern: &str, path: &Path, home: Option<&Path>) -> bool {
    let Some(expanded) = expand_home(pattern, home) else {
        return false;
    };
    let Ok(matcher) = compile(&canonicalize_prefix(&expanded)) else {
        return false;
    };
    path.ancestors().any(|ancestor| matcher.is_match(ancestor))
}

fn expand_home(pattern: &str, home: Option<&Path>) -> Option<String> {
    if pattern == HOME_ALONE {
        return home?.to_str().map(str::to_owned);
    }
    let Some(rest) = pattern.strip_prefix(HOME_PREFIX) else {
        return Some(pattern.to_owned());
    };
    let home = home?.to_str()?.trim_end_matches(SEPARATOR);
    Some(format!("{home}{SEPARATOR}{rest}"))
}

fn split_literal_prefix(pattern: &str) -> (&str, &str) {
    let Some(first_metacharacter) = pattern.find(GLOB_METACHARACTERS) else {
        return (pattern, "");
    };
    let cut = pattern[..first_metacharacter]
        .rfind(SEPARATOR)
        .map_or(0, |separator| separator + 1);
    pattern.split_at(cut)
}

fn canonicalize_prefix(pattern: &str) -> String {
    let (prefix, rest) = split_literal_prefix(pattern);
    let Some(canonical) = canonical_text(prefix) else {
        return pattern.to_owned();
    };
    if rest.is_empty() {
        return canonical;
    }
    format!("{}{SEPARATOR}{rest}", canonical.trim_end_matches(SEPARATOR))
}

fn canonical_text(prefix: &str) -> Option<String> {
    if prefix.is_empty() {
        return None;
    }
    let canonical: PathBuf = Path::new(prefix).canonicalize().ok()?;
    let text = canonical.to_str()?;
    Some(escape(text))
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if GLOB_METACHARACTERS.contains(&character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}
