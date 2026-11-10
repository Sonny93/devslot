//! The machine-wide registry of instances: a TOML file edited in place, so
//! hand-written comments and unknown keys survive, and written atomically
//! under a lock.

pub mod document;
pub mod lock;
pub mod record;
pub mod store;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

/// Why the registry cannot be read or written. These are tool failures.
#[derive(Debug, Error)]
pub enum RegistryError {
    /// The file exists but cannot be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The file.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// The file is not a regular file, is larger than 1 MiB or is not UTF-8.
    #[error("cannot read {path}: {reason}")]
    Unusable {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// The file is not valid TOML.
    #[error("cannot parse {path}: {message}")]
    Parse {
        /// The file.
        path: PathBuf,
        /// What the parser reports.
        message: String,
    },
    /// The file is valid TOML that devslot does not fully understand. It is
    /// never rewritten.
    #[error("{path} is malformed ({entry}: {reason}); ds will not rewrite it, fix it by hand")]
    Malformed {
        /// The file.
        path: PathBuf,
        /// The key or `[[instance]]` entry at fault.
        entry: String,
        /// What is wrong with it.
        reason: String,
    },
    /// The checkout path cannot be written in TOML.
    #[error("cannot register {path}: the path is not valid UTF-8")]
    PathNotUtf8 {
        /// The checkout.
        path: PathBuf,
    },
    /// The registry could not be written.
    #[error("cannot write {path}: {source}")]
    Write {
        /// The file or folder.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// The lock file cannot be opened or locked.
    #[error("cannot lock {path}: {source}")]
    Lock {
        /// The lock file.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// Another `ds` process kept the lock for the whole timeout.
    #[error("the registry is locked by another ds process (gave up after {waited:?})")]
    LockTimeout {
        /// How long ds waited.
        waited: Duration,
    },
}
