//! Registering a checkout: run the checks `ds up` runs on the dev compose,
//! then take the lock, give the instance its slot and ports, write the
//! registry and keep the devslot block of `.env` up to date. Shared by `ds up`,
//! `ds run` and `ds ensure`.

use std::path::Path;
use std::time::Duration;

use crate::allocation::{
    AllocationError, NewInstanceIdentity, PortProbe, PortRequest, allocate_known, allocate_new,
};
use crate::analysis::{CheckoutAnalysis, ProbeScope};
use crate::checkout::Checkout;
use crate::checks::env_config::check_forced_port_conflicts;
use crate::checks::finding::{Finding, Level};
use crate::docker::ComposeCli;
use crate::env_sync::{BlockTarget, stale_block_warning, write_block, write_blockers};
use crate::error::ToolError;
use crate::global_config::{GlobalConfig, effective_write_enabled};
use crate::instance_name::{DEFAULT_HASH_LENGTH, FULL_HASH_LENGTH, HASH_LENGTH_STEP, derive_name};
use crate::project::env_file::env_source;
use crate::project_config::ProjectConfig;
use crate::registry::record::{InstanceRecord, Registry, RegistryChange};
use crate::registry::store::{LockedRegistry, RegistryStore};

/// Who asks for the registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// A hook: a checkout that `ds rm` opted out stays alone.
    Automatic,
    /// The user ran `ds up` or `ds run`: an opted-out checkout is registered
    /// again and its opt-out is cleared.
    Explicit,
}

/// What registration needs besides the checkout.
pub struct RegistrationContext<'a> {
    /// The checkout to register.
    pub checkout: &'a Checkout,
    /// Compose, for the discovery of the ports.
    pub compose: &'a dyn ComposeCli,
    /// The registry.
    pub store: &'a RegistryStore,
    /// The global configuration: the slot geometry and whether `.env` is written.
    pub global: &'a GlobalConfig,
    /// Which ports are free on this machine.
    pub probe: &'a dyn PortProbe,
    /// How long to wait for the registry lock.
    pub lock_timeout: Duration,
    /// Who asks.
    pub intent: Intent,
}

/// What registration changed in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationChange {
    /// A new instance was added.
    Created,
    /// A known instance got new variables or new details.
    Updated,
    /// Nothing changed: the registry was not written.
    Unchanged,
}

/// A checkout that is registered.
#[derive(Debug)]
pub struct Registration {
    /// The registry entry of the checkout.
    pub instance: InstanceRecord,
    /// What changed.
    pub change: RegistrationChange,
    /// The warnings of the checks, to show.
    pub warnings: Vec<Finding>,
    /// Whether the opt-out of `ds rm` was cleared by this registration.
    pub cleared_opt_out: bool,
    /// What was done to `.env`, as lines for stderr.
    pub env_lines: Vec<String>,
}

/// Why a checkout is not registered. Both are answers, not tool failures.
#[derive(Debug)]
pub enum Refusal {
    /// The checks found at least one error. Holds the errors and the warnings.
    Blocked(Vec<Finding>),
    /// There is no room for the instance's ports.
    NoRoom(AllocationError),
}

/// The result of asking for a registration.
#[derive(Debug)]
pub enum RegistrationOutcome {
    /// The checkout is registered.
    Registered(Registration),
    /// An automatic registration found the checkout opted out and did nothing.
    SkippedOptedOut,
    /// The checkout is not registered.
    Refused(Refusal),
}

impl Refusal {
    /// What to tell the user when there is no room, with the file to edit.
    pub fn no_room_message(error: &AllocationError, config_path: &Path) -> String {
        match error {
            AllocationError::SlotFull { .. } => {
                format!("{error}; raise [slots] size in {}", config_path.display())
            }
            AllocationError::NoFreeSlot { .. } => format!(
                "{error}; widen [slots] first_port and last_port in {}",
                config_path.display()
            ),
            AllocationError::SlotBusy { .. } => error.to_string(),
        }
    }
}

