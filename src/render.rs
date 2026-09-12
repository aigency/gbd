//! Human output. Rows look like Beads: `○ #12 ● P1 [Task] Title`.
//! Agents use `--json`; nothing here is parsed back.

use std::io::IsTerminal;

use crate::issue::{Detail, Issue, State};
use std::fmt::Write as _;

/// Status glyph, in the order an agent should care: closed, blocked,
/// deferred, in progress, then plain open.
pub fn glyph(i: &Issue) -> &'static str {
    if i.state == State::Closed {
        "✓"
    } else if i.directly_blocked() {
        "⊘"
    } else if i.is_deferred() {
        "⏸"
    } else if i.in_progress() {
        "◐"
    } else {
        "○"
    }
}

fn color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

fn priority_dot(rank: u8) -> String {
    if !color_enabled() {
        return format!("● P{rank}");
    }
    let code = match rank {
        0 => "31", // red
        1 => "33", // yellow
        2 => "0",  // default
        3 => "36", // cyan
        _ => "2",  // dim
    };
    format!("\x1b[{code}m●\x1b[0m P{rank}")
}

fn type_tag(i: &Issue) -> String {
    match i.issue_type.as_deref() {
        Some(t) if !t.eq_ignore_ascii_case("Task") => format!("[{}] ", t.to_lowercase()),
        _ => String::new(),
    }
}

/// `○ #12 ● P1 [bug] Title  @login`
pub fn line(i: &Issue) -> String {
    let who = if i.assignees.is_empty() {
        String::new()
    } else {
        format!("  @{}", i.assignees.join(" @"))
    };
    let status = match &i.status {
        Some(s) if !s.eq_ignore_ascii_case("Ready") => format!("  [{s}]"),
        _ => String::new(),
    };
    format!(
        "{} #{} {} {}{}{}{}",
        glyph(i),
        i.number,
        priority_dot(i.priority),
        type_tag(i),
        i.title,
        who,
        status
    )
}

/// Nest children under parents that are in the set; sort each level by
/// priority then number. Issues whose parent is absent are roots.
pub fn tree(issues: &[Issue]) -> String {
    use std::collections::{BTreeMap, HashSet};
    fn walk(
        i: &Issue,
        prefix: &str,
        last: bool,
        depth: usize,
        children: &BTreeMap<u64, Vec<&Issue>>,
        out: &mut String,
    ) {
        if depth == 0 {
            out.push_str(&line(i));
        } else {
            out.push_str(prefix);
            out.push_str(if last { "└── " } else { "├── " });
            out.push_str(&line(i));
        }
        out.push('\n');
        if let Some(kids) = children.get(&i.number) {
            let child_prefix = if depth == 0 {
                String::new()
            } else {
                format!("{prefix}{}", if last { "    " } else { "│   " })
            };
            let n = kids.len();
            for (k, kid) in kids.iter().enumerate() {
                walk(kid, &child_prefix, k + 1 == n, depth + 1, children, out);
            }
        }
    }
    let present: HashSet<u64> = issues.iter().map(|i| i.number).collect();
    let mut children: BTreeMap<u64, Vec<&Issue>> = BTreeMap::new();
    let mut roots: Vec<&Issue> = Vec::new();
    for i in issues {
        match i.parent {
            Some(p) if present.contains(&p) => children.entry(p).or_default().push(i),
            _ => roots.push(i),
        }
    }
    let by_rank =
        |a: &&Issue, b: &&Issue| a.priority.cmp(&b.priority).then(a.number.cmp(&b.number));
    roots.sort_by(by_rank);
    for kids in children.values_mut() {
        kids.sort_by(by_rank);
    }
    let mut out = String::new();
    for root in roots {
        walk(root, "", true, 0, &children, &mut out);
    }
    out
}

fn date_only(ts: &str) -> &str {
    ts.split('T').next().unwrap_or(ts)
}

fn section(out: &mut String, title: &str, rows: &[Issue]) {
    if rows.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n{title} ({})", rows.len());
    for r in rows {
        out.push_str("  ");
        out.push_str(&line(r));
        out.push('\n');
    }
}

