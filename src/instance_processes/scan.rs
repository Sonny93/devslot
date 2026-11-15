//! Deciding which processes belong to an instance: the ones that run in its
//! worktree and either listen on one of its ports or carry its
//! `DEVSLOT_INSTANCE` marker. Everything else in the worktree is only listed.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::proc_root::ProcRoot;
use super::{InstanceScope, ProcessSummary};

const INSTANCE_VARIABLE: &str = "DEVSLOT_INSTANCE";

/// What a scan needs to know about the machine and the instance.
#[derive(Debug, Clone, Copy)]
pub struct ScanContext<'a> {
    /// The `/proc` tree to read.
    pub root: &'a ProcRoot,
    /// The process that runs `ds`.
    pub own_pid: i32,
    /// The real uid of `ds`.
    pub own_uid: u32,
    /// The canonical root of the worktree.
    pub worktree: &'a Path,
    /// The instance registered for the worktree, if any.
    pub scope: Option<&'a InstanceScope>,
}

/// The processes found in a worktree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// Every process to signal: the ones that belong to the instance and
    /// their descendants that run in the worktree.
    pub targets: Vec<ProcessSummary>,
    /// The other processes of the worktree, never signalled.
    pub others: Vec<ProcessSummary>,
}

/// `ds` and its ancestors: never signalled.
struct Ancestry {
    pids: HashSet<i32>,
}

impl Ancestry {
    fn of(root: &ProcRoot, own_pid: i32) -> Self {
        let mut pids = HashSet::new();
        let mut next = Some(own_pid);
        while let Some(pid) = next.filter(|pid| *pid > 0 && !pids.contains(pid)) {
            pids.insert(pid);
            next = root.stat(pid).map(|stat| stat.parent);
        }
        Self { pids }
    }
}

/// Lists the processes of the worktree and sorts them into targets and others.
pub fn scan(context: &ScanContext) -> Scan {
    let ancestry = Ancestry::of(context.root, context.own_pid);
    let inodes = listening_inodes(context);
    let candidates: Vec<i32> = context
        .root
        .process_ids()
        .into_iter()
        .filter(|pid| is_candidate(context, &ancestry, *pid))
        .collect();
    let roots: Vec<i32> = candidates
        .iter()
        .copied()
        .filter(|pid| belongs_to_instance(context, &inodes, *pid))
        .collect();
    let targeted = with_descendants(context, &roots);
    let mut found = Scan::default();
    for pid in candidates {
        let summary = summary_of(context, pid);
        if targeted.contains(&pid) {
            found.targets.push(summary);
        } else {
            found.others.push(summary);
        }
    }
    found.targets.sort_by_key(|target| target.pid);
    found.others.sort_by_key(|other| other.pid);
    found
}

/// Whether `pid` is a process to signal right now, by the same rules as [`scan`].
pub fn is_target(context: &ScanContext, pid: i32) -> bool {
    scan(context).targets.iter().any(|target| target.pid == pid)
}

fn listening_inodes(context: &ScanContext) -> HashSet<u64> {
    context
        .scope
        .map(|scope| context.root.listening_inodes(&scope.ports))
        .unwrap_or_default()
}

fn summary_of(context: &ScanContext, pid: i32) -> ProcessSummary {
    ProcessSummary {
        pid,
        comm: context.root.comm(pid),
    }
}

fn is_candidate(context: &ScanContext, ancestry: &Ancestry, pid: i32) -> bool {
    if ancestry.pids.contains(&pid) {
        return false;
    }
    let Some(stat) = context.root.stat(pid) else {
        return false;
    };
    !stat.is_dead() && has_own_uid(context, pid) && runs_in_worktree(context, pid)
}

fn has_own_uid(context: &ScanContext, pid: i32) -> bool {
    context.root.real_uid(pid) == Some(context.own_uid)
}

fn runs_in_worktree(context: &ScanContext, pid: i32) -> bool {
    context
        .root
        .working_directory(pid)
        .is_some_and(|directory| directory.starts_with(context.worktree))
}

fn belongs_to_instance(context: &ScanContext, inodes: &HashSet<u64>, pid: i32) -> bool {
    let Some(scope) = context.scope else {
        return false;
    };
    let marker = format!("{INSTANCE_VARIABLE}={}", scope.name);
    context.root.holds_socket(pid, inodes) || context.root.has_environment_entry(pid, &marker)
}

