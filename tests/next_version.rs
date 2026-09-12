//! Pins the semver-bump semantics of `scripts/next-version.sh` (plan 009).
//!
//! Each test builds a throwaway git repo under a tempdir, seeds it with
//! empty commits carrying crafted conventional-commit subjects/bodies, then
//! shells out to the script exactly as `.github/workflows/release.yml`'s
//! `version` job would and asserts stdout.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// A throwaway git repo used to drive `next-version.sh` against crafted
/// commit history.
struct Repo {
    dir: TempDir,
}

impl Repo {
    fn init() -> Self {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q"]);
        run(dir.path(), &["config", "user.email", "test@example.com"]);
        run(dir.path(), &["config", "user.name", "test"]);
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Create an empty commit with the given subject-only message.
    fn commit(&self, message: &str) {
        run(
            self.path(),
            &["commit", "-q", "--allow-empty", "-m", message],
        );
    }

    /// Create an empty commit with a full multi-line message (subject +
    /// blank + body), needed to exercise `BREAKING CHANGE:`/`BREAKING-CHANGE:`
    /// footers.
    fn commit_full(&self, message: &str) {
        run(
            self.path(),
            &["commit", "-q", "--allow-empty", "-m", message],
        );
    }

    /// Run `scripts/next-version.sh <last-version> <range>` against this repo
    /// and return trimmed stdout.
    fn next_version(&self, last_version: &str, range: &str) -> String {
        let script = script_path();
        let output = Command::new("bash")
            .arg(&script)
            .arg(last_version)
            .arg(range)
            .current_dir(self.path())
            .output()
            .unwrap_or_else(|e| panic!("failed to run {}: {e}", script.display()));
        assert!(
            output.status.success(),
            "next-version.sh exited non-zero: status={:?} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }
}

fn run(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run git {args:?}: {e}"));
    assert!(status.success(), "git {args:?} failed with {status:?}");
}

fn script_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/next-version.sh")
}

#[test]
fn docs_only_yields_no_release() {
    let repo = Repo::init();
    repo.commit("chore: seed");
    repo.commit("docs: update readme");

    assert_eq!(repo.next_version("1.5.3", "HEAD~1..HEAD"), "");
}

#[test]
fn fix_yields_patch_bump() {
    let repo = Repo::init();
    repo.commit("chore: seed");
    repo.commit("fix(core): a bug");

    assert_eq!(repo.next_version("1.5.3", "HEAD~1..HEAD"), "1.5.4");
}

#[test]
fn feat_yields_minor_bump() {
    let repo = Repo::init();
    repo.commit("chore: seed");
    repo.commit("feat(cli): a thing");

    assert_eq!(repo.next_version("1.5.3", "HEAD~1..HEAD"), "1.6.0");
}

#[test]
fn bang_subject_yields_major_bump() {
    let repo = Repo::init();
    repo.commit("chore: seed");
    repo.commit("feat(cli)!: breaking");

    assert_eq!(repo.next_version("1.5.3", "HEAD~1..HEAD"), "2.0.0");
}

#[test]
fn breaking_change_footer_yields_major_bump() {
    let repo = Repo::init();
    repo.commit("chore: seed");
    repo.commit_full("fix(core): a bug\n\nBREAKING CHANGE: removes the old flag");

    assert_eq!(repo.next_version("1.5.3", "HEAD~1..HEAD"), "2.0.0");
}

#[test]
fn hyphenated_breaking_change_footer_yields_major_bump() {
    // Conventional Commits 1.0.0 declares `BREAKING-CHANGE:` synonymous
    // with `BREAKING CHANGE:` — matching only the spaced form under-bumps
    // a major release to minor/patch.
    let repo = Repo::init();
    repo.commit("chore: seed");
    repo.commit_full("fix(core): a bug\n\nBREAKING-CHANGE: removes the old flag");

    assert_eq!(repo.next_version("1.5.3", "HEAD~1..HEAD"), "2.0.0");
}

#[test]
fn mixed_range_takes_highest_bump() {
    let repo = Repo::init();
    repo.commit("chore: seed");
    repo.commit("docs: readme");
    repo.commit("fix(core): a bug");
    repo.commit("feat(cli): a thing");

    assert_eq!(repo.next_version("1.5.3", "HEAD~3..HEAD"), "1.6.0");
}

#[test]
fn release_commit_alone_yields_no_release() {
    // The release commit itself must never re-trigger a bump if it somehow
    // lands in a scanned range.
    let repo = Repo::init();
    repo.commit("chore: seed");
    repo.commit("chore(release): v1.5.3");

    assert_eq!(repo.next_version("1.5.3", "HEAD~1..HEAD"), "");
}
