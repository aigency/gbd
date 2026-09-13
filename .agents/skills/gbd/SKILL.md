---
name: gbd
description: >
  gbd issue tracker for this repo (GitHub Issues + Projects). Use for ready
  work, creating issues, blockers, claiming, closing, and persistent
  memories. Use when the user mentions tasks, blockers, beads, gbd, or
  what's next. Do not use TodoWrite or bd for that work.
---

# gbd

Work lives in GitHub Issues; the board is a GitHub Project. The CLI is
`gbd`. Run `gbd prime` at session start. `--json` on every command.

Do not use `bd`, Dolt, or TodoWrite for surviving work.

## Session

1. `gbd prime` — workflow + memories + ready + assigned to me
2. `gbd show <n>` — fields, parent, children, blockers (one call)
3. `gbd update <n> --claim` — assign @me, board → In Progress. Fails if taken.
4. `gbd comment <n> "…"` as you go
5. `gbd close <n>` — board → Done

## Create

One command. Type, priority, parent, blocked-by together:

`gbd create "Title" -t Task|Bug|Feature|Epic|Chore|Decision -p P1 --parent <n> --deps <n,n>`

## Where things live (never labels)

| Beads | GitHub |
| --- | --- |
| type | issue type (`-t`) |
| priority P0–P4 | org issue field **Priority** (`gbd priority <n> P1`) |
| blocks / blocked-by | issue dependencies (`gbd dep add <n> <blocker>`) |
| parent / epic | sub-issues (`gbd parent <n> --set <epic>`) |
| in_progress / deferred | Project **Status** (`gbd update <n> --status in_progress`) |
| defer until | org field **Start date** (`gbd defer <n> --until tomorrow --reason "…"`) |

`gbd list` is a tree. `gbd dep tree <n>` shows blockers, blockees, and sub-issues.

## Memories

`gbd remember "insight"` / `gbd recall <key>` / `gbd forget <key>` / `gbd memories`.
One closed untyped issue with org field **gbd Role = Memory**. Not AGENTS.md.

## Migrating from Beads

`gbd import --from-beads beads.jsonl --dry-run` shows the plan; `--yes`
creates the issues. One shot, resumable through `beads-map.jsonl`, not
a sync: after it, `bd` is retired for the repo. Export right before the
run (`bd dolt pull`, `bd dolt commit`, then `bd export
--include-memories`) and write nothing to Beads until it finishes: the
JSONL is a snapshot, and later Beads changes never come over.
Afterwards retire Beads (`bd hooks uninstall`, `bd setup <editor>
--remove`, remove `.beads/`; keep `beads-map.jsonl`): README, *Retiring
Beads*.
