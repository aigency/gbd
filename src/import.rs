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

use serde::Serialize;

use crate::beads::{Bead, Export, Kind, Status};
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
    pub memories: usize,
    /// Groups of beads whose edges form a cycle. They are still created,
    /// at the end, with the edges that close the cycle dropped.
    pub cycles: Vec<Vec<String>>,
    pub skipped: Vec<Skip>,
    /// Parse-time problems, rendered.
    pub problems: Vec<String>,
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
    let extras = [
        ("Owner", b.owner.clone()),
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
        Status::InProgress => project::STATUS_IN_PROGRESS,
        Status::Deferred => project::STATUS_DEFERRED,
        _ if b.defer_until.is_some() => project::STATUS_DEFERRED,
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
    Plan {
        items,
        memories: export.memories.len(),
        cycles: cycle_groups,
        skipped,
        problems: export.problems.iter().map(ToString::to_string).collect(),
    }
}

// ---------------------------------------------------------------------------
// Report

/// The dry-run text: counts, the order, and everything dropped.
pub fn render(p: &Plan, source: &str, order_lines: usize) -> String {
    let mut out = format!(
        "Import plan: {} issues, {} memories, from {source}\n\n",
        p.items.len(),
        p.memories
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
    if !p.problems.is_empty() {
        let _ = writeln!(out, "\nExport problems ({}):", p.problems.len());
        for m in &p.problems {
            let _ = writeln!(out, "  {m}");
        }
    }
    out.push_str("\nNothing written (--dry-run).\n");
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
        assert_eq!(p.memories, 1);
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
                "---\nImported from Beads `wx-1` (created 2026-03-01 by dev1). Beads labels: area:api. Owner: dev1."
            ),
            "{}",
            epic.body
        );
        assert!(
            item(&p, "wx-3")
                .body
                .ends_with("(created 2026-03-03 by dev2)."),
            "nothing extra, no tail"
        );
        assert!(
            item(&p, "wx-4")
                .body
                .ends_with("(created 2026-02-20 by dev2). Owner: dev2. Estimate: 30 min."),
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
