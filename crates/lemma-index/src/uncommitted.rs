//! Which source files differ from the checked-out commit, asking Git as
//! little as possible.

use std::collections::BTreeSet;
use std::path::Path;

use crate::discover;
use crate::git::{self, Head};

/// Most paths Git is asked about by name before a full status is cheaper.
const MAX_NAMED_PATHS: usize = 1000;

/// Every source file that differs from `HEAD`.
pub(crate) fn all(root: &Path, head: &Head) -> Vec<String> {
    in_scope(git::uncommitted_paths(root, head, None).unwrap_or_default())
}

/// Updates `previous` by asking Git only about `paths` (files or whole
/// directories). With the commit unchanged, nothing else can have started
/// or stopped differing from it.
pub(crate) fn refreshed(
    root: &Path,
    head: &Head,
    previous: &[String],
    paths: &[&str],
) -> Vec<String> {
    if paths.is_empty() {
        return previous.to_vec();
    }
    if paths.len() > MAX_NAMED_PATHS {
        return all(root, head);
    }
    let Some(now_uncommitted) = git::uncommitted_paths(root, head, Some(paths)) else {
        return all(root, head);
    };
    let mut merged: BTreeSet<String> = previous
        .iter()
        .filter(|path| !paths.iter().any(|asked| is_within(path, asked)))
        .cloned()
        .collect();
    merged.extend(in_scope(now_uncommitted));
    merged.into_iter().collect()
}

fn in_scope(paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .filter(|path| discover::is_in_scope(path))
        .collect()
}

/// Whether `path` is `asked` or below it, as Git matches a pathspec.
fn is_within(path: &str, asked: &str) -> bool {
    path == asked
        || path
            .strip_prefix(asked)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pathspecs_cover_files_and_whole_directories() {
        assert!(is_within("src/a.py", "src/a.py"));
        assert!(is_within("pkg/deep/a.py", "pkg"));
        assert!(!is_within("pkg2/a.py", "pkg"));
    }
}
