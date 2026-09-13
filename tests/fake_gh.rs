//! End-to-end tests through a fake `gh` on PATH (`tests/fake_gh/gh`).
//!
//! These pin the contract between gbd and gh: which subcommands get
//! called, with which flags, and that gbd actually reads what gh prints.
//! Nothing here touches the network.

use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

struct Harness {
    /// Fixture dir for the fake gh; also where calls.log lands.
    gh_dir: TempDir,
    /// A repo root with `.gbd.yml` so repo resolution never shells out.
    cwd: TempDir,
}

impl Harness {
    fn new() -> Self {
        let gh_dir = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        fs::write(
            cwd.path().join(".gbd.yml"),
            "repo: acme/widgets\nmemory_issue: 3\n",
        )
        .unwrap();
        Self { gh_dir, cwd }
    }

    /// Register a fixture: when the gh invocation contains `args`, print `out`.
    fn on(&self, name: &str, args: &str, out: &str) -> &Self {
        fs::write(self.gh_dir.path().join(format!("{name}.args")), args).unwrap();
        fs::write(self.gh_dir.path().join(format!("{name}.out")), out).unwrap();
        self
    }

    /// Successive matches return `outs[0]`, `outs[1]`, … in order.
    fn on_seq(&self, name: &str, args: &str, outs: &[&str]) -> &Self {
        fs::write(self.gh_dir.path().join(format!("{name}.args")), args).unwrap();
        for (i, out) in outs.iter().enumerate() {
            fs::write(
                self.gh_dir.path().join(format!("{name}.out.{}", i + 1)),
                out,
            )
            .unwrap();
        }
        self
    }

    fn on_fail(&self, name: &str, args: &str, stderr_out: &str) -> &Self {
        self.on(name, args, stderr_out);
        fs::write(self.gh_dir.path().join(format!("{name}.code")), "1").unwrap();
        self
    }

    fn gbd(&self) -> Command {
        let fake_bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_gh");
        let path = format!(
            "{}:{}",
            fake_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::cargo_bin("gbd").unwrap();
        cmd.current_dir(self.cwd.path())
            .env("PATH", path)
            .env("FAKE_GH_DIR", self.gh_dir.path());
        cmd
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.gh_dir.path().join("calls.log")).unwrap_or_default()
    }
}

fn fixture(name: &str) -> String {
    let p: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    fs::read_to_string(p).unwrap()
}

fn search_response(nodes: &str) -> String {
    format!(
        r#"{{"data":{{"search":{{"issueCount":1,"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[{nodes}]}}}}}}"#
    )
}

const NODE_10: &str = r#"{"id":"I10","number":10,"title":"child B","url":"u","state":"OPEN",
  "issueType":{"name":"Task"},"assignees":{"nodes":[]},"parent":{"number":8},
  "issueDependenciesSummary":{"blockedBy":0,"blocking":0},"blockedBy":{"nodes":[]},"blocking":{"nodes":[]},
  "issueFieldValues":{"nodes":[{"name":"P1","field":{"name":"Priority"}}]}}"#;

const NODE_8: &str = r#"{"id":"I8","number":8,"title":"parent","url":"u","state":"OPEN",
  "issueType":{"name":"Epic"},"assignees":{"nodes":[]},"parent":null,
  "issueDependenciesSummary":{"blockedBy":0,"blocking":0},"blockedBy":{"nodes":[]},"blocking":{"nodes":[]},
  "issueFieldValues":{"nodes":[]}}"#;

#[test]
fn list_reads_gh_stdout_and_renders_a_tree() {
    // Regression: v0.2.0 inherited stdout, so gh printed to the terminal and
    // gbd parsed "" ("EOF while parsing a value at line 1 column 0").
    let h = Harness::new();
    h.on(
        "search",
        "search(query: $q",
        &search_response(&format!("{NODE_10},{NODE_8}")),
    );
    h.gbd()
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "○ #8 ● P2 [epic] parent\n└── ○ #10 ● P1 child B\n",
        ))
        .stdout(predicate::str::contains(r#"{"number"#).not());
    let calls = h.calls();
    assert!(
        calls.contains("-F q=repo:acme/widgets is:issue is:open"),
        "{calls}"
    );
    assert!(
        !calls.contains("issue list"),
        "list is one GraphQL search, not gh issue list: {calls}"
    );
}

#[test]
fn list_filters_become_search_qualifiers() {
    let h = Harness::new();
    h.on("search", "search(query: $q", &search_response(NODE_10));
    h.gbd()
        .args([
            "list",
            "--state",
            "all",
            "--type",
            "Bug",
            "--assignee",
            "@me",
            "--parent",
            "8",
            "--flat",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("○ #10 ● P1 child B"));
    let calls = h.calls();
    assert!(
        calls.contains(
            "-F q=repo:acme/widgets is:issue type:Bug assignee:@me parent-issue:acme/widgets#8"
        ),
        "{calls}"
    );
}

#[test]
fn ready_is_one_graphql_call_with_the_real_field_shape() {
    let h = Harness::new();
    h.on("snapshot", "api graphql", &fixture("snapshot.json"));
    h.gbd()
        .arg("ready")
        .assert()
        .success()
        // #5 is P0 on the org field and unassigned; #4 is P0 but assigned.
        .stdout(predicate::str::starts_with(
            "○ #5 ● P0 Enable GitHub Issues",
        ))
        .stdout(predicate::str::contains("#4 ").not());
    let calls = h.calls();
    assert_eq!(
        calls.matches("api graphql").count(),
        1,
        "ready must page once for a single-page repo: {calls}"
    );
    assert!(calls.contains("-F owner=acme"), "{calls}");
    assert!(calls.contains("IssueFieldDateValue"), "{calls}");
    assert!(
        calls.contains(" value\n") || calls.contains("value "),
        "date fields are `value` on IssueFieldDateValue: {calls}"
    );
    assert!(
        !calls.contains(" date\n"),
        "no `date` field exists: {calls}"
    );
    assert!(
        !calls.contains("labels("),
        "labels are not part of ready: {calls}"
    );
    assert!(
        !calls.contains("projectItems"),
        "no project configured, so no read:project scope needed: {calls}"
    );
    assert!(
        !calls.contains("issue-field-values"),
        "no REST N+1 after the snapshot: {calls}"
    );
}

#[test]
fn ready_json_reports_field_priority() {
    let h = Harness::new();
    h.on("snapshot", "api graphql", &fixture("snapshot.json"));
    let out = h.gbd().args(["ready", "--json"]).output().unwrap();
    assert!(out.status.success());
    let items: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(items[0]["number"], 5);
    assert_eq!(items[0]["priority"], 0, "P0 from the org Priority field");
    assert_eq!(items[0]["parent"], 4);
    assert!(
        items.iter().all(|i| i["number"] != 4),
        "assigned issue excluded"
    );
}

#[test]
fn ready_reads_project_status_when_configured() {
    let h = Harness::new();
    fs::write(
        h.cwd.path().join(".gbd.yml"),
        "repo: acme/widgets\nmemory_issue: 3\nproject: 7\n",
    )
    .unwrap();
    // Same repo, but #5 is In Progress on project 7 and #6 is Deferred.
    let mut snap: serde_json::Value = serde_json::from_str(&fixture("snapshot.json")).unwrap();
    for node in snap["data"]["repository"]["issues"]["nodes"]
        .as_array_mut()
        .unwrap()
    {
        let status = match node["number"].as_u64().unwrap() {
            5 => "In Progress",
            6 => "Deferred",
            _ => "Ready",
        };
        node["projectItems"] = serde_json::json!({ "pageInfo": { "hasNextPage": false }, "nodes": [
            { "project": { "number": 7, "owner": { "login": "acme" } }, "fieldValueByName": { "name": status } }
        ] });
    }
    h.on("snapshot", "api graphql", &snap.to_string());
    let out = h.gbd().args(["ready", "--json"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let items: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
    let nums: Vec<u64> = items
        .iter()
        .map(|i| i["number"].as_u64().unwrap())
        .collect();
    assert!(!nums.contains(&5), "In Progress on the board: {nums:?}");
    assert!(!nums.contains(&6), "Deferred on the board: {nums:?}");
    assert!(nums.contains(&8), "Ready on the board: {nums:?}");
    assert!(
        h.calls().contains("projectItems"),
        "status is read from the board"
    );
}

#[test]
fn ready_surfaces_graphql_errors_instead_of_falling_back() {
    let h = Harness::new();
    h.on_fail(
        "snapshot",
        "api graphql",
        r#"{"errors":[{"message":"Field 'nope' doesn't exist on type 'Issue'"}]}"#,
    );
    h.gbd()
        .arg("ready")
        .assert()
        .failure()
        .stderr(predicate::str::contains("doesn't exist"));
    assert!(
        !h.calls().contains("issue list"),
        "no silent REST fallback: {}",
        h.calls()
    );
}

#[test]
fn create_writes_priority_to_the_org_field_not_a_label() {
    let h = Harness::new();
    h.on(
        "create",
        "issue create -R acme/widgets",
        "https://github.com/acme/widgets/issues/42",
    )
    .on(
        "fields",
        "orgs/acme/issue-fields",
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]}]"#,
    )
    .on("set", "issues/42/issue-field-values", "{}");
    h.gbd()
        .args([
            "create",
            "Fix auth refresh",
            "-t",
            "Bug",
            "-p",
            "1",
            "--deps",
            "12,13",
            "--parent",
            "8",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("acme/widgets#42"))
        .stdout(predicate::str::contains("Priority P1"));
    let calls = h.calls();
    assert!(
        calls.contains("--type Bug --parent 8 --blocked-by 12,13"),
        "one gh create call carries type, parent, deps: {calls}"
    );
    assert!(
        !calls.contains("--label"),
        "priority is never a label: {calls}"
    );
    assert!(calls.contains("POST"), "{calls}");
    assert!(
        calls.contains("issues/42/issue-field-values --input -"),
        "{calls}"
    );
    assert!(
        calls.contains(r#"STDIN: {"issue_field_values":[{"field_id":46822523,"value":"P1"}]}"#),
        "field id + option name on stdin: {calls}"
    );
}

#[test]
fn priority_accepts_p_prefix_and_rejects_words() {
    let h = Harness::new();
    h.on(
        "fields",
        "orgs/acme/issue-fields",
        r#"[{"id":9,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]}]"#,
    )
    .on("set", "issues/12/issue-field-values", "{}");
    h.gbd()
        .args(["priority", "12", "P3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#12 Priority P3"));
    h.gbd()
        .args(["priority", "12", "High"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("P0-P4"));
}

#[test]
fn show_is_one_graphql_call_rendered_like_beads() {
    let h = Harness::new();
    let detail = format!(
        r#"{{"data":{{"repository":{{"issue":{{
            "id":"I8","number":8,"title":"parent","url":"https://github.com/acme/widgets/issues/8","state":"OPEN",
            "issueType":{{"name":"Epic"}},"assignees":{{"nodes":[{{"login":"octocat"}}]}},"parent":null,
            "issueDependenciesSummary":{{"blockedBy":0,"blocking":0}},
            "issueFieldValues":{{"nodes":[{{"name":"P0","field":{{"name":"Priority"}}}}]}},
            "body":"Hierarchy check.","author":{{"login":"octocat"}},
            "createdAt":"2026-09-11T22:07:56Z","updatedAt":"2026-09-11T22:08:18Z","closedAt":null,"stateReason":null,
            "comments":{{"totalCount":1}},
            "subIssues":{{"nodes":[{NODE_10}]}},
            "blockedBy":{{"nodes":[]}},"blocking":{{"nodes":[]}}
        }}}}}}}}"#
    );
    h.on("detail", "issue(number: $number)", &detail);
    h.gbd()
        .args(["show", "8"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "◐ #8 [EPIC] · parent   [● P0 · OPEN · in progress]",
        ))
        .stdout(predicate::str::contains(
            "Author: octocat · Assignee: octocat · Type: Epic",
        ))
        .stdout(predicate::str::contains(
            "DESCRIPTION\n\n  Hierarchy check.",
        ))
        .stdout(predicate::str::contains(
            "CHILDREN (1)\n  ○ #10 ● P1 child B",
        ))
        .stdout(predicate::str::contains("1 comment: gbd comments 8"));
    let calls = h.calls();
    assert_eq!(calls.matches("api graphql").count(), 1, "{calls}");
    assert!(!calls.contains("issue view"), "{calls}");
    assert!(
        !calls.contains("issue-field-values"),
        "fields come from the same query: {calls}"
    );
}

