//! `ds ensure`: make sure the checkout is registered and its `.env` block is
//! current, without ever starting a service. Meant for a hook that runs before
//! every command of an agent, so every case that has nothing to do is a silent
//! success.

use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::analysis::EnvFiles;
use crate::checkout::{Checkout, CheckoutError};
use crate::checks::finding::{Check, Finding, Level};
use crate::cli::exit_code;
use crate::cli::style::{ColorChoice, Style};
use crate::commands::machine::Machine;
use crate::commands::registering::{
    LOCK_TIMEOUT, announcement_lines, refusal_text, register_checkout_within,
};
use crate::compose::selection::{DevComposeSelection, select_dev_compose};
use crate::ensure_cache::{
    CacheEntry, Fingerprint, Flight, entry_path, read_entry, remove_entry, try_take_flight,
    write_entry,
};
use crate::env_sync::{BlockTarget, EnvSyncError, expected_text, write_block, write_blockers};
use crate::error::ToolError;
use crate::global_config::{GlobalConfig, effective_write_enabled, home_from_environment};
use crate::project::env_file::{EnvContent, read_env, remove_temp_files_older_than};
use crate::project_config::ProjectConfig;
use crate::registration::{Intent, Refusal, RegistrationOutcome};
use crate::registry::record::InstanceRecord;
use crate::registry::store::RegistryStore;

const STALE_TEMP_AGE: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const UNREGISTERED_CLOSING_LINE: &str =
    "ds ensure did not register this checkout until the errors are fixed";
const REGISTERED_CLOSING_LINE: &str =
    "ds ensure did not write the devslot block until the errors are fixed";

/// What the locked write of a registered checkout did.
#[derive(Debug)]
pub enum EnsureOutcome {
    /// Nothing to write, or the checkout left the registry in the meantime.
    Nothing,
    /// `.env` was created or rewritten; holds the lines announcing it.
    Wrote(Vec<String>),
}

/// What the locked write needs.
pub struct EnsureWrite<'a> {
    /// The checkout whose `.env` is written.
    pub checkout: &'a Checkout,
    /// The registry.
    pub store: &'a RegistryStore,
    /// The global configuration, for `[write] enabled`.
    pub global: &'a GlobalConfig,
    /// How long to wait for the lock.
    pub lock_timeout: Duration,
}

/// What `ds ensure` found and did, before anything is printed.
#[derive(Debug)]
pub enum EnsureResult {
    /// Nothing to report: nothing to do, the checkout was already current, or
    /// a refusal that was already reported for the same files.
    Silent,
    /// The checkout was registered or its block rewritten.
    Done {
        /// The instance of the checkout.
        instance: InstanceRecord,
        /// The lines announcing what was done, for stderr.
        lines: Vec<String>,
    },
    /// The checkout is not registered, or its block not written, until the
    /// user fixes the problems; reported for the first time.
    Refused {
        /// The lines explaining why, for stderr.
        lines: Vec<String>,
    },
}

/// Runs `ds ensure` on the current folder.
pub fn run_ensure(color: ColorChoice) -> Result<u8, ToolError> {
    let directory = std::env::current_dir().map_err(ToolError::CurrentDirectory)?;
    let result = ensure_directory(&directory, Style::for_stderr(color))?;
    Ok(print_result(&result))
}

fn print_result(result: &EnsureResult) -> u8 {
    match result {
        EnsureResult::Silent => exit_code::SUCCESS,
        EnsureResult::Done { lines, .. } => {
            print_lines(lines);
            exit_code::SUCCESS
        }
        EnsureResult::Refused { lines } => {
            print_lines(lines);
            exit_code::ERROR
        }
    }
}

fn print_lines(lines: &[String]) {
    for line in lines {
        eprintln!("{line}");
    }
}

