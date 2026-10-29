//! Pausing a server and measuring its memory, for a long-lived process that
//! keeps servers between lookups.
//!
//! A server runs in its own process group, so signals reach the processes it
//! starts too, such as the `tsserver` behind typescript-language-server.
//! Signals and memory readings go through the standard `kill` and `ps`
//! commands, which behave the same on macOS and Linux.

use std::collections::HashMap;
use std::process::{Command, Stdio};

/// Sends a signal, such as `STOP` or `CONT`, to every process in the group
/// led by `group_id`. Returns whether it was delivered.
pub(crate) fn signal_group(group_id: u32, signal: &str) -> bool {
    Command::new("kill")
        .args(["-s", signal, "--", &format!("-{group_id}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Resident memory, in bytes, of each process in `roots` and everything it
/// started. Processes that are not running are left out.
pub(crate) fn tree_memory_bytes(roots: &[u32]) -> HashMap<u32, u64> {
    let Ok(output) = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,rss="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return HashMap::new();
    };
    let table = parse_process_table(&String::from_utf8_lossy(&output.stdout));
    roots
        .iter()
        .filter_map(|root| subtree_kilobytes(&table, *root).map(|kb| (*root, kb * 1024)))
        .collect()
}

/// One row of `ps -o pid=,ppid=,rss=`: process, parent, resident kilobytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProcessRow {
    id: u32,
    parent: u32,
    resident_kb: u64,
}

fn parse_process_table(text: &str) -> Vec<ProcessRow> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some(ProcessRow {
                id: fields.next()?.parse().ok()?,
                parent: fields.next()?.parse().ok()?,
                resident_kb: fields.next()?.parse().ok()?,
            })
        })
        .collect()
}

/// Total resident kilobytes of `root` and its descendants, or `None` if
/// `root` is not running.
fn subtree_kilobytes(table: &[ProcessRow], root: u32) -> Option<u64> {
    let mut children: HashMap<u32, Vec<&ProcessRow>> = HashMap::new();
    for row in table {
        children.entry(row.parent).or_default().push(row);
    }
    let start = table.iter().find(|row| row.id == root)?;
    let mut total = 0;
    let mut pending = vec![start];
    while let Some(row) = pending.pop() {
        total += row.resident_kb;
        if let Some(below) = children.get(&row.id) {
            pending.extend(below.iter().copied());
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_tree_sums_its_descendants() {
        let table =
            parse_process_table("  10   1  100\n  11  10   50\n  12  11   25\n  20   1  999\n");

        assert_eq!(subtree_kilobytes(&table, 10), Some(175));
        assert_eq!(subtree_kilobytes(&table, 12), Some(25));
        assert_eq!(subtree_kilobytes(&table, 30), None);
    }

    #[test]
    fn malformed_rows_are_skipped() {
        let table = parse_process_table("garbage\n  5 1 10\n");

        assert_eq!(
            table,
            [ProcessRow {
                id: 5,
                parent: 1,
                resident_kb: 10
            }]
        );
    }
}
