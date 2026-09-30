//! Board statuses and their Beads categories.
//!
//! A status is a Project column. The category (`active`, `wip`, `frozen`,
//! `done`) is what `ready`, `status`, `stale`, and the Blocked rule reason
//! about. Built-in columns have fixed categories; anything else is a custom
//! status from `.gbd.yml`.

use std::collections::BTreeSet;
use std::fmt::{self, Write as _};

use anyhow::{bail, Result};

pub const STATUS_READY: &str = "Ready";
pub const STATUS_IN_PROGRESS: &str = "In Progress";
/// Maintained from GitHub's open-blocker count, never set by hand.
pub const STATUS_BLOCKED: &str = "Blocked";
pub const STATUS_DEFERRED: &str = "Deferred";
pub const STATUS_DONE: &str = "Done";

/// Built-in names, plus Beads' `open` and `closed`. `pinned` and `hooked`
/// are ordinary customs (`pinned: frozen`, `hooked: wip`) so an import can
/// name those columns.
const RESERVED: [&str; 7] = [
    "ready",
    "in_progress",
    "blocked",
    "deferred",
    "done",
    "open",
    "closed",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Active,
    Wip,
    Frozen,
    Done,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Active => "active",
            Category::Wip => "wip",
            Category::Frozen => "frozen",
            Category::Done => "done",
        }
    }

    pub fn parse(word: &str) -> Result<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "active" => Ok(Category::Active),
            "wip" => Ok(Category::Wip),
            "frozen" => Ok(Category::Frozen),
            "done" => Ok(Category::Done),
            other => {
                bail!("status category {other:?} must be active, wip, done, or frozen")
            }
        }
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One custom status, in the order it was configured (that is the column
/// order within its category).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CustomStatus {
    pub name: String,
    pub category: Category,
}

/// A `--status` word resolved to the column gbd will set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Title Case column name (`in_review` → `In Review`).
    pub column: String,
    pub category: Category,
    /// Config key, or the built-in word (`ready`, `in_progress`, …).
    pub key: String,
}

/// One column `ensure_statuses` wants on the board, in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedColumn {
    pub name: String,
    pub color: &'static str,
}

/// `in_review` → `In Review`.
pub fn title_case(key: &str) -> String {
    key.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `In Review` → `in_review`. The reverse of [`title_case`] for the names
/// gbd writes, so a column renamed only in case still matches.
pub fn key_form(column: &str) -> String {
    column
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("_")
}

/// The board option is this column: same text ignoring case, or the config
/// key against the Title Case form (and the other way around).
pub fn same_column(option_name: &str, wanted_column: &str) -> bool {
    option_name.eq_ignore_ascii_case(wanted_column)
        || option_name.eq_ignore_ascii_case(&key_form(wanted_column))
        || key_form(option_name).eq_ignore_ascii_case(&key_form(wanted_column))
}

pub fn builtin_category(column: &str) -> Option<Category> {
    if column.eq_ignore_ascii_case(STATUS_READY) {
        Some(Category::Active)
    } else if column.eq_ignore_ascii_case(STATUS_IN_PROGRESS)
        || column.eq_ignore_ascii_case(STATUS_BLOCKED)
    {
        Some(Category::Wip)
    } else if column.eq_ignore_ascii_case(STATUS_DEFERRED) {
        Some(Category::Frozen)
    } else if column.eq_ignore_ascii_case(STATUS_DONE) {
        Some(Category::Done)
    } else {
        None
    }
}

/// Category of a board option. Built-ins win over a custom of the same
/// column name (reserved names cannot be customs anyway).
pub fn category_of(column: &str, customs: &[CustomStatus]) -> Option<Category> {
    if let Some(cat) = builtin_category(column) {
        return Some(cat);
    }
    customs
        .iter()
        .find(|c| same_column(&c.name, column) || same_column(&title_case(&c.name), column))
        .map(|c| c.category)
}

/// Column order: Blocked, Deferred, frozen customs, Ready, active customs,
/// In Progress, wip customs, Done, done customs. A `wip` custom lands
/// between In Progress and Done.
pub fn column_order(customs: &[CustomStatus]) -> Vec<OrderedColumn> {
    let mut out = Vec::new();
    let mut push = |name: &str, color: &'static str| {
        out.push(OrderedColumn {
            name: name.to_string(),
            color,
        });
    };
    push(STATUS_BLOCKED, "RED");
    push(STATUS_DEFERRED, "GRAY");
    for c in customs.iter().filter(|c| c.category == Category::Frozen) {
        push(&title_case(&c.name), "GRAY");
    }
    push(STATUS_READY, "GREEN");
    for c in customs.iter().filter(|c| c.category == Category::Active) {
        push(&title_case(&c.name), "GREEN");
    }
    push(STATUS_IN_PROGRESS, "YELLOW");
    for c in customs.iter().filter(|c| c.category == Category::Wip) {
        push(&title_case(&c.name), "YELLOW");
    }
    push(STATUS_DONE, "PURPLE");
    for c in customs.iter().filter(|c| c.category == Category::Done) {
        push(&title_case(&c.name), "PURPLE");
    }
    out
}

