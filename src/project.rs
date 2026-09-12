//! GitHub Projects: the board. Status is where Beads `in_progress` and
//! `deferred` live when a project is configured. Everything goes through
//! `gh project …`, which needs the `project` scope (`gh auth refresh -s project`).

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::gh;
use crate::repo::Repo;

pub const STATUS_READY: &str = "Ready";
pub const STATUS_IN_PROGRESS: &str = "In Progress";
/// Maintained by gbd from GitHub's open-blocker count, never set by hand:
/// project views cannot filter on `is:blocked`, so the board carries it.
pub const STATUS_BLOCKED: &str = "Blocked";
pub const STATUS_DEFERRED: &str = "Deferred";
pub const STATUS_DONE: &str = "Done";

/// The five options `gbd init` puts on the Status field, in column order,
/// with colors.
pub const STATUSES: [(&str, &str); 5] = [
    (STATUS_READY, "GREEN"),
    (STATUS_IN_PROGRESS, "YELLOW"),
    (STATUS_BLOCKED, "RED"),
    (STATUS_DEFERRED, "GRAY"),
    (STATUS_DONE, "PURPLE"),
];

pub const SCOPE_HINT: &str = "gh auth refresh -s project";

#[derive(Debug, Clone)]
pub struct StatusOption {
    pub id: String,
    pub name: String,
}

/// One row of `gh project item-list`. `number` is None for draft items.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BoardItem {
    pub number: Option<u64>,
    pub title: String,
    pub repo: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Board {
    pub owner: String,
    pub number: u64,
    /// Project node id (`PVT_…`).
    pub id: String,
    pub title: String,
    pub url: String,
    pub status_field_id: String,
    pub status_options: Vec<StatusOption>,
}

fn scope_error(err: anyhow::Error) -> anyhow::Error {
    let msg = format!("{err:#}");
    if msg.contains("scope") || msg.contains("read:project") {
        anyhow!("{msg}\nGitHub Projects need the `project` scope. Run: {SCOPE_HINT}")
    } else {
        err
    }
}

