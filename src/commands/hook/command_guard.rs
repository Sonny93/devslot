//! A guard rail against agent commands that delete devslot data. It reads the
//! text of a shell command, so it is not a sandbox: a command built at run time
//! gets through, and a quoted string such as `echo "ds rm"` is denied too.

const SEPARATORS: [char; 9] = [';', '&', '|', '(', ')', '{', '}', '\n', '`'];
const QUOTES: [char; 2] = ['"', '\''];
const WRAPPERS: [&str; 16] = [
    "sudo", "env", "command", "exec", "nohup", "time", "then", "do", "else", "!", "timeout",
    "nice", "ionice", "stdbuf", "xargs", "watch",
];
const DESTRUCTIVE_SUBCOMMANDS: [&str; 2] = ["rm", "prune"];
const PROGRAM_NAME: &str = "ds";
const PROGRAM_PATH_SUFFIX: &str = "/ds";
const RUN_SUBCOMMAND: &str = "run";
const COMMAND_SEPARATOR: &str = "--";
const COLOR_OPTION: &str = "--color";
const COLOR_OPTION_PREFIX: &str = "--color=";

/// Whether `command` runs `ds rm` or `ds prune`: `ds` as a whole word, at the
/// start of the command or after a separator, a wrapper such as `sudo`,
/// `env`, `timeout` or `xargs`, a variable assignment, a shell keyword, or a
/// path ending in `/ds`, followed by `rm` or `prune`. Quotes and a leading
/// backslash on a word are ignored, and the command after `ds run --` is
/// checked too. A quoted text is also read as a command of its own, so
/// `bash -c 'ds rm'` is denied.
pub fn is_destructive_ds_command(command: &str) -> bool {
    command.split(is_separator).any(is_destructive_segment)
        || command
            .split(|character| is_separator(character) || QUOTES.contains(&character))
            .any(is_destructive_segment)
}

fn is_separator(character: char) -> bool {
    SEPARATORS.contains(&character)
}

fn is_destructive_segment(segment: &str) -> bool {
    let words: Vec<String> = segment.split_whitespace().map(plain_word).collect();
    is_destructive_words(&words)
}

fn plain_word(word: &str) -> String {
    let unquoted: String = word
        .chars()
        .filter(|character| !QUOTES.contains(character))
        .collect();
    unquoted.trim_start_matches('\\').to_owned()
}

fn is_destructive_words(words: &[String]) -> bool {
    let Some(start) = program_index(words) else {
        return false;
    };
    if !is_ds_program(&words[start]) {
        return false;
    }
    let mut rest = words[start + 1..].iter().map(String::as_str);
    match subcommand_of(&mut rest) {
        Some(subcommand) if DESTRUCTIVE_SUBCOMMANDS.contains(&subcommand) => true,
        Some(RUN_SUBCOMMAND) => {
            let nested: Vec<String> = rest
                .skip_while(|word| *word != COMMAND_SEPARATOR)
                .skip(1)
                .map(str::to_owned)
                .collect();
            is_destructive_words(&nested)
        }
        _ => false,
    }
}

fn program_index(words: &[String]) -> Option<usize> {
    let mut has_wrapper = false;
    for (index, word) in words.iter().enumerate() {
        let is_wrapper = WRAPPERS.contains(&word.as_str());
        let is_option_value = has_wrapper && word.starts_with(|first: char| first.is_ascii_digit());
        if !(is_wrapper || is_option_value || word.starts_with('-') || is_assignment(word)) {
            return Some(index);
        }
        has_wrapper |= is_wrapper;
    }
    None
}

fn is_ds_program(word: &str) -> bool {
    word == PROGRAM_NAME || word.ends_with(PROGRAM_PATH_SUFFIX)
}

fn is_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    name.chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn subcommand_of<'a>(words: &mut impl Iterator<Item = &'a str>) -> Option<&'a str> {
    while let Some(word) = words.next() {
        if word == COLOR_OPTION {
            words.next();
            continue;
        }
        if word.starts_with(COLOR_OPTION_PREFIX) {
            continue;
        }
        return Some(word);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_deny_ds_rm_and_ds_prune_when_they_stand_alone() {
        for command in ["ds rm", "ds rm --yes", "ds prune", "ds prune --yes"] {
            assert!(is_destructive_ds_command(command), "{command}");
        }
    }

    #[test]
    fn should_deny_the_command_when_it_follows_a_separator() {
        for command in [
            "cd x && ds prune --yes",
            "cd x; ds rm",
            "false || ds rm",
            "echo a | ds rm",
            "(ds rm)",
            "echo a\nds prune",
            "sleep 1 & ds rm",
            "if true; then ds rm; fi",
        ] {
            assert!(is_destructive_ds_command(command), "{command}");
        }
    }

    #[test]
    fn should_deny_the_command_when_it_goes_through_a_wrapper_or_a_path() {
        for command in [
            "/usr/local/bin/ds rm",
            "~/.cargo/bin/ds prune --yes",
            "sudo ds rm",
            "env FOO=1 ds rm",
            "FOO=1 ds prune",
            "ds --color never rm",
            "ds --color=never prune",
            "bash -c 'ds rm --yes'",
            "bash -c \"ds prune\"",
            "ds 'rm'",
            "\"ds\" rm",
            "\\ds rm",
            "d\"s\" rm",
            "ds run -- ds rm --yes",
            "ds run -- bash -c 'ds prune'",
            "timeout 5 ds rm",
            "timeout --foreground 10s ds prune",
            "nice -n 10 ds prune",
            "ionice -c 2 ds rm",
            "stdbuf -oL ds rm",
            "xargs ds rm",
            "watch -n 1 ds rm",
        ] {
            assert!(is_destructive_ds_command(command), "{command}");
        }
    }

    #[test]
    fn should_deny_a_quoted_ds_rm_as_a_known_false_positive() {
        assert!(is_destructive_ds_command("echo \"ds rm\""));
    }

    #[test]
    fn should_allow_the_commands_agents_need() {
        for command in [
            "ds up",
            "ds reset",
            "ds reset postgres",
            "ds info --json",
            "ds run -- rm -rf build",
            "ds ensure",
            "ds",
            "ds run -- ds reset",
            "ds run -- echo rm",
            "timeout 5 ds up",
        ] {
            assert!(!is_destructive_ds_command(command), "{command}");
        }
    }

    #[test]
    fn should_allow_a_different_word_that_only_looks_like_the_command() {
        for command in [
            "dsrm",
            "ds rmx",
            "ds prunes",
            "xds rm",
            "ls /ds/rm",
            "echo ds rm",
            "rm -rf build",
            "grep ds rm file",
        ] {
            assert!(!is_destructive_ds_command(command), "{command}");
        }
    }
}