/// Makes sure the checkout that holds `directory` is registered and its block
/// is current, and returns what happened instead of printing it.
pub fn ensure_directory(directory: &Path, style: Style) -> Result<EnsureResult, ToolError> {
    let machine = Machine::from_process()?;
    let global = GlobalConfig::load(&machine.paths.config_file)?;
    let home = home_from_environment();
    let directory = directory
        .canonicalize()
        .map_err(|source| CheckoutError::Resolve {
            path: directory.to_path_buf(),
            source,
        })?;
    if !global.auto.covers(&directory, home.as_deref()) {
        return Ok(EnsureResult::Silent);
    }
    let checkout = Checkout::locate(&directory)?;
    if !checkout.is_git || !global.auto.covers(&checkout.root, home.as_deref()) {
        return Ok(EnsureResult::Silent);
    }
    let registry = machine.store.read()?;
    if registry.is_opted_out(&checkout.root) {
        return Ok(EnsureResult::Silent);
    }
    let config = ProjectConfig::load(&checkout.root)?;
    let selection = select_dev_compose(&checkout.root, config.compose_file.as_deref());
    let DevComposeSelection::Selected(dev_file) = &selection else {
        return Ok(EnsureResult::Silent);
    };
    match registry.find(&checkout.root) {
        Some(instance) => {
            ensure_registered(&checkout, &machine, &global, (instance, dev_file), style)
        }
        None => ensure_unregistered(&checkout, &machine, &global, dev_file, style),
    }
}

fn ensure_registered(
    checkout: &Checkout,
    machine: &Machine,
    global: &GlobalConfig,
    (instance, dev_file): (&InstanceRecord, &str),
    style: Style,
) -> Result<EnsureResult, ToolError> {
    remove_temp_files_older_than(&checkout.root, STALE_TEMP_AGE).map_err(EnvSyncError::from)?;
    let config = ProjectConfig::load(&checkout.root)?;
    if !effective_write_enabled(&config, global) || is_env_current(checkout, instance, &config)? {
        return Ok(EnsureResult::Silent);
    }
    let cache = RefusalCache::of(machine, checkout, dev_file);
    if cache.is_silent() {
        return Ok(EnsureResult::Silent);
    }
    let blockers = check_before_writing(checkout)?;
    if !blockers.is_empty() {
        return Ok(cache.refuse(
            &Refusal::Blocked(blockers),
            machine,
            style,
            REGISTERED_CLOSING_LINE,
        ));
    }
    let written = write_if_registered(&EnsureWrite {
        checkout,
        store: &machine.store,
        global,
        lock_timeout: LOCK_TIMEOUT,
    });
    let outcome = match written {
        Ok(outcome) => outcome,
        Err(error) => {
            return refuse_or_fail(&error, &cache, (machine, style)).ok_or(error);
        }
    };
    cache.clear();
    Ok(match outcome {
        EnsureOutcome::Nothing => EnsureResult::Silent,
        EnsureOutcome::Wrote(lines) => EnsureResult::Done {
            instance: instance.clone(),
            lines,
        },
    })
}

/// The errors that forbid writing `.env`, found without the registry lock:
/// the `git check-ignore` call and the reads of `.env` stay out of the lock.
pub fn check_before_writing(checkout: &Checkout) -> Result<Vec<Finding>, ToolError> {
    write_blockers(checkout, &EnvFiles::read(&checkout.root, None)?)
}

/// Whether `.env` already holds exactly the block the registry asks for.
/// Reads files only: no lock, no Docker.
pub fn is_env_current(
    checkout: &Checkout,
    instance: &InstanceRecord,
    config: &ProjectConfig,
) -> Result<bool, ToolError> {
    let Ok(EnvContent::Text(current)) = read_env(&checkout.root) else {
        return Ok(false);
    };
    let selection = select_dev_compose(&checkout.root, config.compose_file.as_deref());
    let target = BlockTarget {
        checkout,
        instance,
        selection: &selection,
        settings: &config.env,
    };
    Ok(expected_text(&target, &current)? == current)
}

