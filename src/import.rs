//! `gbd import --from-beads`: turn a parsed Beads export into a creation
//! plan. The plan is pure (no GitHub calls) so `--dry-run` can print it;
//! executing it is the next step.
//!
//! Mapping, in gbd's data model and never a label: type → issue type,
//! priority → Priority, `blocks` → dependency, `parent-child` → sub-issue,
//! `in_progress` → board In Progress, deferred / `defer_until` → board
//! Deferred + Start date, closed → closed with a reason, notes and
//! comments → comments.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::beads::{self, Bead, Export, Kind, Status};
use crate::project;

/// GitHub's three close reasons, chosen from Beads' free-text
/// `close_reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    Completed,
    NotPlanned,
    Duplicate,
}

impl CloseReason {
    /// The `gh issue close --reason` value.
    pub fn as_flag(self) -> &'static str {
        match self {
            CloseReason::Completed => "completed",
            CloseReason::NotPlanned => "not planned",
            CloseReason::Duplicate => "duplicate",
        }
    }

    /// Word-level guess. Anything that does not read as dropped or
    /// duplicated is completed, which is also GitHub's default.
    pub fn guess(text: Option<&str>) -> Self {
        const DUPLICATE: [&str; 2] = ["duplicate", "dupe"];
        const NOT_PLANNED: [&str; 14] = [
            "wontfix",
            "won't fix",
            "wont fix",
            "not planned",
            "cancel",
            "obsolete",
            "supersed",
            "invalid",
            "abandon",
            "no longer",
            "out of scope",
            "dropped",
            "declin",
            "moot",
        ];
        let t = text.unwrap_or("").to_ascii_lowercase();
        if DUPLICATE.iter().any(|w| t.contains(w)) {
            CloseReason::Duplicate
        } else if NOT_PLANNED.iter().any(|w| t.contains(w)) {
            CloseReason::NotPlanned
        } else {
            CloseReason::Completed
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum State {
    Open,
    Closed { reason: CloseReason },
}

/// One issue to create, in creation order. Ids are Beads ids; the
/// executor swaps them for issue numbers as it goes.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub bead: String,
    pub title: String,
    /// Epic / Feature / Bug / Task / Chore.
    pub issue_type: &'static str,
    /// 0–4, written as P0–P4.
    pub priority: u8,
    pub parent: Option<String>,
    pub blocked_by: Vec<String>,
    pub assignee: Option<String>,
    pub state: State,
    /// Board Status once created.
    pub status: &'static str,
    /// Org field Start date, `YYYY-MM-DD`.
    pub start_date: Option<String>,
    /// Beads labels. Never written as labels: they go into the body's
    /// import footer so nothing is lost.
    pub labels: Vec<String>,
    pub body: String,
    pub comments: Vec<String>,
}

/// Something in the export the plan drops, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Skip {
    pub bead: String,
    pub what: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub items: Vec<Item>,
    /// Every `bd remember` line, to be upserted onto the memories issue.
    pub memories: Vec<beads::Memory>,
    /// Groups of beads whose edges form a cycle. They are still created,
    /// at the end, with the edges that close the cycle dropped.
    pub cycles: Vec<Vec<String>>,
    pub skipped: Vec<Skip>,
    /// Parse-time problems, rendered.
    pub problems: Vec<String>,
    /// Beads the mapping file has as done; the run skips them.
    #[serde(default)]
    pub already_imported: Vec<String>,
    /// Beads a previous run created but did not finish; the run finishes
    /// them without creating anything.
    #[serde(default)]
    pub partially_imported: Vec<String>,
}

// ---------------------------------------------------------------------------
// Mapping

/// Beads type → org issue type. Anything gbd has no type for becomes a
/// Task, and the plan says so.
fn issue_type(kind: &Kind) -> (&'static str, bool) {
    match kind {
        Kind::Epic => ("Epic", true),
        Kind::Feature => ("Feature", true),
        Kind::Bug => ("Bug", true),
        Kind::Task => ("Task", true),
        Kind::Chore => ("Chore", true),
        Kind::Decision | Kind::Other(_) => ("Task", false),
    }
}

/// `2026-10-01T00:00:00Z` → `2026-10-01`.
fn date_of(ts: &str) -> Option<String> {
    let d = ts.get(..10)?;
    let ok = d.len() == 10
        && d.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    ok.then(|| d.to_string())
}

fn body_of(b: &Bead) -> String {
    let mut body = b.description.trim_end().to_string();
    for (heading, text) in [
        ("Design", &b.design),
        ("Acceptance criteria", &b.acceptance_criteria),
    ] {
        if let Some(t) = text.as_deref().filter(|t| !t.trim().is_empty()) {
            if !body.is_empty() {
                body.push_str("\n\n");
            }
            let _ = write!(body, "## {heading}\n\n{}", t.trim_end());
        }
    }
    if !body.is_empty() {
        body.push_str("\n\n");
    }
    let _ = write!(body, "---\nImported from Beads `{}`", b.id);
    if let Some(d) = date_of(&b.created_at) {
        let _ = write!(body, " (created {d}");
        if let Some(who) = &b.created_by {
            let _ = write!(body, " by {who}");
        }
        body.push(')');
    }
    body.push('.');
    // Fields GitHub has no home for stay readable here instead of vanishing.
    if !b.labels.is_empty() {
        let _ = write!(body, " Beads labels: {}.", b.labels.join(", "));
    }
    let closed = b.closed_at.as_deref().and_then(date_of).map(|d| {
        match b
            .close_reason
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
        {
            Some(reason) => format!("{d} ({reason})"),
            None => d,
        }
    });
    let extras = [
        ("Owner", b.owner.clone()),
        ("Started", b.started_at.as_deref().and_then(date_of)),
        ("Closed", closed),
        ("Updated", b.updated_at.as_deref().and_then(date_of)),
        ("Due", b.due_at.as_deref().and_then(date_of)),
        ("Estimate", b.estimated_minutes.map(|m| format!("{m} min"))),
        ("Ref", b.external_ref.clone()),
    ];
    for (label, value) in extras {
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            let _ = write!(body, " {label}: {}.", v.trim());
        }
    }
    body
}

fn comments_of(b: &Bead) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(n) = b.notes.as_deref().filter(|n| !n.trim().is_empty()) {
        out.push(format!("**Notes**\n\n{}", n.trim_end()));
    }
    for c in &b.comments {
        let mut head = String::new();
        if !c.author.is_empty() {
            let _ = write!(head, "**{}**", c.author);
        }
        if let Some(d) = date_of(&c.created_at) {
            if !head.is_empty() {
                head.push_str(" · ");
            }
            head.push_str(&d);
        }
        if head.is_empty() {
            out.push(c.text.trim_end().to_string());
        } else {
            out.push(format!("{head}\n\n{}", c.text.trim_end()));
        }
    }
    out
}

/// Where the card lands. `open_blocker` says whether any blocker in the
/// export is still open.
fn board_status(b: &Bead, open_blocker: bool) -> &'static str {
    match b.status {
        Status::Closed => project::STATUS_DONE,
        // A date wins over in_progress: deferred work is not being worked.
        Status::Deferred => project::STATUS_DEFERRED,
        _ if b.defer_until.is_some() => project::STATUS_DEFERRED,
        Status::InProgress => project::STATUS_IN_PROGRESS,
        _ if open_blocker => project::STATUS_BLOCKED,
        _ => project::STATUS_READY,
    }
}

// ---------------------------------------------------------------------------
// Ordering

/// Kahn's algorithm over parent and blocked-by edges, ties broken by
/// creation time then id. Returns the order and what could not be placed
/// (members of a cycle, or behind one), which `residual` then orders.
fn order(export: &Export) -> (Vec<String>, Vec<String>) {
    let by_id: HashMap<&str, &Bead> = export.issues.iter().map(|b| (b.id.as_str(), b)).collect();
    let key = |id: &str| -> (String, String) {
        let b = by_id[id];
        (b.created_at.clone(), b.id.clone())
    };
    let mut indegree: HashMap<&str, usize> = HashMap::new();
    let mut downstream: HashMap<&str, Vec<&str>> = HashMap::new();
    for b in &export.issues {
        indegree.entry(&b.id).or_insert(0);
        let ups = b.parent.iter().chain(&b.blocked_by);
        for up in ups.filter(|u| by_id.contains_key(u.as_str())) {
            *indegree.entry(&b.id).or_insert(0) += 1;
            downstream.entry(up).or_default().push(&b.id);
        }
    }
    let mut ready: BTreeSet<(String, String)> = indegree
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| key(id))
        .collect();
    let mut placed = Vec::new();
    while let Some(next) = ready.pop_first() {
        let id = next.1;
        if let Some(children) = downstream.get(id.as_str()) {
            for c in children {
                let n = indegree.get_mut(c).expect("known node");
                *n -= 1;
                if *n == 0 {
                    ready.insert(key(c));
                }
            }
        }
        placed.push(id);
    }
    let mut stuck: Vec<String> = indegree
        .iter()
        .filter(|(_, n)| **n > 0)
        .map(|(id, _)| (*id).to_string())
        .collect();
    stuck.sort();
    (placed, stuck)
}

