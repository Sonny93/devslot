//! The worktree guard: whether a path lies in a main checkout where agents
//! must work in a linked worktree. It decides from the `[worktrees] require`
//! scope, one git call and the `devslot.local.toml` switch of the checkout, and
//! it answers "required" only on a positive finding: every error is returned
//! to the caller, which must not turn it into a refusal.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use thiserror::Error;

use crate::checkout::{Checkout, CheckoutError, git_failure, run_git_within};
use crate::deadline::{BudgetExhausted, Deadline, timeout_within};
use crate::project_config::LOCAL_CONFIG_FILE;
use crate::require_scope::RequireScope;
use crate::worktree_require_override::{OverrideError, is_guard_disabled_locally};

const GIT_TIMEOUT: Duration = Duration::from_secs(5);
const NOT_A_REPOSITORY_MESSAGE: &str = "not a git repository";
const NOT_A_WORK_TREE_MESSAGE: &str = "must be run in a work tree";
const MAX_SYMLINK_HOPS: usize = 40;
const TRACKED_EXIT_CODE: i32 = 0;
const NOT_TRACKED_EXIT_CODE: i32 = 1;
const LAYOUT_ARGUMENTS: [&str; 5] = [
    "rev-parse",
    "--path-format=absolute",
    "--show-toplevel",
    "--absolute-git-dir",
    "--git-common-dir",
];

/// Why the guard cannot decide. None of these is a refusal.
#[derive(Debug, Error)]
pub enum GuardError {
    /// Git could not be run, answered an error, or ran out of time.
    #[error(transparent)]
    Git(#[from] CheckoutError),
    /// The time budget of the hook ran out.
    #[error(transparent)]
    Budget(#[from] BudgetExhausted),
    /// `devslot.local.toml` cannot be read for its switch.
    #[error(transparent)]
    Override(#[from] OverrideError),
    /// A path goes through more symbolic links than the system follows.
    #[error("{} goes through more than 40 symbolic links", .0.display())]
    TooManySymlinks(PathBuf),
    /// Git answered something else than the three paths that were asked for.
    #[error("git rev-parse answered {0} line(s) instead of 3")]
    UnexpectedGitOutput(usize),
}

/// What the guard needs besides the path: the scope, the home folder for `~`,
/// and the time the decision may take.
#[derive(Debug, Clone, Copy)]
pub struct GuardContext<'a> {
    /// `[worktrees] require` and `require_exclude`.
    pub scope: &'a RequireScope,
    /// The home folder, for `~` in the patterns.
    pub home: Option<&'a Path>,
    /// The budget of the hook.
    pub deadline: Deadline,
}

/// The main checkout that a path is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MainCheckout {
    /// Its working tree root.
    pub toplevel: PathBuf,
    /// The common git directory of its repository.
    pub common_dir: PathBuf,
}

/// What the guard found about a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardVerdict {
    /// Not covered, not in a git work tree, in a linked worktree, or switched
    /// off for the checkout: nothing is required.
    NotRequired,
    /// The path is in a main checkout that must not be worked in directly.
    MainCheckout(MainCheckout),
}

/// The three paths git gives for a folder of a work tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitLayout {
    /// The working tree root.
    pub toplevel: PathBuf,
    /// The git directory of this working tree.
    pub git_dir: PathBuf,
    /// The git directory shared by every working tree of the repository.
    pub common_dir: PathBuf,
}

impl GitLayout {
    /// Whether the working tree is the main one: a linked worktree has its own
    /// git directory below the common one.
    pub fn is_main_checkout(&self) -> bool {
        self.git_dir == self.common_dir
    }
}

