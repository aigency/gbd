//! Organization issue fields. Priority is P0–P4 on the field, not labels.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::gh;
use crate::repo::Repo;

pub const PRIORITY_FIELD: &str = "Priority";
pub const PRIORITY_OPTIONS: [&str; 5] = ["P0", "P1", "P2", "P3", "P4"];
const PRIORITY_COLORS: [&str; 5] = ["red", "orange", "yellow", "green", "gray"];

/// Org single-select. Option `Memory` marks the closed lore blob. Not a type, not a label.
pub const ROLE_FIELD: &str = "gbd Role";
pub const ROLE_MEMORY: &str = "Memory";

/// Org date field. A date in the future is Beads `deferred`.
pub const START_DATE_FIELD: &str = "Start date";

/// GitHub’s default Priority options. `gbd init` renames these in place to P0–P4.
const DEFAULT_RENAME: &[(&str, &str)] = &[
    ("Urgent", "P0"),
    ("High", "P1"),
    ("Medium", "P2"),
    ("Low", "P3"),
];

#[derive(Debug, Clone)]
pub struct IssueField {
    pub id: u64,
    pub name: String,
    pub data_type: String,
    pub options: Vec<FieldOption>,
}

#[derive(Debug, Clone)]
pub struct FieldOption {
    pub id: Option<u64>,
    pub name: String,
    pub color: Option<String>,
}

pub fn option_name(rank: u8) -> Result<&'static str> {
    PRIORITY_OPTIONS
        .get(rank as usize)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("priority must be 0-4"))
}

pub fn parse_rank(token: &str) -> Result<u8> {
    let s = token.trim();
    if let Some(p) = parse_p(s) {
        return Ok(p);
    }
    bail!("priority must be 0-4 or P0-P4 (got {token:?})")
}

pub fn parse_p(s: &str) -> Option<u8> {
    let s = s.trim();
    let rest = s
        .strip_prefix('P')
        .or_else(|| s.strip_prefix('p'))
        .unwrap_or(s);
    match rest {
        "0" => Some(0),
        "1" => Some(1),
        "2" => Some(2),
        "3" => Some(3),
        "4" => Some(4),
        _ => None,
    }
}

/// Rank when an issue has no Priority set (Beads default: P2).
pub const DEFAULT_RANK: u8 = 2;

pub fn rank_from_option(name: &str) -> u8 {
    parse_p(name).unwrap_or(DEFAULT_RANK)
}

pub fn list_org_fields(org: &str) -> Result<Vec<IssueField>> {
    let raw: Value = gh::api_json("GET", &format!("orgs/{org}/issue-fields"), None)?;
    let arr = raw
        .as_array()
        .with_context(|| format!("orgs/{org}/issue-fields: expected array"))?;
    Ok(arr.iter().filter_map(parse_field).collect())
}

pub fn find_named_field(org: &str, name: &str) -> Result<Option<IssueField>> {
    Ok(list_org_fields(org)?
        .into_iter()
        .find(|f| f.name.eq_ignore_ascii_case(name)))
}

/// Like [`find_named_field`], but a field of the right name and the wrong
/// `data_type` is an error, not a match: gbd would otherwise write values
/// the GraphQL fragments never read back.
pub fn find_typed_field(org: &str, name: &str, data_type: &str) -> Result<Option<IssueField>> {
    match find_named_field(org, name)? {
        Some(f) if f.data_type.eq_ignore_ascii_case(data_type) => Ok(Some(f)),
        Some(f) => bail!(
            "org {org} has an issue field named {name:?} of type {}, but gbd needs a {data_type} field. Rename or delete it: https://github.com/organizations/{org}/settings/issue-fields",
            if f.data_type.is_empty() { "unknown" } else { f.data_type.as_str() }
        ),
        None => Ok(None),
    }
}

pub fn find_priority_field(org: &str) -> Result<Option<IssueField>> {
    find_typed_field(org, PRIORITY_FIELD, "single_select")
}

pub fn role_has_memory(field: &IssueField) -> bool {
    field
        .options
        .iter()
        .any(|o| o.name.eq_ignore_ascii_case(ROLE_MEMORY))
}

