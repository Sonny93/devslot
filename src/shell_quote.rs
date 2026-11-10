//! Quoting text for a POSIX shell.

/// Wraps `text` in single quotes so a shell reads it as one literal word.
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn should_wrap_a_plain_name_in_single_quotes() {
        assert_eq!(shell_quote("dev.compose.yml"), "'dev.compose.yml'");
    }

    #[test]
    fn should_keep_a_space_inside_the_quotes() {
        assert_eq!(shell_quote("my file.yml"), "'my file.yml'");
    }

    #[test]
    fn should_leave_a_command_substitution_inert() {
        assert_eq!(
            shell_quote("x$(id>pwned).compose.yml"),
            "'x$(id>pwned).compose.yml'"
        );
    }

    #[test]
    fn should_escape_a_single_quote_inside_the_name() {
        assert_eq!(shell_quote("it's.yml"), "'it'\\''s.yml'");
    }

    #[test]
    fn should_keep_a_newline_inside_the_quotes() {
        assert_eq!(shell_quote("a\nb"), "'a\nb'");
    }
}
