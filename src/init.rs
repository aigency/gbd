//! `gbd init` and `gbd doctor`.

use anyhow::{bail, Result};
use std::fs;
use std::path::Path;

use crate::config::Config;
use crate::fields;
use crate::gh;
use crate::memory::{self, MEMORY_TITLE};
use crate::project::{self, Board};
use crate::repo::Repo;
use std::fmt::Write as _;

pub const BEGIN: &str = "<!-- BEGIN GBD -->";
pub const END: &str = "<!-- END GBD -->";

pub const AGENTS_BLOCK: &str = r#"<!-- BEGIN GBD -->
This repo tracks work with **gbd** (GitHub Issues + Projects). Do not use Beads (`bd`) or TodoWrite for work that should survive this session.

- Session start: `gbd prime`
- Loop: `gbd ready` → `gbd show <n>` → `gbd update <n> --claim` → `gbd close <n>`
- Create: `gbd create "…" -t Task -p 1 --parent 88 --deps 12`
- State: `gbd update <n> --status in_progress|deferred|ready` (board), `gbd priority <n> P1` (org field). Never labels.
- Lore: `gbd remember "insight"` (not MEMORY.md)

Run `gbd prime` after compaction. Full skill: `.agents/skills/gbd/SKILL.md` (same file under `.claude/skills` and `.cursor/skills`).
<!-- END GBD -->
"#;

pub const SKILL_MD: &str = r#"---
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

`gbd create "Title" -t Task|Bug|Feature|Epic|Chore -p P1 --parent <n> --deps <n,n>`

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
"#;

pub const ISSUE_TYPES: [&str; 5] = ["Epic", "Feature", "Bug", "Task", "Chore"];

/// One SKILL.md, written to every agent's project-skill location:
/// Codex / GPT Astra (`.agents`), Claude Code (`.claude`), Cursor (`.cursor`).
pub const SKILL_DIRS: [&str; 3] = [
    ".agents/skills/gbd",
    ".claude/skills/gbd",
    ".cursor/skills/gbd",
];

/// Always-on instruction files that get the marked gbd block.
/// Codex, Astra, and Cursor read AGENTS.md; Claude Code reads CLAUDE.md.
pub const INSTRUCTION_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

pub struct InitOpts {
    pub no_project: bool,
    pub no_skills: bool,
    pub no_memory: bool,
}

pub struct DoctorReport {
    pub ok: bool,
    pub lines: Vec<Check>,
}

pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub warn: bool,
}

/// `gh` must be on PATH and at least [`gh::MIN_GH_VERSION`]; older releases
/// lack the issue-type, sub-issue, and dependency flags every write uses.
pub fn require_gh() -> Result<()> {
    let Some(v) = gh::version() else {
        bail!("`gh` is not on PATH. Install GitHub CLI: brew install gh\nhttps://cli.github.com/");
    };
    if v < gh::MIN_GH_VERSION {
        bail!(
            "gh {} is too old; gbd needs {} or newer (issue types, sub-issues, dependencies). Upgrade: brew upgrade gh — https://cli.github.com/",
            gh::version_string(v),
            gh::version_string(gh::MIN_GH_VERSION)
        );
    }
    Ok(())
}

pub fn require_auth() -> Result<()> {
    require_gh()?;
    if !gh::auth_logged_in() {
        bail!("`gh` is not logged in. Run: gh auth login");
    }
    Ok(())
}

pub fn upsert_agents(path: &Path) -> Result<()> {
    if path.exists() && path.symlink_metadata()?.file_type().is_symlink() {
        eprintln!("warning: {} is a symlink; not rewriting it", path.display());
        return Ok(());
    }
    if path.exists() {
        let existing = fs::read_to_string(path)?;
        if existing.contains(BEGIN) && existing.contains(END) {
            let rewritten = replace_block(&existing, AGENTS_BLOCK);
            fs::write(path, rewritten)?;
            return Ok(());
        }
        let mut out = existing;
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(AGENTS_BLOCK);
        fs::write(path, out)?;
    } else {
        fs::write(path, AGENTS_BLOCK)?;
    }
    Ok(())
}