/// Ensure org field `gbd Role` exists with option Memory.
pub fn ensure_role_field(org: &str) -> Result<IssueField> {
    if let Some(existing) = find_typed_field(org, ROLE_FIELD, "single_select")? {
        if role_has_memory(&existing) {
            return Ok(existing);
        }
        add_select_option(org, &existing, ROLE_MEMORY, "gray")?;
        return find_named_field(org, ROLE_FIELD)?
            .ok_or_else(|| anyhow::anyhow!("{ROLE_FIELD} field missing after update"));
    }
    let body = json!({
        "name": ROLE_FIELD,
        "description": "gbd classifier. Memory is the closed lore blob, not work.",
        "data_type": "single_select",
        "options": [{
            "name": ROLE_MEMORY,
            "description": "Closed memories blob (not work)",
            "color": "gray",
            "priority": 1,
        }],
    });
    gh::api("POST", &format!("orgs/{org}/issue-fields"), Some(&body))?;
    find_named_field(org, ROLE_FIELD)?
        .ok_or_else(|| anyhow::anyhow!("{ROLE_FIELD} field missing after create"))
}

fn add_select_option(org: &str, existing: &IssueField, name: &str, color: &str) -> Result<()> {
    let mut options = Vec::new();
    for (i, opt) in existing.options.iter().enumerate() {
        let mut item = json!({
            "name": opt.name,
            "color": rest_color(opt.color.as_deref(), color),
            "priority": i + 1,
        });
        if let Some(id) = opt.id {
            item["id"] = json!(id);
        }
        options.push(item);
    }
    options.push(json!({
        "name": name,
        "color": color,
        "priority": existing.options.len() + 1,
    }));
    gh::api(
        "PATCH",
        &format!("orgs/{org}/issue-fields/{}", existing.id),
        Some(&json!({ "options": options })),
    )?;
    Ok(())
}

pub fn set_select(repo: &Repo, number: u64, field_name: &str, option: &str) -> Result<String> {
    let field = find_typed_field(repo.owner(), field_name, "single_select")?.ok_or_else(|| {
        anyhow::anyhow!(
            "no {field_name} issue field on org {}. Org settings: https://github.com/organizations/{}/settings/issue-fields",
            repo.owner(),
            repo.owner()
        )
    })?;
    let body = json!({
        "issue_field_values": [{
            "field_id": field.id,
            "value": option,
        }]
    });
    gh::api(
        "POST",
        &format!(
            "repos/{}/issues/{number}/issue-field-values",
            repo.name_with_owner
        ),
        Some(&body),
    )?;
    Ok(option.to_string())
}

pub fn set_role_memory(repo: &Repo, number: u64) -> Result<String> {
    set_select(repo, number, ROLE_FIELD, ROLE_MEMORY)
}

/// Ensure the org has a `Start date` date field (Beads `defer_until`).
/// GitHub ships one by default; a fresh org may have deleted it.
pub fn ensure_start_date_field(org: &str) -> Result<IssueField> {
    if let Some(existing) = find_typed_field(org, START_DATE_FIELD, "date")? {
        return Ok(existing);
    }
    let body = json!({
        "name": START_DATE_FIELD,
        "description": "Date when work on issue will begin",
        "data_type": "date",
    });
    gh::api("POST", &format!("orgs/{org}/issue-fields"), Some(&body))?;
    find_typed_field(org, START_DATE_FIELD, "date")?
        .ok_or_else(|| anyhow::anyhow!("{START_DATE_FIELD} field missing after create"))
}

pub fn set_start_date(repo: &Repo, number: u64, date: &str) -> Result<String> {
    let date = parse_iso_date(date)?;
    let field = find_typed_field(repo.owner(), START_DATE_FIELD, "date")?.ok_or_else(|| {
        anyhow::anyhow!("no {START_DATE_FIELD} issue field on org {}", repo.owner())
    })?;
    let body = json!({
        "issue_field_values": [{
            "field_id": field.id,
            "value": date,
        }]
    });
    gh::api(
        "POST",
        &format!(
            "repos/{}/issues/{number}/issue-field-values",
            repo.name_with_owner
        ),
        Some(&body),
    )?;
    Ok(date)
}

/// Remove a field's value from an issue. The API is a DELETE on the field
/// id (a POST with `delete: true` is rejected with 422). Clearing a field
/// that has no value is not an error.
pub fn clear_field(repo: &Repo, number: u64, field_name: &str) -> Result<()> {
    let field = find_named_field(repo.owner(), field_name)?
        .ok_or_else(|| anyhow::anyhow!("no {field_name} issue field on org {}", repo.owner()))?;
    match gh::api(
        "DELETE",
        &format!(
            "repos/{}/issues/{number}/issue-field-values/{}",
            repo.name_with_owner, field.id
        ),
        None,
    ) {
        Ok(_) => Ok(()),
        Err(err) if format!("{err:#}").contains("HTTP 404") => Ok(()),
        Err(err) => Err(err),
    }
}

