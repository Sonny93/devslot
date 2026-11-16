//! Starting external programs: a spawn that survives a busy executable, and a
//! captured run that is killed, process group included, when it hangs.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use rustix::process::{WaitId, WaitIdOptions};

const SPAWN_ATTEMPTS: u32 = 5;
const SPAWN_RETRY_DELAY: Duration = Duration::from_millis(20);
const FIRST_POLL_DELAY: Duration = Duration::from_micros(500);
const LONGEST_POLL_DELAY: Duration = Duration::from_millis(10);
const READER_GRACE_AFTER_EXIT: Duration = Duration::from_secs(2);
const READ_CHUNK_BYTES: usize = 8192;
const READER_GRACE_AFTER_KILL: Duration = Duration::from_secs(1);

/// Spawns `command`, trying again a few times when the executable is busy
/// (a script that was just written is still open for writing in another thread).
pub fn spawn_with_retry(command: &mut Command) -> io::Result<Child> {
    let mut attempt = 1;
    loop {
        match command.spawn() {
            Err(error)
                if error.kind() == io::ErrorKind::ExecutableFileBusy
                    && attempt < SPAWN_ATTEMPTS =>
            {
                attempt += 1;
                thread::sleep(SPAWN_RETRY_DELAY);
            }
            outcome => return outcome,
        }
    }
}

/// The delay before the next check on a running program: it doubles from half
/// a millisecond up to ten, so a quick program is noticed quickly and a slow
/// one is not polled hard.
pub fn next_poll_delay(previous: Option<Duration>) -> Duration {
    previous.map_or(FIRST_POLL_DELAY, |delay| {
        (delay * 2).min(LONGEST_POLL_DELAY)
    })
}

/// Kills the whole process group of `child`.
pub fn kill_process_group(child: &Child) {
    let group = rustix::process::Pid::from_child(child);
    let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
}

/// What a finished program printed.
#[derive(Debug)]
pub struct CapturedOutput {
    /// How it ended.
    pub status: ExitStatus,
    /// Everything on stdout.
    pub stdout: Vec<u8>,
    /// Everything on stderr.
    pub stderr: Vec<u8>,
}

/// Why a captured run gave nothing.
#[derive(Debug)]
pub enum CaptureFailure {
    /// The program could not be started or waited for.
    Io(io::Error),
    /// It did not finish in time and was killed.
    TimedOut,
}

/// Runs `command` in its own process group with stdin closed, and captures
/// stdout and stderr. Kills the group after `timeout`. When the program exits but a descendant
/// keeps the pipes open, the output read so far is returned after a short grace.
pub fn capture_with_timeout(
    mut command: Command,
    timeout: Duration,
) -> Result<CapturedOutput, CaptureFailure> {
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn_with_retry(&mut command).map_err(CaptureFailure::Io)?;
    let stdout_reader = read_in_thread(child.stdout.take());
    let stderr_reader = read_in_thread(child.stderr.take());
    let started = Instant::now();
    let mut delay = None;
    loop {
        match has_exited(&child) {
            Ok(true) => break,
            Ok(false) if started.elapsed() < timeout => {
                let next = next_poll_delay(delay);
                thread::sleep(next);
                delay = Some(next);
            }
            Ok(false) => {
                kill_process_group(&child);
                let _ = child.wait();
                return Err(CaptureFailure::TimedOut);
            }
            Err(error) => return Err(CaptureFailure::Io(error)),
        }
    }
    let deadline = started + timeout;
    let readers = [stdout_reader, stderr_reader];
    let grace = READER_GRACE_AFTER_EXIT.min(deadline.saturating_duration_since(Instant::now()));
    if !wait_for_readers(&readers, grace) {
        kill_process_group(&child);
        wait_for_readers(&readers, READER_GRACE_AFTER_KILL);
    }
    let status = child.wait().map_err(CaptureFailure::Io)?;
    let [stdout_reader, stderr_reader] = readers;
    Ok(CapturedOutput {
        status,
        stdout: stdout_reader.snapshot(),
        stderr: stderr_reader.snapshot(),
    })
}

/// Whether `child` has exited, without reaping it: the zombie keeps its pid,
/// and so its process group id, from being reused until the group is killed.
fn has_exited(child: &Child) -> io::Result<bool> {
    let pid = rustix::process::Pid::from_child(child);
    let options = WaitIdOptions::NOHANG | WaitIdOptions::EXITED | WaitIdOptions::NOWAIT;
    Ok(rustix::process::waitid(WaitId::Pid(pid), options)?.is_some())
}

/// A thread that appends what a pipe delivers to a shared buffer, so the
/// bytes read so far stay available when the pipe never reaches its end.
struct PipeReader {
    buffer: Arc<Mutex<Vec<u8>>>,
    finished: Receiver<()>,
}

impl PipeReader {
    fn snapshot(&self) -> Vec<u8> {
        self.buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Waits until both readers reached the end of their pipe, or `wait` passed.
fn wait_for_readers(readers: &[PipeReader; 2], wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    readers.iter().all(|reader| {
        let remaining = deadline.saturating_duration_since(Instant::now());
        reader.finished.recv_timeout(remaining) != Err(RecvTimeoutError::Timeout)
    })
}

fn read_in_thread<R: Read + Send + 'static>(stream: Option<R>) -> PipeReader {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let (sender, finished) = mpsc::channel();
    let shared = Arc::clone(&buffer);
    thread::spawn(move || {
        if let Some(mut stream) = stream {
            let mut chunk = [0_u8; READ_CHUNK_BYTES];
            while let Ok(count) = stream.read(&mut chunk) {
                if count == 0 {
                    break;
                }
                shared
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend_from_slice(&chunk[..count]);
            }
        }
        let _ = sender.send(());
    });
    PipeReader { buffer, finished }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_double_the_poll_delay_up_to_ten_milliseconds() {
        let first = next_poll_delay(None);
        let second = next_poll_delay(Some(first));

        assert_eq!(first, Duration::from_micros(500));
        assert_eq!(second, Duration::from_millis(1));
        assert_eq!(
            next_poll_delay(Some(Duration::from_millis(8))),
            Duration::from_millis(10)
        );
        assert_eq!(
            next_poll_delay(Some(Duration::from_millis(10))),
            Duration::from_millis(10)
        );
    }

    #[test]
    fn should_capture_stdout_and_stderr_and_the_status() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo out; echo err >&2; exit 3"]);

        let output = capture_with_timeout(command, Duration::from_secs(5)).unwrap();

        assert_eq!(output.stdout, b"out\n");
        assert_eq!(output.stderr, b"err\n");
        assert_eq!(output.status.code(), Some(3));
    }

    #[test]
    fn should_kill_the_program_when_it_outlives_the_timeout() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        let started = Instant::now();

        let outcome = capture_with_timeout(command, Duration::from_millis(100));

        assert!(matches!(outcome, Err(CaptureFailure::TimedOut)));
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn should_return_the_output_when_a_background_process_keeps_the_pipes_open() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo done; sleep 30 &"]);
        let started = Instant::now();

        let output = capture_with_timeout(command, Duration::from_secs(10)).unwrap();

        assert!(output.status.success());
        assert_eq!(output.stdout, b"done\n");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn should_report_an_io_error_when_the_program_does_not_exist() {
        let command = Command::new("/does/not/exist");

        let outcome = capture_with_timeout(command, Duration::from_secs(1));

        assert!(matches!(outcome, Err(CaptureFailure::Io(_))));
    }
}