impl Board {
    /// Two `gh project` calls: the project itself and its fields.
    pub fn load(owner: &str, number: u64) -> Result<Board> {
        let n = number.to_string();
        let view: Value =
            gh::run_json(&["project", "view", &n, "--owner", owner, "--format", "json"])
                .map_err(scope_error)?;
        let fields: Value = gh::run_json(&[
            "project",
            "field-list",
            &n,
            "--owner",
            owner,
            "--format",
            "json",
            "--limit",
            "100",
        ])
        .map_err(scope_error)?;
        let status = fields
            .get("fields")
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter().find(|f| {
                    f.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.eq_ignore_ascii_case("Status"))
                })
            })
            .with_context(|| format!("project #{number} has no Status field"))?;
        let status_options = status
            .get("options")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| {
                        Some(StatusOption {
                            id: o.get("id")?.as_str()?.to_string(),
                            name: o.get("name")?.as_str()?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let s = |key: &str| {
            view.get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        Ok(Board {
            owner: owner.to_string(),
            number,
            id: s("id"),
            title: s("title"),
            url: s("url"),
            status_field_id: status
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            status_options,
        })
    }

    /// Every item on the board with its Status, closed issues included.
    /// Every item on the board with its Status, closed issues and drafts
    /// included. Paged to the end (`gh project item-list --limit` is a cap,
    /// not a pager, and a truncated list would make `board sync` overwrite
    /// Statuses it could not see).
    pub fn items(&self) -> Result<Vec<BoardItem>> {
        const QUERY: &str = r#"query($id: ID!, $cursor: String) {
  node(id: $id) { ... on ProjectV2 { items(first: 100, after: $cursor) {
    pageInfo { hasNextPage endCursor }
    nodes {
      fieldValueByName(name: "Status") { ... on ProjectV2ItemFieldSingleSelectValue { name } }
      content {
        __typename
        ... on Issue { number title repository { nameWithOwner } }
        ... on PullRequest { number title repository { nameWithOwner } }
        ... on DraftIssue { title }
      }
    }
  } } }
}"#;
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        let mut previous: Option<String> = None;
        loop {
            let mut vars: Vec<(&str, &str)> = vec![("id", &self.id)];
            if let Some(c) = &cursor {
                vars.push(("cursor", c));
            }
            let data = gh::graphql(QUERY, &vars).map_err(scope_error)?;
            if let Some(errors) = data.get("errors") {
                bail!("listing items of project #{}: {errors}", self.number);
            }
            let conn = data
                .pointer("/data/node/items")
                .context("GraphQL response missing node.items")?;
            all.extend(
                conn.get("nodes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|it| BoardItem {
                        number: it.pointer("/content/number").and_then(Value::as_u64),
                        title: it
                            .pointer("/content/title")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        repo: it
                            .pointer("/content/repository/nameWithOwner")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        status: it
                            .pointer("/fieldValueByName/name")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    }),
            );
            let more = conn
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            cursor = if more {
                conn.pointer("/pageInfo/endCursor")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            } else {
                None
            };
            if cursor.is_none() {
                return Ok(all);
            }
            if cursor == previous {
                bail!("pagination did not advance (cursor {cursor:?} repeated)");
            }
            previous.clone_from(&cursor);
        }
    }

    pub fn has_status(&self, name: &str) -> bool {
        self.status_options
            .iter()
            .any(|o| o.name.eq_ignore_ascii_case(name))
    }

    fn option_id(&self, status: &str) -> Result<&str> {
        self.status_options
            .iter()
            .find(|o| o.name.eq_ignore_ascii_case(status))
            .map(|o| o.id.as_str())
            .ok_or_else(|| {
                anyhow!(
                    "project #{} has no Status option {status:?} (has: {}). Run: gbd init",
                    self.number,
                    self.status_options
                        .iter()
                        .map(|o| o.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    /// Add the issue to the board if it is not there (idempotent on GitHub's
    /// side) and set its Status. Two `gh project` calls.
    pub fn set_status(&self, issue_url: &str, status: &str) -> Result<()> {
        let option_id = self.option_id(status)?;
        let n = self.number.to_string();
        let item: Value = gh::run_json(&[
            "project",
            "item-add",
            &n,
            "--owner",
            &self.owner,
            "--url",
            issue_url,
            "--format",
            "json",
        ])
        .map_err(scope_error)?;
        let item_id = item
            .get("id")
            .and_then(Value::as_str)
            .context("gh project item-add returned no item id")?;
        gh::run(&[
            "project",
            "item-edit",
            "--id",
            item_id,
            "--project-id",
            &self.id,
            "--field-id",
            &self.status_field_id,
            "--single-select-option-id",
            option_id,
        ])
        .map_err(scope_error)?;
        Ok(())
    }

    /// Make sure every option in [`STATUSES`] exists on Status.
    ///
    /// `updateProjectV2Field` replaces the option list wholesale. Existing
    /// options are carried over *with their ids*: GitHub keeps an option's
    /// identity, and so every card's value, only when the id is sent. A
    /// missing option is slotted in after the nearest earlier gbd status the
    /// board has (Blocked lands between In Progress and Deferred on a
    /// pre-1.2 board). On a board `gbd init` just created, GitHub's default
    /// `Todo` is renamed to `Ready`; on an existing board it is kept, so no
    /// item silently loses its status.
    pub fn ensure_statuses(&mut self, fresh: bool) -> Result<bool> {
        struct Opt {
            /// None for an option that does not exist yet.
            id: Option<String>,
            name: String,
            color: &'static str,
        }
        let mut options: Vec<Opt> = self
            .status_options
            .iter()
            .map(|o| Opt {
                id: Some(o.id.clone()),
                name: o.name.clone(),
                color: STATUSES
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(&o.name))
                    .map_or("GRAY", |(_, c)| *c),
            })
            .collect();
        if fresh && !self.has_status(STATUS_READY) {
            if let Some(todo) = options
                .iter_mut()
                .find(|o| o.name.eq_ignore_ascii_case("Todo"))
            {
                todo.name = STATUS_READY.to_string();
                todo.color = "GREEN";
            }
        }
        let mut changed = false;
        for (pos, (want, color)) in STATUSES.iter().enumerate() {
            if options.iter().any(|o| o.name.eq_ignore_ascii_case(want)) {
                continue;
            }
            let at = STATUSES[..pos]
                .iter()
                .rev()
                .find_map(|(prev, _)| {
                    options
                        .iter()
                        .position(|o| o.name.eq_ignore_ascii_case(prev))
                })
                .map_or(options.len(), |i| i + 1);
            options.insert(
                at,
                Opt {
                    id: None,
                    name: (*want).to_string(),
                    color,
                },
            );
            changed = true;
        }
        if !changed && (!fresh || self.has_status(STATUS_READY)) {
            return Ok(false);
        }
        let options = options
            .iter()
            .map(|o| {
                let id =
                    o.id.as_deref()
                        .map_or(String::new(), |id| format!(r#"id: "{id}", "#));
                format!(
                    r#"{{{id}name: "{}", color: {}, description: ""}}"#,
                    o.name.replace('"', "\\\""),
                    o.color
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mutation = format!(
            r#"mutation {{
  updateProjectV2Field(input: {{ fieldId: "{}", singleSelectOptions: [{options}] }}) {{
    projectV2Field {{ ... on ProjectV2SingleSelectField {{ id options {{ id name }} }} }}
  }}
}}"#,
            self.status_field_id
        );
        let data = gh::graphql(&mutation, &[]).map_err(scope_error)?;
        if let Some(errors) = data.get("errors") {
            bail!("updating Status options: {errors}");
        }
        *self = Board::load(&self.owner, self.number)?;
        Ok(true)
    }
}

/// Open projects linked to the repo, as `(number, title)`. Paged to the end,
/// so a decision to create a board is never made on a partial list.
pub fn linked_to(repo: &Repo) -> Result<Vec<(u64, String)>> {
    const QUERY: &str = "query($owner: String!, $name: String!, $cursor: String) { repository(owner: $owner, name: $name) { projectsV2(first: 100, after: $cursor) { pageInfo { hasNextPage endCursor } nodes { number title closed } } } }";
    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    let mut previous: Option<String> = None;
    loop {
        let mut vars: Vec<(&str, &str)> = vec![("owner", repo.owner()), ("name", repo.name())];
        if let Some(c) = &cursor {
            vars.push(("cursor", c));
        }
        let data = gh::graphql(QUERY, &vars).map_err(scope_error)?;
        if let Some(errors) = data.get("errors") {
            bail!(
                "listing projects linked to {}: {errors}",
                repo.name_with_owner
            );
        }
        let conn = data
            .pointer("/data/repository/projectsV2")
            .context("GraphQL response missing repository.projectsV2")?;
        all.extend(
            conn.get("nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|p| p.get("closed").and_then(Value::as_bool) != Some(true))
                .filter_map(|p| {
                    Some((
                        p.get("number")?.as_u64()?,
                        p.get("title")?.as_str()?.to_string(),
                    ))
                }),
        );
        let more = conn
            .pointer("/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        cursor = if more {
            conn.pointer("/pageInfo/endCursor")
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        };
        if cursor.is_none() {
            return Ok(all);
        }
        if cursor == previous {
            bail!("pagination did not advance (cursor {cursor:?} repeated)");
        }
        previous.clone_from(&cursor);
    }
}

/// Reuse a board already linked to the repo with this title, if any. With
/// several, the lowest number wins and the caller should warn.
pub fn find_linked(repo: &Repo, title: &str) -> Result<Vec<u64>> {
    let mut nums: Vec<u64> = linked_to(repo)?
        .into_iter()
        .filter(|(_, t)| t.eq_ignore_ascii_case(title))
        .map(|(n, _)| n)
        .collect();
    nums.sort_unstable();
    Ok(nums)
}

/// `gh project create` + Status options + link to the repo.
pub fn create(repo: &Repo, title: &str) -> Result<Board> {
    let created: Value = gh::run_json(&[
        "project",
        "create",
        "--owner",
        repo.owner(),
        "--title",
        title,
        "--format",
        "json",
    ])
    .map_err(scope_error)?;
    let number = created
        .get("number")
        .and_then(Value::as_u64)
        .context("gh project create returned no number")?;
    let mut board = Board::load(repo.owner(), number)?;
    board.ensure_statuses(true)?;
    link(&board, repo)?;
    Ok(board)
}

pub fn link(board: &Board, repo: &Repo) -> Result<()> {
    let n = board.number.to_string();
    gh::run(&[
        "project",
        "link",
        &n,
        "--owner",
        &board.owner,
        "--repo",
        &repo.name_with_owner,
    ])
    .map_err(scope_error)?;
    Ok(())
}

/// Beads status word → board Status option.
pub fn status_for(word: &str) -> Result<&'static str> {
    Ok(
        match word.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "open" | "ready" | "todo" => STATUS_READY,
            "in_progress" | "inprogress" | "claimed" => STATUS_IN_PROGRESS,
            "deferred" | "defer" => STATUS_DEFERRED,
            "done" | "closed" => STATUS_DONE,
            "blocked" => bail!(
                "blocked is not set by hand; it follows from open blockers (gbd dep add <n> <blocker>)"
            ),
            other => bail!("unknown status {other:?}; use ready, in_progress, deferred, or done"),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_words_map_to_board_options() {
        assert_eq!(status_for("in_progress").unwrap(), STATUS_IN_PROGRESS);
        assert_eq!(status_for("in-progress").unwrap(), STATUS_IN_PROGRESS);
        assert_eq!(status_for("Ready").unwrap(), STATUS_READY);
        assert_eq!(status_for("deferred").unwrap(), STATUS_DEFERRED);
        assert_eq!(status_for("done").unwrap(), STATUS_DONE);
        assert!(
            status_for("blocked").is_err(),
            "blocked is derived, never set"
        );
    }
}
