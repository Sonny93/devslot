//! `${NAME}` references in `[env]` values.

use crate::project_config::is_valid_variable_name;

const REFERENCE_START: &str = "${";
const REFERENCE_END: char = '}';

/// Replaces every `${NAME}` (a valid variable name in braces) with what
/// `resolve` gives. Any other `$` is left as it is. Returns the first name
/// `resolve` does not know.
pub fn substitute_references(
    text: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(REFERENCE_START) {
        output.push_str(&rest[..start]);
        let after = &rest[start + REFERENCE_START.len()..];
        let name = after
            .find(REFERENCE_END)
            .map(|end| &after[..end])
            .filter(|name| is_valid_variable_name(name));
        let Some(name) = name else {
            output.push_str(REFERENCE_START);
            rest = after;
            continue;
        };
        output.push_str(&resolve(name).ok_or_else(|| name.to_owned())?);
        rest = &after[name.len() + REFERENCE_END.len_utf8()..];
    }
    output.push_str(rest);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(name: &str) -> Option<String> {
        (name == "PORT").then(|| "20050".to_owned())
    }

    #[test]
    fn should_replace_a_known_reference() {
        assert_eq!(
            substitute_references("a:${PORT}:b", &known).unwrap(),
            "a:20050:b"
        );
    }

    #[test]
    fn should_return_the_unknown_name() {
        assert_eq!(
            substitute_references("${NOPE}", &known).unwrap_err(),
            "NOPE"
        );
    }

    #[test]
    fn should_leave_other_dollar_signs_alone() {
        assert_eq!(
            substitute_references("$PORT ${A:-b} $$ ${", &known).unwrap(),
            "$PORT ${A:-b} $$ ${"
        );
    }
}
