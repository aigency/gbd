//! The issue record as gbd sees it. One GraphQL shape, shared by `ready`
//! (repository.issues), `list` (search), and `show` (repository.issue).
//! Org issue fields (Priority, Start date, gbd Role) and the optional
//! Project Status ride along so no command needs a second read.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fields;
use crate::gh;
use crate::repo::Repo;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Open,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Issue {
    /// GraphQL node id (needed to add the issue to a Project).
    pub id: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: State,
    pub issue_type: Option<String>,
    pub assignees: Vec<String>,
    pub parent: Option<u64>,
    /// GitHub's own open-blocker count (`issueDependenciesSummary.blockedBy`).
    /// This is what `is:blocked` means; it already excludes closed blockers.
    pub open_blockers: u32,
    /// Open blockers we could list (max 50). Used for `--explain`.
    pub blocked_by_open: Vec<u64>,
    /// Open issues this one blocks. Drives "unblocks" and critical-path height.
    pub blocking_open: Vec<u64>,
    /// 0 = P0 (highest) … 4 = P4. Missing field defaults to P2.
    pub priority: u8,
    /// Org field Start date, ISO date.
    pub start_date: Option<String>,
    /// Start date is after today.
    pub deferred: bool,
    /// Org field gbd Role = Memory.
    pub memory: bool,
    /// Project Status option name, when the issue is on the configured board.
    pub status: Option<String>,
}

/// `show`: the record plus everything hanging off it.
#[derive(Debug, Clone, Serialize)]
pub struct Detail {
    #[serde(flatten)]
    pub issue: Issue,
    pub body: String,
    pub author: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub state_reason: Option<String>,
    pub comments: u64,
    pub parent_issue: Option<Issue>,
    pub children: Vec<Issue>,
    pub blocked_by: Vec<Issue>,
    pub blocking: Vec<Issue>,
}

impl Issue {
    pub fn is_open(&self) -> bool {
        self.state == State::Open
    }

    pub fn is_epic(&self) -> bool {
        self.issue_type
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case("Epic"))
    }

    pub fn directly_blocked(&self) -> bool {
        self.open_blockers > 0 || !self.blocked_by_open.is_empty()
    }

    pub fn has_status(&self, name: &str) -> bool {
        self.status
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case(name))
    }

    /// Beads `in_progress`: on the board as In Progress, or assigned when
    /// there is no board.
    pub fn in_progress(&self) -> bool {
        self.has_status(crate::project::STATUS_IN_PROGRESS)
            || (self.status.is_none() && !self.assignees.is_empty())
    }

    /// Beads `deferred`: future Start date, or Deferred on the board.
    pub fn is_deferred(&self) -> bool {
        self.deferred || self.has_status(crate::project::STATUS_DEFERRED)
    }
}

// ---------------------------------------------------------------------------
// GraphQL

/// Everything a row needs. Nested (related) issues use this too.
const ROW_FIELDS: &str = r"
        id
        number
        title
        url
        state
        issueType { name }
        assignees(first: 10) { nodes { login } }
        parent { number }
        issueDependenciesSummary { blockedBy blocking }
        issueFieldValues(first: 50) {
          nodes {
            ... on IssueFieldSingleSelectValue {
              name
              field { ... on IssueFieldSingleSelect { name } }
            }
            ... on IssueFieldDateValue {
              value
              field { ... on IssueFieldDate { name } }
            }
          }
        }";

/// Row fields plus the edge lists the ready graph walks.
const GRAPH_FIELDS: &str = r"
        blockedBy(first: 50) { nodes { number state } }
        blocking(first: 50) { nodes { number state } }";

/// Needs the `read:project` scope. Only requested when `.gbd.yml` names a
/// project. 100 is GraphQL's page maximum; `hasNextPage` is checked so a
/// board hiding past it warns instead of silently reading as "no status".
const PROJECT_FIELDS: &str = r#"
        projectItems(first: 100) {
          pageInfo { hasNextPage }
          nodes {
            project {
              number
              owner {
                ... on Organization { login }
                ... on User { login }
              }
            }
            fieldValueByName(name: "Status") {
              ... on ProjectV2ItemFieldSingleSelectValue { name }
            }
          }
        }"#;

fn row_fields(with_project: bool) -> String {
    let project = if with_project { PROJECT_FIELDS } else { "" };
    format!("{ROW_FIELDS}{project}")
}