/// Tarjan's walk over the residual graph. Components come out in reverse
/// topological order of the condensation, which is exactly what placing
/// them needs; the ones of size > 1 are real cycles.
struct Tarjan<'a> {
    out: &'a BTreeMap<&'a str, Vec<&'a str>>,
    index: HashMap<&'a str, usize>,
    low: HashMap<&'a str, usize>,
    on_stack: BTreeSet<&'a str>,
    stack: Vec<&'a str>,
    next: usize,
    /// Every component, in emission order.
    components: Vec<Vec<String>>,
}
impl<'a> Tarjan<'a> {
    fn visit(&mut self, v: &'a str) {
        self.index.insert(v, self.next);
        self.low.insert(v, self.next);
        self.next += 1;
        self.stack.push(v);
        self.on_stack.insert(v);
        for &w in self.out.get(v).map(Vec::as_slice).unwrap_or_default() {
            if !self.index.contains_key(w) {
                self.visit(w);
                let lw = self.low[w];
                let lv = self.low.get_mut(v).expect("visited");
                *lv = (*lv).min(lw);
            } else if self.on_stack.contains(w) {
                let iw = self.index[w];
                let lv = self.low.get_mut(v).expect("visited");
                *lv = (*lv).min(iw);
            }
        }
        if self.low[v] == self.index[v] {
            let mut comp = Vec::new();
            while let Some(w) = self.stack.pop() {
                self.on_stack.remove(w);
                comp.push(w.to_string());
                if w == v {
                    break;
                }
            }
            self.components.push(comp);
        }
    }
}

/// Place what Kahn could not: components in topological order (a bead
/// behind a cycle comes after the cycle, whatever its timestamp), members
/// of a cycle by creation time then id. Also returns the cycles, a bead
/// that blocks or parents itself included.
fn residual(export: &Export, ids: &[String]) -> (Vec<String>, Vec<Vec<String>>) {
    let set: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    let by_id: HashMap<&str, &Bead> = export.issues.iter().map(|b| (b.id.as_str(), b)).collect();
    // Edges point downstream: blocker → blocked, parent → child.
    let mut out: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for id in &set {
        let b = by_id[id];
        for up in b.parent.iter().chain(&b.blocked_by) {
            if set.contains(up.as_str()) {
                out.entry(up.as_str()).or_default().push(id);
            }
        }
    }
    let mut t = Tarjan {
        out: &out,
        index: HashMap::new(),
        low: HashMap::new(),
        on_stack: BTreeSet::new(),
        stack: Vec::new(),
        next: 0,
        components: Vec::new(),
    };
    for id in &set {
        if !t.index.contains_key(id) {
            t.visit(id);
        }
    }
    let mut components = t.components;
    components.reverse();
    for c in &mut components {
        c.sort_by_key(|id| (by_id[id.as_str()].created_at.clone(), id.clone()));
    }
    // A bead that depends on itself is a one-node cycle.
    let self_edge = |c: &Vec<String>| {
        c.len() == 1
            && out
                .get(c[0].as_str())
                .is_some_and(|d| d.contains(&c[0].as_str()))
    };
    let mut cycles: Vec<Vec<String>> = components
        .iter()
        .filter(|c| c.len() > 1 || self_edge(c))
        .map(|c| {
            let mut c = c.clone();
            c.sort();
            c
        })
        .collect();
    cycles.sort();
    (components.into_iter().flatten().collect(), cycles)
}

// ---------------------------------------------------------------------------
// The plan

pub fn plan(export: &Export) -> Plan {
    let by_id: HashMap<&str, &Bead> = export.issues.iter().map(|b| (b.id.as_str(), b)).collect();
    let (placed, stuck) = order(export);
    let (behind, cycle_groups) = residual(export, &stuck);
    let mut skipped: Vec<Skip> = Vec::new();
    let mut items = Vec::new();
    let mut done: BTreeSet<&str> = BTreeSet::new();
    for id in placed.iter().chain(&behind) {
        let b = by_id[id.as_str()];
        let (issue_type, native) = issue_type(&b.kind);
        if !native {
            skipped.push(Skip {
                bead: b.id.clone(),
                what: format!(
                    "type {:?} has no issue type; created as Task",
                    b.kind.as_str()
                ),
            });
        }
        // Edges must point at something created earlier. A target outside
        // the export, or one still unplaced (a cycle), is dropped.
        let mut keep = |target: &String, kind: &str| -> bool {
            if done.contains(target.as_str()) {
                return true;
            }
            let why = if by_id.contains_key(target.as_str()) {
                "part of a dependency cycle"
            } else {
                "not in the export"
            };
            skipped.push(Skip {
                bead: b.id.clone(),
                what: format!("{kind} {target} dropped: {why}"),
            });
            false
        };
        let parent = b.parent.iter().find(|p| keep(p, "parent")).cloned();
        let blocked_by: Vec<String> = b
            .blocked_by
            .iter()
            .filter(|t| keep(t, "blocked-by"))
            .cloned()
            .collect();
        for e in &b.other_deps {
            skipped.push(Skip {
                bead: b.id.clone(),
                what: format!("{} edge to {} has no GitHub relation", e.kind, e.to),
            });
        }
        // From the edges that survived, so a card never says Blocked when
        // the issue behind it has no blocker.
        let open_blocker = blocked_by
            .iter()
            .filter_map(|t| by_id.get(t.as_str()))
            .any(|t| t.status != Status::Closed);
        let state = match b.status {
            Status::Closed => State::Closed {
                reason: CloseReason::guess(b.close_reason.as_deref()),
            },
            _ => State::Open,
        };
        let start_date = b.defer_until.as_deref().and_then(date_of);
        if b.defer_until.is_some() && start_date.is_none() {
            skipped.push(Skip {
                bead: b.id.clone(),
                what: "defer_until is not a date; Start date not set".into(),
            });
        }
        items.push(Item {
            bead: b.id.clone(),
            title: b.title.clone(),
            issue_type,
            priority: b.priority,
            parent,
            blocked_by,
            assignee: b.assignee.clone(),
            state,
            status: board_status(b, open_blocker),
            start_date,
            labels: b.labels.clone(),
            body: body_of(b),
            comments: comments_of(b),
        });
        done.insert(&b.id);
    }
    let mut seen_keys = BTreeSet::new();
    for m in &export.memories {
        if !seen_keys.insert(m.key.as_str()) {
            skipped.push(Skip {
                bead: "memory".into(),
                what: format!(
                    "key {:?} appears more than once; the last value wins",
                    m.key
                ),
            });
        }
    }
    Plan {
        items,
        memories: export.memories.clone(),
        cycles: cycle_groups,
        skipped,
        problems: export.problems.iter().map(ToString::to_string).collect(),
        already_imported: Vec::new(),
        partially_imported: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// The mapping file

/// How far a bead got. Each step appends a new line; the last line for a
/// bead wins, so a run killed anywhere resumes at the right step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// The issue exists. Priority, dates, assignee, close, and the card are
    /// idempotent and get replayed; `comments` says how many comments are
    /// on it already.
    Created,
    /// Everything is on it.
    #[default]
    Done,
}

/// One line of the mapping file: which issue a bead became, and how far
/// the import got with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mapped {
    pub bead: String,
    pub number: u64,
    pub url: String,
    #[serde(default)]
    pub phase: Phase,
    /// Comments already on the issue while the phase is `created`, so a
    /// resume posts only the rest.
    #[serde(default)]
    pub comments: usize,
    /// The body was already edited for forward references, so a retry
    /// leaves it alone.
    #[serde(default)]
    pub rewritten: bool,
}

impl Mapped {
    /// `owner/repo` from the issue URL.
    pub fn repo(&self) -> Option<&str> {
        let rest = self.url.strip_prefix("https://github.com/")?;
        let (repo, tail) = rest.split_once("/issues/")?;
        (repo.matches('/').count() == 1 && !tail.is_empty()).then_some(repo)
    }
}

/// The mapping file's records, last line per bead winning; absent means
/// empty.
pub fn read_mapping(path: &Path) -> Result<BTreeMap<String, Mapped>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
    };
    let mut map = BTreeMap::new();
    let complete = text.ends_with('\n');
    let lines: Vec<&str> = text.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Mapped>(line) {
            Ok(m) => {
                map.insert(m.bead.clone(), m);
            }
            // A last line without its newline is a write that was cut off
            // (disk full, kill): it recorded nothing, so it is ignored, and
            // `Mapping::open` drops it before appending.
            Err(_) if idx + 1 == lines.len() && !complete => {
                eprintln!(
                    "warning: {}: ignoring an incomplete last line (an interrupted write)",
                    path.display()
                );
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("{}:{}: not a mapping line", path.display(), idx + 1)
                });
            }
        }
    }
    Ok(map)
}

/// `<!-- gbd-import wx-1/2 -->`: a hidden marker at the end of each
/// imported comment, so a resume can tell which ones GitHub already has.
pub fn comment_marker(bead: &str, k: usize) -> String {
    format!("<!-- gbd-import {bead}/{k} -->")
}