fn board_fixtures(h: &Harness) {
    fs::write(
        h.cwd.path().join(".gbd.yml"),
        "repo: acme/widgets\nmemory_issue: 3\nproject: 7\n",
    )
    .unwrap();
    h.on("pview", "project view 7 --owner acme --format json",
         r#"{"id":"PVT_1","number":7,"title":"widgets board","url":"https://github.com/orgs/acme/projects/7"}"#)
     .on("pfields", "project field-list 7 --owner acme --format json",
         r#"{"fields":[{"id":"F_status","name":"Status","type":"ProjectV2SingleSelectField","options":[
            {"id":"O_blocked","name":"Blocked"},{"id":"O_def","name":"Deferred"},{"id":"O_ready","name":"Ready"},
            {"id":"O_wip","name":"In Progress"},{"id":"O_done","name":"Done"}]}]}"#)
     .on("padd", "project item-add 7 --owner acme --url https://github.com/acme/widgets/issues/12 --format json",
         r#"{"id":"PVTI_12"}"#)
     .on("pedit", "project item-edit --id PVTI_12 --project-id PVT_1 --field-id F_status --single-select-option-id", "")
     // The importer's check for an issue an earlier run created but never recorded.
     .on("no-unrecorded", "issue list -R acme/widgets --search", "[]")
     .on("no-newest", "issue list -R acme/widgets --state all --limit 20 --json number,url,body", "[]")
     // The importer's preflights: any login can be assigned here, and the
     // org has every type.
     .on("assignable", "repos/acme/widgets/assignees/", "")
     .on("types", TYPES_GET, r#"[{"name":"Epic"},{"name":"Feature"},{"name":"Bug"},{"name":"Task"},{"name":"Chore"},{"name":"Decision"}]"#);
}

#[test]
fn claim_assigns_me_and_moves_the_board_item_to_in_progress() {
    let h = Harness::new();
    board_fixtures(&h);
    h.on_seq(
        "assignees",
        "issue view 12 -R acme/widgets --json assignees",
        &[
            r#"{"assignees":[]}"#,
            r#"{"assignees":[{"login":"octocat"}]}"#,
        ],
    )
    .on("me", "api user --jq .login", "octocat")
    .on(
        "assign",
        "issue edit 12 -R acme/widgets --add-assignee @me",
        "",
    );
    h.gbd()
        .args(["update", "12", "--claim"])
        .assert()
        .success()
        .stdout(predicate::str::contains("updated #12 (claimed)"));
    let calls = h.calls();
    assert!(
        calls.contains("issue edit 12 -R acme/widgets --add-assignee @me"),
        "{calls}"
    );
    assert!(
        calls.contains("--single-select-option-id O_wip"),
        "board → In Progress: {calls}"
    );
    assert!(!calls.contains("--add-label"), "{calls}");
}

#[test]
fn claim_refuses_an_assigned_issue() {
    let h = Harness::new();
    h.on(
        "assignees",
        "issue view 12 -R acme/widgets --json assignees",
        r#"{"assignees":[{"login":"someone"}]}"#,
    );
    h.gbd()
        .args(["update", "12", "--claim"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already assigned to someone"));
    assert!(!h.calls().contains("--add-assignee"), "{}", h.calls());
}

#[test]
fn close_moves_the_board_item_to_done() {
    let h = Harness::new();
    board_fixtures(&h);
    h.on(
        "close",
        "issue close 12 -R acme/widgets --reason completed",
        "",
    );
    h.gbd()
        .args(["close", "12"])
        .assert()
        .success()
        .stdout(predicate::str::contains("closed #12 (completed)"));
    assert!(
        h.calls().contains("--single-select-option-id O_done"),
        "{}",
        h.calls()
    );
}

#[test]
fn update_status_deferred_needs_a_board_or_a_date() {
    let h = Harness::new();
    h.gbd()
        .args(["update", "12", "--status", "deferred"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("gbd defer 12 --until tomorrow"));
}

#[test]
fn update_with_nothing_to_do_fails_fast() {
    let h = Harness::new();
    h.gbd()
        .args(["update", "12"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nothing to update"));
    assert!(
        h.calls().trim().is_empty() || !h.calls().contains("issue edit"),
        "{}",
        h.calls()
    );
}

#[test]
fn remember_sends_the_body_on_stdin_not_a_temp_file() {
    let h = Harness::new();
    h.on(
        "view",
        "issue view 3 -R acme/widgets --json body",
        r#"{"body":"<!-- gbd-memories v1 -->\n\n## auth-jwt\nauth module uses JWT\n"}"#,
    )
    .on("edit", "issue edit 3 -R acme/widgets --body-file -", "");
    h.gbd()
        .args(["remember", "always run tests with the race flag"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "remembered [always-run-tests-with-the-race-flag]",
        ));
    let calls = h.calls();
    assert!(calls.contains("--body-file -"), "{calls}");
    assert!(
        calls.contains("STDIN: <!-- gbd-memories v1 -->"),
        "body travels on stdin: {calls}"
    );
    assert!(calls.contains("## auth-jwt"), "existing keys kept: {calls}");
    assert!(
        calls.contains("## always-run-tests-with-the-race-flag"),
        "{calls}"
    );
    assert!(!calls.contains("gbd-mem-"), "no temp file path: {calls}");
}

#[test]
fn board_lists_the_projects_own_items_including_done() {
    let h = Harness::new();
    board_fixtures(&h);
    h.on(
        "items",
        "items(first: 100",
        r#"{"data":{"node":{"items":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
            {"fieldValueByName":{"name":"Ready"},"content":{"__typename":"Issue","number":10,"title":"child B","repository":{"nameWithOwner":"acme/widgets"}}},
            {"fieldValueByName":{"name":"Done"},"content":{"__typename":"Issue","number":3,"title":"shipped","repository":{"nameWithOwner":"acme/widgets"}}},
            {"fieldValueByName":{"name":"In Progress"},"content":{"__typename":"DraftIssue","title":"a draft"}},
            {"fieldValueByName":{"name":"Ready"},"content":{"__typename":"Issue","number":10,"title":"other repo","repository":{"nameWithOwner":"acme/other"}}}
        ]}}}}"#,
    )
    .on("snapshot", "issues(states: [OPEN]", &search_snapshot_with(NODE_10));
    h.gbd()
        .arg("board")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "widgets board  #7  https://github.com/orgs/acme/projects/7",
        ))
        .stdout(predicate::str::contains(
            "Ready (2)\n  ○ #10 ● P1 child B\n  · acme/other#10 other repo",
        ))
        .stdout(predicate::str::contains(
            "In Progress (1)\n  · a draft (draft)",
        ))
        .stdout(predicate::str::contains("Done (1)\n  ✓ #3 shipped"));
    // Ready before In Progress before Done, regardless of alphabetical order.
    let out = h.gbd().arg("board").output().unwrap().stdout;
    let out = String::from_utf8(out).unwrap();
    let (r, p, d) = (
        out.find("Ready (").unwrap(),
        out.find("In Progress (").unwrap(),
        out.find("Done (").unwrap(),
    );
    assert!(r < p && p < d, "{out}");
}

fn search_snapshot_with(nodes: &str) -> String {
    format!(
        r#"{{"data":{{"repository":{{"issues":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[{nodes}]}}}}}}}}"#
    )
}

#[test]
fn status_counts_come_from_the_snapshot() {
    let h = Harness::new();
    h.on(
        "01-snapshot",
        "issues(states: [OPEN]",
        &fixture("snapshot.json"),
    )
    .on(
        "02-count",
        "first: 1) { issueCount }",
        r#"{"data":{"search":{"issueCount":4}}}"#,
    )
    .on("03-me", "api user --jq .login", "octocat");
    let out = h.gbd().args(["status", "--json"]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // snapshot.json: 6 open issues, #4 assigned to octocat, none blocked.
    assert_eq!(v["open"], 6);
    assert_eq!(v["blocked"], 0);
    assert_eq!(
        v["in_progress"], 1,
        "assigned counts as in progress without a board"
    );
    assert_eq!(v["assigned_to_me"], 1);
    assert_eq!(v["deferred"], 0);
    assert_eq!(v["closed_last_7d"], 4);
    assert!(v["ready"].as_u64().unwrap() >= 1);
    let calls = h.calls();
    assert_eq!(
        calls.matches("api graphql").count(),
        2,
        "one snapshot + one count: {calls}"
    );
}

#[test]
fn board_sync_adds_open_issues_that_have_no_status() {
    let h = Harness::new();
    board_fixtures(&h);
    // #10 already Ready; #9 is on the board with no Status; the rest are absent.
    // Two pages: #10 (Ready) on page one, #9 (no Status) and a foreign #5 on page two.
    h.on(
        "01-items-page2",
        "-F cursor=p2",
        r#"{"data":{"node":{"items":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
            {"fieldValueByName":null,"content":{"__typename":"Issue","number":9,"title":"child A","repository":{"nameWithOwner":"acme/widgets"}}},
            {"fieldValueByName":{"name":"Ready"},"content":{"__typename":"Issue","number":5,"title":"x","repository":{"nameWithOwner":"acme/other"}}}
        ]}}}}"#,
    )
    .on(
        "02-items-page1",
        "items(first: 100",
        r#"{"data":{"node":{"items":{"pageInfo":{"hasNextPage":true,"endCursor":"p2"},"nodes":[
            {"fieldValueByName":{"name":"Ready"},"content":{"__typename":"Issue","number":10,"title":"child B","repository":{"nameWithOwner":"acme/widgets"}}}
        ]}}}}"#,
    )
    .on(
        "snapshot",
        "issues(states: [OPEN]",
        &fixture("snapshot.json"),
    )
    .on(
        "anyadd",
        "project item-add 7 --owner acme --url",
        r#"{"id":"PVTI_new"}"#,
    )
    .on("anyedit", "project item-edit --id PVTI_new", "");
    h.gbd()
        .args(["board", "sync"])
        .assert()
        .success()
        // #4 is assigned in snapshot.json: the pre-board in-progress signal.
        // BTreeMap order puts "In Progress" before "Ready".
        .stdout(predicate::str::contains(
            "added #4 as In Progress, #5 #6 #8 #9 as Ready; 1 unchanged",
        ));
    let calls = h.calls();
    assert_eq!(calls.matches("project item-add").count(), 5, "{calls}");
    assert!(
        !calls.contains("issues/10 --format"),
        "#10 already Ready, untouched: {calls}"
    );
    assert!(
        calls.contains("issues/9 --format"),
        "#9 had no Status, gets Ready: {calls}"
    );
    assert_eq!(
        calls.matches("--single-select-option-id O_ready").count(),
        4,
        "{calls}"
    );
    assert_eq!(
        calls.matches("--single-select-option-id O_wip").count(),
        1,
        "{calls}"
    );
}

#[test]
fn malformed_config_fails_before_any_mutation() {
    let h = Harness::new();
    fs::write(
        h.cwd.path().join(".gbd.yml"),
        "repo: acme/widgets\nmemory_issue: abc\nproject: 7\n",
    )
    .unwrap();
    h.on("close", "issue close 12", "");
    h.gbd()
        .args(["close", "12"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("memory_issue"));
    assert!(
        !h.calls().contains("issue close"),
        "no mutation on a broken config: {}",
        h.calls()
    );
    // doctor reports it instead of crashing.
    h.gbd()
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains("FAIL  .gbd.yml"));
}

#[test]
fn status_change_with_a_board_moves_the_card_only() {
    let h = Harness::new();
    board_fixtures(&h);
    h.gbd()
        .args(["update", "12", "--status", "in_progress"])
        .assert()
        .success();
    let calls = h.calls();
    assert!(calls.contains("--single-select-option-id O_wip"), "{calls}");
    assert!(
        !calls.contains("--add-assignee"),
        "board is authoritative; no assignee edit: {calls}"
    );
    h.on(
        "detail",
        "issue(number: $number)",
        &detail_response(&card_node(12, "OPEN", 0, Some("In Progress"), "")),
    );
    h.gbd()
        .args(["update", "12", "--status", "ready"])
        .assert()
        .success()
        .stdout(predicate::str::contains("updated #12 (status Ready)"));
    let calls = h.calls();
    assert!(
        calls.contains("--single-select-option-id O_ready"),
        "{calls}"
    );
    assert!(!calls.contains("--remove-assignee"), "{calls}");
}

#[test]
fn status_change_without_a_board_uses_the_assignee() {
    let h = Harness::new();
    h.on(
        "assign",
        "issue edit 12 -R acme/widgets --add-assignee @me",
        "",
    );
    h.gbd()
        .args(["update", "12", "--status", "in_progress"])
        .assert()
        .success();
    assert!(h.calls().contains("--add-assignee @me"), "{}", h.calls());
}

