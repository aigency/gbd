//! The JSONL that `bd export --include-memories` writes, as a typed model.
//! Read once by `gbd import`; nothing here talks to GitHub.
//!
//! Each line is one record with a `_type`: `issue` (labels, dependencies,
//! and comments inlined) or `memory` (`key` / `value`). Other kinds
//! (`--all` adds agents, gates, templates) are skipped and reported.
//!
//! Dependency edges hang off the dependent side: for `blocks`, `issue_id`
//! is blocked by `depends_on_id`; for `parent-child`, `issue_id` is the
//! child. Every other kind (`related`, `discovered-from`, `supersedes`, …)
//! has no GitHub primitive and is kept only for the dry-run report.

use std::collections::BTreeSet;
use std::fmt;
use std::io::BufRead;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Beads `issue_type`. Anything Beads may grow that gbd does not know is
/// carried as `Other`, so the dry run can name it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub enum Kind {
    Bug,
    Task,
    Epic,
    Feature,
    Chore,
    Decision,
    Other(String),
}

impl From<String> for Kind {
    fn from(s: String) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "bug" => Kind::Bug,
            "task" => Kind::Task,
            "epic" => Kind::Epic,
            "feature" => Kind::Feature,
            "chore" => Kind::Chore,
            "decision" => Kind::Decision,
            _ => Kind::Other(s),
        }
    }
}

impl Kind {
    pub fn as_str(&self) -> &str {
        match self {
            Kind::Bug => "bug",
            Kind::Task => "task",
            Kind::Epic => "epic",
            Kind::Feature => "feature",
            Kind::Chore => "chore",
            Kind::Decision => "decision",
            Kind::Other(s) => s,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Beads `status` as stored. `Blocked` is a stored value in Beads, not a
/// derived one; the importer recomputes blocked-ness from the edges.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub enum Status {
    Open,
    InProgress,
    Blocked,
    Deferred,
    Closed,
    Other(String),
}

impl From<String> for Status {
    fn from(s: String) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "open" => Status::Open,
            "in_progress" => Status::InProgress,
            "blocked" => Status::Blocked,
            "deferred" => Status::Deferred,
            "closed" => Status::Closed,
            _ => Status::Other(s),
        }
    }
}