/// The highest comment index found by marker among `bodies` for `bead`.
pub fn posted_comments(bodies: &[&str], bead: &str) -> usize {
    let prefix = format!("<!-- gbd-import {bead}/");
    bodies
        .iter()
        .filter_map(|b| {
            let start = b.rfind(&prefix)? + prefix.len();
            let digits: String = b[start..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse::<usize>().ok()
        })
        .max()
        .unwrap_or(0)
}

/// Mark on the plan which beads the mapping file says are done or partly
/// done, so the report and the refusal can say so.
pub fn note_imported(plan: &mut Plan, done: &BTreeMap<String, Mapped>) {
    let with_phase = |phase: fn(Phase) -> bool| -> Vec<String> {
        plan.items
            .iter()
            .filter(|i| done.get(&i.bead).is_some_and(|m| phase(m.phase)))
            .map(|i| i.bead.clone())
            .collect()
    };
    plan.already_imported = with_phase(|p| p == Phase::Done);
    plan.partially_imported = with_phase(|p| p != Phase::Done);
}

/// The first record that belongs to another repository than `repo`, if any.
pub fn foreign_entry<'a>(map: &'a BTreeMap<String, Mapped>, repo: &str) -> Option<&'a Mapped> {
    map.values()
        .find(|m| !m.repo().is_some_and(|r| r.eq_ignore_ascii_case(repo)))
}

/// A file whose last line has no newline ends in a cut-off write. If that
/// tail still parses it is a whole record and only needs its newline;
/// otherwise it is dropped so the next record starts on a clean line. Done
/// in place on the locked handle: a truncate is one call, so a kill during
/// it leaves the old file or the new one, and no rename moves the lock off
/// the file.
fn repair_tail(file: &mut std::fs::File, path: &Path) -> Result<()> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("opening {}", path.display()))?;
    if text.is_empty() || text.ends_with('\n') {
        return Ok(());
    }
    let cut = text.rfind('\n').map_or(0, |i| i + 1);
    let repaired = if serde_json::from_str::<Mapped>(&text[cut..]).is_ok() {
        writeln!(file).and_then(|()| file.flush())
    } else {
        file.set_len(cut as u64)
    };
    repaired.with_context(|| format!("repairing {}", path.display()))
}

/// Append-only, flushed per line: a run killed halfway loses nothing, and
/// the next run resumes from it. Locked for as long as the value lives.
#[derive(Debug)]
pub struct Mapping {
    path: PathBuf,
    file: std::fs::File,
}

impl Mapping {
    /// Opens the file, creating it, and takes its lock for the life of the
    /// value: a second import on the same file fails at once instead of
    /// starting from the same state and creating everything twice. The
    /// lock goes with the process, so a killed run leaves nothing behind.
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening {} for writing", path.display()))?;
        // The lock is held for a whole run, so a busy file stays busy; the
        // brief retry covers only a descriptor momentarily duplicated into
        // a child another thread is spawning (the fork-to-exec window).
        let mut tries = 0;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if tries < 20 => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(std::fs::TryLockError::WouldBlock) => anyhow::bail!(
                    "{} is in use by another gbd import; wait for it to finish",
                    path.display()
                ),
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(e).with_context(|| format!("locking {}", path.display()));
                }
            }
        }
        repair_tail(&mut file, path)?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn record(&mut self, m: &Mapped) -> Result<()> {
        let line = serde_json::to_string(m).context("encoding mapping line")?;
        writeln!(self.file, "{line}")
            .and_then(|()| self.file.flush())
            .with_context(|| format!("writing {}", self.path.display()))
    }
}

/// Byte length of the line at the start of `s`, newline included.
fn line_len(s: &str) -> usize {
    s.find('\n').map_or(s.len(), |i| i + 1)
}

/// Byte length of a link title at the start of `s`: quoted with `"` or
/// `'`, or in parentheses, on one line.
fn title_len(s: &str) -> Option<usize> {
    let close = match s.chars().next()? {
        '"' => '"',
        '\'' => '\'',
        '(' => ')',
        _ => return None,
    };
    let end = s[1..].find([close, '\n'])?;
    s[1 + end..].starts_with(close).then_some(end + 2)
}

/// The bytes at the start of `rest` that a link reference definition
/// takes, as `CommonMark` defines one: `[label]:` after up to three spaces,
/// whitespace with at most one line ending, a destination (`<…>` or a run
/// of non-whitespace), then optionally a title, on the same line or the
/// next, with nothing else after either. None when it is not one.
fn reference_definition_end(rest: &str) -> Option<usize> {
    let line = &rest[..line_len(rest)];
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 || !trimmed.starts_with('[') {
        return None;
    }
    let close = label_end(trimmed)?;
    if close < 2 || !trimmed[close + 1..].starts_with(':') {
        return None;
    }
    let blanks = |s: &str| s.len() - s.trim_start_matches([' ', '\t']).len();
    let mut at = line.len() - trimmed.len() + close + 2;
    at += blanks(&rest[at..]);
    if rest[at..].starts_with('\n') {
        at += 1;
        at += blanks(&rest[at..]);
    }
    let dest = &rest[at..];
    let dest_len = if dest.starts_with('<') {
        dest.find(['>', '\n'])
            .filter(|&i| dest[i..].starts_with('>'))?
            + 1
    } else {
        dest.find(char::is_whitespace).unwrap_or(dest.len())
    };
    if dest_len == 0 {
        return None;
    }
    at += dest_len;
    // The rest of the line is blank, or a title and then blank.
    at += blanks(&rest[at..]);
    let title_here = rest[at..].starts_with(['"', '\'', '(']);
    if title_here {
        at += title_len(&rest[at..])?;
        at += blanks(&rest[at..]);
    }
    if !(rest[at..].is_empty() || rest[at..].starts_with('\n')) {
        return None;
    }
    at += usize::from(rest[at..].starts_with('\n'));
    // A title alone on the next line belongs to the definition too.
    if !title_here {
        let next = &rest[at..];
        let lead = blanks(next);
        if let Some(n) = title_len(&next[lead..]) {
            let after = lead + n + blanks(&next[lead + n..]);
            if next[after..].is_empty() || next[after..].starts_with('\n') {
                at += after + usize::from(next[after..].starts_with('\n'));
            }
        }
    }
    Some(at)
}

/// Block-quote markers at the start of a line: how many `>` prefixes (each
/// after up to three spaces, with an optional space after it) there are,
/// up to `max`, and what follows them.
fn unquote(line: &str, max: usize) -> (usize, &str) {
    let (mut depth, mut s) = (0, line);
    while depth < max {
        let t = s.trim_start_matches(' ');
        if s.len() - t.len() > 3 || !t.starts_with('>') {
            break;
        }
        depth += 1;
        s = t[1..].strip_prefix(' ').unwrap_or(&t[1..]);
    }
    (depth, s)
}

/// Byte index of the `]` that closes the link label opening `s`, with
/// backslash escapes honoured; None if the line ends first.
fn label_end(s: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices().skip(1) {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            ']' => return Some(i),
            '\n' => return None,
            _ => {}
        }
    }
    None
}

/// The fence a line opens, as (char, run length, block-quote depth): three
/// or more backticks or tildes after up to three spaces, inside any block
/// quote. A backtick fence's info string cannot contain a backtick.
fn fence_open(line: &str) -> Option<(char, usize, usize)> {
    let (depth, line) = unquote(line, usize::MAX);
    let s = line.trim_start_matches(' ');
    if line.len() - s.len() > 3 {
        return None;
    }
    let c = s.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let n = s.chars().take_while(|&x| x == c).count();
    (n >= 3 && (c == '~' || !s[n..].contains('`'))).then_some((c, n, depth))
}

/// Whether a line closes a fence of `n` of `c`: a run at least as long
/// after up to three spaces, with nothing but whitespace after it.
fn fence_close(line: &str, c: char, n: usize) -> bool {
    let s = line.trim_start_matches(' ');
    let run = s.chars().take_while(|&x| x == c).count();
    line.len() - s.len() <= 3 && run >= n && s[run..].trim().is_empty()
}

/// Byte index of the `)` that closes a link destination starting at the
/// beginning of `s`, with nested parentheses balanced and backslash-escaped
/// ones skipped.
fn destination_end(s: &str) -> Option<usize> {
    let mut depth = 1usize;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Where the scanner is with respect to Markdown code.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Code {
    Prose,
    /// Inside a span opened by a run of this many backticks; only a run of
    /// the same length closes it, so a literal backtick inside is fine.
    Span(usize),
    /// Inside a fenced block opened by a line of this many backticks or
    /// tildes (the char says which) at this block-quote depth; only a line
    /// of at least as many closes it, so a run of the fence char inside the
    /// block is fine, and the quote ending ends it too.
    Fence(char, usize, usize),
}

/// Where the paragraph at the start of `s` ends: the start of its first
/// blank line, or the end of `s`.
fn paragraph_end(s: &str) -> usize {
    let mut at = 0;
    for line in s.split_inclusive('\n') {
        if line.trim().is_empty() {
            return at;
        }
        at += line.len();
    }
    s.len()
}

/// Whether a run of exactly `n` backticks occurs before the paragraph ends.
fn has_closer(s: &str, n: usize) -> bool {
    let mut run = 0;
    for c in s[..paragraph_end(s)].chars().chain(std::iter::once('\0')) {
        if c == '`' {
            run += 1;
            continue;
        }
        if run == n {
            return true;
        }
        run = 0;
    }
    false
}

/// `str::find` without regard to ASCII case; `needle` is ASCII.
fn find_ignore_ascii_case(hay: &str, needle: &str) -> Option<usize> {
    hay.as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Byte length of `hay` up to and including the first `</name>` (any case,
/// whitespace allowed before the `>`); the name must end there, so
/// `</code-example>` does not close `<code>`.
fn closing_tag_end(hay: &str, name: &str) -> Option<usize> {
    let needle = format!("</{name}");
    let mut from = 0;
    while let Some(j) = find_ignore_ascii_case(&hay[from..], &needle) {
        let after = from + j + needle.len();
        let rest = hay[after..].trim_start_matches([' ', '\t', '\n', '\r']);
        if let Some(r) = rest.strip_prefix('>') {
            return Some(hay.len() - r.len());
        }
        from = after;
    }
    None
}

/// Byte index of the `>` that closes an HTML tag whose `<` sits just before
/// `s`, quotes respected; None when it does not close in the paragraph.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in s[..paragraph_end(s)].char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None if matches!(c, '"' | '\'') => quote = Some(c),
            None if c == '>' => return Some(i),
            None => {}
        }
    }
    None
}