/// Beads-style detail view.
pub fn show(d: &Detail) -> String {
    let i = &d.issue;
    let mut out = String::new();
    let ty = i
        .issue_type
        .as_deref()
        .map(|t| format!(" [{}]", t.to_uppercase()))
        .unwrap_or_default();
    let state = match (i.state, state_reason(d)) {
        (State::Closed, Some(r)) => format!("CLOSED · {r}"),
        (State::Closed, None) => "CLOSED".into(),
        (State::Open, _) if i.directly_blocked() => "OPEN · blocked".into(),
        (State::Open, _) if i.in_progress() => "OPEN · in progress".into(),
        (State::Open, _) if i.is_deferred() => "OPEN · deferred".into(),
        (State::Open, _) => "OPEN".into(),
    };
    let _ = writeln!(
        out,
        "{} #{}{ty} · {}   [{} · {state}]",
        glyph(i),
        i.number,
        i.title,
        priority_dot(i.priority)
    );
    let mut meta: Vec<String> = Vec::new();
    if let Some(a) = &d.author {
        meta.push(format!("Author: {a}"));
    }
    if !i.assignees.is_empty() {
        meta.push(format!("Assignee: {}", i.assignees.join(", ")));
    }
    if let Some(t) = &i.issue_type {
        meta.push(format!("Type: {t}"));
    }
    if let Some(s) = &i.status {
        meta.push(format!("Status: {s}"));
    }
    if let Some(sd) = &i.start_date {
        meta.push(format!("Start date: {sd}"));
    }
    out.push_str(&meta.join(" · "));
    out.push('\n');
    let _ = write!(
        out,
        "Created: {} · Updated: {}",
        date_only(&d.created_at),
        date_only(&d.updated_at)
    );
    if let Some(c) = &d.closed_at {
        let _ = write!(out, " · Closed: {}", date_only(c));
    }
    out.push('\n');
    out.push_str(&i.url);
    out.push('\n');
    if !d.body.trim().is_empty() {
        out.push_str("\nDESCRIPTION\n\n");
        for l in d.body.lines() {
            out.push_str("  ");
            out.push_str(l);
            out.push('\n');
        }
    }
    if let Some(p) = &d.parent_issue {
        out.push_str("\nPARENT\n  ");
        out.push_str(&line(p));
        out.push('\n');
    }
    section(&mut out, "BLOCKED BY", &d.blocked_by);
    section(&mut out, "BLOCKING", &d.blocking);
    section(&mut out, "CHILDREN", &d.children);
    if d.comments > 0 {
        let _ = write!(
            out,
            "\n{} comment{}: gbd comments {}\n",
            d.comments,
            if d.comments == 1 { "" } else { "s" },
            i.number
        );
    }
    out
}

fn state_reason(d: &Detail) -> Option<&str> {
    d.state_reason.as_deref().map(|r| match r {
        "COMPLETED" => "completed",
        "NOT_PLANNED" => "not planned",
        "DUPLICATE" => "duplicate",
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(n: u64, title: &str, priority: u8) -> Issue {
        Issue {
            id: format!("I{n}"),
            number: n,
            title: title.into(),
            url: format!("https://example/{n}"),
            state: State::Open,
            issue_type: Some("Task".into()),
            assignees: vec![],
            parent: None,
            open_blockers: 0,
            blocked_by_open: vec![],
            blocking_open: vec![],
            priority,
            start_date: None,
            deferred: false,
            memory: false,
            status: None,
        }
    }

    #[test]
    fn line_looks_like_beads() {
        let mut i = issue(12, "Fix auth", 1);
        i.issue_type = Some("Bug".into());
        assert_eq!(line(&i), "○ #12 ● P1 [bug] Fix auth");
        i.issue_type = Some("Task".into());
        i.assignees = vec!["octocat".into()];
        assert_eq!(line(&i), "◐ #12 ● P1 Fix auth  @octocat");
        i.open_blockers = 2;
        assert!(line(&i).starts_with("⊘ "));
        i.state = State::Closed;
        assert!(line(&i).starts_with("✓ "));
    }

    #[test]
    fn tree_nests_children_and_sorts_by_priority() {
        let epic = {
            let mut e = issue(1, "Epic", 0);
            e.issue_type = Some("Epic".into());
            e
        };
        let mut a = issue(2, "A", 2);
        a.parent = Some(1);
        let mut b = issue(3, "B", 1);
        b.parent = Some(1);
        let mut orphan = issue(4, "orphan child", 3);
        orphan.parent = Some(99); // parent not in the set → root
        let out = tree(&[a, epic, orphan, b]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "○ #1 ● P0 [epic] Epic");
        assert_eq!(lines[1], "├── ○ #3 ● P1 B");
        assert_eq!(lines[2], "└── ○ #2 ● P2 A");
        assert_eq!(lines[3], "○ #4 ● P3 orphan child");
    }

    #[test]
    fn tree_indents_grandchildren() {
        let root = issue(1, "root", 2);
        let mut mid = issue(2, "mid", 2);
        mid.parent = Some(1);
        let mut leaf = issue(3, "leaf", 2);
        leaf.parent = Some(2);
        let mut mid2 = issue(4, "mid2", 2);
        mid2.parent = Some(1);
        let out = tree(&[root, mid, leaf, mid2]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[1], "├── ○ #2 ● P2 mid");
        assert_eq!(lines[2], "│   └── ○ #3 ● P2 leaf");
        assert_eq!(lines[3], "└── ○ #4 ● P2 mid2");
    }
}
