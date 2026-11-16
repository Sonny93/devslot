//! A guard rail against agent commands that delete devslot data or start an
//! environment from a checkout that must not run one. It reads the text of a
//! shell command, so it is not a sandbox: a command built at run time gets
//! through, and a quoted string such as `echo "ds rm"` is denied too.

const SEPARATORS: [char; 9] = [';', '&', '|', '(', ')', '{', '}', '\n', '`'];
const LINE_CONTINUATION: &str = "\\\n";
const QUOTES: [char; 2] = ['"', '\''];
const WRAPPERS: [&str; 22] = [
    "sudo", "env", "command", "exec", "nohup", "time", "then", "do", "else", "!", "timeout",
    "nice", "ionice", "stdbuf", "xargs", "watch", "if", "elif", "while", "until", "eval",
    "builtin",
];
const DESTRUCTIVE_SUBCOMMANDS: [&str; 2] = ["rm", "prune"];
const ENVIRONMENT_SUBCOMMANDS: [&str; 3] = ["up", "run", "reset"];
const PROGRAM_NAME: &str = "ds";
const PROGRAM_PATH_SUFFIX: &str = "/ds";
const RUN_SUBCOMMAND: &str = "run";
const HOOK_SUBCOMMAND: &str = "hook";
const WORKTREE_SUBCOMMAND: &str = "worktree";
const WORKTREE_REMOVE_ACTION: &str = "rm";
const COMMAND_SEPARATOR: &str = "--";
const COLOR_OPTION: &str = "--color";
const COLOR_OPTION_PREFIX: &str = "--color=";

/// Whether `command` runs `ds rm`, `ds prune`, `ds worktree rm` or any
/// `ds hook` command (Claude Code runs the hooks itself, never through the
/// Bash tool, so only an agent calling one by hand is blocked): `ds` as a whole word, at the
/// start of the command or after a separator, a wrapper such as `sudo`,
/// `env`, `timeout` or `xargs`, a variable assignment, a shell keyword, or a
/// path ending in `/ds`, followed by `rm` or `prune`. Quotes and a leading
/// backslash on a word are ignored, and the command after `ds run --` is
/// checked too. A quoted text is also read as a command of its own, so
/// `bash -c 'ds rm'` is denied.
pub fn is_destructive_ds_command(command: &str) -> bool {
    command_segments(command)
        .iter()
        .any(|segment| is_destructive_segment(segment))
}

/// Whether `command` runs `ds up`, `ds run` or `ds reset`, found the way
/// [`is_destructive_ds_command`] finds its commands.
pub fn is_environment_ds_command(command: &str) -> bool {
    ds_subcommands(command)
        .iter()
        .any(|subcommand| ENVIRONMENT_SUBCOMMANDS.contains(&subcommand.as_str()))
}

/// The distinct `ds` subcommands that `command` runs, in order of first
/// appearance, found the way [`is_destructive_ds_command`] finds its
/// commands: `ds up && ds info` gives `up` and `info`, and `ds run -- ds
/// reset` gives `run` and `reset`.
pub fn ds_subcommands(command: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for subcommand in command_segments(command)
        .iter()
        .flat_map(|segment| subcommands_of_segment(segment))
    {
        if !found.contains(&subcommand) {
            found.push(subcommand);
        }
    }
    found
}

fn command_segments(command: &str) -> Vec<String> {
    let command = command.replace(LINE_CONTINUATION, " ");
    let by_separator = command.split(is_separator).map(str::to_owned);
    let by_quote = command
        .split(|character| is_separator(character) || QUOTES.contains(&character))
        .map(str::to_owned);
    by_separator.chain(by_quote).collect()
}

fn is_separator(character: char) -> bool {
    SEPARATORS.contains(&character)
}

fn words_of(segment: &str) -> Vec<String> {
    segment.split_whitespace().map(plain_word).collect()
}

fn is_destructive_segment(segment: &str) -> bool {
    is_destructive_words(&words_of(segment))
}

fn subcommands_of_segment(segment: &str) -> Vec<String> {
    subcommands_of_words(&words_of(segment))
}

