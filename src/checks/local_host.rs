//! Spotting a value that hardcodes a port behind a local host, such as
//! `postgres://u:p@localhost:5432/app`.

const LOCAL_HOSTS: [&str; 4] = ["localhost", "127.0.0.1", "0.0.0.0", "[::1]"];

/// Whether `value` holds a local host followed by `:<port>` that is not
/// followed by another digit. The value is raw: `localhost:${DB_PORT}` does
/// not match.
pub fn hardcodes_local_port(value: &str, port: u16) -> bool {
    let lowercase = value.to_ascii_lowercase();
    let suffix = format!(":{port}");
    LOCAL_HOSTS
        .iter()
        .any(|host| has_host_with_suffix(&lowercase, host, &suffix))
}

/// The literal ports that follow a local host in `value`, in order of
/// appearance and without repeats. The value is raw: `localhost:${DB_PORT}`
/// holds none.
pub fn literal_local_ports(value: &str) -> Vec<u16> {
    let lowercase = value.to_ascii_lowercase();
    let mut found: Vec<(usize, u16)> = LOCAL_HOSTS
        .iter()
        .flat_map(|host| literal_ports_after_host(&lowercase, host))
        .collect();
    found.sort_unstable();
    let mut ports: Vec<u16> = Vec::new();
    for (_, port) in found {
        if !ports.contains(&port) {
            ports.push(port);
        }
    }
    ports
}

fn literal_ports_after_host(text: &str, host: &str) -> Vec<(usize, u16)> {
    text.match_indices(host)
        .filter(|(start, _)| {
            !text[..*start]
                .chars()
                .next_back()
                .is_some_and(continues_a_name)
        })
        .filter_map(|(start, _)| {
            let after = text[start + host.len()..].strip_prefix(':')?;
            let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
            Some((start, digits.parse().ok()?))
        })
        .collect()
}

fn has_host_with_suffix(text: &str, host: &str, suffix: &str) -> bool {
    text.match_indices(host).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = &text[start + host.len()..];
        !before.is_some_and(continues_a_name) && port_follows(after, suffix)
    })
}

fn continues_a_name(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
}

fn port_follows(after_host: &str, suffix: &str) -> bool {
    after_host
        .strip_prefix(suffix)
        .is_some_and(|rest| !rest.starts_with(|character: char| character.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn should_match_when_localhost_is_followed_by_the_default_port() {
        assert!(hardcodes_local_port(
            "postgres://u:p@localhost:5432/app",
            5432
        ));
    }

    #[test]
    fn should_match_each_local_host_spelling() {
        assert!(
            [
                "127.0.0.1:5432",
                "0.0.0.0:5432",
                "[::1]:5432",
                "LOCALHOST:5432"
            ]
            .iter()
            .all(|value| hardcodes_local_port(value, 5432))
        );
    }

    #[test]
    fn should_match_when_the_port_ends_the_value() {
        assert!(hardcodes_local_port("localhost:5432", 5432));
    }

    #[test]
    fn should_not_match_when_the_port_is_followed_by_a_digit() {
        assert!(!hardcodes_local_port("localhost:54320", 5432));
    }

    #[test]
    fn should_not_match_when_the_port_is_another_number() {
        assert!(!hardcodes_local_port("localhost:6543", 5432));
    }

    #[test]
    fn should_not_match_when_the_host_is_not_local() {
        assert!(!hardcodes_local_port("smtp.example.com:5432", 5432));
    }

    #[test]
    fn should_not_match_when_a_local_host_is_the_tail_of_a_longer_name() {
        assert!(!hardcodes_local_port("db.mylocalhost:5432", 5432));
    }

    #[test]
    fn should_not_match_when_the_port_is_a_substitution() {
        assert!(!hardcodes_local_port("localhost:${DB_PORT}", 5432));
    }

    #[test]
    fn should_not_match_when_the_port_has_no_colon() {
        assert!(!hardcodes_local_port("localhost 5432", 5432));
    }

    #[test]
    fn should_match_when_a_second_occurrence_has_the_port() {
        assert!(hardcodes_local_port("localhost:1 localhost:5432", 5432));
    }

    #[test]
    fn should_list_the_ports_in_order_when_several_hosts_have_one() {
        assert_eq!(
            literal_local_ports("http://localhost:3333 and 127.0.0.1:8080/x, localhost:3333"),
            vec![3333, 8080]
        );
    }

    #[test]
    fn should_list_nothing_when_the_port_is_a_substitution_or_the_host_is_not_local() {
        assert_eq!(
            literal_local_ports(
                "http://localhost:${WEB_PORT} db.mylocalhost:5432 smtp.example.com:25"
            ),
            Vec::<u16>::new()
        );
    }

    #[test]
    fn should_list_nothing_when_the_number_is_not_a_port() {
        assert_eq!(literal_local_ports("localhost:99999"), Vec::<u16>::new());
    }
}
