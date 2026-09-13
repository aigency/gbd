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

### In a cloud or CI session

An agent in a cloud session (Claude Code on the web, Codex, a CI runner) starts from an image with nothing installed and, often, a `gh` with no token. Two things make gbd work there.

**Install it in the environment's setup step.** The installer is POSIX `sh` and needs `curl` (or `gh`), `tar`, `shasum` or `sha256sum` for the checksum, and the usual coreutils (`uname`, `grep`, `cut`, `head`, `mktemp`, `chmod`, `mkdir`); it names whatever is missing and stops before downloading:

```bash
curl -fsSL https://raw.githubusercontent.com/aigency/gbd/main/scripts/install.sh | sh && export PATH="$HOME/.local/bin:$PATH"
```

`GBD_VERSION=v1.6.0` pins a release; `GBD_INSTALL_DIR=/usr/local/bin` puts it somewhere already on `PATH`. If the environment restricts egress, allow `github.com`, `api.github.com`, `raw.githubusercontent.com`, and `release-assets.githubusercontent.com`, which the release downloads redirect to. A `SessionStart` hook that runs the same line when `gbd` is missing does the job from inside a repository instead of the environment, at the cost of a few seconds per fresh session; whether that belongs in a repository is each project's choice, and this one does not carry it.

**Give `gh` a token.** gbd talks to GitHub only through `gh`, and `gh` reads `GH_TOKEN` with no login step, so an environment secret of that name is enough. A classic token needs `repo` and `project`, plus `read:org` for the org's issue types and fields; a fine-grained token needs Issues and Contents on the repositories, Projects on the organization, and read access to the organization. The token a cloud session already holds for git is usually a GitHub App installation token, and it carries only the permissions the app was granted: one granted Contents alone gets 403s from every gbd command. For gbd it needs Issues (read and write) on the repositories and Projects (read and write) on the organization; even then it cannot change organization settings, so `gbd init` on a new organization stays a laptop task. When in doubt, the `GH_TOKEN` secret above is the simpler path.

The `project` scope is required whenever `.gbd.yml` names a board: the snapshot asks for each issue's card and anything that moves a card loads the board, so without the scope even `gbd prime` stops with the scope hint. Assignee-only mode (an assignee means in progress, `defer --until` means deferred, no Blocked or Done column) is what a repository with no `project:` in `.gbd.yml` gets; it follows from the config, not from the token. `gbd doctor` in the session says which you have; `gbd prime` is the first command either way.

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
| `create "Title" -t Bug -p 1 --parent 88 --deps 12,13 --blocking 40` | One `gh issue create` carrying type, parent, and both edge lists; Priority and board Status set right after (`--deps` on open issues → Blocked; `--blocking` moves those cards to Blocked). `q` is the same and prints only `repo#n`. |
| `show <id>` | The issue with fields, parent, children, blocked-by, and blocking, rendered like `bd show`. |
| `list [--state open\|closed\|all] [--type T] [--assignee A] [--parent N] [--search Q] [--flat]` | A tree with children under parents: `○ #12 ● P1 [bug] Title`. |
| `search "<GitHub search syntax>"` | Same output as `list`. |
| `ready [--claim] [--explain] [--strict-parent] [--include-epics] [--sort priority\|unblocks\|path]` | See above. |
| `update <id> [--claim] [--status ready\|in_progress\|deferred\|done] [--priority P1] [--type T] [--title] [--body] [--add-assignee] [--remove-assignee]` | Beads `update` / `set-state`. `--claim` runs first and, if refused, nothing else is touched. |
| `assign <id> [login]` | Default `@me`. |
| `close <id> [--reason completed\|not_planned\|duplicate]` | Board → Done. Cards this issue was blocking go Blocked → Ready once their last open blocker is gone. |
| `reopen <id>` | Board → Ready (Blocked if it still has open blockers); what it blocks goes back to Blocked. |
| `duplicate <id> <of>` | Close as duplicate with a comment. |
| `delete <id> --yes` | Prefer `close --reason not_planned`. |
| `defer <id…> [--until WHEN] [--reason WHY]` | Beads `defer`. `WHEN`: `YYYY-MM-DD`, `today`, `tomorrow`, `+1h`, `+3d`, `+2w`, `next monday`, or a weekday. Board → Deferred; `--reason` becomes a comment. With a board `--until` is optional. |
| `undefer <id…>` | Clear Start date, board → Ready (Blocked if it still has open blockers). |
| `priority <id> P2` | Org **Priority** field. Accepts `P0`–`P4` or `0`–`4`. |
| `comment <id> "…"` / `note` / `comments <id>` | Comments are Beads notes. |
| `label <id> --add x --remove y` | Plain labels only; never state. |

