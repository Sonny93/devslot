//! The local host name of an instance: `<label>.localhost`.
//!
//! Browsers keep cookies per host, not per port, so two instances on
//! `localhost` share them. `*.localhost` resolves to the loopback address, so
//! each instance gets its own label. The label is computed once, when the
//! checkout is registered, and stored: an existing instance never changes
//! host. Only `[a-z0-9-]` and the dot can appear in a host.

/// What follows the label in a host name.
pub const HOST_SUFFIX: &str = ".localhost";
/// The longest a DNS label may be.
pub const MAX_LABEL_LENGTH: usize = 63;

const FALLBACK_LABEL: &str = "devslot";
const SEPARATOR: char = '-';

/// The label an instance name gives: lowercase letters, digits and `-`, runs
/// of `-` collapsed, none at either end, at most [`MAX_LABEL_LENGTH`]
/// characters. A name too long keeps its last segment (the path hash) and
/// loses the end of what comes before it.
pub fn label_from_name(name: &str) -> String {
    let mut reduced = String::with_capacity(name.len());
    for character in name.to_lowercase().chars() {
        let is_allowed = character.is_ascii_lowercase() || character.is_ascii_digit();
        let kept = if is_allowed { character } else { SEPARATOR };
        if kept == SEPARATOR && reduced.ends_with(SEPARATOR) {
            continue;
        }
        reduced.push(kept);
    }
    let trimmed = reduced.trim_matches(SEPARATOR);
    if trimmed.is_empty() {
        return FALLBACK_LABEL.to_owned();
    }
    truncate_label(trimmed, MAX_LABEL_LENGTH)
}

/// The label of the instance `name` in slot `slot`: the label of its name, or
/// that label followed by the slot when `is_taken` says another instance
/// already has it.
pub fn pick_label(name: &str, slot: u32, is_taken: impl Fn(&str) -> bool) -> String {
    let base = label_from_name(name);
    if !is_taken(&base) {
        return base;
    }
    let mut attempt = 0;
    loop {
        let candidate = suffixed_label(&base, slot, attempt);
        if !is_taken(&candidate) {
            return candidate;
        }
        attempt += 1;
    }
}

/// The host name of `label`.
pub fn host_of_label(label: &str) -> String {
    format!("{label}{HOST_SUFFIX}")
}

/// Whether `host` is `<label>.localhost` with a label of `[a-z0-9-]`, at most
/// [`MAX_LABEL_LENGTH`] characters, not starting or ending with `-`.
pub fn is_valid_host(host: &str) -> bool {
    host.strip_suffix(HOST_SUFFIX).is_some_and(is_valid_label)
}

fn is_valid_label(label: &str) -> bool {
    let is_allowed = |character: char| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == SEPARATOR
    };
    !label.is_empty()
        && label.len() <= MAX_LABEL_LENGTH
        && !label.starts_with(SEPARATOR)
        && !label.ends_with(SEPARATOR)
        && label.chars().all(is_allowed)
}

fn suffixed_label(base: &str, slot: u32, attempt: u32) -> String {
    let suffix = if attempt == 0 {
        format!("{SEPARATOR}{slot}")
    } else {
        format!("{SEPARATOR}{slot}{SEPARATOR}{attempt}")
    };
    let room = MAX_LABEL_LENGTH.saturating_sub(suffix.len());
    format!("{}{suffix}", truncate_label(base, room))
}

fn truncate_label(label: &str, max: usize) -> String {
    if label.len() <= max {
        return label.to_owned();
    }
    let Some((prefix, hash)) = label.rsplit_once(SEPARATOR) else {
        return trim_end(&label[..max]);
    };
    if hash.len() + 1 >= max {
        return trim_end(&label[..max]);
    }
    let budget = max - hash.len() - 1;
    let kept = trim_end(&prefix[..budget.min(prefix.len())]);
    format!("{kept}{SEPARATOR}{hash}")
}