impl Registration {
    /// The lines announcing what registration did, for stderr, in order: the
    /// opt-out that was cleared, the first registration, what happened to `.env`.
    pub fn notices(&self) -> Vec<String> {
        let instance = &self.instance;
        let mut lines = Vec::new();
        if self.cleared_opt_out {
            lines.push(format!(
                "ds: {} was opted out by ds rm; registering it again",
                instance.path.display()
            ));
        }
        if self.change == RegistrationChange::Created {
            lines.push(format!(
                "ds: registered {} as {}, slot {} ({})",
                instance.path.display(),
                instance.name,
                instance.slot,
                instance.range
            ));
            lines.push(format!(
                "ds: compose project is now {}; containers and volumes created before devslot keep their old project name",
                instance.name
            ));
        }
        lines.extend(self.env_lines.iter().cloned());
        lines
    }
}

enum Allocated {
    Conflicts(Vec<Finding>),
    Saved(Box<Saved>),
    NoRoom(AllocationError),
    OptedOut,
}

struct Saved {
    instance: InstanceRecord,
    change: RegistrationChange,
    cleared_opt_out: bool,
    env_lines: Vec<String>,
}

struct Pending<'a> {
    context: &'a RegistrationContext<'a>,
    repository: String,
    requests: Vec<PortRequest>,
    analysis: CheckoutAnalysis,
    is_writing: bool,
}

/// Registers the checkout, or says why it cannot be.
pub fn register(context: &RegistrationContext<'_>) -> Result<RegistrationOutcome, ToolError> {
    let checkout = context.checkout;
    let project_config = ProjectConfig::load(&checkout.root)?;
    let is_writing = effective_write_enabled(&project_config, context.global);
    let creation_source = is_writing
        .then(|| env_source(&checkout.root, checkout.main_root.as_deref()))
        .flatten()
        .map(|source| source.path);
    let analysis = CheckoutAnalysis::analyze_with_creation_source(
        checkout,
        context.compose,
        ProbeScope::DevComposeOnly,
        creation_source.as_deref(),
    )?;
    let mut findings = shown_findings(analysis.up_findings(&checkout.root));
    if is_writing {
        findings.extend(write_blockers(checkout, &analysis.env_files)?);
    }
    if findings.iter().any(|finding| finding.level == Level::Error) {
        return Ok(RegistrationOutcome::Refused(Refusal::Blocked(findings)));
    }
    let pending = Pending {
        context,
        repository: checkout.repository_name()?,
        requests: analysis.ports.iter().map(PortRequest::from).collect(),
        analysis,
        is_writing,
    };
    let allocated = context.store.locked(context.lock_timeout, |locked| {
        register_locked(&pending, locked)
    })?;
    Ok(match allocated {
        Allocated::Saved(saved) => RegistrationOutcome::Registered(Registration {
            instance: saved.instance,
            change: saved.change,
            warnings: findings,
            cleared_opt_out: saved.cleared_opt_out,
            env_lines: saved.env_lines,
        }),
        Allocated::OptedOut => RegistrationOutcome::SkippedOptedOut,
        Allocated::Conflicts(conflicts) => {
            let blocking = conflicts.into_iter().chain(findings).collect();
            RegistrationOutcome::Refused(Refusal::Blocked(blocking))
        }
        Allocated::NoRoom(error) => RegistrationOutcome::Refused(Refusal::NoRoom(error)),
    })
}

fn shown_findings(findings: Vec<Finding>) -> Vec<Finding> {
    findings
        .into_iter()
        .filter(|finding| finding.level != Level::Info)
        .collect()
}