/// Advance the span state across the first `len` bytes of `text`. The rest
/// of `text` is only looked at to tell an opening run from a stray one: a
/// run with no closer of the same length before the paragraph ends is
/// prose, and so is an escaped backtick. Fences are whole lines, seen at
/// the line start, never here.
fn note_code(text: &str, len: usize, code: &mut Code) {
    let (mut run, mut escaped) = (0usize, false);
    for (i, c) in text[..len]
        .char_indices()
        .chain(std::iter::once((len, '\0')))
    {
        if c == '`' && !escaped {
            run += 1;
            continue;
        }
        if run > 0 {
            *code = match *code {
                Code::Prose if has_closer(&text[i..], run) => Code::Span(run),
                Code::Span(n) if n == run => Code::Prose,
                same => same,
            };
            run = 0;
        }
        escaped = !escaped && c == '\\' && *code == Code::Prose;
    }
}

/// A line indented enough to be, or to continue, an indented code block.
fn is_indented(line: &str) -> bool {
    line.starts_with("    ") || line.starts_with('\t')
}

/// `wx-12` → `#101` for every mention whose target is in `known`. A token
/// is a maximal run of id characters; trailing dots are punctuation. Left
/// alone: anything inside a code span, a fenced block, or an indented code
/// block (the import footer and command examples), a bare URL (a word
/// containing `://`, words ending at whitespace or angle or square
/// brackets, so parentheses inside a URL are part of it), a Markdown
/// link destination (`](…)`), a reference definition (`[label]: …`), an
/// HTML tag or comment (`<a href="…">`), and a `<code>` or `<pre>` element;
/// the link's visible text is still rewritten. Anything not in `known`
/// stays as written.
///
/// An indented code block is a line indented four spaces or a tab after a
/// blank line (or at the start), through the next line indented less. A
/// nested list item indented that far after a blank line counts too and is
/// left as written: the safe direction. Block-quote markers are seen
/// through; no other container is modelled beyond its indentation.
pub fn rewrite_ids(text: &str, known: &BTreeMap<String, u64>) -> String {
    fn id_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_')
    }
    fn boundary(c: char) -> bool {
        c.is_whitespace() || matches!(c, '[' | ']' | '<' | '>')
    }
    fn tag_start(c: char) -> bool {
        c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?')
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut md = Code::Prose;
    // For indented code blocks: the previous line was blank (or there was
    // none), and the scanner is inside such a block.
    let mut prev_blank = true;
    let mut indented = false;
    while !rest.is_empty() {
        let line_end = rest.find('\n').map_or(rest.len(), |i| i + 1);
        // Whole-line rules at a line start: a reference definition line is
        // all destination, and an indented code block is copied through.
        if out.is_empty() || out.ends_with('\n') {
            let line = &rest[..line_end];
            // Block-quote markers are seen through by the line rules.
            let (_, inner) = unquote(line, usize::MAX);
            let strip = line.len() - inner.len();
            let blank = inner.trim().is_empty();
            // Inside a fence every line is code, and only a line of the
            // fence closes it, or the block quote it sits in ending.
            let mut verbatim = None;
            if let Code::Fence(c, n, depth) = md {
                // Only the fence's own markers come off: a deeper marker is
                // code inside it.
                let (d, inner) = unquote(line, depth);
                if d < depth {
                    md = Code::Prose;
                } else {
                    if fence_close(inner, c, n) {
                        md = Code::Prose;
                    }
                    verbatim = Some(line_end);
                }
            }
            if verbatim.is_none() {
                if blank {
                    // A blank line ends the paragraph, and any span in it.
                    md = Code::Prose;
                }
                if indented && !blank && !is_indented(inner) {
                    indented = false;
                }
                if !indented && prev_blank && !blank && is_indented(inner) {
                    indented = true;
                }
                if indented {
                    verbatim = Some(line_end);
                } else if let Some(len) = reference_definition_end(&rest[strip..]) {
                    verbatim = Some(strip + len);
                } else if let Some((c, n, depth)) = fence_open(line) {
                    md = Code::Fence(c, n, depth);
                    verbatim = Some(line_end);
                }
            }
            prev_blank = blank;
            if let Some(len) = verbatim {
                out.push_str(&rest[..len]);
                rest = &rest[len..];
                continue;
            }
        }
        // Inside `](…)`: copy the destination through its closing paren,
        // parentheses inside it balanced.
        if out.ends_with("](") {
            match destination_end(rest) {
                Some(i) => {
                    out.push_str(&rest[..=i]);
                    rest = &rest[i + 1..];
                    continue;
                }
                None => break,
            }
        }
        // Inside an HTML tag: copy it through its `>`; one that never
        // closes is prose. A `<code>` or `<pre>` element is code through
        // its closing tag.
        if md == Code::Prose && out.ends_with('<') && rest.starts_with(tag_start) {
            if let Some(i) = tag_end(rest) {
                let name: String = rest[..i]
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .map(|c| c.to_ascii_lowercase())
                    .collect();
                let mut end = i + 1;
                if matches!(name.as_str(), "code" | "pre") && !rest[..i].ends_with('/') {
                    let body = end;
                    end = closing_tag_end(&rest[body..], &name).map_or(rest.len(), |e| body + e);
                }
                out.push_str(&rest[..end]);
                rest = &rest[end..];
                continue;
            }
        }
        // Only the rest of this line, so the next line starts at the top.
        let Some(start) = rest[..line_end].find(id_char) else {
            note_code(rest, line_end, &mut md);
            out.push_str(&rest[..line_end]);
            rest = &rest[line_end..];
            continue;
        };
        let literal = &rest[..start];
        // A destination that starts with something other than an id char
        // (`](/issues/wx-2)`): the `](` is inside this literal run.
        if let Some(p) = literal.rfind("](") {
            if !literal[p..].contains(')') {
                let head = &literal[..p + 2];
                note_code(rest, head.len(), &mut md);
                out.push_str(head);
                rest = &rest[head.len()..];
                continue;
            }
        }
        // An HTML tag that starts in this run (`<a href="/x/wx-2">`): copy
        // through the `<`; the tag is then copied through its `>`.
        if let Some(p) = literal.rfind('<') {
            if md == Code::Prose
                && !literal[p..].contains('>')
                && rest[p + 1..].starts_with(tag_start)
            {
                let head = &literal[..=p];
                note_code(rest, head.len(), &mut md);
                out.push_str(head);
                rest = &rest[head.len()..];
                continue;
            }
        }
        note_code(rest, start, &mut md);
        out.push_str(literal);
        rest = &rest[start..];
        let end = rest.find(|c: char| !id_char(c)).unwrap_or(rest.len());
        let token = &rest[..end];
        let core = token.trim_end_matches('.');
        let fenced = md != Code::Prose;
        let in_url = {
            let word_start = out
                .rfind(boundary)
                .map_or(0, |i| i + out[i..].chars().next().map_or(1, char::len_utf8));
            let word_end = rest[end..].find(boundary).map_or(rest.len(), |i| end + i);
            out[word_start..].contains("://") || rest[..word_end].contains("://")
        };
        match known.get(core) {
            Some(n) if !fenced && !in_url => {
                let _ = write!(out, "#{n}{}", &token[core.len()..]);
            }
            _ => out.push_str(token),
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// Report

/// How a Beads assignee is matched: case and runs of whitespace aside.
pub fn assignee_key(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// `NAME=LOGIN` flags as [`assignee_key`] → login; `None` when the login
/// is empty, which drops that assignee.
pub fn assignee_map(flags: &[String]) -> Result<BTreeMap<String, Option<String>>> {
    let mut map = BTreeMap::new();
    for flag in flags {
        // The last `=`: a login never contains one, a display name might.
        let (name, login) = flag
            .rsplit_once('=')
            .with_context(|| format!("--assignee {flag}: expected NAME=LOGIN"))?;
        let (name, login) = (name.trim(), login.trim());
        if name.is_empty() {
            anyhow::bail!("--assignee {flag}: the Beads name is empty");
        }
        if !login.is_empty() && !is_login(login) {
            anyhow::bail!("--assignee {flag}: {login} is not a GitHub login");
        }
        map.insert(
            assignee_key(name),
            (!login.is_empty()).then(|| login.to_string()),
        );
    }
    Ok(map)
}

/// The shape of a GitHub login: letters, digits, and single hyphens not at
/// either end, at most 39 of them.
pub fn is_login(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 39
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
        && !s.contains("--")
}

/// Replace every mapped assignee on the plan; an unmapped one is kept
/// trimmed, since `--add-assignee` takes a login, not padding.
pub fn map_assignees(plan: &mut Plan, map: &BTreeMap<String, Option<String>>) {
    for item in &mut plan.items {
        let Some(name) = item.assignee.take() else {
            continue;
        };
        item.assignee = match map.get(&assignee_key(&name)) {
            Some(login) => login.clone(),
            None => Some(name.trim().to_string()).filter(|n| !n.is_empty()),
        };
    }
}

/// Distinct assignees on the plan (spelling variants of one name counted
/// together, shown as first seen and trimmed) with how many beads carry
/// each, most first, and whether each can be a GitHub login.
pub fn assignee_summary(p: &Plan) -> Vec<(String, usize, bool)> {
    assignee_summary_of(p.items.iter())
}

/// [`assignee_summary`] over some of the items, for a resume that only
/// needs the unfinished ones checked. A blank assignee is no assignee.
pub fn assignee_summary_of<'a>(
    items: impl Iterator<Item = &'a Item>,
) -> Vec<(String, usize, bool)> {
    let mut counts: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for name in items
        .filter_map(|i| i.assignee.as_deref())
        .filter(|n| !n.trim().is_empty())
    {
        let entry = counts
            .entry(assignee_key(name))
            .or_insert_with(|| (name.trim().to_string(), 0));
        entry.1 += 1;
    }
    let mut out: Vec<(String, usize, bool)> = counts
        .into_values()
        .map(|(name, n)| {
            let ok = is_login(&name);
            (name, n, ok)
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// The dry-run text: counts, the order, and everything dropped.
pub fn render(p: &Plan, source: &str, order_lines: usize) -> String {
    let mut out = format!(
        "Import plan: {} issues, {} memories, from {source}\n\n",
        p.items.len(),
        p.memories.len()
    );
    let count = |f: &dyn Fn(&Item) -> String| -> BTreeMap<String, usize> {
        let mut m = BTreeMap::new();
        for i in &p.items {
            *m.entry(f(i)).or_insert(0) += 1;
        }
        m
    };
    let line = |label: &str, m: &BTreeMap<String, usize>| -> String {
        let parts: Vec<String> = m.iter().map(|(k, n)| format!("{k} {n}")).collect();
        format!("{label:<11} {}\n", parts.join(", "))
    };
    out.push_str(&line("By type:", &count(&|i| i.issue_type.to_string())));
    out.push_str(&line("Board:", &count(&|i| i.status.to_string())));
    out.push_str(&line(
        "State:",
        &count(&|i| match i.state {
            State::Open => "open".into(),
            State::Closed { reason } => format!("closed ({})", reason.as_flag()),
        }),
    ));
    out.push_str(&line("Priority:", &count(&|i| format!("P{}", i.priority))));
    let parents = p.items.iter().filter(|i| i.parent.is_some()).count();
    let blocks: usize = p.items.iter().map(|i| i.blocked_by.len()).sum();
    let comments: usize = p.items.iter().map(|i| i.comments.len()).sum();
    let labels: BTreeSet<&str> = p
        .items
        .iter()
        .flat_map(|i| i.labels.iter().map(String::as_str))
        .collect();
    let _ = writeln!(
        out,
        "{:<11} {parents} parent links, {blocks} blocked-by",
        "Edges:"
    );
    let _ = writeln!(
        out,
        "{:<11} {comments} comments, {} distinct Beads labels kept in the body footer (never labels)",
        "Also:",
        labels.len()
    );
    let assignees = assignee_summary(p);
    if !assignees.is_empty() {
        let parts: Vec<String> = assignees
            .iter()
            .map(|(name, n, ok)| {
                if *ok {
                    format!("{name} ({n})")
                } else {
                    format!("{name} ({n}) — not a GitHub login; pass --assignee '{name}=LOGIN'")
                }
            })
            .collect();
        let _ = writeln!(out, "{:<11} {}", "Assignees:", parts.join(", "));
    }
    let _ = writeln!(
        out,
        "\nOrder ({} of {}):",
        order_lines.min(p.items.len()),
        p.items.len()
    );
    for (n, i) in p.items.iter().take(order_lines).enumerate() {
        let mut edges = Vec::new();
        if let Some(parent) = &i.parent {
            edges.push(format!("parent {parent}"));
        }
        if !i.blocked_by.is_empty() {
            edges.push(format!("blocked by {}", i.blocked_by.join(" ")));
        }
        let edges = if edges.is_empty() {
            String::new()
        } else {
            format!("  ← {}", edges.join(", "))
        };
        let _ = writeln!(
            out,
            "{:>5}. {}  [{}] P{} {}{edges}",
            n + 1,
            i.bead,
            i.issue_type,
            i.priority,
            i.title
        );
    }
    if p.items.len() > order_lines {
        let _ = writeln!(
            out,
            "       … {} more (--json for all)",
            p.items.len() - order_lines
        );
    }
    if !p.memories.is_empty() {
        let keys: Vec<&str> = p.memories.iter().map(|m| m.key.as_str()).collect();
        let _ = writeln!(
            out,
            "\nMemories ({}), upserted by key: {}",
            keys.len(),
            keys.join(" ")
        );
    }
    out.push_str(&render_diagnostics(p));
    out.push_str("\nNothing written (--dry-run).\n");
    out
}

/// The parts of the report a real run must show too: cycles, what cannot
/// map, and the parser's problems. Empty when there is nothing to say.
pub fn render_diagnostics(p: &Plan) -> String {
    let mut out = String::new();
    if !p.cycles.is_empty() {
        let _ = writeln!(
            out,
            "\nDependency cycles ({}), created last with the closing edges dropped:",
            p.cycles.len()
        );
        for c in &p.cycles {
            let _ = writeln!(out, "  {}", c.join(" ↔ "));
        }
    }
    if !p.skipped.is_empty() {
        let _ = writeln!(out, "\nCannot map ({}):", p.skipped.len());
        for s in &p.skipped {
            let _ = writeln!(out, "  {}: {}", s.bead, s.what);
        }
    }
    if !p.already_imported.is_empty() {
        let _ = writeln!(
            out,
            "\nAlready imported ({}), skipped: {}",
            p.already_imported.len(),
            p.already_imported.join(" ")
        );
    }
    if !p.partially_imported.is_empty() {
        let _ = writeln!(
            out,
            "\nPartially imported ({}), to be finished: {}",
            p.partially_imported.len(),
            p.partially_imported.join(" ")
        );
    }
    if !p.problems.is_empty() {
        let _ = writeln!(out, "\nExport problems ({}):", p.problems.len());
        for m in &p.problems {
            let _ = writeln!(out, "  {m}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beads;

    fn fixture() -> Plan {
        let e =
            beads::parse(include_str!("../tests/fixtures/beads-export.jsonl").as_bytes()).unwrap();
        plan(&e)
    }

    fn pos(p: &Plan, id: &str) -> usize {
        p.items
            .iter()
            .position(|i| i.bead == id)
            .unwrap_or_else(|| panic!("{id} not planned"))
    }

    fn item<'a>(p: &'a Plan, id: &str) -> &'a Item {
        &p.items[pos(p, id)]
    }

    #[test]
    fn everything_is_planned_in_dependency_order() {
        let p = fixture();
        assert_eq!(p.items.len(), 8);
        assert_eq!(p.memories.len(), 1);
        assert_eq!(p.memories[0].key, "deploy-runbook");
        assert!(pos(&p, "wx-1") < pos(&p, "wx-1.1"), "parent first");
        assert!(pos(&p, "wx-2") < pos(&p, "wx-1.1"), "blocker first");
        assert!(pos(&p, "wx-1") < pos(&p, "wx-6"));
        assert!(p.cycles.is_empty());
        // Ties: creation time, then id.
        assert_eq!(
            p.items[0].bead,
            "wx-4",
            "{:?}",
            p.items.iter().map(|i| &i.bead).collect::<Vec<_>>()
        );
    }

    #[test]
    fn fields_map_onto_the_gbd_model() {
        let p = fixture();
        let epic = item(&p, "wx-1");
        assert_eq!(epic.issue_type, "Epic");
        assert_eq!(epic.priority, 1);
        assert_eq!(epic.status, project::STATUS_READY);
        assert_eq!(epic.labels, vec!["area:api"]);
        assert!(epic.body.starts_with("Umbrella for the API rework."));
        assert!(
            epic.body.ends_with(
                "---\nImported from Beads `wx-1` (created 2026-03-01 by dev1). Beads labels: area:api. Owner: dev1. Updated: 2026-03-05."
            ),
            "{}",
            epic.body
        );
        assert!(
            item(&p, "wx-3")
                .body
                .ends_with("(created 2026-03-03 by dev2). Updated: 2026-03-03."),
            "only what the bead has"
        );
        assert!(
            item(&p, "wx-4")
                .body
                .ends_with("(created 2026-02-20 by dev2). Owner: dev2. Closed: 2026-02-21 (shipped in 1.4). Updated: 2026-02-21. Estimate: 30 min."),
            "{}",
            item(&p, "wx-4").body
        );
        assert_eq!(
            epic.comments,
            vec!["**dev1** · 2026-03-01\n\nKickoff notes in the wiki."]
        );

        let bug = item(&p, "wx-2");
        assert_eq!(bug.issue_type, "Bug");
        assert_eq!(bug.status, project::STATUS_IN_PROGRESS);
        assert_eq!(bug.assignee.as_deref(), Some("dev1"));

        let child = item(&p, "wx-1.1");
        assert_eq!(child.parent.as_deref(), Some("wx-1"));
        assert_eq!(child.blocked_by, vec!["wx-2"]);
        assert_eq!(child.status, project::STATUS_BLOCKED, "wx-2 is open");

        let deferred = item(&p, "wx-3");
        assert_eq!(deferred.status, project::STATUS_DEFERRED);
        assert_eq!(deferred.start_date.as_deref(), Some("2026-10-01"));
        assert_eq!(deferred.comments, vec!["**Notes**\n\nDesign in figma."]);

        let closed = item(&p, "wx-4");
        assert_eq!(
            closed.state,
            State::Closed {
                reason: CloseReason::Completed
            }
        );
        assert_eq!(closed.status, project::STATUS_DONE);

        let decision = item(&p, "wx-5");
        assert_eq!(decision.issue_type, "Task");
        assert!(decision.body.contains("## Design\n\nTwo options"));
        assert!(decision
            .body
            .contains("## Acceptance criteria\n\nA decision record"));
        assert!(decision.blocked_by.is_empty(), "wx-9 is not in the export");
        assert_eq!(
            decision.status,
            project::STATUS_READY,
            "a dropped blocker does not block"
        );
    }

    #[test]
    fn what_cannot_map_is_listed_with_a_reason() {
        let p = fixture();
        let what: Vec<String> = p
            .skipped
            .iter()
            .map(|s| format!("{}: {}", s.bead, s.what))
            .collect();
        let has = |s: &str| what.iter().any(|w| w.contains(s));
        assert!(
            has("wx-5: type \"decision\" has no issue type; created as Task"),
            "{what:?}"
        );
        assert!(
            has("wx-7: type \"wisp\" has no issue type; created as Task"),
            "{what:?}"
        );
        assert!(
            has("wx-5: blocked-by wx-9 dropped: not in the export"),
            "{what:?}"
        );
        assert!(
            has("wx-5: related edge to wx-1 has no GitHub relation"),
            "{what:?}"
        );
        assert_eq!(p.skipped.len(), 4, "{what:?}");
        assert_eq!(p.problems.len(), 6);
    }

    #[test]
    fn a_cycle_is_reported_and_broken_at_the_end() {
        let lines = "{\"id\":\"c-1\",\"title\":\"one\",\"issue_type\":\"task\",\"status\":\"open\",\"priority\":2,\"created_at\":\"2026-01-01T00:00:00Z\",\"dependencies\":[{\"issue_id\":\"c-1\",\"depends_on_id\":\"c-2\",\"type\":\"blocks\"}]}\n\
                     {\"id\":\"c-2\",\"title\":\"two\",\"issue_type\":\"task\",\"status\":\"open\",\"priority\":2,\"created_at\":\"2026-01-02T00:00:00Z\",\"dependencies\":[{\"issue_id\":\"c-2\",\"depends_on_id\":\"c-1\",\"type\":\"blocks\"}]}\n\
                     {\"id\":\"c-3\",\"title\":\"three\",\"issue_type\":\"task\",\"status\":\"open\",\"priority\":2,\"created_at\":\"2025-12-31T00:00:00Z\",\"dependencies\":[{\"issue_id\":\"c-3\",\"depends_on_id\":\"c-1\",\"type\":\"blocks\"}]}\n\
                     {\"id\":\"c-0\",\"title\":\"free\",\"issue_type\":\"task\",\"status\":\"open\",\"priority\":2,\"created_at\":\"2026-01-04T00:00:00Z\"}\n";
        let e = beads::parse(lines.as_bytes()).unwrap();
        let p = plan(&e);
        assert_eq!(p.cycles, vec![vec!["c-1".to_string(), "c-2".to_string()]]);
        let ids: Vec<&str> = p.items.iter().map(|i| i.bead.as_str()).collect();
        assert_eq!(
            ids,
            vec!["c-0", "c-1", "c-2", "c-3"],
            "free first; the cycle by time; c-3 after the cycle it depends on, although older"
        );
        assert_eq!(
            item(&p, "c-1").status,
            project::STATUS_READY,
            "no blocker survived, so not Blocked"
        );
        assert_eq!(item(&p, "c-2").status, project::STATUS_BLOCKED);
        assert!(
            item(&p, "c-1").blocked_by.is_empty(),
            "the closing edge is dropped"
        );
        assert_eq!(item(&p, "c-2").blocked_by, vec!["c-1"]);
        assert_eq!(
            item(&p, "c-3").blocked_by,
            vec!["c-1"],
            "behind the cycle, but its edge holds"
        );
        assert!(
            p.skipped.iter().any(|s| s.bead == "c-1"
                && s.what == "blocked-by c-2 dropped: part of a dependency cycle"),
            "{:?}",
            p.skipped
        );
    }

    #[test]
    fn a_self_dependency_is_a_one_bead_cycle() {
        let line = "{\"id\":\"s-1\",\"title\":\"loops\",\"issue_type\":\"task\",\"status\":\"open\",\"priority\":2,\"created_at\":\"t\",\"dependencies\":[{\"issue_id\":\"s-1\",\"depends_on_id\":\"s-1\",\"type\":\"blocks\"}]}\n";
        let e = beads::parse(line.as_bytes()).unwrap();
        let p = plan(&e);
        assert_eq!(p.cycles, vec![vec!["s-1".to_string()]]);
        assert!(item(&p, "s-1").blocked_by.is_empty());
        assert!(
            p.skipped
                .iter()
                .any(|s| s.what == "blocked-by s-1 dropped: part of a dependency cycle"),
            "{:?}",
            p.skipped
        );
        assert!(render(&p, "x", 5).contains("Dependency cycles (1)"));
    }

    #[test]
    fn a_defer_date_beats_in_progress_and_memories_ride_along() {
        let line = "{\"id\":\"d-1\",\"title\":\"later\",\"issue_type\":\"task\",\"status\":\"in_progress\",\"priority\":2,\"assignee\":\"dev1\",\"created_at\":\"t\",\"defer_until\":\"2027-01-01T00:00:00Z\"}\n\
                    {\"_type\":\"memory\",\"key\":\"k\",\"value\":\"one\"}\n{\"_type\":\"memory\",\"key\":\"k\",\"value\":\"two\"}\n";
        let e = beads::parse(line.as_bytes()).unwrap();
        let p = plan(&e);
        assert_eq!(item(&p, "d-1").status, project::STATUS_DEFERRED);
        assert_eq!(item(&p, "d-1").start_date.as_deref(), Some("2027-01-01"));
        assert_eq!(
            p.memories.len(),
            2,
            "both kept; the upsert makes the last one win"
        );
        assert!(
            p.skipped
                .iter()
                .any(|s| s.bead == "memory" && s.what.contains("appears more than once")),
            "{:?}",
            p.skipped
        );
        assert!(render(&p, "x", 5).contains("Memories (2), upserted by key: k k"));
    }

    #[test]
    fn ids_are_rewritten_only_when_known_and_not_in_the_footer() {
        let known: BTreeMap<String, u64> = [("wx-2".to_string(), 102), ("wx-1.1".to_string(), 103)]
            .into_iter()
            .collect();
        assert_eq!(rewrite_ids("see wx-2.", &known), "see #102.");
        assert_eq!(
            rewrite_ids("(wx-2) and wx-1.1, then wx-9", &known),
            "(#102) and #103, then wx-9"
        );
        assert_eq!(
            rewrite_ids("wx-20 is not wx-2", &known),
            "wx-20 is not #102"
        );
        assert_eq!(
            rewrite_ids("Imported from Beads `wx-2` (x).", &known),
            "Imported from Beads `wx-2` (x)."
        );
        assert_eq!(rewrite_ids("", &known), "");
        assert_eq!(
            rewrite_ids("see https://beads.example/wx-2/notes and wx-2", &known),
            "see https://beads.example/wx-2/notes and #102",
            "a URL keeps its path"
        );
        assert_eq!(
            rewrite_ids("(https://x.io/a?bead=wx-1.1) wx-1.1", &known),
            "(https://x.io/a?bead=wx-1.1) #103"
        );
        assert_eq!(
            rewrite_ids(
                "[wx-2](https://tracker/issues/wx-2) and [details](/issues/wx-2)",
                &known
            ),
            "[#102](https://tracker/issues/wx-2) and [details](/issues/wx-2)",
            "link text is rewritten, destinations never"
        );
        assert_eq!(
            rewrite_ids("<https://x/wx-2> wx-2", &known),
            "<https://x/wx-2> #102"
        );
        assert_eq!(
            rewrite_ids("[wx-2](broken wx-2", &known),
            "[#102](broken wx-2",
            "an unclosed destination copies through"
        );
        assert_eq!(
            rewrite_ids("run `bd show wx-2` then wx-2", &known),
            "run `bd show wx-2` then #102",
            "a code span is left alone"
        );
        assert_eq!(
            rewrite_ids("```\nbd dep add wx-1.1 wx-2\n```\nwx-2 done", &known),
            "```\nbd dep add wx-1.1 wx-2\n```\n#102 done",
            "a fenced block is left alone"
        );
        assert_eq!(
            rewrite_ids("``wx-2`` wx-2", &known),
            "``wx-2`` #102",
            "double-backtick span"
        );
        assert_eq!(
            rewrite_ids("``a ` b wx-2`` wx-2", &known),
            "``a ` b wx-2`` #102",
            "a span closes only on a run of the same length"
        );
        assert_eq!(
            rewrite_ids("````\n```\nwx-2\n````\nwx-2", &known),
            "````\n```\nwx-2\n````\n#102",
            "a fence closes only on a run at least as long"
        );
        assert_eq!(
            rewrite_ids("see\u{a0}wx-2.", &known),
            "see\u{a0}#102.",
            "multi-byte whitespace"
        );
        assert_eq!(
            rewrite_ids("~~~\nbd show wx-2\n~~~\nwx-2", &known),
            "~~~\nbd show wx-2\n~~~\n#102",
            "a tilde fence is left alone"
        );
        assert_eq!(
            rewrite_ids("a~b wx-2", &known),
            "a~b #102",
            "a lone tilde is prose"
        );
        assert_eq!(
            rewrite_ids("[doc]: /issues/wx-2\nsee [wx-2][doc] and wx-2", &known),
            "[doc]: /issues/wx-2\nsee [#102][doc] and #102",
            "a reference definition line is a destination"
        );
        assert_eq!(
            rewrite_ids("[doc]:/issues/wx-2\n\n[wx-2][doc]", &known),
            "[doc]:/issues/wx-2\n\n[#102][doc]",
            "the space after the colon is optional"
        );
        assert_eq!(
            rewrite_ids("[wx-2]:\n\nwx-2", &known),
            "[#102]:\n\n#102",
            "without a destination it is prose"
        );
        assert_eq!(
            rewrite_ids("[doc]:\n/issues/wx-2\n\n[wx-2][doc]", &known),
            "[doc]:\n/issues/wx-2\n\n[#102][doc]",
            "the destination may follow on the next line"
        );
        assert_eq!(
            rewrite_ids("```\necho '```' wx-2\n```\nwx-2", &known),
            "```\necho '```' wx-2\n```\n#102",
            "a run inside a fenced line does not close the fence"
        );
        assert_eq!(
            rewrite_ids("```\n``` not a close wx-2\n```\nwx-2", &known),
            "```\n``` not a close wx-2\n```\n#102",
            "a closing fence has nothing after it"
        );
        assert_eq!(
            rewrite_ids("```wx-2\nx\n```\nwx-2", &known),
            "```wx-2\nx\n```\n#102",
            "the info string is not prose"
        );
        assert_eq!(
            rewrite_ids("```wx-2``` wx-2", &known),
            "```wx-2``` #102",
            "a run of three in a line is a span, not a fence"
        );
        assert_eq!(
            rewrite_ids("https://tracker.example/archive(v1)/wx-2 wx-2", &known),
            "https://tracker.example/archive(v1)/wx-2 #102",
            "parentheses inside a bare URL are part of it"
        );
        assert_eq!(
            rewrite_ids("[doc](/archive(v1)/wx-2) wx-2", &known),
            "[doc](/archive(v1)/wx-2) #102",
            "parentheses inside a destination balance"
        );
        assert_eq!(
            rewrite_ids("[doc](/archive\\)/wx-2) wx-2", &known),
            "[doc](/archive\\)/wx-2) #102",
            "an escaped parenthesis does not close a destination"
        );
        assert_eq!(
            rewrite_ids("para\n\n    bd show wx-2\n    wx-1.1\n\nwx-2", &known),
            "para\n\n    bd show wx-2\n    wx-1.1\n\n#102",
            "an indented code block is left alone"
        );
        assert_eq!(
            rewrite_ids("\tbd show wx-2\nwx-2", &known),
            "\tbd show wx-2\n#102",
            "a tab indents; the block ends at a line indented less"
        );
        assert_eq!(
            rewrite_ids("para\n    wx-2", &known),
            "para\n    #102",
            "an indented line cannot interrupt a paragraph"
        );
        assert_eq!(
            rewrite_ids("```\n    wx-2\n```\n\n    wx-2\nwx-2", &known),
            "```\n    wx-2\n```\n\n    wx-2\n#102",
            "indentation inside a fence is the fence's"
        );
        assert_eq!(
            rewrite_ids("see wx-2\n[doc]: /issues/wx-2", &known),
            "see #102\n[doc]: /issues/wx-2",
            "a reference definition after the first line"
        );
        assert_eq!(
            rewrite_ids("Use \\` literally; see wx-2", &known),
            "Use \\` literally; see #102",
            "an escaped backtick is prose"
        );
        assert_eq!(
            rewrite_ids("a ` b wx-2", &known),
            "a ` b #102",
            "a backtick with no closer is prose"
        );
        assert_eq!(
            rewrite_ids("` a \\` wx-2 ` wx-2", &known),
            "` a \\` #102 ` #102",
            "inside a span a backslash is literal, so the span closes at the escaped backtick"
        );
        assert_eq!(
            rewrite_ids("<a href=\"/tracker/issues/wx-2\">wx-2</a>", &known),
            "<a href=\"/tracker/issues/wx-2\">#102</a>",
            "an HTML tag is copied through, its text is rewritten"
        );
        assert_eq!(
            rewrite_ids("<!-- note wx-2 --> wx-2", &known),
            "<!-- note wx-2 --> #102",
            "an HTML comment is left alone"
        );
        assert_eq!(
            rewrite_ids("a < b and wx-2 > c", &known),
            "a < b and #102 > c",
            "a lone less-than is prose"
        );
        assert_eq!(
            rewrite_ids("> ```\n> bd show wx-2\n> ```\n\nwx-2", &known),
            "> ```\n> bd show wx-2\n> ```\n\n#102",
            "a fence inside a block quote"
        );
        assert_eq!(
            rewrite_ids("> ```\n> wx-2\n\nwx-2", &known),
            "> ```\n> wx-2\n\n#102",
            "the quote ending ends its fence"
        );
        assert_eq!(
            rewrite_ids("> see wx-2", &known),
            "> see #102",
            "quoted prose is prose"
        );
        assert_eq!(
            rewrite_ids("> para\n>\n>     bd show wx-2\n\nwx-2", &known),
            "> para\n>\n>     bd show wx-2\n\n#102",
            "indented code inside a block quote"
        );
        assert_eq!(
            rewrite_ids("<code>bd show wx-2</code> wx-2", &known),
            "<code>bd show wx-2</code> #102",
            "a code element is code"
        );
        assert_eq!(
            rewrite_ids("<pre>\nwx-2\n</PRE>\nwx-2", &known),
            "<pre>\nwx-2\n</PRE>\n#102",
            "a pre element is code, whatever the case of its closing tag"
        );
        assert_eq!(
            rewrite_ids("<code/> wx-2 <b>wx-2</b>", &known),
            "<code/> #102 <b>#102</b>",
            "a self-closing code tag and other elements are prose"
        );
        assert_eq!(
            rewrite_ids("[note]: See wx-2 for details", &known),
            "[note]: See #102 for details",
            "words after the destination make it prose"
        );
        assert_eq!(
            rewrite_ids("[doc]: /issues/wx-2 \"about wx-2\"\nwx-2", &known),
            "[doc]: /issues/wx-2 \"about wx-2\"\n#102",
            "a title after the destination"
        );
        assert_eq!(
            rewrite_ids("[doc]: /issues/wx-2\n  'about wx-2'\nwx-2", &known),
            "[doc]: /issues/wx-2\n  'about wx-2'\n#102",
            "a title on the next line"
        );
        assert_eq!(
            rewrite_ids("[doc]: </a b/wx-2>\nwx-2", &known),
            "[doc]: </a b/wx-2>\n#102",
            "a pointy destination may hold spaces"
        );
        assert_eq!(
            rewrite_ids("[doc]: /issues/wx-2 wx-2", &known),
            "[doc]: /issues/#102 #102",
            "a second word that is not a title makes the whole line prose"
        );
        assert_eq!(
            rewrite_ids("> ```\n> > ```\n> wx-2\n> ```\nwx-2", &known),
            "> ```\n> > ```\n> wx-2\n> ```\n#102",
            "a deeper quote marker inside a quoted fence is code"
        );
        assert_eq!(
            rewrite_ids("[foo\\]]: /issues/wx-2\n\n[wx-2][foo\\]]", &known),
            "[foo\\]]: /issues/wx-2\n\n[#102][foo\\]]",
            "an escaped bracket inside a label"
        );
        assert_eq!(
            rewrite_ids("<code>a </code-example> wx-2</code > wx-2", &known),
            "<code>a </code-example> wx-2</code > #102",
            "only the exact closing tag ends a code element"
        );
        assert_eq!(
            rewrite_ids("> [doc]: /issues/wx-2\n>\n> [wx-2][doc]", &known),
            "> [doc]: /issues/wx-2\n>\n> [#102][doc]",
            "a reference definition inside a block quote"
        );
        assert_eq!(rewrite_ids("no ids here.", &known), "no ids here.");
    }

    #[test]
    fn assignee_flags_map_names_to_logins() {
        let flags = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let map = assignee_map(&flags(&["  Pat   Example =patexample", "Bot="])).unwrap();
        assert_eq!(
            map["pat example"].as_deref(),
            Some("patexample"),
            "the name is matched case and whitespace aside"
        );
        assert_eq!(map["bot"], None, "an empty login drops the assignee");
        let split = assignee_map(&flags(&["Team = Backend=backend-user", "A=B="])).unwrap();
        assert_eq!(
            split["team = backend"].as_deref(),
            Some("backend-user"),
            "the split is at the last equals sign"
        );
        assert_eq!(split["a=b"], None);
        assert!(assignee_map(&flags(&["no-equals"])).is_err());
        assert!(assignee_map(&flags(&["=x"])).is_err());
        assert!(
            assignee_map(&flags(&["A=not a login"])).is_err(),
            "the login side must be a login"
        );
        assert!(is_login("patexample") && is_login("a-b1"));
        assert!(!is_login("Pat Example") && !is_login("-x") && !is_login("a--b") && !is_login(""));

        let export = crate::beads::parse(
            r#"{"_type":"issue","id":"n-1","title":"a","issue_type":"task","status":"open","priority":2,"assignee":"Pat Example","created_at":"2026-04-01T09:00:00Z"}
{"_type":"issue","id":"n-2","title":"b","issue_type":"task","status":"open","priority":2,"assignee":"pat example","created_at":"2026-04-01T09:00:00Z"}
{"_type":"issue","id":"n-3","title":"c","issue_type":"task","status":"open","priority":2,"assignee":"dev1","created_at":"2026-04-01T09:00:00Z"}
{"_type":"issue","id":"n-4","title":"d","issue_type":"task","status":"open","priority":2,"assignee":" Pat  Example ","created_at":"2026-04-01T09:00:00Z"}
{"_type":"issue","id":"n-5","title":"e","issue_type":"task","status":"open","priority":2,"assignee":" dev2 ","created_at":"2026-04-01T09:00:00Z"}
{"_type":"issue","id":"n-6","title":"f","issue_type":"task","status":"open","priority":2,"assignee":"   ","created_at":"2026-04-01T09:00:00Z"}
"#
            .as_bytes(),
        )
        .unwrap();
        let mut p = plan(&export);
        assert_eq!(
            assignee_summary(&p),
            vec![
                ("Pat Example".to_string(), 3, false),
                ("dev1".to_string(), 1, true),
                ("dev2".to_string(), 1, true)
            ],
            "spelling variants of one name count together; a padded login is shown trimmed; a blank one is nothing"
        );
        assert!(render(&p, "x", 5).contains(
            "Assignees:  Pat Example (3) — not a GitHub login; pass --assignee 'Pat Example=LOGIN'"
        ));
        map_assignees(&mut p, &map);
        let logins: Vec<Option<&str>> = p.items.iter().map(|i| i.assignee.as_deref()).collect();
        assert_eq!(
            logins,
            vec![
                Some("patexample"),
                Some("patexample"),
                Some("dev1"),
                Some("patexample"),
                Some("dev2"),
                None
            ],
            "every variant maps; a login stays, trimmed; a blank one is dropped"
        );
        assert!(render(&p, "x", 5).contains("Assignees:  patexample (3), dev1 (1), dev2 (1)"));
    }

    #[test]
    fn the_mapping_file_appends_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("map.jsonl");
        assert!(read_mapping(&path).unwrap().is_empty(), "absent is empty");
        let rec = |bead: &str, number: u64, phase: Phase| Mapped {
            bead: bead.into(),
            number,
            url: format!("https://github.com/acme/widgets/issues/{number}"),
            phase,
            comments: 0,
            rewritten: false,
        };
        let mut m = Mapping::open(&path).unwrap();
        m.record(&rec("a-1", 7, Phase::Created)).unwrap();
        m.record(&rec("a-1", 7, Phase::Done)).unwrap();
        m.record(&rec("a-2", 8, Phase::Created)).unwrap();
        drop(m);
        let mut again = Mapping::open(&path).unwrap();
        again.record(&rec("a-3", 9, Phase::Done)).unwrap();
        let map = read_mapping(&path).unwrap();
        assert_eq!(map.len(), 3);
        assert_eq!(map["a-1"].phase, Phase::Done, "last line wins");
        assert_eq!(map["a-2"].phase, Phase::Created);
        assert_eq!(map["a-3"].number, 9);
        assert_eq!(map["a-3"].repo(), Some("acme/widgets"));
        assert!(foreign_entry(&map, "acme/widgets").is_none());
        assert_eq!(
            foreign_entry(&map, "acme/other").map(|m| m.bead.as_str()),
            Some("a-1")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 4);
        // A line without a phase (added by hand) counts as done.
        std::fs::write(&path, "{\"bead\":\"h-1\",\"number\":3,\"url\":\"https://github.com/acme/widgets/issues/3\"}\n").unwrap();
        assert_eq!(read_mapping(&path).unwrap()["h-1"].phase, Phase::Done);
        std::fs::write(&path, "{\"bead\":1}\n").unwrap();
        let err = read_mapping(&path).unwrap_err();
        assert!(
            format!("{err:#}").contains(":1: not a mapping line"),
            "{err:#}"
        );
    }

    #[test]
    fn the_mapping_file_is_locked_while_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("map.jsonl");
        let held = Mapping::open(&path).unwrap();
        let err = Mapping::open(&path).unwrap_err();
        assert!(
            format!("{err:#}").contains("is in use by another gbd import"),
            "{err:#}"
        );
        drop(held);
        Mapping::open(&path).unwrap();
    }

    #[test]
    fn a_cut_off_last_line_is_ignored_and_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("map.jsonl");
        let good = r#"{"bead":"a-1","number":7,"url":"https://github.com/acme/widgets/issues/7","phase":"done"}"#;
        std::fs::write(&path, format!("{good}\n{{\"bead\":\"a-2\",\"num")).unwrap();
        let map = read_mapping(&path).unwrap();
        assert_eq!(map.len(), 1, "the torn tail records nothing");
        let mut m = Mapping::open(&path).unwrap();
        m.record(&Mapped {
            bead: "a-2".into(),
            number: 8,
            url: "https://github.com/acme/widgets/issues/8".into(),
            phase: Phase::Created,
            comments: 0,
            rewritten: false,
        })
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.lines().count(),
            2,
            "the tail was dropped before appending: {text}"
        );
        assert_eq!(read_mapping(&path).unwrap().len(), 2);
        drop(m);
        // A whole last record that only lacks its newline is kept.
        std::fs::write(&path, good).unwrap();
        Mapping::open(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{good}\n"));
        // A malformed line in the middle is still an error.
        std::fs::write(&path, format!("{{\"bead\":1}}\n{good}\n")).unwrap();
        assert!(read_mapping(&path).is_err());
    }

    #[test]
    fn comment_markers_say_what_github_already_has() {
        assert_eq!(comment_marker("wx-1.1", 2), "<!-- gbd-import wx-1.1/2 -->");
        let bodies = [
            "first\n\n<!-- gbd-import wx-1.1/1 -->",
            "unrelated <!-- gbd-import wx-9/4 -->",
            "second\n\n<!-- gbd-import wx-1.1/2 -->",
            "a human comment",
        ];
        assert_eq!(posted_comments(&bodies, "wx-1.1"), 2);
        assert_eq!(posted_comments(&bodies, "wx-9"), 4);
        assert_eq!(posted_comments(&bodies, "wx-2"), 0);
        assert_eq!(posted_comments(&[], "wx-1.1"), 0);
    }

    #[test]
    fn close_reasons_are_guessed_from_words() {
        assert_eq!(CloseReason::guess(None), CloseReason::Completed);
        assert_eq!(
            CloseReason::guess(Some("shipped in 1.4")),
            CloseReason::Completed
        );
        assert_eq!(
            CloseReason::guess(Some("Duplicate of wx-2")),
            CloseReason::Duplicate
        );
        assert_eq!(
            CloseReason::guess(Some("won't fix, out of scope")),
            CloseReason::NotPlanned
        );
        assert_eq!(
            CloseReason::guess(Some("Superseded by the v2 plan")),
            CloseReason::NotPlanned
        );
        assert_eq!(CloseReason::NotPlanned.as_flag(), "not planned");
    }

    #[test]
    fn the_report_reads_top_down() {
        let p = fixture();
        let text = render(&p, "beads.jsonl", 3);
        assert!(
            text.starts_with("Import plan: 8 issues, 1 memories, from beads.jsonl\n"),
            "{text}"
        );
        assert!(
            text.contains("By type:    Bug 1, Chore 1, Epic 1, Feature 1, Task 4\n"),
            "{text}"
        );
        assert!(
            text.contains("Board:      Blocked 1, Deferred 1, Done 1, In Progress 1, Ready 4\n"),
            "{text}"
        );
        assert!(
            text.contains("State:      closed (completed) 1, open 7\n"),
            "{text}"
        );
        assert!(
            text.contains("Edges:      2 parent links, 1 blocked-by\n"),
            "{text}"
        );
        assert!(
            text.contains("Order (3 of 8):\n    1. wx-4  [Chore] P4 Bump CI runners\n"),
            "{text}"
        );
        assert!(
            text.contains("       … 5 more (--json for all)\n"),
            "{text}"
        );
        assert!(text.contains("Cannot map (4):\n"), "{text}");
        assert!(text.contains("Export problems (6):\n"), "{text}");
        assert!(text.ends_with("Nothing written (--dry-run).\n"), "{text}");
        assert!(
            text.contains("← parent wx-1, blocked by wx-2")
                || render(&p, "x", 8).contains("← parent wx-1, blocked by wx-2")
        );
    }
}