fn trim_end(text: &str) -> String {
    text.trim_end_matches(SEPARATOR).to_owned()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn nothing_taken(_: &str) -> bool {
        false
    }

    #[test]
    fn should_keep_a_plain_name_as_its_label() {
        assert_eq!(
            label_from_name("my-links-feat-auth-3f2a"),
            "my-links-feat-auth-3f2a"
        );
    }

    #[test]
    fn should_replace_underscores_with_dashes_when_the_name_has_them() {
        assert_eq!(label_from_name("my_app-feat_x-3f2a"), "my-app-feat-x-3f2a");
    }

    #[test]
    fn should_collapse_and_trim_dashes_when_the_name_has_runs_of_separators() {
        assert_eq!(label_from_name("_my__app--x_-3f2a_"), "my-app-x-3f2a");
    }

    #[test]
    fn should_use_the_fallback_label_when_nothing_is_left() {
        assert_eq!(label_from_name("__--"), "devslot");
    }

    #[test]
    fn should_replace_characters_outside_the_name_alphabet_when_the_name_was_edited_by_hand() {
        assert_eq!(
            label_from_name("My App.evil/x:1-3f2a"),
            "my-app-evil-x-1-3f2a"
        );
    }

    #[test]
    fn should_keep_the_hash_and_cut_the_rest_when_the_name_is_longer_than_a_label() {
        let name = format!("{}-feature-3f2a", "a".repeat(80));

        let label = label_from_name(&name);

        assert_eq!(label.len(), MAX_LABEL_LENGTH);
        assert!(label.ends_with("-3f2a"));
        assert!(label.starts_with("aaaa"));
    }

    #[test]
    fn should_trim_a_trailing_dash_of_the_cut_part() {
        let name = format!("{}-x-3f2a", "a".repeat(57));

        assert_eq!(label_from_name(&name), format!("{}-3f2a", "a".repeat(57)));
    }

    #[test]
    fn should_cut_the_whole_label_when_the_last_segment_leaves_no_room() {
        let name = format!("repo-{}", "f".repeat(70));

        let label = label_from_name(&name);

        assert_eq!(label, format!("repo-{}", "f".repeat(58)));
    }

    #[test]
    fn should_pick_the_plain_label_when_nobody_has_it() {
        assert_eq!(pick_label("app-3f2a", 4, nothing_taken), "app-3f2a");
    }

    #[test]
    fn should_append_the_slot_when_another_instance_has_the_label() {
        let taken = |label: &str| label == "app-3f2a";

        assert_eq!(pick_label("app-3f2a", 4, taken), "app-3f2a-4");
    }

    #[test]
    fn should_keep_the_suffixed_label_within_a_label_when_the_name_is_long() {
        let name = format!("{}-3f2a", "a".repeat(80));
        let plain = label_from_name(&name);
        let taken = |label: &str| label == plain;

        let label = pick_label(&name, 12, taken);

        assert_eq!(label.len(), MAX_LABEL_LENGTH);
        assert!(label.ends_with("-3f2a-12"));
    }

    #[test]
    fn should_try_again_when_the_suffixed_label_is_taken_too() {
        let taken = |label: &str| label == "app-3f2a" || label == "app-3f2a-4";

        assert_eq!(pick_label("app-3f2a", 4, taken), "app-3f2a-4-1");
    }

    #[test]
    fn should_build_the_host_from_the_label() {
        assert_eq!(host_of_label("app-3f2a"), "app-3f2a.localhost");
    }

    #[test]
    fn should_accept_a_host_made_of_a_valid_label() {
        assert!(is_valid_host("app-3f2a.localhost"));
    }

    #[test]
    fn should_refuse_a_host_that_is_not_a_single_localhost_label() {
        let candidates = [
            "app.example.com",
            "app.localhost.evil.com",
            "a.b.localhost",
            ".localhost",
            "-app.localhost",
            "app-.localhost",
            "App.localhost",
            "app_x.localhost",
            "app:80.localhost",
            "localhost",
        ];

        let accepted: Vec<&str> = candidates
            .into_iter()
            .filter(|host| is_valid_host(host))
            .collect();

        assert_eq!(accepted, Vec::<&str>::new());
    }

    #[test]
    fn should_refuse_a_label_longer_than_sixty_three_characters() {
        let host = format!("{}.localhost", "a".repeat(64));

        assert!(!is_valid_host(&host));
    }
}
