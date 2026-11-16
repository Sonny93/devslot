//! The per-checkout switch of the worktree guard: `[worktrees] require = false`
//! in `devslot.local.toml`, the untracked file of the user. The same key in the
//! committed `devslot.toml` is never honoured, so a cloned repository cannot
//! switch the guard off. The reader parses this one key and nothing else, so
//! the rest of the project configuration cannot affect the guard.

use std::io;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::project::bounded_read::{BoundedRead, Unreadable, read_bounded};
use crate::project_config::LOCAL_CONFIG_FILE;

/// Why the key cannot be read.
#[derive(Debug, Error)]
pub enum OverrideError {
    /// The file exists but cannot be read.
    #[error("cannot read {file}: {source}")]
    Read {
        /// The file name.
        file: &'static str,
        /// The underlying error.
        source: io::Error,
    },
    /// The file is not a regular file or is larger than 1 MiB.
    #[error("cannot read {file}: {reason}")]
    Unusable {
        /// The file name.
        file: &'static str,
        /// "not a regular file" or "larger than 1 MiB".
        reason: &'static str,
    },
    /// The file is not valid TOML, or the key is not a boolean.
    #[error("cannot parse {file}: {message}")]
    Parse {
        /// The file name.
        file: &'static str,
        /// What the parser reports.
        message: String,
    },
}

#[derive(Debug, Default, Deserialize)]
struct RawFile {
    #[serde(default)]
    worktrees: RawWorktrees,
}

#[derive(Debug, Default, Deserialize)]
struct RawWorktrees {
    require: Option<bool>,
}

/// The value of `[worktrees] require` in `file` of the checkout at `root`;
/// `None` when the file or the key is missing.
pub fn read_require_key(root: &Path, file: &'static str) -> Result<Option<bool>, OverrideError> {
    let bytes = match read_bounded(&root.join(file)) {
        Ok(BoundedRead::Bytes(bytes)) => bytes,
        Ok(BoundedRead::Missing) => return Ok(None),
        Ok(BoundedRead::Unusable(Unreadable::NotRegularFile)) => {
            return Err(OverrideError::Unusable {
                file,
                reason: "not a regular file",
            });
        }
        Ok(BoundedRead::Unusable(Unreadable::TooLarge)) => {
            return Err(OverrideError::Unusable {
                file,
                reason: "larger than 1 MiB",
            });
        }
        Err(source) => return Err(OverrideError::Read { file, source }),
    };
    let text = String::from_utf8_lossy(&bytes);
    let raw: RawFile = toml_edit::de::from_str(&text).map_err(|error| OverrideError::Parse {
        file,
        message: error.message().to_owned(),
    })?;
    Ok(raw.worktrees.require)
}

/// Whether `devslot.local.toml` of the checkout at `root` switches the guard
/// off for it (`[worktrees] require = false`).
pub fn is_guard_disabled_locally(root: &Path) -> Result<bool, OverrideError> {
    Ok(read_require_key(root, LOCAL_CONFIG_FILE)? == Some(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disabled_by(text: &str) -> Result<bool, OverrideError> {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(LOCAL_CONFIG_FILE), text).unwrap();
        is_guard_disabled_locally(root.path())
    }

    #[test]
    fn should_disable_the_guard_when_the_local_file_sets_require_to_false() {
        assert!(disabled_by("[worktrees]\nrequire = false\n").unwrap());
    }

    #[test]
    fn should_keep_the_guard_when_the_local_file_sets_require_to_true() {
        assert!(!disabled_by("[worktrees]\nrequire = true\n").unwrap());
    }

    #[test]
    fn should_keep_the_guard_when_the_local_file_does_not_set_the_key() {
        assert!(!disabled_by("run = \"pnpm dev\"\n[write]\nenabled = false\n").unwrap());
    }

    #[test]
    fn should_keep_the_guard_when_there_is_no_local_file() {
        let root = tempfile::tempdir().unwrap();

        assert!(!is_guard_disabled_locally(root.path()).unwrap());
    }

    #[test]
    fn should_ignore_the_other_keys_even_when_they_are_wrong() {
        assert!(disabled_by("[ports]\nextra = 3\n[worktrees]\nrequire = false\n").unwrap());
    }

    #[test]
    fn should_fail_when_the_key_is_not_a_boolean() {
        let failure = disabled_by("[worktrees]\nrequire = [\"/work\"]\n").unwrap_err();

        assert!(matches!(failure, OverrideError::Parse { .. }));
    }

    #[test]
    fn should_fail_when_the_file_is_not_toml() {
        assert!(matches!(
            disabled_by("[worktrees\n").unwrap_err(),
            OverrideError::Parse { .. }
        ));
    }
}
