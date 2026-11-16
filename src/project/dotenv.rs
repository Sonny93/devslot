//! Reading a `.env` file: syntax validation by `dotenvy`, raw values by hand.
//!
//! `dotenvy` 0.15 substitutes `${VAR}` and `$VAR` using the process
//! environment first, so its values depend on the caller's shell. The values
//! this module hands out are therefore the raw text of the file. A value is
//! never put in an error or a message: it can hold credentials.

use std::io;
use std::path::Path;

use thiserror::Error;

use super::bounded_read::{BoundedRead, Unreadable, read_bounded};
use super::dotenv_block::{BlockError, split_block};

const EXPORT_PREFIX: &str = "export ";
const COMMENT_MARKER: &str = " #";

/// Why a `.env` file cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum EnvFileError {
    /// A line is not valid dotenv syntax. The line is never kept.
    #[error("line {line} is not valid dotenv syntax")]
    Unparsable {
        /// The 1-based number of the offending line.
        line: usize,
    },
    /// A quoted value starts on this line and never ends.
    #[error("the quoted value starting at line {line} is never closed")]
    UnterminatedQuote {
        /// The 1-based number of the line where the quote opens.
        line: usize,
    },
    /// The path is not a regular file (a FIFO, a device, a folder).
    #[error("not a regular file")]
    NotRegularFile,
    /// The file is larger than the 1 MiB limit.
    #[error("larger than 1 MiB")]
    TooLarge,
    /// The devslot block is malformed.
    #[error(transparent)]
    Block(#[from] BlockError),
}

/// One `KEY=value` line, the value as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvEntry {
    /// The variable name.
    pub key: String,
    /// The text after the `=`, quotes removed, nothing substituted.
    pub value: String,
}

/// The variables of a `.env` file outside its devslot block.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvFile {
    entries: Vec<EnvEntry>,
}

/// What reading a `.env` file gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvRead {
    /// There is no such file.
    Missing,
    /// The file was read.
    Parsed(EnvFile),
    /// The file exists but cannot be used.
    Invalid(EnvFileError),
}

impl EnvFile {
    /// Parses the text of a file, ignoring its devslot block.
    pub fn parse(text: &str) -> Result<Self, EnvFileError> {
        let masked = split_block(text)?.masked;
        let entries =
            parse_raw_entries(&masked).map_err(|line| EnvFileError::UnterminatedQuote { line })?;
        validate_syntax(&masked)?;
        Ok(Self { entries })
    }

    /// The raw value of `key`; the last definition wins, as in Compose.
    pub fn value_of(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .rev()
            .find(|entry| entry.key == key)
            .map(|entry| entry.value.as_str())
    }

    /// Every entry, in file order.
    pub fn entries(&self) -> &[EnvEntry] {
        &self.entries
    }
}

/// The raw value of `name` in `.env`, else in `.env.example`.
pub fn lookup_value<'a>(
    env: Option<&'a EnvFile>,
    example: Option<&'a EnvFile>,
    name: &str,
) -> Option<&'a str> {
    env.and_then(|file| file.value_of(name))
        .or_else(|| example.and_then(|file| file.value_of(name)))
}

/// Reads and parses the file at `path`, which must be a regular file of at
/// most 1 MiB.
pub fn read_env_file(path: &Path) -> io::Result<EnvRead> {
    let bytes = match read_bounded(path)? {
        BoundedRead::Missing => return Ok(EnvRead::Missing),
        BoundedRead::Unusable(Unreadable::NotRegularFile) => {
            return Ok(EnvRead::Invalid(EnvFileError::NotRegularFile));
        }
        BoundedRead::Unusable(Unreadable::TooLarge) => {
            return Ok(EnvRead::Invalid(EnvFileError::TooLarge));
        }
        BoundedRead::Bytes(bytes) => bytes,
    };
    let text = String::from_utf8_lossy(&bytes);
    Ok(match EnvFile::parse(&text) {
        Ok(file) => EnvRead::Parsed(file),
        Err(error) => EnvRead::Invalid(error),
    })
}

fn validate_syntax(text: &str) -> Result<(), EnvFileError> {
    for item in dotenvy::from_read_iter(text.as_bytes()) {
        match item {
            Ok(_) => {}
            Err(dotenvy::Error::LineParse(line, _)) => {
                return Err(EnvFileError::Unparsable {
                    line: locate_line(text, &line),
                });
            }
            Err(_) => return Err(EnvFileError::Unparsable { line: 1 }),
        }
    }
    Ok(())
}

