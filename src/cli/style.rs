//! How the output looks: color, decided once from the terminal and the
//! environment. Color is never the only carrier of information.

use std::io::IsTerminal;

use clap::ValueEnum;

const NO_COLOR_VARIABLE: &str = "NO_COLOR";
const TERM_VARIABLE: &str = "TERM";
const DUMB_TERMINAL: &str = "dumb";

/// What `--color` asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    /// Color when stdout is a terminal that supports it.
    #[default]
    Auto,
    /// Always color.
    Always,
    /// Never color.
    Never,
}

/// The facts the style is decided from; gathered once, so the decision itself
/// is pure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StyleEnvironment<'a> {
    /// The value of `--color`.
    pub color_choice: ColorChoice,
    /// Whether stdout is a terminal.
    pub stdout_is_terminal: bool,
    /// The value of `NO_COLOR`, when set.
    pub no_color: Option<&'a str>,
    /// The value of `TERM`, when set.
    pub term: Option<&'a str>,
}

/// Whether to color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    /// Colors the level words.
    pub color: bool,
}

/// A color a word can be painted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Something is wrong.
    Bad,
    /// Something needs attention.
    Warning,
    /// Something is fine.
    Good,
    /// Something is secondary.
    Muted,
}

impl Tone {
    const fn escape_code(self) -> &'static str {
        match self {
            Self::Bad => "31",
            Self::Warning => "33",
            Self::Good => "32",
            Self::Muted => "2",
        }
    }
}

impl Style {
    /// Decides the style from the environment.
    ///
    /// Color needs a terminal, no `NO_COLOR` (unless empty), and a `TERM` that
    /// is not `dumb`; `--color always` and `never` override all of that.
    pub fn detect(environment: &StyleEnvironment<'_>) -> Self {
        Self {
            color: should_color(environment),
        }
    }

    /// The style for this process: reads the environment and asks whether
    /// stdout is a terminal.
    pub fn for_process(color_choice: ColorChoice) -> Self {
        Self::for_stream(color_choice, std::io::stdout().is_terminal())
    }

    /// The style for messages on stderr: like [`Style::for_process`], but asks
    /// whether stderr is a terminal.
    pub fn for_stderr(color_choice: ColorChoice) -> Self {
        Self::for_stream(color_choice, std::io::stderr().is_terminal())
    }

    fn for_stream(color_choice: ColorChoice, is_terminal: bool) -> Self {
        let no_color = std::env::var(NO_COLOR_VARIABLE).ok();
        let term = std::env::var(TERM_VARIABLE).ok();
        Self::detect(&StyleEnvironment {
            color_choice,
            stdout_is_terminal: is_terminal,
            no_color: no_color.as_deref(),
            term: term.as_deref(),
        })
    }

    /// Wraps `text` in the escape codes of `tone` when color is on.
    pub fn paint(self, tone: Tone, text: &str) -> String {
        if !self.color {
            return text.to_owned();
        }
        format!("\x1b[{}m{text}\x1b[0m", tone.escape_code())
    }
}

fn should_color(environment: &StyleEnvironment<'_>) -> bool {
    match environment.color_choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => {
            environment.stdout_is_terminal
                && environment.no_color.is_none_or(str::is_empty)
                && environment.term != Some(DUMB_TERMINAL)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal_environment() -> StyleEnvironment<'static> {
        StyleEnvironment {
            color_choice: ColorChoice::Auto,
            stdout_is_terminal: true,
            no_color: None,
            term: Some("xterm-256color"),
        }
    }

    fn color_of(environment: StyleEnvironment<'_>) -> bool {
        Style::detect(&environment).color
    }

    #[test]
    fn should_color_when_stdout_is_a_capable_terminal() {
        assert!(color_of(terminal_environment()));
    }

    #[test]
    fn should_not_color_when_stdout_is_not_a_terminal() {
        assert!(!color_of(StyleEnvironment {
            stdout_is_terminal: false,
            ..terminal_environment()
        }));
    }

    #[test]
    fn should_not_color_when_no_color_is_set() {
        assert!(!color_of(StyleEnvironment {
            no_color: Some("1"),
            ..terminal_environment()
        }));
    }

    #[test]
    fn should_color_when_no_color_is_empty() {
        assert!(color_of(StyleEnvironment {
            no_color: Some(""),
            ..terminal_environment()
        }));
    }

    #[test]
    fn should_not_color_when_the_terminal_is_dumb() {
        assert!(!color_of(StyleEnvironment {
            term: Some("dumb"),
            ..terminal_environment()
        }));
    }

    #[test]
    fn should_color_when_term_is_unset() {
        assert!(color_of(StyleEnvironment {
            term: None,
            ..terminal_environment()
        }));
    }

    #[test]
    fn should_color_when_always_is_asked_even_without_a_terminal() {
        assert!(color_of(StyleEnvironment {
            color_choice: ColorChoice::Always,
            stdout_is_terminal: false,
            no_color: Some("1"),
            term: Some("dumb"),
        }));
    }

    #[test]
    fn should_not_color_when_never_is_asked_even_on_a_capable_terminal() {
        assert!(!color_of(StyleEnvironment {
            color_choice: ColorChoice::Never,
            ..terminal_environment()
        }));
    }

    #[test]
    fn should_leave_the_text_untouched_when_painting_without_color() {
        let style = Style { color: false };

        assert_eq!(style.paint(Tone::Bad, "failed"), "failed");
    }

    #[test]
    fn should_wrap_the_text_in_escape_codes_when_painting_with_color() {
        let style = Style { color: true };

        assert_eq!(style.paint(Tone::Good, "ok"), "\x1b[32mok\x1b[0m");
    }
}