pub fn replace_block(existing: &str, block: &str) -> String {
    let Some(start) = existing.find(BEGIN) else {
        return format!("{existing}\n{block}");
    };
    let Some(end_rel) = existing[start..].find(END) else {
        return format!("{existing}\n{block}");
    };
    let end = start + end_rel + END.len();
    let rest = &existing[end..];
    let mut out = String::new();
    out.push_str(&existing[..start]);
    out.push_str(block.trim_end());
    // `rest` already starts with the newline that followed the old END marker;
    // only add one when the marker was the very end of the file.
    if !rest.starts_with('\n') {
        out.push('\n');
    }
    out.push_str(rest);
    out
}

pub fn write_skill(root: &Path) -> Result<()> {
    for dir in SKILL_DIRS {
        let dir = root.join(dir);
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("SKILL.md"), SKILL_MD)?;
    }
    Ok(())
}

/// Upsert the marked block into every instruction file.
pub fn write_instructions(root: &Path) -> Result<()> {
    for file in INSTRUCTION_FILES {
        upsert_agents(&root.join(file))?;
    }
    Ok(())
}

pub fn warn_beads(root: &Path) {
    if root.join(".beads").exists() {
        eprintln!("warning: `.beads/` exists. This repo used Beads. gbd is the tracker now.");
        eprintln!("         Do not `bd dolt push`. Uninstall the Beads plugin if `bd` is on PATH.");
        eprintln!("         gbd will not delete `.beads/` — that's a human call.");
    }
}

pub fn ensure_memory_issue(repo: &Repo, cfg: &mut Config) -> Result<u64> {
    if let Ok(n) = memory::locate(repo, cfg.memory_issue) {
        if let Err(err) = fields::set_role_memory(repo, n) {
            eprintln!(
                "warning: memories issue #{n}: could not set {}={} ({err:#})",
                fields::ROLE_FIELD,
                fields::ROLE_MEMORY
            );
        }
        cfg.memory_issue = Some(n);
        return Ok(n);
    }
    let url = gh::run(&[
        "issue",
        "create",
        "-R",
        &repo.name_with_owner,
        "--title",
        MEMORY_TITLE,
        "--body",
        &format!("{}\n\n", memory::HEADER),
    ])?;
    let n = crate::ids::number_from_url(&url)?;
    if let Err(err) = fields::set_role_memory(repo, n) {
        eprintln!(
            "warning: created memories issue #{n} but could not set {}={} ({err:#}). Set it in the issue sidebar.",
            fields::ROLE_FIELD,
            fields::ROLE_MEMORY
        );
    }
    let _ = gh::run(&[
        "issue",
        "close",
        &n.to_string(),
        "-R",
        &repo.name_with_owner,
        "--reason",
        "not planned",
    ]);
    cfg.memory_issue = Some(n);
    Ok(n)
}