fn node_fields(with_project: bool) -> String {
    let project = if with_project { PROJECT_FIELDS } else { "" };
    format!("{ROW_FIELDS}{GRAPH_FIELDS}{project}")
}

/// Every open issue, paged. `$cursor` is nullable: omit it for page one.
pub fn snapshot_query(with_project: bool) -> String {
    let fields = node_fields(with_project);
    format!(
        r"query($owner: String!, $name: String!, $cursor: String) {{
  repository(owner: $owner, name: $name) {{
    issues(states: [OPEN], first: 100, after: $cursor) {{
      pageInfo {{ hasNextPage endCursor }}
      nodes {{{fields}
      }}
    }}
  }}
}}"
    )
}

/// Search-qualifier query (`repo:x is:issue is:open type:Task …`), paged.
pub fn search_query(with_project: bool) -> String {
    let fields = node_fields(with_project);
    format!(
        r"query($q: String!, $first: Int!, $cursor: String) {{
  search(query: $q, type: ISSUE, first: $first, after: $cursor) {{
    issueCount
    pageInfo {{ hasNextPage endCursor }}
    nodes {{
      ... on Issue {{{fields}
      }}
    }}
  }}
}}"
    )
}

/// One issue with body, parent, children, and blocker titles. Related rows
/// carry the project fields too, so their glyph and status are right.
pub fn detail_query(with_project: bool) -> String {
    let fields = node_fields(with_project);
    let row = row_fields(with_project);
    format!(
        r"query($owner: String!, $name: String!, $number: Int!) {{
  repository(owner: $owner, name: $name) {{
    issue(number: $number) {{{fields}
        body
        author {{ login }}
        createdAt
        updatedAt
        closedAt
        stateReason
        comments {{ totalCount }}
        parent {{{row}
        }}
        subIssues(first: 100) {{ nodes {{{row}
        }} }}
        blockedBy(first: 50) {{ nodes {{{row}
        }} }}
        blocking(first: 50) {{ nodes {{{row}
        }} }}
    }}
  }}
}}"
    )
}

/// Which snapshot shape to fetch: the board, if configured. Project numbers
/// are per owner, so the owner's login disambiguates `#8` from another
/// owner's `#8`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Scope<'a> {
    pub project: Option<u64>,
    pub owner: &'a str,
}

/// Page every open issue into memory. One GraphQL call per 100 issues.
pub fn snapshot(repo: &Repo, scope: Scope<'_>) -> Result<Vec<Issue>> {
    let query = snapshot_query(scope.project.is_some());
    let today = fields::today_utc();
    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut vars: Vec<(&str, &str)> = vec![("owner", repo.owner()), ("name", repo.name())];
        if let Some(c) = &cursor {
            vars.push(("cursor", c));
        }
        let data = gh::graphql(&query, &vars)?;
        let (page, next) = page(&data, &["data", "repository", "issues"], scope, &today)?;
        all.extend(page);
        if next.is_some() && next == cursor {
            bail!("pagination did not advance (cursor {next:?} repeated)");
        }
        cursor = next;
        if cursor.is_none() || all.len() >= 5000 {
            break;
        }
    }
    Ok(all)
}

/// Run a GitHub search and hydrate up to `limit` issues.
pub fn search(q: &str, limit: usize, scope: Scope<'_>) -> Result<Vec<Issue>> {
    let query = search_query(scope.project.is_some());
    let today = fields::today_utc();
    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let first = (limit - all.len()).min(100).to_string();
        let mut vars: Vec<(&str, &str)> = vec![("q", q), ("first", &first)];
        if let Some(c) = &cursor {
            vars.push(("cursor", c));
        }
        let data = gh::graphql(&query, &vars)?;
        let (page, next) = page(&data, &["data", "search"], scope, &today)?;
        all.extend(page);
        if next.is_some() && next == cursor {
            bail!("pagination did not advance (cursor {next:?} repeated)");
        }
        cursor = next;
        if cursor.is_none() || all.len() >= limit {
            break;
        }
    }
    all.truncate(limit);
    Ok(all)
}

pub fn fetch(repo: &Repo, number: u64, scope: Scope<'_>) -> Result<Detail> {
    let query = detail_query(scope.project.is_some());
    let n = number.to_string();
    let data = gh::graphql(
        &query,
        &[
            ("owner", repo.owner()),
            ("name", repo.name()),
            ("number", &n),
        ],
    )?;
    check_errors(&data)?;
    let node = data
        .pointer("/data/repository/issue")
        .filter(|v| !v.is_null())
        .with_context(|| format!("{}#{number}: no such issue", repo.name_with_owner))?;
    let today = fields::today_utc();
    detail_from_graphql(node, scope, &today).with_context(|| {
        format!(
            "{}#{number}: unexpected GraphQL shape",
            repo.name_with_owner
        )
    })
}