/// Column identity of a config key. Title Case then back to a key collapses
/// repeated and edge underscores, so `in__review` and `in_review` are one
/// column (`In  Review` and `In Review` match through [`same_column`]).
fn column_key(name: &str) -> String {
    key_form(&title_case(name))
}

/// Built-in columns, their keys, and the words [`resolve`] already accepts
/// as those columns (`todo`, `claimed`, `defer`, …).
fn collides_with_builtin(key: &str) -> bool {
    const ALIASES: [&str; 4] = ["todo", "inprogress", "claimed", "defer"];
    if key.is_empty() || RESERVED.contains(&key) || ALIASES.contains(&key) {
        return true;
    }
    let column = title_case(key);
    [
        STATUS_READY,
        STATUS_IN_PROGRESS,
        STATUS_BLOCKED,
        STATUS_DEFERRED,
        STATUS_DONE,
    ]
    .iter()
    .any(|builtin| same_column(&column, builtin))
}

pub fn parse_one(name: &str, category: &str) -> Result<CustomStatus> {
    let name = name.trim();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        bail!("status name {name:?} must match [a-z0-9_]");
    }
    let canonical = column_key(name);
    if canonical.is_empty() {
        bail!("status name {name:?} is not a column name");
    }
    if canonical != name {
        if collides_with_builtin(&canonical) {
            bail!(
                "status name {name:?} matches built-in column {canonical:?}; those names are reserved"
            );
        }
        bail!(
            "status name {name:?} normalizes to {canonical:?}; use that name so two keys cannot share a column"
        );
    }
    if collides_with_builtin(name) {
        bail!(
            "status name {name:?} is reserved (ready, in_progress, blocked, deferred, done, open, closed)"
        );
    }
    Ok(CustomStatus {
        name: name.to_string(),
        category: Category::parse(category)?,
    })
}

fn ensure_unique(list: &[CustomStatus]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for c in list {
        if !seen.insert(column_key(&c.name)) {
            bail!("status {:?} is listed twice", c.name);
        }
    }
    Ok(())
}

/// `triage:active,in_review:wip`. Empty is an empty list. This replaces the
/// whole custom set; it does not merge.
pub fn parse_custom_list(text: &str) -> Result<Vec<CustomStatus>> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((name, category)) = part.split_once(':') else {
            bail!("status {part:?} must be name:category");
        };
        out.push(parse_one(name, category)?);
    }
    ensure_unique(&out)?;
    Ok(out)
}

pub fn custom_list_string(list: &[CustomStatus]) -> String {
    list.iter()
        .map(|c| format!("{}:{}", c.name, c.category))
        .collect::<Vec<_>>()
        .join(",")
}

/// Beads `status.custom`, which may name a built-in. Reserved entries are
/// dropped (gbd already knows them); a bad category is an error.
pub fn parse_beads_custom(text: &str) -> Result<Vec<CustomStatus>> {
    let text = text.trim().trim_matches('"');
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((name, category)) = part.split_once(':') else {
            bail!("status {part:?} must be name:category");
        };
        let name_l = name.trim().to_ascii_lowercase();
        if RESERVED.contains(&name_l.as_str()) {
            continue;
        }
        out.push(parse_one(&name_l, category)?);
    }
    ensure_unique(&out)?;
    Ok(out)
}

fn configured_names(customs: &[CustomStatus]) -> String {
    let mut names = vec![
        "ready".to_string(),
        "in_progress".to_string(),
        "deferred".to_string(),
        "done".to_string(),
    ];
    names.extend(customs.iter().map(|c| c.name.clone()));
    names.join(", ")
}

pub fn resolve(word: &str, customs: &[CustomStatus]) -> Result<Resolved> {
    let key = word.trim().to_ascii_lowercase().replace('-', "_");
    if key == "blocked" {
        bail!(
            "blocked is not set by hand; it follows from open blockers (gbd dep add <n> <blocker>)"
        );
    }
    let builtin = match key.as_str() {
        "open" | "ready" | "todo" => Some((STATUS_READY, Category::Active, "ready")),
        "in_progress" | "inprogress" | "claimed" => {
            Some((STATUS_IN_PROGRESS, Category::Wip, "in_progress"))
        }
        "deferred" | "defer" => Some((STATUS_DEFERRED, Category::Frozen, "deferred")),
        "done" | "closed" => Some((STATUS_DONE, Category::Done, "done")),
        _ => None,
    };
    if let Some((column, category, key)) = builtin {
        return Ok(Resolved {
            column: column.to_string(),
            category,
            key: key.to_string(),
        });
    }
    if let Some(c) = customs.iter().find(|c| c.name == key) {
        return Ok(Resolved {
            column: title_case(&c.name),
            category: c.category,
            key: c.name.clone(),
        });
    }
    bail!(
        "unknown status {key:?}; configured: {}",
        configured_names(customs)
    );
}

