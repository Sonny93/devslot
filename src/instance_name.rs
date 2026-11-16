//! The name of an instance, used as its compose project name.
//!
//! `<repo>-<folder>-<hash>`, where the folder loses the repo name as a prefix:
//! `~/dev/my-links` gives `my-links-3f2a` and `~/dev/my-links.feat-auth` gives
//! `my-links-feat-auth-3f2a`. The name is computed once, when the checkout is
//! registered, and stored: an existing instance is never renamed.

use std::fmt::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use sha2::{Digest, Sha256};

/// How many hex characters of the path hash a name starts with.
pub const DEFAULT_HASH_LENGTH: usize = 4;
/// How many more hex characters a name gets each time it is already taken.
pub const HASH_LENGTH_STEP: usize = 2;
/// The length of a whole SHA-256 in hex characters.
pub const FULL_HASH_LENGTH: usize = 64;

const FALLBACK_NAME: &str = "devslot";
const GIT_DIRECTORY_NAME: &str = ".git";
const BARE_SUFFIX: &str = ".git";
const SEPARATORS: [char; 2] = ['-', '_'];

/// Lowercases `text` and reduces it to the characters Compose accepts.
///
/// Every other character becomes `-`, runs of `-` collapse, and leading and
/// trailing `-` and `_` go away. Nothing left gives `devslot`.
pub fn sanitize(text: &str) -> String {
    let mut reduced = String::with_capacity(text.len());
    for character in text.to_lowercase().chars() {
        let is_allowed = character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || SEPARATORS.contains(&character);
        let kept = if is_allowed { character } else { '-' };
        if kept == '-' && reduced.ends_with('-') {
            continue;
        }
        reduced.push(kept);
    }
    let trimmed = reduced.trim_matches(SEPARATORS);
    if trimmed.is_empty() {
        return FALLBACK_NAME.to_owned();
    }
    trimmed.to_owned()
}

/// The repository name behind the common git directory: the folder above
/// `.git`, or for a bare repository its folder without a trailing `.git`.
pub fn repository_name_from_common_dir(common_dir: &Path) -> String {
    let folder_name = |path: &Path| {
        path.file_name()
            .map_or_default(|name| name.to_string_lossy().into_owned())
    };
    if common_dir
        .file_name()
        .is_some_and(|name| name == GIT_DIRECTORY_NAME)
    {
        return folder_name(common_dir.parent().unwrap_or(common_dir));
    }
    let name = folder_name(common_dir);
    name.strip_suffix(BARE_SUFFIX).unwrap_or(&name).to_owned()
}

/// The first `length` hex characters of the SHA-256 of the path's bytes.
pub fn path_hash(path: &Path, length: usize) -> String {
    let digest = Sha256::digest(path.as_os_str().as_bytes());
    let mut hex = String::with_capacity(FULL_HASH_LENGTH);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex[..length.min(FULL_HASH_LENGTH)].to_owned()
}

/// The folder part of the name: empty when the folder is the repo itself, the
/// folder without the repo prefix when it starts with it.
fn folder_part(repository: &str, folder: &str) -> String {
    if folder == repository {
        return String::new();
    }
    let rest = folder
        .strip_prefix(repository)
        .and_then(|rest| rest.strip_prefix(SEPARATORS));
    match rest {
        Some(rest) => rest.trim_matches(SEPARATORS).to_owned(),
        None => folder.to_owned(),
    }
}

/// The name of the instance at `checkout_root` in the repository `repository`.
pub fn derive_name(repository: &str, checkout_root: &Path, hash_length: usize) -> String {
    let repository = sanitize(repository);
    let folder = sanitize(
        &checkout_root
            .file_name()
            .map_or_default(|name| name.to_string_lossy().into_owned()),
    );
    let hash = path_hash(checkout_root, hash_length);
    [repository.clone(), folder_part(&repository, &folder), hash]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn name_of(repository: &str, path: &str) -> String {
        derive_name(repository, Path::new(path), DEFAULT_HASH_LENGTH)
    }

    #[test]
    fn should_name_the_main_checkout_after_the_repo_and_the_hash_when_the_folder_is_the_repo() {
        assert_eq!(
            name_of("my-links", "/home/sonny/dev/my-links"),
            "my-links-e319"
        );
    }

    #[test]
    fn should_drop_the_repo_prefix_when_the_folder_is_repo_dot_branch() {
        assert_eq!(
            name_of("my-links", "/home/sonny/dev/my-links.feat-auth"),
            "my-links-feat-auth-3560"
        );
    }

    #[test]
    fn should_keep_the_worktree_folder_when_it_does_not_start_with_the_repo() {
        assert_eq!(
            name_of(
                "my-links",
                "/home/sonny/dev/my-links/.claude/worktrees/agent-x"
            ),
            "my-links-agent-x-ac72"
        );
    }

    #[test]
    fn should_sanitize_both_parts_when_they_hold_spaces_and_capitals() {
        assert_eq!(
            name_of("Weird Name!", "/srv/Weird Name!/App"),
            "weird-name-app-2cd1"
        );
    }

    #[test]
    fn should_keep_the_whole_folder_when_the_repo_name_is_not_followed_by_a_separator() {
        assert!(name_of("app", "/work/application").starts_with("app-application-"));
    }

    #[test]
    fn should_use_a_longer_hash_when_asked() {
        assert_eq!(
            derive_name("my-links", Path::new("/home/sonny/dev/my-links"), 8),
            "my-links-e319266e"
        );
    }

    #[test]
    fn should_give_the_same_name_every_time_for_the_same_path() {
        assert_eq!(
            name_of("my-links", "/home/sonny/dev/my-links"),
            name_of("my-links", "/home/sonny/dev/my-links")
        );
    }

    #[test]
    fn should_fall_back_to_devslot_when_nothing_is_left_after_sanitizing() {
        assert_eq!(sanitize("..."), "devslot");
        assert_eq!(sanitize(""), "devslot");
    }

    #[test]
    fn should_collapse_dashes_and_trim_edges_when_sanitizing() {
        assert_eq!(sanitize("--My..Repo__"), "my-repo");
    }

    #[test]
    fn should_keep_underscores_inside_the_name_when_sanitizing() {
        assert_eq!(sanitize("my_repo"), "my_repo");
    }

    #[test]
    fn should_take_the_folder_above_git_when_the_common_dir_ends_with_dot_git() {
        assert_eq!(
            repository_name_from_common_dir(Path::new("/home/sonny/dev/my-links/.git")),
            "my-links"
        );
    }

    #[test]
    fn should_drop_the_dot_git_suffix_when_the_repository_is_bare() {
        assert_eq!(
            repository_name_from_common_dir(Path::new("/srv/git/my-links.git")),
            "my-links"
        );
    }

    #[test]
    fn should_keep_the_folder_name_when_a_bare_repository_has_no_suffix() {
        assert_eq!(
            repository_name_from_common_dir(Path::new("/srv/git/my-links")),
            "my-links"
        );
    }

    #[test]
    fn should_cap_the_hash_at_the_full_digest() {
        assert_eq!(path_hash(Path::new("/x"), 500).len(), FULL_HASH_LENGTH);
    }
}
