//! Reading the `/proc` tree. The root is a parameter so tests can run on a
//! fake tree written in a temporary folder. Nothing here prints or keeps the
//! command line or the environment of a process.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

const SYSTEM_PROC_ROOT: &str = "/proc";
const DELETED_SUFFIX: &[u8] = b" (deleted)";
const SOCKET_LINK_PREFIX: &[u8] = b"socket:[";
const SOCKET_LINK_SUFFIX: u8 = b']';
const LISTEN_STATE: &str = "0A";
const TCP_TABLES: [&str; 2] = ["net/tcp", "net/tcp6"];
const LOCAL_ADDRESS_COLUMN: usize = 1;
const STATE_COLUMN: usize = 3;
const INODE_COLUMN: usize = 9;
const UID_PREFIX: &str = "Uid:";
const DEAD_STATES: [char; 2] = ['Z', 'X'];

/// What `/proc/<pid>/stat` says about the family of a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessStat {
    /// The state letter: `R`, `S`, `Z` for a zombie...
    pub state: char,
    /// The parent process.
    pub parent: i32,
    /// The process group.
    pub group: i32,
}

impl ProcessStat {
    /// Whether the process is a zombie or dead: it holds nothing any more.
    pub fn is_dead(&self) -> bool {
        DEAD_STATES.contains(&self.state)
    }
}

/// A `/proc` folder: the real one, or a fake tree in tests.
#[derive(Debug, Clone)]
pub struct ProcRoot {
    path: PathBuf,
}

impl ProcRoot {
    /// The tree at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The real `/proc`.
    pub fn system() -> Self {
        Self::new(SYSTEM_PROC_ROOT)
    }

    /// The numeric folders: every process the caller may list.
    pub fn process_ids(&self) -> Vec<i32> {
        let Ok(entries) = std::fs::read_dir(&self.path) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
            .collect()
    }

    /// The state, parent and group of `pid`; `None` when it is gone.
    pub fn stat(&self, pid: i32) -> Option<ProcessStat> {
        let text = std::fs::read_to_string(self.process_file(pid, "stat")).ok()?;
        parse_stat(&text)
    }

    /// The kernel name of `pid` (`/proc/<pid>/comm`), control characters
    /// replaced. Never the command line.
    pub fn comm(&self, pid: i32) -> String {
        let text = std::fs::read_to_string(self.process_file(pid, "comm")).unwrap_or_default();
        text.trim_end_matches('\n')
            .chars()
            .map(|character| {
                if character.is_control() {
                    '?'
                } else {
                    character
                }
            })
            .collect()
    }

    /// The working directory of `pid`, canonical when it still exists, with
    /// the ` (deleted)` mark of the kernel removed. `None` when unreadable.
    pub fn working_directory(&self, pid: i32) -> Option<PathBuf> {
        let target = std::fs::read_link(self.process_file(pid, "cwd")).ok()?;
        let bytes = target.as_os_str().as_bytes();
        let stripped = bytes.strip_suffix(DELETED_SUFFIX).unwrap_or(bytes);
        let path = PathBuf::from(OsStr::from_bytes(stripped));
        Some(path.canonicalize().unwrap_or(path))
    }

    /// The real uid of `pid`, from its `status` file.
    pub fn real_uid(&self, pid: i32) -> Option<u32> {
        let text = std::fs::read_to_string(self.process_file(pid, "status")).ok()?;
        text.lines()
            .find_map(|line| line.strip_prefix(UID_PREFIX))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }

    /// Whether the environment of `pid` holds exactly `entry` (`NAME=value`).
    /// Looks for the marker only: the rest of the environment is dropped.
    pub fn has_environment_entry(&self, pid: i32, entry: &str) -> bool {
        let Ok(bytes) = std::fs::read(self.process_file(pid, "environ")) else {
            return false;
        };
        bytes
            .split(|byte| *byte == 0)
            .any(|candidate| candidate == entry.as_bytes())
    }

    /// The socket inodes that listen on TCP on one of `ports`, IPv4 or IPv6.
    pub fn listening_inodes(&self, ports: &[u16]) -> HashSet<u64> {
        TCP_TABLES
            .iter()
            .filter_map(|table| std::fs::read_to_string(self.path.join(table)).ok())
            .flat_map(|text| parse_listening_inodes(&text, ports))
            .collect()
    }