/// Under the lock: re-reads the registry, and when the checkout is still
/// registered and not opted out, creates `.env` if missing and writes the
/// block. The caller has already run [`check_before_writing`].
pub fn write_if_registered(write: &EnsureWrite<'_>) -> Result<EnsureOutcome, ToolError> {
    let checkout = write.checkout;
    write.store.locked(write.lock_timeout, |locked| {
        let root = &checkout.root;
        let registry = locked.registry();
        let Some(instance) = registry.find(root).cloned() else {
            return Ok(EnsureOutcome::Nothing);
        };
        if registry.is_opted_out(root) {
            return Ok(EnsureOutcome::Nothing);
        }
        let config = ProjectConfig::load(root)?;
        if !effective_write_enabled(&config, write.global) {
            return Ok(EnsureOutcome::Nothing);
        }
        let selection = select_dev_compose(root, config.compose_file.as_deref());
        let lines = write_block(&BlockTarget {
            checkout,
            instance: &instance,
            selection: &selection,
            settings: &config.env,
        })?;
        Ok(if lines.is_empty() {
            EnsureOutcome::Nothing
        } else {
            EnsureOutcome::Wrote(lines)
        })
    })
}

fn ensure_unregistered(
    checkout: &Checkout,
    machine: &Machine,
    global: &GlobalConfig,
    dev_file: &str,
    style: Style,
) -> Result<EnsureResult, ToolError> {
    let cache = RefusalCache::of(machine, checkout, dev_file);
    if cache.is_silent() {
        return Ok(EnsureResult::Silent);
    }
    let flight = try_take_flight(machine.paths.state_directory(), &checkout.root);
    if matches!(flight, Flight::Busy) {
        return Ok(EnsureResult::Silent);
    }
    let outcome =
        register_checkout_within(checkout, machine, global, Intent::Automatic, PROBE_TIMEOUT);
    match outcome {
        Ok(RegistrationOutcome::Registered(registration)) => {
            cache.clear();
            let lines = announcement_lines(&registration, style);
            if lines.is_empty() {
                return Ok(EnsureResult::Silent);
            }
            Ok(EnsureResult::Done {
                instance: registration.instance,
                lines,
            })
        }
        Ok(RegistrationOutcome::SkippedOptedOut) => Ok(EnsureResult::Silent),
        Ok(RegistrationOutcome::Refused(refusal)) => {
            Ok(cache.refuse(&refusal, machine, style, UNREGISTERED_CLOSING_LINE))
        }
        Err(error @ ToolError::Docker(_)) => {
            cache.remember_docker_failure();
            Err(error)
        }
        Err(error) => refuse_or_fail(&error, &cache, (machine, style)).ok_or(error),
    }
}

/// When `error` is a problem of the checkout's own files or settings (and not
/// of the tool), reports it as a refusal and caches it.
fn refuse_or_fail(
    error: &ToolError,
    cache: &RefusalCache,
    (machine, style): (&Machine, Style),
) -> Option<EnsureResult> {
    let finding = refusal_of(error)?;
    Some(cache.refuse(
        &Refusal::Blocked(vec![finding]),
        machine,
        style,
        REGISTERED_CLOSING_LINE,
    ))
}

/// The finding for a failure to write the block that only the user can fix,
/// so it is a refusal (exit 1) rather than a tool failure.
fn refusal_of(error: &ToolError) -> Option<Finding> {
    let ToolError::EnvSync(problem) = error else {
        return None;
    };
    let check = match problem {
        EnvSyncError::MissingSource => Check::EnvMissing,
        EnvSyncError::Block(_) => Check::EnvBlockMalformed,
        EnvSyncError::Render(_) | EnvSyncError::Entries(_) => Check::EnvBlockUnwritable,
        EnvSyncError::File(_) => return None,
    };
    Some(Finding::new(Level::Error, check, problem.to_string()).in_file(".env"))
}

/// The refusal cache entry of one checkout, with the fingerprint taken before
/// the work started.
struct RefusalCache {
    file: std::path::PathBuf,
    fingerprint: Fingerprint,
}

impl RefusalCache {
    fn of(machine: &Machine, checkout: &Checkout, dev_file: &str) -> Self {
        Self {
            file: entry_path(machine.paths.state_directory(), &checkout.root),
            fingerprint: Fingerprint::of(&checkout.root, checkout.main_root.as_deref(), dev_file),
        }
    }

    fn is_silent(&self) -> bool {
        read_entry(&self.file)
            .is_some_and(|entry| entry.silences(&self.fingerprint, SystemTime::now()))
    }

