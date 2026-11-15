//! What every hook reads first: one JSON object on stdin, bounded in size and
//! in time, so a writer that never closes the pipe cannot stall the hook.

use std::io::{self, Read};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::de::DeserializeOwned;
use thiserror::Error;

/// The most a hook reads from stdin.
pub const MAX_INPUT_BYTES: u64 = 1024 * 1024;

/// How long a hook waits for its input to end.
pub const INPUT_TIMEOUT: Duration = Duration::from_secs(2);

/// Why the input of a hook cannot be used.
#[derive(Debug, Error)]
pub enum InputError {
    /// Stdin cannot be read.
    #[error("cannot read the hook input: {0}")]
    Read(io::Error),
    /// Stdin holds more than [`MAX_INPUT_BYTES`].
    #[error("the hook input is larger than 1 MiB")]
    TooLarge,
    /// Stdin did not end in time.
    #[error("the hook input did not end within 2 seconds")]
    TimedOut,
    /// Stdin is not a JSON object with the fields the event needs.
    #[error("the hook input is not valid: {0}")]
    Invalid(serde_json::Error),
}

/// Reads at most [`MAX_INPUT_BYTES`] from `reader` within [`INPUT_TIMEOUT`]
/// and parses them as `T`.
pub fn read_input<T: DeserializeOwned>(
    reader: impl Read + Send + 'static,
) -> Result<T, InputError> {
    read_input_within(reader, INPUT_TIMEOUT)
}

/// Like [`read_input`], with an explicit `timeout`. The read runs on its own
/// thread, which is left behind when the time is up.
pub fn read_input_within<T: DeserializeOwned>(
    reader: impl Read + Send + 'static,
    timeout: Duration,
) -> Result<T, InputError> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(read_bounded_bytes(reader));
    });
    let bytes = receiver
        .recv_timeout(timeout)
        .map_err(|_| InputError::TimedOut)??;
    serde_json::from_slice(&bytes).map_err(InputError::Invalid)
}

fn read_bounded_bytes(reader: impl Read) -> Result<Vec<u8>, InputError> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(InputError::Read)?;
    if u64::try_from(bytes.len()).is_ok_and(|length| length > MAX_INPUT_BYTES) {
        return Err(InputError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize)]
    struct Sample {
        cwd: String,
    }

    #[test]
    fn should_parse_the_fields_when_the_input_is_valid() {
        let sample: Sample = read_input(Cursor::new(
            r#"{"cwd":"/work","other":1}"#.as_bytes().to_vec(),
        ))
        .unwrap();

        assert_eq!(sample.cwd, "/work");
    }

    #[test]
    fn should_fail_when_the_input_is_not_json() {
        let result = read_input::<Sample>(Cursor::new(b"not json".to_vec()));

        assert!(matches!(result, Err(InputError::Invalid(_))));
    }

    #[test]
    fn should_fail_when_a_field_is_missing() {
        let result = read_input::<Sample>(Cursor::new(b"{}".to_vec()));

        assert!(matches!(result, Err(InputError::Invalid(_))));
    }

    #[test]
    fn should_fail_when_the_input_is_larger_than_the_limit() {
        let padding = " ".repeat(usize::try_from(MAX_INPUT_BYTES).unwrap());
        let text = format!("{{\"cwd\":\"/work\"}}{padding}");

        let result = read_input::<Sample>(Cursor::new(text.into_bytes()));

        assert!(matches!(result, Err(InputError::TooLarge)));
    }

    struct NeverEnds;

    impl Read for NeverEnds {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            thread::sleep(Duration::from_secs(5));
            Ok(0)
        }
    }

    #[test]
    fn should_time_out_when_the_writer_never_closes_the_input() {
        let result = read_input_within::<Sample>(NeverEnds, Duration::from_millis(50));

        assert!(matches!(result, Err(InputError::TimedOut)));
    }
}