fn locate_line(text: &str, offending: &str) -> usize {
    let first_offending_line = offending.lines().next().unwrap_or_default().trim();
    text.lines()
        .position(|line| line.trim() == first_offending_line)
        .map_or(1, |index| index + 1)
}

/// The entries of `text`, or the line where a quoted value is never closed.
fn parse_raw_entries(text: &str) -> Result<Vec<EnvEntry>, usize> {
    let mut entries = Vec::new();
    let mut lines = text.lines().enumerate();
    while let Some((index, line)) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let declaration = trimmed.strip_prefix(EXPORT_PREFIX).unwrap_or(trimmed);
        let Some((key, raw_value)) = declaration.split_once('=') else {
            continue;
        };
        let value = read_value(raw_value.trim_start(), &mut lines).ok_or(index + 1)?;
        entries.push(EnvEntry {
            key: key.trim().to_owned(),
            value,
        });
    }
    Ok(entries)
}

fn read_value<'a>(
    first_part: &str,
    following: &mut impl Iterator<Item = (usize, &'a str)>,
) -> Option<String> {
    let Some(quote) = first_part
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\''))
    else {
        return Some(strip_inline_comment(first_part).trim().to_owned());
    };
    let mut collected = first_part[1..].to_owned();
    let mut scanned_until = 0;
    let mut escape = EscapeState::Plain;
    loop {
        match find_closing_quote_from(&collected, quote, scanned_until, escape) {
            Scan::Found(end) => {
                collected.truncate(end);
                return Some(collected);
            }
            Scan::NotFound {
                scanned_until: next_start,
                escape: next_escape,
            } => {
                scanned_until = next_start;
                escape = next_escape;
            }
        }
        let (_, next_line) = following.next()?;
        collected.push('\n');
        collected.push_str(next_line);
    }
}

/// Whether the next character of a double-quoted value is escaped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EscapeState {
    Plain,
    Escaped,
}

/// Where a closing-quote scan stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    Found(usize),
    NotFound {
        scanned_until: usize,
        escape: EscapeState,
    },
}

fn find_closing_quote_from(text: &str, quote: char, start: usize, mut escape: EscapeState) -> Scan {
    for (offset, character) in text[start..].char_indices() {
        if escape == EscapeState::Escaped {
            escape = EscapeState::Plain;
            continue;
        }
        if character == '\\' && quote == '"' {
            escape = EscapeState::Escaped;
            continue;
        }
        if character == quote {
            return Scan::Found(start + offset);
        }
    }
    Scan::NotFound {
        scanned_until: text.len(),
        escape,
    }
}

