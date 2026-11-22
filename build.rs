//! Exposes the commit `ds` is built from as `DEVSLOT_BUILD_COMMIT` (empty
//! when it cannot be told), so the session-start hook can compare it with
//! the plugin copy Claude Code runs. A failure here never fails the build.

use std::path::{Path, PathBuf};
use std::process::Command;

const COMMIT_LENGTH: usize = 40;

fn main() {
    let manifest_directory = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let manifest_directory = Path::new(&manifest_directory);
    let commit = git_output(manifest_directory, &["rev-parse", "HEAD"])
        .filter(|commit| is_full_commit(commit))
        .unwrap_or_default();
    println!("cargo:rustc-env=DEVSLOT_BUILD_COMMIT={commit}");
    if !commit.is_empty() {
        watch_head(manifest_directory);
    }
}

fn is_full_commit(text: &str) -> bool {
    text.len() == COMMIT_LENGTH && text.chars().all(|character| character.is_ascii_hexdigit())
}

fn git_output(directory: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

/// Reruns the script when the checked-out commit moves: the HEAD file of the
/// worktree and the branch file it points to (kept in the common git
/// directory). A packed branch is only noticed at the next change of the
/// sources, which is fine: `cargo install` always builds fresh.
fn watch_head(directory: &Path) {
    println!("cargo:rerun-if-changed=build.rs");
    if let Some(git_directory) = git_output(directory, &["rev-parse", "--absolute-git-dir"]) {
        watch_if_exists(&PathBuf::from(git_directory).join("HEAD"));
    }
    let common_directory =
        git_output(directory, &["rev-parse", "--git-common-dir"]).map(|path| directory.join(path));
    let branch = git_output(directory, &["symbolic-ref", "-q", "HEAD"]);
    if let (Some(common_directory), Some(branch)) = (common_directory, branch) {
        watch_if_exists(&common_directory.join(branch));
    }
}

fn watch_if_exists(path: &Path) {
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
