//! Reading a project file without trusting it: only regular files up to a
//! fixed size, so a FIFO cannot hang the tool and `/dev/zero` cannot fill the
//! memory.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// The largest file devslot reads: 1 MiB.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Why a file is not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unreadable {
    /// Not a regular file once symlinks are followed: a FIFO, a device, a folder.
    NotRegularFile,
    /// Larger than [`MAX_FILE_BYTES`].
    TooLarge,
}

/// What reading a file gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundedRead {
    /// There is no such file.
    Missing,
    /// The file exists but is not read.
    Unusable(Unreadable),
    /// The bytes of the file.
    Bytes(Vec<u8>),
}

/// Reads `path` when it is a regular file of at most [`MAX_FILE_BYTES`].
pub fn read_bounded(path: &Path) -> io::Result<BoundedRead> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BoundedRead::Missing),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Ok(BoundedRead::Unusable(Unreadable::NotRegularFile));
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Ok(BoundedRead::Unusable(Unreadable::TooLarge));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Ok(BoundedRead::Unusable(Unreadable::TooLarge));
    }
    Ok(BoundedRead::Bytes(bytes))
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn should_report_missing_when_the_file_does_not_exist() {
        let directory = tempfile::tempdir().unwrap();

        assert_eq!(
            read_bounded(&directory.path().join("nope")).unwrap(),
            BoundedRead::Missing
        );
    }

    #[test]
    fn should_return_the_bytes_when_the_file_is_small() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("f"), "A=1").unwrap();

        assert_eq!(
            read_bounded(&directory.path().join("f")).unwrap(),
            BoundedRead::Bytes(b"A=1".to_vec())
        );
    }

    #[test]
    fn should_refuse_a_file_larger_than_the_limit() {
        let directory = tempfile::tempdir().unwrap();
        let size = usize::try_from(MAX_FILE_BYTES).unwrap() + 1;
        std::fs::write(directory.path().join("f"), vec![b'a'; size]).unwrap();

        assert_eq!(
            read_bounded(&directory.path().join("f")).unwrap(),
            BoundedRead::Unusable(Unreadable::TooLarge)
        );
    }

    #[test]
    fn should_refuse_a_fifo_without_blocking() {
        let directory = tempfile::tempdir().unwrap();
        let fifo = directory.path().join("f");
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );

        assert_eq!(
            read_bounded(&fifo).unwrap(),
            BoundedRead::Unusable(Unreadable::NotRegularFile)
        );
    }

    #[test]
    fn should_refuse_a_symlink_to_a_device() {
        let directory = tempfile::tempdir().unwrap();
        let link = directory.path().join("f");
        std::os::unix::fs::symlink("/dev/zero", &link).unwrap();

        assert_eq!(
            read_bounded(&link).unwrap(),
            BoundedRead::Unusable(Unreadable::NotRegularFile)
        );
    }
}
