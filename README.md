# gbd

**gbd** is GitHub Beads: the [Beads](https://github.com/gastownhall/beads) workflow for coding agents (`ready`, `create`, `dep`, `claim`, `close`, `remember`) with GitHub Issues as the record and a GitHub Project as the board.

Beads gives an agent a task graph it can query: typed, prioritized issues with blockers and parent/child structure, so "what should I work on next" is a query rather than a guess, and lore survives context compaction. Beads keeps that graph in its own database. gbd keeps it in GitHub, which now has every primitive Beads needed: issue types, issue fields, dependencies, sub-issues, and Projects.

There is no Dolt, SQLite, daemon, or git-synced JSONL. GitHub is the database, `gh` is the transport, and `gbd` is a small Rust CLI on top. Agents get `--json` on every command. Humans get the same issues on github.com and on the board, and never need to install anything.

Nothing is a label. Type, priority, blockers, hierarchy, status, and defer-until all use first-class GitHub features. See [Data model](#data-model).

## Requirements

- A repository owned by a **GitHub organization**. Issue types and issue fields are organization features; personal accounts do not have them.
- The [GitHub CLI](https://cli.github.com/) **2.94.0 or newer** (the release that added `gh issue create --blocked-by` / `--parent` and issue types), logged in, with the `project` scope for the board:

```bash
gh auth login
gh auth refresh -s project
```

- To let `gbd init` set the organization up for you: an org **owner** login. Anyone else can still use gbd once an owner has run it once; see [Organization setup](#organization-setup).

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/aigency/gbd/main/scripts/install.sh | sh
```

```bash
cargo binstall gbd
```

```bash
# From source, via crates.io (Intel Mac, or any arch without a prebuilt):
cargo install --locked gbd
```

Both download the release tarball for your OS and architecture (linux x86_64/arm64, macOS arm64). `install.sh` also verifies the tarball against the release's `SHA256SUMS` and installs `~/.local/bin/gbd` (override with `GBD_INSTALL_DIR`), so put `~/.local/bin` on your `PATH`; `cargo binstall` does no checksum verification of its own.

`gbd --version` prints the crate version and the git commit it was built from, e.g. `gbd 1.0.0 (1a2b3c4)`, so you can tell a release from a local build.

### Verifying a download

Every release asset is signed with [minisign](https://jedisct1.github.io/minisign/) and carries a GitHub build-provenance attestation.

- `cargo binstall` verifies the tarball's `.sig` against the public key in `Cargo.toml` automatically and refuses a mismatch.
- `install.sh` verifies `SHA256SUMS.minisig` when `minisign` is installed, then the tarball's checksum.
- By hand, the public key is `RWTJfFNVFWOcQa3j8m8WBvpgOGO0qocEnMMt8UnIb0wqO0KLgvwb6Fi4`:

```bash
minisign -V -P RWTJfFNVFWOcQa3j8m8WBvpgOGO0qocEnMMt8UnIb0wqO0KLgvwb6Fi4 -x gbd-v1.1.0-aarch64-apple-darwin.tar.gz.sig -m gbd-v1.1.0-aarch64-apple-darwin.tar.gz
```

```bash
gh attestation verify gbd-v1.1.0-aarch64-apple-darwin.tar.gz --repo aigency/gbd
```

The attestation proves the file was built by this repository's release workflow at a specific commit; the signature proves it was published with this project's key. Releases before 1.1.0 have neither.

## Quick start

Once per repository, as an org owner (see [Organization setup](#organization-setup) for what that means):

```bash
cd your-repo
gbd init            # org fields + types, memories issue, the board, .gbd.yml, agent files
git add .gbd.yml AGENTS.md CLAUDE.md .agents .claude .cursor
git commit -m "Initialize gbd"
gbd board sync      # put existing open issues on the board
gbd doctor
gbd ready
```

`gbd init` is idempotent: it lists first and creates only what is missing, and re-running it rewrites the agent files without growing them. Teammates cloning a repo that already has `.gbd.yml` only install the binary; they do not need to be org owners.

## The workflow

What an agent does every session (this is what `AGENTS.md` and the skill say):

```bash
gbd prime                     # workflow + memories + ready + assigned to me
gbd ready                     # unblocked, unclaimed work, best first
gbd show 12                   # fields, parent, children, blockers — one call
gbd update 12 --claim         # assign @me, board → In Progress; fails if taken
gbd comment 12 "…"            # as you go
gbd close 12                  # board → Done
gbd remember "insight"        # lore that survives compaction
```

`gbd ready` ranks by priority, then by how many open issues each one unblocks, then by critical-path height on the open dependency graph. `--explain` prints why. `--claim` takes the top item. `--strict-parent` is the Beads-parity switch that hides children of a blocked parent.

## Commands

Every command takes `--json` (for agents) and `--repo OWNER/REPO`. Issue ids are numbers (`12`, `#12`) or `owner/repo#12` for another repo under the same owner.

### Work

| Command | Does |
| --- | --- |
| `create "Title" -t Bug -p 1 --parent 88 --deps 12,13 --blocking 40` | One `gh issue create` carrying type, parent, and both edge lists; Priority and board Status set right after. `q` is the same and prints only `repo#n`. |
| `show <id>` | The issue with fields, parent, children, blocked-by, and blocking, rendered like `bd show`. |
| `list [--state open\|closed\|all] [--type T] [--assignee A] [--parent N] [--search Q] [--flat]` | A tree with children under parents: `○ #12 ● P1 [bug] Title`. |
| `search "<GitHub search syntax>"` | Same output as `list`. |
| `ready [--claim] [--explain] [--strict-parent] [--include-epics] [--sort priority\|unblocks\|path]` | See above. |
| `update <id> [--claim] [--status ready\|in_progress\|deferred\|done] [--priority P1] [--type T] [--title] [--body] [--add-assignee] [--remove-assignee]` | Beads `update` / `set-state`. `--claim` runs first and, if refused, nothing else is touched. |
| `assign <id> [login]` | Default `@me`. |
| `close <id> [--reason completed\|not_planned\|duplicate]` | Board → Done. |
| `reopen <id>` | Board → Ready. |
| `duplicate <id> <of>` | Close as duplicate with a comment. |
| `delete <id> --yes` | Prefer `close --reason not_planned`. |
| `defer <id…> [--until WHEN] [--reason WHY]` | Beads `defer`. `WHEN`: `YYYY-MM-DD`, `today`, `tomorrow`, `+1h`, `+3d`, `+2w`, `next monday`, or a weekday. Board → Deferred; `--reason` becomes a comment. With a board `--until` is optional. |
| `undefer <id…>` | Clear Start date, board → Ready. |
| `priority <id> P2` | Org **Priority** field. Accepts `P0`–`P4` or `0`–`4`. |
| `comment <id> "…"` / `note` / `comments <id>` | Comments are Beads notes. |
| `label <id> --add x --remove y` | Plain labels only; never state. |

### Structure

| Command | Does |
| --- | --- |
| `dep add <id> <blocker>` / `dep remove` / `link` | `#id` is blocked by `#blocker` (native issue dependencies, cross-repo by URL). |
| `dep list <id>` | Direct blockers and blockees. |
| `dep tree <id>` | Upstream blockers, downstream blockees, and the sub-issue subtree, walked in memory from one snapshot. |
| `children <id>` | Sub-issues, as a tree. |
| `parent <id> [--set N \| --remove]` | Sub-issue parent. |
| `blocked` | Open `is:blocked` issues with their blockers. |

### Board and counts

| Command | Does |
| --- | --- |
| `board` | The board URL and every item grouped by Status, Done included. |
| `board sync` | Put every open issue on the board: In Progress if assigned, else Ready. Items with a Status are left alone. |
| `status` | `open ready blocked in-progress deferred assigned-to-me closed-last-7d`. |
| `count [--state]`, `stale [--days N]`, `statuses`, `types` | Small views. |

### Memories

| Command | Does |
| --- | --- |
| `remember "insight" [--key k]` | Store. The key is derived from the content like Beads (`DeriveKey`); a bare existing key *reads* instead of overwriting. |
| `recall <key>` / `forget <key>` / `memories [search]` | Read, delete, list. |

Memories live in one closed, untyped issue with org field **gbd Role = Memory**. `gbd ready` ignores it. `.gbd.yml` `memory_issue` points at it.

### Setup and diagnostics

| Command | Does |
| --- | --- |
| `init [--no-project] [--no-memory] [--no-skills]` | See [Quick start](#quick-start). |
| `doctor` (`info`) | One row per prerequisite with a copy-paste fix. Exit 1 only when gbd cannot work. |
| `where`, `ping`, `config get\|set`, `onboard`, `open <id>`, `completion <shell>`, `upgrade` | Utilities. |

## Data model

Two layers, never collapsed: the issue is the record; the Project item is the board state.

| Beads | GitHub | Set with |
| --- | --- | --- |
| type (`task`, `bug`, `feature`, `epic`) | org **issue type** Epic / Feature / Bug / Task / Chore | `create -t`, `update --type` |
| priority 0–4 | org issue field **Priority** with options P0–P4 | `create -p`, `priority`, `update --priority` |
| `blocks` / blocked-by | native issue **dependencies** | `create --deps`, `dep add` |
| parent / epic children | **sub-issues** | `create --parent`, `parent --set` |
| `open` | issue open, board **Ready** | `update --status ready`, `reopen` |
| `in_progress` | board **In Progress** (an assignee, when there is no board) | `update --claim`, `ready --claim` |
| `blocked` | derived: GitHub `is:blocked` (an open blocker) | never set directly |
| `deferred` | board **Deferred** and/or org field **Start date** in the future | `defer` |
| `closed` | issue closed, board **Done** | `close` |
| `defer_until` | org issue field **Start date** | `defer --until` |
| notes | comments | `comment` |
| memories (`bd remember`) | one closed issue, gbd Role = Memory | `remember` |
| ids | issue numbers; `owner/repo#n` across repos | |

`ready` is computed in process on a snapshot of the open issues: one paged GraphQL query brings every open issue with its edges, parent, type, org fields, and board Status. Direct blocked-ness is GitHub's own open-blocker count. There is no local cache and no REST N+1.

## The board

`gbd init` creates an org Project named `<repo> board` (or adopts one of that title already linked to the repo) with Status options **Ready / In Progress / Deferred / Done**, links the repo, and writes `project: N` to `.gbd.yml`. From then on `create` adds new issues as Ready, and claim, close, reopen, defer, and `update --status` move the card. `gbd ready` skips In Progress and Deferred. Projects need the `project` scope on the `gh` token (`gh auth refresh -s project`).

Without `project:` in `.gbd.yml`, gbd still works: an assignee means in progress, and `defer --until` means deferred.

## Organization setup

GitHub [issue types](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/managing-issue-types-in-an-organization) and [issue fields](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/managing-issue-fields-in-your-organization) live on the **organization**, not on a repository, and every repo in the org shares them. Set them up once per organization; every repo then benefits.

### What gbd needs on the org

`gbd init` creates all of this when the `gh` login is an **organization owner** (the REST APIs need `admin:org`; the board needs the `project` scope). It only creates what is missing, and it rejects a same-named field of the wrong type instead of silently using it. `gbd doctor` reports each row.

| Prerequisite | Exactly | Used by |
| --- | --- | --- |
| Issue types | **Epic**, **Feature**, **Bug**, **Task**, **Chore** | `create -t`, `ready` (skips Epics) |
| Issue field **Priority** | single-select, options **P0 P1 P2 P3 P4** | `-p`, `priority`, `ready` ranking |
| Issue field **Start date** | date (GitHub's default; recreated if deleted) | `defer --until`, `ready` |
| Issue field **gbd Role** | single-select, option **Memory** | the memories issue, excluded from `ready` |
| Project **`<repo> board`** | Status options **Ready / In Progress / Deferred / Done**, linked to the repo | `claim`, `update --status`, `close`, `board` |

If the org still has GitHub's default Priority (Urgent / High / Medium / Low), init **renames those options in place** (Urgent→P0 … Low→P3) and adds P4, so existing values survive. Init never maps P0 onto "High" at read or write time.

### If you are not an org owner

Init still writes the repo files and says what it could not create. An owner does the rest once, in Organization → **Settings** → **Planning** (replace `ORG`):

- Issue types: `https://github.com/organizations/ORG/settings/issue-types` — enable the five above (GitHub ships Task, Bug, Feature; add Epic purple and Chore gray; disable Enhancement).
- Issue fields: `https://github.com/organizations/ORG/settings/issue-fields` — Priority options P0 red, P1 orange, P2 yellow, P3 green, P4 gray; a **date** field named exactly `Start date`; a single-select `gbd Role` with option `Memory`.
- Projects: create `<repo> board` under the org, set Status to Ready / In Progress / Deferred / Done, link the repo, then `gbd config set project <number>`.

Then re-run `gbd init` in the repo. Note that a GitHub App installation token (the kind some cloud agents run with) usually cannot edit org settings even for an admin; use an org-owner's own login for this step.

### What init cannot do: pin fields to types

Pinned fields show in the create form and sidebar for that type; unpinned fields with no value stay hidden. GitHub's REST field API does not expose pinning, so do it in **Settings → Planning → Issue fields** → the field → **Pin to types** (GitHub allows 10 pinned fields per type):

| Field | Pin to |
| --- | --- |
| Priority | Epic, Feature, Bug, Task, Chore |
| Start date | Feature, Task, Chore |
| Target date | Epic, Feature |
| Effort | Feature, Bug, Task, Chore — not Epic |
| gbd Role | **Issues without a type** only |

## Agent files

`gbd init` primes every agent that might work in the repo, from one source of truth in the binary:

| Agent | Skill | Always-on file |
| --- | --- | --- |
| Codex, GPT Astra | `.agents/skills/gbd/SKILL.md` | `AGENTS.md` |
| Claude Code | `.claude/skills/gbd/SKILL.md` | `CLAUDE.md` |
| Cursor | `.cursor/skills/gbd/SKILL.md` | `AGENTS.md` |

The three `SKILL.md` files are identical. The always-on files get a marked block (`<!-- BEGIN GBD -->` … `<!-- END GBD -->`) that init upserts without touching the rest of the file. No git hooks, no editor session hooks: the files tell the agent to run `gbd prime`. `--no-skills` skips them (CI).

## Configuration

`.gbd.yml` in the repo root, written by init and committed:

```yaml
repo: acme/widgets    # else `gh repo view`, else the git remote
memory_issue: 3       # the memories issue
project: 8            # the board's number under the org; omit for assignee-only mode
```

`--repo OWNER/REPO` overrides `repo` for one command. Output is plain text with a few glyphs (`○` open, `◐` in progress, `⊘` blocked, `⏸` deferred, `✓` closed); the priority dot is colored on a terminal unless `NO_COLOR` is set. `--json` prints stable records instead.

## Development

```bash
bash scripts/check.sh       # fmt, clippy (pedantic, -D warnings), tests, cargo-deny if installed
```

The toolchain is pinned in `rust-toolchain.toml` and CI reads it, so local clippy and CI clippy agree. `[lints.clippy] pedantic` is on in `Cargo.toml`.

Tests never touch the network: `tests/fake_gh/gh` is a fake `gh` on `PATH` that answers from fixtures and logs every invocation, so tests assert what `gbd` asks `gh` for (one GraphQL call, no `--label`, the exact field-value body on stdin), not only what it prints. `tests/fixtures/snapshot.json` is a captured live response, so the parser is tested against GitHub's real shape.

Releases are cut on merge to `main` in two stages. `release.yml` decides the semver bump from conventional-commit PR titles (`feat:` minor, `fix:` patch, `!`/`BREAKING CHANGE` major), commits and tags the version, and dispatches `build-release.yml` on that tag. That second run builds tarballs for linux x86_64/arm64 and macOS arm64, signs them and `SHA256SUMS` with minisign, attests build provenance, and publishes the GitHub Release and the crate. Running the build on the tag is what makes the attestation name the same commit `gbd --version` prints. Pushing the release commit to the protected `main` uses the `RELEASE_TOKEN` secret, a fine-grained token with Contents write on this repository owned by an admin, because the workflow's own token cannot bypass the ruleset.

### Contributing

Open a pull request against `main` with a conventional-commit title; it is squash-merged through a merge queue once `fmt`, `clippy`, and the tests pass on Linux and macOS. `bash scripts/check.sh` runs the same gates locally. Add a fake-`gh` test for any new call to `gh`.

## Non-goals

gbd is a replacement for Beads on repositories that live on GitHub, not a Beads plugin, storage driver, or sync target. It does not mirror issues to or from Beads, Jira, Linear, or anywhere else, and it does not implement Beads' molecules, gates, swarms, or Dolt features. If GitHub has no primitive for something, gbd leaves it out rather than faking it locally.

## License

MIT. See [LICENSE](LICENSE).