/// Whether `path` (relative paths are read from `base`) lies in a main
/// checkout covered by the scope. With an empty scope nothing is touched: no
/// file, no git. Otherwise: the glob check on the path, one git call from its
/// nearest existing folder, then the `devslot.local.toml` switch.
pub fn locate_required_main_checkout(
    context: &GuardContext<'_>,
    path: &Path,
    base: &Path,
) -> Result<GuardVerdict, GuardError> {
    if context.scope.is_empty() {
        return Ok(GuardVerdict::NotRequired);
    }
    let mut first_failure = None;
    for target in resolve_targets(path, base) {
        let verdict = target.and_then(|target| verdict_for(context, &target));
        match verdict {
            Ok(GuardVerdict::NotRequired) => {}
            Ok(required) => return Ok(required),
            Err(error) => first_failure = first_failure.or(Some(error)),
        }
    }
    first_failure.map_or(Ok(GuardVerdict::NotRequired), Err)
}

fn verdict_for(context: &GuardContext<'_>, target: &Path) -> Result<GuardVerdict, GuardError> {
    if !context.scope.covers(target, context.home) {
        return Ok(GuardVerdict::NotRequired);
    }
    let Some(directory) = nearest_existing_directory(target) else {
        return Ok(GuardVerdict::NotRequired);
    };
    let Some(layout) = read_layout(directory, context.deadline)? else {
        return Ok(GuardVerdict::NotRequired);
    };
    if !layout.is_main_checkout()
        || is_local_switch_effective(&layout.toplevel, Some(context.deadline))?
    {
        return Ok(GuardVerdict::NotRequired);
    }
    Ok(GuardVerdict::MainCheckout(MainCheckout {
        toplevel: layout.toplevel,
        common_dir: layout.common_dir,
    }))
}

/// Whether the guard applies to a checkout, and why not when it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardStatus {
    /// `[worktrees] require` is empty: the guard is off everywhere.
    NotConfigured,
    /// The checkout is not in a git repository or is outside the scope.
    NotCovered,
    /// A covered linked worktree: the place to work in.
    LinkedWorktree,
    /// A covered main checkout that `devslot.local.toml` excludes.
    DisabledLocally,
    /// A covered main checkout: writes and environments are refused.
    Active,
}

/// The status of the guard for `checkout`, from the scope and the local
/// switch, without any git call of its own.
pub fn guard_status(
    scope: &RequireScope,
    home: Option<&Path>,
    checkout: &Checkout,
) -> Result<GuardStatus, GuardError> {
    if scope.is_empty() {
        return Ok(GuardStatus::NotConfigured);
    }
    if !checkout.is_git || !scope.covers(&checkout.root, home) {
        return Ok(GuardStatus::NotCovered);
    }
    if checkout.main_root.is_some() {
        return Ok(GuardStatus::LinkedWorktree);
    }
    if is_local_switch_effective(&checkout.root, None)? {
        return Ok(GuardStatus::DisabledLocally);
    }
    Ok(GuardStatus::Active)
}

/// Whether `devslot.local.toml` of the checkout at `root` switches the guard
/// off. A file that git tracks came with the repository, not from the user, so
/// it counts as absent whatever it holds. Git is asked only when the file
/// exists.
pub fn is_local_switch_effective(
    root: &Path,
    deadline: Option<Deadline>,
) -> Result<bool, GuardError> {
    if std::fs::symlink_metadata(root.join(LOCAL_CONFIG_FILE)).is_err() {
        return Ok(false);
    }
    if is_tracked(root, LOCAL_CONFIG_FILE, deadline)? {
        return Ok(false);
    }
    Ok(is_guard_disabled_locally(root)?)
}

/// Whether git tracks `file`, given relative to `root`.
pub fn is_tracked(root: &Path, file: &str, deadline: Option<Deadline>) -> Result<bool, GuardError> {
    let timeout = timeout_within(deadline, GIT_TIMEOUT)?;
    let output = run_git_within(root, ["ls-files", "--error-unmatch", "--", file], timeout)?;
    match output.status.code() {
        Some(TRACKED_EXIT_CODE) => Ok(true),
        Some(NOT_TRACKED_EXIT_CODE) => Ok(false),
        _ => Err(git_failure("ls-files", &output).into()),
    }
}