#[test]
fn claim_backs_out_when_someone_else_won_the_race() {
    let h = Harness::new();
    board_fixtures(&h);
    h.on_seq(
        "assignees",
        "issue view 12 -R acme/widgets --json assignees",
        &[
            r#"{"assignees":[]}"#,
            r#"{"assignees":[{"login":"otheragent"},{"login":"octocat"}]}"#,
        ],
    )
    .on("me", "api user --jq .login", "octocat")
    .on(
        "assign",
        "issue edit 12 -R acme/widgets --add-assignee @me",
        "",
    )
    .on(
        "unassign",
        "issue edit 12 -R acme/widgets --remove-assignee @me",
        "",
    );
    h.gbd()
        .args(["update", "12", "--claim"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("lost the race to otheragent"));
    let calls = h.calls();
    assert!(
        calls.contains("--remove-assignee @me"),
        "relinquish the losing claim: {calls}"
    );
    assert!(
        !calls.contains("item-edit"),
        "board untouched on a lost claim: {calls}"
    );
}

#[test]
fn malformed_project_number_is_a_config_error() {
    let h = Harness::new();
    fs::write(
        h.cwd.path().join(".gbd.yml"),
        "repo: acme/widgets\nmemory_issue: 3\nproject: abc\n",
    )
    .unwrap();
    h.on("close", "issue close 12", "");
    h.gbd()
        .args(["close", "12"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("project"));
    assert!(!h.calls().contains("issue close"), "{}", h.calls());
}

#[test]
fn claim_runs_before_any_other_edit() {
    let h = Harness::new();
    h.on(
        "assignees",
        "issue view 12 -R acme/widgets --json assignees",
        r#"{"assignees":[{"login":"someone"}]}"#,
    )
    .on("edit", "issue edit 12 -R acme/widgets --title", "");
    h.gbd()
        .args(["update", "12", "--claim", "--title", "stolen"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already assigned to someone"));
    assert!(
        !h.calls().contains("--title"),
        "a refused claim must not edit the issue: {}",
        h.calls()
    );
}

#[test]
fn claim_fails_before_assigning_when_the_board_is_unavailable() {
    let h = Harness::new();
    fs::write(
        h.cwd.path().join(".gbd.yml"),
        "repo: acme/widgets\nmemory_issue: 3\nproject: 7\n",
    )
    .unwrap();
    // No `project view` fixture: the fake gh fails like a token without the scope.
    h.on(
        "assignees",
        "issue view 12 -R acme/widgets --json assignees",
        r#"{"assignees":[]}"#,
    )
    .on(
        "assign",
        "issue edit 12 -R acme/widgets --add-assignee @me",
        "",
    );
    h.gbd().args(["update", "12", "--claim"]).assert().failure();
    assert!(
        !h.calls().contains("--add-assignee"),
        "board is validated before anything is written: {}",
        h.calls()
    );
}

#[test]
fn claim_rolls_back_the_assignee_when_the_card_move_fails() {
    let h = Harness::new();
    board_fixtures(&h);
    h.on_fail(
        "pedit",
        "project item-edit",
        "GraphQL: Resource not accessible",
    )
    .on_seq(
        "assignees",
        "issue view 12 -R acme/widgets --json assignees",
        &[
            r#"{"assignees":[]}"#,
            r#"{"assignees":[{"login":"octocat"}]}"#,
        ],
    )
    .on("me", "api user --jq .login", "octocat")
    .on(
        "assign",
        "issue edit 12 -R acme/widgets --add-assignee @me",
        "",
    )
    .on(
        "unassign",
        "issue edit 12 -R acme/widgets --remove-assignee @me",
        "",
    );
    h.gbd()
        .args(["update", "12", "--claim"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("claim rolled back"));
    let calls = h.calls();
    assert!(calls.contains("--add-assignee @me"), "{calls}");
    assert!(
        calls.contains("--remove-assignee @me"),
        "assignee removed after the board failure: {calls}"
    );
}

const FIELDS_GET: &str = "--method GET -H Accept: application/vnd.github+json -H X-GitHub-Api-Version: 2026-03-10 orgs/acme/issue-fields";
const TYPES_GET: &str = "--method GET -H Accept: application/vnd.github+json -H X-GitHub-Api-Version: 2026-03-10 orgs/acme/issue-types";

#[test]
fn init_creates_the_missing_org_vocabulary() {
    let h = Harness::new();
    // Org has Priority (P0–P4) and gbd Role, but no Start date and only
    // GitHub's three default types.
    h.on(
        "01-fields",
        FIELDS_GET,
        r#"[{"id":1,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},
            {"id":2,"name":"gbd Role","data_type":"single_select","options":[{"id":9,"name":"Memory"}]}]"#,
    )
    .on("02-types", TYPES_GET, r#"[{"name":"Task","is_enabled":true},{"name":"Bug","is_enabled":true},{"name":"Feature","is_enabled":true}]"#)
    .on("03-create-field", "--method POST -H Accept: application/vnd.github+json -H X-GitHub-Api-Version: 2026-03-10 orgs/acme/issue-fields", "{}")
    .on("04-create-type", "--method POST -H Accept: application/vnd.github+json -H X-GitHub-Api-Version: 2026-03-10 orgs/acme/issue-types", "{}");
    h.gbd()
        .args(["init", "--no-memory", "--no-project", "--no-skills"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "issue field Priority: P0, P1, P2, P3, P4",
        ))
        .stdout(predicate::str::contains(
            "issue types: Epic, Feature, Bug, Task, Chore, Decision — created Epic, Chore, Decision",
        ));
    let calls = h.calls();
    assert!(
        calls.contains(r#"STDIN: {"data_type":"date","description":"Date when work on issue will begin","name":"Start date"}"#),
        "Start date created: {calls}"
    );
    assert!(
        calls.contains(r#""name":"Epic""#) && calls.contains(r#""name":"Chore""#),
        "{calls}"
    );
    assert!(
        !calls.contains(r#""name":"Task""#),
        "existing types are not re-created: {calls}"
    );
    assert!(
        !calls.contains("--add-label") && !calls.contains("label create"),
        "{calls}"
    );
}

#[test]
fn doctor_reports_missing_types_and_fields_as_warnings() {
    let h = Harness::new();
    h.on(
        "01-fields",
        FIELDS_GET,
        r#"[{"id":1,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"Urgent"},{"id":2,"name":"High"},{"id":3,"name":"Medium"},{"id":4,"name":"Low"}]}]"#,
    )
    .on("02-types", TYPES_GET, r#"[{"name":"Task","is_enabled":true},{"name":"Epic","is_enabled":false}]"#);
    h.gbd()
        .args(["doctor", "--no-skills"])
        .assert()
        .success() // warnings only; nothing hard-fails
        .stdout(predicate::str::contains(
            "!     priority  issue field Priority options are Urgent, High, Medium, Low, not P0–P4",
        ))
        .stdout(predicate::str::contains(
            "!     role  no gbd Role issue field",
        ))
        .stdout(predicate::str::contains(
            "!     start-date  no Start date issue field",
        ))
        .stdout(predicate::str::contains(
            "!     types  missing issue types Epic, Feature, Bug, Chore, Decision",
        ));
}

#[test]
fn defer_many_with_a_relative_until_and_a_reason() {
    let h = Harness::new();
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    .on("set", "issue-field-values --input -", "{}")
    .on(
        "comment",
        "comment",
        "https://github.com/acme/widgets/issues/9#issuecomment-1",
    );
    h.gbd()
        .args([
            "defer",
            "9",
            "10",
            "--until",
            "tomorrow",
            "--reason",
            "waiting on API access",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::is_match(r"#9 deferred until \d{4}-\d{2}-\d{2}\n#10 deferred until")
                .unwrap(),
        );
    let calls = h.calls();
    assert_eq!(
        calls.matches("issues/9/issue-field-values").count(),
        1,
        "{calls}"
    );
    assert_eq!(
        calls.matches("issues/10/issue-field-values").count(),
        1,
        "{calls}"
    );
    assert!(
        calls.contains(r#"STDIN: {"issue_field_values":[{"field_id":7886557,"value":"20"#),
        "a date, not a label: {calls}"
    );
    assert_eq!(calls.matches("issue comment").count(), 2, "{calls}");
    assert!(calls.contains("--body Deferred until 20"), "{calls}");
    assert!(calls.contains(": waiting on API access"), "{calls}");
    assert!(!calls.contains("--add-label"), "{calls}");
}

#[test]
fn defer_without_a_board_needs_until() {
    let h = Harness::new();
    h.gbd()
        .args(["defer", "9"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--until"));
    assert!(
        h.calls().trim().is_empty() || !h.calls().contains("issue-field-values"),
        "{}",
        h.calls()
    );
}

#[test]
fn defer_with_a_board_and_no_until_only_moves_the_card() {
    let h = Harness::new();
    board_fixtures(&h);
    h.gbd()
        .args(["defer", "12"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#12 deferred\n"));
    let calls = h.calls();
    assert!(calls.contains("--single-select-option-id O_def"), "{calls}");
    assert!(
        !calls.contains("issue-field-values"),
        "no date given, none written: {calls}"
    );
}

#[test]
fn undefer_deletes_the_field_value_and_moves_the_card_to_ready() {
    let h = Harness::new();
    board_fixtures(&h);
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    .on(
        "clear",
        "--method DELETE -H Accept: application/vnd.github+json -H X-GitHub-Api-Version: 2026-03-10 repos/acme/widgets/issues/12/issue-field-values/7886557",
        "",
    )
    .on(
        "detail",
        "issue(number: $number)",
        &detail_response(&card_node(12, "OPEN", 0, Some("Deferred"), "")),
    );
    h.gbd()
        .args(["undefer", "12"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#12 undeferred"));
    let calls = h.calls();
    assert!(calls.contains("--method DELETE"), "{calls}");
    assert!(
        !calls.contains(r#""delete":true"#),
        "the POST form is rejected by GitHub: {calls}"
    );
    assert!(
        calls.contains("--single-select-option-id O_ready"),
        "{calls}"
    );
}

#[test]
fn init_adopts_a_board_already_linked_to_the_repo() {
    let h = Harness::new();
    // .gbd.yml has no project:. A "widgets board" is already linked to the repo.
    fs::write(
        h.cwd.path().join(".gbd.yml"),
        "repo: acme/widgets\nmemory_issue: 3\n",
    )
    .unwrap();
    h.on(
        "01-fields",
        FIELDS_GET,
        r#"[{"id":1,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},
            {"id":2,"name":"gbd Role","data_type":"single_select","options":[{"id":9,"name":"Memory"}]},
            {"id":3,"name":"Start date","data_type":"date"}]"#,
    )
    .on("02-types", TYPES_GET, r#"[{"name":"Epic"},{"name":"Feature"},{"name":"Bug"},{"name":"Task"},{"name":"Chore"},{"name":"Decision"}]"#)
    .on(
        "03-linked",
        "projectsV2(first: 100",
        r#"{"data":{"repository":{"projectsV2":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
            {"number":4,"title":"Content Planning","closed":false},
            {"number":8,"title":"widgets board","closed":false}]}}}}"#,
    )
    .on("04-pview", "project view 8 --owner acme --format json",
        r#"{"id":"PVT_8","number":8,"title":"widgets board","url":"https://github.com/orgs/acme/projects/8"}"#)
    .on("05-pfields", "project field-list 8 --owner acme --format json",
        r#"{"fields":[{"id":"F_status","name":"Status","options":[
            {"id":"O_blocked","name":"Blocked"},{"id":"O_def","name":"Deferred"},{"id":"O_ready","name":"Ready"},
            {"id":"O_wip","name":"In Progress"},{"id":"O_done","name":"Done"}]}]}"#)
    .on("06-options", "updateProjectV2Field", r#"{"data":{"updateProjectV2Field":{"projectV2Field":{"id":"F_status"}}}}"#)
    // The stock view on the first run; the shaped one from then on.
    .on("08-views", "views(first: 50",
        r#"{"data":{"node":{"views":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"id":"PVTV_1","name":"Board","layout":"BOARD_LAYOUT","filter":"-type:Epic"}]}}}}"#)
    .on("09-view", "updateProjectV2View", r#"{"data":{"updateProjectV2View":{"projectV2View":{"id":"PVTV_1"}}}}"#)
    .on("07-details", "options { id name color description }",
        r#"{"data":{"node":{"options":[
            {"id":"O_ready","name":"Ready","color":"GREEN","description":""},
            {"id":"O_wip","name":"In Progress","color":"BLUE","description":"hands on"},
            {"id":"O_def","name":"Deferred","color":"GRAY","description":null},
            {"id":"O_done","name":"Done","color":"PURPLE","description":""}]}}}"#);
    // The board predates Blocked and the column order: the first read shows
    // four options in GitHub's order.
    fs::write(
        h.gh_dir.path().join("05-pfields.out.1"),
        r#"{"fields":[{"id":"F_status","name":"Status","options":[
            {"id":"O_ready","name":"Ready"},{"id":"O_wip","name":"In Progress"},
            {"id":"O_def","name":"Deferred"},{"id":"O_done","name":"Done"}]}]}"#,
    )
    .unwrap();
    fs::write(
        h.gh_dir.path().join("08-views.out.1"),
        r#"{"data":{"node":{"views":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"id":"PVTV_1","name":"View 1","layout":"TABLE_LAYOUT","filter":null}]}}}}"#,
    )
    .unwrap();
    h.gbd()
        .args(["init", "--no-memory", "--no-skills"])
        .assert()
        .success()
        .stdout(predicate::str::contains("board: #8 widgets board"))
        .stdout(predicate::str::contains(
            "view: Board (board layout, -type:Epic)",
        ));
    let calls = h.calls();
    assert!(
        !calls.contains("project create"),
        "must adopt, not create a second board: {calls}"
    );
    assert!(!calls.contains("project link"), "{calls}");
    // Existing options go back with their ids (without them GitHub treats
    // the list as new options and clears every card's Status) and with the
    // board's own colors and descriptions, not gbd's defaults.
    assert!(
        calls.contains(
            r#"{name: "Blocked", color: RED, description: ""}, {id: "O_def", name: "Deferred", color: GRAY, description: ""}, {id: "O_ready", name: "Ready", color: GREEN, description: ""}, {id: "O_wip", name: "In Progress", color: BLUE, description: "hands on"}, {id: "O_done", name: "Done", color: PURPLE, description: ""}"#
        ),
        "Blocked added and the five put in column order, existing options kept: {calls}"
    );
    assert!(
        calls.contains("updateProjectV2View")
            && calls.contains("-F name=Board")
            && calls.contains("-F filter=-type:Epic"),
        "the stock view is shaped: {calls}"
    );
    let cfg = fs::read_to_string(h.cwd.path().join(".gbd.yml")).unwrap();
    assert!(cfg.contains("project: 8\n"), "{cfg}");
    // Second run: same result, still no create.
    h.gbd()
        .args(["init", "--no-memory", "--no-skills"])
        .assert()
        .success();
    assert!(!h.calls().contains("project create"), "{}", h.calls());
    assert_eq!(
        h.calls().matches("updateProjectV2Field").count(),
        1,
        "options are complete and in order after the first run: {}",
        h.calls()
    );
    assert_eq!(
        h.calls().matches("updateProjectV2View").count(),
        1,
        "the view is shaped once: {}",
        h.calls()
    );
}

#[test]
fn init_and_doctor_leave_a_hand_shaped_view_alone() {
    let h = Harness::new();
    board_fixtures(&h);
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":1,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},
            {"id":2,"name":"gbd Role","data_type":"single_select","options":[{"id":9,"name":"Memory"}]},
            {"id":3,"name":"Start date","data_type":"date"}]"#,
    )
    .on("types", TYPES_GET, r#"[{"name":"Epic"},{"name":"Feature"},{"name":"Bug"},{"name":"Task"},{"name":"Chore"},{"name":"Decision"}]"#)
    .on(
        "views",
        "views(first: 50",
        r#"{"data":{"node":{"views":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
            {"id":"PVTV_1","name":"Sprint","layout":"BOARD_LAYOUT","filter":"is:open"},
            {"id":"PVTV_2","name":"View 1","layout":"TABLE_LAYOUT","filter":null,"sortByFields":{"totalCount":1}}]}}}}"#,
    );
    h.gbd()
        .args(["init", "--no-memory", "--no-skills"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "view: left as is (Sprint (board, is:open), View 1 (table)). By hand: name Board, board layout, filter -type:Epic",
        ));
    let calls = h.calls();
    // The stock-named table is sorted by someone: shaped, so not rewritten.
    assert!(!calls.contains("updateProjectV2View"), "{calls}");
    assert!(
        !calls.contains("updateProjectV2Field"),
        "options already complete and in order: {calls}"
    );
    h.gbd()
        .args(["doctor", "--no-skills"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ok    board  #7 widgets board"))
        .stdout(predicate::str::contains(
            "!     view  Sprint (board, is:open), View 1 (table); a hand-shaped view is left alone.",
        ));
}

#[test]
fn claim_with_status_is_rejected_before_any_call() {
    let h = Harness::new();
    h.gbd()
        .args([
            "update", "12", "--claim", "--status", "ready", "--title", "x",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("drop --status"));
    assert!(
        !h.calls().contains("issue "),
        "no issue call at all: {}",
        h.calls()
    );
}

#[test]
fn close_loads_the_board_before_closing() {
    let h = Harness::new();
    fs::write(
        h.cwd.path().join(".gbd.yml"),
        "repo: acme/widgets\nmemory_issue: 3\nproject: 7\n",
    )
    .unwrap();
    // No `project view` fixture: the board cannot be loaded.
    h.on("close", "issue close 12", "");
    h.gbd().args(["close", "12"]).assert().failure();
    assert!(
        !h.calls().contains("issue close"),
        "issue must not be closed when the board is unavailable: {}",
        h.calls()
    );
}

#[test]
fn a_start_date_field_of_the_wrong_type_is_rejected() {
    let h = Harness::new();
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":5,"name":"Start date","data_type":"text"},
            {"id":1,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]}]"#,
    )
    .on("set", "issue-field-values --input -", "{}")
    .on("types", TYPES_GET, "[]");
    h.gbd()
        .args(["defer", "9", "--until", "tomorrow"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "of type text, but gbd needs a date field",
        ));
    assert!(!h.calls().contains("issue-field-values"), "{}", h.calls());
    h.gbd()
        .args(["doctor", "--no-skills"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "of type text, but gbd needs a date field",
        ));
}

#[test]
fn init_pages_linked_projects_before_deciding_to_create() {
    let h = Harness::new();
    fs::write(
        h.cwd.path().join(".gbd.yml"),
        "repo: acme/widgets\nmemory_issue: 3\n",
    )
    .unwrap();
    h.on(
        "01-fields",
        FIELDS_GET,
        r#"[{"id":1,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},
            {"id":2,"name":"gbd Role","data_type":"single_select","options":[{"id":9,"name":"Memory"}]},
            {"id":3,"name":"Start date","data_type":"date"}]"#,
    )
    .on("02-types", TYPES_GET, r#"[{"name":"Epic"},{"name":"Feature"},{"name":"Bug"},{"name":"Task"},{"name":"Chore"},{"name":"Decision"}]"#)
    // Page two (matched first because the call carries the cursor) holds the board.
    .on(
        "03-linked-page2",
        "-F cursor=abc",
        r#"{"data":{"repository":{"projectsV2":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
            {"number":8,"title":"widgets board","closed":false}]}}}}"#,
    )
    .on(
        "04-linked-page1",
        "projectsV2(first: 100",
        r#"{"data":{"repository":{"projectsV2":{"pageInfo":{"hasNextPage":true,"endCursor":"abc"},"nodes":[
            {"number":4,"title":"Content Planning","closed":false}]}}}}"#,
    )
    .on("05-pview", "project view 8 --owner acme --format json",
        r#"{"id":"PVT_8","number":8,"title":"widgets board","url":"https://github.com/orgs/acme/projects/8"}"#)
    .on("06-pfields", "project field-list 8 --owner acme --format json",
        r#"{"fields":[{"id":"F_status","name":"Status","options":[
            {"id":"O_blocked","name":"Blocked"},{"id":"O_def","name":"Deferred"},{"id":"O_ready","name":"Ready"},
            {"id":"O_wip","name":"In Progress"},{"id":"O_done","name":"Done"}]}]}"#);
    h.gbd()
        .args(["init", "--no-memory", "--no-skills"])
        .assert()
        .success()
        .stdout(predicate::str::contains("board: #8 widgets board"));
    let calls = h.calls();
    assert_eq!(
        calls.matches("projectsV2(first: 100").count(),
        2,
        "two pages fetched: {calls}"
    );
    assert!(!calls.contains("project create"), "{calls}");
}