fn register_locked(
    pending: &Pending<'_>,
    locked: &mut LockedRegistry<'_>,
) -> Result<Allocated, ToolError> {
    let context = pending.context;
    let root = &context.checkout.root;
    let is_opted_out = locked.registry().is_opted_out(root);
    if is_opted_out && context.intent == Intent::Automatic {
        return Ok(Allocated::OptedOut);
    }
    let conflicts = check_forced_port_conflicts(
        &pending.analysis.config,
        &pending.analysis.ports,
        locked.registry(),
        root,
    );
    if !conflicts.is_empty() {
        return Ok(Allocated::Conflicts(conflicts));
    }
    let existing = locked.registry().find(root).cloned();
    let record = match allocate(pending, locked.registry(), existing.as_ref()) {
        Ok(record) => record,
        Err(error) => return Ok(Allocated::NoRoom(error)),
    };
    let change = change_of(existing.as_ref(), &record);
    locked.apply(&registry_changes(root, &record, change, is_opted_out))?;
    let env_lines = sync_env(pending, &record)?;
    Ok(Allocated::Saved(Box::new(Saved {
        instance: record,
        change,
        cleared_opt_out: is_opted_out,
        env_lines,
    })))
}

fn allocate(
    pending: &Pending<'_>,
    registry: &Registry,
    existing: Option<&InstanceRecord>,
) -> Result<InstanceRecord, AllocationError> {
    let context = pending.context;
    let root = &context.checkout.root;
    if let Some(known) = existing {
        return allocate_known(known, registry, &pending.requests, context.probe);
    }
    let identity = NewInstanceIdentity {
        path: root.clone(),
        name: unique_name(registry, &pending.repository, root),
    };
    allocate_new(
        identity,
        registry,
        context.global.slots,
        &pending.requests,
        context.probe,
    )
}

fn change_of(existing: Option<&InstanceRecord>, record: &InstanceRecord) -> RegistrationChange {
    match existing {
        None => RegistrationChange::Created,
        Some(known) if known == record => RegistrationChange::Unchanged,
        Some(_) => RegistrationChange::Updated,
    }
}

fn registry_changes(
    root: &Path,
    record: &InstanceRecord,
    change: RegistrationChange,
    is_opted_out: bool,
) -> Vec<RegistryChange> {
    let mut changes = Vec::new();
    if change != RegistrationChange::Unchanged {
        changes.push(RegistryChange::SaveInstance(record.clone()));
    }
    if is_opted_out {
        changes.push(RegistryChange::ClearOptOut(root.to_path_buf()));
    }
    changes
}

fn sync_env(pending: &Pending<'_>, record: &InstanceRecord) -> Result<Vec<String>, ToolError> {
    let checkout = pending.context.checkout;
    if !pending.is_writing {
        return Ok(stale_block_warning(checkout).into_iter().collect());
    }
    Ok(write_block(&BlockTarget {
        checkout,
        instance: record,
        selection: &pending.analysis.selection,
        settings: &pending.analysis.config.env,
    })?)
}

