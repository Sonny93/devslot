//! The hint that opens an instance from the user's computer through Tunneler:
//! a command and a `tunneler://` link, printed text only. devslot never calls
//! Tunneler. The bounds on both segments are shared with Tunneler.

use std::fmt::Write as _;

use serde::Serialize;

/// The longest the SSH alias of the server may be.
pub const MAX_HOST_LENGTH: usize = 64;
/// The longest an instance name may be to get a hint.
pub const MAX_INSTANCE_LENGTH: usize = 255;

const LINK_PREFIX: &str = "tunneler://devslot";
const COMMAND_PREFIX: &str = "tunneler devslot";
const SEGMENT_SEPARATORS: [char; 3] = ['.', '_', '-'];
const UNRESERVED_SEPARATORS: &[u8] = b"-._~";

/// What `[tunneler]` in the global configuration says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelerSetting {
    /// No `[tunneler] host`: the integration is off.
    NotSet,
    /// A valid alias.
    Host(TunnelerHost),
    /// The value cannot be used; the rest of the configuration still loads.
    Invalid {
        /// What is wrong with it.
        reason: String,
    },
}

/// The SSH alias of the server, as the user's computer knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelerHost(String);

impl TunnelerHost {
    /// Checks `value` against the bounds shared with Tunneler.
    pub fn parse(value: &str) -> Result<Self, String> {
        check_segment(value, MAX_HOST_LENGTH)?;
        Ok(Self(value.to_owned()))
    }
}

/// The command and the link that open an instance through Tunneler, in the
/// shape `ds info --json` prints under `tunneler`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TunnelerHint {
    /// The command to run on the user's computer.
    pub command: String,
    /// The link that opens the same thing.
    pub url: String,
}

/// The hint for the instance `instance_name`; `None` when the name is outside
/// the bounds shared with Tunneler.
pub fn tunneler_hint(host: &TunnelerHost, instance_name: &str) -> Option<TunnelerHint> {
    check_segment(instance_name, MAX_INSTANCE_LENGTH).ok()?;
    Some(TunnelerHint {
        command: format!("{COMMAND_PREFIX} {} {instance_name}", host.0),
        url: format!(
            "{LINK_PREFIX}/{}/{}",
            percent_encode(&host.0),
            percent_encode(instance_name)
        ),
    })
}

fn check_segment(value: &str, maximum: usize) -> Result<(), String> {
    let Some(first) = value.chars().next() else {
        return Err("is empty".to_owned());
    };
    if value.chars().count() > maximum {
        return Err(format!("is longer than {maximum} characters"));
    }
    if !first.is_ascii_alphanumeric() {
        return Err("must start with a letter or a digit".to_owned());
    }
    let is_allowed = |character: char| {
        character.is_ascii_alphanumeric() || SEGMENT_SEPARATORS.contains(&character)
    };
    if !value.chars().all(is_allowed) {
        return Err("may only hold letters, digits, '.', '_' and '-'".to_owned());
    }
    Ok(())
}

fn percent_encode(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || UNRESERVED_SEPARATORS.contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn host(alias: &str) -> TunnelerHost {
        TunnelerHost::parse(alias).unwrap()
    }

    #[test]
    fn should_accept_an_alias_made_of_letters_digits_and_separators() {
        assert_eq!(
            TunnelerHost::parse("vega-2.lan_x"),
            Ok(host("vega-2.lan_x"))
        );
    }

    #[test]
    fn should_reject_an_alias_outside_the_bounds() {
        let too_long = "a".repeat(MAX_HOST_LENGTH + 1);

        for value in [
            "",
            too_long.as_str(),
            "-vega",
            ".vega",
            "ve ga",
            "ve/ga",
            "me@vega",
        ] {
            assert!(
                TunnelerHost::parse(value).is_err(),
                "{value:?} was accepted"
            );
        }
    }

    #[test]
    fn should_accept_an_alias_of_the_longest_length() {
        assert!(TunnelerHost::parse(&"a".repeat(MAX_HOST_LENGTH)).is_ok());
    }

    #[test]
    fn should_give_the_command_and_the_link_when_the_name_is_plain() {
        let hint = tunneler_hint(&host("vega"), "web-shop-098a").unwrap();

        assert_eq!(hint.command, "tunneler devslot vega web-shop-098a");
        assert_eq!(hint.url, "tunneler://devslot/vega/web-shop-098a");
    }

    #[test]
    fn should_give_no_hint_when_the_name_has_a_character_outside_the_set() {
        assert_eq!(tunneler_hint(&host("vega"), "web shop"), None);
    }

    #[test]
    fn should_give_no_hint_when_the_name_starts_with_a_dash() {
        assert_eq!(tunneler_hint(&host("vega"), "-web"), None);
    }

    #[test]
    fn should_give_no_hint_when_the_name_is_too_long() {
        let name = "a".repeat(MAX_INSTANCE_LENGTH + 1);

        assert_eq!(tunneler_hint(&host("vega"), &name), None);
    }

    #[test]
    fn should_give_a_hint_when_the_name_has_the_longest_length() {
        let name = "a".repeat(MAX_INSTANCE_LENGTH);

        assert!(tunneler_hint(&host("vega"), &name).is_some());
    }

    #[test]
    fn should_encode_a_space_and_a_non_ascii_character_per_utf8_byte() {
        assert_eq!(percent_encode("a b~é.-_"), "a%20b~%C3%A9.-_");
    }
}
