//! `carrick touch` and `carrick check`: answer about one file, from the index.
//!
//! Everything here is a read. The index holds the rows, their counterparts and
//! the verdicts the type check reached at index time; this module finds the
//! file in it, says how far the tree has moved since, and shapes the answer
//! into the contract. Nothing re-extracts, nothing calls out, and the only
//! process started is `git`.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::contract::{CheckOutput, Counterpart, Item, ReadError, SCHEMA, Verdict};
use super::read_model::{IndexedItem, IndexedRepo, LocalIndex};

/// Which of the two read-only commands is asking. The only difference is
/// whether verdicts are stated: `touch` answers "what is on the other side of
/// this file", `check` answers "and what did the type check conclude".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Touch,
    Check,
}

/// Answer about one file.
pub fn answer(workspace_root: &Path, file: &Path, mode: Mode) -> Result<CheckOutput, ReadError> {
    let index_file = workspace_root
        .join(super::workspace::INDEX_DIR)
        .join("index.json");
    if !index_file.is_file() {
        return Err(ReadError::NotIndexed);
    }
    let index = LocalIndex::read(&index_file).map_err(|e| {
        eprintln!("carrick: {e}");
        ReadError::IndexUnreadable
    })?;

    let (repo, relative) = index.locate_file(file).ok_or(ReadError::NotInWorkspace)?;
    let items = repo.files.get(&relative).cloned().unwrap_or_default();

    // The service is the file's own when the index holds rows for it, and the
    // repo's single service otherwise. A file with no rows in a monorepo has
    // no service to name, so the repo answers for it.
    let service = items
        .first()
        .map(|item| item.service.clone())
        .or_else(|| repo.services.first().map(|service| service.name.clone()))
        .unwrap_or_else(|| repo.name.clone());
    let service_row = repo.services.iter().find(|indexed| indexed.name == service);

    let commit = service_row
        .map(|service| service.commit.clone())
        .unwrap_or_default();
    let indexed_at = service_row
        .map(|service| service.indexed_at.clone())
        .unwrap_or_else(|| index.indexed_at.clone());

    let repo_root = PathBuf::from(&repo.path);
    let changed = changed_since(&repo_root, &commit);
    let stale = changed.contains(&relative) || newer_than_index(&repo_root, &relative, &indexed_at);
    let deleted = !repo_root.join(&relative).exists();

    let boundary = service_row.and_then(|service| service.boundary.clone());
    let boundary_note = boundary_note(boundary.as_ref());
    let boundary_lines = boundary_lines(&service, &boundary_note, boundary.as_ref());

    // Where every other repo in the workspace lives, so a counterpart's
    // repo-relative file can be opened without guessing which directory it
    // hangs off (carrick#709).
    let repo_of_service: BTreeMap<String, String> = index
        .repos
        .iter()
        .flat_map(|indexed| {
            indexed
                .services
                .iter()
                .map(|service| (service.name.clone(), indexed.path.clone()))
        })
        .collect();

    Ok(CheckOutput {
        schema: SCHEMA.to_string(),
        file: relative,
        repo: repo.path.clone(),
        service,
        index_commit: commit,
        indexed_at,
        scanner_version: index.scanner_version.clone(),
        changed_since_index: changed.len(),
        stale,
        deleted,
        items: items
            .iter()
            .map(|item| project(item, mode, stale, deleted, repo, &repo_of_service))
            .collect(),
        boundary,
        boundary_note,
        boundary_lines,
    })
}

/// One indexed row, in the contract's shape.
fn project(
    item: &IndexedItem,
    mode: Mode,
    stale: bool,
    deleted: bool,
    repo: &IndexedRepo,
    repo_of_service: &BTreeMap<String, String>,
) -> Item {
    let counterparts: Vec<Counterpart> = item
        .counterparts
        .iter()
        .map(|counterpart| Counterpart {
            role: counterpart.role.clone(),
            service: counterpart.service.clone(),
            file: counterpart.file.clone(),
            line: counterpart.line,
            repo: repo_of_service.get(&counterpart.service).cloned(),
        })
        .collect();

    let verdict = match mode {
        // `touch` states where things are, and nothing about whether they
        // agree. Every verdict is null, by contract.
        Mode::Touch => None,
        Mode::Check => Some(verdict_for(item, &counterparts, stale, deleted, repo)),
    };

    Item {
        kind: item.kind.as_str().to_string(),
        method: item.method.clone(),
        path: item.path.clone(),
        line: item.line,
        col: item.col,
        source: item.source.as_str().to_string(),
        resolution_source: item.resolution_source.map(|source| {
            serde_json::to_value(source)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{source:?}"))
        }),
        evidence: item.evidence.clone(),
        counterparts,
        verdict,
    }
}

