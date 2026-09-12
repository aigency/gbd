#!/usr/bin/env bash
# gbd local quality gate — runs the same checks as .github/workflows/ci.yml,
# in the same order, so you can catch CI failures before pushing.
#
# Gates: cargo fmt --check, cargo clippy --all-targets -D warnings,
# cargo test --all-targets, cargo deny check advisories licenses bans sources.
#
# cargo-deny is skipped locally (with a warning) if it isn't installed —
# CI always runs it, so this is only a local convenience gap.

set -euo pipefail

info() {
    printf '==> %s\n' "$1"
}

info "cargo fmt --check"
cargo fmt --check

info "cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets -- -D warnings

info "cargo test --all-targets"
cargo test --all-targets

if command -v cargo-deny >/dev/null 2>&1; then
    info "cargo deny check advisories licenses bans sources"
    cargo deny check advisories licenses bans sources
else
    printf 'warning: cargo-deny not installed, skipping locally (CI still runs it)\n' >&2
    printf 'warning: install with `cargo install cargo-deny` to run it here too\n' >&2
fi

info "all gates passed"
