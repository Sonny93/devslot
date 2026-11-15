//! The `.env` file of a checkout: reading it for a rewrite, creating it from
//! another file, and replacing it atomically. A `.env` that is a symlink is
//! never followed, so a rewrite cannot reach a file outside the checkout.

use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use thiserror::Error;

use super::bounded_read::{BoundedRead, MAX_FILE_BYTES, Unreadable, read_bounded};
use super::dotenv_block::{BlockError, strip_block};

/// The name of the file devslot writes in the checkout root.
pub const ENV_FILE: &str = ".env";
/// The example file `.env` is created from when the main checkout has none.
pub const ENV_EXAMPLE_FILE: &str = ".env.example";

const TEMP_PREFIX: &str = ".env.devslot-";
const NEW_FILE_MODE: u32 = 0o600;
const PERMISSION_BITS: u32 = 0o777;
const SUFFIX_BYTES: usize = 8;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Why `.env` cannot be read or written. No message holds a file value.
#[derive(Debug, Error)]
pub enum EnvFileError {
    /// `.env` is a symbolic link.
    #[error("{path} is a symbolic link; ds does not write through it")]
    Symlink {
        /// The file.
        path: PathBuf,
    },
    /// The file is a folder, a FIFO or another special file.
    #[error("{path} is not a regular file")]
    NotRegularFile {
        /// The file.
        path: PathBuf,
    },
    /// The file is larger than 1 MiB.
    #[error("{path} is larger than 1 MiB")]
    TooLarge {
        /// The file.
        path: PathBuf,
    },
    /// The file is not valid UTF-8.
    #[error("{path} is not valid UTF-8")]
    NotUtf8 {
        /// The file.
        path: PathBuf,
    },
    /// `.env` exists although it was about to be created.
    #[error("{path} already exists")]
    AlreadyExists {
        /// The file.
        path: PathBuf,
    },
    /// The source of a creation holds a block that cannot be removed.
    #[error("{path}: {source}")]
    MalformedSourceBlock {
        /// The source file.
        path: PathBuf,
        /// What is wrong with its block.
        source: BlockError,
    },
    /// A read failed.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The file or folder.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// A write failed.
    #[error("cannot write {path}: {source}")]
    Write {
        /// The file or folder.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
}

/// What `.env` holds before a rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvContent {
    /// There is no `.env`.
    Missing,
    /// The text of the file.
    Text(String),
}

fn read_error(path: &Path) -> impl FnOnce(io::Error) -> EnvFileError {
    let path = path.to_path_buf();
    move |source| EnvFileError::Read { path, source }
}

fn write_error(path: &Path) -> impl FnOnce(io::Error) -> EnvFileError {
    let path = path.to_path_buf();
    move |source| EnvFileError::Write { path, source }
}

fn inspect_env(root: &Path) -> Result<Option<std::fs::Metadata>, EnvFileError> {
    let path = root.join(ENV_FILE);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(read_error(&path)(error)),
    };
    if metadata.file_type().is_symlink() {
        return Err(EnvFileError::Symlink { path });
    }
    if !metadata.is_file() {
        return Err(EnvFileError::NotRegularFile { path });
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(EnvFileError::TooLarge { path });
    }
    Ok(Some(metadata))
}

fn read_text(path: &Path) -> Result<Option<String>, EnvFileError> {
    let bytes = match read_bounded(path).map_err(read_error(path))? {
        BoundedRead::Missing => return Ok(None),
        BoundedRead::Unusable(Unreadable::NotRegularFile) => {
            return Err(EnvFileError::NotRegularFile {
                path: path.to_path_buf(),
            });
        }
        BoundedRead::Unusable(Unreadable::TooLarge) => {
            return Err(EnvFileError::TooLarge {
                path: path.to_path_buf(),
            });
        }
        BoundedRead::Bytes(bytes) => bytes,
    };
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| EnvFileError::NotUtf8 {
            path: path.to_path_buf(),
        })
}

