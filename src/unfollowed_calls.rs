//! The member calls the scan could not follow to a definition, written on the
//! index blob as `unfollowed_member_calls` (carrick#813, fix G; the reader is
//! carrick-cloud#2858, `get_callers`).
//!
//! A member call (`store.read()`, `holder.client.notify()`) takes its call
//! edge from the compiler's `call` row ([`crate::call_graph::add_row_edges`]).
//! Where the row names no definition of the service, no edge is stated, and
//! `get_callers` would list nothing for a function that is in fact called.
//! These rows say so: a call the scan saw, named by member, that it could not
//! tie to a definition, with the reason.
//!
//! - **Never an edge, never an input.** Nothing in matching, the type check
//!   or findings reads the rows; the cloud shows them beside `callers`,
//!   labelled "not followed".
//! - **Rows only.** Written from the compiler's position rows; a service
//!   whose walk left none has neither key.
//! - **Cap.** At most [`MAX_PER_SERVICE`] rows per service, sorted by file
//!   then line; `unfollowed_member_calls_total` is the exact count.
//! - **Spelling.** The keys and the reason words are what the reader reads;
//!   `tests/fixtures/unfollowed-calls/contract.json` is the one example both
//!   repos pin by hash (`aws_storage.rs`, `UNFOLLOWED_CONTRACT`).

use serde::{Deserialize, Serialize};

use crate::cloud_storage::CloudRepoData;

/// The most rows one service's blob carries; the total beside them is exact.
pub const MAX_PER_SERVICE: usize = 500;

/// The receiver's source text is cut to this many characters.
pub const RECEIVER_CHARS: usize = 60;

/// The reason for a call whose declaration is a member of an interface or
/// type in the service's own files, which is no definition.
pub const INTERFACE_MEMBER: &str = "interface_member";

/// One member call the scan could not follow (`unfollowed_member_calls[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnfollowedMemberCall {
    /// Repo-relative, `/`-separated, once on the blob.
    pub file: String,
    pub line: u32,
    /// The key of the function the call is written in.
    pub caller: String,
    /// The bare member name (`read` in `store.read()`).
    pub member: String,
    /// The receiver's source text, cut to [`RECEIVER_CHARS`].
    pub receiver: String,
    /// `interface_member`, or the name of the hole the rows left
    /// ([`crate::request_summary::HoleReason`]), such as `untyped_receiver`.
    pub reason: String,
    /// `<file>:<line>` of the declaration, for `interface_member`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_at: Option<String>,
}

/// For `skip_serializing_if` on the total.
pub fn is_zero(total: &u32) -> bool {
    *total == 0
}

/// `text` cut to [`RECEIVER_CHARS`] characters.
pub fn cut_receiver(text: &str) -> String {
    text.trim().chars().take(RECEIVER_CHARS).collect()
}

/// `rows` (paths as walked) as the blob carries them: repo-relative, sorted
/// by file then line, capped, with the exact total beside them.
pub fn place(rows: &[UnfollowedMemberCall], repo_path: &str) -> (Vec<UnfollowedMemberCall>, u32) {
    let mut placed: Vec<UnfollowedMemberCall> = rows
        .iter()
        .map(|row| UnfollowedMemberCall {
            file: crate::engine::repo_relative(&row.file, repo_path),
            declared_at: row
                .declared_at
                .as_deref()
                .map(|at| crate::engine::repo_relative(at, repo_path)),
            ..row.clone()
        })
        .collect();
    placed.sort_by(|a, b| {
        (&a.file, a.line, &a.caller, &a.member, &a.receiver).cmp(&(
            &b.file,
            b.line,
            &b.caller,
            &b.member,
            &b.receiver,
        ))
    });
    let total = u32::try_from(placed.len()).unwrap_or(u32::MAX);
    placed.truncate(MAX_PER_SERVICE);
    (placed, total)
}

/// `rows` less the calls a fact site row stands at (`followed`, repo-relative
/// file and line): the fact layer followed those to a labelled send
/// (carrick#2448).
fn not_followed(
    rows: &[UnfollowedMemberCall],
    repo_path: &str,
    followed: &std::collections::HashSet<(String, u32)>,
) -> Vec<UnfollowedMemberCall> {
    rows.iter()
        .filter(|row| {
            !followed.contains(&(crate::engine::repo_relative(&row.file, repo_path), row.line))
        })
        .cloned()
        .collect()
}

/// Write `rows` on the blob, less the calls a fact site row stands at. No
/// rows leaves both keys as they are (absent).
pub fn attach(
    cloud_data: &mut CloudRepoData,
    rows: &[UnfollowedMemberCall],
    repo_path: &str,
    followed: &std::collections::HashSet<(String, u32)>,
) {
    let rows = not_followed(rows, repo_path, followed);
    if rows.is_empty() {
        return;
    }
    let (placed, total) = place(&rows, repo_path);
    cloud_data.unfollowed_member_calls = placed;
    cloud_data.unfollowed_member_calls_total = total;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(file: &str, line: u32, member: &str) -> UnfollowedMemberCall {
        UnfollowedMemberCall {
            file: file.to_string(),
            line,
            caller: "f".to_string(),
            member: member.to_string(),
            receiver: "r".to_string(),
            reason: "untyped_receiver".to_string(),
            declared_at: None,
        }
    }

    #[test]
    fn a_receiver_is_cut_to_sixty_characters() {
        assert_eq!(
            cut_receiver(&"a".repeat(80)).chars().count(),
            RECEIVER_CHARS
        );
        assert_eq!(cut_receiver("  store "), "store");
        // Characters, not bytes: a cut never splits one.
        assert_eq!(
            cut_receiver(&"é".repeat(80)).chars().count(),
            RECEIVER_CHARS
        );
    }

    #[test]
    fn rows_sort_by_file_then_line_and_the_total_is_exact_past_the_cap() {
        let rows: Vec<_> = (0..(MAX_PER_SERVICE as u32 + 7))
            .rev()
            .map(|line| row("/repo/src/b.ts", line, "m"))
            .chain([row("/repo/src/a.ts", 900, "m")])
            .collect();
        let (placed, total) = place(&rows, "/repo");
        assert_eq!(placed.len(), MAX_PER_SERVICE);
        assert_eq!(total as usize, MAX_PER_SERVICE + 8);
        assert_eq!(placed[0].file, "src/a.ts");
        assert_eq!(placed[1].line, 0);
    }

    #[test]
    fn a_call_a_fact_site_row_stands_at_is_no_unfollowed_call() {
        let rows = [
            row("/repo/src/a.ts", 3, "get"),
            row("/repo/src/a.ts", 4, "put"),
        ];
        let followed = std::collections::HashSet::from([("src/a.ts".to_string(), 3)]);
        let kept = not_followed(&rows, "/repo", &followed);
        let (placed, total) = place(&kept, "/repo");
        assert_eq!(placed.iter().map(|r| r.line).collect::<Vec<_>>(), vec![4]);
        assert_eq!(total, 1);
    }

    #[test]
    fn a_declaration_site_is_made_repo_relative_like_the_call() {
        let mut call = row("/repo/src/a.ts", 3, "read");
        call.declared_at = Some("/repo/src/store.ts:2".to_string());
        let (placed, _) = place(&[call], "/repo");
        assert_eq!(placed[0].declared_at.as_deref(), Some("src/store.ts:2"));
    }
}