#[test]
fn an_old_gh_is_refused_before_any_other_call() {
    let h = Harness::new();
    h.on("list", "search(query: $q", "{}");
    h.gbd()
        .env("FAKE_GH_VERSION", "2.93.2")
        .arg("list")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "gh 2.93.2 is too old; gbd needs 2.94.0 or newer",
        ));
    assert!(!h.calls().contains("api graphql"), "{}", h.calls());
    h.gbd()
        .env("FAKE_GH_VERSION", "2.93.2")
        .arg("doctor")
        .assert()
        .failure()
        .stdout(predicate::str::contains("FAIL  gh  gh 2.93.2 is too old"));
    h.gbd()
        .env("FAKE_GH_VERSION", "2.94.0")
        .arg("ping")
        .assert()
        .success()
        .stdout(predicate::str::contains("gh: 2.94.0"));
}

// ---------------------------------------------------------------------------
// Blocked on the board (#19)

/// A row node for `acme/widgets#number`: `open_blockers` is GitHub's own
/// count, `status` its card on board #7 (None: not on the board), and
/// `blocking` the JSON nodes of the issues it blocks.
fn card_node(
    number: u64,
    state: &str,
    open_blockers: u32,
    status: Option<&str>,
    blocking: &str,
) -> String {
    let item = status.map_or(String::new(), |s| {
        format!(
            r#"{{"project":{{"number":7,"owner":{{"login":"acme"}}}},"fieldValueByName":{{"name":"{s}"}}}}"#
        )
    });
    format!(
        r#"{{"id":"I{number}","number":{number},"title":"issue {number}","url":"https://github.com/acme/widgets/issues/{number}","state":"{state}",
          "issueType":{{"name":"Task"}},"assignees":{{"nodes":[]}},"parent":null,
          "issueDependenciesSummary":{{"blockedBy":{open_blockers},"blocking":0}},
          "blockedBy":{{"nodes":[]}},"blocking":{{"nodes":[{blocking}]}},
          "issueFieldValues":{{"nodes":[]}},
          "projectItems":{{"pageInfo":{{"hasNextPage":false}},"nodes":[{item}]}}}}"#
    )
}

fn detail_response(node: &str) -> String {
    format!(r#"{{"data":{{"repository":{{"issue":{node}}}}}}}"#)
}

#[test]
fn create_with_open_deps_is_added_to_the_board_as_blocked() {
    let h = Harness::new();
    board_fixtures(&h);
    h.on(
        "create",
        "issue create -R acme/widgets",
        "https://github.com/acme/widgets/issues/42",
    )
    .on(
        "detail",
        "issue(number: $number)",
        &detail_response(&card_node(42, "OPEN", 2, None, "")),
    )
    .on(
        "padd42",
        "project item-add 7 --owner acme --url https://github.com/acme/widgets/issues/42 --format json",
        r#"{"id":"PVTI_42"}"#,
    )
    .on("pedit42", "project item-edit --id PVTI_42", "");
    h.gbd()
        .args(["create", "Wire the board", "-t", "Task", "--deps", "12,13"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Status Blocked"));
    let calls = h.calls();
    assert!(calls.contains("--blocked-by 12,13"), "{calls}");
    assert!(
        calls.contains("--single-select-option-id O_blocked"),
        "{calls}"
    );
}

#[test]
fn create_without_deps_is_ready_and_does_not_fetch() {
    let h = Harness::new();
    board_fixtures(&h);
    h.on(
        "create",
        "issue create -R acme/widgets",
        "https://github.com/acme/widgets/issues/42",
    )
    .on(
        "padd42",
        "project item-add 7 --owner acme --url https://github.com/acme/widgets/issues/42 --format json",
        r#"{"id":"PVTI_42"}"#,
    )
    .on("pedit42", "project item-edit --id PVTI_42", "");
    h.gbd()
        .args(["create", "Wire the board", "-t", "Task"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Status Ready"));
    let calls = h.calls();
    assert!(
        !calls.contains("issue(number"),
        "no fetch without deps: {calls}"
    );
    assert!(
        calls.contains("--single-select-option-id O_ready"),
        "{calls}"
    );
}

#[test]
fn dep_add_moves_a_ready_card_to_blocked() {
    let h = Harness::new();
    board_fixtures(&h);
    h.on(
        "edit",
        "issue edit 12 -R acme/widgets --add-blocked-by 9",
        "",
    )
    .on(
        "detail",
        "issue(number: $number)",
        &detail_response(&card_node(12, "OPEN", 1, Some("Ready"), "")),
    );
    h.gbd()
        .args(["dep", "add", "12", "9"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "#12 is blocked by #9; #12 → Blocked",
        ));
    let calls = h.calls();
    let edit = calls.find("--add-blocked-by 9").expect(&calls);
    let card = calls
        .find("--single-select-option-id O_blocked")
        .expect(&calls);
    assert!(
        edit < card,
        "the dependency lands before the card moves: {calls}"
    );
}

#[test]
fn dep_add_leaves_an_in_progress_card_alone() {
    let h = Harness::new();
    board_fixtures(&h);
    h.on(
        "edit",
        "issue edit 12 -R acme/widgets --add-blocked-by 9",
        "",
    )
    .on(
        "detail",
        "issue(number: $number)",
        &detail_response(&card_node(12, "OPEN", 1, Some("In Progress"), "")),
    );
    h.gbd()
        .args(["dep", "add", "12", "9"])
        .assert()
        .success()
        .stdout(predicate::str::contains("#12 is blocked by #9\n"));
    let calls = h.calls();
    assert!(calls.contains("--add-blocked-by 9"), "{calls}");
    assert!(
        !calls.contains("project item-"),
        "someone's decision, not touched: {calls}"
    );
}

#[test]
fn dep_remove_moves_a_blocked_card_back_to_ready() {
    let h = Harness::new();
    board_fixtures(&h);
    h.on(
        "edit",
        "issue edit 12 -R acme/widgets --remove-blocked-by 9",
        "",
    )
    .on(
        "detail",
        "issue(number: $number)",
        &detail_response(&card_node(12, "OPEN", 0, Some("Blocked"), "")),
    );
    h.gbd()
        .args(["dep", "remove", "12", "9"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "#12 no longer blocked by #9; #12 → Ready",
        ));
    assert!(
        h.calls().contains("--single-select-option-id O_ready"),
        "{}",
        h.calls()
    );
}

#[test]
fn close_frees_the_blocked_cards_it_was_holding() {
    let h = Harness::new();
    board_fixtures(&h);
    // #14: last blocker gone, Blocked → Ready. #15: another blocker is
    // still open. #16: In Progress is someone's decision. #17: closed.
    let blocking = [
        card_node(14, "OPEN", 0, Some("Blocked"), ""),
        card_node(15, "OPEN", 1, Some("Blocked"), ""),
        card_node(16, "OPEN", 0, Some("In Progress"), ""),
        card_node(17, "CLOSED", 0, Some("Blocked"), ""),
    ]
    .join(",");
    h.on(
        "close",
        "issue close 12 -R acme/widgets --reason completed",
        "",
    )
    .on(
        "detail",
        "issue(number: $number)",
        &detail_response(&card_node(12, "CLOSED", 0, Some("Done"), &blocking)),
    )
    .on(
        "padd14",
        "project item-add 7 --owner acme --url https://github.com/acme/widgets/issues/14 --format json",
        r#"{"id":"PVTI_14"}"#,
    )
    .on("pedit14", "project item-edit --id PVTI_14", "");
    h.gbd()
        .args(["close", "12"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "closed #12 (completed); #14 → Ready",
        ));
    let calls = h.calls();
    assert!(
        calls.contains(
            "--id PVTI_12 --project-id PVT_1 --field-id F_status --single-select-option-id O_done"
        ),
        "{calls}"
    );
    assert!(
        calls.contains(
            "--id PVTI_14 --project-id PVT_1 --field-id F_status --single-select-option-id O_ready"
        ),
        "{calls}"
    );
    for untouched in ["issues/15 ", "issues/16 ", "issues/17 "] {
        assert!(!calls.contains(untouched), "{untouched} untouched: {calls}");
    }
    assert_eq!(
        calls.matches("api graphql").count(),
        1,
        "one fetch: {calls}"
    );

    let out = h.gbd().args(["close", "12", "--json"]).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["moved"],
        serde_json::json!([{ "number": 14, "status": "Ready" }])
    );
}

#[test]
fn board_sync_reconciles_ready_and_blocked_cards() {
    let h = Harness::new();
    board_fixtures(&h);
    // On the board: #10 Ready but blocked, #9 Blocked but free, #8 In
    // Progress and blocked, #6 Blocked and blocked. Off the board: #7, blocked.
    h.on(
        "items",
        "items(first: 100",
        r#"{"data":{"node":{"items":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[
            {"fieldValueByName":{"name":"Ready"},"content":{"__typename":"Issue","number":10,"title":"a","repository":{"nameWithOwner":"acme/widgets"}}},
            {"fieldValueByName":{"name":"Blocked"},"content":{"__typename":"Issue","number":9,"title":"b","repository":{"nameWithOwner":"acme/widgets"}}},
            {"fieldValueByName":{"name":"In Progress"},"content":{"__typename":"Issue","number":8,"title":"c","repository":{"nameWithOwner":"acme/widgets"}}},
            {"fieldValueByName":{"name":"Blocked"},"content":{"__typename":"Issue","number":6,"title":"d","repository":{"nameWithOwner":"acme/widgets"}}}
        ]}}}}"#,
    )
    .on(
        "snapshot",
        "issues(states: [OPEN]",
        &search_snapshot_with(
            &[
                card_node(10, "OPEN", 1, Some("Ready"), ""),
                card_node(9, "OPEN", 0, Some("Blocked"), ""),
                card_node(8, "OPEN", 1, Some("In Progress"), ""),
                card_node(6, "OPEN", 1, Some("Blocked"), ""),
                card_node(7, "OPEN", 1, None, ""),
            ]
            .join(","),
        ),
    )
    .on(
        "anyadd",
        "project item-add 7 --owner acme --url",
        r#"{"id":"PVTI_new"}"#,
    )
    .on("anyedit", "project item-edit --id PVTI_new", "");
    h.gbd()
        .args(["board", "sync"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "board #7: added #7 as Blocked; moved #10 to Blocked, #9 to Ready; 2 unchanged",
        ));
    let calls = h.calls();
    assert_eq!(calls.matches("project item-add").count(), 3, "{calls}");
    assert_eq!(
        calls.matches("--single-select-option-id O_blocked").count(),
        2,
        "#7 and #10: {calls}"
    );
    assert_eq!(
        calls.matches("--single-select-option-id O_ready").count(),
        1,
        "#9: {calls}"
    );
    for untouched in ["issues/8 ", "issues/6 "] {
        assert!(!calls.contains(untouched), "{untouched} untouched: {calls}");
    }
}

