//! The check on `[tunneler] host` in the global configuration.

use std::path::Path;

use super::finding::{Check, Finding, Level};
use crate::tunneler::TunnelerSetting;

const HOST_KEY: &str = "tunneler.host";

/// Reports a `[tunneler] host` that cannot be used. The rest of devslot keeps
/// working without the Tunneler hint.
pub fn check_tunneler(setting: &TunnelerSetting, config_file: &Path) -> Option<Finding> {
    let TunnelerSetting::Invalid { reason } = setting else {
        return None;
    };
    Some(
        Finding::new(
            Level::Error,
            Check::TunnelerHostInvalid,
            format!(
                "invalid [tunneler] host in {}: {reason}; ds info leaves the Tunneler hint out until it is fixed",
                config_file.display()
            ),
        )
        .for_key(HOST_KEY),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tunneler::TunnelerHost;

    #[test]
    fn should_report_an_error_naming_the_key_and_the_reason_when_the_host_is_invalid() {
        let setting = TunnelerSetting::Invalid {
            reason: "is empty".to_owned(),
        };

        let finding = check_tunneler(&setting, Path::new("/c/config.toml")).unwrap();

        assert_eq!(finding.level, Level::Error);
        assert_eq!(finding.check, Check::TunnelerHostInvalid);
        assert_eq!(finding.key.as_deref(), Some("tunneler.host"));
        assert!(
            finding
                .message
                .starts_with("invalid [tunneler] host in /c/config.toml: is empty")
        );
    }

    #[test]
    fn should_report_nothing_when_the_host_is_not_set_or_valid() {
        let host = TunnelerSetting::Host(TunnelerHost::parse("vega").unwrap());

        assert_eq!(
            check_tunneler(&TunnelerSetting::NotSet, Path::new("/c")),
            None
        );
        assert_eq!(check_tunneler(&host, Path::new("/c")), None);
    }
}