/// Reads `.env` at `root` for a rewrite. Refuses a symlink, a file that is not
/// regular, a file over 1 MiB and a file that is not UTF-8.
pub fn read_env(root: &Path) -> Result<EnvContent, EnvFileError> {
    if inspect_env(root)?.is_none() {
        return Ok(EnvContent::Missing);
    }
    let path = root.join(ENV_FILE);
    Ok(read_text(&path)?.map_or(EnvContent::Missing, EnvContent::Text))
}

/// Replaces `.env` at `root` with `text`, keeping its permission bits (or a
/// private mode when it does not exist), through a temporary file and a rename.
pub fn write_env(root: &Path, text: &str) -> Result<(), EnvFileError> {
    let mode =
        inspect_env(root)?.map_or(NEW_FILE_MODE, |metadata| metadata.mode() & PERMISSION_BITS);
    replace_env(root, text, mode)
}

/// Creates `.env` at `root` from the file at `source`, without the devslot
/// block of the source, with the permission bits of the source. Refuses when
/// `.env` exists.
pub fn create_env(root: &Path, source: &Path) -> Result<(), EnvFileError> {
    let path = root.join(ENV_FILE);
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(EnvFileError::AlreadyExists { path });
    }
    let text = read_text(source)?.ok_or_else(|| EnvFileError::Read {
        path: source.to_path_buf(),
        source: io::ErrorKind::NotFound.into(),
    })?;
    let stripped = strip_block(&text).map_err(|error| EnvFileError::MalformedSourceBlock {
        path: source.to_path_buf(),
        source: error,
    })?;
    let mode = std::fs::metadata(source)
        .map_err(read_error(source))?
        .mode()
        & PERMISSION_BITS;
    replace_env(root, &stripped, mode)
}

/// The file `.env` is created from: the main checkout's `.env`, else the
/// `.env.example` of `root`, each only when it is a regular file.
pub fn env_source(root: &Path, main_root: Option<&Path>) -> Option<PathBuf> {
    main_root
        .map(|main| main.join(ENV_FILE))
        .into_iter()
        .chain(std::iter::once(root.join(ENV_EXAMPLE_FILE)))
        .find(|candidate| candidate.is_file())
}

/// Deletes the temporary files an interrupted write left in `root`.
pub fn remove_stale_temp_files(root: &Path) -> Result<(), EnvFileError> {
    let entries = std::fs::read_dir(root).map_err(read_error(root))?;
    for entry in entries.filter_map(Result::ok) {
        let is_temp = entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX);
        let is_folder = entry.file_type().is_ok_and(|kind| kind.is_dir());
        if is_temp && !is_folder {
            remove_ignoring_missing(&entry.path())?;
        }
    }
    Ok(())
}

fn remove_ignoring_missing(path: &Path) -> Result<(), EnvFileError> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(write_error(path)(error)),
        _ => Ok(()),
    }
}

fn random_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut hasher = Sha256::new();
    hasher.update(nanos.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(counter.to_le_bytes());
    let mut suffix = String::with_capacity(SUFFIX_BYTES * 2);
    for byte in hasher.finalize().iter().take(SUFFIX_BYTES) {
        let _ = write!(suffix, "{byte:02x}");
    }
    suffix
}

fn temp_path(root: &Path) -> PathBuf {
    root.join(format!(
        "{TEMP_PREFIX}{}-{}",
        std::process::id(),
        random_suffix()
    ))
}

fn replace_env(root: &Path, text: &str, mode: u32) -> Result<(), EnvFileError> {
    remove_stale_temp_files(root)?;
    let temp = temp_path(root);
    let outcome = write_temp(&temp, text, mode).and_then(|()| publish_temp(root, &temp));
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    outcome
}

fn write_temp(temp: &Path, text: &str, mode: u32) -> Result<(), EnvFileError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(temp)
        .map_err(write_error(temp))?;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
        .and_then(|()| file.write_all(text.as_bytes()))
        .and_then(|()| file.sync_all())
        .map_err(write_error(temp))
}

