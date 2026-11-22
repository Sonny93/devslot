//! The global configuration, `config.toml` in the devslot config folder.
//!
//! Only `[slots]`, `[auto]`, `[write]`, `[worktrees]` and `[tunneler]` are modeled; other
//! keys and tables are accepted and ignored.

use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::IgnoredAny;
use thiserror::Error;

use crate::auto_scope::AutoScope;
use crate::port_range::PortRange;
use crate::project::bounded_read::{BoundedRead, Unreadable, read_bounded};
use crate::project_config::ProjectConfig;
use crate::require_scope::RequireScope;
use crate::tunneler::{TunnelerHost, TunnelerSetting};

/// The largest slot size a configuration may ask for.
pub const MAX_SLOT_SIZE: u16 = 100;

const DEFAULT_FIRST_PORT: u16 = 20000;
const DEFAULT_LAST_PORT: u16 = 29999;
const DEFAULT_SLOT_SIZE: u16 = 50;
const LOWEST_USABLE_PORT: i64 = 1024;
const HIGHEST_PORT: i64 = 65535;
const MINIMUM_SLOT_COUNT: i64 = 2;
const HOME_VARIABLE: &str = "HOME";
const HOME_ALONE: &str = "~";
const HOME_PREFIX: &str = "~/";
const DEFAULT_WORKTREES_SUBPATH: &str = "dev/.worktrees";

/// Why the global configuration cannot be used. These are tool failures.
#[derive(Debug, Error)]
pub enum GlobalConfigError {
    /// The file exists but cannot be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The file.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// The file is not a regular file or is larger than 1 MiB.
    #[error("cannot read {path}: {reason}")]
    Unusable {
        /// The file.
        path: PathBuf,
        /// "not a regular file" or "larger than 1 MiB".
        reason: &'static str,
    },
    /// The file is not valid TOML or has a value of the wrong type.
    #[error("cannot parse {path}: {message}")]
    Parse {
        /// The file.
        path: PathBuf,
        /// What the parser reports.
        message: String,
    },
    /// A value is out of bounds.
    #[error("invalid {key} in {path}: {reason}")]
    Invalid {
        /// The file.
        path: PathBuf,
        /// The key at fault, with its table.
        key: &'static str,
        /// What is wrong with it.
        reason: String,
    },
}

#[derive(Debug, Default, Deserialize)]
struct RawGlobalConfig {
    #[serde(default)]
    slots: RawSlots,
    #[serde(default)]
    auto: RawAuto,
    #[serde(default)]
    write: RawWrite,
    #[serde(default)]
    worktrees: RawWorktrees,
    tunneler: Option<RawTunneler>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawTunneler {
    Table(RawTunnelerTable),
    Other(IgnoredAny),
}

#[derive(Debug, Default, Deserialize)]
struct RawTunnelerTable {
    host: Option<RawTunnelerHost>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawTunnelerHost {
    Text(String),
    Other(IgnoredAny),
}

#[derive(Debug, Default, Deserialize)]
struct RawAuto {
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct RawWrite {
    enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct RawWorktrees {
    root: Option<String>,
    require: Option<Vec<String>>,
    require_exclude: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct RawSlots {
    first_port: Option<i64>,
    last_port: Option<i64>,
    size: Option<i64>,
}

/// How the port range is cut into slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotGeometry {
    first_port: u16,
    last_port: u16,
    size: u16,
}

impl Default for SlotGeometry {
    fn default() -> Self {
        Self {
            first_port: DEFAULT_FIRST_PORT,
            last_port: DEFAULT_LAST_PORT,
            size: DEFAULT_SLOT_SIZE,
        }
    }
}

impl SlotGeometry {
    /// How many ports a slot holds.
    pub fn size(self) -> usize {
        usize::from(self.size)
    }

    /// How many slots the range holds, slot 0 included.
    pub fn slot_count(self) -> u32 {
        (u32::from(self.last_port) - u32::from(self.first_port) + 1) / u32::from(self.size)
    }

    /// The ports of slot `slot`; `None` past the last slot.
    pub fn range_of(self, slot: u32) -> Option<PortRange> {
        if slot >= self.slot_count() {
            return None;
        }
        let first = u32::from(self.first_port) + u32::from(self.size) * slot;
        let last = first + u32::from(self.size) - 1;
        Some(PortRange {
            first: u16::try_from(first).ok()?,
            last: u16::try_from(last).ok()?,
        })
    }
}

/// The global configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalConfig {
    /// How the port range is cut into slots.
    pub slots: SlotGeometry,
    /// `[auto]`: the checkouts `ds ensure` may register.
    pub auto: AutoScope,
    /// `[write] enabled`: whether ds may write `.env`.
    pub write_enabled: bool,
    /// `[worktrees] root`, absolute, when the file sets it.
    pub worktrees_root: Option<PathBuf>,
    /// `[worktrees] require` and `require_exclude`: where agents must work in
    /// a linked worktree. Empty covers nothing.
    pub require: RequireScope,
    /// `[tunneler] host`, read leniently: a bad value never fails the load.
    pub tunneler: TunnelerSetting,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            slots: SlotGeometry::default(),
            auto: AutoScope::default(),
            write_enabled: true,
            worktrees_root: None,
            require: RequireScope::default(),
            tunneler: TunnelerSetting::NotSet,
        }
    }
}

impl GlobalConfig {
    /// Reads the file at `path`; a missing file gives the defaults. `~` in the
    /// file is expanded with the `HOME` variable.
    pub fn load(path: &Path) -> Result<Self, GlobalConfigError> {
        Self::load_with_home(path, home_from_environment().as_deref())
    }

