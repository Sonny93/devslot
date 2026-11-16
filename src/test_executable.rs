//! Writing a script a unit test then runs, without the test process ever
//! holding a write descriptor on it. A descriptor open for writing in this
//! process can leak into a child forked by another test thread, and executing
//! the file while any process holds one fails with ETXTBSY: `install` writes
//! it in its own process instead.

use std::io::Write;
use std::path::Path;
use std::process::Command;

/// Installs `script` as an executable file at `path`.
pub fn install_executable(path: &Path, script: &str) {
    let mut source = tempfile::NamedTempFile::new().unwrap();
    source.write_all(script.as_bytes()).unwrap();
    source.flush().unwrap();
    let status = Command::new("install")
        .args(["-m", "755"])
        .arg(source.path())
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}
