//! The devslot block of a `.env`: the lines between the two markers.
//!
//! Phase 3 reuses this module to write the block, so the parsing rules live
//! here and nowhere else.

use thiserror::Error;

/// The first line of the block.
pub const BLOCK_START: &str = "# >>> devslot: generated automatically, do not edit by hand >>>";
/// The last line of the block.
pub const BLOCK_END: &str = "# <<< devslot <<<";

const START_PREFIX: &str = "# >>> devslot";
const END_PREFIX: &str = "# <<< devslot";

/// Why the block cannot be told apart from the rest of the file. Line numbers
/// start at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BlockError {
    /// A start marker has no end marker after it.
    #[error("the devslot block starting at line {start_line} has no end marker")]
    StartWithoutEnd {
        /// The line of the start marker.
        start_line: usize,
    },
    /// An end marker has no start marker before it.
    #[error("the end marker of the devslot block at line {end_line} has no start marker")]
    EndWithoutStart {
        /// The line of the end marker.
        end_line: usize,
    },
    /// A second block starts after a first one.
    #[error("a second devslot block starts at line {start_line}")]
    MultipleBlocks {
        /// The line of the second start marker.
        start_line: usize,
    },
    /// A start marker appears inside a block.
    #[error("a devslot block starts at line {start_line} inside another one")]
    NestedStart {
        /// The line of the inner start marker.
        start_line: usize,
    },
}

/// The text of a file around its block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockSplit {
    /// The lines outside the block, in their original order and endings.
    pub outside: String,
    /// The whole text with each block line replaced by an empty line, so line
    /// numbers still match the original file.
    pub masked: String,
    /// Whether the file holds a block.
    pub has_block: bool,
}

fn is_start(line: &str) -> bool {
    line.trim().starts_with(START_PREFIX)
}

fn is_end(line: &str) -> bool {
    line.trim().starts_with(END_PREFIX)
}

/// Separates the lines of the block from the others.
pub fn split_block(text: &str) -> Result<BlockSplit, BlockError> {
    let mut outside = String::with_capacity(text.len());
    let mut masked = String::with_capacity(text.len());
    let mut open_line: Option<usize> = None;
    let mut has_block = false;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        let line_number = index + 1;
        if is_start(line) || is_end(line) || open_line.is_some() {
            masked.push('\n');
        } else {
            masked.push_str(line);
        }
        match (open_line, is_start(line), is_end(line)) {
            (None, true, _) if has_block => {
                return Err(BlockError::MultipleBlocks {
                    start_line: line_number,
                });
            }
            (None, true, _) => {
                open_line = Some(line_number);
                has_block = true;
            }
            (None, false, true) => {
                return Err(BlockError::EndWithoutStart {
                    end_line: line_number,
                });
            }
            (Some(_), true, _) => {
                return Err(BlockError::NestedStart {
                    start_line: line_number,
                });
            }
            (Some(_), false, true) => open_line = None,
            (Some(_), false, false) => {}
            (None, false, false) => outside.push_str(line),
        }
    }
    match open_line {
        Some(start_line) => Err(BlockError::StartWithoutEnd { start_line }),
        None => Ok(BlockSplit {
            outside,
            masked,
            has_block,
        }),
    }
}

/// The text of a file without its block.
pub fn strip_block(text: &str) -> Result<String, BlockError> {
    split_block(text).map(|split| split.outside)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn block_text() -> String {
        format!("{BLOCK_START}\nPORT=20050\n{BLOCK_END}\n")
    }

    #[test]
    fn should_return_the_text_unchanged_when_there_is_no_block() {
        let split = split_block("A=1\nB=2\n").unwrap();

        assert_eq!(
            split,
            BlockSplit {
                outside: "A=1\nB=2\n".to_owned(),
                masked: "A=1\nB=2\n".to_owned(),
                has_block: false
            }
        );
    }

    #[test]
    fn should_remove_the_block_and_keep_the_other_lines_when_the_block_is_at_the_end() {
        let text = format!("A=1\n{}", block_text());

        assert_eq!(strip_block(&text).unwrap(), "A=1\n");
    }

    #[test]
    fn should_keep_the_lines_after_the_block_when_the_block_is_in_the_middle() {
        let text = format!("A=1\n{}B=2\n", block_text());

        assert_eq!(strip_block(&text).unwrap(), "A=1\nB=2\n");
    }

    #[test]
    fn should_keep_line_numbers_when_masking_a_block() {
        let text = format!("A=1\n{}B=2\n", block_text());

        assert_eq!(split_block(&text).unwrap().masked, "A=1\n\n\n\nB=2\n");
    }

    #[test]
    fn should_report_the_start_line_when_the_end_marker_is_missing() {
        let text = format!("A=1\n{BLOCK_START}\nPORT=1\n");

        assert_eq!(
            strip_block(&text),
            Err(BlockError::StartWithoutEnd { start_line: 2 })
        );
    }

    #[test]
    fn should_report_the_end_line_when_the_start_marker_is_missing() {
        let text = format!("A=1\n{BLOCK_END}\n");

        assert_eq!(
            strip_block(&text),
            Err(BlockError::EndWithoutStart { end_line: 2 })
        );
    }

    #[test]
    fn should_report_the_second_start_line_when_there_are_two_blocks() {
        let text = format!("{}A=1\n{}", block_text(), block_text());

        assert_eq!(
            strip_block(&text),
            Err(BlockError::MultipleBlocks { start_line: 5 })
        );
    }

    #[test]
    fn should_report_a_nested_start_when_a_block_starts_inside_a_block() {
        let text = format!("{BLOCK_START}\n{BLOCK_START}\n{BLOCK_END}\n");

        assert_eq!(
            strip_block(&text),
            Err(BlockError::NestedStart { start_line: 2 })
        );
    }

    #[test]
    fn should_handle_a_file_without_trailing_newline_when_the_end_marker_is_last() {
        let text = format!("A=1\n{BLOCK_START}\nPORT=1\n{BLOCK_END}");

        assert_eq!(strip_block(&text).unwrap(), "A=1\n");
    }
}