/// Where blocker maintenance moves this card, if it moves at all.
///
/// An `active` column (Ready or a custom) with an open blocker goes to
/// Blocked. Blocked with no open blocker goes back to Ready — the column
/// it came from is not remembered. `wip`, `frozen`, and `done` never move.
pub fn blocker_destination(
    column: &str,
    open_blocker: bool,
    customs: &[CustomStatus],
) -> Option<&'static str> {
    if column.eq_ignore_ascii_case(STATUS_BLOCKED) {
        return if open_blocker {
            None
        } else {
            Some(STATUS_READY)
        };
    }
    if category_of(column, customs) == Some(Category::Active) && open_blocker {
        return Some(STATUS_BLOCKED);
    }
    None
}

/// `stale` skips frozen columns, and a future Start date when the issue
/// has no column (that date is the no-board form of deferred).
pub fn parked(column: Option<&str>, date_deferred: bool, customs: &[CustomStatus]) -> bool {
    match column.and_then(|c| category_of(c, customs)) {
        Some(Category::Frozen) => true,
        Some(_) => false,
        None => date_deferred,
    }
}

/// Where a Beads status lands. `gbd` config wins when it names the status;
/// otherwise the Beads category (from the export's `status.custom`, or the
/// known extras `pinned` = frozen and `hooked` = wip) maps to the built-in
/// of that category. An active status with an open blocker is Blocked.
/// Anything still unknown is active.
pub fn column_for_beads_custom(
    name: &str,
    open_blocker: bool,
    gbd: &[CustomStatus],
    beads_cfg: &[CustomStatus],
) -> String {
    let key = name.to_ascii_lowercase();
    let configured = gbd
        .iter()
        .find(|c| c.name == key)
        .or_else(|| beads_cfg.iter().find(|c| c.name == key));
    let (category, column) = if let Some(c) = gbd.iter().find(|c| c.name == key) {
        (c.category, title_case(&c.name))
    } else {
        let category = configured
            .map(|c| c.category)
            .or(match key.as_str() {
                "pinned" => Some(Category::Frozen),
                "hooked" => Some(Category::Wip),
                _ => None,
            })
            .unwrap_or(Category::Active);
        let column = match category {
            Category::Active => STATUS_READY.to_string(),
            Category::Wip => STATUS_IN_PROGRESS.to_string(),
            Category::Frozen => STATUS_DEFERRED.to_string(),
            Category::Done => STATUS_DONE.to_string(),
        };
        (category, column)
    };
    if category == Category::Active && open_blocker {
        STATUS_BLOCKED.to_string()
    } else {
        column
    }
}

/// What `gbd prime` prints so an agent knows this repo's vocabulary.
pub fn vocabulary(customs: &[CustomStatus]) -> String {
    let mut out = String::from(
        "ready [active], in_progress [wip], blocked [wip, derived], deferred [frozen], done [done]\n",
    );
    if customs.is_empty() {
        out.push_str("custom: none (`gbd config set status.custom \"name:category,…\"`)\n");
    } else {
        let list = customs
            .iter()
            .map(|c| format!("{} [{}] → {}", c.name, c.category, title_case(&c.name)))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "custom: {list}");
    }
    out
}

