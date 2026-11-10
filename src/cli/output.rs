//! Writing to the standard streams.

use std::io::Write;

use crate::error::ToolError;

/// Writes `text` to stdout; a closed pipe is not an error.
pub fn write_stdout(text: &str) -> Result<(), ToolError> {
    match std::io::stdout().write_all(text.as_bytes()) {
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(ToolError::WriteReport(error)),
        Ok(()) => Ok(()),
    }
}
