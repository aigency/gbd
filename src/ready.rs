//! Ready work: filter + rank on the open-issue DAG, in process.
//! The snapshot comes from `issue::snapshot`; nothing here talks to gh.
//! See the Data model section of the README.

use serde::Serialize;
use std::collections::{HashMap, HashSet};

use crate::issue::Issue;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReadyItem {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub issue_type: Option<String>,
    pub priority: u8,
    pub unblocks: u32,
    pub height: u32,
    pub parent: Option<u64>,
    pub explain: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    #[default]
    Default,
    Priority,
    Unblocks,
    Path,
}

/// How `rank` filters and orders. `Default` is what `gbd ready` does with no flags.
#[derive(Debug, Clone, Copy, Default)]
pub struct RankOpts {
    /// Beads parity: a blocked parent hides its children.
    pub strict_parent: bool,
    pub include_epics: bool,
    pub sort: SortKey,
}

/// Beads rule 2, optional: parent blocked ⇒ children blocked, depth-capped
/// at GitHub's sub-issue depth (8).
pub fn blocked_set(issues: &[Issue], strict_parent: bool) -> HashSet<u64> {
    let mut blocked: HashSet<u64> = issues
        .iter()
        .filter(|i| i.directly_blocked())
        .map(|i| i.number)
        .collect();
    if !strict_parent {
        return blocked;
    }
    for _ in 0..8 {
        let mut changed = false;
        for issue in issues {
            if blocked.contains(&issue.number) {
                continue;
            }
            if issue.parent.is_some_and(|p| blocked.contains(&p)) {
                blocked.insert(issue.number);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    blocked
}

/// Longest remaining `blocking` chain starting at each issue (DAG DP, cycle-safe).
pub fn critical_path_height(issues: &[Issue]) -> HashMap<u64, u32> {
    fn walk(
        n: u64,
        by_number: &HashMap<u64, &Issue>,
        memo: &mut HashMap<u64, u32>,
        stack: &mut HashSet<u64>,
    ) -> u32 {
        if let Some(&h) = memo.get(&n) {
            return h;
        }
        if !stack.insert(n) {
            return 1;
        }
        let h = match by_number.get(&n) {
            None => 1,
            Some(issue) => {
                let max_child = issue
                    .blocking_open
                    .iter()
                    .filter(|b| by_number.contains_key(b))
                    .map(|&b| walk(b, by_number, memo, stack))
                    .max()
                    .unwrap_or(0);
                1 + max_child
            }
        };
        stack.remove(&n);
        memo.insert(n, h);
        h
    }
    let by_number: HashMap<u64, &Issue> = issues.iter().map(|i| (i.number, i)).collect();
    let mut memo: HashMap<u64, u32> = HashMap::new();
    for issue in issues {
        let mut stack = HashSet::new();
        walk(issue.number, &by_number, &mut memo, &mut stack);
    }
    memo
}

pub fn rank(issues: &[Issue], opts: &RankOpts) -> Vec<ReadyItem> {
    let blocked = blocked_set(issues, opts.strict_parent);
    let heights = critical_path_height(issues);
    let mut items: Vec<ReadyItem> = issues
        .iter()
        .filter(|i| {
            i.is_open()
                && !i.memory
                && !blocked.contains(&i.number)
                && !i.is_deferred()
                && !i.in_progress()
                // Unclaimed is its own predicate, separate from board Status:
                // claim fails on any assigned issue (spec), so an issue someone
                // owns but left at Ready is not offerable to other agents.
                && i.assignees.is_empty()
                && (opts.include_epics || !i.is_epic())
        })
        .map(|i| {
            let unblocks = u32::try_from(i.blocking_open.len()).unwrap_or(u32::MAX);
            let height = heights.get(&i.number).copied().unwrap_or(1);
            ReadyItem {
                number: i.number,
                title: i.title.clone(),
                url: i.url.clone(),
                issue_type: i.issue_type.clone(),
                priority: i.priority,
                unblocks,
                height,
                parent: i.parent,
                explain: format!(
                    "no open blockers; unblocks {unblocks}; path height {height}; P{}",
                    i.priority
                ),
            }
        })
        .collect();

    items.sort_by(|a, b| match opts.sort {
        SortKey::Priority => a.priority.cmp(&b.priority).then(a.number.cmp(&b.number)),
        SortKey::Unblocks => b
            .unblocks
            .cmp(&a.unblocks)
            .then(a.priority.cmp(&b.priority))
            .then(a.number.cmp(&b.number)),
        SortKey::Path => b
            .height
            .cmp(&a.height)
            .then(a.priority.cmp(&b.priority))
            .then(a.number.cmp(&b.number)),
        SortKey::Default => a
            .priority
            .cmp(&b.priority)
            .then(b.unblocks.cmp(&a.unblocks))
            .then(b.height.cmp(&a.height))
            .then(a.number.cmp(&b.number)),
    });
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::State;

    fn issue(n: u64, title: &str) -> Issue {
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
            priority: 2,
            start_date: None,
            deferred: false,
            memory: false,
            status: None,
        }
    }

    fn numbers(items: &[ReadyItem]) -> Vec<u64> {
        items.iter().map(|i| i.number).collect()
    }

    #[test]
    fn child_of_blocked_parent_is_ready_by_default() {
        let mut a = issue(1, "A");
        let mut b = issue(2, "B");
        b.open_blockers = 1;
        b.blocked_by_open = vec![1];
        a.blocking_open = vec![2];
        let mut c = issue(3, "C");
        c.parent = Some(2);
        let nums = numbers(&rank(&[a, b, c], &RankOpts::default()));
        assert!(nums.contains(&3), "C should be ready: {nums:?}");
        assert!(!nums.contains(&2), "B is blocked");
        assert!(nums.contains(&1), "A is ready");
    }

    #[test]
    fn strict_parent_hides_child_of_blocked() {
        let mut a = issue(1, "A");
        let mut b = issue(2, "B");
        b.open_blockers = 1;
        a.blocking_open = vec![2];
        let mut c = issue(3, "C");
        c.parent = Some(2);
        let nums = numbers(&rank(
            &[a, b, c],
            &RankOpts {
                strict_parent: true,
                ..RankOpts::default()
            },
        ));
        assert!(
            !nums.contains(&3),
            "C hidden under --strict-parent: {nums:?}"
        );
        assert!(!nums.contains(&2));
    }

    #[test]
    fn summary_count_alone_marks_blocked() {
        // Cross-repo blockers may not appear in the node list; GitHub's count still does.
        let mut b = issue(2, "B");
        b.open_blockers = 1;
        assert!(rank(&[b], &RankOpts::default()).is_empty());
    }

    #[test]
    fn memory_role_excluded() {
        let mut m = issue(9, "gbd memories");
        m.memory = true;
        assert!(rank(&[m], &RankOpts::default()).is_empty());
    }

    #[test]
    fn closed_excluded() {
        let mut c = issue(9, "done");
        c.state = State::Closed;
        assert!(rank(&[c], &RankOpts::default()).is_empty());
    }

    #[test]
    fn future_start_date_excluded() {
        let mut d = issue(8, "later");
        d.deferred = true;
        assert!(rank(&[d], &RankOpts::default()).is_empty());
    }

    #[test]
    fn board_in_progress_and_deferred_excluded() {
        let mut ip = issue(5, "working");
        ip.status = Some("In Progress".into());
        let mut df = issue(6, "parked");
        df.status = Some("deferred".into());
        let mut ok = issue(7, "todo");
        ok.status = Some("Ready".into());
        let nums = numbers(&rank(&[ip, df, ok], &RankOpts::default()));
        assert_eq!(nums, vec![7]);
    }

    #[test]
    fn epic_excluded_unless_flag() {
        let mut e = issue(4, "Epic");
        e.issue_type = Some("Epic".into());
        assert!(rank(&[e.clone()], &RankOpts::default()).is_empty());
        assert_eq!(
            rank(
                &[e],
                &RankOpts {
                    include_epics: true,
                    ..RankOpts::default()
                }
            )
            .len(),
            1
        );
    }

    #[test]
    fn assigned_is_in_progress() {
        let mut i = issue(5, "claimed");
        i.assignees = vec!["octocat".into()];
        assert!(rank(&[i], &RankOpts::default()).is_empty());
    }

    #[test]
    fn rank_priority_then_unblocks_then_height() {
        let mut p1 = issue(10, "p1 few");
        p1.priority = 1;
        p1.blocking_open = vec![99];
        let mut p0 = issue(11, "p0");
        p0.priority = 0;
        let mut p1_many = issue(12, "p1 many");
        p1_many.priority = 1;
        p1_many.blocking_open = vec![1, 2, 3];
        let nums = numbers(&rank(&[p1, p0, p1_many], &RankOpts::default()));
        assert_eq!(nums, vec![11, 12, 10]);
    }

    #[test]
    fn height_follows_blocking_chain() {
        let mut a = issue(1, "A");
        a.blocking_open = vec![2];
        let mut b = issue(2, "B");
        b.open_blockers = 1;
        b.blocking_open = vec![3];
        let mut c = issue(3, "C");
        c.open_blockers = 1;
        let h = critical_path_height(&[a, b, c]);
        assert_eq!(h[&1], 3);
        assert_eq!(h[&2], 2);
        assert_eq!(h[&3], 1);
    }
}