    /// Reads the file at `path`, expanding `~` with `home`.
    pub fn load_with_home(path: &Path, home: Option<&Path>) -> Result<Self, GlobalConfigError> {
        let bytes = match read_bounded(path) {
            Ok(BoundedRead::Bytes(bytes)) => bytes,
            Ok(BoundedRead::Missing) => return Ok(Self::default()),
            Ok(BoundedRead::Unusable(Unreadable::NotRegularFile)) => {
                return Err(unusable(path, "not a regular file"));
            }
            Ok(BoundedRead::Unusable(Unreadable::TooLarge)) => {
                return Err(unusable(path, "larger than 1 MiB"));
            }
            Err(source) => {
                return Err(GlobalConfigError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        Self::from_text_with_home(&String::from_utf8_lossy(&bytes), path, home)
    }

    /// Parses the text of a config file read from `path`, expanding `~` with
    /// the `HOME` variable.
    pub fn from_text(text: &str, path: &Path) -> Result<Self, GlobalConfigError> {
        Self::from_text_with_home(text, path, home_from_environment().as_deref())
    }

    /// Parses the text of a config file read from `path`, expanding `~` with `home`.
    pub fn from_text_with_home(
        text: &str,
        path: &Path,
        home: Option<&Path>,
    ) -> Result<Self, GlobalConfigError> {
        let raw: RawGlobalConfig =
            toml_edit::de::from_str(text).map_err(|error| GlobalConfigError::Parse {
                path: path.to_path_buf(),
                message: error.message().to_owned(),
            })?;
        let invalid = |(key, reason): Violation| GlobalConfigError::Invalid {
            path: path.to_path_buf(),
            key,
            reason,
        };
        let require = require_scope_from(
            raw.worktrees.require.unwrap_or_default(),
            raw.worktrees.require_exclude.unwrap_or_default(),
        )
        .map_err(invalid)?;
        Ok(Self {
            slots: geometry_from(&raw.slots).map_err(invalid)?,
            auto: scope_from(raw.auto).map_err(invalid)?,
            write_enabled: raw.write.enabled.unwrap_or(true),
            worktrees_root: worktrees_root_from(raw.worktrees.root, home).map_err(invalid)?,
            require,
            tunneler: tunneler_from(raw.tunneler),
        })
    }

    /// Where worktrees are created: `[worktrees] root`, else `~/dev/.worktrees`.
    /// `None` when neither the file nor `home` gives a location.
    pub fn worktrees_root(&self, home: Option<&Path>) -> Option<PathBuf> {
        self.worktrees_root
            .clone()
            .or_else(|| home.map(|home| home.join(DEFAULT_WORKTREES_SUBPATH)))
    }
}

/// Whether ds may write `.env` of a checkout: `devslot.local.toml`, then
/// `devslot.toml` (already merged in `project`), then the global
/// configuration, which defaults to true.
pub fn effective_write_enabled(project: &ProjectConfig, global: &GlobalConfig) -> bool {
    project.write_enabled.unwrap_or(global.write_enabled)
}

/// The `HOME` directory when the variable holds an absolute path.
pub fn home_from_environment() -> Option<PathBuf> {
    std::env::var_os(HOME_VARIABLE)
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
}

fn unusable(path: &Path, reason: &'static str) -> GlobalConfigError {
    GlobalConfigError::Unusable {
        path: path.to_path_buf(),
        reason,
    }
}

type Violation = (&'static str, String);

fn scope_from(raw: RawAuto) -> Result<AutoScope, Violation> {
    let include = raw.include.unwrap_or_default();
    let exclude = raw.exclude.unwrap_or_default();
    check_patterns("[auto] include", &include)?;
    check_patterns("[auto] exclude", &exclude)?;
    Ok(AutoScope::new(include, exclude))
}

fn require_scope_from(
    require: Vec<String>,
    exclude: Vec<String>,
) -> Result<RequireScope, Violation> {
    check_patterns("[worktrees] require", &require)?;
    check_patterns("[worktrees] require_exclude", &exclude)?;
    Ok(RequireScope::new(require, exclude))
}

fn tunneler_from(raw: Option<RawTunneler>) -> TunnelerSetting {
    let table = match raw {
        None => return TunnelerSetting::NotSet,
        Some(RawTunneler::Table(table)) => table,
        Some(RawTunneler::Other(_)) => {
            return TunnelerSetting::Invalid {
                reason: "[tunneler] must be a table, not a single value".to_owned(),
            };
        }
    };
    match table.host {
        None => TunnelerSetting::NotSet,
        Some(RawTunnelerHost::Other(_)) => TunnelerSetting::Invalid {
            reason: "must be a string".to_owned(),
        },
        Some(RawTunnelerHost::Text(text)) => match TunnelerHost::parse(&text) {
            Ok(host) => TunnelerSetting::Host(host),
            Err(reason) => TunnelerSetting::Invalid { reason },
        },
    }
}

fn check_patterns(key: &'static str, patterns: &[String]) -> Result<(), Violation> {
    for pattern in patterns {
        AutoScope::check_pattern(pattern)
            .map_err(|reason| (key, format!("{pattern:?} is not a valid glob: {reason}")))?;
    }
    Ok(())
}

fn worktrees_root_from(
    root: Option<String>,
    home: Option<&Path>,
) -> Result<Option<PathBuf>, Violation> {
    const KEY: &str = "[worktrees] root";
    let Some(root) = root else {
        return Ok(None);
    };
    let expanded = if root == HOME_ALONE {
        home.map(Path::to_path_buf)
    } else if let Some(rest) = root.strip_prefix(HOME_PREFIX) {
        home.map(|home| home.join(rest))
    } else {
        Some(PathBuf::from(&root))
    };
    match expanded {
        Some(path) if path.is_absolute() => Ok(Some(path)),
        None => Err((KEY, "starts with ~ but HOME is not set".to_owned())),
        Some(_) => Err((KEY, "must be an absolute path, or start with ~/".to_owned())),
    }
}

fn geometry_from(raw: &RawSlots) -> Result<SlotGeometry, Violation> {
    let defaults = SlotGeometry::default();
    let first = raw
        .first_port
        .unwrap_or_else(|| i64::from(defaults.first_port));
    let last = raw
        .last_port
        .unwrap_or_else(|| i64::from(defaults.last_port));
    let size = raw.size.unwrap_or_else(|| i64::from(defaults.size));
    check_size(size)?;
    check_port("[slots] first_port", first)?;
    check_port("[slots] last_port", last)?;
    if first > last {
        return Err((
            "[slots] last_port",
            format!("must not be lower than first_port ({first})"),
        ));
    }
    if (last - first + 1) / size < MINIMUM_SLOT_COUNT {
        return Err((
            "[slots] size",
            format!(
                "the range {first}-{last} must hold at least {MINIMUM_SLOT_COUNT} slots of {size} ports (slot 0 is reserved)"
            ),
        ));
    }
    Ok(SlotGeometry {
        first_port: to_port(first)?,
        last_port: to_port(last)?,
        size: to_port(size)?,
    })
}

fn check_size(size: i64) -> Result<(), Violation> {
    if (1..=i64::from(MAX_SLOT_SIZE)).contains(&size) {
        return Ok(());
    }
    Err((
        "[slots] size",
        format!("must be between 1 and {MAX_SLOT_SIZE}"),
    ))
}

fn check_port(key: &'static str, port: i64) -> Result<(), Violation> {
    if (LOWEST_USABLE_PORT..=HIGHEST_PORT).contains(&port) {
        return Ok(());
    }
    Err((
        key,
        format!("must be between {LOWEST_USABLE_PORT} and {HIGHEST_PORT}"),
    ))
}

fn to_port(value: i64) -> Result<u16, Violation> {
    u16::try_from(value).map_err(|_| ("[slots]", "value out of range".to_owned()))
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn parse(text: &str) -> Result<GlobalConfig, GlobalConfigError> {
        GlobalConfig::from_text(text, Path::new("/c/config.toml"))
    }

    fn invalid_key(text: &str) -> &'static str {
        match parse(text).unwrap_err() {
            GlobalConfigError::Invalid { key, .. } => key,
            other => panic!("unexpected error {other}"),
        }
    }

    fn tunneler_of(text: &str) -> TunnelerSetting {
        parse(text).unwrap().tunneler
    }

    fn tunneler_reason(text: &str) -> String {
        match tunneler_of(text) {
            TunnelerSetting::Invalid { reason } => reason,
            other => panic!("unexpected setting {other:?}"),
        }
    }

    #[test]
    fn should_not_set_the_tunneler_when_the_table_is_absent() {
        assert_eq!(tunneler_of(""), TunnelerSetting::NotSet);
    }

    #[test]
    fn should_not_set_the_tunneler_when_the_table_has_no_host() {
        assert_eq!(tunneler_of("[tunneler]\n"), TunnelerSetting::NotSet);
    }

    #[test]
    fn should_read_the_tunneler_host_when_it_is_a_valid_alias() {
        assert_eq!(
            tunneler_of("[tunneler]\nhost = \"vega\"\n"),
            TunnelerSetting::Host(TunnelerHost::parse("vega").unwrap())
        );
    }

    #[test]
    fn should_flag_the_tunneler_host_invalid_when_it_is_outside_the_bounds() {
        let too_long = "a".repeat(65);
        let values = [
            "",
            too_long.as_str(),
            "-vega",
            ".vega",
            "ve ga",
            "ve/ga",
            "me@vega",
        ];

        for value in values {
            let text = format!("[tunneler]\nhost = \"{value}\"\n");
            assert!(
                matches!(tunneler_of(&text), TunnelerSetting::Invalid { .. }),
                "{value:?} was accepted"
            );
        }
    }

    #[test]
    fn should_flag_the_tunneler_host_invalid_when_it_is_not_a_string() {
        assert_eq!(
            tunneler_reason("[tunneler]\nhost = 3\n"),
            "must be a string"
        );
    }

    #[test]
    fn should_flag_the_tunneler_invalid_when_it_is_not_a_table() {
        assert_eq!(
            tunneler_reason("tunneler = \"x\"\n"),
            "[tunneler] must be a table, not a single value"
        );
    }

    #[test]
    fn should_keep_reading_the_rest_of_the_file_when_the_tunneler_value_is_invalid() {
        let config = parse("tunneler = \"x\"\n[worktrees]\nrequire = [\"/work/*\"]\n").unwrap();

        assert!(!config.require.is_empty());
    }

    #[test]
    fn should_use_the_defaults_when_the_file_is_missing() {
        let directory = tempfile::tempdir().unwrap();

        let config = GlobalConfig::load(&directory.path().join("config.toml")).unwrap();

        assert_eq!(config, GlobalConfig::default());
    }

    #[test]
    fn should_cut_the_default_range_into_200_slots_when_nothing_is_configured() {
        assert_eq!(SlotGeometry::default().slot_count(), 200);
    }

    #[test]
    fn should_place_slot_one_at_20050_when_the_geometry_is_the_default() {
        assert_eq!(
            SlotGeometry::default().range_of(1),
            Some(PortRange {
                first: 20050,
                last: 20099
            })
        );
    }

    #[test]
    fn should_have_no_slot_past_the_last_one() {
        assert_eq!(SlotGeometry::default().range_of(200), None);
    }

    #[test]
    fn should_read_the_slots_table_when_it_is_set() {
        let config = parse("[slots]\nfirst_port = 40000\nlast_port = 40999\nsize = 10\n").unwrap();

        assert_eq!((config.slots.slot_count(), config.slots.size()), (100, 10));
    }

    #[test]
    fn should_ignore_the_other_tables_when_the_file_has_them() {
        let config = parse("[mobile]\nname = \"app\"\n[auto]\ninclude = []\n").unwrap();

        assert_eq!(config, GlobalConfig::default());
    }

    fn parse_with_home(text: &str) -> Result<GlobalConfig, GlobalConfigError> {
        GlobalConfig::from_text_with_home(
            text,
            Path::new("/c/config.toml"),
            Some(Path::new("/home/me")),
        )
    }

    #[test]
    fn should_default_to_everywhere_writing_enabled_and_no_worktrees_root() {
        let config = parse("").unwrap();

        assert_eq!(
            (
                config.auto.covers(Path::new("/x"), None),
                config.write_enabled
            ),
            (true, true)
        );
        assert_eq!(config, GlobalConfig::default());
    }

    #[test]
    fn should_read_the_auto_lists() {
        let config =
            parse("[auto]\ninclude = [\"/work/*\"]\nexclude = [\"/work/vendor\"]\n").unwrap();

        assert_eq!(
            (
                config.auto.covers(Path::new("/work/app"), None),
                config.auto.covers(Path::new("/work/vendor"), None),
                config.auto.covers(Path::new("/other"), None)
            ),
            (true, false, false)
        );
    }

    #[test]
    fn should_refuse_an_invalid_glob_in_include_naming_the_key() {
        assert_eq!(
            invalid_key("[auto]\ninclude = [\"/a/[b\"]\n"),
            "[auto] include"
        );
    }

    #[test]
    fn should_refuse_an_invalid_glob_in_exclude_naming_the_key() {
        assert_eq!(
            invalid_key("[auto]\nexclude = [\"{a\"]\n"),
            "[auto] exclude"
        );
    }

    #[test]
    fn should_report_a_parse_error_when_auto_include_is_not_a_list() {
        assert!(matches!(
            parse("[auto]\ninclude = \"/work\"\n").unwrap_err(),
            GlobalConfigError::Parse { .. }
        ));
    }

    #[test]
    fn should_read_the_global_write_setting() {
        assert!(!parse("[write]\nenabled = false\n").unwrap().write_enabled);
    }

    #[test]
    fn should_expand_the_home_folder_of_the_worktrees_root() {
        let config = parse_with_home("[worktrees]\nroot = \"~/trees\"\n").unwrap();

        assert_eq!(config.worktrees_root, Some(PathBuf::from("/home/me/trees")));
    }

    #[test]
    fn should_accept_an_absolute_worktrees_root() {
        let config = parse_with_home("[worktrees]\nroot = \"/srv/trees\"\n").unwrap();

        assert_eq!(config.worktrees_root, Some(PathBuf::from("/srv/trees")));
    }

    #[test]
    fn should_refuse_a_relative_worktrees_root_naming_the_key() {
        assert_eq!(
            match parse_with_home("[worktrees]\nroot = \"trees\"\n").unwrap_err() {
                GlobalConfigError::Invalid { key, .. } => key,
                other => panic!("unexpected error {other}"),
            },
            "[worktrees] root"
        );
    }

    #[test]
    fn should_refuse_a_home_worktrees_root_when_home_is_unknown() {
        let outcome = GlobalConfig::from_text_with_home(
            "[worktrees]\nroot = \"~/trees\"\n",
            Path::new("/c/config.toml"),
            None,
        );

        assert!(matches!(outcome, Err(GlobalConfigError::Invalid { .. })));
    }

    #[test]
    fn should_default_the_worktrees_root_below_home() {
        assert_eq!(
            GlobalConfig::default().worktrees_root(Some(Path::new("/home/me"))),
            Some(PathBuf::from("/home/me/dev/.worktrees"))
        );
    }

    #[test]
    fn should_prefer_the_configured_worktrees_root_to_the_default() {
        let config = parse_with_home("[worktrees]\nroot = \"/srv/trees\"\n").unwrap();

        assert_eq!(
            config.worktrees_root(Some(Path::new("/home/me"))),
            Some(PathBuf::from("/srv/trees"))
        );
    }

    #[test]
    fn should_read_the_require_lists() {
        let config =
            parse("[worktrees]\nrequire = [\"/work/*\"]\nrequire_exclude = [\"/work/scratch\"]\n")
                .unwrap();

        assert_eq!(
            (
                config.require.covers(Path::new("/work/app"), None),
                config.require.covers(Path::new("/work/scratch"), None),
                config.require.covers(Path::new("/other"), None)
            ),
            (true, false, false)
        );
    }

    #[test]
    fn should_refuse_an_invalid_glob_in_require_naming_the_key() {
        assert_eq!(
            invalid_key("[worktrees]\nrequire = [\"/a/[b\"]\n"),
            "[worktrees] require"
        );
    }

    #[test]
    fn should_refuse_an_invalid_glob_in_require_exclude_naming_the_key() {
        assert_eq!(
            invalid_key("[worktrees]\nrequire_exclude = [\"{a\"]\n"),
            "[worktrees] require_exclude"
        );
    }

    #[test]
    fn should_let_the_project_setting_win_over_the_global_one() {
        let global = parse("[write]\nenabled = false\n").unwrap();
        let project = ProjectConfig {
            write_enabled: Some(true),
            ..ProjectConfig::default()
        };

        assert!(effective_write_enabled(&project, &global));
    }

    #[test]
    fn should_fall_back_to_the_global_write_setting_when_the_project_sets_none() {
        let global = parse("[write]\nenabled = false\n").unwrap();

        assert!(!effective_write_enabled(&ProjectConfig::default(), &global));
    }

    #[test]
    fn should_write_by_default_when_nothing_sets_it() {
        assert!(effective_write_enabled(
            &ProjectConfig::default(),
            &GlobalConfig::default()
        ));
    }

    #[test]
    fn should_refuse_a_size_above_the_maximum_naming_the_key() {
        assert_eq!(invalid_key("[slots]\nsize = 101\n"), "[slots] size");
    }

    #[test]
    fn should_refuse_a_size_of_zero_naming_the_key() {
        assert_eq!(invalid_key("[slots]\nsize = 0\n"), "[slots] size");
    }

    #[test]
    fn should_refuse_a_first_port_below_1024_naming_the_key() {
        assert_eq!(
            invalid_key("[slots]\nfirst_port = 80\n"),
            "[slots] first_port"
        );
    }

    #[test]
    fn should_refuse_a_last_port_above_65535_naming_the_key() {
        assert_eq!(
            invalid_key("[slots]\nlast_port = 70000\n"),
            "[slots] last_port"
        );
    }

    #[test]
    fn should_refuse_a_range_when_the_last_port_is_below_the_first() {
        assert_eq!(
            invalid_key("[slots]\nfirst_port = 30000\nlast_port = 29999\n"),
            "[slots] last_port"
        );
    }

    #[test]
    fn should_refuse_a_range_that_holds_a_single_slot() {
        assert_eq!(
            invalid_key("[slots]\nfirst_port = 20000\nlast_port = 20099\nsize = 100\n"),
            "[slots] size"
        );
    }

    #[test]
    fn should_accept_a_range_that_holds_exactly_two_slots() {
        assert!(parse("[slots]\nfirst_port = 20000\nlast_port = 20099\nsize = 50\n").is_ok());
    }

    #[test]
    fn should_report_a_parse_error_naming_the_file_when_the_toml_is_invalid() {
        assert!(matches!(
            parse("[slots\n").unwrap_err(),
            GlobalConfigError::Parse { .. }
        ));
    }

    #[test]
    fn should_report_a_parse_error_when_a_value_has_the_wrong_type() {
        assert!(matches!(
            parse("[slots]\nsize = \"big\"\n").unwrap_err(),
            GlobalConfigError::Parse { .. }
        ));
    }
}