pub fn parse_iso_date(s: &str) -> Result<String> {
    let s = s.trim();
    let mut parts = s.split('-');
    let (Some(ys), Some(ms), Some(ds), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        bail!("date must be YYYY-MM-DD (got {s:?})");
    };
    if ys.len() != 4 {
        bail!("date must be YYYY-MM-DD (got {s:?})");
    }
    let y: u32 = ys
        .parse()
        .map_err(|_| anyhow::anyhow!("date must be YYYY-MM-DD (got {s:?})"))?;
    let m: u32 = ms
        .parse()
        .map_err(|_| anyhow::anyhow!("date must be YYYY-MM-DD (got {s:?})"))?;
    let d: u32 = ds
        .parse()
        .map_err(|_| anyhow::anyhow!("date must be YYYY-MM-DD (got {s:?})"))?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        bail!("date must be YYYY-MM-DD (got {s:?})");
    }
    Ok(format!("{y:04}-{m:02}-{d:02}"))
}

/// Beads-style `--until`: `YYYY-MM-DD`, `today`, `tomorrow`, `+1h` / `+3d` /
/// `+2w`, `next monday`, or a weekday name (the next one). Resolves to an
/// ISO date because GitHub's Start date has no time of day; `+1h` is
/// therefore today or tomorrow, never a moment.
pub fn parse_until(input: &str) -> Result<String> {
    parse_until_at(input, unix_secs_now())
}

fn parse_until_at(input: &str, now_secs: u64) -> Result<String> {
    let s = input.trim().to_ascii_lowercase();
    let today_days = i64::try_from(now_secs / 86_400).unwrap_or(i64::MAX);
    if s.contains('-') && s.len() >= 8 && s.starts_with(|c: char| c.is_ascii_digit()) {
        return parse_iso_date(&s);
    }
    let date = |days: i64| iso_date_from_unix_days(days);
    if let Some(rest) = s.strip_prefix('+') {
        let (num, unit) = rest.split_at(
            rest.trim_end_matches(|c: char| c.is_ascii_alphabetic())
                .len(),
        );
        let n: u64 = num.parse().map_err(|_| {
            anyhow::anyhow!("--until: expected +<n>h, +<n>d, or +<n>w (got {input:?})")
        })?;
        let secs = match unit {
            "h" | "hr" | "hour" | "hours" => n * 3_600,
            "d" | "day" | "days" => n * 86_400,
            "w" | "wk" | "week" | "weeks" => n * 7 * 86_400,
            _ => bail!("--until: unknown unit {unit:?}; use h, d, or w (got {input:?})"),
        };
        return Ok(date(
            i64::try_from((now_secs + secs) / 86_400).unwrap_or(i64::MAX),
        ));
    }
    match s.as_str() {
        "today" | "now" => return Ok(date(today_days)),
        "tomorrow" => return Ok(date(today_days + 1)),
        _ => {}
    }
    let day_name = s.strip_prefix("next ").unwrap_or(&s);
    let Some(target) = weekday_index(day_name) else {
        bail!("--until: expected YYYY-MM-DD, today, tomorrow, +<n>h/d/w, or a weekday (got {input:?})");
    };
    // 1970-01-01 was a Thursday; index Monday = 0.
    let today_wd = (today_days + 3).rem_euclid(7);
    let mut ahead = (target - today_wd).rem_euclid(7);
    if ahead == 0 {
        ahead = 7;
    }
    Ok(date(today_days + ahead))
}

fn weekday_index(name: &str) -> Option<i64> {
    const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    DAYS.iter()
        .position(|d| name.starts_with(d))
        .and_then(|i| i64::try_from(i).ok())
}

fn unix_secs_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn is_future_date(date: &str, today: &str) -> bool {
    date > today
}

pub fn today_utc() -> String {
    iso_date_from_unix_days(unix_days_now())
}

/// ISO date `days` ago (UTC). For search qualifiers like `updated:<DATE`.
pub fn days_ago(days: u32) -> String {
    iso_date_from_unix_days(unix_days_now() - i64::from(days))
}

fn unix_days_now() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    i64::try_from(secs / 86_400).unwrap_or(i64::MAX)
}

