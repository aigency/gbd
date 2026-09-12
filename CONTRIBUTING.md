# Contributing to gbd

Thanks for helping. gbd is small on purpose; the best contributions keep it that way.

## Ground rules

- **GitHub is the database.** If GitHub has no primitive for something, gbd leaves it out rather than faking it locally. No caches, no sidecar files, no labels standing in for state.
- **`gh` is the transport.** Every request goes through the GitHub CLI so auth, hosts, and proxies stay the user's problem, not ours.
- **One `gh` call per user action** where GitHub allows it. `gh issue create` carries type, parent, and dependency edges in one call; do not split that into steps.
- **Agents read `--json`, humans read the text.** Both are outputs of the same command; keep them in step.

## Working on it

```bash
gh auth refresh -s project
bash scripts/check.sh      # fmt, clippy (pedantic, -D warnings), tests, cargo-deny if installed
cargo run -- doctor
```

The toolchain is pinned in `rust-toolchain.toml`; `rustup` installs it on first use. Clippy's pedantic group is enforced (`[lints.clippy]` in `Cargo.toml`), so `cargo clippy` locally is the same bar as CI.

### Tests

Tests never touch the network. `tests/fake_gh/gh` is a fake `gh` placed first on `PATH`; it answers from fixtures and logs every invocation to `calls.log`. Tests assert what gbd asks `gh` for, not only what it prints. When you add or change a call to `gh`:

1. Add or extend a test in `tests/fake_gh.rs` that pins the exact subcommand and flags.
2. If the call reads GitHub data, capture a real response once and keep it as a fixture (see `tests/fixtures/snapshot.json`) so the parser is tested against GitHub's real shape.

### Pull requests

- Branch from `main`, open a PR. The title must be a [conventional commit](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `ci:`, `chore:`); it becomes the squash commit and decides the next version (`feat` → minor, `fix` → patch, `!` or a `BREAKING CHANGE:` footer → major). Docs-only merges do not cut a release.
- Required checks: `fmt`, `clippy`, `test (ubuntu-latest)`, `test (macos-latest)`. PRs merge through a merge queue.
- Keep the README truthful: it describes the current binary. If you change a command, change its row.
- `gbd init` writes `AGENTS.md`, `CLAUDE.md`, and three `SKILL.md` files from constants in `src/init.rs`; a test fails if the checked-in copies drift, so edit the constants and regenerate.

## Reporting bugs and proposing features

Use the issue templates. For bugs, include `gbd --version`, `gh --version`, and the command with `--json` if it has one; gbd's `--json` output and the `gh` error text are usually enough to reproduce.