#[test]
fn update_status_blocked_is_refused() {
    let h = Harness::new();
    board_fixtures(&h);
    h.gbd()
        .args(["update", "12", "--status", "blocked"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("blocked is not set by hand"));
    let calls = h.calls();
    assert!(
        !calls.contains("issue edit") && !calls.contains("project"),
        "refused before any change: {calls}"
    );
}

#[test]
fn doctor_flags_a_board_that_predates_blocked() {
    let h = Harness::new();
    board_fixtures(&h);
    fs::write(
        h.gh_dir.path().join("pfields.out"),
        r#"{"fields":[{"id":"F_status","name":"Status","options":[
            {"id":"O_ready","name":"Ready"},{"id":"O_wip","name":"In Progress"},
            {"id":"O_def","name":"Deferred"},{"id":"O_done","name":"Done"}]}]}"#,
    )
    .unwrap();
    h.on("fields", FIELDS_GET, "[]")
        .on("types", TYPES_GET, "[]");
    h.gbd()
        .args(["doctor", "--no-skills"])
        .assert()
        .stdout(predicate::str::contains(
            "!     board  #7 missing Status options Blocked. Run: gbd init",
        ));
}

#[test]
fn create_with_blocking_moves_the_cards_it_blocks() {
    let h = Harness::new();
    board_fixtures(&h);
    // #42 has no blockers itself (Ready); #12, which it now blocks, was Ready.
    h.on(
        "create",
        "issue create -R acme/widgets",
        "https://github.com/acme/widgets/issues/42",
    )
    .on(
        "detail",
        "issue(number: $number)",
        &detail_response(&card_node(
            42,
            "OPEN",
            0,
            None,
            &card_node(12, "OPEN", 1, Some("Ready"), ""),
        )),
    )
    .on(
        "padd42",
        "project item-add 7 --owner acme --url https://github.com/acme/widgets/issues/42 --format json",
        r#"{"id":"PVTI_42"}"#,
    )
    .on("pedit42", "project item-edit --id PVTI_42", "");
    h.gbd()
        .args(["create", "Ship first", "-t", "Task", "--blocking", "12"])
        .assert()
        .success()
        .stdout(predicate::str::contains("(Status Ready); #12 → Blocked"));
    let calls = h.calls();
    assert!(
        calls.contains(
            "--id PVTI_42 --project-id PVT_1 --field-id F_status --single-select-option-id O_ready"
        ),
        "{calls}"
    );
    assert!(
        calls.contains("--id PVTI_12 --project-id PVT_1 --field-id F_status --single-select-option-id O_blocked"),
        "{calls}"
    );
}

#[test]
fn status_ready_lands_on_blocked_while_a_blocker_is_open() {
    let h = Harness::new();
    board_fixtures(&h);
    h.on(
        "detail",
        "issue(number: $number)",
        &detail_response(&card_node(12, "OPEN", 1, Some("Deferred"), "")),
    );
    h.gbd()
        .args(["update", "12", "--status", "ready"])
        .assert()
        .success()
        .stdout(predicate::str::contains("updated #12 (status Blocked)"));
    let calls = h.calls();
    assert!(
        calls.contains("--single-select-option-id O_blocked"),
        "{calls}"
    );
    assert!(!calls.contains("O_ready"), "{calls}");
}

#[test]
fn reopen_lands_on_blocked_and_reblocks_what_it_holds() {
    let h = Harness::new();
    board_fixtures(&h);
    // #12 still has an open blocker; #14, which it blocks, went Ready when
    // #12 was closed.
    h.on(
        "detail",
        "issue(number: $number)",
        &detail_response(&card_node(
            12,
            "CLOSED",
            1,
            Some("Done"),
            &card_node(14, "OPEN", 1, Some("Ready"), ""),
        )),
    )
    .on("reopen", "issue reopen 12 -R acme/widgets", "")
    .on(
        "padd14",
        "project item-add 7 --owner acme --url https://github.com/acme/widgets/issues/14 --format json",
        r#"{"id":"PVTI_14"}"#,
    )
    .on("pedit14", "project item-edit --id PVTI_14", "");
    h.gbd()
        .args(["reopen", "12"])
        .assert()
        .success()
        .stdout(predicate::str::contains("reopened #12; #14 → Blocked"));
    let calls = h.calls();
    assert!(
        calls.contains("--id PVTI_12 --project-id PVT_1 --field-id F_status --single-select-option-id O_blocked"),
        "{calls}"
    );
    assert!(
        calls.contains("--id PVTI_14 --project-id PVT_1 --field-id F_status --single-select-option-id O_blocked"),
        "{calls}"
    );
    let card = calls.find("PVTI_12").expect(&calls);
    let reopen = calls.find("issue reopen").expect(&calls);
    assert!(card < reopen, "card first, then the issue: {calls}");
}

#[test]
fn delete_frees_the_cards_it_was_holding() {
    let h = Harness::new();
    board_fixtures(&h);
    // #14 had only #12 as a blocker; #15 has another one.
    let blocking = [
        card_node(14, "OPEN", 1, Some("Blocked"), ""),
        card_node(15, "OPEN", 2, Some("Blocked"), ""),
    ]
    .join(",");
    h.on(
        "detail",
        "issue(number: $number)",
        &detail_response(&card_node(12, "OPEN", 0, Some("Ready"), &blocking)),
    )
    .on("delete", "issue delete 12 -R acme/widgets --yes", "")
    .on(
        "padd14",
        "project item-add 7 --owner acme --url https://github.com/acme/widgets/issues/14 --format json",
        r#"{"id":"PVTI_14"}"#,
    )
    .on("pedit14", "project item-edit --id PVTI_14", "");
    h.gbd()
        .args(["delete", "12", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("deleted #12; #14 → Ready"));
    let calls = h.calls();
    let fetch = calls.find("issue(number").expect(&calls);
    let delete = calls.find("issue delete").expect(&calls);
    assert!(
        fetch < delete,
        "blockees are read before the issue is gone: {calls}"
    );
    assert!(
        calls.contains(
            "--id PVTI_14 --project-id PVT_1 --field-id F_status --single-select-option-id O_ready"
        ),
        "{calls}"
    );
    assert!(
        !calls.contains("issues/15 "),
        "#15 is still blocked: {calls}"
    );
}

#[test]
fn import_dry_run_prints_the_plan_and_calls_nothing() {
    let h = Harness::new();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-export.jsonl");
    h.gbd()
        .args([
            "import",
            "--from-beads",
            fixture.to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Import plan: 8 issues, 1 memories",
        ))
        .stdout(predicate::str::contains("Nothing written (--dry-run)."));
    assert!(
        h.calls().is_empty(),
        "a dry run is local: not even gh --version or auth: {}",
        h.calls()
    );
    // Without --dry-run the real run needs --yes; nothing is written first.
    board_fixtures(&h);
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Add --yes, or --dry-run"));
    assert!(!h.calls().contains("issue create"), "{}", h.calls());
}

#[test]
fn import_creates_issues_in_dependency_order_through_the_create_path() {
    let h = Harness::new();
    board_fixtures(&h);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-small.jsonl");
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},
            {"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    .on("create-1", "--title Widget API v2", "https://github.com/acme/widgets/issues/101")
    .on("create-2", "--title Auth refresh drops the session", "https://github.com/acme/widgets/issues/102")
    .on("create-3", "--title Rename the endpoints", "https://github.com/acme/widgets/issues/103")
    .on("values", "issue-field-values --input -", "{}")
    .on("assign", "issue edit 102 -R acme/widgets --add-assignee dev1", "")
    .on("close", "issue close 102 -R acme/widgets --reason duplicate", "")
    .on("comment", "issue comment 103 -R acme/widgets --body-file -", "")
    .on("anyadd", "project item-add 7 --owner acme --url", r#"{"id":"PVTI_new"}"#)
    .on("anyedit", "project item-edit --id PVTI_new", "")
    .on("mem-view", "issue view 3 -R acme/widgets --json body", "{\"body\":\"## old-key\\n\\nstill here\\n\"}")
    .on("mem-save", "issue edit 3 -R acme/widgets --body-file -", "")
    .on("edit-101", "issue edit 101 -R acme/widgets --body-file -", "");

    // Refuses without --yes, before any write.
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "this creates 3 issues in acme/widgets and writes 1 memories. Add --yes",
        ));
    assert!(!h.calls().contains("issue create"), "{}", h.calls());

    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "1/3  wx-1 → #101  [Epic] Widget API v2\n",
        ))
        .stdout(predicate::str::contains(
            "2/3  wx-2 → #102  [Bug] Auth refresh drops the session  ✓\n",
        ))
        .stdout(predicate::str::contains(
            "3/3  wx-1.1 → #103  [Task] Rename the endpoints\n",
        ))
        .stdout(predicate::str::contains(
            "imported 3 issues (1 closed); memories: 1 new, 0 updated on #3; 0 warnings",
        ));
    let calls = h.calls();
    let at = |s: &str| {
        calls
            .find(s)
            .unwrap_or_else(|| panic!("{s} not called:\n{calls}"))
    };
    // Creation order follows the edges; each create carries its edges.
    assert!(
        at("--title Widget API v2") < at("--title Auth refresh")
            && at("--title Auth refresh") < at("--title Rename"),
        "{calls}"
    );
    assert!(
        calls.contains("--title Rename the endpoints --body Child of the epic; see #102 and wx-9."),
        "a mention of a bead created earlier in the run is rewritten; an unknown one is kept: {calls}"
    );
    assert!(
        calls.contains("--type Task --parent 101 --blocked-by 102"),
        "{calls}"
    );
    assert!(
        calls.contains("--type Epic\n"),
        "epic has no edges: {calls}"
    );
    assert!(
        !calls.contains("--label"),
        "nothing is ever a label: {calls}"
    );
    // Org fields: one lookup per field for the whole run, then plain writes.
    assert_eq!(
        calls.matches(FIELDS_GET).count(),
        2,
        "field ids are cached: {calls}"
    );
    assert!(
        calls.contains(r#"STDIN: {"issue_field_values":[{"field_id":46822523,"value":"P1"}]}"#),
        "{calls}"
    );
    assert!(
        calls.contains(r#"STDIN: {"issue_field_values":[{"field_id":46822523,"value":"P0"}]}"#),
        "{calls}"
    );
    assert!(
        calls.contains(
            r#"STDIN: {"issue_field_values":[{"field_id":7886557,"value":"2026-10-01"}]}"#
        ),
        "{calls}"
    );
    // Closed bead: assignee, close with the guessed reason, then Done.
    assert!(at("--add-assignee dev1") < at("issue close 102"), "{calls}");
    assert!(
        at("issue close 102") < at("--single-select-option-id O_done"),
        "{calls}"
    );
    // Cards: Deferred (defer_until), Done, Ready (its only blocker is closed).
    for opt in ["O_def", "O_done", "O_ready"] {
        assert_eq!(
            calls
                .matches(&format!("--single-select-option-id {opt}"))
                .count(),
            1,
            "{opt}: {calls}"
        );
    }
    assert!(!calls.contains("O_blocked"), "{calls}");
    // Notes and comments arrive on stdin, notes first.
    assert!(
        at("STDIN: **Notes**\n\nKeep the old routes")
            < at("STDIN: **dev2** · 2026-03-03\n\nAlso drop the v1 docs."),
        "{calls}"
    );
    assert_eq!(calls.matches("issue comment 103").count(), 2, "{calls}");
    // wx-1 mentioned wx-2 before it existed: its body is edited once at the end.
    let fixed = calls
        .rsplit("issue edit 101 -R acme/widgets --body-file -\nSTDIN: ")
        .next()
        .unwrap();
    assert!(
        fixed.starts_with("Umbrella; the auth fix is #102. Tracked as #101."),
        "a self-reference is rewritten too: {fixed}"
    );
    assert!(
        !calls.contains("issue edit 103 -R acme/widgets --body-file"),
        "a backward reference needs no edit: {calls}"
    );
    // Memories merge into the existing body.
    let saved = calls
        .rsplit("issue edit 3 -R acme/widgets --body-file -\nSTDIN: ")
        .next()
        .unwrap();
    assert!(
        saved.contains("## deploy-runbook") && saved.contains("## old-key"),
        "{saved}"
    );
}

#[test]
fn import_needs_a_board_before_it_creates_anything() {
    let h = Harness::new(); // .gbd.yml without project:
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-small.jsonl");
    h.on("assignable", "repos/acme/widgets/assignees/", "")
        .on("types", TYPES_GET, r#"[{"name":"Epic"},{"name":"Feature"},{"name":"Bug"},{"name":"Task"},{"name":"Chore"},{"name":"Decision"}]"#);
    h.on(
        "create",
        "issue create -R acme/widgets",
        "https://github.com/acme/widgets/issues/1",
    );
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "needs a board so In Progress and Deferred survive",
        ));
    assert!(!h.calls().contains("issue create"), "{}", h.calls());
}

