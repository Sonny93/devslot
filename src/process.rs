//! Starting external programs: a spawn that survives a busy executable, and a
//! captured run that is killed, process group included, when it hangs.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const SPAWN_ATTEMPTS: u32 = 5;
const SPAWN_RETRY_DELAY: Duration = Duration::from_millis(20);
const FIRST_POLL_DELAY: Duration = Duration::from_micros(500);
const LONGEST_POLL_DELAY: Duration = Duration::from_millis(10);

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
/// stdout and stderr. Kills the group after `timeout`.
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
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                let next = next_poll_delay(delay);
                thread::sleep(next);
                delay = Some(next);
            }
            Ok(None) => {
                kill_process_group(&child);
                let _ = child.wait();
                return Err(CaptureFailure::TimedOut);
            }
            Err(error) => return Err(CaptureFailure::Io(error)),
        }
    };
    Ok(CapturedOutput {
        status,
        stdout: stdout_reader.join().unwrap_or_default(),
        stderr: stderr_reader.join().unwrap_or_default(),
    })
}

fn read_in_thread<R: Read + Send + 'static>(stream: Option<R>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut stream) = stream {
            let _ = stream.read_to_end(&mut bytes);
        }
        bytes
    })
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
    fn should_report_an_io_error_when_the_program_does_not_exist() {
        let command = Command::new("/does/not/exist");

        let outcome = capture_with_timeout(command, Duration::from_secs(1));

        assert!(matches!(outcome, Err(CaptureFailure::Io(_))));
    }
}
