//! A fake `/proc` tree written in a temporary folder, for the unit tests of
//! the scan and of the stop loop.

use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use super::proc_root::ProcRoot;

const TCP_HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
const OWN_UID: u32 = 1000;
/// The start time every fake process has until a test sets another one.
pub const DEFAULT_START_TIME: u64 = 1_000;
const START_TIME_TOKEN: usize = 19;

/// A `/proc` tree and a folder for the working directories of its processes.
pub struct FakeProc {
    proc: tempfile::TempDir,
    folders: tempfile::TempDir,
}

impl FakeProc {
    /// An empty tree.
    pub fn new() -> Self {
        let proc = tempfile::tempdir().unwrap();
        fs::create_dir_all(proc.path().join("net")).unwrap();
        Self {
            proc,
            folders: tempfile::tempdir().unwrap(),
        }
    }

    /// The tree, as the code under test reads it.
    pub fn root(&self) -> ProcRoot {
        ProcRoot::new(self.proc.path())
    }

    /// The canonical folder `relative` below the working directories, created;
    /// `/` stays `/`.
    pub fn directory(&self, relative: &str) -> PathBuf {
        if relative == "/" {
            return PathBuf::from("/");
        }
        let path = self.folders.path().join(relative);
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    /// A process with the working directory `cwd`, running as the test user.
    pub fn add_process(&self, pid: i32, parent: i32, group: i32, comm: &str, cwd: &Path) {
        self.add_process_with_link(pid, parent, group, comm, &cwd.display().to_string());
    }

    /// Like [`Self::add_process`], with the raw text of the `cwd` link.
    pub fn add_process_with_link(&self, pid: i32, parent: i32, group: i32, comm: &str, link: &str) {
        let folder = self.process_folder(pid);
        fs::create_dir_all(folder.join("fd")).unwrap();
        fs::write(folder.join("comm"), format!("{comm}\n")).unwrap();
        symlink(link, folder.join("cwd")).unwrap();
        fs::write(
            folder.join("stat"),
            format!(
                "{pid} ({comm}) S {parent} {group} {group} 0 -1 4194304 0 0 0 0 0 0 0 0 20 0 1 0 {DEFAULT_START_TIME} 0 0"
            ),
        )
        .unwrap();
        self.set_uid(pid, OWN_UID);
    }

    /// Replaces the environment of `pid` with `entries`.
    pub fn set_environment(&self, pid: i32, entries: &[&str]) {
        let mut bytes = Vec::new();
        for entry in entries {
            bytes.extend_from_slice(entry.as_bytes());
            bytes.push(0);
        }
        fs::write(self.process_folder(pid).join("environ"), bytes).unwrap();
    }

    /// Sets the real uid of `pid`.
    pub fn set_uid(&self, pid: i32, uid: u32) {
        fs::write(
            self.process_folder(pid).join("status"),
            format!("Name:\tx\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
        )
        .unwrap();
    }

    /// Sets the state letter of `pid`.
    pub fn set_state(&self, pid: i32, state: char) {
        let path = self.process_folder(pid).join("stat");
        let text = fs::read_to_string(&path).unwrap();
        let (head, tail) = text.split_once(") ").unwrap();
        let rest = &tail[1..];
        fs::write(path, format!("{head}) {state}{rest}")).unwrap();
    }

    /// Sets the start time of `pid`, as a process that reused the pid would have.
    pub fn set_start_time(&self, pid: i32, start_time: u64) {
        let path = self.process_folder(pid).join("stat");
        let text = fs::read_to_string(&path).unwrap();
        let (head, tail) = text.split_once(") ").unwrap();
        let mut tokens: Vec<String> = tail.split_whitespace().map(str::to_owned).collect();
        tokens[START_TIME_TOKEN] = start_time.to_string();
        fs::write(path, format!("{head}) {}", tokens.join(" "))).unwrap();
    }

    /// Makes `pid` hold the socket `inode`.
    pub fn add_socket(&self, pid: i32, inode: u64) {
        let link = self.process_folder(pid).join("fd").join(inode.to_string());
        symlink(format!("socket:[{inode}]"), link).unwrap();
    }

    /// Adds a TCP socket on `port` in `state` (`0A` listens) to `net/tcp`.
    pub fn add_listener(&self, port: u16, inode: u64, state: &str) {
        let path = self.proc.path().join("net/tcp");
        let mut text = fs::read_to_string(&path).unwrap_or_else(|_| TCP_HEADER.to_owned());
        writeln!(
            text,
            "   0: 0100007F:{port:04X} 00000000:0000 {state} 00000000:00000000 00:00000000 00000000  1000        0 {inode} 1 0000000000000000 100 0 0 10 0"
        )
        .unwrap();
        fs::write(path, text).unwrap();
    }

    /// Removes `pid` from the tree, as if it had exited.
    pub fn remove_process(&self, pid: i32) {
        fs::remove_dir_all(self.process_folder(pid)).unwrap();
    }

    fn process_folder(&self, pid: i32) -> PathBuf {
        self.proc.path().join(pid.to_string())
    }
}