// ---------------------------------------------------------------------------
// Parsing

fn check_errors(data: &Value) -> Result<()> {
    if let Some(errors) = data.get("errors").and_then(|e| e.as_array()) {
        let msgs: Vec<&str> = errors
            .iter()
            .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
            .collect();
        bail!("GraphQL: {}", msgs.join("; "));
    }
    Ok(())
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn login_list(v: &Value) -> Vec<String> {
    v.get("nodes")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|n| str_at(n, "login").map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn open_numbers(v: &Value) -> Vec<u64> {
    v.get("nodes")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter(|n| str_at(n, "state").is_some_and(|s| s.eq_ignore_ascii_case("OPEN")))
                .filter_map(|n| n.get("number").and_then(Value::as_u64))
                .collect()
        })
        .unwrap_or_default()
}

/// Status of this issue's item on `owner`'s project `number`, if any.
pub fn project_status(node: &Value, project: u64, owner: &str) -> Option<String> {
    node.get("projectItems")?
        .get("nodes")?
        .as_array()?
        .iter()
        .find(|item| {
            item.pointer("/project/number").and_then(Value::as_u64) == Some(project)
                && item
                    .pointer("/project/owner/login")
                    .and_then(Value::as_str)
                    .is_some_and(|l| l.eq_ignore_ascii_case(owner))
        })
        .and_then(|item| item.pointer("/fieldValueByName/name"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The issue is in more projects than one page returns.
pub fn project_memberships_truncated(node: &Value) -> bool {
    node.pointer("/projectItems/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn from_graphql(node: &Value, scope: Scope<'_>, today: &str) -> Option<Issue> {
    let number = node.get("number")?.as_u64()?;
    let title = str_at(node, "title")?.to_string();
    let state = match str_at(node, "state") {
        Some(s) if s.eq_ignore_ascii_case("CLOSED") => State::Closed,
        _ => State::Open,
    };
    let start_date = fields::graphql_start_date(node);
    Some(Issue {
        id: str_at(node, "id").unwrap_or("").to_string(),
        number,
        title,
        url: str_at(node, "url").unwrap_or("").to_string(),
        state,
        issue_type: node
            .pointer("/issueType/name")
            .and_then(Value::as_str)
            .map(str::to_string),
        assignees: node.get("assignees").map(login_list).unwrap_or_default(),
        parent: node.pointer("/parent/number").and_then(Value::as_u64),
        open_blockers: node
            .pointer("/issueDependenciesSummary/blockedBy")
            .and_then(Value::as_u64)
            .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX)),
        blocked_by_open: node.get("blockedBy").map(open_numbers).unwrap_or_default(),
        blocking_open: node.get("blocking").map(open_numbers).unwrap_or_default(),
        priority: fields::graphql_priority_name(node)
            .as_deref()
            .map_or(fields::DEFAULT_RANK, fields::rank_from_option),
        deferred: start_date
            .as_deref()
            .is_some_and(|d| fields::is_future_date(d, today)),
        start_date,
        memory: fields::is_memory_role(fields::graphql_role(node).as_deref()),
        status: scope.project.and_then(|p| {
            let status = project_status(node, p, scope.owner);
            if status.is_none() && project_memberships_truncated(node) {
                eprintln!(
                    "warning: #{number} is in more than 100 projects; board Status may be missing"
                );
            }
            status
        }),
    })
}

fn related(node: &Value, key: &str, scope: Scope<'_>, today: &str) -> Vec<Issue> {
    node.pointer(&format!("/{key}/nodes"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|n| from_graphql(n, scope, today))
                .collect()
        })
        .unwrap_or_default()
}

pub fn detail_from_graphql(node: &Value, scope: Scope<'_>, today: &str) -> Option<Detail> {
    let issue = from_graphql(node, scope, today)?;
    Some(Detail {
        issue,
        body: str_at(node, "body").unwrap_or("").to_string(),
        author: node
            .pointer("/author/login")
            .and_then(Value::as_str)
            .map(str::to_string),
        created_at: str_at(node, "createdAt").unwrap_or("").to_string(),
        updated_at: str_at(node, "updatedAt").unwrap_or("").to_string(),
        closed_at: str_at(node, "closedAt").map(str::to_string),
        state_reason: str_at(node, "stateReason").map(str::to_string),
        comments: node
            .pointer("/comments/totalCount")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        parent_issue: node
            .get("parent")
            .filter(|p| !p.is_null())
            .and_then(|p| from_graphql(p, scope, today)),
        children: related(node, "subIssues", scope, today),
        blocked_by: related(node, "blockedBy", scope, today),
        blocking: related(node, "blocking", scope, today),
    })
}

/// Parse one page of a connection at `path`. Returns the issues and, when
/// there is another page, the cursor to fetch it with.
pub fn page(
    data: &Value,
    path: &[&str],
    scope: Scope<'_>,
    today: &str,
) -> Result<(Vec<Issue>, Option<String>)> {
    check_errors(data)?;
    let pointer = format!("/{}", path.join("/"));
    let conn = data
        .pointer(&pointer)
        .with_context(|| format!("GraphQL response missing {}", path.join(".")))?;
    let nodes = conn
        .get("nodes")
        .and_then(Value::as_array)
        .context("GraphQL response missing nodes")?;
    let parsed: Vec<Issue> = nodes
        .iter()
        .filter_map(|n| from_graphql(n, scope, today))
        .collect();
    let has_next = conn
        .pointer("/pageInfo/hasNextPage")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let cursor = if has_next {
        conn.pointer("/pageInfo/endCursor")
            .and_then(Value::as_str)
            .map(str::to_string)
    } else {
        None
    };
    Ok((parsed, cursor))
}

/// `search(...).issueCount` for one query. One GraphQL call, no nodes.
pub fn count(q: &str) -> Result<u64> {
    let data = gh::graphql(
        "query($q: String!) { search(query: $q, type: ISSUE, first: 1) { issueCount } }",
        &[("q", q)],
    )?;
    check_errors(&data)?;
    data.pointer("/data/search/issueCount")
        .and_then(Value::as_u64)
        .context("GraphQL response missing search.issueCount")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Shape captured from a real `gh api graphql` response (GitHub API
    /// 2026-09). If GitHub renames a field, this is the test that fails.
    pub fn real_node() -> Value {
        json!({
            "id": "I_kwDOUXTazc8AAAABQ52aow",
            "number": 12,
            "title": "Fix auth refresh",
            "url": "https://github.com/acme/widgets/issues/12",
            "state": "OPEN",
            "issueType": { "name": "Bug" },
            "assignees": { "nodes": [] },
            "parent": { "number": 8 },
            "issueDependenciesSummary": { "blockedBy": 1, "blocking": 2 },
            "blockedBy": { "nodes": [ { "number": 3, "state": "OPEN" }, { "number": 4, "state": "CLOSED" } ] },
            "blocking": { "nodes": [ { "number": 20, "state": "OPEN" }, { "number": 21, "state": "OPEN" } ] },
            "issueFieldValues": { "nodes": [
                { "name": "P1", "field": { "name": "Priority" } },
                { "value": "2099-01-01", "field": { "name": "Start date" } }
            ] },
            "projectItems": { "pageInfo": { "hasNextPage": false }, "nodes": [
                { "project": { "number": 3, "owner": { "login": "acme" } }, "fieldValueByName": { "name": "In Progress" } },
                { "project": { "number": 7, "owner": { "login": "someone-else" } }, "fieldValueByName": { "name": "Deferred" } },
                { "project": { "number": 7, "owner": { "login": "acme" } }, "fieldValueByName": { "name": "Ready" } }
            ] }
        })
    }

    fn scope(project: Option<u64>) -> Scope<'static> {
        Scope {
            project,
            owner: "acme",
        }
    }

    #[test]
    fn parses_real_graphql_node() {
        let i = from_graphql(&real_node(), scope(Some(7)), "2026-09-11").unwrap();
        assert_eq!(i.number, 12);
        assert_eq!(i.state, State::Open);
        assert_eq!(i.issue_type.as_deref(), Some("Bug"));
        assert_eq!(i.parent, Some(8));
        assert_eq!(i.open_blockers, 1);
        assert_eq!(i.blocked_by_open, vec![3], "closed blocker dropped");
        assert_eq!(i.blocking_open, vec![20, 21]);
        assert_eq!(i.priority, 1);
        assert_eq!(i.start_date.as_deref(), Some("2099-01-01"));
        assert!(i.deferred, "future Start date");
        assert!(!i.memory);
        assert_eq!(
            i.status.as_deref(),
            Some("Ready"),
            "status from acme's #7, not #3 and not someone-else's #7"
        );
        let none = from_graphql(&real_node(), scope(None), "2026-09-11").unwrap();
        assert_eq!(none.status, None);
    }

    #[test]
    fn in_progress_prefers_board_over_assignee() {
        let mut i = from_graphql(&real_node(), scope(None), "2026-09-11").unwrap();
        assert!(!i.in_progress());
        i.assignees = vec!["octocat".into()];
        assert!(i.in_progress(), "assigned, no board");
        i.status = Some("Ready".into());
        assert!(
            !i.in_progress(),
            "board says Ready; assignee alone does not park it"
        );
        i.status = Some("In Progress".into());
        assert!(i.in_progress());
    }

    #[test]
    fn truncated_memberships_are_detected() {
        let mut node = real_node();
        assert!(!project_memberships_truncated(&node));
        node["projectItems"]["pageInfo"]["hasNextPage"] = json!(true);
        assert!(project_memberships_truncated(&node));
        assert!(snapshot_query(true).contains("projectItems(first: 100)"));
    }

    #[test]
    fn queries_include_project_only_when_asked() {
        assert!(!snapshot_query(false).contains("projectItems"));
        assert!(snapshot_query(true).contains("projectItems"));
        assert!(!search_query(false).contains("projectItems"));
        assert!(detail_query(true).contains("projectItems"));
        assert_eq!(
            detail_query(true).matches("projectItems").count(),
            5,
            "the issue plus parent, subIssues, blockedBy, blocking rows"
        );
        assert_eq!(detail_query(false).matches("projectItems").count(), 0);
        assert!(snapshot_query(false).contains("$cursor: String)"));
        assert!(!snapshot_query(false).contains(" date\n"));
    }

    #[test]
    fn page_surfaces_graphql_errors() {
        let bad = json!({ "errors": [ { "message": "Field 'date' doesn't exist" } ] });
        let err = page(&bad, &["data", "search"], scope(None), "2026-09-11").unwrap_err();
        assert!(err.to_string().contains("doesn't exist"));
    }

    #[test]
    fn page_returns_cursor_only_when_more() {
        let p = json!({ "data": { "repository": { "issues": {
            "pageInfo": { "hasNextPage": true, "endCursor": "abc" },
            "nodes": [ real_node() ]
        } } } });
        let (issues, cursor) = page(
            &p,
            &["data", "repository", "issues"],
            scope(None),
            "2026-09-11",
        )
        .unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(cursor.as_deref(), Some("abc"));
        let last = json!({ "data": { "search": {
            "pageInfo": { "hasNextPage": false, "endCursor": "zzz" },
            "nodes": []
        } } });
        let (_, cursor) = page(&last, &["data", "search"], scope(None), "2026-09-11").unwrap();
        assert_eq!(cursor, None);
    }

    #[test]
    fn detail_parses_related_issues() {
        let mut node = real_node();
        node["body"] = json!("Body text");
        node["author"] = json!({ "login": "octocat" });
        node["createdAt"] = json!("2026-09-11T22:07:56Z");
        node["updatedAt"] = json!("2026-09-11T22:08:18Z");
        node["comments"] = json!({ "totalCount": 2 });
        node["parent"] = json!({ "number": 8, "title": "Epic", "state": "OPEN", "id": "P" });
        node["subIssues"] = json!({ "nodes": [
            { "number": 9, "title": "child A", "state": "OPEN", "id": "A" },
            { "number": 10, "title": "child B", "state": "CLOSED", "id": "B" }
        ] });
        node["blockedBy"] =
            json!({ "nodes": [ { "number": 3, "title": "blocker", "state": "OPEN", "id": "C" } ] });
        let d = detail_from_graphql(&node, scope(None), "2026-09-11").unwrap();
        assert_eq!(d.body, "Body text");
        assert_eq!(d.author.as_deref(), Some("octocat"));
        assert_eq!(d.comments, 2);
        assert_eq!(d.parent_issue.as_ref().map(|p| p.number), Some(8));
        assert_eq!(d.children.len(), 2);
        assert_eq!(d.children[1].state, State::Closed);
        assert_eq!(d.blocked_by[0].title, "blocker");
        assert_eq!(d.issue.blocked_by_open, vec![3]);
    }
}
