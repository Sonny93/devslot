//! `ds ensure`: make sure the checkout is registered and its `.env` block is
//! current, without ever starting a service. Meant for a hook that runs before
//! every command of an agent, so every case that has nothing to do is a silent
//! success.

use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::analysis::EnvFiles;
use crate::analysis::is_local_config_tracked;
use crate::block_entries::BlockEntriesError;
use crate::checkout::{Checkout, CheckoutError};
use crate::checks::env_config::check_local_config_tracked;
use crate::checks::finding::{Check, Finding, Level};
use crate::cli::exit_code;
use crate::cli::style::{ColorChoice, Style};
use crate::commands::machine::Machine;
use crate::commands::registering::{
    LOCK_TIMEOUT, announcement_lines, refusal_text, register_checkout_within,
};
use crate::compose::selection::{DevComposeSelection, select_dev_compose};
use crate::deadline::{BudgetExhausted, Deadline};
use crate::ensure_cache::{
    CacheEntry, Fingerprint, Flight, entry_path, locate_entry_path, read_entry,
    remove_budget_entry, remove_entry, try_take_flight, write_entry,
};
use crate::env_sync::{BlockTarget, EnvSyncError, expected_text, write_block, write_blockers};
use crate::environment::EnvironmentError;
use crate::error::ToolError;
use crate::global_config::{GlobalConfig, effective_write_enabled, home_from_environment};
use crate::project::env_file::{EnvContent, read_env, remove_temp_files_older_than};
use crate::project_config::ProjectConfig;
use crate::registration::{Intent, Refusal, RegistrationOutcome, lock_wait};
use crate::registry::RegistryError;
use crate::registry::record::InstanceRecord;
use crate::registry::store::RegistryStore;
use crate::worktree_guard::is_local_switch_effective;

const STALE_TEMP_AGE: Duration = Duration::from_secs(60);
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
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
    ensure_directory_within(directory, style, None)
}

/// Like [`ensure_directory`], for a caller that is killed when its time is up.
/// The git calls, the compose probes and the wait for the registry lock stop
/// at `deadline`; the work under the lock is never cut short. A run that runs
/// out of budget is [`EnsureResult::Silent`]: it is remembered in the cache of
/// the checkout when the checkout is known by then, so the next calls do not
/// pay the same time again.
pub fn ensure_directory_within(
    directory: &Path,
    style: Style,
    deadline: Option<Deadline>,
) -> Result<EnsureResult, ToolError> {
    match ensure_checkout(directory, style, deadline) {
        Err(error) if error.is_budget_exhausted() => Ok(EnsureResult::Silent),
        other => other,
    }
}

/// The checkout of `directory`, or `None` when an earlier run ran out of budget
/// locating it and its entry still silences. Under a deadline, a failure to
/// locate in time is remembered by folder, since no checkout key exists yet.
fn locate_checkout(
    directory: &Path,
    machine: &Machine,
    deadline: Option<Deadline>,
) -> Result<Option<Checkout>, ToolError> {
    let Some(deadline) = deadline else {
        return Ok(Some(Checkout::locate(directory)?));
    };
    let file = locate_entry_path(machine.paths.state_directory(), directory);
    let previous = read_entry(&file);
    let silenced = previous
        .as_ref()
        .is_some_and(|entry| entry.silences(&Fingerprint::none(), SystemTime::now()));
    if silenced {
        return Ok(None);
    }
    match Checkout::locate_within(directory, deadline) {
        Ok(checkout) => {
            remove_entry(&file);
            Ok(Some(checkout))
        }
        Err(error @ CheckoutError::Budget(_)) => {
            let entry = CacheEntry::budget_exhaustion(
                Fingerprint::none(),
                SystemTime::now(),
                previous.as_ref(),
            );
            let _ = write_entry(&file, &entry);
            Err(error.into())
        }
        Err(error) => Err(error.into()),
    }
}