/// Names of the org's enabled issue types.
pub fn list_issue_types(org: &str) -> Result<Vec<String>> {
    let v: serde_json::Value = gh::api_json("GET", &format!("orgs/{org}/issue-types"), None)?;
    Ok(v.as_array()
        .map(|arr| {
            arr.iter()
                .filter(|t| t.get("is_enabled").and_then(serde_json::Value::as_bool) != Some(false))
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

/// gbd types the org does not have (enabled).
pub fn missing_issue_types(existing: &[String]) -> Vec<&'static str> {
    ISSUE_TYPES
        .iter()
        .copied()
        .filter(|want| !existing.iter().any(|e| e.eq_ignore_ascii_case(want)))
        .collect()
}

/// Ensure the five gbd issue types exist on the org. One GET, then a POST
/// per missing type. Returns a single log line.
pub fn ensure_issue_types(repo: &Repo) -> String {
    let org = repo.owner();
    let settings = format!("https://github.com/organizations/{org}/settings/issue-types");
    let existing = match list_issue_types(org) {
        Ok(v) => v,
        Err(err) => {
            return format!(
                "issue types: could not list ({}). Personal accounts have no issue types; for an org, see {settings}",
                first_line(&err)
            )
        }
    };
    let colors = ["purple", "blue", "red", "yellow", "gray"];
    let mut created = Vec::new();
    let mut failed = Vec::new();
    let missing = missing_issue_types(&existing);
    for (name, color) in ISSUE_TYPES.iter().zip(colors) {
        if !missing.contains(name) {
            continue;
        }
        let body = serde_json::json!({
            "name": name,
            "description": format!("gbd {name}"),
            "is_enabled": true,
            "color": color,
        });
        match gh::api("POST", &format!("orgs/{org}/issue-types"), Some(&body)) {
            Ok(_) => created.push(*name),
            Err(err) => failed.push(format!("{name} ({})", first_line(&err))),
        }
    }
    let mut line = format!("issue types: {}", ISSUE_TYPES.join(", "));
    if !created.is_empty() {
        let _ = write!(line, " — created {}", created.join(", "));
    }
    if !failed.is_empty() {
        let _ = write!(
            line,
            " — could not create {}. Org admin: {settings}",
            failed.join(", ")
        );
    }
    line
}

/// gh errors echo the whole command; keep the part a human acts on.
fn first_line(err: &anyhow::Error) -> String {
    let msg = format!("{err:#}");
    let tail = msg.rsplit("failed: ").next().unwrap_or(&msg);
    tail.lines().next().unwrap_or(tail).trim().to_string()
}

/// Use the configured board; else adopt a `<repo> board` already linked to
/// the repo; else create one. Either way the Status field ends up with
/// Ready / In Progress / Deferred / Done. Re-running never makes a second
/// board.
fn ensure_board(repo: &Repo, cfg: &mut Config) -> Result<Board> {
    let title = format!("{} board", repo.name());
    let number = match cfg.project_number() {
        Some(n) => n,
        None => match project::find_linked(repo, &title)?.as_slice() {
            [] => {
                let board = project::create(repo, &title)?;
                cfg.project = Some(board.number);
                return Ok(board);
            }
            [n] => *n,
            [n, rest @ ..] => {
                eprintln!(
                    "warning: several projects titled {title:?} are linked to this repo (#{n}, {}); adopting #{n}. Override with: gbd config set project <number>",
                    rest.iter().map(|m| format!("#{m}")).collect::<Vec<_>>().join(", ")
                );
                *n
            }
        },
    };
    let mut board = Board::load(repo.owner(), number)?;
    board.ensure_statuses(false)?;
    cfg.project = Some(number);
    Ok(board)
}

pub fn init(root: &Path, repo: &Repo, opts: &InitOpts) -> Result<Vec<String>> {
    require_auth()?;
    warn_beads(root);
    let mut log = Vec::new();

    let cfg_path = root.join(".gbd.yml");
    let mut cfg = if cfg_path.exists() {
        Config::load_from(&cfg_path)?
    } else {
        Config::default()
    };
    cfg.repo = Some(repo.name_with_owner.clone());

    match fields::ensure_role_field(repo.owner()) {
        Ok(_) => log.push(format!(
            "issue field {}: {}",
            fields::ROLE_FIELD,
            fields::ROLE_MEMORY
        )),
        Err(err) => log.push(format!(
            "issue field {}: not ensured ({err:#}). Org admin: Settings → Planning → Issue fields — https://github.com/organizations/{}/settings/issue-fields — option Memory, pin to Issues without a type only",
            fields::ROLE_FIELD,
            repo.owner()
        )),
    }
    match fields::ensure_priority_field(repo.owner()) {
        Ok(f) => {
            let opts: Vec<&str> = f.options.iter().map(|o| o.name.as_str()).collect();
            log.push(format!(
                "issue field {}: {}",
                fields::PRIORITY_FIELD,
                opts.join(", ")
            ));
        }
        Err(err) => log.push(format!(
            "issue field Priority: not ensured ({err:#}). Org admin: Settings → Planning → Issue fields — https://github.com/organizations/{}/settings/issue-fields — options must be P0–P4, not Urgent/High/Medium/Low",
            repo.owner()
        )),
    }
    match fields::ensure_start_date_field(repo.owner()) {
        Ok(_) => log.push(format!("issue field {}: date", fields::START_DATE_FIELD)),
        Err(err) => log.push(format!(
            "issue field {}: not ensured ({err:#}). Org admin: Settings → Planning → Issue fields — https://github.com/organizations/{}/settings/issue-fields — a date field named exactly \"{}\"",
            fields::START_DATE_FIELD,
            repo.owner(),
            fields::START_DATE_FIELD
        )),
    }

    if !opts.no_memory {
        let n = ensure_memory_issue(repo, &mut cfg)?;
        log.push(format!("memories issue #{n}"));
    }

    log.push(ensure_issue_types(repo));

    if !opts.no_project {
        match ensure_board(repo, &mut cfg) {
            Ok(board) => log.push(format!("board: #{} {} {}", board.number, board.title, board.url)),
            Err(err) => log.push(format!(
                "board: skipped ({err:#}). Needs the `project` scope ({}) — re-run gbd init, or set `project: N` in .gbd.yml.",
                project::SCOPE_HINT
            )),
        }
    }

    cfg.save_to(&cfg_path)?;
    log.push("wrote .gbd.yml".into());

    if !opts.no_skills {
        write_instructions(root)?;
        write_skill(root)?;
        log.push(format!(
            "wrote {} and {}/SKILL.md",
            INSTRUCTION_FILES.join(", "),
            SKILL_DIRS.join("/SKILL.md, ")
        ));
    }

    log.push(if cfg.project_number().is_some() {
        "next: gbd doctor, then gbd board sync (puts existing open issues on the board), then gbd ready".into()
    } else {
        "next: gbd doctor && gbd ready".into()
    });
    Ok(log)
}

pub fn doctor(
    root: &Path,
    repo: Option<&Repo>,
    cfg: &Config,
    cfg_error: Option<&str>,
    skills_required: bool,
) -> DoctorReport {
    let mut lines = Vec::new();
    let mut ok = true;

    let push = |lines: &mut Vec<Check>, name: &str, pass: bool, detail: String, warn: bool| {
        lines.push(Check {
            name: name.into(),
            ok: pass,
            detail,
            warn,
        });
    };

    push(
        &mut lines,
        "gbd",
        true,
        format!("gbd {}", env!("GBD_VERSION")),
        false,
    );

    match gh::version() {
        Some(v) if v >= gh::MIN_GH_VERSION => push(
            &mut lines,
            "gh",
            true,
            format!(
                "gh {} (needs {} or newer)",
                gh::version_string(v),
                gh::version_string(gh::MIN_GH_VERSION)
            ),
            false,
        ),
        Some(v) => {
            ok = false;
            push(
                &mut lines,
                "gh",
                false,
                format!(
                    "gh {} is too old; gbd needs {} or newer. brew upgrade gh — https://cli.github.com/",
                    gh::version_string(v),
                    gh::version_string(gh::MIN_GH_VERSION)
                ),
                false,
            );
        }
        None => {
            ok = false;
            push(
                &mut lines,
                "gh",
                false,
                "`gh` missing. brew install gh — https://cli.github.com/".into(),
                false,
            );
        }
    }

    if gh::auth_logged_in() {
        push(
            &mut lines,
            "auth",
            true,
            "gh auth status: logged in".into(),
            false,
        );
    } else {
        ok = false;
        push(
            &mut lines,
            "auth",
            false,
            "not logged in. Run: gh auth login".into(),
            false,
        );
    }

    match crate::config::find_from(root) {
        Some(_) if cfg_error.is_some() => {
            ok = false;
            push(
                &mut lines,
                ".gbd.yml",
                false,
                format!(
                    "{}. Fix the file before running gbd",
                    cfg_error.unwrap_or("")
                ),
                false,
            );
        }
        Some(p) => push(&mut lines, ".gbd.yml", true, p.display().to_string(), false),
        None => push(
            &mut lines,
            ".gbd.yml",
            false,
            "missing. Run: gbd init".into(),
            true,
        ),
    }

    if let Some(repo) = repo {
        push(
            &mut lines,
            "repo",
            true,
            repo.name_with_owner.clone(),
            false,
        );
        match fields::find_priority_field(repo.owner()) {
            Ok(Some(f)) if fields::has_p0_p4(&f) => push(
                &mut lines,
                "priority",
                true,
                "issue field Priority: P0–P4".into(),
                false,
            ),
            Ok(Some(f)) => push(
                &mut lines,
                "priority",
                true,
                format!(
                    "issue field Priority options are {}, not P0–P4. Run: gbd init (org admin)",
                    f.options
                        .iter()
                        .map(|o| o.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                true,
            ),
            Ok(None) => push(
                &mut lines,
                "priority",
                true,
                "no Priority issue field. Run: gbd init (org admin)".into(),
                true,
            ),
            Err(err) => push(
                &mut lines,
                "priority",
                true,
                format!("could not list issue fields ({err:#})"),
                true,
            ),
        }
        match fields::find_typed_field(repo.owner(), fields::ROLE_FIELD, "single_select") {
            Ok(Some(f)) if fields::role_has_memory(&f) => push(
                &mut lines,
                "role",
                true,
                format!(
                    "issue field {}: {}",
                    fields::ROLE_FIELD,
                    fields::ROLE_MEMORY
                ),
                false,
            ),
            Ok(Some(_)) => push(
                &mut lines,
                "role",
                true,
                format!(
                    "issue field {} exists but has no {} option. Run: gbd init (org admin)",
                    fields::ROLE_FIELD,
                    fields::ROLE_MEMORY
                ),
                true,
            ),
            Ok(None) => push(
                &mut lines,
                "role",
                true,
                format!(
                    "no {} issue field. Run: gbd init (org admin)",
                    fields::ROLE_FIELD
                ),
                true,
            ),
            Err(err) => push(
                &mut lines,
                "role",
                true,
                format!("could not list issue fields ({err:#})"),
                true,
            ),
        }
        match list_issue_types(repo.owner()) {
            Ok(existing) => {
                let missing = missing_issue_types(&existing);
                if missing.is_empty() {
                    push(
                        &mut lines,
                        "types",
                        true,
                        format!("issue types: {}", ISSUE_TYPES.join(", ")),
                        false,
                    );
                } else {
                    push(
                        &mut lines,
                        "types",
                        true,
                        format!(
                            "missing issue type{} {}. Run: gbd init (org admin)",
                            if missing.len() == 1 { "" } else { "s" },
                            missing.join(", ")
                        ),
                        true,
                    );
                }
            }
            Err(err) => push(
                &mut lines,
                "types",
                true,
                format!("could not list issue types ({err:#})"),
                true,
            ),
        }
        match fields::find_typed_field(repo.owner(), fields::START_DATE_FIELD, "date") {
            Ok(Some(_)) => push(
                &mut lines,
                "start-date",
                true,
                format!("issue field {}: date", fields::START_DATE_FIELD),
                false,
            ),
            Ok(None) => push(
                &mut lines,
                "start-date",
                true,
                format!(
                    "no {} issue field; gbd defer needs it. Run: gbd init (org admin)",
                    fields::START_DATE_FIELD
                ),
                true,
            ),
            Err(err) => push(
                &mut lines,
                "start-date",
                true,
                format!("could not list issue fields ({err:#})"),
                true,
            ),
        }
        match cfg.project_number() {
            None => push(
                &mut lines,
                "board",
                true,
                "no project in .gbd.yml; In Progress / Deferred use assignees. Run: gbd init"
                    .into(),
                true,
            ),
            Some(n) => match Board::load(repo.owner(), n) {
                Ok(b) => {
                    let missing: Vec<&str> = project::STATUSES
                        .iter()
                        .map(|(name, _)| *name)
                        .filter(|name| !b.has_status(name))
                        .collect();
                    if missing.is_empty() {
                        push(
                            &mut lines,
                            "board",
                            true,
                            format!("#{n} {} {}", b.title, b.url),
                            false,
                        );
                    } else {
                        push(
                            &mut lines,
                            "board",
                            true,
                            format!(
                                "#{n} missing Status options {}. Run: gbd init",
                                missing.join(", ")
                            ),
                            true,
                        );
                    }
                }
                Err(err) => {
                    ok = false;
                    push(
                        &mut lines,
                        "board",
                        false,
                        format!("project #{n}: {err:#}"),
                        false,
                    );
                }
            },
        }
    } else {
        ok = false;
        push(
            &mut lines,
            "repo",
            false,
            "could not infer owner/repo. Fix git remote or pass --repo".into(),
            false,
        );
    }

    let has_block =
        |file: &str| fs::read_to_string(root.join(file)).is_ok_and(|t| t.contains(BEGIN));
    let missing: Vec<String> = SKILL_DIRS
        .iter()
        .map(|d| format!("{d}/SKILL.md"))
        .filter(|p| !root.join(p).is_file())
        .chain(
            INSTRUCTION_FILES
                .iter()
                .filter(|f| !has_block(f))
                .map(|f| format!("{f} (gbd block)")),
        )
        .collect();
    if missing.is_empty() {
        push(
            &mut lines,
            "skills",
            true,
            format!(
                "{} + {}/SKILL.md",
                INSTRUCTION_FILES.join(", "),
                SKILL_DIRS.join("/SKILL.md, ")
            ),
            false,
        );
    } else if skills_required {
        ok = false;
        push(
            &mut lines,
            "skills",
            false,
            format!("missing {}. Run: gbd init", missing.join(", ")),
            false,
        );
    } else {
        push(
            &mut lines,
            "skills",
            true,
            format!("skipped (--no-skills / CI); missing {}", missing.join(", ")),
            true,
        );
    }

    if root.join(".beads").exists() {
        push(
            &mut lines,
            "beads",
            true,
            ".beads/ leftover — warning only".into(),
            true,
        );
    }

    if which_bd() {
        push(
            &mut lines,
            "bd",
            true,
            "`bd` is still on PATH. Uninstall the Beads plugin when you are ready.".into(),
            true,
        );
    }

    DoctorReport { ok, lines }
}

fn which_bd() -> bool {
    std::process::Command::new("bd")
        .arg("--help")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn upsert_agents_creates_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AGENTS.md");
        upsert_agents(&path).unwrap();
        let first = fs::read_to_string(&path).unwrap();
        assert!(first.contains(BEGIN));
        fs::write(&path, format!("hello\n{BEGIN}\nold\n{END}\nfooter\n")).unwrap();
        upsert_agents(&path).unwrap();
        let second = fs::read_to_string(&path).unwrap();
        assert!(second.contains("gbd prime"));
        assert!(second.contains("footer"));
        assert!(!second.contains("old\n"));
    }

    #[test]
    fn upsert_agents_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        fs::write(&path, "# Project\n\nrules\n").unwrap();
        upsert_agents(&path).unwrap();
        let once = fs::read_to_string(&path).unwrap();
        upsert_agents(&path).unwrap();
        upsert_agents(&path).unwrap();
        let thrice = fs::read_to_string(&path).unwrap();
        assert_eq!(once, thrice, "re-running init must not grow the file");
        assert!(once.starts_with("# Project\n\nrules\n\n<!-- BEGIN GBD -->"));
        assert!(once.ends_with("<!-- END GBD -->\n"));
    }
}