/// The `roots` and every descendant of theirs, found through the parent
/// links of `/proc/*/stat`. Candidates are filtered by the caller.
fn with_descendants(context: &ScanContext, roots: &[i32]) -> HashSet<i32> {
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for pid in context.root.process_ids() {
        if let Some(stat) = context.root.stat(pid) {
            children.entry(stat.parent).or_default().push(pid);
        }
    }
    let mut found: HashSet<i32> = roots.iter().copied().collect();
    let mut pending: Vec<i32> = roots.to_vec();
    while let Some(pid) = pending.pop() {
        for child in children.get(&pid).into_iter().flatten() {
            if found.insert(*child) {
                pending.push(*child);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::instance_processes::testing::FakeProc;

    const OWN_UID: u32 = 1000;
    const OWN_PID: i32 = 100;
    const INSTANCE: &str = "app-feat-3f2a";
    const PORT: u16 = 20050;

    struct Setup {
        fake: FakeProc,
        worktree: PathBuf,
        scope: InstanceScope,
    }

    fn setup() -> Setup {
        let fake = FakeProc::new();
        let worktree = fake.directory("work/feat");
        fake.add_process(1, 0, 1, "systemd", &fake.directory("/"));
        fake.add_process(50, 1, 50, "claude", &fake.directory("work/feat"));
        fake.add_process(OWN_PID, 50, 50, "ds", &fake.directory("work/feat"));
        Setup {
            fake,
            worktree,
            scope: InstanceScope {
                name: INSTANCE.to_owned(),
                ports: vec![PORT],
            },
        }
    }

    impl Setup {
        fn scan(&self) -> Scan {
            let root = self.fake.root();
            scan(&ScanContext {
                root: &root,
                own_pid: OWN_PID,
                own_uid: OWN_UID,
                worktree: &self.worktree,
                scope: Some(&self.scope),
            })
        }

        fn target_pids(&self) -> Vec<i32> {
            self.scan()
                .targets
                .iter()
                .map(|target| target.pid)
                .collect()
        }

        fn other_pids(&self) -> Vec<i32> {
            self.scan().others.iter().map(|other| other.pid).collect()
        }
    }

    fn marker() -> String {
        format!("{INSTANCE_VARIABLE}={INSTANCE}")
    }

    #[test]
    fn should_target_a_process_that_carries_the_marker_in_the_worktree() {
        let setup = setup();
        setup
            .fake
            .add_process(200, 1, 200, "node", &setup.fake.directory("work/feat/web"));
        setup.fake.set_environment(200, &["A=1", &marker()]);

        assert_eq!(setup.target_pids(), vec![200]);
    }

    #[test]
    fn should_target_a_process_that_listens_on_a_port_of_the_instance() {
        let setup = setup();
        setup
            .fake
            .add_process(200, 1, 200, "node", &setup.fake.directory("work/feat"));
        setup.fake.add_listener(PORT, 777, "0A");
        setup.fake.add_socket(200, 777);

        assert_eq!(setup.target_pids(), vec![200]);
    }

    #[test]
    fn should_only_list_a_process_of_the_worktree_without_marker_or_port() {
        let setup = setup();
        setup
            .fake
            .add_process(200, 1, 200, "tmux", &setup.fake.directory("work/feat"));
        setup
            .fake
            .set_environment(200, &["DEVSLOT_INSTANCE=other-1"]);
        setup.fake.add_listener(PORT + 1, 888, "0A");
        setup.fake.add_socket(200, 888);

        assert_eq!(setup.target_pids(), Vec::<i32>::new());
        assert_eq!(setup.other_pids(), vec![200]);
    }

    #[test]
    fn should_not_target_a_process_that_only_connects_to_the_port() {
        let setup = setup();
        setup
            .fake
            .add_process(200, 1, 200, "curl", &setup.fake.directory("work/feat"));
        setup.fake.add_listener(PORT, 777, "01");
        setup.fake.add_socket(200, 777);

        assert_eq!(setup.target_pids(), Vec::<i32>::new());
    }

    #[test]
    fn should_not_match_a_sibling_folder_that_starts_with_the_same_name() {
        let setup = setup();
        let sibling = setup.fake.directory("work/feat-2");
        setup.fake.add_process(200, 1, 200, "node", &sibling);
        setup.fake.set_environment(200, &[&marker()]);

        assert_eq!(setup.target_pids(), Vec::<i32>::new());
        assert_eq!(setup.other_pids(), Vec::<i32>::new());
    }

    #[test]
    fn should_match_a_working_directory_marked_as_deleted() {
        let setup = setup();
        let gone = setup.fake.directory("work/feat/gone");
        setup.fake.add_process_with_link(
            200,
            1,
            200,
            "node",
            &format!("{} (deleted)", gone.display()),
        );
        setup.fake.set_environment(200, &[&marker()]);

        assert_eq!(setup.target_pids(), vec![200]);
    }

    #[test]
    fn should_never_target_ds_or_its_ancestors() {
        let setup = setup();
        setup.fake.set_environment(50, &[&marker()]);
        setup.fake.set_environment(OWN_PID, &[&marker()]);

        assert_eq!(setup.target_pids(), Vec::<i32>::new());
        assert_eq!(setup.other_pids(), Vec::<i32>::new());
    }

    #[test]
    fn should_ignore_a_process_of_another_user() {
        let setup = setup();
        setup
            .fake
            .add_process(200, 1, 200, "node", &setup.fake.directory("work/feat"));
        setup.fake.set_environment(200, &[&marker()]);
        setup.fake.set_uid(200, 0);

        assert_eq!(setup.target_pids(), Vec::<i32>::new());
        assert_eq!(setup.other_pids(), Vec::<i32>::new());
    }

    #[test]
    fn should_ignore_a_zombie() {
        let setup = setup();
        setup
            .fake
            .add_process(200, 1, 200, "node", &setup.fake.directory("work/feat"));
        setup.fake.set_environment(200, &[&marker()]);
        setup.fake.set_state(200, 'Z');

        assert_eq!(setup.target_pids(), Vec::<i32>::new());
    }

    #[test]
    fn should_signal_only_the_target_when_its_group_leader_is_not_a_target() {
        let setup = setup();
        setup
            .fake
            .add_process(200, 1, 200, "nvim", &setup.fake.directory("work/feat"));
        setup
            .fake
            .add_process(201, 200, 200, "node", &setup.fake.directory("work/feat"));
        setup.fake.add_listener(PORT, 777, "0A");
        setup.fake.add_socket(201, 777);

        assert_eq!(setup.target_pids(), vec![201]);
        assert_eq!(setup.other_pids(), vec![200]);
    }

    #[test]
    fn should_signal_a_descendant_of_a_target_that_runs_in_the_worktree() {
        let setup = setup();
        setup
            .fake
            .add_process(200, 1, 200, "just", &setup.fake.directory("work/feat"));
        setup.fake.set_environment(200, &[&marker()]);
        setup.fake.add_process(
            201,
            200,
            200,
            "node",
            &setup.fake.directory("work/feat/web"),
        );
        setup
            .fake
            .add_process(202, 201, 200, "esbuild", &setup.fake.directory("work/feat"));

        assert_eq!(setup.target_pids(), vec![200, 201, 202]);
    }

    #[test]
    fn should_not_signal_a_descendant_of_a_target_whose_directory_is_outside() {
        let setup = setup();
        setup
            .fake
            .add_process(200, 1, 200, "just", &setup.fake.directory("work/feat"));
        setup.fake.set_environment(200, &[&marker()]);
        setup
            .fake
            .add_process(201, 200, 200, "ssh", &setup.fake.directory("elsewhere"));
        setup
            .fake
            .add_process(202, 201, 200, "node", &setup.fake.directory("work/feat"));

        assert_eq!(setup.target_pids(), vec![200, 202]);
        assert_eq!(setup.other_pids(), Vec::<i32>::new());
    }

    #[test]
    fn should_not_target_anything_without_a_registered_instance() {
        let setup = setup();
        setup
            .fake
            .add_process(200, 1, 200, "node", &setup.fake.directory("work/feat"));
        setup.fake.set_environment(200, &[&marker()]);
        let root = setup.fake.root();

        let found = scan(&ScanContext {
            root: &root,
            own_pid: OWN_PID,
            own_uid: OWN_UID,
            worktree: &setup.worktree,
            scope: None,
        });

        assert_eq!(found.targets, Vec::new());
        assert_eq!(found.others.len(), 1);
    }
}
