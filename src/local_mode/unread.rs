//! The call sites of one file that the scan considered and did not state as a
//! row (carrick#3688 U4), as `carrick check` and `check --changed` report
//! them.
//!
//! Folded once, when the read model is built, from the `unread_sites` the
//! blobs carry, so `check` answers without opening a blob. The words are
//! [`crate::request_summary::Stop`]'s and the phrase beside each is the one
//! table every surface reads (`npm/carrick/src/unread-reasons.json`).
//!
//! Lines are the lines of the file as it is on disk now, or the item is not
//! stated: a file the tree has moved past, with no re-check answer, has no
//! lines to give, and saying none would read as "nothing unread".

use std::collections::BTreeMap;

use crate::cloud_storage::CloudRepoData;
use crate::git_state::Hunk;

use super::contract::UnreadItem;
use super::read_model::IndexedUnread;

/// Fold one repo's local blobs into its per-file unread sites, in line order,
/// each (line, reason) once: a file two services include is scanned twice.
pub(super) fn fold(blobs: &[&CloudRepoData]) -> BTreeMap<String, Vec<IndexedUnread>> {
    let mut by_file: BTreeMap<String, Vec<IndexedUnread>> = BTreeMap::new();
    for blob in blobs {
        for site in blob.unread_sites.iter().flatten() {
            let file = super::normalize_relative(std::path::Path::new(&site.file));
            by_file.entry(file).or_default().push(IndexedUnread {
                line: site.line,
                reason: site.reason,
            });
        }
    }
    for sites in by_file.values_mut() {
        sites.sort_by(|a, b| (a.line, a.reason.as_str()).cmp(&(b.line, b.reason.as_str())));
        sites.dedup();
    }
    by_file
}

/// The hunks that turn the file as `commit` holds it into the working tree's.
/// Empty when git cannot say, which marks no line edited: a line is never
/// called edited on a guess.
pub(super) fn edited_hunks(repo_path: &std::path::Path, commit: &str, file: &str) -> Vec<Hunk> {
    crate::git_state::changed_hunks(&repo_path.to_string_lossy(), commit, file, None)
        .unwrap_or_default()
}

/// Whether the working tree's line `line` was added or changed by `hunks`.
fn on_edited_line(hunks: &[Hunk], line: u32) -> bool {
    hunks.iter().any(|hunk| {
        hunk.new_count > 0 && line >= hunk.new_start && line < hunk.new_start + hunk.new_count
    })
}

/// The items for `file`: `sites` as they stand in the file on disk, each
/// marked `edited` when `hunks` changed its line.
pub(super) fn items(file: &str, sites: &[IndexedUnread], hunks: &[Hunk]) -> Vec<UnreadItem> {
    sites
        .iter()
        .map(|site| UnreadItem {
            file: file.to_string(),
            line: site.line,
            reason: site.reason.as_str().to_string(),
            phrase: site.reason.phrase().to_string(),
            edited: on_edited_line(hunks, site.line),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request_summary::Stop;

    fn site(line: u32, reason: Stop) -> IndexedUnread {
        IndexedUnread { line, reason }
    }

    #[test]
    fn an_item_is_edited_only_on_a_line_a_hunk_wrote() {
        let hunks = [Hunk {
            old_start: 4,
            old_count: 1,
            new_start: 4,
            new_count: 2,
        }];
        let rows = items(
            "src/a.ts",
            &[site(3, Stop::ValueCallee), site(5, Stop::Recursion)],
            &hunks,
        );
        assert_eq!(
            rows.iter().map(|row| row.edited).collect::<Vec<_>>(),
            [false, true]
        );
        assert_eq!(rows[0].phrase, Stop::ValueCallee.phrase());
        assert_eq!(rows[1].reason, "recursion");
        assert!(
            !items("src/a.ts", &[site(5, Stop::Recursion)], &[])[0].edited,
            "no hunks, no edited line"
        );
    }
}