    fn clear(&self) {
        remove_entry(&self.file);
    }

    fn refuse(
        &self,
        refusal: &Refusal,
        machine: &Machine,
        style: Style,
        closing: &str,
    ) -> EnsureResult {
        let text = refusal_text(refusal, machine, style, closing);
        let entry = if depends_on_other_checkouts(refusal) {
            CacheEntry::shared_state_refusal(self.fingerprint.clone(), SystemTime::now())
        } else {
            CacheEntry::refusal(self.fingerprint.clone())
        };
        self.save(&entry);
        EnsureResult::Refused {
            lines: text.lines().map(str::to_owned).collect(),
        }
    }

    fn remember_docker_failure(&self) {
        self.save(&CacheEntry::docker_failure(
            self.fingerprint.clone(),
            SystemTime::now(),
        ));
    }

    fn save(&self, entry: &CacheEntry) {
        let _ = write_entry(&self.file, entry);
    }
}

/// Whether the refusal comes from the other checkouts of the machine (no room
/// left, a forced port that another instance holds) rather than from the files
/// of this one, so the fingerprint alone cannot tell when it is over.
fn depends_on_other_checkouts(refusal: &Refusal) -> bool {
    match refusal {
        Refusal::NoRoom(_) => true,
        Refusal::Blocked(findings) => findings
            .iter()
            .any(|finding| finding.check == Check::ForcedPortConflict),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::allocation::PortProbe;
    use crate::commands::rm::remove_block_then_apply;
    use crate::compose::probe::testing::FakeComposeCli;
    use crate::paths::{DevslotPaths, PathVariables};
    use crate::registration::{RegistrationContext, register};
    use crate::registry::record::RegistryChange;

    const TIMEOUT: Duration = Duration::from_secs(5);

    struct AllFree;

    impl PortProbe for AllFree {
        fn is_free(&self, _: u16) -> bool {
            true
        }
    }

    struct Fixture {
        state: tempfile::TempDir,
        project: tempfile::TempDir,
        global: GlobalConfig,
    }

    impl Fixture {
        fn new() -> Self {
            let project = tempfile::tempdir().unwrap();
            std::fs::write(project.path().join(".env"), "A=1\n").unwrap();
            std::fs::write(project.path().join("dev.compose.yml"), "").unwrap();
            Self {
                state: tempfile::tempdir().unwrap(),
                project,
                global: GlobalConfig::default(),
            }
        }

        fn store(&self) -> RegistryStore {
            RegistryStore::new(
                DevslotPaths::from_variables(&PathVariables {
                    config_home: Some(OsString::from(self.state.path().join("config"))),
                    state_home: Some(OsString::from(self.state.path().join("state"))),
                    home: None,
                })
                .unwrap(),
            )
        }

        fn checkout(&self) -> Checkout {
            Checkout::locate(self.project.path()).unwrap()
        }

        fn register(&self) {
            let checkout = self.checkout();
            let outcome = register(&RegistrationContext {
                checkout: &checkout,
                compose: &FakeComposeCli::new(&[], "{}"),
                store: &self.store(),
                global: &self.global,
                probe: &AllFree,
                lock_timeout: TIMEOUT,
                intent: Intent::Explicit,
            })
            .unwrap();
            assert!(matches!(outcome, RegistrationOutcome::Registered(_)));
        }

        fn ensure(&self) -> EnsureOutcome {
            write_if_registered(&EnsureWrite {
                checkout: &self.checkout(),
                store: &self.store(),
                global: &self.global,
                lock_timeout: TIMEOUT,
            })
            .unwrap()
        }

        fn env_text(&self) -> String {
            std::fs::read_to_string(self.project.path().join(".env")).unwrap()
        }

        fn remove_instance_and_block(&self) {
            let root: PathBuf = self.checkout().root;
            remove_block_then_apply(&self.store(), &root, |_| {
                vec![
                    RegistryChange::RemoveInstance(root.clone()),
                    RegistryChange::OptOut(root.clone()),
                ]
            })
            .unwrap();
        }
    }

    #[test]
    fn should_write_the_block_again_when_it_was_deleted_by_hand() {
        let fixture = Fixture::new();
        fixture.register();
        std::fs::write(fixture.project.path().join(".env"), "A=1\n").unwrap();

        let outcome = fixture.ensure();

        assert!(matches!(outcome, EnsureOutcome::Wrote(_)));
        assert!(fixture.env_text().contains("COMPOSE_PROJECT_NAME="));
    }

    #[test]
    fn should_write_nothing_when_the_block_is_already_current() {
        let fixture = Fixture::new();
        fixture.register();
        let before = fixture.env_text();

        let outcome = fixture.ensure();

        assert!(matches!(outcome, EnsureOutcome::Nothing));
        assert_eq!(fixture.env_text(), before);
    }

    #[test]
    fn should_see_the_env_as_current_after_a_registration_and_stale_after_an_edit() {
        let fixture = Fixture::new();
        fixture.register();
        let checkout = fixture.checkout();
        let instance = fixture.store().read().unwrap().instances[0].clone();
        let config = ProjectConfig::load(&checkout.root).unwrap();

        let before = is_env_current(&checkout, &instance, &config).unwrap();
        std::fs::write(checkout.root.join(".env"), "A=1\n").unwrap();
        let after = is_env_current(&checkout, &instance, &config).unwrap();

        assert_eq!((before, after), (true, false));
    }

    #[test]
    fn should_write_nothing_after_ds_rm_took_the_instance_and_the_block_away() {
        let fixture = Fixture::new();
        fixture.register();
        fixture.remove_instance_and_block();
        let after_rm = fixture.env_text();

        let outcome = fixture.ensure();

        assert!(matches!(outcome, EnsureOutcome::Nothing));
        assert_eq!(fixture.env_text(), after_rm);
        assert_eq!(after_rm, "A=1\n");
    }

    #[test]
    fn should_write_nothing_when_the_checkout_is_opted_out_but_still_registered() {
        let fixture = Fixture::new();
        fixture.register();
        let root = fixture.checkout().root;
        fixture
            .store()
            .locked(TIMEOUT, |locked| {
                locked.apply(&[RegistryChange::OptOut(root)])
            })
            .unwrap();
        std::fs::write(fixture.project.path().join(".env"), "A=1\n").unwrap();

        let outcome = fixture.ensure();

        assert!(matches!(outcome, EnsureOutcome::Nothing));
        assert_eq!(fixture.env_text(), "A=1\n");
    }

    #[test]
    fn should_list_the_blockers_before_the_lock_when_the_env_is_gone_and_nothing_can_create_it() {
        let fixture = Fixture::new();
        fixture.register();
        std::fs::remove_file(fixture.project.path().join(".env")).unwrap();

        let blockers = check_before_writing(&fixture.checkout()).unwrap();

        assert_eq!(blockers.len(), 1);
        assert!(!fixture.project.path().join(".env").exists());
    }

    #[test]
    fn should_turn_a_missing_source_into_a_refusal_and_an_io_failure_into_a_tool_failure() {
        let missing = ToolError::EnvSync(EnvSyncError::MissingSource);
        let io = ToolError::EnvSync(EnvSyncError::File(
            crate::project::env_file::EnvFileError::Symlink {
                path: std::path::PathBuf::from("/x/.env"),
            },
        ));

        assert_eq!(refusal_of(&missing).unwrap().check, Check::EnvMissing);
        assert!(refusal_of(&io).is_none());
    }

    #[test]
    fn should_treat_a_refusal_about_other_checkouts_as_shared_state() {
        let forced = Finding::new(Level::Error, Check::ForcedPortConflict, "x");
        let own = Finding::new(Level::Error, Check::ContainerName, "x");

        assert!(depends_on_other_checkouts(&Refusal::Blocked(vec![forced])));
        assert!(!depends_on_other_checkouts(&Refusal::Blocked(vec![own])));
        assert!(depends_on_other_checkouts(&Refusal::NoRoom(
            crate::allocation::AllocationError::SlotFull {
                requested: 2,
                capacity: 1
            }
        )));
    }
}
