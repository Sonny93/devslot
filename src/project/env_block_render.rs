//! Writing the devslot block of a `.env`: the text of the block, and its
//! placement in a file without touching any other byte.

use std::fmt::Write as _;

use thiserror::Error;

use super::dotenv_block::{BLOCK_END, BLOCK_START, BlockError, split_block, strip_block};

const BLOCK_COMMENT_LINES: [&str; 2] = [
    "# To go back to the default ports: ds rm",
    "# (also deletes the containers and data of this checkout)",
];
const BARE_EXTRA_CHARACTERS: &str = "_./:@,+=%-";
const FORBIDDEN_IN_QUOTES: [char; 4] = ['\'', '$', '\\', '\n'];

/// Why a block cannot be written. The value is never part of the message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RenderError {
    /// A value holds a character no dotenv parser reads the same way.
    #[error(
        "the value of {key} holds a quote, a dollar sign, a backslash or a line break, and cannot be written to .env"
    )]
    UnsupportedValue {
        /// The variable at fault.
        key: String,
    },
}

fn is_bare(value: &str) -> bool {
    value.chars().all(|character| {
        character.is_ascii_alphanumeric() || BARE_EXTRA_CHARACTERS.contains(character)
    })
}

fn encode_value(key: &str, value: &str) -> Result<String, RenderError> {
    if is_bare(value) {
        return Ok(value.to_owned());
    }
    if value.contains(FORBIDDEN_IN_QUOTES) {
        return Err(RenderError::UnsupportedValue {
            key: key.to_owned(),
        });
    }
    Ok(format!("'{value}'"))
}

/// The text of the block for `entries`, every line ending in a line feed.
pub fn render_block(entries: &[(String, String)]) -> Result<String, RenderError> {
    let mut block = format!("{BLOCK_START}\n");
    for line in BLOCK_COMMENT_LINES {
        block.push_str(line);
        block.push('\n');
    }
    for (key, value) in entries {
        let _ = writeln!(block, "{key}={}", encode_value(key, value)?);
    }
    block.push_str(BLOCK_END);
    block.push('\n');
    Ok(block)
}

/// `text` with `block` in place of its current block, or appended when it has
/// none. Every other byte of `text` is kept.
pub fn place_block(text: &str, block: &str) -> Result<String, BlockError> {
    let split = split_block(text)?;
    if split.has_block {
        return Ok(format!("{}{block}{}", split.before, split.after));
    }
    let separator = if text.is_empty() || text.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    Ok(format!("{text}{separator}{block}"))
}

/// `text` without its block.
pub fn remove_block(text: &str) -> Result<String, BlockError> {
    strip_block(text)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn entries(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn block(pairs: &[(&str, &str)]) -> String {
        render_block(&entries(pairs)).unwrap()
    }

    #[test]
    fn should_render_the_markers_the_comments_and_the_entries_in_order() {
        assert_eq!(
            block(&[("PORT", "20050"), ("DEVSLOT_INSTANCE", "app-3f2a")]),
            format!(
                "{BLOCK_START}\n# To go back to the default ports: ds rm\n# (also deletes the containers and data of this checkout)\nPORT=20050\nDEVSLOT_INSTANCE=app-3f2a\n{BLOCK_END}\n"
            )
        );
    }

    #[test]
    fn should_write_a_value_bare_when_it_only_holds_safe_characters() {
        assert!(
            block(&[
                ("COMPOSE_FILE", "a/b.yml:c/d.yml"),
                ("URL", "http://h:1/a@b,c+d=e%f-g_h")
            ])
            .contains("COMPOSE_FILE=a/b.yml:c/d.yml\nURL=http://h:1/a@b,c+d=e%f-g_h\n")
        );
    }

    #[test]
    fn should_write_an_empty_value_bare() {
        assert!(block(&[("EMPTY", "")]).contains("\nEMPTY=\n"));
    }

    #[test]
    fn should_single_quote_a_value_with_a_space_or_a_hash() {
        assert!(block(&[("A", "two words"), ("B", "x#y")]).contains("\nA='two words'\nB='x#y'\n"));
    }

    #[test]
    fn should_refuse_a_value_with_a_single_quote_naming_the_key_only() {
        let error = render_block(&entries(&[("SECRET_KEY", "pa'ss")])).unwrap_err();

        assert_eq!(
            error,
            RenderError::UnsupportedValue {
                key: "SECRET_KEY".to_owned()
            }
        );
        assert!(!error.to_string().contains("pa'ss"));
    }

    #[test]
    fn should_refuse_a_value_with_a_dollar_a_backslash_or_a_line_break() {
        for value in ["a$b", "a\\b", "a\nb"] {
            assert!(
                render_block(&entries(&[("K", value)])).is_err(),
                "{value:?}"
            );
        }
    }

    #[test]
    fn should_append_the_block_when_the_file_has_none() {
        assert_eq!(place_block("A=1\n", "BLOCK\n").unwrap(), "A=1\nBLOCK\n");
    }

    #[test]
    fn should_add_a_line_feed_before_the_block_when_the_file_does_not_end_with_one() {
        assert_eq!(place_block("A=1", "BLOCK\n").unwrap(), "A=1\nBLOCK\n");
    }

    #[test]
    fn should_write_only_the_block_when_the_file_is_empty() {
        assert_eq!(place_block("", "BLOCK\n").unwrap(), "BLOCK\n");
    }

    #[test]
    fn should_replace_the_block_in_place_and_keep_the_surrounding_bytes() {
        let text = format!("A=1\r\nB=2\n{BLOCK_START}\nOLD=1\n{BLOCK_END}\nC=3\r\nD=4");

        assert_eq!(
            place_block(&text, "NEW\n").unwrap(),
            "A=1\r\nB=2\nNEW\nC=3\r\nD=4"
        );
    }

    #[test]
    fn should_keep_crlf_lines_and_the_missing_final_newline_when_appending() {
        assert_eq!(
            place_block("A=1\r\nB=2", "BLOCK\n").unwrap(),
            "A=1\r\nB=2\nBLOCK\n"
        );
    }

    #[test]
    fn should_return_the_original_when_placing_then_removing_in_a_file_ending_with_a_newline() {
        let original = "A=1\r\n# note\nB='x y'\n";

        let placed = place_block(original, &block(&[("PORT", "1")])).unwrap();

        assert_eq!(remove_block(&placed).unwrap(), original);
    }

    #[test]
    fn should_return_the_original_when_placing_then_removing_in_an_empty_file() {
        let placed = place_block("", &block(&[("PORT", "1")])).unwrap();

        assert_eq!(remove_block(&placed).unwrap(), "");
    }

    #[test]
    fn should_refuse_to_place_a_block_when_the_existing_block_is_malformed() {
        let text = format!("A=1\n{BLOCK_START}\nPORT=1\n");

        assert_eq!(
            place_block(&text, "BLOCK\n"),
            Err(BlockError::StartWithoutEnd { start_line: 2 })
        );
    }

    #[test]
    fn should_refuse_to_remove_a_malformed_block() {
        let text = format!("{BLOCK_END}\n");

        assert_eq!(
            remove_block(&text),
            Err(BlockError::EndWithoutStart { end_line: 1 })
        );
    }
}