/// Every place `path` can mean: its textual form (see [`resolve_target`]) and
/// the one the system opens, where each `..` applies to the folder reached
/// after following the component before it. Identical forms give one entry.
/// One result per form, so a form that cannot be resolved does not hide the
/// other.
pub fn resolve_targets(path: &Path, base: &Path) -> Vec<Result<PathBuf, GuardError>> {
    let textual = resolve_target(path, base);
    let on_disk = resolve_on_disk(&base.join(path));
    match (&textual, &on_disk) {
        (Ok(first), Ok(second)) if first == second => vec![textual],
        _ => vec![textual, on_disk],
    }
}

enum Part {
    Root,
    Parent,
    Name(OsString),
}

fn resolve_on_disk(raw: &Path) -> Result<PathBuf, GuardError> {
    let mut pending: VecDeque<Part> = parts_of(raw).collect();
    let mut resolved = PathBuf::new();
    let mut hops = 0;
    while let Some(part) = pending.pop_front() {
        match part {
            Part::Root => resolved = PathBuf::from("/"),
            Part::Parent => {
                resolved.pop();
            }
            Part::Name(name) => {
                let candidate = resolved.join(&name);
                let Some(target) = link_target(&candidate) else {
                    resolved = candidate;
                    continue;
                };
                hops += 1;
                if hops > MAX_SYMLINK_HOPS {
                    return Err(GuardError::TooManySymlinks(candidate));
                }
                let parts: Vec<Part> = parts_of(&target).collect();
                for part in parts.into_iter().rev() {
                    pending.push_front(part);
                }
            }
        }
    }
    Ok(resolved)
}

fn parts_of(path: &Path) -> impl Iterator<Item = Part> + '_ {
    path.components().filter_map(|component| match component {
        Component::RootDir => Some(Part::Root),
        Component::ParentDir => Some(Part::Parent),
        Component::Normal(name) => Some(Part::Name(name.to_owned())),
        Component::CurDir | Component::Prefix(_) => None,
    })
}

fn link_target(path: &Path) -> Option<PathBuf> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_symlink() {
        return None;
    }
    std::fs::read_link(path).ok()
}

/// The textual absolute form of `path`: relative to `base` when relative, with the
/// nearest existing ancestor canonical and the part that does not exist yet
/// appended, so a file about to be created resolves like one that exists. A
/// symbolic link met on the way, dangling or not, is followed.
pub fn resolve_target(path: &Path, base: &Path) -> Result<PathBuf, GuardError> {
    let mut current = normalized(&base.join(path));
    for _ in 0..=MAX_SYMLINK_HOPS {
        match resolve_once(&current) {
            Step::Resolved(resolved) => return Ok(resolved),
            Step::Followed(next) => current = next,
        }
    }
    Err(GuardError::TooManySymlinks(current))
}

enum Step {
    Resolved(PathBuf),
    Followed(PathBuf),
}

fn resolve_once(path: &Path) -> Step {
    for ancestor in path.ancestors() {
        let Ok(metadata) = std::fs::symlink_metadata(ancestor) else {
            continue;
        };
        let rest = path.strip_prefix(ancestor).unwrap_or(Path::new(""));
        if metadata.file_type().is_symlink() {
            return Step::Followed(followed_link(ancestor, rest));
        }
        let canonical = ancestor
            .canonicalize()
            .unwrap_or_else(|_| ancestor.to_path_buf());
        return Step::Resolved(normalized(&canonical.join(rest)));
    }
    Step::Resolved(normalized(path))
}

fn followed_link(link: &Path, rest: &Path) -> PathBuf {
    let target = std::fs::read_link(link).unwrap_or_else(|_| link.to_path_buf());
    let parent = link.parent().unwrap_or(Path::new("/"));
    normalized(&parent.join(target).join(rest))
}

fn normalized(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            other => result.push(other),
        }
    }
    result
}

/// The nearest ancestor of `path`, itself included, that is a folder.
pub fn nearest_existing_directory(path: &Path) -> Option<&Path> {
    path.ancestors().find(|ancestor| ancestor.is_dir())
}