fn unique_name(registry: &Registry, repository: &str, root: &Path) -> String {
    (DEFAULT_HASH_LENGTH..=FULL_HASH_LENGTH)
        .step_by(HASH_LENGTH_STEP)
        .map(|length| derive_name(repository, root, length))
        .find(|name| !registry.name_is_taken_by_other(name, root))
        .unwrap_or_else(|| derive_name(repository, root, FULL_HASH_LENGTH))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::checks::finding::Check;
    use crate::compose::probe::testing::FakeComposeCli;
    use crate::global_config::{GlobalConfig, SlotGeometry};
    use crate::paths::{DevslotPaths, PathVariables};

    const POSTGRES: &str =
        r#"{"services":{"postgres":{"ports":[{"target":5432,"published":"61000"}]}}}"#;
    const POSTGRES_WITH_CONTAINER_NAME: &str = r#"{"services":{"postgres":{"container_name":"fixed","ports":[{"target":5432,"published":"61000"}]}}}"#;
    const TIMEOUT: Duration = Duration::from_secs(5);

    struct AllFree;

    impl PortProbe for AllFree {
        fn is_free(&self, _: u16) -> bool {
            true
        }
    }

    struct Fixture {
        state: tempfile::TempDir,
        projects: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                state: tempfile::tempdir().unwrap(),
                projects: tempfile::tempdir().unwrap(),
            }
        }

        fn store(&self) -> RegistryStore {
            let paths = DevslotPaths::from_variables(&PathVariables {
                config_home: Some(OsString::from(self.state.path().join("config"))),
                state_home: Some(OsString::from(self.state.path().join("state"))),
                home: None,
            })
            .unwrap();
            RegistryStore::new(paths)
        }

        fn registry_text(&self) -> String {
            std::fs::read_to_string(self.state.path().join("state/devslot/registry.toml")).unwrap()
        }

        fn project(&self, folder: &str, files: &[(&str, &str)]) -> Checkout {
            let root = self.projects.path().join(folder);
            std::fs::create_dir_all(&root).unwrap();
            for (name, content) in files {
                std::fs::write(root.join(name), content).unwrap();
            }
            Checkout::locate(&root).unwrap()
        }
    }

    fn global_config(geometry: SlotGeometry, is_writing: bool) -> GlobalConfig {
        GlobalConfig {
            slots: geometry,
            write_enabled: is_writing,
            ..GlobalConfig::default()
        }
    }

    fn register_as(
        fixture: &Fixture,
        checkout: &Checkout,
        compose: &dyn ComposeCli,
        global: &GlobalConfig,
        intent: Intent,
    ) -> RegistrationOutcome {
        register(&RegistrationContext {
            checkout,
            compose,
            store: &fixture.store(),
            global,
            probe: &AllFree,
            lock_timeout: TIMEOUT,
            intent,
        })
        .unwrap()
    }

    fn register_in(
        fixture: &Fixture,
        checkout: &Checkout,
        compose: &dyn ComposeCli,
        geometry: SlotGeometry,
    ) -> RegistrationOutcome {
        register_as(
            fixture,
            checkout,
            compose,
            &global_config(geometry, false),
            Intent::Explicit,
        )
    }

    fn register_writing(
        fixture: &Fixture,
        checkout: &Checkout,
        intent: Intent,
    ) -> RegistrationOutcome {
        register_as(
            fixture,
            checkout,
            &postgres_compose(),
            &global_config(SlotGeometry::default(), true),
            intent,
        )
    }

    fn registered(outcome: RegistrationOutcome) -> Registration {
        match outcome {
            RegistrationOutcome::Registered(registration) => registration,
            RegistrationOutcome::Refused(refusal) => panic!("refused: {refusal:?}"),
            RegistrationOutcome::SkippedOptedOut => panic!("skipped: the checkout is opted out"),
        }
    }

    fn postgres_compose() -> FakeComposeCli {
        FakeComposeCli::new(&[("DB_PORT", "")], POSTGRES)
    }

    const TEMPLATE_FILES: [(&str, &str); 2] = [(".env", "PORT=3333\n"), ("dev.compose.yml", "")];

    #[test]
    fn should_give_a_new_checkout_slot_one_with_its_ports_in_discovery_order() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &TEMPLATE_FILES);

        let registration = registered(register_in(
            &fixture,
            &checkout,
            &postgres_compose(),
            SlotGeometry::default(),
        ));

        let ports: Vec<(String, u16)> = registration
            .instance
            .ports
            .iter()
            .map(|assigned| (assigned.variable.clone(), assigned.port))
            .collect();
        assert_eq!(
            (registration.change, registration.instance.slot, ports),
            (
                RegistrationChange::Created,
                1,
                vec![("PORT".to_owned(), 20050), ("DB_PORT".to_owned(), 20051)]
            )
        );
    }

    #[test]
    fn should_announce_the_registration_and_the_compose_project_on_the_first_run_only() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &TEMPLATE_FILES);
        let compose = postgres_compose();

        let first = registered(register_in(
            &fixture,
            &checkout,
            &compose,
            SlotGeometry::default(),
        ));
        let second = registered(register_in(
            &fixture,
            &checkout,
            &compose,
            SlotGeometry::default(),
        ));

        assert_eq!(first.notices().len(), 2);
        assert!(first.notices()[0].starts_with("ds: registered "));
        assert!(first.notices()[0].ends_with("slot 1 (20050-20099)"));
        assert!(first.notices()[1].contains("keep their old project name"));
        assert_eq!(second.notices(), Vec::<String>::new());
    }

    #[test]
    fn should_not_write_the_registry_when_the_checkout_is_already_registered_unchanged() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &TEMPLATE_FILES);
        let compose = postgres_compose();
        register_in(&fixture, &checkout, &compose, SlotGeometry::default());
        let before = fixture.registry_text();
        let modified = std::fs::metadata(fixture.state.path().join("state/devslot/registry.toml"))
            .unwrap()
            .modified()
            .unwrap();

        let second = registered(register_in(
            &fixture,
            &checkout,
            &compose,
            SlotGeometry::default(),
        ));

        let after_modified =
            std::fs::metadata(fixture.state.path().join("state/devslot/registry.toml"))
                .unwrap()
                .modified()
                .unwrap();
        assert_eq!(
            (second.change, fixture.registry_text(), after_modified),
            (RegistrationChange::Unchanged, before, modified)
        );
    }

    #[test]
    fn should_keep_the_ports_and_give_the_new_variable_the_next_one_when_a_variable_appears() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &TEMPLATE_FILES);
        register_in(
            &fixture,
            &checkout,
            &postgres_compose(),
            SlotGeometry::default(),
        );
        std::fs::write(
            checkout.root.join("devslot.toml"),
            "[ports]\nextra = [\"FRONT_PORT\"]\n",
        )
        .unwrap();

        let registration = registered(register_in(
            &fixture,
            &checkout,
            &postgres_compose(),
            SlotGeometry::default(),
        ));

        let ports: Vec<(String, u16)> = registration
            .instance
            .ports
            .iter()
            .map(|assigned| (assigned.variable.clone(), assigned.port))
            .collect();
        assert_eq!(
            (registration.change, ports),
            (
                RegistrationChange::Updated,
                vec![
                    ("PORT".to_owned(), 20050),
                    ("DB_PORT".to_owned(), 20051),
                    ("FRONT_PORT".to_owned(), 20052)
                ]
            )
        );
    }

    #[test]
    fn should_leave_the_other_instances_byte_for_byte_when_one_of_them_is_updated() {
        let fixture = Fixture::new();
        let first = fixture.project("app", &TEMPLATE_FILES);
        let second = fixture.project("other", &TEMPLATE_FILES);
        register_in(
            &fixture,
            &first,
            &postgres_compose(),
            SlotGeometry::default(),
        );
        register_in(
            &fixture,
            &second,
            &postgres_compose(),
            SlotGeometry::default(),
        );
        let hand_edited = fixture
            .registry_text()
            .replace("version = 1\n", "# keep this comment\nversion = 1\n");
        std::fs::write(
            fixture.state.path().join("state/devslot/registry.toml"),
            &hand_edited,
        )
        .unwrap();
        let second_entry_before = hand_edited.split("[[instance]]").nth(2).unwrap().to_owned();
        std::fs::write(
            first.root.join("devslot.toml"),
            "[ports]\nextra = [\"FRONT_PORT\"]\n",
        )
        .unwrap();

        register_in(
            &fixture,
            &first,
            &postgres_compose(),
            SlotGeometry::default(),
        );

        let after = fixture.registry_text();
        assert!(after.starts_with("# keep this comment\nversion = 1\n"));
        assert!(after.contains(&second_entry_before));
        assert!(after.contains("FRONT_PORT"));
    }

    #[test]
    fn should_block_and_register_nothing_when_the_dev_compose_sets_a_container_name() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &TEMPLATE_FILES);
        let compose = FakeComposeCli::new(&[("DB_PORT", "")], POSTGRES_WITH_CONTAINER_NAME);

        let outcome = register_in(&fixture, &checkout, &compose, SlotGeometry::default());

        let RegistrationOutcome::Refused(Refusal::Blocked(findings)) = outcome else {
            panic!("expected a blocked registration");
        };
        assert_eq!(findings[0].check, Check::ContainerName);
        assert!(
            !fixture
                .state
                .path()
                .join("state/devslot/registry.toml")
                .exists()
        );
    }

    #[test]
    fn should_block_when_the_committed_config_forces_a_managed_port() {
        let fixture = Fixture::new();
        let checkout = fixture.project(
            "app",
            &[
                (".env", "PORT=3333\n"),
                ("devslot.toml", "[env]\nPORT = \"3333\"\n"),
            ],
        );

        let outcome = register_in(
            &fixture,
            &checkout,
            &FakeComposeCli::new(&[], "{}"),
            SlotGeometry::default(),
        );

        assert!(matches!(
            outcome,
            RegistrationOutcome::Refused(Refusal::Blocked(_))
        ));
    }

    #[test]
    fn should_return_the_warnings_without_blocking_when_the_checks_only_warn() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &TEMPLATE_FILES);
        let hardcoded = r#"{"services":{"postgres":{"ports":[{"target":5432,"published":"61000"},{"target":6379,"published":"6379"}]}}}"#;
        let compose = FakeComposeCli::new(&[("DB_PORT", "")], hardcoded);

        let registration = registered(register_in(
            &fixture,
            &checkout,
            &compose,
            SlotGeometry::default(),
        ));

        assert_eq!(
            registration
                .warnings
                .iter()
                .map(|finding| finding.check)
                .collect::<Vec<_>>(),
            [Check::HardcodedPort]
        );
    }

    #[test]
    fn should_register_a_checkout_without_any_compose_file() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &[(".env", "PORT=3333\n")]);

        let registration = registered(register_in(
            &fixture,
            &checkout,
            &FakeComposeCli::new(&[], "{}"),
            SlotGeometry::default(),
        ));

        assert_eq!(registration.instance.port_of("PORT"), Some(20050));
    }

    #[test]
    fn should_refuse_when_the_instance_needs_more_ports_than_a_slot_holds() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &TEMPLATE_FILES);
        let geometry = GlobalConfig::from_text(
            "[slots]\nfirst_port = 20000\nlast_port = 20099\nsize = 1\n",
            Path::new("/c"),
        )
        .unwrap()
        .slots;

        let outcome = register_in(&fixture, &checkout, &postgres_compose(), geometry);

        assert!(matches!(
            outcome,
            RegistrationOutcome::Refused(Refusal::NoRoom(AllocationError::SlotFull {
                requested: 2,
                capacity: 1
            }))
        ));
    }

    #[test]
    fn should_lengthen_the_hash_when_another_checkout_already_has_the_derived_name() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &TEMPLATE_FILES);
        let taken = derive_name("app", &checkout.root, DEFAULT_HASH_LENGTH);
        fixture
            .store()
            .locked(TIMEOUT, |locked| {
                locked.save_instance(&InstanceRecord {
                    path: "/elsewhere/app".into(),
                    name: taken.clone(),
                    slot: 1,
                    range: crate::port_range::PortRange {
                        first: 20050,
                        last: 20099,
                    },
                    ports: vec![],
                })
            })
            .unwrap();

        let registration = registered(register_in(
            &fixture,
            &checkout,
            &postgres_compose(),
            SlotGeometry::default(),
        ));

        assert_eq!(
            registration.instance.name,
            derive_name(
                "app",
                &checkout.root,
                DEFAULT_HASH_LENGTH + HASH_LENGTH_STEP
            )
        );
        assert_ne!(registration.instance.name, taken);
    }

    #[test]
    fn should_name_the_instance_after_the_folder_and_the_hash_when_outside_git() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &TEMPLATE_FILES);

        let registration = registered(register_in(
            &fixture,
            &checkout,
            &postgres_compose(),
            SlotGeometry::default(),
        ));

        assert_eq!(
            registration.instance.name,
            derive_name("app", &checkout.root, DEFAULT_HASH_LENGTH)
        );
    }

    #[test]
    fn should_give_two_checkouts_two_disjoint_slots() {
        let fixture = Fixture::new();
        let first = fixture.project("app", &TEMPLATE_FILES);
        let second = fixture.project("other", &TEMPLATE_FILES);

        let one = registered(register_in(
            &fixture,
            &first,
            &postgres_compose(),
            SlotGeometry::default(),
        ));
        let two = registered(register_in(
            &fixture,
            &second,
            &postgres_compose(),
            SlotGeometry::default(),
        ));

        assert_eq!((one.instance.slot, two.instance.slot), (1, 2));
    }

    #[test]
    fn should_describe_the_config_file_to_edit_when_no_room_is_left() {
        let message = Refusal::no_room_message(
            &AllocationError::SlotFull {
                requested: 5,
                capacity: 3,
            },
            Path::new("/c/config.toml"),
        );

        assert_eq!(
            message,
            "5 ports requested, slots hold 3; raise [slots] size in /c/config.toml"
        );
    }

    #[test]
    fn should_block_when_a_local_forced_port_belongs_to_another_instance() {
        let fixture = Fixture::new();
        let first = fixture.project("app", &TEMPLATE_FILES);
        let second = fixture.project(
            "other",
            &[
                (".env", "PORT=3333\n"),
                ("dev.compose.yml", ""),
                ("devslot.local.toml", "[env]\nDB_PORT = \"20051\"\n"),
            ],
        );
        register_in(
            &fixture,
            &first,
            &postgres_compose(),
            SlotGeometry::default(),
        );

        let outcome = register_in(
            &fixture,
            &second,
            &postgres_compose(),
            SlotGeometry::default(),
        );

        let RegistrationOutcome::Refused(Refusal::Blocked(findings)) = outcome else {
            panic!("expected a blocked registration");
        };
        assert_eq!(findings[0].check, Check::ForcedPortConflict);
        assert_eq!(fixture.registry_text().matches("[[instance]]").count(), 1);
    }

    #[test]
    fn should_block_when_the_committed_config_sets_a_key_no_env_file_declares() {
        let fixture = Fixture::new();
        let checkout = fixture.project(
            "app",
            &[
                (".env", "PORT=3333\n"),
                ("devslot.toml", "[env]\nFOO = \"x\"\n"),
            ],
        );

        let outcome = register_in(
            &fixture,
            &checkout,
            &FakeComposeCli::new(&[], "{}"),
            SlotGeometry::default(),
        );

        let RegistrationOutcome::Refused(Refusal::Blocked(findings)) = outcome else {
            panic!("expected a blocked registration");
        };
        assert_eq!(findings[0].check, Check::EnvKeyUndeclared);
    }

    fn opt_out(fixture: &Fixture, checkout: &Checkout) {
        fixture
            .store()
            .locked(TIMEOUT, |locked| {
                locked.apply(&[RegistryChange::OptOut(checkout.root.clone())])
            })
            .unwrap();
    }

    fn env_text(checkout: &Checkout) -> String {
        std::fs::read_to_string(checkout.root.join(".env")).unwrap()
    }

    #[test]
    fn should_skip_an_automatic_registration_and_change_nothing_when_the_checkout_is_opted_out() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &TEMPLATE_FILES);
        opt_out(&fixture, &checkout);
        let before = fixture.registry_text();

        let outcome = register_writing(&fixture, &checkout, Intent::Automatic);

        assert!(matches!(outcome, RegistrationOutcome::SkippedOptedOut));
        assert_eq!(
            (fixture.registry_text(), env_text(&checkout)),
            (before, "PORT=3333\n".to_owned())
        );
    }

    #[test]
    fn should_register_again_and_clear_the_opt_out_when_the_registration_is_explicit() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &TEMPLATE_FILES);
        opt_out(&fixture, &checkout);

        let registration = registered(register_writing(&fixture, &checkout, Intent::Explicit));

        assert!(registration.cleared_opt_out);
        assert!(
            registration.notices()[0].ends_with("was opted out by ds rm; registering it again")
        );
        assert_eq!(
            fixture.store().read().unwrap().opted_out,
            Vec::<PathBuf>::new()
        );
        assert_eq!(fixture.registry_text().matches("[[instance]]").count(), 1);
    }

    #[test]
    fn should_write_the_block_and_announce_it_when_writing_is_enabled() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &TEMPLATE_FILES);

        let registration = registered(register_writing(&fixture, &checkout, Intent::Explicit));

        let text = env_text(&checkout);
        assert!(text.starts_with("PORT=3333\n# >>> devslot"));
        assert!(text.contains(&format!(
            "COMPOSE_PROJECT_NAME={}\n",
            registration.instance.name
        )));
        assert!(
            registration
                .notices()
                .last()
                .unwrap()
                .starts_with("ds: wrote the devslot block in ")
        );
    }

    #[test]
    fn should_write_nothing_to_the_env_when_writing_is_disabled() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &TEMPLATE_FILES);

        let registration = registered(register_in(
            &fixture,
            &checkout,
            &postgres_compose(),
            SlotGeometry::default(),
        ));

        assert_eq!(
            (env_text(&checkout), registration.env_lines),
            ("PORT=3333\n".to_owned(), Vec::<String>::new())
        );
    }

    #[test]
    fn should_warn_about_the_stale_block_when_writing_is_disabled_and_the_env_has_one() {
        let fixture = Fixture::new();
        let checkout = fixture.project(
            "app",
            &[
                (
                    ".env",
                    "PORT=3333\n# >>> devslot: x >>>\nA=1\n# <<< devslot <<<\n",
                ),
                ("dev.compose.yml", ""),
            ],
        );

        let registration = registered(register_in(
            &fixture,
            &checkout,
            &postgres_compose(),
            SlotGeometry::default(),
        ));

        assert_eq!(registration.env_lines.len(), 1);
        assert!(registration.env_lines[0].contains("writing is disabled; the devslot block in "));
    }

    #[test]
    fn should_block_when_writing_is_enabled_and_there_is_nothing_to_create_the_env_from() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &[("dev.compose.yml", "")]);

        let outcome = register_writing(&fixture, &checkout, Intent::Explicit);

        let RegistrationOutcome::Refused(Refusal::Blocked(findings)) = outcome else {
            panic!("expected a blocked registration");
        };
        assert_eq!(findings[0].check, Check::EnvMissing);
        assert!(
            !fixture
                .state
                .path()
                .join("state/devslot/registry.toml")
                .exists()
        );
    }

    #[test]
    fn should_create_the_env_and_find_the_port_of_the_source_on_the_first_registration() {
        let fixture = Fixture::new();
        let checkout = fixture.project(
            "app",
            &[(".env.example", "PORT=3333\n"), ("dev.compose.yml", "")],
        );

        let registration = registered(register_writing(&fixture, &checkout, Intent::Explicit));

        assert_eq!(registration.instance.port_of("PORT"), Some(20050));
        assert!(
            registration
                .notices()
                .iter()
                .any(|line| line.starts_with("ds: created "))
        );
        assert!(env_text(&checkout).starts_with("PORT=3333\n"));
    }

    #[test]
    fn should_block_and_leave_the_env_alone_when_git_does_not_ignore_it() {
        let fixture = Fixture::new();
        let checkout = fixture.project("app", &TEMPLATE_FILES);
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&checkout.root)
            .status()
            .unwrap();
        assert!(status.success());
        let checkout = Checkout::locate(&checkout.root).unwrap();

        let outcome = register_writing(&fixture, &checkout, Intent::Explicit);

        let RegistrationOutcome::Refused(Refusal::Blocked(findings)) = outcome else {
            panic!("expected a blocked registration");
        };
        assert_eq!(findings[0].check, Check::EnvNotIgnored);
        assert_eq!(env_text(&checkout), "PORT=3333\n");
    }
}