#[test]
fn import_reports_what_it_could_not_map_and_a_failed_close() {
    let h = Harness::new();
    board_fixtures(&h);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-small.jsonl");
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},
            {"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    .on("create-1", "--title Widget API v2", "https://github.com/acme/widgets/issues/101")
    .on("create-2", "--title Auth refresh drops the session", "https://github.com/acme/widgets/issues/102")
    .on("create-3", "--title Rename the endpoints", "https://github.com/acme/widgets/issues/103")
    .on("values", "issue-field-values --input -", "{}")
    .on("assign", "issue edit 102 -R acme/widgets --add-assignee dev1", "")
    .on_fail("close", "issue close 102 -R acme/widgets --reason duplicate", "HTTP 502")
    .on("comment", "issue comment 103 -R acme/widgets --body-file -", "")
    .on("anyadd", "project item-add 7 --owner acme --url", r#"{"id":"PVTI_new"}"#)
    .on("anyedit", "project item-edit --id PVTI_new", "")
    .on("mem-view", "issue view 3 -R acme/widgets --json body", "{\"body\":\"\"}")
    .on("mem-save", "issue edit 3 -R acme/widgets --body-file -", "")
    .on("edit-101", "issue edit 101 -R acme/widgets --body-file -", "");
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        // The plan's losses are shown before anything is created.
        .stdout(predicate::str::contains("related edge").not())
        .stdout(predicate::str::contains(
            "2/3  wx-2 → #102  [Bug] Auth refresh drops the session\n",
        ))
        .stdout(predicate::str::contains("imported 3 issues (0 closed)"))
        .stdout(predicate::str::contains(
            "2 warnings; 0 could not map (listed above)",
        ))
        .stderr(predicate::str::contains("wx-2 (#102): not closed: "))
        .stderr(predicate::str::contains("Run gbd import again to retry"));
    let calls = h.calls();
    assert!(
        !calls.contains("O_done"),
        "an open issue is never marked Done: {calls}"
    );
    assert!(!calls.contains("✓"), "{calls}");
    // wx-1.1 is blocked by wx-2, which is still open: Blocked, not the planned Ready.
    assert!(
        calls.contains("--single-select-option-id O_blocked"),
        "{calls}"
    );
    assert!(
        !calls.contains("--single-select-option-id O_ready"),
        "{calls}"
    );
    let map = fs::read_to_string(h.cwd.path().join("beads-map.jsonl")).unwrap();
    // Its dependent is placed against it and stays resumable too.
    let last_wx11 = map
        .lines()
        .rfind(|l| l.contains("\"bead\":\"wx-1.1\""))
        .unwrap();
    assert!(
        last_wx11.contains("\"phase\":\"created\""),
        "the dependent waits for the close to succeed: {map}"
    );
    // The blocker never reaches `done`, so the next run retries its close.
    let last_wx2 = map
        .lines()
        .rfind(|l| l.contains("\"bead\":\"wx-2\""))
        .unwrap();
    assert!(last_wx2.contains("\"phase\":\"created\""), "{map}");
}

#[test]
fn import_reports_what_exists_when_a_create_fails() {
    let h = Harness::new();
    board_fixtures(&h);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-small.jsonl");
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},
            {"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    .on("create-1", "--title Widget API v2", "https://github.com/acme/widgets/issues/101")
    .on("create-2", "--title Auth refresh drops the session", "https://github.com/acme/widgets/issues/102")
    .on_fail("create-3", "--title Rename the endpoints", "HTTP 500: Internal Server Error")
    .on("values", "issue-field-values --input -", "{}")
    .on("assign", "issue edit 102 -R acme/widgets --add-assignee dev1", "")
    .on("close", "issue close 102 -R acme/widgets --reason duplicate", "")
    .on("anyadd", "project item-add 7 --owner acme --url", r#"{"id":"PVTI_new"}"#)
    .on("anyedit", "project item-edit --id PVTI_new", "");
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "stopped: wx-1.1: stopped after 2 of 3",
        ))
        .stdout(predicate::str::contains(
            "imported 2 of 3 issues before that (1 closed)",
        ))
        .stdout(predicate::str::contains(
            "/beads-map.jsonl (run gbd import again to resume)",
        ));
    let calls = h.calls();
    assert!(
        !calls.contains("issue view 3"),
        "memories are not written on a failed run: {calls}"
    );
    let map = fs::read_to_string(h.cwd.path().join("beads-map.jsonl")).unwrap();
    assert_eq!(
        map.lines()
            .filter(|l| l.contains("\"phase\":\"done\""))
            .count(),
        0,
        "nothing is done until every issue exists and its comments are on: {map}"
    );
    assert_eq!(
        map.lines()
            .filter(|l| l.contains("\"phase\":\"created\""))
            .count(),
        2,
        "{map}"
    );
}

#[test]
fn import_resumes_from_the_mapping_file() {
    let h = Harness::new();
    board_fixtures(&h);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-small.jsonl");
    // A previous run finished wx-1, created wx-2, and died before its
    // follow-up steps.
    fs::write(
        h.cwd.path().join("beads-map.jsonl"),
        "{\"bead\":\"wx-1\",\"number\":101,\"url\":\"https://github.com/acme/widgets/issues/101\",\"phase\":\"created\"}\n\
         {\"bead\":\"wx-1\",\"number\":101,\"url\":\"https://github.com/acme/widgets/issues/101\",\"phase\":\"done\"}\n\
         {\"bead\":\"wx-2\",\"number\":102,\"url\":\"https://github.com/acme/widgets/issues/102\",\"phase\":\"created\"}\n",
    )
    .unwrap();
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},
            {"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    .on("create-2", "--title Auth refresh drops the session", "https://github.com/acme/widgets/issues/102")
    .on("create-3", "--title Rename the endpoints", "https://github.com/acme/widgets/issues/103")
    .on("values", "issue-field-values --input -", "{}")
    .on("assign", "issue edit 102 -R acme/widgets --add-assignee dev1", "")
    .on("close", "issue close 102 -R acme/widgets --reason duplicate", "")
    .on("comment", "issue comment 103 -R acme/widgets --body-file -", "")
    .on("anyadd", "project item-add 7 --owner acme --url", r#"{"id":"PVTI_new"}"#)
    .on("anyedit", "project item-edit --id PVTI_new", "")
    .on("mem-view", "issue view 3 -R acme/widgets --json body", "{\"body\":\"\"}")
    .on("mem-save", "issue edit 3 -R acme/widgets --body-file -", "")
    .on("edit-101", "issue edit 101 -R acme/widgets --body-file -", "");

    // The dry run says what will be skipped.
    h.gbd()
        .args([
            "import",
            "--from-beads",
            fixture.to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Already imported (1), skipped: wx-1",
        ))
        .stdout(predicate::str::contains(
            "Partially imported (1), to be finished: wx-2",
        ));

    fs::create_dir(h.cwd.path().join("sub")).unwrap();
    // From a subdirectory: the default mapping file is the one next to .gbd.yml.
    h.gbd()
        .current_dir(h.cwd.path().join("sub"))
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "1/3  wx-1 = #101  (already imported)\n",
        ))
        .stdout(predicate::str::contains("2/3  wx-2 = #102  (finishing)\n"))
        .stdout(predicate::str::contains(
            "2/3  wx-2 → #102  [Bug] Auth refresh drops the session  ✓\n",
        ))
        .stdout(predicate::str::contains(
            "imported 2 issues (1 closed, 1 finished from an earlier run, 1 already imported)",
        ))
        .stdout(predicate::str::contains("/beads-map.jsonl (bead → issue"));
    let calls = h.calls();
    assert!(
        !calls.contains("--title Widget API v2") && !calls.contains("--title Auth refresh"),
        "nothing is created twice: {calls}"
    );
    // wx-2 gets its follow-up steps replayed: priority, assignee, close, card.
    assert!(
        calls.contains("issues/102/issue-field-values --input -"),
        "{calls}"
    );
    assert!(
        calls.contains("--add-assignee dev1") && calls.contains("issue close 102"),
        "{calls}"
    );
    assert_eq!(
        calls.matches("--single-select-option-id O_done").count(),
        1,
        "{calls}"
    );
    assert!(
        calls.contains("--type Task --parent 101 --blocked-by 102"),
        "edges resolve through the mapping: {calls}"
    );
    assert!(
        !calls.contains("issue edit 101"),
        "a done bead is never re-edited, even with a forward reference: {calls}"
    );
    let map = fs::read_to_string(h.cwd.path().join("beads-map.jsonl")).unwrap();
    let lines: Vec<&str> = map.lines().collect();
    assert_eq!(lines.len(), 8, "{map}");
    let phases: Vec<String> = lines[3..]
        .iter()
        .map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            format!(
                "{} {} {}",
                v["bead"].as_str().unwrap(),
                v["phase"].as_str().unwrap(),
                v["comments"]
            )
        })
        .collect();
    assert_eq!(
        phases,
        [
            "wx-1.1 created 0",
            "wx-2 done 0",
            "wx-1.1 created 1",
            "wx-1.1 created 2",
            "wx-1.1 done 2"
        ],
        "pass one creates and places; pass two posts comments with a checkpoint each, then done: {map}"
    );
}

#[test]
fn import_refuses_a_mapping_file_from_another_repo() {
    let h = Harness::new();
    board_fixtures(&h);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-small.jsonl");
    fs::write(
        h.cwd.path().join("beads-map.jsonl"),
        "{\"bead\":\"wx-1\",\"number\":5,\"url\":\"https://github.com/acme/other/issues/5\",\"phase\":\"done\"}\n",
    )
    .unwrap();
    h.on(
        "create",
        "issue create -R acme/widgets",
        "https://github.com/acme/widgets/issues/1",
    );
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("beads-map.jsonl was written for acme/other (wx-1 → https://github.com/acme/other/issues/5), not acme/widgets"));
    assert!(!h.calls().contains("issue create"), "{}", h.calls());
}

#[test]
fn import_keeps_a_bead_off_done_when_its_body_edit_fails() {
    let h = Harness::new();
    board_fixtures(&h);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-small.jsonl");
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},
            {"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    .on("create-1", "--title Widget API v2", "https://github.com/acme/widgets/issues/101")
    .on("create-2", "--title Auth refresh drops the session", "https://github.com/acme/widgets/issues/102")
    .on("create-3", "--title Rename the endpoints", "https://github.com/acme/widgets/issues/103")
    .on("values", "issue-field-values --input -", "{}")
    .on("assign", "issue edit 102 -R acme/widgets --add-assignee dev1", "")
    .on("close", "issue close 102 -R acme/widgets --reason duplicate", "")
    .on("comment", "issue comment 103 -R acme/widgets --body-file -", "")
    .on("anyadd", "project item-add 7 --owner acme --url", r#"{"id":"PVTI_new"}"#)
    .on("anyedit", "project item-edit --id PVTI_new", "")
    .on("mem-view", "issue view 3 -R acme/widgets --json body", "{\"body\":\"\"}")
    .on("mem-save", "issue edit 3 -R acme/widgets --body-file -", "")
    .on_fail("edit-101", "issue edit 101 -R acme/widgets --body-file -", "HTTP 500: Internal Server Error");
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "wx-1 (#101): body still mentions Beads ids; edit failed",
        ));
    let map = fs::read_to_string(h.cwd.path().join("beads-map.jsonl")).unwrap();
    let last_wx1 = map
        .lines()
        .rfind(|l| l.contains("\"bead\":\"wx-1\""))
        .unwrap();
    assert!(
        last_wx1.contains("\"phase\":\"created\""),
        "not done until the body is right: {map}"
    );
    assert!(
        map.lines()
            .rfind(|l| l.contains("\"bead\":\"wx-2\""))
            .unwrap()
            .contains("\"phase\":\"done\""),
        "{map}"
    );
}

#[test]
fn a_secondary_rate_limit_is_retried_after_waiting() {
    let h = Harness::new();
    h.on_seq(
        "search",
        "search(query: $q",
        &[
            "HTTP 429: You have exceeded a secondary rate limit. Please wait a few minutes before you try again.",
            &search_response(&format!("{NODE_10},{NODE_8}")),
        ],
    );
    fs::write(h.gh_dir.path().join("search.code.1"), "1").unwrap();
    h.gbd()
        .env("GBD_BACKOFF_MS", "1")
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("child B"))
        .stderr(predicate::str::contains(
            "gh: rate limited; retrying in 1s (1 of 5)",
        ));
    assert_eq!(
        h.calls().matches("search(query: $q").count(),
        2,
        "{}",
        h.calls()
    );

    // GraphQL reports the secondary limit without a status line; still retried.
    let h = Harness::new();
    h.on_seq(
        "search",
        "search(query: $q",
        &[
            "gh: You have exceeded a secondary rate limit. Please wait a few minutes before you try again.",
            &search_response(&format!("{NODE_10},{NODE_8}")),
        ],
    );
    fs::write(h.gh_dir.path().join("search.code.1"), "1").unwrap();
    h.gbd()
        .env("GBD_BACKOFF_MS", "1")
        .arg("list")
        .assert()
        .success();
    assert_eq!(
        h.calls().matches("search(query: $q").count(),
        2,
        "{}",
        h.calls()
    );

    // The primary hourly quota is not retried either: it will not clear in time.
    let h = Harness::new();
    h.on_fail(
        "search",
        "search(query: $q",
        "HTTP 403: API rate limit exceeded for user ID 1. (https://docs.github.com/rest/overview/rate-limits-for-the-rest-api)",
    );
    h.gbd()
        .env("GBD_BACKOFF_MS", "1")
        .arg("list")
        .assert()
        .failure();
    assert_eq!(
        h.calls().matches("search(query: $q").count(),
        1,
        "{}",
        h.calls()
    );

    // A plain failure is not retried: a create behind a 502 may have gone through.
    let h = Harness::new();
    h.on_fail("search", "search(query: $q", "HTTP 502: Bad Gateway");
    h.gbd()
        .env("GBD_BACKOFF_MS", "1")
        .arg("list")
        .assert()
        .failure();
    assert_eq!(
        h.calls().matches("search(query: $q").count(),
        1,
        "{}",
        h.calls()
    );

    // `issue create` is several mutations in one: never retried, even on a
    // real secondary limit, since the issue may already exist.
    let h = Harness::new();
    h.on_fail(
        "create",
        "issue create -R acme/widgets",
        "HTTP 403: You have exceeded a secondary rate limit. Please wait a few minutes before you try again.",
    );
    h.gbd()
        .env("GBD_BACKOFF_MS", "1")
        .args(["create", "One shot", "-t", "Task"])
        .assert()
        .failure();
    assert_eq!(
        h.calls().matches("issue create").count(),
        1,
        "{}",
        h.calls()
    );

    // The words in the command itself never count: only what gh printed.
    let h = Harness::new();
    h.on_fail(
        "create",
        "issue create -R acme/widgets",
        "HTTP 502: Bad Gateway",
    );
    h.gbd()
        .env("GBD_BACKOFF_MS", "1")
        .args([
            "create",
            "HTTP 403 rate limit abuse",
            "-t",
            "Task",
            "--body",
            "HTTP 429",
        ])
        .assert()
        .failure();
    assert_eq!(
        h.calls().matches("issue create").count(),
        1,
        "{}",
        h.calls()
    );
}

