//! Keeping the devslot block of a checkout's `.env` in line with the registry:
//! creating `.env` when it is missing, writing the block when its text
//! changes, and telling what the checks say before any write. The writes
//! assume the caller holds the registry lock.

use std::path::PathBuf;

use thiserror::Error;

use crate::analysis::EnvFiles;
use crate::block_entries::{BlockEntriesError, block_entries};
use crate::checkout::Checkout;
use crate::checks::env::{EnvCheckInput, check_env};
use crate::checks::finding::{Check, Finding, Level};
use crate::compose::selection::DevComposeSelection;
use crate::error::ToolError;
use crate::project::dotenv_block::{BlockError, split_block};
use crate::project::env_block_render::{RenderError, place_block, render_block};
use crate::project::env_file::{
    ENV_FILE, EnvContent, EnvFileError, create_env, env_source, read_env, write_env,
};
use crate::project_config::EnvSetting;
use crate::registry::record::InstanceRecord;

const BLOCKING_CHECKS: [Check; 5] = [
    Check::EnvMissing,
    Check::EnvNotIgnored,
    Check::EnvUnreadable,
    Check::EnvUnparsable,
    Check::EnvBlockMalformed,
];

/// Why the block cannot be written.
#[derive(Debug, Error)]
pub enum EnvSyncError {
    /// `.env` is missing and there is nothing to create it from.
    #[error(
        "there is no .env, and neither the main checkout's .env nor a .env.example to create it from"
    )]
    MissingSource,
    /// `.env` cannot be read or replaced.
    #[error(transparent)]
    File(#[from] EnvFileError),
    /// The devslot block of `.env` is malformed.
    #[error(".env: {0}")]
    Block(#[from] BlockError),
    /// A value cannot be written to `.env`.
    #[error(transparent)]
    Render(#[from] RenderError),
    /// The lines of the block cannot be built.
    #[error(transparent)]
    Entries(#[from] BlockEntriesError),
}

/// What the block of one instance is built from.
pub struct BlockTarget<'a> {
    /// The checkout whose `.env` is written.
    pub checkout: &'a Checkout,
    /// The registered instance.
    pub instance: &'a InstanceRecord,
    /// The dev compose of the checkout.
    pub selection: &'a DevComposeSelection,
    /// The `[env]` values of the project.
    pub settings: &'a [EnvSetting],
}

impl BlockTarget<'_> {
    fn env_path(&self) -> PathBuf {
        self.checkout.root.join(ENV_FILE)
    }

    fn block(&self) -> Result<String, EnvSyncError> {
        let entries = block_entries(
            self.instance,
            &self.checkout.root,
            self.selection,
            self.settings,
        )?;
        Ok(render_block(&entries)?)
    }
}

/// `current` with the block of `target` in place of its own block.
pub fn expected_text(target: &BlockTarget<'_>, current: &str) -> Result<String, EnvSyncError> {
    Ok(place_block(current, &target.block()?)?)
}

/// Creates `.env` when it is missing, then writes the block when its text
/// changes. Returns the lines announcing what was done, in order.
pub fn write_block(target: &BlockTarget<'_>) -> Result<Vec<String>, EnvSyncError> {
    let block = target.block()?;
    let mut announcements = Vec::new();
    let current = match read_env(&target.checkout.root)? {
        EnvContent::Text(text) => text,
        EnvContent::Missing => {
            announcements.push(create_missing_env(target.checkout)?);
            read_created_env(target.checkout)?
        }
    };
    let expected = place_block(&current, &block)?;
    if expected != current {
        write_env(&target.checkout.root, &expected)?;
        announcements.push(format!(
            "ds: wrote the devslot block in {}",
            target.env_path().display()
        ));
    }
    Ok(announcements)
}

fn create_missing_env(checkout: &Checkout) -> Result<String, EnvSyncError> {
    let source = env_source(&checkout.root, checkout.main_root.as_deref())
        .ok_or(EnvSyncError::MissingSource)?;
    create_env(&checkout.root, &source)?;
    Ok(format!(
        "ds: created {} from {}",
        checkout.root.join(ENV_FILE).display(),
        source.path.display()
    ))
}

fn read_created_env(checkout: &Checkout) -> Result<String, EnvSyncError> {
    Ok(match read_env(&checkout.root)? {
        EnvContent::Text(text) => text,
        EnvContent::Missing => String::new(),
    })
}

/// The warning to print when writing is disabled and `.env` still holds a
/// block, which nothing keeps up to date any more.
pub fn stale_block_warning(checkout: &Checkout) -> Option<String> {
    let EnvContent::Text(text) = read_env(&checkout.root).ok()? else {
        return None;
    };
    split_block(&text).ok()?.has_block.then(|| {
        format!(
            "ds: warning: writing is disabled; the devslot block in {} is stale, remove it with ds rm",
            checkout.root.join(ENV_FILE).display()
        )
    })
}

/// The errors that forbid writing the block: `.env` missing with nothing to
/// create it from, not ignored by git, unreadable, unparsable or with a
/// malformed block. Warnings and the other files never block.
pub fn write_blockers(
    checkout: &Checkout,
    env_files: &EnvFiles,
) -> Result<Vec<Finding>, ToolError> {
    let is_env_ignored = if checkout.is_git {
        Some(checkout.is_env_ignored()?)
    } else {
        None
    };
    let findings = check_env(&EnvCheckInput {
        env: &env_files.env,
        example: &env_files.example,
        test: &env_files.test,
        has_creation_source: has_creation_source(checkout),
        is_env_ignored,
        managed: &[],
        env_config_keys: &[],
        ignored_ports: &[],
    });
    Ok(findings.into_iter().filter(is_blocker).collect())
}

/// Whether `.env` could be created: the one predicate the checks, the
/// registration and `ds ensure` all use.
pub fn has_creation_source(checkout: &Checkout) -> bool {
    env_source(&checkout.root, checkout.main_root.as_deref()).is_some()
}

fn is_blocker(finding: &Finding) -> bool {
    finding.level == Level::Error
        && finding.file.as_deref() == Some(ENV_FILE)
        && BLOCKING_CHECKS.contains(&finding.check)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::discovery::PortKind;
    use crate::port_range::PortRange;
    use crate::project::dotenv_block::{BLOCK_END, BLOCK_START};
    use crate::registry::record::{AssignedPort, PortDetail};

    fn instance(root: &Path) -> InstanceRecord {
        InstanceRecord {
            path: root.to_path_buf(),
            name: "app-3f2a".to_owned(),
            slot: 1,
            range: PortRange {
                first: 20050,
                last: 20099,
            },
            ports: vec![AssignedPort {
                variable: "PORT".to_owned(),
                port: 20050,
                detail: PortDetail {
                    kind: PortKind::Web,
                    service: None,
                    target: None,
                },
            }],
        }
    }

    fn write_in(root: &Path) -> Result<Vec<String>, EnvSyncError> {
        let checkout = Checkout::locate(root).unwrap();
        write_block(&BlockTarget {
            checkout: &checkout,
            instance: &instance(&checkout.root),
            selection: &DevComposeSelection::NoneFound,
            settings: &[],
        })
    }

    fn env_text(root: &Path) -> String {
        std::fs::read_to_string(root.join(".env")).unwrap()
    }

    #[test]
    fn should_append_the_block_and_announce_it_when_the_env_has_none() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".env"), "A=1\n").unwrap();

        let lines = write_in(root.path()).unwrap();

        assert!(env_text(root.path()).starts_with(&format!("A=1\n{BLOCK_START}\n")));
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("ds: wrote the devslot block in "));
    }

    #[test]
    fn should_not_write_again_when_the_block_is_already_up_to_date() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".env"), "A=1\n").unwrap();
        write_in(root.path()).unwrap();
        let before = env_text(root.path());

        let lines = write_in(root.path()).unwrap();

        assert_eq!((lines, env_text(root.path())), (Vec::new(), before));
    }

    #[test]
    fn should_create_the_env_from_the_example_and_announce_both_steps() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".env.example"), "A=1\n").unwrap();

        let lines = write_in(root.path()).unwrap();

        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("ds: created "));
        assert!(lines[0].contains(" from "));
        assert!(env_text(root.path()).starts_with("A=1\n"));
    }

    #[test]
    fn should_report_a_missing_source_when_there_is_nothing_to_create_the_env_from() {
        let root = tempfile::tempdir().unwrap();

        let outcome = write_in(root.path());

        assert!(matches!(outcome, Err(EnvSyncError::MissingSource)));
        assert!(!root.path().join(".env").exists());
    }

    #[test]
    fn should_refuse_and_leave_the_env_untouched_when_its_block_is_malformed() {
        let root = tempfile::tempdir().unwrap();
        let malformed = format!("A=1\n{BLOCK_START}\nPORT=1\n");
        std::fs::write(root.path().join(".env"), &malformed).unwrap();

        let outcome = write_in(root.path());

        assert!(matches!(outcome, Err(EnvSyncError::Block(_))));
        assert_eq!(env_text(root.path()), malformed);
    }

    #[test]
    fn should_warn_when_writing_is_disabled_and_the_env_holds_a_block() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(".env"),
            format!("{BLOCK_START}\nPORT=1\n{BLOCK_END}\n"),
        )
        .unwrap();
        let checkout = Checkout::locate(root.path()).unwrap();

        let warning = stale_block_warning(&checkout);

        assert!(warning.unwrap().contains("the devslot block in "));
    }

    #[test]
    fn should_not_warn_when_the_env_has_no_block() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".env"), "A=1\n").unwrap();
        let checkout = Checkout::locate(root.path()).unwrap();

        assert_eq!(stale_block_warning(&checkout), None);
    }

    #[test]
    fn should_only_keep_the_errors_about_the_env_when_listing_the_blockers() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".env"), "A=1\n").unwrap();
        std::fs::write(root.path().join(".env.example"), "not valid\n").unwrap();
        let checkout = Checkout::locate(root.path()).unwrap();
        let env_files = EnvFiles::read(&checkout.root, None).unwrap();

        let blockers = write_blockers(&checkout, &env_files).unwrap();

        assert_eq!(blockers, Vec::new());
    }

    #[test]
    fn should_block_when_the_only_source_is_an_example_that_is_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "A=1\n").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret"),
            root.path().join(".env.example"),
        )
        .unwrap();
        let checkout = Checkout::locate(root.path()).unwrap();
        let env_files = EnvFiles::read(&checkout.root, None).unwrap();

        let blockers = write_blockers(&checkout, &env_files).unwrap();

        assert_eq!(blockers[0].check, Check::EnvMissing);
    }

    #[test]
    fn should_block_when_the_env_is_missing_and_nothing_can_create_it() {
        let root = tempfile::tempdir().unwrap();
        let checkout = Checkout::locate(root.path()).unwrap();
        let env_files = EnvFiles::read(&checkout.root, None).unwrap();

        let blockers = write_blockers(&checkout, &env_files).unwrap();

        assert_eq!(blockers[0].check, Check::EnvMissing);
    }
}