    /// Whether `pid` holds one of the sockets `inodes`.
    pub fn holds_socket(&self, pid: i32, inodes: &HashSet<u64>) -> bool {
        if inodes.is_empty() {
            return false;
        }
        let Ok(entries) = std::fs::read_dir(self.process_file(pid, "fd")) else {
            return false;
        };
        entries
            .flatten()
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .filter_map(|link| socket_inode(link.as_os_str().as_bytes()))
            .any(|inode| inodes.contains(&inode))
    }

    fn process_file(&self, pid: i32, name: &str) -> PathBuf {
        self.path.join(pid.to_string()).join(name)
    }
}

fn parse_stat(text: &str) -> Option<ProcessStat> {
    let after_name = &text[text.rfind(')')? + 1..];
    let mut fields = after_name.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let parent = fields.next()?.parse().ok()?;
    let group = fields.next()?.parse().ok()?;
    Some(ProcessStat {
        state,
        parent,
        group,
    })
}

fn socket_inode(link: &[u8]) -> Option<u64> {
    let inner = link
        .strip_prefix(SOCKET_LINK_PREFIX)?
        .strip_suffix(&[SOCKET_LINK_SUFFIX])?;
    std::str::from_utf8(inner).ok()?.parse().ok()
}

fn parse_listening_inodes(text: &str, ports: &[u16]) -> Vec<u64> {
    text.lines()
        .skip(1)
        .filter_map(|line| listening_inode(line, ports))
        .collect()
}

fn listening_inode(line: &str, ports: &[u16]) -> Option<u64> {
    let columns: Vec<&str> = line.split_whitespace().collect();
    if *columns.get(STATE_COLUMN)? != LISTEN_STATE {
        return None;
    }
    let (_, port_hex) = columns.get(LOCAL_ADDRESS_COLUMN)?.rsplit_once(':')?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    if !ports.contains(&port) {
        return None;
    }
    columns.get(INODE_COLUMN)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TCP_HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode";

    fn line(local: &str, state: &str, inode: u64) -> String {
        format!(
            "   0: {local} 00000000:0000 {state} 00000000:00000000 00:00000000 00000000  1000        0 {inode} 1 0000000000000000 100 0 0 10 0"
        )
    }

    #[test]
    fn should_read_the_state_parent_and_group_when_the_name_holds_parentheses() {
        let stat = parse_stat("42 (my (odd) name) S 7 9 9 0 -1").unwrap();

        assert_eq!(
            stat,
            ProcessStat {
                state: 'S',
                parent: 7,
                group: 9
            }
        );
    }

    #[test]
    fn should_reject_a_stat_line_that_is_cut_short() {
        assert_eq!(parse_stat("42 (x) S 7"), None);
    }

    #[test]
    fn should_treat_zombies_and_dead_processes_as_dead() {
        let alive = ProcessStat {
            state: 'S',
            parent: 1,
            group: 1,
        };

        assert!(!alive.is_dead());
        assert!(
            ProcessStat {
                state: 'Z',
                ..alive
            }
            .is_dead()
        );
        assert!(
            ProcessStat {
                state: 'X',
                ..alive
            }
            .is_dead()
        );
    }

    #[test]
    fn should_keep_only_the_listening_sockets_of_the_wanted_ports() {
        let text = [
            TCP_HEADER.to_owned(),
            line("0100007F:4E20", "0A", 111),
            line("0100007F:4E20", "01", 222),
            line("0100007F:4E21", "0A", 333),
            line("00000000:4E22", "0A", 444),
        ]
        .join("\n");

        let inodes = parse_listening_inodes(&text, &[0x4E20, 0x4E22]);

        assert_eq!(inodes, vec![111, 444]);
    }

    #[test]
    fn should_read_the_port_of_an_ipv6_listener() {
        let text = [
            TCP_HEADER.to_owned(),
            line("00000000000000000000000001000000:4E20", "0A", 555),
        ]
        .join("\n");

        assert_eq!(parse_listening_inodes(&text, &[0x4E20]), vec![555]);
    }

    #[test]
    fn should_read_the_inode_of_a_socket_link_only() {
        assert_eq!(socket_inode(b"socket:[1234]"), Some(1234));
        assert_eq!(socket_inode(b"pipe:[1234]"), None);
        assert_eq!(socket_inode(b"socket:[12a]"), None);
    }
}
