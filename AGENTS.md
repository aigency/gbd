<!-- BEGIN GBD -->
This repo tracks work with **gbd** (GitHub Issues + Projects). Do not use Beads (`bd`) or TodoWrite for work that should survive this session.

- Session start: `gbd prime`
- Loop: `gbd ready` → `gbd show <n>` → `gbd update <n> --claim` → `gbd close <n>`
- Create: `gbd create "…" -t Task -p 1 --parent 88 --deps 12`
- State: `gbd update <n> --status in_progress|deferred|ready` (board), `gbd priority <n> P1` (org field). Never labels.
- Lore: `gbd remember "insight"` (not MEMORY.md)

Run `gbd prime` after compaction. Full skill: `.agents/skills/gbd/SKILL.md` (same file under `.claude/skills` and `.cursor/skills`).
<!-- END GBD -->