fn subcommands_of_words(words: &[String]) -> Vec<String> {
    let Some(start) = program_index(words) else {
        return Vec::new();
    };
    if !is_ds_program(&words[start]) {
        return Vec::new();
    }
    let mut rest = words[start + 1..].iter().map(String::as_str);
    let Some(subcommand) = subcommand_of(&mut rest) else {
        return Vec::new();
    };
    let mut found = vec![subcommand.to_owned()];
    if subcommand == RUN_SUBCOMMAND {
        found.extend(subcommands_of_words(&nested_command(rest)));
    }
    found
}

fn nested_command<'a>(rest: impl Iterator<Item = &'a str>) -> Vec<String> {
    rest.skip_while(|word| *word != COMMAND_SEPARATOR)
        .skip(1)
        .map(str::to_owned)
        .collect()
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
        Some(HOOK_SUBCOMMAND) => true,
        Some(WORKTREE_SUBCOMMAND) => subcommand_of(&mut rest) == Some(WORKTREE_REMOVE_ACTION),
        Some(RUN_SUBCOMMAND) => is_destructive_words(&nested_command(rest)),
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
            "ds worktree rm feat",
            "ds worktree rm ./x --yes --force",
            "ds worktree 'rm' feat",
            "ds --color never worktree rm feat",
            "ds worktree --color=never rm feat",
            "/usr/local/bin/ds worktree rm feat",
            "sudo ds worktree rm feat",
            "cd x && ds worktree rm feat",
            "bash -c 'ds worktree rm feat'",
            "ds run -- ds worktree rm feat",
            "ds worktree \\\n  rm feat",
            "ds hook worktree-remove",
            "ds hook worktree-create",
            "ds hook pre-tool-use",
            "ds hook session-start",
            "ds hook",
            "echo '{}' | ds hook worktree-remove",
            "timeout 5 ds hook worktree-remove",
            "ds run -- ds hook worktree-remove",
            "timeout 5 ds rm",
            "timeout --foreground 10s ds prune",
            "nice -n 10 ds prune",
            "ionice -c 2 ds rm",
            "stdbuf -oL ds rm",
            "xargs ds rm",
            "ds \\\n  prune --yes",
            "if ds rm --yes; then echo done; fi",
            "until ds prune --yes; do sleep 1; done",
            "while ds rm; do :; done",
            "eval ds rm --yes",
            "builtin exec ds rm",
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
            "ds worktree",
            "ds worktree --help",
            "ds worktree -h",
            "ds worktree help",
            "ds run -- ds worktree --help",
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
            "ds hooks",
            "ds worktrees rm",
            "echo ds hook worktree-remove",
            "grep hook file",
            "xds rm",
            "ls /ds/rm",
            "echo ds rm",
            "rm -rf build",
            "grep ds rm file",
        ] {
            assert!(!is_destructive_ds_command(command), "{command}");
        }
    }

    #[test]
    fn should_name_the_subcommand_of_every_ds_command() {
        assert_eq!(ds_subcommands("ds up --no-run"), ["up"]);
        assert_eq!(
            ds_subcommands("cd x && ds info --json; ds reset"),
            ["info", "reset"]
        );
        assert_eq!(ds_subcommands("ds run -- ds reset"), ["run", "reset"]);
        assert_eq!(
            ds_subcommands("sudo /usr/local/bin/ds --color never up"),
            ["up"]
        );
        assert_eq!(
            ds_subcommands("bash -c 'ds up'")
                .first()
                .map(String::as_str),
            Some("up")
        );
    }

    #[test]
    fn should_name_nothing_when_no_ds_command_runs() {
        for command in ["ls", "echo ds up", "dsup", "xds up", "grep ds file", "ds"] {
            assert_eq!(ds_subcommands(command), Vec::<String>::new(), "{command}");
        }
    }

    #[test]
    fn should_see_ds_up_ds_run_and_ds_reset_as_environment_commands() {
        for command in [
            "ds up",
            "ds up --no-run",
            "ds run -- pnpm dev",
            "ds reset postgres",
            "cd x && ds up",
            "timeout 5 ds up",
            "\\ds 'up'",
        ] {
            assert!(is_environment_ds_command(command), "{command}");
        }
    }

    #[test]
    fn should_not_see_the_other_commands_as_environment_commands() {
        for command in [
            "ds info --json",
            "ds ensure",
            "ds doctor",
            "ds ls",
            "ds down",
            "upload",
            "echo ds",
        ] {
            assert!(!is_environment_ds_command(command), "{command}");
        }
    }
}