/// The verdict `check` states for one row.
///
/// A deleted producer is decided here rather than at index time: the index
/// records what the tree held, and whether a route's file still exists is a
/// fact about the tree right now. Everything else was computed by the type
/// check at index time and is repeated, marked `unresolved` when the file has
/// moved under it.
fn verdict_for(
    item: &IndexedItem,
    counterparts: &[Counterpart],
    stale: bool,
    deleted: bool,
    repo: &IndexedRepo,
) -> Verdict {
    if deleted && item.kind == super::read_model::ItemKind::Route {
        let consumers = counterparts.len();
        return Verdict {
            state: "resolved".to_string(),
            result: Some("producer_removed".to_string()),
            detail: format!(
                "this file is gone and the index still serves {} {} here: producer removed, {} consumer(s)",
                item.method, item.path, consumers
            ),
        };
    }

    let Some(stored) = &item.verdict else {
        return Verdict {
            state: "not_checked".to_string(),
            result: None,
            detail: match counterparts.len() {
                0 => format!(
                    "nothing in this workspace pairs with {} {}, so nothing was compared",
                    item.method, item.path
                ),
                _ => format!(
                    "matched, and the type check reached no verdict on {} {} — its types are not both resolved",
                    item.method, item.path
                ),
            },
        };
    };

    Verdict {
        state: if stale {
            "unresolved".to_string()
        } else {
            "resolved".to_string()
        },
        result: Some(stored.result.clone()),
        detail: if stale {
            format!(
                "{} (unresolved since your edit: {} has changed since it was indexed)",
                stored.detail, repo.name
            )
        } else {
            stored.detail.clone()
        },
    }
}

/// The repo-relative paths that differ from the commit the index was built at:
/// everything committed since, plus everything uncommitted, plus what git does
/// not track at all.
///
/// Two cheap git calls and no walk of the tree. A repo git cannot answer for (a
/// tarball, a commit that no longer exists after a rebase) yields an empty set,
/// which reads as "nothing known to have changed" — the same thing the absence
/// of a git repo has always meant here.
fn changed_since(repo: &Path, commit: &str) -> HashSet<String> {
    let mut changed = HashSet::new();
    if commit.is_empty() {
        return changed;
    }
    if let Some(text) = git(repo, &["diff", "--name-only", commit]) {
        changed.extend(text.lines().map(str::to_string).filter(|l| !l.is_empty()));
    }
    if let Some(text) = git(repo, &["ls-files", "--others", "--exclude-standard"]) {
        changed.extend(text.lines().map(str::to_string).filter(|l| !l.is_empty()));
    }
    changed
}

/// Whether the file itself has been written since the index was built. Covers
/// the edit git cannot see: a file in `.gitignore`, or a tree that is not a
/// git repo at all.
fn newer_than_index(repo: &Path, relative: &str, indexed_at: &str) -> bool {
    let Ok(indexed_at) = chrono::DateTime::parse_from_rfc3339(indexed_at) else {
        return false;
    };
    let Ok(metadata) = std::fs::metadata(repo.join(relative)) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    chrono::DateTime::<chrono::Utc>::from(modified) > indexed_at
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// The sentence a local answer ends on. It names what is absent, with the
/// count the scan kept, so a thin index never reads as "there is no API here".
pub fn boundary_note(boundary: Option<&crate::boundary::ServiceBoundary>) -> String {
    let unclassified = boundary
        .map(|boundary| boundary.unemitted_literal_candidates)
        .unwrap_or(0);
    format!(
        "{}. A route registered on a typed receiver (`app.get(\"/x\", h)`) and a call whose \
         URL is built at the call site are classified by the model in the hosted index and \
         are absent here: {unclassified} route-literal call site(s) counted and unclassified \
         in this service.",
        super::NOT_CLASSIFIED_LOCALLY
    )
}

/// The boundary exactly as the terminal prints it: the sentence about what a
/// local index cannot hold, then the counts this scan kept.
///
/// One renderer, so a hook that prints these bytes and a developer running
/// `carrick check` read the same sentence about the same number — the
/// alternative is two ports of one wording, drifting (carrick#709).
pub fn boundary_lines(
    service: &str,
    note: &str,
    boundary: Option<&crate::boundary::ServiceBoundary>,
) -> Vec<String> {
    let mut lines = vec![format!("boundary ({service}): {note}")];
    if let Some(boundary) = boundary {
        lines.extend(
            boundary
                .lines(service)
                .into_iter()
                .map(|line| format!("  {line}")),
        );
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boundary::ServiceBoundary;

    #[test]
    fn the_boundary_note_names_what_is_absent_and_counts_it() {
        let boundary = ServiceBoundary {
            unemitted_literal_candidates: 7,
            ..Default::default()
        };
        let note = boundary_note(Some(&boundary));
        assert!(note.contains("not classified locally"), "{note}");
        assert!(note.contains("7 route-literal call site(s)"), "{note}");
    }

    #[test]
    fn the_note_is_still_stated_when_the_scan_counted_none() {
        // "Nothing counted" is not "nothing missing": the sentence has to be
        // there either way, or a thin index reads as an empty service.
        let note = boundary_note(None);
        assert!(note.contains("not classified locally"), "{note}");
        assert!(note.contains("0 route-literal call site(s)"), "{note}");
    }
}
