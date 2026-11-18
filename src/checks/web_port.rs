//! The check on `[web] port`.

use super::finding::{Check, Finding, Level};
use crate::discovery::{ManagedPort, PortKind};

const WEB_PORT_KEY: &str = "[web] port";

/// Warns when `[web] port` names a variable that is not a managed web port of
/// the checkout: no url moves to the host of the instance.
pub fn check_web_port(web_port: Option<&str>, managed: &[ManagedPort]) -> Option<Finding> {
    let variable = web_port?;
    let is_managed_web_port = managed
        .iter()
        .any(|port| port.variable == variable && port.kind == PortKind::Web);
    if is_managed_web_port {
        return None;
    }
    Some(
        Finding::new(
            Level::Warning,
            Check::WebPortUnmanaged,
            format!(
                "[web] port names {variable}, which is not a managed web port of this checkout; every url stays on localhost"
            ),
        )
        .for_key(WEB_PORT_KEY),
    )
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::discovery::PortSource;

    fn managed(variable: &str, kind: PortKind) -> ManagedPort {
        ManagedPort {
            variable: variable.to_owned(),
            kind,
            default: None,
            source: PortSource::Compose,
            file: "compose.yml".to_owned(),
            service: None,
            target: None,
        }
    }

    #[test]
    fn should_report_nothing_when_no_web_port_is_set() {
        assert_eq!(
            check_web_port(None, &[managed("PORT", PortKind::Web)]),
            None
        );
    }

    #[test]
    fn should_report_nothing_when_the_variable_is_a_managed_web_port() {
        assert_eq!(
            check_web_port(Some("PORT"), &[managed("PORT", PortKind::Web)]),
            None
        );
    }

    #[test]
    fn should_warn_naming_the_key_when_the_variable_is_not_managed() {
        let finding = check_web_port(Some("NOPE_PORT"), &[managed("PORT", PortKind::Web)]).unwrap();

        assert_eq!(
            (finding.level, finding.check, finding.key.as_deref()),
            (Level::Warning, Check::WebPortUnmanaged, Some("[web] port"))
        );
        assert!(finding.message.contains("NOPE_PORT"));
    }

    #[test]
    fn should_warn_when_the_variable_is_a_service_port() {
        let finding = check_web_port(Some("DB_PORT"), &[managed("DB_PORT", PortKind::Service)]);

        assert_eq!(
            finding.map(|finding| finding.check),
            Some(Check::WebPortUnmanaged)
        );
    }
}