/// Civil date from days since Unix epoch (UTC). Howard Hinnant's
/// `civil_from_days`; every intermediate fits comfortably in i64.
pub fn iso_date_from_unix_days(days: i64) -> String {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn is_memory_role(option: Option<&str>) -> bool {
    option.is_some_and(|s| s.eq_ignore_ascii_case(ROLE_MEMORY))
}

/// Ensure the org has a Priority single-select whose options are P0–P4.
///
/// If GitHub already created the default Urgent/High/Medium/Low field, rename
/// those options in place (keep ids) and add P4. Do not invent a second field
/// and do not map P0 to "High" at read/write time.
pub fn ensure_priority_field(org: &str) -> Result<IssueField> {
    if let Some(existing) = find_priority_field(org)? {
        if has_p0_p4(&existing) {
            return Ok(existing);
        }
        patch_to_p0_p4(org, &existing)?;
        return find_priority_field(org)?
            .ok_or_else(|| anyhow::anyhow!("Priority field missing after update"));
    }
    let body = json!({
        "name": PRIORITY_FIELD,
        "description": "gbd priority (P0 highest … P4 lowest)",
        "data_type": "single_select",
        "options": create_options_json(),
    });
    gh::api("POST", &format!("orgs/{org}/issue-fields"), Some(&body))?;
    find_priority_field(org)?.ok_or_else(|| anyhow::anyhow!("Priority field missing after create"))
}

pub fn set_priority(repo: &Repo, number: u64, rank: u8) -> Result<String> {
    let field = find_priority_field(repo.owner())?.ok_or_else(|| {
        anyhow::anyhow!("no {PRIORITY_FIELD} issue field on org {}", repo.owner())
    })?;
    if !has_p0_p4(&field) {
        bail!(
            "{PRIORITY_FIELD} options are {}, not P0–P4. Run: gbd init\nOrg settings: https://github.com/organizations/{}/settings/issue-fields",
            field
                .options
                .iter()
                .map(|o| o.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            repo.owner()
        );
    }
    let option = option_name(rank)?;
    let body = json!({
        "issue_field_values": [{
            "field_id": field.id,
            "value": option,
        }]
    });
    gh::api(
        "POST",
        &format!(
            "repos/{}/issues/{number}/issue-field-values",
            repo.name_with_owner
        ),
        Some(&body),
    )?;
    Ok(option.to_string())
}

pub fn field_value_on_issue(repo: &Repo, number: u64, field_name: &str) -> Result<Option<String>> {
    let vals: Value = gh::api_json(
        "GET",
        &format!(
            "repos/{}/issues/{number}/issue-field-values",
            repo.name_with_owner
        ),
        None,
    )?;
    let Some(arr) = vals.as_array() else {
        return Ok(None);
    };
    for v in arr {
        let name = v
            .get("issue_field_name")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if !name.eq_ignore_ascii_case(field_name) {
            continue;
        }
        if let Some(opt) = v
            .get("single_select_option")
            .and_then(|o| o.get("name"))
            .and_then(|x| x.as_str())
        {
            return Ok(Some(opt.to_string()));
        }
        if let Some(s) = v.get("date").and_then(|x| x.as_str()) {
            return Ok(Some(s.to_string()));
        }
        if let Some(s) = v.get("value").and_then(|x| x.as_str()) {
            return Ok(Some(s.to_string()));
        }
    }
    Ok(None)
}

pub fn priority_on_issue(repo: &Repo, number: u64) -> Result<Option<String>> {
    field_value_on_issue(repo, number, PRIORITY_FIELD)
}

pub fn has_p0_p4(field: &IssueField) -> bool {
    PRIORITY_OPTIONS
        .iter()
        .all(|want| field.options.iter().any(|o| o.name == *want))
}

fn patch_to_p0_p4(org: &str, existing: &IssueField) -> Result<()> {
    let mut options = Vec::new();
    for (i, want) in PRIORITY_OPTIONS.iter().enumerate() {
        let color = PRIORITY_COLORS[i];
        if let Some(opt) = existing.options.iter().find(|o| o.name == *want) {
            let mut item = json!({
                "name": want,
                "color": rest_color(opt.color.as_deref(), color),
                "priority": i + 1,
            });
            if let Some(id) = opt.id {
                item["id"] = json!(id);
            }
            options.push(item);
            continue;
        }
        if let Some(src_name) = github_default_name_for(want) {
            if let Some(opt) = existing.options.iter().find(|o| o.name == src_name) {
                let mut item = json!({
                    "name": want,
                    "color": rest_color(opt.color.as_deref(), color),
                    "priority": i + 1,
                });
                if let Some(id) = opt.id {
                    item["id"] = json!(id);
                }
                options.push(item);
                continue;
            }
        }
        options.push(json!({
            "name": want,
            "color": color,
            "priority": i + 1,
        }));
    }
    let body = json!({ "options": options });
    gh::api(
        "PATCH",
        &format!("orgs/{org}/issue-fields/{}", existing.id),
        Some(&body),
    )?;
    Ok(())
}

fn create_options_json() -> Value {
    PRIORITY_OPTIONS
        .iter()
        .enumerate()
        .map(|(i, name)| {
            json!({
                "name": name,
                "description": format!("gbd {name}"),
                "color": PRIORITY_COLORS[i],
                "priority": i + 1,
            })
        })
        .collect()
}

fn github_default_name_for(want: &str) -> Option<&'static str> {
    DEFAULT_RENAME
        .iter()
        .find(|(_, p)| *p == want)
        .map(|(src, _)| *src)
}

fn rest_color(existing: Option<&str>, fallback: &str) -> String {
    let c = existing.unwrap_or(fallback).to_lowercase();
    match c.as_str() {
        "gray" | "blue" | "green" | "yellow" | "orange" | "red" | "pink" | "purple" => c,
        _ => fallback.to_string(),
    }
}

fn parse_field(v: &Value) -> Option<IssueField> {
    let id = numeric_id(v.get("id")).or_else(|| numeric_id(v.get("full_database_id")))?;
    let name = v.get("name")?.as_str()?.to_string();
    let data_type = v
        .get("data_type")
        .or_else(|| v.get("content_type"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let options = v
        .get("options")
        .and_then(|o| o.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|o| {
                    Some(FieldOption {
                        id: numeric_id(o.get("id")),
                        name: o.get("name")?.as_str()?.to_string(),
                        color: o.get("color").and_then(|x| x.as_str()).map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(IssueField {
        id,
        name,
        data_type,
        options,
    })
}

fn numeric_id(v: Option<&Value>) -> Option<u64> {
    match v? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

pub fn graphql_field_value(node: &Value, field_name: &str) -> Option<String> {
    let nodes = node.get("issueFieldValues")?.get("nodes")?.as_array()?;
    for n in nodes {
        let field = n
            .get("field")
            .and_then(|f| f.get("name"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if !field.eq_ignore_ascii_case(field_name) {
            continue;
        }
        // Single-select values carry the option `name`; date/text/number carry `value`.
        if let Some(s) = n.get("name").and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
        if let Some(s) = n.get("value").and_then(|x| x.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

pub fn graphql_priority_name(node: &Value) -> Option<String> {
    graphql_field_value(node, PRIORITY_FIELD)
}

pub fn graphql_role(node: &Value) -> Option<String> {
    graphql_field_value(node, ROLE_FIELD)
}

pub fn graphql_start_date(node: &Value) -> Option<String> {
    graphql_field_value(node, START_DATE_FIELD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_names_are_p0_p4() {
        assert_eq!(option_name(0).unwrap(), "P0");
        assert_eq!(option_name(4).unwrap(), "P4");
        assert!(option_name(5).is_err());
    }

    #[test]
    fn parse_rank_accepts_p_prefix() {
        assert_eq!(parse_rank("0").unwrap(), 0);
        assert_eq!(parse_rank("P1").unwrap(), 1);
        assert_eq!(parse_rank("p4").unwrap(), 4);
        assert!(parse_rank("High").is_err());
        assert!(parse_rank("Urgent").is_err());
    }

    #[test]
    fn rank_from_option_is_p_only() {
        assert_eq!(rank_from_option("P0"), 0);
        assert_eq!(rank_from_option("P3"), 3);
        assert_eq!(rank_from_option("Urgent"), 2);
        assert_eq!(rank_from_option("High"), 2);
    }

    #[test]
    fn has_p0_p4_requires_all_five() {
        let field = IssueField {
            id: 1,
            name: "Priority".into(),
            data_type: "single_select".into(),
            options: ["P0", "P1", "P2", "P3", "P4"]
                .into_iter()
                .map(|n| FieldOption {
                    id: None,
                    name: n.into(),
                    color: None,
                })
                .collect(),
        };
        assert!(has_p0_p4(&field));
        let defaults = IssueField {
            id: 1,
            name: "Priority".into(),
            data_type: "single_select".into(),
            options: ["Urgent", "High", "Medium", "Low"]
                .into_iter()
                .map(|n| FieldOption {
                    id: None,
                    name: n.into(),
                    color: None,
                })
                .collect(),
        };
        assert!(!has_p0_p4(&defaults));
    }

    #[test]
    fn github_default_name_for_binds_urgent_not_p0() {
        assert_eq!(github_default_name_for("P0"), Some("Urgent"));
        assert_eq!(github_default_name_for("P1"), Some("High"));
        assert_eq!(github_default_name_for("P2"), Some("Medium"));
        assert_eq!(github_default_name_for("P3"), Some("Low"));
        assert_eq!(github_default_name_for("P4"), None);
    }

    #[test]
    fn rename_keeps_default_option_ids() {
        let defaults = IssueField {
            id: 1,
            name: "Priority".into(),
            data_type: "single_select".into(),
            options: [("Urgent", 11u64), ("High", 12), ("Medium", 13), ("Low", 14)]
                .into_iter()
                .map(|(n, id)| FieldOption {
                    id: Some(id),
                    name: n.into(),
                    color: None,
                })
                .collect(),
        };
        let src = github_default_name_for("P0").unwrap();
        let opt = defaults.options.iter().find(|o| o.name == src).unwrap();
        assert_eq!(opt.id, Some(11));
    }

    #[test]
    fn parse_iso_date_normalizes() {
        assert_eq!(parse_iso_date("2026-09-11").unwrap(), "2026-09-11");
        assert_eq!(parse_iso_date("2026-9-7").unwrap(), "2026-09-07");
        assert!(parse_iso_date("09/11/2026").is_err());
        assert!(parse_iso_date("tomorrow").is_err());
    }

    #[test]
    fn until_accepts_beads_forms() {
        // 2000-01-01 00:00 UTC, a Saturday.
        let now = 10_957 * 86_400;
        let at = |s: &str| parse_until_at(s, now).unwrap();
        assert_eq!(at("2026-09-11"), "2026-09-11");
        assert_eq!(at("today"), "2000-01-01");
        assert_eq!(at("tomorrow"), "2000-01-02");
        assert_eq!(
            at("+1h"),
            "2000-01-01",
            "an hour from midnight is still today"
        );
        assert_eq!(at("+36h"), "2000-01-02");
        assert_eq!(at("+3d"), "2000-01-04");
        assert_eq!(at("+2w"), "2000-01-15");
        assert_eq!(at("next monday"), "2000-01-03");
        assert_eq!(at("monday"), "2000-01-03");
        assert_eq!(at("Friday"), "2000-01-07");
        assert_eq!(at("saturday"), "2000-01-08", "same weekday means next week");
        assert!(parse_until_at("someday", now).is_err());
        assert!(parse_until_at("+2y", now).is_err());
        assert!(parse_until_at("09/11/2026", now).is_err());
    }

    #[test]
    fn iso_date_from_unix_epoch() {
        assert_eq!(iso_date_from_unix_days(0), "1970-01-01");
        assert_eq!(iso_date_from_unix_days(1), "1970-01-02");
        assert_eq!(iso_date_from_unix_days(10957), "2000-01-01");
    }

    #[test]
    fn future_start_date_is_deferred() {
        assert!(is_future_date("2026-09-12", "2026-09-11"));
        assert!(!is_future_date("2026-09-11", "2026-09-11"));
        assert!(!is_future_date("2026-09-10", "2026-09-11"));
    }

    #[test]
    fn memory_role_is_memory_option() {
        assert!(is_memory_role(Some("Memory")));
        assert!(is_memory_role(Some("memory")));
        assert!(!is_memory_role(Some("P0")));
        assert!(!is_memory_role(None));
    }

    #[test]
    fn graphql_reads_role_and_start_date() {
        let node = serde_json::json!({
            "issueFieldValues": {
                "nodes": [
                    {
                        "name": "Memory",
                        "field": { "name": "gbd Role" }
                    },
                    {
                        "value": "2026-12-01",
                        "field": { "name": "Start date" }
                    }
                ]
            }
        });
        assert_eq!(graphql_role(&node).as_deref(), Some("Memory"));
        assert_eq!(graphql_start_date(&node).as_deref(), Some("2026-12-01"));
        assert!(is_memory_role(graphql_role(&node).as_deref()));
        assert!(is_future_date(
            graphql_start_date(&node).as_deref().unwrap(),
            "2026-09-11"
        ));
    }
}