### Structure

| Command | Does |
| --- | --- |
| `dep add <id> <blocker>` / `dep remove` / `link` | `#id` is blocked by `#blocker` (native issue dependencies, cross-repo by URL). The card moves Ready → Blocked, and back once no open blocker remains. |
| `dep list <id>` | Direct blockers and blockees. |
| `dep tree <id>` | Upstream blockers, downstream blockees, and the sub-issue subtree, walked in memory from one snapshot. |
| `children <id>` | Sub-issues, as a tree. |
| `parent <id> [--set N \| --remove]` | Sub-issue parent. |
| `blocked` | Open `is:blocked` issues with their blockers. |

### Board and counts

| Command | Does |
| --- | --- |
| `board` | The board URL and every item grouped by Status, Done included. |
| `board sync` | Put every open issue on the board (In Progress if assigned, else Blocked or Ready) and make Ready ⇄ Blocked agree with GitHub's open-blocker counts. In Progress, Deferred, and Done cards are left alone. |
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
| `import --from-beads FILE [--dry-run \| --yes] [--mapping FILE]` | Move a Beads tracker onto GitHub, once. See [Migrating from Beads](#migrating-from-beads). |
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
| `blocked` | derived: GitHub `is:blocked` (an open blocker); mirrored to board **Blocked** by gbd | never set by hand; `dep add`, `close`, `board sync` keep it |
| `deferred` | board **Deferred** and/or org field **Start date** in the future | `defer` |
| `closed` | issue closed, board **Done** | `close` |
| `defer_until` | org issue field **Start date** | `defer --until` |
| notes | comments | `comment` |
| memories (`bd remember`) | one closed issue, gbd Role = Memory | `remember` |
| ids | issue numbers; `owner/repo#n` across repos | |

`ready` is computed in process on a snapshot of the open issues: one paged GraphQL query brings every open issue with its edges, parent, type, org fields, and board Status. Direct blocked-ness is GitHub's own open-blocker count. There is no local cache and no REST N+1.

## The board

`gbd init` creates an org Project named `<repo> board` (or adopts one of that title already linked to the repo) with Status options **Blocked / Deferred / Ready / In Progress / Done** in that column order, names the default view **Board** in board layout with the filter `-type:Epic` (epics are containers, not work), links the repo, and writes `project: N` to `.gbd.yml`. On a board from an earlier gbd, re-running `init` adds any missing option and puts the five in that order with their ids kept, so no card loses its value; the view is rewritten only while it is still GitHub's untouched "View 1" table, and `gbd doctor` says when a hand-shaped view differs. From then on `create` adds new issues as Ready (Blocked when `--deps` names an open issue), and claim, close, reopen, defer, and `update --status` move the card. `gbd ready` skips In Progress and Deferred. Projects need the `project` scope on the `gh` token (`gh auth refresh -s project`).

**Blocked** exists because project views cannot filter on dependency state (`-is:blocked` is not understood), so without it blocked cards sit in the Ready column. gbd keeps the column from GitHub's own open-blocker count: `dep add` moves a Ready card to Blocked, and `dep remove` or closing the last open blocker through gbd moves it back. `update --status ready`, `reopen`, and `undefer` land on Blocked instead when a blocker is still open. Only Ready and Blocked ever swap; In Progress, Deferred, and Done are someone's decision. The column is display only: `gbd ready` reads `is:blocked` directly and never looks at it. One limit: a blocker closed in the GitHub UI leaves the blocked card in Blocked until the next `gbd board sync` (or the next gbd command that touches that issue).

Without `project:` in `.gbd.yml`, gbd still works: an assignee means in progress, and `defer --until` means deferred.

## Migrating from Beads

`gbd import` moves an existing Beads tracker onto GitHub Issues and the board, once. It is not a sync: run it, keep the mapping file, retire `bd` for that repo.

The JSONL is a snapshot of the Beads database at export time, and the Dolt database is the system of record, so export right before the run and not earlier: pull the Dolt remote, commit anything pending, then stop writing to Beads until the import has finished. Anything written to Beads after the export never comes over.

```bash
bd dolt pull                                      # in the Beads repo: everything on the remote
bd dolt commit                                    # anything pending locally (bd dolt status shows it)
bd export --include-memories -o ~/beads.jsonl     # fresh, outside any repo; --all is not needed
gbd import --from-beads ~/beads.jsonl --dry-run   # the plan; nothing written, gh not called
gbd import --from-beads ~/beads.jsonl --yes       # in the target repo, board configured
```

Compare the dry run's counts with any earlier one before `--yes`; a surprising difference means the export is not what you think it is.

The dry run prints counts by type, board column, state, and priority; the creation order (parents and blockers before what depends on them); dependency cycles; everything that cannot map and why; the parser's problems; the memory keys. The real run prints the same diagnostics first, then one line per bead, then a summary.

| Beads | GitHub |
| --- | --- |
| type `epic` / `feature` / `bug` / `task` / `chore` | issue type; `decision` and anything unknown → Task, reported |
| priority 0–4 | Priority P0–P4 |
| `blocks` | dependency (`--blocked-by`), created after its blocker |
| `parent-child` | sub-issue (`--parent`), created after its parent |
| open | board Ready, or Blocked when a blocker is still open |
| `in_progress` | board In Progress, assignee kept |
| `deferred`, or any `defer_until` | board Deferred, Start date |
| closed | closed with a reason read from the free-text `close_reason` (duplicate / not planned / else completed), board Done |
| `notes`, comments | comments, with author and date |
| description, design, acceptance criteria | the body, as sections |
| labels, owner, dates, estimate, external ref, the original close reason | an import footer at the end of the body (dates to the day), so nothing becomes a label |
| `_type: memory` lines | the memories issue, upserted by key |
| `related`, `discovered-from`, `supersedes`, `duplicates`, `tracks`, edges to beads outside the export, agents, gates, templates | dropped, each one listed with the reason |

**The mapping file.** `beads-map.jsonl` (`--mapping` to choose another) records every created issue as it happens, flushed per line: `{"bead":"wx-1","number":101,"url":"…","phase":"created","comments":0}`, one line per step, the last line per bead winning. `phase` is `created` (the issue exists; `comments` says how many of its comments are on, `rewritten` whether its body was already fixed up for references to later beads) or `done`. Keep it: it resolves `bd-xxxx` references in old docs and commit messages, and mentions of a mapped id inside imported bodies and comments are rewritten to `#n`. A body that mentions a bead created later in the run is written with the Beads id first and edited once at the end, when every number is known; comments are posted only after every issue exists. Ids inside code spans, fenced and indented code blocks, URLs, link destinations, and reference definitions are left as they are. Re-running with the same file resumes: done beads are skipped, partially imported ones are finished, and nothing recorded is created twice. Imported comments carry a hidden `<!-- gbd-import bead/k -->` marker, and a resumed bead is reconciled against the comments GitHub already has before any are posted, so a Ctrl-C between a comment and its checkpoint is harmless. A line cut off mid-write is ignored and repaired. The one check left after a Ctrl-C: an issue created in the instant before its line was written is unknown to the file, so look at the newest issue in the repo before resuming, and if it is missing, append its `created` line with `"comments":0` (the format is what the import's own errors print). The file is tied to the repository it was written for and refused elsewhere, and one import holds it at a time: a second `gbd import` on the same file fails at once rather than creating everything twice.

**Rate limits and time.** Each bead is several `gh` calls (create, Priority, comments, close, card). GitHub's secondary limit on content-creating requests is a few hundred per hour, so a corpus of thousands takes hours. gbd backs off and retries when GitHub says to wait (a minute first, doubling, as GitHub documents for its secondary limit; the retry line on stderr says how long), with one exception: the create call itself is never repeated, because `gh issue create` is several mutations in one and a repeat could duplicate the issue. A limit hit right there stops the run with the report and the mapping path; run the same command again and it resumes, adopting an issue that was created but not yet recorded (it reads the newest issues directly, so it does not wait on GitHub's search index). gbd looks Priority and Start date up once per run, and the mapping file lets you interrupt and continue whenever you like.

**Retiring Beads.** The last step, per repo, once the import has finished and a few issues have been checked on GitHub. Keep two things: the export file and `beads-map.jsonl` (commit it), the only durable record of what each old id became. Then remove Beads so nothing keeps writing to a tracker no one reads:

```bash
bd dolt stop                        # the embedded Dolt server, if it is running
bd hooks uninstall                  # the git hooks
bd setup codex --remove             # each editor integration you installed: codex, claude, cursor, …
git rm -r --ignore-unmatch .beads && rm -rf .beads   # the tracked files (if any), then the ignored database and backups
```

Check `AGENTS.md` and `CLAUDE.md` for a leftover Beads block (`gbd init` writes its own), `.codex/hooks.json` and `.claude/settings.json` for `bd` hooks, and `.agents/skills` for the Beads skill. If Beads synced to a DoltHub database, archive or delete it there: it is a frozen copy now, and a second tracker only diverges. Older Beads versions synced through a `refs/dolt/data` git ref; delete it locally and on origin if `git for-each-ref refs/dolt` shows one.

Once no repository on the machine uses Beads: `brew uninstall beads dolt`, remove `~/.beads` (the global registry), and delete any Beads skills under `~/.claude/skills`.

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
| Project **`<repo> board`** | Status options **Blocked / Deferred / Ready / In Progress / Done**, a **Board** view filtered `-type:Epic`, linked to the repo | `claim`, `update --status`, `close`, `dep add`, `board` |

If the org still has GitHub's default Priority (Urgent / High / Medium / Low), init **renames those options in place** (Urgent→P0 … Low→P3) and adds P4, so existing values survive. Init never maps P0 onto "High" at read or write time.

### If you are not an org owner

Init still writes the repo files and says what it could not create. An owner does the rest once, in Organization → **Settings** → **Planning** (replace `ORG`):

- Issue types: `https://github.com/organizations/ORG/settings/issue-types` — enable the five above (GitHub ships Task, Bug, Feature; add Epic purple and Chore gray; disable Enhancement).
- Issue fields: `https://github.com/organizations/ORG/settings/issue-fields` — Priority options P0 red, P1 orange, P2 yellow, P3 green, P4 gray; a **date** field named exactly `Start date`; a single-select `gbd Role` with option `Memory`.
- Projects: create `<repo> board` under the org, set Status to Blocked / Deferred / Ready / In Progress / Done, link the repo, then `gbd config set project <number>`.

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

gbd is open source, not open contribution: fork it freely, report bugs, open an issue to talk about a change, but pull requests from outside the project are closed automatically. [CONTRIBUTING.md](CONTRIBUTING.md) says why, and how gbd is built and released.

## Non-goals

gbd is a replacement for Beads on repositories that live on GitHub, not a Beads plugin, storage driver, or sync target. It does not mirror issues to or from Beads, Jira, Linear, or anywhere else, and it does not implement Beads' molecules, gates, swarms, or Dolt features. If GitHub has no primitive for something, gbd leaves it out rather than faking it locally.

## License

MIT. See [LICENSE](LICENSE).