fn strip_inline_comment(value: &str) -> &str {
    value
        .find(COMMENT_MARKER)
        .map_or(value, |index| &value[..index])
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::project::dotenv_block::{BLOCK_END, BLOCK_START};

    fn parse(text: &str) -> EnvFile {
        EnvFile::parse(text).unwrap()
    }

    #[test]
    fn should_read_the_raw_value_when_the_line_is_a_plain_assignment() {
        assert_eq!(parse("PORT=3333\n").value_of("PORT"), Some("3333"));
    }

    #[test]
    fn should_not_substitute_variables_when_the_value_references_one() {
        let file = parse("DB_PORT=5432\nURL=localhost:${DB_PORT}\n");

        assert_eq!(file.value_of("URL"), Some("localhost:${DB_PORT}"));
    }

    #[test]
    fn should_not_substitute_process_variables_when_the_value_references_home() {
        assert_eq!(parse("DIR=$HOME/x\n").value_of("DIR"), Some("$HOME/x"));
    }

    #[test]
    fn should_keep_the_last_value_when_a_key_is_defined_twice() {
        assert_eq!(parse("PORT=1\nPORT=2\n").value_of("PORT"), Some("2"));
    }

    #[test]
    fn should_remove_quotes_when_the_value_is_quoted() {
        let file = parse("A=\"x y\"\nB='z'\n");

        assert_eq!(
            (file.value_of("A"), file.value_of("B")),
            (Some("x y"), Some("z"))
        );
    }

    #[test]
    fn should_drop_the_inline_comment_when_the_value_is_unquoted() {
        assert_eq!(parse("PORT=3333 # app\n").value_of("PORT"), Some("3333"));
    }

    #[test]
    fn should_accept_the_export_prefix_when_the_line_starts_with_it() {
        assert_eq!(parse("export PORT=3333\n").value_of("PORT"), Some("3333"));
    }

    #[test]
    fn should_read_a_multiline_quoted_value_as_one_entry_when_the_quote_spans_lines() {
        let file = parse("KEY=\"line1\nPORT=9\"\nOTHER=1\n");

        assert_eq!((file.value_of("PORT"), file.entries().len()), (None, 2));
    }

    #[test]
    fn should_ignore_the_devslot_block_when_the_file_has_one() {
        let text = format!("PORT=3333\n{BLOCK_START}\nPORT=20050\n{BLOCK_END}\n");

        assert_eq!(parse(&text).value_of("PORT"), Some("3333"));
    }

    #[test]
    fn should_report_the_line_number_only_when_a_line_cannot_be_parsed() {
        let error = EnvFile::parse("A=1\nB=2\nTHIS LINE IS FAKE-SECRET\nC=3\n").unwrap_err();

        assert_eq!(error, EnvFileError::Unparsable { line: 3 });
    }

    #[test]
    fn should_report_the_line_where_the_quote_opens_when_a_quoted_value_is_unterminated() {
        let error = EnvFile::parse("A=1\nC=2\nB=\"unterminated FAKE-SECRET\nD=3\n").unwrap_err();

        assert_eq!(error, EnvFileError::UnterminatedQuote { line: 3 });
        assert!(!error.to_string().contains("FAKE-SECRET"));
    }

    #[test]
    fn should_keep_original_line_numbers_when_a_block_precedes_the_faulty_line() {
        let text = format!("A=1\n{BLOCK_START}\nPORT=1\n{BLOCK_END}\nBROKEN LINE\n");

        assert_eq!(
            EnvFile::parse(&text).unwrap_err(),
            EnvFileError::Unparsable { line: 5 }
        );
    }

    #[test]
    fn should_report_not_a_regular_file_when_the_path_is_a_folder() {
        let directory = tempfile::tempdir().unwrap();

        assert_eq!(
            read_env_file(directory.path()).unwrap(),
            EnvRead::Invalid(EnvFileError::NotRegularFile)
        );
    }

    #[test]
    fn should_not_contain_the_line_in_the_message_when_a_line_cannot_be_parsed() {
        let error = EnvFile::parse("THIS LINE IS FAKE-SECRET\n").unwrap_err();

        assert!(!error.to_string().contains("FAKE-SECRET"));
    }

    #[test]
    fn should_report_the_block_problem_when_the_block_is_malformed() {
        let text = format!("A=1\n{BLOCK_START}\n");

        assert_eq!(
            EnvFile::parse(&text).unwrap_err(),
            EnvFileError::Block(BlockError::StartWithoutEnd { start_line: 2 })
        );
    }

    #[test]
    fn should_fall_back_to_the_example_when_the_env_file_lacks_the_key() {
        let env = parse("A=1\n");
        let example = parse("A=2\nB=3\n");

        assert_eq!(
            (
                lookup_value(Some(&env), Some(&example), "A"),
                lookup_value(Some(&env), Some(&example), "B"),
                lookup_value(None, None, "A")
            ),
            (Some("1"), Some("3"), None)
        );
    }

    #[test]
    fn should_report_missing_when_the_file_does_not_exist() {
        let directory = tempfile::tempdir().unwrap();

        let read = read_env_file(&directory.path().join(".env")).unwrap();

        assert_eq!(read, EnvRead::Missing);
    }

    #[test]
    fn should_parse_an_unclosed_quote_in_linear_time_when_many_lines_follow() {
        let text = format!("A=\"{}", "\n".repeat(200_000));

        let started = std::time::Instant::now();
        let outcome = EnvFile::parse(&text);

        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        assert!(outcome.is_err());
    }

    #[test]
    fn should_keep_the_backslash_when_a_double_quoted_value_continues_after_it() {
        let entries = parse_raw_entries("A=\"one\\\ntwo\"\n").unwrap();

        assert_eq!(entries[0].value, "one\\\ntwo");
    }

    #[test]
    fn should_close_on_the_quote_when_the_backslash_escapes_the_line_break() {
        let entries = parse_raw_entries("A=\"x\\\n\"y\"\n").unwrap();

        assert_eq!(entries[0].value, "x\\\n");
    }
}
