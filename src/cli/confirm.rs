//! Asking the user to confirm a destructive command. The prompt is shown only
//! when stdin and stderr are both terminals, so an agent never waits on it.

use std::io::{BufRead, IsTerminal, Write};

use super::exit_code;

/// What the user answered, or why nobody could be asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    /// The user typed `y` or `yes`.
    Confirmed,
    /// The user typed anything else, or nothing.
    Declined,
    /// stdin or stderr is not a terminal: nobody was asked.
    NoTerminal,
}

/// Whether the command asks before it deletes anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationPolicy {
    /// `--yes` was given: no question.
    AssumeYes,
    /// Ask in a terminal, refuse without one.
    Ask,
}

impl From<bool> for ConfirmationPolicy {
    /// Reads the value of a `--yes` flag.
    fn from(is_yes_given: bool) -> Self {
        if is_yes_given {
            Self::AssumeYes
        } else {
            Self::Ask
        }
    }
}

/// `None` when the user agreed (or `--yes` was given). Otherwise tells the
/// user nothing was changed and gives the exit code of the refusal.
pub fn exit_code_unless_confirmed(
    command: &str,
    policy: ConfirmationPolicy,
    prompt: &str,
) -> Option<u8> {
    let confirmation = match policy {
        ConfirmationPolicy::AssumeYes => Confirmation::Confirmed,
        ConfirmationPolicy::Ask => confirm(prompt),
    };
    match confirmation {
        Confirmation::Confirmed => None,
        Confirmation::NoTerminal => {
            eprintln!("ds: error: {command} deletes data; run it in a terminal or pass --yes");
            Some(exit_code::ERROR)
        }
        Confirmation::Declined => {
            eprintln!("ds: nothing was changed");
            Some(exit_code::ERROR)
        }
    }
}

/// Whether `answer` is a yes: `y` or `yes`, in any case, around any spaces.
pub fn is_affirmative(answer: &str) -> bool {
    let answer = answer.trim();
    answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

/// Whether stdin and stderr are both terminals, so [`confirm`] can ask.
pub fn can_ask() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Asks `prompt` on stderr and reads the answer from stdin, when both are
/// terminals. Never opens `/dev/tty`.
pub fn confirm(prompt: &str) -> Confirmation {
    ask(
        prompt,
        can_ask(),
        &mut std::io::stdin().lock(),
        &mut std::io::stderr().lock(),
    )
}

fn ask(
    prompt: &str,
    is_interactive: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Confirmation {
    if !is_interactive {
        return Confirmation::NoTerminal;
    }
    let _ = write!(output, "{prompt}");
    let _ = output.flush();
    let mut answer = String::new();
    if input.read_line(&mut answer).is_err() {
        return Confirmation::Declined;
    }
    if is_affirmative(&answer) {
        Confirmation::Confirmed
    } else {
        Confirmation::Declined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answered(answer: &str) -> (Confirmation, String) {
        let mut output = Vec::new();
        let confirmation = ask("Sure? ", true, &mut answer.as_bytes(), &mut output);
        (confirmation, String::from_utf8(output).unwrap())
    }

    #[test]
    fn should_ask_unless_yes_is_given() {
        assert_eq!(
            ConfirmationPolicy::from(true),
            ConfirmationPolicy::AssumeYes
        );
        assert_eq!(ConfirmationPolicy::from(false), ConfirmationPolicy::Ask);
    }

    #[test]
    fn should_not_refuse_when_yes_is_given() {
        let code = exit_code_unless_confirmed("ds rm", ConfirmationPolicy::AssumeYes, "Sure? ");

        assert_eq!(code, None);
    }

    #[test]
    fn should_accept_y_and_yes_in_any_case() {
        let answers = ["y\n", "Y\n", "yes\n", "YES\n", " Yes \n"];

        assert!(answers.iter().all(|answer| is_affirmative(answer)));
    }

    #[test]
    fn should_decline_anything_else() {
        let answers = ["\n", "n\n", "no\n", "yep\n", "yes please\n", ""];

        assert!(answers.iter().all(|answer| !is_affirmative(answer)));
    }

    #[test]
    fn should_print_the_prompt_and_confirm_when_the_user_types_yes() {
        assert_eq!(
            answered("yes\n"),
            (Confirmation::Confirmed, "Sure? ".to_owned())
        );
    }

    #[test]
    fn should_decline_when_the_input_ends_without_an_answer() {
        assert_eq!(answered("").0, Confirmation::Declined);
    }

    #[test]
    fn should_not_ask_and_report_no_terminal_when_the_session_is_not_interactive() {
        let mut output = Vec::new();

        let confirmation = ask("Sure? ", false, &mut "y\n".as_bytes(), &mut output);

        assert_eq!(
            (confirmation, output),
            (Confirmation::NoTerminal, Vec::new())
        );
    }
}