#[test]
fn import_reconciles_comments_from_github_before_resuming() {
    let h = Harness::new();
    board_fixtures(&h);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-small.jsonl");
    // Everything was created; wx-1 had its body rewritten; wx-1.1's first
    // comment was posted but the kill came before its checkpoint.
    fs::write(
        h.cwd.path().join("beads-map.jsonl"),
        concat!(
            "{\"bead\":\"wx-1\",\"number\":101,\"url\":\"https://github.com/acme/widgets/issues/101\",\"phase\":\"created\",\"comments\":0}\n",
            "{\"bead\":\"wx-2\",\"number\":102,\"url\":\"https://github.com/acme/widgets/issues/102\",\"phase\":\"done\"}\n",
            "{\"bead\":\"wx-1.1\",\"number\":103,\"url\":\"https://github.com/acme/widgets/issues/103\",\"phase\":\"created\",\"comments\":0}\n",
        ),
    )
    .unwrap();
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},
            {"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    .on("values", "issue-field-values --input -", "{}")
    .on(
        "comments-103",
        "repos/acme/widgets/issues/103/comments --paginate --slurp",
        r#"[[{"body":"**Notes**\n\nKeep the old routes for a release.\n\n<!-- gbd-import wx-1.1/1 -->"}],[{"body":"a human said hi"}]]"#,
    )
    // wx-2 is done in the file and closed in the export, but was reopened on
    // GitHub since: wx-1.1 (blocked by it) must land on Blocked, not Ready.
    .on("state-102", "issue view 102 -R acme/widgets --json state", "{\"state\":\"OPEN\"}")
    // wx-1 (resumed, planned open) was closed by hand meanwhile: Done, not Ready.
    .on("state-101", "issue view 101 -R acme/widgets --json state", "{\"state\":\"CLOSED\"}")
    .on("state-103", "issue view 103 -R acme/widgets --json state", "{\"state\":\"OPEN\"}")
    // wx-1's body was already rewritten (and touched by hand) before the kill.
    .on(
        "body-101",
        "issue view 101 -R acme/widgets --json body",
        "{\"body\":\"Umbrella; the auth fix is #102, says a human.\\n\\n---\\nImported from Beads `wx-1` (created 2026-03-01 by dev1).\"}",
    )
    .on("comment", "issue comment 103 -R acme/widgets --body-file -", "")
    .on("anyadd", "project item-add 7 --owner acme --url", r#"{"id":"PVTI_new"}"#)
    .on("anyedit", "project item-edit --id PVTI_new", "")
    .on("mem-view", "issue view 3 -R acme/widgets --json body", "{\"body\":\"\"}")
    .on("mem-save", "issue edit 3 -R acme/widgets --body-file -", "");
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "imported 2 issues (1 closed, 2 finished from an earlier run, 1 already imported)",
        ));
    let calls = h.calls();
    assert!(!calls.contains("issue create"), "{calls}");
    assert!(
        !calls.contains("issue edit 101 -R acme/widgets --body-file"),
        "nothing left to rewrite, so no edit: {calls}"
    );
    let map = fs::read_to_string(h.cwd.path().join("beads-map.jsonl")).unwrap();
    assert!(
        map.lines()
            .any(|l| l.contains("\"bead\":\"wx-1\"") && l.contains("\"rewritten\":true")),
        "the finished rewrite is checkpointed: {map}"
    );
    assert_eq!(
        calls.matches("issue comment 103").count(),
        1,
        "only the second comment is posted: {calls}"
    );
    assert!(
        calls.contains("issue view 102 -R acme/widgets --json state"),
        "a finished blocker is read live: {calls}"
    );
    assert_eq!(
        calls.matches("--single-select-option-id O_done").count(),
        1,
        "wx-1, closed on GitHub meanwhile: {calls}"
    );
    assert!(
        calls.contains("--single-select-option-id O_blocked"),
        "wx-1.1 is blocked by the reopened wx-2: {calls}"
    );
    assert!(
        !calls.contains("--single-select-option-id O_ready"),
        "{calls}"
    );
    assert!(calls.contains("STDIN: **dev2** · 2026-03-03\n\nAlso drop the v1 docs.\n\n<!-- gbd-import wx-1.1/2 -->"), "marker on the posted comment: {calls}");
    let map = fs::read_to_string(h.cwd.path().join("beads-map.jsonl")).unwrap();
    let last: Vec<&str> = map.lines().rev().take(3).collect();
    assert!(
        last[0].contains("\"bead\":\"wx-1.1\"")
            && last[0].contains("\"phase\":\"done\"")
            && last[0].contains("\"comments\":2"),
        "{map}"
    );
}

#[test]
fn import_adopts_an_issue_created_but_never_recorded() {
    let h = Harness::new();
    board_fixtures(&h);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-small.jsonl");
    // wx-1 is done; the previous run was killed right after creating wx-2,
    // before its line was written.
    fs::write(
        h.cwd.path().join("beads-map.jsonl"),
        "{\"bead\":\"wx-1\",\"number\":101,\"url\":\"https://github.com/acme/widgets/issues/101\",\"phase\":\"done\"}\n",
    )
    .unwrap();
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},
            {"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    // Just created, so not in the search index yet: it shows among the newest issues.
    .on(
        "newest",
        "issue list -R acme/widgets --state all --limit 20 --json number,url,body",
        r#"[{"number":102,"url":"https://github.com/acme/widgets/issues/102","body":"Token refresh races the request.\n\n---\nImported from Beads `wx-2` (created 2026-03-02 by dev2)."},{"number":101,"url":"https://github.com/acme/widgets/issues/101","body":"…Imported from Beads `wx-1`…"}]"#,
    )
    .on("create-3", "--title Rename the endpoints", "https://github.com/acme/widgets/issues/103")
    .on("values", "issue-field-values --input -", "{}")
    .on("retype", "issue edit 102 -R acme/widgets --type Bug", "")
    .on("assign", "issue edit 102 -R acme/widgets --add-assignee dev1", "")
    .on("close", "issue close 102 -R acme/widgets --reason duplicate", "")
    .on("comment", "issue comment 103 -R acme/widgets --body-file -", "")
    .on("anyadd", "project item-add 7 --owner acme --url", r#"{"id":"PVTI_new"}"#)
    .on("anyedit", "project item-edit --id PVTI_new", "")
    .on("mem-view", "issue view 3 -R acme/widgets --json body", "{\"body\":\"\"}")
    .on("mem-save", "issue edit 3 -R acme/widgets --body-file -", "");
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "wx-2 = #102  (found on GitHub, unrecorded; recorded now)",
        ))
        .stdout(predicate::str::contains(
            "imported 2 issues (1 closed, 1 finished from an earlier run, 1 already imported)",
        ));
    let calls = h.calls();
    assert!(
        !calls.contains("--title Auth refresh"),
        "not created twice: {calls}"
    );
    assert!(
        calls.contains("issue edit 102 -R acme/widgets --type Bug"),
        "the type a dying create may not have set is reapplied: {calls}"
    );
    assert!(
        !calls.contains("issue list -R acme/widgets --search"),
        "found among the newest issues, no search needed: {calls}"
    );
    assert!(
        calls.contains("--type Task --parent 101 --blocked-by 102"),
        "{calls}"
    );
    let map = fs::read_to_string(h.cwd.path().join("beads-map.jsonl")).unwrap();
    assert!(
        map.lines().nth(1).unwrap().contains("\"bead\":\"wx-2\"")
            && map
                .lines()
                .nth(1)
                .unwrap()
                .contains("\"phase\":\"created\""),
        "{map}"
    );

    // With an empty file and the second bead on GitHub too, it is a refusal:
    // an earlier import ran without this file.
    let h = Harness::new();
    board_fixtures(&h);
    h.on(
        "found-wx2",
        "issue list -R acme/widgets --search \"Imported from Beads wx-2\" in:body",
        r#"[{"number":8,"url":"https://github.com/acme/widgets/issues/8","body":"y\n\n---\nImported from Beads `wx-2` (created 2026-03-02 by dev2)."}]"#,
    )
    .on(
        "found-wx1",
        "issue list -R acme/widgets --search \"Imported from Beads wx-1\" in:body",
        r#"[{"number":7,"url":"https://github.com/acme/widgets/issues/7","body":"x\n\n---\nImported from Beads `wx-1` (created 2026-03-01 by dev1)."}]"#,
    )
    .on("create", "issue create -R acme/widgets", "https://github.com/acme/widgets/issues/1")
    .on(
        "fields",
        FIELDS_GET,
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[{"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},{"id":7886557,"name":"Start date","data_type":"date"}]"#,
    );
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "wx-1 already exists as #7 (https://github.com/acme/widgets/issues/7) and so does wx-2, but",
        ))
        .stderr(predicate::str::contains(
            "is empty: an earlier import ran without this file",
        ));
    assert!(!h.calls().contains("issue create"), "{}", h.calls());

    // With an empty file and only the first bead on GitHub, the first record
    // was lost: adopt it and carry on.
    let h = Harness::new();
    board_fixtures(&h);
    h.on(
        "found-wx1",
        "issue list -R acme/widgets --search \"Imported from Beads wx-1\" in:body",
        r#"[{"number":7,"url":"https://github.com/acme/widgets/issues/7","body":"Umbrella; the auth fix is wx-2.\n\n---\nImported from Beads `wx-1` (created 2026-03-01 by dev1)."}]"#,
    )
    .on("fields", FIELDS_GET, r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[{"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},{"id":7886557,"name":"Start date","data_type":"date"}]"#)
    .on("create-2", "--title Auth refresh drops the session", "https://github.com/acme/widgets/issues/102")
    .on("create-3", "--title Rename the endpoints", "https://github.com/acme/widgets/issues/103")
    .on("values", "issue-field-values --input -", "{}")
    .on("assign", "issue edit 102 -R acme/widgets --add-assignee dev1", "")
    .on("close", "issue close 102 -R acme/widgets --reason duplicate", "")
    .on("comment", "issue comment 103 -R acme/widgets --body-file -", "")
    .on("retype-7", "issue edit 7 -R acme/widgets --type Epic", "")
    .on("body-7", "issue view 7 -R acme/widgets --json body", "{\"body\":\"Umbrella; the auth fix is wx-2.\\n\\n---\\nImported from Beads `wx-1` (created 2026-03-01 by dev1).\"}")
    .on("edit-7", "issue edit 7 -R acme/widgets --body-file -", "")
    .on("anyadd", "project item-add 7 --owner acme --url", r#"{"id":"PVTI_new"}"#)
    .on("anyedit", "project item-edit --id PVTI_new", "")
    .on("mem-view", "issue view 3 -R acme/widgets --json body", "{\"body\":\"\"}")
    .on("mem-save", "issue edit 3 -R acme/widgets --body-file -", "");
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "wx-1 = #7  (found on GitHub, unrecorded; recorded now)",
        ));
    let calls = h.calls();
    assert!(!calls.contains("--title Widget API v2"), "{calls}");
    assert!(
        calls.contains("--type Task --parent 7 --blocked-by 102"),
        "{calls}"
    );
    // Its live body is rewritten in place, so the forward reference is fixed.
    let fixed = calls
        .rsplit("issue edit 7 -R acme/widgets --body-file -\nSTDIN: ")
        .next()
        .unwrap();
    assert!(
        fixed.starts_with("Umbrella; the auth fix is #102."),
        "{fixed}"
    );
}

#[test]
fn import_of_memories_alone_needs_no_board() {
    let h = Harness::new(); // .gbd.yml without project:, memory_issue 3
    let export = h.cwd.path().join("memories.jsonl");
    fs::write(&export, "{\"_type\":\"memory\",\"key\":\"deploy-runbook\",\"value\":\"Deploy with make deploy.\"}\n").unwrap();
    h.on(
        "mem-view",
        "issue view 3 -R acme/widgets --json body",
        "{\"body\":\"\"}",
    )
    .on("mem-save", "issue edit 3 -R acme/widgets --body-file -", "");
    h.gbd()
        .args(["import", "--from-beads", export.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "no issues to import; memories: 1 new, 0 updated on #3",
        ));
    let calls = h.calls();
    assert!(
        !calls.contains("project view") && !calls.contains("issue-fields"),
        "no board, no fields: {calls}"
    );
    assert!(calls.contains("\n## deploy-runbook\n"), "{calls}");
}

#[test]
fn import_with_everything_done_only_retries_the_memories() {
    let h = Harness::new(); // no board configured
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-small.jsonl");
    // wx-2's assignee is history: a finished bead is not checked again,
    // so a login that stopped being assignable cannot block the resume.
    h.on_fail(
        "assignable",
        "repos/acme/widgets/assignees/dev1",
        "gh: Not Found (HTTP 404)",
    );
    fs::write(
        h.cwd.path().join("beads-map.jsonl"),
        concat!(
            "{\"bead\":\"wx-1\",\"number\":101,\"url\":\"https://github.com/acme/widgets/issues/101\",\"phase\":\"done\"}\n",
            "{\"bead\":\"wx-2\",\"number\":102,\"url\":\"https://github.com/acme/widgets/issues/102\",\"phase\":\"done\"}\n",
            "{\"bead\":\"wx-1.1\",\"number\":103,\"url\":\"https://github.com/acme/widgets/issues/103\",\"phase\":\"done\"}\n",
        ),
    )
    .unwrap();
    h.on(
        "mem-view",
        "issue view 3 -R acme/widgets --json body",
        "{\"body\":\"\"}",
    )
    .on("mem-save", "issue edit 3 -R acme/widgets --body-file -", "");
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "no issues left to import (3 already imported); memories: 1 new, 0 updated on #3",
        ));
    let calls = h.calls();
    assert!(
        !calls.contains("project view")
            && !calls.contains("issue-fields")
            && !calls.contains("issue create"),
        "{calls}"
    );
}