fn publish_temp(root: &Path, temp: &Path) -> Result<(), EnvFileError> {
    let target = root.join(ENV_FILE);
    std::fs::rename(temp, &target).map_err(write_error(&target))?;
    File::open(root)
        .and_then(|folder| folder.sync_all())
        .map_err(write_error(root))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::project::dotenv_block::{BLOCK_END, BLOCK_START};

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().mode() & PERMISSION_BITS
    }

    fn write_with_mode(path: &Path, text: &str, mode: u32) {
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn names_in(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn should_report_missing_when_there_is_no_env() {
        let root = tempfile::tempdir().unwrap();

        assert_eq!(read_env(root.path()).unwrap(), EnvContent::Missing);
    }

    #[test]
    fn should_read_the_text_of_the_env() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".env"), "A=1\n").unwrap();

        assert_eq!(
            read_env(root.path()).unwrap(),
            EnvContent::Text("A=1\n".to_owned())
        );
    }

    #[test]
    fn should_refuse_to_read_an_env_that_is_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("real"), "A=1\n").unwrap();
        symlink("real", root.path().join(".env")).unwrap();

        assert!(matches!(
            read_env(root.path()),
            Err(EnvFileError::Symlink { .. })
        ));
    }

    #[test]
    fn should_refuse_to_read_an_env_that_is_a_folder() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".env")).unwrap();

        assert!(matches!(
            read_env(root.path()),
            Err(EnvFileError::NotRegularFile { .. })
        ));
    }

    #[test]
    fn should_refuse_to_read_an_env_larger_than_1_mib() {
        let root = tempfile::tempdir().unwrap();
        let size = usize::try_from(MAX_FILE_BYTES).unwrap() + 1;
        std::fs::write(root.path().join(".env"), vec![b'a'; size]).unwrap();

        assert!(matches!(
            read_env(root.path()),
            Err(EnvFileError::TooLarge { .. })
        ));
    }

    #[test]
    fn should_refuse_to_read_an_env_that_is_not_utf8() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".env"), [0xff, 0xfe]).unwrap();

        assert!(matches!(
            read_env(root.path()),
            Err(EnvFileError::NotUtf8 { .. })
        ));
    }

    #[test]
    fn should_replace_the_text_and_keep_the_permissions_when_the_env_exists() {
        let root = tempfile::tempdir().unwrap();
        let env = root.path().join(".env");
        write_with_mode(&env, "A=1\n", 0o640);

        write_env(root.path(), "B=2\n").unwrap();

        assert_eq!(
            (std::fs::read_to_string(&env).unwrap(), mode_of(&env)),
            ("B=2\n".to_owned(), 0o640)
        );
    }

    #[test]
    fn should_create_a_private_env_when_writing_without_an_existing_one() {
        let root = tempfile::tempdir().unwrap();

        write_env(root.path(), "B=2\n").unwrap();

        assert_eq!(mode_of(&root.path().join(".env")), 0o600);
    }

    #[test]
    fn should_leave_only_the_env_and_the_other_files_after_a_write() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("other"), "x").unwrap();
        std::fs::write(root.path().join(".env"), "A=1\n").unwrap();

        write_env(root.path(), "B=2\n").unwrap();

        assert_eq!(
            (
                names_in(root.path()),
                std::fs::read_to_string(root.path().join("other")).unwrap()
            ),
            (vec![".env".to_owned(), "other".to_owned()], "x".to_owned())
        );
    }

    #[test]
    fn should_refuse_to_write_through_a_symlink_and_leave_its_target_untouched() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        std::fs::write(&target, "KEEP=1\n").unwrap();
        symlink(&target, root.path().join(".env")).unwrap();

        let outcome = write_env(root.path(), "B=2\n");

        assert!(matches!(outcome, Err(EnvFileError::Symlink { .. })));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "KEEP=1\n");
    }

    #[test]
    fn should_remove_the_stale_temp_files_of_the_pattern_and_nothing_else() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".env.devslot-1-aa"), "SECRET=1\n").unwrap();
        std::fs::write(root.path().join(".env.devslot-2-bb"), "SECRET=2\n").unwrap();
        std::fs::write(root.path().join(".env.example"), "A=1\n").unwrap();

        remove_stale_temp_files(root.path()).unwrap();

        assert_eq!(names_in(root.path()), vec![".env.example".to_owned()]);
    }

    #[test]
    fn should_remove_stale_temp_files_when_writing() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".env.devslot-1-aa"), "SECRET=1\n").unwrap();

        write_env(root.path(), "B=2\n").unwrap();

        assert_eq!(names_in(root.path()), vec![".env".to_owned()]);
    }

    #[test]
    fn should_copy_the_permissions_of_the_source_when_creating_the_env() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.env");
        write_with_mode(&source, "A=1\n", 0o640);

        create_env(root.path(), &source).unwrap();

        assert_eq!(mode_of(&root.path().join(".env")), 0o640);
    }

    #[test]
    fn should_strip_the_devslot_block_of_the_source_when_creating_the_env() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.env");
        std::fs::write(
            &source,
            format!("A=1\n{BLOCK_START}\nPORT=20050\n{BLOCK_END}\nB=2\n"),
        )
        .unwrap();

        create_env(root.path(), &source).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.path().join(".env")).unwrap(),
            "A=1\nB=2\n"
        );
    }

    #[test]
    fn should_refuse_to_create_the_env_when_the_source_block_is_malformed() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.env");
        std::fs::write(&source, format!("{BLOCK_START}\nPORT=1\n")).unwrap();

        assert!(matches!(
            create_env(root.path(), &source),
            Err(EnvFileError::MalformedSourceBlock { .. })
        ));
        assert!(!root.path().join(".env").exists());
    }

    #[test]
    fn should_refuse_to_create_the_env_when_it_already_exists() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.env");
        std::fs::write(&source, "A=1\n").unwrap();
        std::fs::write(root.path().join(".env"), "KEEP=1\n").unwrap();

        assert!(matches!(
            create_env(root.path(), &source),
            Err(EnvFileError::AlreadyExists { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(root.path().join(".env")).unwrap(),
            "KEEP=1\n"
        );
    }

    #[test]
    fn should_prefer_the_main_env_over_the_example_when_choosing_the_source() {
        let root = tempfile::tempdir().unwrap();
        let main = tempfile::tempdir().unwrap();
        std::fs::write(main.path().join(".env"), "A=1\n").unwrap();
        std::fs::write(root.path().join(".env.example"), "B=2\n").unwrap();

        assert_eq!(
            env_source(root.path(), Some(main.path())),
            Some(main.path().join(".env"))
        );
    }

    #[test]
    fn should_fall_back_to_the_example_when_the_main_checkout_has_no_env() {
        let root = tempfile::tempdir().unwrap();
        let main = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".env.example"), "B=2\n").unwrap();

        assert_eq!(
            env_source(root.path(), Some(main.path())),
            Some(root.path().join(".env.example"))
        );
    }

    #[test]
    fn should_have_no_source_when_neither_file_exists() {
        let root = tempfile::tempdir().unwrap();

        assert_eq!(env_source(root.path(), None), None);
    }

    #[test]
    fn should_skip_a_main_env_that_is_not_a_regular_file() {
        let root = tempfile::tempdir().unwrap();
        let main = tempfile::tempdir().unwrap();
        std::fs::create_dir(main.path().join(".env")).unwrap();
        std::fs::write(root.path().join(".env.example"), "B=2\n").unwrap();

        assert_eq!(
            env_source(root.path(), Some(main.path())),
            Some(root.path().join(".env.example"))
        );
    }
}