pub fn known_beads_status(name: &str, beads_cfg: &[CustomStatus]) -> bool {
    let key = name.to_ascii_lowercase();
    matches!(key.as_str(), "pinned" | "hooked") || beads_cfg.iter().any(|c| c.name == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(name: &str, category: Category) -> CustomStatus {
        CustomStatus {
            name: name.into(),
            category,
        }
    }

    #[test]
    fn title_case_splits_on_underscores() {
        assert_eq!(title_case("in_review"), "In Review");
        assert_eq!(title_case("pinned"), "Pinned");
        assert_eq!(title_case("p1"), "P1");
    }

    #[test]
    fn names_match_the_key_or_the_title() {
        assert!(same_column("in_review", "In Review"));
        assert!(same_column("In Review", "In Review"));
        assert!(same_column("IN REVIEW", "in_review"));
        assert!(!same_column("In Progress", "In Review"));
    }

    #[test]
    fn categories_of_builtins_and_customs() {
        let customs = [
            custom("in_review", Category::Wip),
            custom("pinned", Category::Frozen),
        ];
        assert_eq!(category_of("Ready", &customs), Some(Category::Active));
        assert_eq!(category_of("Blocked", &customs), Some(Category::Wip));
        assert_eq!(category_of("In Review", &customs), Some(Category::Wip));
        assert_eq!(category_of("in_review", &customs), Some(Category::Wip));
        assert_eq!(category_of("Pinned", &customs), Some(Category::Frozen));
        assert_eq!(category_of("Icebox", &customs), None);
    }

    #[test]
    fn customs_sit_beside_their_category() {
        let customs = [
            custom("triage", Category::Active),
            custom("in_review", Category::Wip),
            custom("pinned", Category::Frozen),
            custom("archived", Category::Done),
        ];
        let names: Vec<_> = column_order(&customs).into_iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "Blocked",
                "Deferred",
                "Pinned",
                "Ready",
                "Triage",
                "In Progress",
                "In Review",
                "Done",
                "Archived",
            ]
        );
    }

    #[test]
    fn validation_rejects_reserved_names_and_bad_categories() {
        assert!(parse_one("blocked", "wip").is_err());
        assert!(parse_one("open", "active").is_err());
        assert!(parse_one("InReview", "wip").is_err());
        assert!(parse_one("triage", "nope").is_err());
        assert_eq!(
            parse_one("pinned", "frozen").unwrap(),
            custom("pinned", Category::Frozen)
        );
        assert!(parse_one("hooked", "wip").is_ok());
        let err = parse_custom_list("triage:active,triage:wip").unwrap_err();
        assert!(format!("{err:#}").contains("twice"), "{err:#}");
        let collapsed = parse_one("in__progress", "active").unwrap_err();
        assert!(
            format!("{collapsed:#}").contains("in_progress"),
            "{collapsed:#}"
        );
        let alias = parse_one("todo", "frozen").unwrap_err();
        assert!(format!("{alias:#}").contains("reserved"), "{alias:#}");
        assert!(parse_one("_pinned", "frozen").is_err());
        assert!(parse_one("pinned_", "frozen").is_err());
        let shared = parse_custom_list("in_review:wip,in__review:active").unwrap_err();
        assert!(
            format!("{shared:#}").contains("in_review"),
            "two keys, one column: {shared:#}"
        );
    }

    #[test]
    fn resolve_lists_what_is_configured() {
        let customs = [
            custom("in_review", Category::Wip),
            custom("triage", Category::Active),
        ];
        let r = resolve("in-review", &customs).unwrap();
        assert_eq!(r.column, "In Review");
        assert_eq!(r.category, Category::Wip);
        assert_eq!(resolve("ready", &[]).unwrap().column, STATUS_READY);
        assert!(resolve("blocked", &customs).is_err());
        let err = resolve("nonsense", &customs).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("in_review") && msg.contains("triage"), "{msg}");
        assert!(msg.contains("ready"), "{msg}");
    }

    #[test]
    fn blocker_moves_only_active_and_blocked() {
        let customs = [
            custom("triage", Category::Active),
            custom("in_review", Category::Wip),
        ];
        assert_eq!(
            blocker_destination("Triage", true, &customs),
            Some(STATUS_BLOCKED)
        );
        assert_eq!(
            blocker_destination("Ready", true, &customs),
            Some(STATUS_BLOCKED)
        );
        assert_eq!(blocker_destination("Triage", false, &customs), None);
        assert_eq!(
            blocker_destination("Blocked", false, &customs),
            Some(STATUS_READY)
        );
        assert_eq!(blocker_destination("Blocked", true, &customs), None);
        assert_eq!(blocker_destination("In Review", true, &customs), None);
        assert_eq!(blocker_destination("Deferred", true, &customs), None);
        assert_eq!(blocker_destination("Done", true, &customs), None);
    }

    #[test]
    fn pinned_and_hooked_fall_back_to_their_beads_categories() {
        assert_eq!(
            column_for_beads_custom("pinned", false, &[], &[]),
            STATUS_DEFERRED
        );
        assert_eq!(
            column_for_beads_custom("pinned", true, &[], &[]),
            STATUS_DEFERRED,
            "frozen is not pulled to Blocked"
        );
        assert_eq!(
            column_for_beads_custom("Pinned", false, &[custom("pinned", Category::Frozen)], &[]),
            "Pinned"
        );
        assert_eq!(
            column_for_beads_custom("hooked", false, &[], &[]),
            STATUS_IN_PROGRESS
        );
        assert_eq!(
            column_for_beads_custom("triage", true, &[custom("triage", Category::Active)], &[]),
            STATUS_BLOCKED
        );
        assert_eq!(
            column_for_beads_custom("triage", false, &[], &[custom("triage", Category::Active)]),
            STATUS_READY
        );
        assert_eq!(
            column_for_beads_custom("someday", false, &[], &[]),
            STATUS_READY
        );
    }
}