impl Status {
    pub fn as_str(&self) -> &str {
        match self {
            Status::Open => "open",
            Status::InProgress => "in_progress",
            Status::Blocked => "blocked",
            Status::Deferred => "deferred",
            Status::Closed => "closed",
            Status::Other(s) => s,
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A dependency edge gbd cannot turn into a GitHub relation. Kept so the
/// dry run can list what is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// Beads dependency type, e.g. `related`, `discovered-from`.
    pub kind: String,
    /// The record the edge was found on.
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Comment {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub author: String,
    pub text: String,
    #[serde(default)]
    pub created_at: String,
}

/// One Beads issue with its edges resolved to ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bead {
    pub id: String,
    pub title: String,
    pub description: String,
    pub kind: Kind,
    pub status: Status,
    /// 0 (highest) … 4. A value outside that range is reported and
    /// clamped to the nearest end.
    pub priority: u8,
    pub assignee: Option<String>,
    pub owner: Option<String>,
    pub created_by: Option<String>,
    /// RFC 3339, as Beads writes them (`2026-09-12T08:19:26Z`).
    pub created_at: String,
    pub updated_at: Option<String>,
    pub closed_at: Option<String>,
    pub close_reason: Option<String>,
    pub started_at: Option<String>,
    pub defer_until: Option<String>,
    pub due_at: Option<String>,
    pub notes: Option<String>,
    pub design: Option<String>,
    pub acceptance_criteria: Option<String>,
    pub external_ref: Option<String>,
    pub estimated_minutes: Option<u64>,
    pub labels: Vec<String>,
    /// Ids this bead is blocked by (`blocks` edges), in file order.
    pub blocked_by: Vec<String>,
    /// Parent id from the first `parent-child` edge.
    pub parent: Option<String>,
    /// Edges of every other kind, for the report.
    pub other_deps: Vec<Edge>,
    pub comments: Vec<Comment>,
}

/// A `bd remember` line.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Memory {
    pub key: String,
    pub value: String,
}

/// Something the importer reports but does not stop on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// 1-based line in the export.
    pub line: usize,
    pub id: Option<String>,
    pub what: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.id {
            Some(id) => write!(f, "line {}: {id}: {}", self.line, self.what),
            None => write!(f, "line {}: {}", self.line, self.what),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Export {
    pub issues: Vec<Bead>,
    pub memories: Vec<Memory>,
    pub problems: Vec<Problem>,
}

impl Export {
    pub fn get(&self, id: &str) -> Option<&Bead> {
        self.issues.iter().find(|b| b.id == id)
    }

    /// Every issue id in the export.
    pub fn ids(&self) -> BTreeSet<&str> {
        self.issues.iter().map(|b| b.id.as_str()).collect()
    }
}

// ---------------------------------------------------------------------------
// Wire shapes

#[derive(Deserialize)]
struct RawDep {
    issue_id: String,
    depends_on_id: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct RawIssue {
    id: String,
    title: String,
    #[serde(default)]
    description: String,
    issue_type: Kind,
    status: Status,
    /// Wide on purpose: an out-of-range value is a problem to report,
    /// not a reason to reject the record.
    priority: i64,
    assignee: Option<String>,
    owner: Option<String>,
    created_by: Option<String>,
    created_at: String,
    updated_at: Option<String>,
    closed_at: Option<String>,
    close_reason: Option<String>,
    started_at: Option<String>,
    defer_until: Option<String>,
    due_at: Option<String>,
    notes: Option<String>,
    design: Option<String>,
    acceptance_criteria: Option<String>,
    external_ref: Option<String>,
    estimated_minutes: Option<u64>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    dependencies: Vec<RawDep>,
    #[serde(default)]
    comments: Vec<Comment>,
}

#[derive(Deserialize)]
struct Header {
    #[serde(rename = "_type")]
    kind: Option<String>,
}

pub fn load(path: &Path) -> Result<Export> {
    let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    parse(std::io::BufReader::new(file)).with_context(|| format!("reading {}", path.display()))
}

/// Parse the whole export. Malformed JSON or a missing required field is
/// an error naming the line; everything softer lands in `problems`.
pub fn parse(reader: impl BufRead) -> Result<Export> {
    let mut export = Export::default();
    let mut raw: Vec<(usize, RawIssue)> = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let n = idx + 1;
        let line = line.with_context(|| format!("line {n}: read error"))?;
        if line.trim().is_empty() {
            continue;
        }
        let header: Header =
            serde_json::from_str(&line).with_context(|| format!("line {n}: not JSON"))?;
        match header.kind.as_deref() {
            // Older exports carry no `_type`; every such line is an issue.
            Some("issue") | None => {
                let issue: RawIssue = serde_json::from_str(&line)
                    .with_context(|| format!("line {n}: malformed issue record"))?;
                raw.push((n, issue));
            }
            Some("memory") => {
                let m: Memory = serde_json::from_str(&line)
                    .with_context(|| format!("line {n}: malformed memory record"))?;
                export.memories.push(m);
            }
            Some(other) => export.problems.push(Problem {
                line: n,
                id: None,
                what: format!("skipped a {other} record; gbd imports issues and memories only"),
            }),
        }
    }
    resolve(raw, &mut export);
    Ok(export)
}

/// Second pass, with every id known: dedupe, direct the edges, and note
/// what points outside the export.
fn resolve(raw: Vec<(usize, RawIssue)>, export: &mut Export) {
    let ids: BTreeSet<String> = raw.iter().map(|(_, r)| r.id.clone()).collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (line, r) in raw {
        let mut note = |what: String| {
            export.problems.push(Problem {
                line,
                id: Some(r.id.clone()),
                what,
            });
        };
        if !seen.insert(r.id.clone()) {
            note("duplicate id; later record skipped".into());
            continue;
        }
        if let Kind::Other(k) = &r.issue_type {
            note(format!("unknown issue type {k:?}"));
        }
        if let Status::Other(s) = &r.status {
            note(format!("unknown status {s:?}"));
        }
        let priority = match u8::try_from(r.priority) {
            Ok(p) if p <= 4 => p,
            _ => {
                let clamped = if r.priority < 0 { 0 } else { 4 };
                note(format!(
                    "priority {} is outside 0–4; using {clamped}",
                    r.priority
                ));
                clamped
            }
        };
        let mut blocked_by = Vec::new();
        let mut parent = None;
        let mut other_deps = Vec::new();
        for d in &r.dependencies {
            if d.issue_id != r.id {
                note(format!(
                    "dependency edge belongs to {}, not this record; ignored",
                    d.issue_id
                ));
                continue;
            }
            let target = d.depends_on_id.clone();
            if !ids.contains(&target) {
                note(format!(
                    "{} edge to {target}, which is not in the export",
                    d.kind
                ));
            }
            match d.kind.as_str() {
                "blocks" => blocked_by.push(target),
                "parent-child" => {
                    if parent.is_some() {
                        note(format!("second parent {target} ignored"));
                    } else {
                        parent = Some(target);
                    }
                }
                _ => other_deps.push(Edge {
                    kind: d.kind.clone(),
                    from: r.id.clone(),
                    to: target,
                }),
            }
        }
        export.issues.push(Bead {
            id: r.id,
            title: r.title,
            description: r.description,
            kind: r.issue_type,
            status: r.status,
            priority,
            assignee: r.assignee,
            owner: r.owner,
            created_by: r.created_by,
            created_at: r.created_at,
            updated_at: r.updated_at,
            closed_at: r.closed_at,
            close_reason: r.close_reason,
            started_at: r.started_at,
            defer_until: r.defer_until,
            due_at: r.due_at,
            notes: r.notes,
            design: r.design,
            acceptance_criteria: r.acceptance_criteria,
            external_ref: r.external_ref,
            estimated_minutes: r.estimated_minutes,
            labels: r.labels,
            blocked_by,
            parent,
            other_deps,
            comments: r.comments,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shapes copied from a real `bd export --include-memories`; every
    /// string is made up.
    const FIXTURE: &str = include_str!("../tests/fixtures/beads-export.jsonl");

    fn fixture() -> Export {
        parse(FIXTURE.as_bytes()).unwrap()
    }

    #[test]
    fn counts_and_record_kinds() {
        let e = fixture();
        assert_eq!(e.issues.len(), 8, "{:?}", e.ids());
        assert_eq!(e.memories.len(), 1);
        assert_eq!(e.memories[0].key, "deploy-runbook");
        assert!(e.memories[0].value.starts_with("Deploy with"));
    }

    #[test]
    fn fields_come_through_typed() {
        let e = fixture();
        let epic = e.get("wx-1").unwrap();
        assert_eq!(epic.kind, Kind::Epic);
        assert_eq!(epic.status, Status::Open);
        assert_eq!(epic.priority, 1);
        assert_eq!(epic.labels, vec!["area:api"]);
        assert_eq!(epic.comments.len(), 1);
        assert_eq!(epic.comments[0].author, "dev1");
        assert_eq!(epic.created_at, "2026-03-01T09:00:00Z");
        assert!(epic.description.contains("wx-2"));

        let bug = e.get("wx-2").unwrap();
        assert_eq!(bug.kind, Kind::Bug);
        assert_eq!(bug.status, Status::InProgress);
        assert_eq!(bug.priority, 0);
        assert_eq!(bug.assignee.as_deref(), Some("dev1"));
        assert!(bug.started_at.is_some());

        let deferred = e.get("wx-3").unwrap();
        assert_eq!(deferred.status, Status::Deferred);
        assert_eq!(
            deferred.defer_until.as_deref(),
            Some("2026-10-01T00:00:00Z")
        );

        let closed = e.get("wx-4").unwrap();
        assert_eq!(closed.kind, Kind::Chore);
        assert_eq!(closed.status, Status::Closed);
        assert_eq!(closed.close_reason.as_deref(), Some("shipped in 1.4"));
        assert!(closed.closed_at.is_some());
        assert_eq!(closed.estimated_minutes, Some(30));
    }

    #[test]
    fn edges_are_directed_from_the_dependent_side() {
        let e = fixture();
        let child = e.get("wx-1.1").unwrap();
        assert_eq!(child.parent.as_deref(), Some("wx-1"));
        assert_eq!(child.blocked_by, vec!["wx-2"]);
        assert!(child.other_deps.is_empty());

        let decision = e.get("wx-5").unwrap();
        assert_eq!(decision.kind, Kind::Decision);
        assert_eq!(decision.status, Status::Blocked);
        assert_eq!(decision.blocked_by, vec!["wx-9"], "dangling target is kept");
        assert_eq!(
            decision.other_deps,
            vec![Edge {
                kind: "related".into(),
                from: "wx-5".into(),
                to: "wx-1".into()
            }]
        );
    }

    #[test]
    fn soft_problems_are_reported_not_fatal() {
        let e = fixture();
        let what: Vec<String> = e.problems.iter().map(ToString::to_string).collect();
        let has = |s: &str| what.iter().any(|w| w.contains(s));
        assert!(has("line 9: skipped a agent record"), "{what:?}");
        assert!(
            has("wx-5: blocks edge to wx-9, which is not in the export"),
            "{what:?}"
        );
        assert!(has("wx-6: second parent wx-3 ignored"), "{what:?}");
        assert!(
            has("wx-6: dependency edge belongs to wx-2, not this record"),
            "{what:?}"
        );
        assert!(has("wx-7: unknown issue type \"wisp\""), "{what:?}");
        assert!(has("wx-7: unknown status \"someday\""), "{what:?}");
        assert_eq!(e.get("wx-6").unwrap().parent.as_deref(), Some("wx-1"));
        assert_eq!(e.get("wx-7").unwrap().kind, Kind::Other("wisp".into()));
        assert_eq!(e.problems.len(), 6, "{what:?}");
    }

    #[test]
    fn malformed_input_names_the_line() {
        let bad = "{\"_type\":\"issue\",\"id\":\"a-1\",\"title\":\"ok\",\"issue_type\":\"task\",\"status\":\"open\",\"priority\":2,\"created_at\":\"t\"}\n{not json\n";
        let err = parse(bad.as_bytes()).unwrap_err();
        assert!(format!("{err:#}").contains("line 2: not JSON"), "{err:#}");

        let missing = "\n{\"_type\":\"issue\",\"id\":\"a-1\",\"issue_type\":\"task\",\"status\":\"open\",\"priority\":2,\"created_at\":\"t\"}\n";
        let err = parse(missing.as_bytes()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("line 2: malformed issue record"), "{msg}");
        assert!(msg.contains("title"), "{msg}");

        let mem = "{\"_type\":\"memory\",\"key\":\"k\"}\n";
        let err = parse(mem.as_bytes()).unwrap_err();
        assert!(format!("{err:#}").contains("line 1: malformed memory record"));
    }

    #[test]
    fn a_line_without_type_is_an_issue_and_duplicates_are_skipped() {
        let two = "{\"id\":\"a-1\",\"title\":\"one\",\"issue_type\":\"task\",\"status\":\"open\",\"priority\":2,\"created_at\":\"t\"}\n\
                   {\"id\":\"a-1\",\"title\":\"again\",\"issue_type\":\"task\",\"status\":\"open\",\"priority\":9,\"created_at\":\"t\"}\n";
        let e = parse(two.as_bytes()).unwrap();
        assert_eq!(e.issues.len(), 1);
        assert_eq!(e.issues[0].title, "one");
        assert_eq!(e.problems.len(), 1);
        assert!(e.problems[0].to_string().contains("duplicate id"));
    }

    #[test]
    fn priorities_outside_0_to_4_are_clamped_and_reported() {
        let lines = "{\"id\":\"a-1\",\"title\":\"low\",\"issue_type\":\"task\",\"status\":\"open\",\"priority\":900,\"created_at\":\"t\"}\n\
                     {\"id\":\"a-2\",\"title\":\"high\",\"issue_type\":\"task\",\"status\":\"open\",\"priority\":-3,\"created_at\":\"t\"}\n";
        let e = parse(lines.as_bytes()).unwrap();
        assert_eq!(e.get("a-1").unwrap().priority, 4);
        assert_eq!(e.get("a-2").unwrap().priority, 0);
        let what: Vec<String> = e.problems.iter().map(ToString::to_string).collect();
        assert_eq!(what.len(), 2, "{what:?}");
        assert!(
            what[0].contains("a-1: priority 900 is outside 0–4; using 4"),
            "{what:?}"
        );
        assert!(
            what[1].contains("a-2: priority -3 is outside 0–4; using 0"),
            "{what:?}"
        );
    }

    #[test]
    fn kinds_and_statuses_round_trip_and_ignore_case() {
        assert_eq!(Kind::from("Feature".to_string()), Kind::Feature);
        assert_eq!(Kind::Feature.to_string(), "feature");
        assert_eq!(Kind::from("wisp".to_string()).as_str(), "wisp");
        assert_eq!(Status::from("IN_PROGRESS".to_string()), Status::InProgress);
        assert_eq!(Status::InProgress.to_string(), "in_progress");
    }
}