fn ensure_checkout(
    directory: &Path,
    style: Style,
    deadline: Option<Deadline>,
) -> Result<EnsureResult, ToolError> {
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
    let Some(checkout) = locate_checkout(&directory, &machine, deadline)? else {
        return Ok(EnsureResult::Silent);
    };
    if !checkout.is_git || !global.auto.covers(&checkout.root, home.as_deref()) {
        return Ok(EnsureResult::Silent);
    }
    if is_required_main_checkout(&checkout, &global, home.as_deref())? {
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
        Some(instance) => ensure_registered(
            (&checkout, &machine, &global),
            (instance, dev_file),
            (style, deadline),
        ),
        None => ensure_unregistered((&checkout, &machine, &global), dev_file, (style, deadline)),
    }
}

/// Whether the worktree guard covers the checkout as a main checkout: agents
/// work in a linked worktree there, so the checkout is left as it is, whether
/// or not it was registered before the guard was turned on.
fn is_required_main_checkout(
    checkout: &Checkout,
    global: &GlobalConfig,
    home: Option<&Path>,
) -> Result<bool, ToolError> {
    let is_main_checkout = checkout.main_root.is_none();
    if !is_main_checkout || !global.require.covers(&checkout.root, home) {
        return Ok(false);
    }
    Ok(!is_local_switch_effective(&checkout.root, None)?)
}

fn ensure_registered(
    (checkout, machine, global): (&Checkout, &Machine, &GlobalConfig),
    (instance, dev_file): (&InstanceRecord, &str),
    (style, deadline): (Style, Option<Deadline>),
) -> Result<EnsureResult, ToolError> {
    remove_temp_files_older_than(&checkout.root, STALE_TEMP_AGE).map_err(EnvSyncError::from)?;
    let config = ProjectConfig::load(&checkout.root)?;
    let state = if effective_write_enabled(&config, global) {
        env_state(checkout, instance, &config)?
    } else {
        EnvState::Current
    };
    if state == EnvState::Current {
        remove_budget_entry(&entry_path(machine.paths.state_directory(), &checkout.root));
        return Ok(EnsureResult::Silent);
    }
    let cache = RefusalCache::of(machine, checkout, dev_file);
    if cache.is_silent() {
        return Ok(EnsureResult::Silent);
    }
    if state == EnvState::RegistryBehind {
        return refresh_registration((checkout, machine, global), &cache, (style, deadline));
    }
    let flight = try_take_flight(machine.paths.state_directory(), &checkout.root);
    if matches!(flight, Flight::Busy) {
        return Ok(EnsureResult::Silent);
    }
    let result = write_registered(
        (checkout, machine, global),
        (instance, &cache),
        (style, deadline),
    );
    cache.absorb_budget(result)
}

/// How the `.env` block of a registered checkout compares with what its
/// project asks for.
#[derive(Debug, PartialEq, Eq)]
enum EnvState {
    /// The block is what the registry asks for.
    Current,
    /// The block must be rewritten from the registry.
    Stale,
    /// `[env]` refers to a variable the registry does not have yet (a new
    /// `[ports] extra` or compose variable): the registration must be refreshed.
    RegistryBehind,
}

fn env_state(
    checkout: &Checkout,
    instance: &InstanceRecord,
    config: &ProjectConfig,
) -> Result<EnvState, ToolError> {
    match is_env_current(checkout, instance, config) {
        Ok(true) => Ok(EnvState::Current),
        Ok(false) => Ok(EnvState::Stale),
        Err(error) if is_unknown_reference(&error) => Ok(EnvState::RegistryBehind),
        Err(error) => Err(error),
    }
}

/// Whether the error is an `[env]` reference to a variable the instance does
/// not have.
fn is_unknown_reference(error: &ToolError) -> bool {
    matches!(
        error,
        ToolError::EnvSync(EnvSyncError::Entries(BlockEntriesError::Environment(
            EnvironmentError::UnknownReference { .. }
        )))
    )
}

