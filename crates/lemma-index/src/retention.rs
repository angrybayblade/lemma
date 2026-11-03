//! Which snapshots an index keeps. A repository's index holds the snapshot
//! of its working directory and snapshots of worktrees at commits
//! workspaces pin; the working directory's is always kept, the others only
//! while recently indexed.

use std::time::Duration;

/// How many commit snapshots to keep, and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    /// The most commit snapshots kept, most recently indexed first.
    pub keep: usize,
    /// How long a commit snapshot is kept after it was last indexed;
    /// forever if `None`.
    pub ttl: Option<Duration>,
}

/// A stored snapshot, as retention sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    /// Its id.
    pub id: i64,
    /// The directory it is of.
    pub root: String,
    /// When it was last indexed, in nanoseconds since the Unix epoch.
    pub indexed_at_ns: i64,
}

/// The snapshots to drop: those of directories other than `own_root`
/// beyond the `keep` most recently indexed, or last indexed more than the
/// TTL before `now_ns`. Sorted by id.
pub fn to_drop(snapshots: &[Kept], own_root: &str, retention: Retention, now_ns: i64) -> Vec<i64> {
    let mut others: Vec<&Kept> = snapshots
        .iter()
        .filter(|snapshot| snapshot.root != own_root)
        .collect();
    others.sort_by(|left, right| {
        right
            .indexed_at_ns
            .cmp(&left.indexed_at_ns)
            .then(right.id.cmp(&left.id))
    });
    let ttl_ns = retention
        .ttl
        .map(|ttl| i64::try_from(ttl.as_nanos()).unwrap_or(i64::MAX));
    let mut dropped: Vec<i64> = others
        .iter()
        .enumerate()
        .filter(|(position, snapshot)| {
            let is_beyond_keep = *position >= retention.keep;
            let is_stale =
                ttl_ns.is_some_and(|ttl| now_ns.saturating_sub(snapshot.indexed_at_ns) > ttl);
            is_beyond_keep || is_stale
        })
        .map(|(_, snapshot)| snapshot.id)
        .collect();
    dropped.sort_unstable();
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_NS: i64 = 24 * 60 * 60 * 1_000_000_000;

    fn kept(id: i64, root: &str, days_ago: i64) -> Kept {
        Kept {
            id,
            root: root.to_owned(),
            indexed_at_ns: 100 * DAY_NS - days_ago * DAY_NS,
        }
    }

    #[test]
    fn the_working_directory_stays_and_commits_go_by_count_and_age() {
        let snapshots = [
            kept(1, "/repo", 90),
            kept(2, "/worktrees/a", 1),
            kept(3, "/worktrees/b", 2),
            kept(4, "/worktrees/c", 3),
            kept(5, "/worktrees/d", 30),
        ];
        let retention = Retention {
            keep: 2,
            ttl: Some(Duration::from_secs(14 * 24 * 60 * 60)),
        };

        let dropped = to_drop(&snapshots, "/repo", retention, 100 * DAY_NS);

        assert_eq!(dropped, [4, 5]);
    }

    #[test]
    fn without_a_ttl_only_the_count_applies() {
        let snapshots = [kept(1, "/repo", 0), kept(2, "/worktrees/a", 400)];
        let retention = Retention { keep: 1, ttl: None };

        assert_eq!(
            to_drop(&snapshots, "/repo", retention, 100 * DAY_NS),
            Vec::<i64>::new()
        );
    }
}