#[test]
fn import_keeps_a_dependent_unfinished_when_its_blocker_cannot_be_read() {
    let h = Harness::new();
    board_fixtures(&h);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-small.jsonl");
    fs::write(
        h.cwd.path().join("beads-map.jsonl"),
        concat!(
            "{\"bead\":\"wx-1\",\"number\":101,\"url\":\"https://github.com/acme/widgets/issues/101\",\"phase\":\"done\"}\n",
            "{\"bead\":\"wx-2\",\"number\":102,\"url\":\"https://github.com/acme/widgets/issues/102\",\"phase\":\"done\"}\n",
            "{\"bead\":\"wx-1.1\",\"number\":103,\"url\":\"https://github.com/acme/widgets/issues/103\",\"phase\":\"created\",\"comments\":2,\"rewritten\":true}\n",
        ),
    )
    .unwrap();
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[
            {"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},
            {"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    .on("values", "issue-field-values --input -", "{}")
    .on_fail("state-102", "issue view 102 -R acme/widgets --json state", "HTTP 502: Bad Gateway")
    .on("state-103", "issue view 103 -R acme/widgets --json state", "{\"state\":\"OPEN\"}")
    .on("anyadd", "project item-add 7 --owner acme --url", r#"{"id":"PVTI_new"}"#)
    .on("anyedit", "project item-edit --id PVTI_new", "")
    .on("mem-view", "issue view 3 -R acme/widgets --json body", "{\"body\":\"\"}")
    .on("mem-save", "issue edit 3 -R acme/widgets --body-file -", "");
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "wx-1.1 (#103): placed against wx-2, whose state could not be read",
        ));
    let map = fs::read_to_string(h.cwd.path().join("beads-map.jsonl")).unwrap();
    let last = map
        .lines()
        .rfind(|l| l.contains("\"bead\":\"wx-1.1\""))
        .unwrap();
    assert!(
        last.contains("\"phase\":\"created\""),
        "not done until the blocker can be read: {map}"
    );
}

#[test]
fn import_stops_when_an_adopted_issue_cannot_be_completed() {
    let h = Harness::new();
    board_fixtures(&h);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-small.jsonl");
    fs::write(
        h.cwd.path().join("beads-map.jsonl"),
        "{\"bead\":\"wx-1\",\"number\":101,\"url\":\"https://github.com/acme/widgets/issues/101\",\"phase\":\"done\"}\n",
    )
    .unwrap();
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[{"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},{"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    .on(
        "newest",
        "issue list -R acme/widgets --state all --limit 20 --json number,url,body",
        r#"[{"number":102,"url":"https://github.com/acme/widgets/issues/102","body":"Token refresh races the request.\n\n---\nImported from Beads `wx-2` (created 2026-03-02 by dev2)."}]"#,
    )
    // The type a dying create may not have set cannot be put back.
    .on_fail("retype", "issue edit 102 -R acme/widgets --type Bug", "HTTP 500: boom")
    .on("create-3", "--title Rename the endpoints", "https://github.com/acme/widgets/issues/103");
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "wx-2 is #102 (https://github.com/acme/widgets/issues/102) but its type could not be reapplied; nothing was recorded",
        ));
    let calls = h.calls();
    assert!(
        !calls.contains("--title Rename the endpoints"),
        "nothing after it is created: {calls}"
    );
    let map = fs::read_to_string(h.cwd.path().join("beads-map.jsonl")).unwrap();
    assert_eq!(
        map.lines().count(),
        1,
        "the adoption is not recorded: {map}"
    );
}

#[test]
fn import_refuses_a_mapping_file_another_import_holds() {
    let h = Harness::new();
    board_fixtures(&h);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-small.jsonl");
    // Another import (or a retry of one that only looks hung) has the file.
    let other = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(h.cwd.path().join("beads-map.jsonl"))
        .unwrap();
    other.try_lock().unwrap();
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "beads-map.jsonl is in use by another gbd import; wait for it to finish",
        ));
    assert!(!h.calls().contains("issue create"), "{}", h.calls());
    drop(other);
}

#[test]
fn import_refuses_an_assignee_that_is_not_a_login_until_mapped() {
    let h = Harness::new();
    board_fixtures(&h);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-named.jsonl");
    // The dry run says what it found and what to pass.
    h.gbd()
        .args([
            "import",
            "--from-beads",
            fixture.to_str().unwrap(),
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Assignees:  Pat Example (1) — not a GitHub login; pass --assignee 'Pat Example=LOGIN'",
        ));
    // The real run refuses before creating anything.
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "assignees to map before the import can start:\n  Pat Example (1 bead): not a GitHub login\nPass --assignee 'Pat Example=LOGIN' to map them, or NAME= to import without one",
        ));
    assert!(!h.calls().contains("issue create"), "{}", h.calls());

    // Mapped to a login the repo cannot assign: refused, naming the login.
    fs::write(
        h.gh_dir.path().join("00-nobody.args"),
        "repos/acme/widgets/assignees/nobody",
    )
    .unwrap();
    fs::write(
        h.gh_dir.path().join("00-nobody.out"),
        "gh: Not Found (HTTP 404)",
    )
    .unwrap();
    fs::write(h.gh_dir.path().join("00-nobody.code"), "1").unwrap();
    h.gbd()
        .args([
            "import",
            "--from-beads",
            fixture.to_str().unwrap(),
            "--yes",
            "--assignee",
            "Pat Example=nobody",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "  Pat Example (1 bead): nobody cannot be assigned in acme/widgets",
        ));
    assert!(!h.calls().contains("issue create"), "{}", h.calls());

    // The preflight failing for any other reason is that reason, not
    // advice about the mapping.
    fs::write(
        h.gh_dir.path().join("00-someone.args"),
        "repos/acme/widgets/assignees/someone",
    )
    .unwrap();
    fs::write(
        h.gh_dir.path().join("00-someone.out"),
        "gh: Bad Gateway (HTTP 502)",
    )
    .unwrap();
    fs::write(h.gh_dir.path().join("00-someone.code"), "1").unwrap();
    h.gbd()
        .args([
            "import",
            "--from-beads",
            fixture.to_str().unwrap(),
            "--yes",
            "--assignee",
            "Pat Example=someone",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "could not check whether someone can be assigned in acme/widgets",
        ))
        .stderr(predicate::str::contains("HTTP 502"))
        .stderr(predicate::str::contains("assignees to map").not());
    assert!(!h.calls().contains("issue create"), "{}", h.calls());

    // Mapped: the login is assigned.
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[{"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},{"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    .on("create", "--title Wire the thing", "https://github.com/acme/widgets/issues/201")
    .on("values", "issue-field-values --input -", "{}")
    .on("assign", "issue edit 201 -R acme/widgets --add-assignee patexample", "")
    .on("anyadd", "project item-add 7 --owner acme --url", r#"{"id":"PVTI_new"}"#)
    .on("anyedit", "project item-edit --id PVTI_new", "");
    h.gbd()
        .args([
            "import",
            "--from-beads",
            fixture.to_str().unwrap(),
            "--yes",
            "--assignee",
            "Pat Example=patexample",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("imported 1 issues"))
        .stdout(predicate::str::contains("0 warnings"));
    assert!(
        h.calls()
            .contains("issue edit 201 -R acme/widgets --add-assignee patexample"),
        "{}",
        h.calls()
    );

    // Dropped: no assignee call at all, and no warning.
    let h = Harness::new();
    board_fixtures(&h);
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[{"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},{"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    .on("create", "--title Wire the thing", "https://github.com/acme/widgets/issues/201")
    .on("values", "issue-field-values --input -", "{}")
    .on("anyadd", "project item-add 7 --owner acme --url", r#"{"id":"PVTI_new"}"#)
    .on("anyedit", "project item-edit --id PVTI_new", "");
    h.gbd()
        .args([
            "import",
            "--from-beads",
            fixture.to_str().unwrap(),
            "--yes",
            "--assignee",
            "Pat Example=",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 warnings"));
    assert!(!h.calls().contains("--add-assignee"), "{}", h.calls());
}

#[test]
fn import_finishes_a_bead_whose_assignee_github_rejects() {
    let h = Harness::new();
    board_fixtures(&h);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-small.jsonl");
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[{"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},{"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    .on("create-1", "--title Widget API v2", "https://github.com/acme/widgets/issues/101")
    .on("create-2", "--title Auth refresh drops the session", "https://github.com/acme/widgets/issues/102")
    .on("create-3", "--title Rename the endpoints", "https://github.com/acme/widgets/issues/103")
    .on("values", "issue-field-values --input -", "{}")
    // dev1 looks like a login but is not one on this GitHub.
    .on_fail("assign", "issue edit 102 -R acme/widgets --add-assignee dev1", "GraphQL: Could not resolve to a user or bot with the login 'dev1'.")
    .on("close", "issue close 102 -R acme/widgets --reason duplicate", "")
    .on("comment", "issue comment 103 -R acme/widgets --body-file -", "")
    .on("anyadd", "project item-add 7 --owner acme --url", r#"{"id":"PVTI_new"}"#)
    .on("anyedit", "project item-edit --id PVTI_new", "")
    .on("mem-view", "issue view 3 -R acme/widgets --json body", "{\"body\":\"\"}")
    .on("mem-save", "issue edit 3 -R acme/widgets --body-file -", "")
    .on("edit-101", "issue edit 101 -R acme/widgets --body-file -", "");
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("; 1 warning;"))
        .stderr(predicate::str::contains(
            "wx-2 (#102): assignee dev1 not set: ",
        ))
        .stderr(predicate::str::contains(
            "Set it by hand, or re-run with --assignee 'dev1=LOGIN'",
        ));
    let map = fs::read_to_string(h.cwd.path().join("beads-map.jsonl")).unwrap();
    let last_wx2 = map
        .lines()
        .rfind(|l| l.contains("\"bead\":\"wx-2\""))
        .unwrap();
    assert!(
        last_wx2.contains("\"phase\":\"done\""),
        "a rejected login is not retried, the bead is finished: {map}"
    );

    // Any other failure on the assignee is retried like every step: the
    // bead stays at `created`.
    let h = Harness::new();
    board_fixtures(&h);
    h.on(
        "fields",
        FIELDS_GET,
        r#"[{"id":46822523,"name":"Priority","data_type":"single_select","options":[{"id":1,"name":"P0"},{"id":2,"name":"P1"},{"id":3,"name":"P2"},{"id":4,"name":"P3"},{"id":5,"name":"P4"}]},{"id":7886557,"name":"Start date","data_type":"date"}]"#,
    )
    .on("create-1", "--title Widget API v2", "https://github.com/acme/widgets/issues/101")
    .on("create-2", "--title Auth refresh drops the session", "https://github.com/acme/widgets/issues/102")
    .on("create-3", "--title Rename the endpoints", "https://github.com/acme/widgets/issues/103")
    .on("values", "issue-field-values --input -", "{}")
    .on_fail("assign", "issue edit 102 -R acme/widgets --add-assignee dev1", "HTTP 502: bad gateway")
    .on("close", "issue close 102 -R acme/widgets --reason duplicate", "")
    .on("comment", "issue comment 103 -R acme/widgets --body-file -", "")
    .on("anyadd", "project item-add 7 --owner acme --url", r#"{"id":"PVTI_new"}"#)
    .on("anyedit", "project item-edit --id PVTI_new", "")
    .on("mem-view", "issue view 3 -R acme/widgets --json body", "{\"body\":\"\"}")
    .on("mem-save", "issue edit 3 -R acme/widgets --body-file -", "")
    .on("edit-101", "issue edit 101 -R acme/widgets --body-file -", "");
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stderr(predicate::str::contains("assignee dev1 not set: "))
        .stderr(predicate::str::contains("Run gbd import again to retry"));
    let map = fs::read_to_string(h.cwd.path().join("beads-map.jsonl")).unwrap();
    let last_wx2 = map
        .lines()
        .rfind(|l| l.contains("\"bead\":\"wx-2\""))
        .unwrap();
    assert!(
        last_wx2.contains("\"phase\":\"created\""),
        "a transient failure keeps the bead resumable: {map}"
    );
}

#[test]
fn import_refuses_when_the_org_lacks_a_type_the_plan_needs() {
    let h = Harness::new();
    board_fixtures(&h);
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beads-export.jsonl");
    // An org set up by an earlier gbd: five types, no Decision.
    h.on(
        "types",
        TYPES_GET,
        r#"[{"name":"Epic"},{"name":"Feature"},{"name":"Bug"},{"name":"Task"},{"name":"Chore"}]"#,
    );
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "issue type Decision missing on acme: run gbd init (org admin) first",
        ));
    assert!(!h.calls().contains("issue create"), "{}", h.calls());

    // Not being able to read the types is a stop, not a pass.
    let h = Harness::new();
    board_fixtures(&h);
    h.on_fail("types", TYPES_GET, "gh: Not Found (HTTP 404)");
    h.gbd()
        .args(["import", "--from-beads", fixture.to_str().unwrap(), "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "reading the issue types of acme; they live on an organization, which gbd needs",
        ));
    assert!(!h.calls().contains("issue create"), "{}", h.calls());
}