fn write_registered(
    (checkout, machine, global): (&Checkout, &Machine, &GlobalConfig),
    (instance, cache): (&InstanceRecord, &RefusalCache),
    (style, deadline): (Style, Option<Deadline>),
) -> Result<EnsureResult, ToolError> {
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
        lock_timeout: lock_wait(LOCK_TIMEOUT, deadline)?,
    });
    let outcome = match written.map_err(|error| lock_timeout_as_budget(error, deadline)) {
        Ok(outcome) => outcome,
        Err(error) => {
            return refuse_or_fail(&error, cache, (machine, style)).ok_or(error);
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

/// With a deadline, a registry lock that stayed taken is a budget that ran
/// out: the lock wait was shortened to fit it.
fn lock_timeout_as_budget(error: ToolError, deadline: Option<Deadline>) -> ToolError {
    match error {
        ToolError::Registry(RegistryError::LockTimeout { .. }) if deadline.is_some() => {
            ToolError::Budget(BudgetExhausted)
        }
        other => other,
    }
}

/// The errors that forbid writing `.env`, found without the registry lock:
/// the `git check-ignore` call and the reads of `.env` stay out of the lock.
pub fn check_before_writing(checkout: &Checkout) -> Result<Vec<Finding>, ToolError> {
    let mut blockers = write_blockers(checkout, &EnvFiles::read(&checkout.root, None)?)?;
    blockers.extend(check_local_config_tracked(is_local_config_tracked(
        checkout,
    )?));
    Ok(blockers)
}

/// Whether `.env` already holds exactly the block the registry asks for. The
/// block is built even when `.env` is missing, so an error in what the
/// registry asks for is reported in every case.
/// Reads files only: no lock, no Docker.
pub fn is_env_current(
    checkout: &Checkout,
    instance: &InstanceRecord,
    config: &ProjectConfig,
) -> Result<bool, ToolError> {
    let selection = select_dev_compose(&checkout.root, config.compose_file.as_deref());
    let target = BlockTarget {
        checkout,
        instance,
        selection: &selection,
        settings: &config.env,
    };
    let current = match read_env(&checkout.root) {
        Ok(EnvContent::Text(text)) => Some(text),
        _ => None,
    };
    let expected = expected_text(&target, current.as_deref().unwrap_or_default())?;
    Ok(current.is_some_and(|text| text == expected))
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
    (checkout, machine, global): (&Checkout, &Machine, &GlobalConfig),
    dev_file: &str,
    (style, deadline): (Style, Option<Deadline>),
) -> Result<EnsureResult, ToolError> {
    let cache = RefusalCache::of(machine, checkout, dev_file);
    if cache.is_silent() {
        return Ok(EnsureResult::Silent);
    }
    refresh_registration((checkout, machine, global), &cache, (style, deadline))
}

/// Registers the checkout again (or for the first time) under the single
/// flight of the checkout, with the compose probe inside the hook budget. The
/// caller has already checked that the refusal cache is not silent.
fn refresh_registration(
    (checkout, machine, global): (&Checkout, &Machine, &GlobalConfig),
    cache: &RefusalCache,
    (style, deadline): (Style, Option<Deadline>),
) -> Result<EnsureResult, ToolError> {
    let flight = try_take_flight(machine.paths.state_directory(), &checkout.root);
    if matches!(flight, Flight::Busy) {
        return Ok(EnsureResult::Silent);
    }
    let outcome = register_checkout_within(
        checkout,
        machine,
        global,
        Intent::Automatic,
        (PROBE_TIMEOUT, deadline),
    )
    .map_err(|error| lock_timeout_as_budget(error, deadline));
    cache.absorb_budget(registration_result(outcome, cache, (machine, style)))
}

fn registration_result(
    outcome: Result<RegistrationOutcome, ToolError>,
    cache: &RefusalCache,
    (machine, style): (&Machine, Style),
) -> Result<EnsureResult, ToolError> {
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
        Err(error @ ToolError::Docker(_)) if !error.is_budget_exhausted() => {
            cache.remember_docker_failure();
            Err(error)
        }
        Err(error) => refuse_or_fail(&error, cache, (machine, style)).ok_or(error),
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

    fn remember_budget_exhaustion(&self) {
        let previous = read_entry(&self.file);
        self.save(&CacheEntry::budget_exhaustion(
            self.fingerprint.clone(),
            SystemTime::now(),
            previous.as_ref(),
        ));
    }

    /// A run that ran out of budget is remembered and silent; any other result
    /// goes through.
    fn absorb_budget(
        &self,
        result: Result<EnsureResult, ToolError>,
    ) -> Result<EnsureResult, ToolError> {
        match result {
            Err(error) if error.is_budget_exhausted() => {
                self.remember_budget_exhaustion();
                Ok(EnsureResult::Silent)
            }
            other => other,
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
    use std::path::{Path, PathBuf};

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::allocation::PortProbe;
    use crate::commands::rm::remove_block_then_apply;
    use crate::compose::probe::testing::FakeComposeCli;
    use crate::paths::{DevslotPaths, PathVariables};
    use crate::registration::{RegistrationContext, register};
    use crate::registry::lock::RegistryLock;
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

        fn paths(&self) -> DevslotPaths {
            DevslotPaths::from_variables(&PathVariables {
                config_home: Some(OsString::from(self.state.path().join("config"))),
                state_home: Some(OsString::from(self.state.path().join("state"))),
                home: None,
            })
            .unwrap()
        }

        fn store(&self) -> RegistryStore {
            RegistryStore::new(self.paths())
        }

        fn entry_file(&self, root: &Path) -> PathBuf {
            entry_path(self.paths().state_directory(), root)
        }

        fn budget_streak(&self) -> Option<u32> {
            read_entry(&self.entry_file(&self.checkout().root))?.budget_streak
        }

        fn ensure_registered_within(&self, deadline: Deadline) -> EnsureResult {
            let machine = Machine {
                paths: self.paths(),
                store: self.store(),
            };
            let instance = self.store().read().unwrap().instances[0].clone();
            ensure_registered(
                (&self.checkout(), &machine, &self.global),
                (&instance, "dev.compose.yml"),
                (Style { color: false }, Some(deadline)),
            )
            .unwrap()
        }

        fn make_env_stale(&self, content: &str) {
            std::fs::write(self.project.path().join(".env"), content).unwrap();
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
                deadline: None,
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

    #[test]
    fn should_stay_out_of_the_locked_section_and_remember_the_budget_when_no_time_is_left_for_the_lock()
     {
        let fixture = Fixture::new();
        fixture.register();
        fixture.make_env_stale("A=1\n");
        let neighbour = fixture.entry_file(Path::new("/work/neighbour"));
        write_entry(
            &neighbour,
            &CacheEntry::refusal(Fingerprint::of(
                fixture.project.path(),
                None,
                "dev.compose.yml",
            )),
        )
        .unwrap();
        let _held = RegistryLock::acquire(&fixture.paths().lock_file, TIMEOUT).unwrap();
        let started = std::time::Instant::now();

        let result =
            fixture.ensure_registered_within(Deadline::starting_now(Duration::from_millis(500)));

        assert!(matches!(result, EnsureResult::Silent));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(fixture.budget_streak(), Some(1));
        assert_eq!(fixture.env_text(), "A=1\n");
        assert!(read_entry(&neighbour).is_some());
    }

    #[test]
    fn should_give_up_the_wait_for_the_lock_within_the_budget_and_remember_it() {
        let fixture = Fixture::new();
        fixture.register();
        fixture.make_env_stale("A=1\n");
        let _held = RegistryLock::acquire(&fixture.paths().lock_file, TIMEOUT).unwrap();
        let started = std::time::Instant::now();

        let result =
            fixture.ensure_registered_within(Deadline::starting_now(Duration::from_secs(2)));

        assert!(matches!(result, EnsureResult::Silent));
        assert!(started.elapsed() < LOCK_TIMEOUT);
        assert_eq!(fixture.budget_streak(), Some(1));
    }

    #[test]
    fn should_go_on_counting_the_streak_when_the_checkout_changed_and_the_budget_ran_out_again() {
        let fixture = Fixture::new();
        fixture.register();
        fixture.make_env_stale("A=1\n");
        let _held = RegistryLock::acquire(&fixture.paths().lock_file, TIMEOUT).unwrap();
        let exhausted = Duration::from_millis(500);
        fixture.ensure_registered_within(Deadline::starting_now(exhausted));

        fixture.make_env_stale("A=22\n");
        fixture.ensure_registered_within(Deadline::starting_now(exhausted));

        assert_eq!(fixture.budget_streak(), Some(2));
    }

    #[test]
    fn should_stay_silent_without_trying_again_while_the_budget_entry_lasts() {
        let fixture = Fixture::new();
        fixture.register();
        fixture.make_env_stale("A=1\n");
        let held = RegistryLock::acquire(&fixture.paths().lock_file, TIMEOUT).unwrap();
        fixture.ensure_registered_within(Deadline::starting_now(Duration::from_millis(500)));
        drop(held);

        let result =
            fixture.ensure_registered_within(Deadline::starting_now(Duration::from_secs(15)));

        assert!(matches!(result, EnsureResult::Silent));
        assert_eq!(fixture.env_text(), "A=1\n");
    }

    #[test]
    fn should_forget_the_budget_entry_when_a_later_run_writes_the_block() {
        let fixture = Fixture::new();
        fixture.register();
        fixture.make_env_stale("A=1\n");
        let held = RegistryLock::acquire(&fixture.paths().lock_file, TIMEOUT).unwrap();
        fixture.ensure_registered_within(Deadline::starting_now(Duration::from_millis(500)));
        drop(held);
        fixture.make_env_stale("A=22\n");

        let result =
            fixture.ensure_registered_within(Deadline::starting_now(Duration::from_secs(15)));

        assert!(matches!(result, EnsureResult::Done { .. }));
        assert_eq!(fixture.budget_streak(), None);
        assert!(fixture.env_text().contains("COMPOSE_PROJECT_NAME="));
    }

    #[test]
    fn should_forget_the_budget_entry_when_the_block_is_current_again() {
        let fixture = Fixture::new();
        fixture.register();
        let current = fixture.env_text();
        fixture.make_env_stale("A=1\n");
        let held = RegistryLock::acquire(&fixture.paths().lock_file, TIMEOUT).unwrap();
        fixture.ensure_registered_within(Deadline::starting_now(Duration::from_millis(500)));
        drop(held);
        fixture.make_env_stale(&current);

        fixture.ensure_registered_within(Deadline::starting_now(Duration::from_secs(15)));

        assert_eq!(fixture.budget_streak(), None);
    }

    #[test]
    fn should_return_silent_at_once_when_another_hook_already_writes_the_block() {
        let fixture = Fixture::new();
        fixture.register();
        fixture.make_env_stale("A=1\n");
        let state = fixture.paths();
        let _flight = try_take_flight(state.state_directory(), &fixture.checkout().root);
        let _held = RegistryLock::acquire(&state.lock_file, TIMEOUT).unwrap();
        let started = std::time::Instant::now();

        let result =
            fixture.ensure_registered_within(Deadline::starting_now(Duration::from_secs(15)));

        assert!(matches!(result, EnsureResult::Silent));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(fixture.budget_streak(), None);
        assert_eq!(fixture.env_text(), "A=1\n");
    }

    #[test]
    fn should_stay_silent_without_locating_again_when_the_locate_stage_ran_out_of_budget() {
        let fixture = Fixture::new();
        let machine = Machine {
            paths: fixture.paths(),
            store: fixture.store(),
        };
        let directory = fixture.project.path();
        let exhausted = Some(Deadline::starting_now(Duration::ZERO));
        let first = locate_checkout(directory, &machine, exhausted);

        let second = locate_checkout(
            directory,
            &machine,
            Some(Deadline::starting_now(Duration::from_secs(60))),
        );

        assert!(first.unwrap_err().is_budget_exhausted());
        assert!(second.unwrap().is_none());
        let file = locate_entry_path(machine.paths.state_directory(), directory);
        assert_eq!(read_entry(&file).unwrap().budget_streak, Some(1));
    }

    #[test]
    fn should_forget_the_locate_entry_when_the_checkout_is_located_again() {
        let fixture = Fixture::new();
        let machine = Machine {
            paths: fixture.paths(),
            store: fixture.store(),
        };
        let directory = fixture.project.path();
        let file = locate_entry_path(machine.paths.state_directory(), directory);
        let expired = CacheEntry::budget_exhaustion(
            Fingerprint::none(),
            SystemTime::now() - Duration::from_secs(60),
            None,
        );
        write_entry(&file, &expired).unwrap();

        let located = locate_checkout(
            directory,
            &machine,
            Some(Deadline::starting_now(Duration::from_secs(60))),
        );

        assert!(located.unwrap().is_some());
        assert_eq!(read_entry(&file), None);
    }
}