/// One `git rev-parse` call from `directory`. `None` when it is not in a git
/// work tree: no repository, or a bare one.
pub fn read_layout(directory: &Path, deadline: Deadline) -> Result<Option<GitLayout>, GuardError> {
    let timeout = timeout_within(Some(deadline), GIT_TIMEOUT)?;
    let output = run_git_within(directory, LAYOUT_ARGUMENTS, timeout)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains(NOT_A_REPOSITORY_MESSAGE) || stderr.contains(NOT_A_WORK_TREE_MESSAGE) {
            return Ok(None);
        }
        return Err(git_failure("rev-parse", &output).into());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().collect();
    let [toplevel, git_dir, common_dir] = lines.as_slice() else {
        return Err(GuardError::UnexpectedGitOutput(lines.len()));
    };
    Ok(Some(GitLayout {
        toplevel: PathBuf::from(toplevel),
        git_dir: PathBuf::from(git_dir),
        common_dir: PathBuf::from(common_dir),
    }))
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use pretty_assertions::assert_eq;

    use super::*;

    const BUDGET: Duration = Duration::from_secs(30);

    fn git(directory: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?}");
    }

    fn repository() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap().join("app");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "initial",
            ],
        );
        (directory, root)
    }

    fn scope_of(directory: &Path) -> RequireScope {
        RequireScope::new(vec![format!("{}/**", directory.display())], Vec::new())
    }

    fn verdict(scope: &RequireScope, path: &Path, base: &Path) -> GuardVerdict {
        let context = GuardContext {
            scope,
            home: None,
            deadline: Deadline::starting_now(BUDGET),
        };
        locate_required_main_checkout(&context, path, base).unwrap()
    }

    #[test]
    fn should_require_a_worktree_when_the_path_is_in_a_covered_main_checkout() {
        let (directory, root) = repository();
        let scope = scope_of(directory.path());

        let found = verdict(&scope, &root.join("src/new.rs"), &root);

        assert_eq!(
            found,
            GuardVerdict::MainCheckout(MainCheckout {
                toplevel: root.clone(),
                common_dir: root.join(".git"),
            })
        );
    }

    #[test]
    fn should_resolve_a_relative_path_against_the_base() {
        let (directory, root) = repository();
        let scope = scope_of(directory.path());

        let found = verdict(&scope, Path::new("src/new.rs"), &root);

        assert!(matches!(found, GuardVerdict::MainCheckout(_)));
    }

    #[test]
    fn should_not_require_a_worktree_when_the_path_is_in_a_linked_worktree() {
        let (directory, root) = repository();
        let linked = directory.path().canonicalize().unwrap().join("linked");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat",
                linked.to_str().unwrap(),
            ],
        );
        let scope = scope_of(directory.path());

        let found = verdict(&scope, &linked.join("file.rs"), &linked);

        assert_eq!(found, GuardVerdict::NotRequired);
    }

    #[test]
    fn should_not_require_a_worktree_when_the_path_is_outside_a_repository() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let scope = scope_of(&base);

        let found = verdict(&scope, &base.join("notes.txt"), &base);

        assert_eq!(found, GuardVerdict::NotRequired);
    }

    #[test]
    fn should_not_require_a_worktree_when_the_repository_is_bare() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        git(&base, &["init", "-q", "--bare", "bare.git"]);
        let scope = scope_of(&base);

        let found = verdict(&scope, &base.join("bare.git/hooks/new"), &base);

        assert_eq!(found, GuardVerdict::NotRequired);
    }

    #[test]
    fn should_not_require_a_worktree_when_the_path_is_outside_the_globs() {
        let (_directory, root) = repository();
        let elsewhere = tempfile::tempdir().unwrap();
        let scope = scope_of(elsewhere.path());

        let found = verdict(&scope, &root.join("file.rs"), &root);

        assert_eq!(found, GuardVerdict::NotRequired);
    }

    #[test]
    fn should_not_require_a_worktree_when_the_local_file_switches_the_guard_off() {
        let (directory, root) = repository();
        std::fs::write(
            root.join("devslot.local.toml"),
            "[worktrees]\nrequire = false\n",
        )
        .unwrap();
        let scope = scope_of(directory.path());

        let found = verdict(&scope, &root.join("file.rs"), &root);

        assert_eq!(found, GuardVerdict::NotRequired);
    }

    #[test]
    fn should_require_a_worktree_when_only_the_committed_file_switches_the_guard_off() {
        let (directory, root) = repository();
        std::fs::write(root.join("devslot.toml"), "[worktrees]\nrequire = false\n").unwrap();
        let scope = scope_of(directory.path());

        let found = verdict(&scope, &root.join("file.rs"), &root);

        assert!(matches!(found, GuardVerdict::MainCheckout(_)));
    }

    #[test]
    fn should_fail_instead_of_requiring_when_the_local_file_cannot_be_parsed() {
        let (directory, root) = repository();
        std::fs::write(root.join("devslot.local.toml"), "[worktrees\n").unwrap();
        let scope = scope_of(directory.path());
        let context = GuardContext {
            scope: &scope,
            home: None,
            deadline: Deadline::starting_now(BUDGET),
        };

        let failure = locate_required_main_checkout(&context, &root.join("file.rs"), &root);

        assert!(matches!(failure, Err(GuardError::Override(_))));
    }

    #[test]
    fn should_touch_nothing_when_the_scope_is_empty() {
        let scope = RequireScope::default();
        let context = GuardContext {
            scope: &scope,
            home: None,
            deadline: Deadline::starting_now(Duration::ZERO),
        };

        let found =
            locate_required_main_checkout(&context, Path::new("/no/such/file"), Path::new("/"));

        assert_eq!(found.unwrap(), GuardVerdict::NotRequired);
    }

    #[test]
    fn should_fail_when_the_budget_is_exhausted() {
        let (directory, root) = repository();
        let scope = scope_of(directory.path());
        let context = GuardContext {
            scope: &scope,
            home: None,
            deadline: Deadline::starting_now(Duration::ZERO),
        };

        let failure = locate_required_main_checkout(&context, &root.join("file.rs"), &root);

        assert!(matches!(failure, Err(GuardError::Budget(_))));
    }

    #[test]
    fn should_resolve_a_file_that_does_not_exist_below_the_canonical_ancestor() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&base, &link).unwrap();

        let resolved = resolve_target(Path::new("link/new/./deep/../file.rs"), &base).unwrap();

        assert_eq!(resolved, base.join("new/file.rs"));
    }

    fn status_in(directory: &Path, scope: &RequireScope) -> GuardStatus {
        let checkout = Checkout::locate(directory).unwrap();
        guard_status(scope, None, &checkout).unwrap()
    }

    #[test]
    fn should_report_the_guard_as_not_configured_when_the_scope_is_empty() {
        let (_directory, root) = repository();

        assert_eq!(
            status_in(&root, &RequireScope::default()),
            GuardStatus::NotConfigured
        );
    }

    #[test]
    fn should_report_the_guard_as_active_in_a_covered_main_checkout() {
        let (directory, root) = repository();

        assert_eq!(
            status_in(&root, &scope_of(directory.path())),
            GuardStatus::Active
        );
    }

    #[test]
    fn should_report_a_linked_worktree_and_an_uncovered_checkout() {
        let (directory, root) = repository();
        let linked = directory.path().canonicalize().unwrap().join("linked");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat",
                linked.to_str().unwrap(),
            ],
        );
        let elsewhere = tempfile::tempdir().unwrap();

        assert_eq!(
            status_in(&linked, &scope_of(directory.path())),
            GuardStatus::LinkedWorktree
        );
        assert_eq!(
            status_in(&root, &scope_of(elsewhere.path())),
            GuardStatus::NotCovered
        );
    }

    #[test]
    fn should_report_the_guard_as_disabled_when_the_local_file_says_so() {
        let (directory, root) = repository();
        std::fs::write(
            root.join("devslot.local.toml"),
            "[worktrees]\nrequire = false\n",
        )
        .unwrap();

        assert_eq!(
            status_in(&root, &scope_of(directory.path())),
            GuardStatus::DisabledLocally
        );
    }

    fn commit_local_file(root: &Path, text: &str) {
        std::fs::write(root.join("devslot.local.toml"), text).unwrap();
        git(root, &["add", "devslot.local.toml"]);
        git(
            root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "-m",
                "local",
            ],
        );
    }

    #[test]
    fn should_require_a_worktree_when_a_tracked_local_file_switches_the_guard_off() {
        let (directory, root) = repository();
        commit_local_file(&root, "[worktrees]\nrequire = false\n");
        let scope = scope_of(directory.path());

        let found = verdict(&scope, &root.join("file.rs"), &root);

        assert!(matches!(found, GuardVerdict::MainCheckout(_)));
    }

    #[test]
    fn should_require_a_worktree_instead_of_failing_when_a_tracked_local_file_is_malformed() {
        let (directory, root) = repository();
        commit_local_file(&root, "[worktrees\n");
        let scope = scope_of(directory.path());

        let found = verdict(&scope, &root.join("file.rs"), &root);

        assert!(matches!(found, GuardVerdict::MainCheckout(_)));
    }

    #[test]
    fn should_report_a_tracked_local_file_as_not_disabling_the_status() {
        let (directory, root) = repository();
        commit_local_file(&root, "[worktrees]\nrequire = false\n");

        assert_eq!(
            status_in(&root, &scope_of(directory.path())),
            GuardStatus::Active
        );
    }

    #[test]
    fn should_follow_a_dangling_symlink_into_the_main_checkout() {
        let (directory, root) = repository();
        let linked = directory.path().canonicalize().unwrap().join("linked");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat",
                linked.to_str().unwrap(),
            ],
        );
        std::os::unix::fs::symlink("../app/not_yet.rs", linked.join("link.rs")).unwrap();
        let scope = scope_of(directory.path());

        let found = verdict(&scope, &linked.join("link.rs"), &linked);

        assert!(matches!(found, GuardVerdict::MainCheckout(_)));
    }

    #[test]
    fn should_fail_when_symlinks_loop() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(base.join("b"), base.join("a")).unwrap();
        std::os::unix::fs::symlink(base.join("a"), base.join("b")).unwrap();

        let failure = resolve_target(&base.join("a"), &base);

        assert!(matches!(failure, Err(GuardError::TooManySymlinks(_))));
    }

    #[test]
    fn should_require_a_worktree_when_dot_dot_after_a_symlink_leads_into_the_main_checkout() {
        let (directory, root) = repository();
        let linked = directory.path().canonicalize().unwrap().join("linked");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat",
                linked.to_str().unwrap(),
            ],
        );
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::os::unix::fs::symlink(root.join("src"), linked.join("sub")).unwrap();
        let scope = scope_of(directory.path());

        let found = verdict(&scope, Path::new("sub/../evil.rs"), &linked);

        assert!(matches!(found, GuardVerdict::MainCheckout(_)));
    }

    #[test]
    fn should_give_one_target_when_the_textual_and_the_disk_forms_are_the_same() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();

        let targets = resolve_targets(Path::new("a/../b/file.rs"), &base);

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].as_ref().unwrap(), &base.join("b/file.rs"));
    }

    #[test]
    fn should_deny_when_the_textual_form_is_in_the_main_checkout_and_the_on_disk_form_loops() {
        let (directory, root) = repository();
        let linked = root.join(".claude/worktrees/n");
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat",
                linked.to_str().unwrap(),
            ],
        );
        std::os::unix::fs::symlink("loop", linked.join("loop")).unwrap();
        let scope = scope_of(directory.path());

        let found = verdict(&scope, Path::new("loop/../../../../src/x.rs"), &linked);

        assert!(matches!(found, GuardVerdict::MainCheckout(_)));
    }
}
