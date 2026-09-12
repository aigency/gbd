//! Memories are a KV plane stored in one closed GitHub issue, not a type.

use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::BTreeMap;

use crate::gh;
use crate::repo::Repo;

pub const HEADER: &str = "<!-- gbd-memories v1 -->";
pub const MEMORY_TITLE: &str = "gbd memories";

/// Beads `DeriveKey`: lowercase, non-alnum → hyphen, first eight
/// hyphen-segments, cap 60 bytes.
pub fn derive_key(insight: &str) -> String {
    let mut out = String::new();
    let mut last_hyphen = false;
    for c in insight.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_hyphen = false;
        } else if !last_hyphen && !out.is_empty() {
            out.push('-');
            last_hyphen = true;
        }
    }
    let trimmed = out.trim_matches('-');
    let joined = trimmed
        .split('-')
        .filter(|s| !s.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join("-");
    truncate_bytes(&joined, 60)
        .trim_end_matches('-')
        .to_string()
}

fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

pub fn parse_body(body: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut current_key: Option<String> = None;
    let mut buf: Vec<String> = Vec::new();
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            flush(&mut map, current_key.take(), &mut buf);
            current_key = Some(rest.trim().to_string());
            continue;
        }
        if current_key.is_some() {
            buf.push(line.to_string());
        }
    }
    flush(&mut map, current_key, &mut buf);
    map
}

fn flush(map: &mut BTreeMap<String, String>, key: Option<String>, buf: &mut Vec<String>) {
    if let Some(k) = key {
        if k.is_empty() {
            buf.clear();
            return;
        }
        let value = buf.join("\n").trim().to_string();
        map.insert(k, value);
    }
    buf.clear();
}

pub fn render_body(map: &BTreeMap<String, String>) -> String {
    let mut out = String::from(HEADER);
    out.push_str("\n\n");
    for (k, v) in map {
        out.push_str("## ");
        out.push_str(k);
        out.push('\n');
        out.push_str(v);
        if !v.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

#[derive(Debug, Serialize)]
pub struct RememberResult {
    pub key: String,
    pub value: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub found: Option<bool>,
}

/// Footgun guard: a bare slug that round-trips through `DeriveKey` is a
/// getter, not a write, unless `--key` was given.
pub fn remember_without_key(
    insight: &str,
    existing: &BTreeMap<String, String>,
) -> Result<RememberOutcome> {
    let derived = derive_key(insight);
    if derived.is_empty() {
        bail!("could not generate key from content; use --key to specify one");
    }
    if derived == insight {
        if let Some(value) = existing.get(&derived) {
            return Ok(RememberOutcome::Recalled {
                key: derived,
                value: value.clone(),
            });
        }
        bail!(
            "no memory named {derived:?} to recall — and refusing to store a bare key-like token as its own content. \
             `gbd remember` WRITES (its positional arg is CONTENT, not a key). \
             To store it anyway: gbd remember {insight:?} --key {derived}. To browse keys: gbd memories"
        );
    }
    Ok(RememberOutcome::Store {
        key: derived,
        value: insight.to_string(),
    })
}

#[derive(Debug)]
pub enum RememberOutcome {
    Store { key: String, value: String },
    Recalled { key: String, value: String },
}

// ---------------------------------------------------------------------------
// Store: the closed blob issue, via gh.

/// Find the memories issue: `.gbd.yml` `memory_issue`, else by title.
pub fn locate(repo: &Repo, configured: Option<u64>) -> Result<u64> {
    if let Some(n) = configured {
        return Ok(n);
    }
    let list: Vec<serde_json::Value> = gh::run_json(&[
        "issue",
        "list",
        "-R",
        &repo.name_with_owner,
        "--search",
        MEMORY_TITLE,
        "--state",
        "all",
        "--limit",
        "20",
        "--json",
        "number,title",
    ])?;
    list.iter()
        .find(|i| {
            i.get("title")
                .and_then(|x| x.as_str())
                .is_some_and(|t| t.eq_ignore_ascii_case(MEMORY_TITLE))
        })
        .and_then(|i| i.get("number").and_then(serde_json::Value::as_u64))
        .ok_or_else(|| anyhow::anyhow!("no memories issue (gbd Role=Memory); run gbd init"))
}

pub fn load(repo: &Repo, number: u64) -> Result<BTreeMap<String, String>> {
    let view: serde_json::Value = gh::run_json(&[
        "issue",
        "view",
        &number.to_string(),
        "-R",
        &repo.name_with_owner,
        "--json",
        "body",
    ])?;
    let body = view.get("body").and_then(|b| b.as_str()).unwrap_or("");
    Ok(parse_body(body))
}

/// Last write wins (matches Beads). The body goes to gh on stdin
/// (`--body-file -`): no temp file, nothing on disk to race.
pub fn save(repo: &Repo, number: u64, map: &BTreeMap<String, String>) -> Result<()> {
    let body = render_body(map);
    gh::run_stdin(
        &[
            "issue",
            "edit",
            &number.to_string(),
            "-R",
            &repo.name_with_owner,
            "--body-file",
            "-",
        ],
        body.as_bytes(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_key_slugifies_prose() {
        assert_eq!(
            derive_key("always run tests with -race flag"),
            "always-run-tests-with-race-flag"
        );
    }

    #[test]
    fn derive_key_caps_eight_segments() {
        let key = derive_key("one two three four five six seven eight nine ten");
        assert_eq!(key, "one-two-three-four-five-six-seven-eight");
    }

    #[test]
    fn derive_key_caps_sixty_bytes() {
        let key = derive_key(&"a".repeat(80));
        assert!(key.len() <= 60);
    }

    #[test]
    fn derive_key_bare_slug_round_trips() {
        assert_eq!(derive_key("auth-jwt"), "auth-jwt");
    }

    #[test]
    fn parse_and_render_round_trip() {
        let body = "<!-- gbd-memories v1 -->\n\n## auth-jwt\nauth module uses JWT not sessions\n\n## always-run-tests-with-race\nalways run tests with -race flag\n";
        let map = parse_body(body);
        assert_eq!(map["auth-jwt"], "auth module uses JWT not sessions");
        assert_eq!(
            map["always-run-tests-with-race"],
            "always run tests with -race flag"
        );
        let rendered = render_body(&map);
        let again = parse_body(&rendered);
        assert_eq!(map, again);
        assert!(rendered.starts_with(HEADER));
    }

    #[test]
    fn bare_existing_slug_recalls() {
        let mut map = BTreeMap::new();
        map.insert("auth-jwt".into(), "jwt".into());
        match remember_without_key("auth-jwt", &map).unwrap() {
            RememberOutcome::Recalled { key, value } => {
                assert_eq!(key, "auth-jwt");
                assert_eq!(value, "jwt");
            }
            RememberOutcome::Store { key, value } => {
                panic!("expected recall, got Store {key:?} = {value:?}")
            }
        }
    }

    #[test]
    fn bare_unknown_slug_refused() {
        let map = BTreeMap::new();
        assert!(remember_without_key("auth-jwt", &map).is_err());
    }

    #[test]
    fn prose_stores() {
        let map = BTreeMap::new();
        match remember_without_key("auth module uses JWT not sessions", &map).unwrap() {
            RememberOutcome::Store { key, value } => {
                assert_eq!(key, "auth-module-uses-jwt-not-sessions");
                assert_eq!(value, "auth module uses JWT not sessions");
            }
            RememberOutcome::Recalled { key, value } => {
                panic!("expected store, got Recalled {key:?} = {value:?}")
            }
        }
    }
}
