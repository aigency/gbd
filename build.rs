//! Stamp the git commit into `--version` so "which gbd is this" has an answer.
//! Release tarballs are built from a tag checkout, so they get the tag's sha.

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty());
    let version = env!("CARGO_PKG_VERSION");
    let suffix = if dirty { "-dirty" } else { "" };
    println!("cargo:rustc-env=GBD_VERSION={version} ({sha}{suffix})");
    // Re-stamp when HEAD moves. `--git-path` resolves inside worktrees too.
    for name in ["HEAD", "index"] {
        if let Some(path) = Command::new("git")
            .args(["rev-parse", "--git-path", name])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}
