//! Which folder a worktree is. A removed and recreated worktree at the same
//! path is another folder, even when the file system reuses the inode, so the
//! identity holds the inode of the folder, the inode of the `.git` file git
//! writes at creation, and the birth time where the file system records it.

use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::UNIX_EPOCH;

const GIT_FILE: &str = ".git";
const NO_BIRTH_TIME: &str = "-";

/// The identity of a worktree folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorktreeIdentity {
    folder_inode: u64,
    git_file_inode: u64,
    birth_nanoseconds: Option<u128>,
}

impl WorktreeIdentity {
    /// The identity of the worktree at `path`; an error when the folder or its
    /// `.git` is missing.
    pub fn of(path: &Path) -> std::io::Result<Self> {
        let folder = std::fs::metadata(path)?;
        let git_file = std::fs::symlink_metadata(path.join(GIT_FILE))?;
        let birth_nanoseconds = folder
            .created()
            .ok()
            .and_then(|created| created.duration_since(UNIX_EPOCH).ok())
            .map(|elapsed| elapsed.as_nanos());
        Ok(Self {
            folder_inode: folder.ino(),
            git_file_inode: git_file.ino(),
            birth_nanoseconds,
        })
    }

    /// Whether both are the same folder; the birth time counts only when both
    /// sides have one.
    pub fn is_same_folder_as(&self, other: &Self) -> bool {
        let is_same_birth = match (self.birth_nanoseconds, other.birth_nanoseconds) {
            (Some(first), Some(second)) => first == second,
            _ => true,
        };
        self.folder_inode == other.folder_inode
            && self.git_file_inode == other.git_file_inode
            && is_same_birth
    }

    /// The identity as one word, for [`WorktreeIdentity::from_text`].
    pub fn to_text(&self) -> String {
        let birth = self.birth_nanoseconds.map_or_else(
            || NO_BIRTH_TIME.to_owned(),
            |nanoseconds| nanoseconds.to_string(),
        );
        format!("{}:{}:{birth}", self.folder_inode, self.git_file_inode)
    }

    /// Reads what [`WorktreeIdentity::to_text`] wrote.
    pub fn from_text(text: &str) -> Option<Self> {
        let mut parts = text.split(':');
        let folder_inode = parts.next()?.parse().ok()?;
        let git_file_inode = parts.next()?.parse().ok()?;
        let birth = parts.next()?;
        let birth_nanoseconds = if birth == NO_BIRTH_TIME {
            None
        } else {
            Some(birth.parse().ok()?)
        };
        Some(Self {
            folder_inode,
            git_file_inode,
            birth_nanoseconds,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worktree_at(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join(GIT_FILE), "gitdir: x").unwrap();
    }

    #[test]
    fn should_round_trip_through_its_text() {
        let directory = tempfile::tempdir().unwrap();
        worktree_at(&directory.path().join("feat"));
        let identity = WorktreeIdentity::of(&directory.path().join("feat")).unwrap();

        assert_eq!(
            WorktreeIdentity::from_text(&identity.to_text()),
            Some(identity)
        );
    }

    #[test]
    fn should_have_no_identity_when_the_git_file_is_missing() {
        let directory = tempfile::tempdir().unwrap();

        assert!(WorktreeIdentity::of(directory.path()).is_err());
    }

    #[test]
    fn should_differ_when_the_git_file_was_replaced_in_the_same_folder() {
        let directory = tempfile::tempdir().unwrap();
        worktree_at(directory.path());
        let before = WorktreeIdentity::of(directory.path()).unwrap();
        std::fs::rename(
            directory.path().join(GIT_FILE),
            directory.path().join("old-git"),
        )
        .unwrap();
        std::fs::write(directory.path().join(GIT_FILE), "gitdir: y").unwrap();

        let after = WorktreeIdentity::of(directory.path()).unwrap();

        assert!(!before.is_same_folder_as(&after));
    }

    #[test]
    fn should_compare_the_inodes_alone_when_a_birth_time_is_unknown() {
        let known = WorktreeIdentity {
            folder_inode: 7,
            git_file_inode: 9,
            birth_nanoseconds: Some(1),
        };
        let unknown = WorktreeIdentity {
            birth_nanoseconds: None,
            ..known
        };
        let other_git_file = WorktreeIdentity {
            git_file_inode: 10,
            ..unknown
        };

        assert!(known.is_same_folder_as(&unknown));
        assert!(!known.is_same_folder_as(&other_git_file));
    }
}
