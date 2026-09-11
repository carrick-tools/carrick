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

use super::contract::{
    CheckOutput, Counterpart, Item, MAX_STALE_FILES, ReadError, SCHEMA, STATUS_SCHEMA,
    StatusOutput, StatusService, Verdict,
};
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
    let index = read_index(workspace_root)?;

    let (repo, relative) = index.locate_file(file).ok_or(ReadError::NotInWorkspace)?;
    let items = repo.files.get(&relative).cloned().unwrap_or_default();

    // The service is the file's own when the index holds rows for it, and
    // otherwise the service whose directory contains the file. Most files in a
    // monorepo hold no row at all, so the directory is what answers in the
    // common case; picking the repo's first service instead would answer for
    // every rowless file in a 34-service tree with whichever one sorted first.
    let service_row = items
        .first()
        .and_then(|item| {
            repo.services
                .iter()
                .find(|indexed| indexed.name == item.service)
        })
        .or_else(|| repo.service_for(&relative));
    let service = service_row
        .map(|service| service.name.clone())
        .unwrap_or_else(|| repo.name.clone());

    let commit = service_row
        .map(|service| service.commit.clone())
        .unwrap_or_default();
    let indexed_at = service_row
        .map(|service| service.indexed_at.clone())
        .unwrap_or_else(|| index.indexed_at.clone());

    let repo_root = PathBuf::from(&repo.path);
    // Git is the whole answer where git can give one: it compares CONTENT, and
    // a rewrite that lands the same bytes is not a change. The mtime signal is
    // the fallback for the tree git cannot speak for, and it is built into the
    // same set rather than OR-ed on top of it, so `stale` and
    // `changed_since_index` can never contradict each other (carrick#857).
    let changed = changed_since(&repo_root, &commit).unwrap_or_else(|| {
        let mut changed = HashSet::new();
        if newer_than_index(&repo_root, &relative, &indexed_at) {
            changed.insert(relative.clone());
        }
        changed
    });
    let stale = changed.contains(&relative);
    let deleted = !repo_root.join(&relative).exists();

    let boundary = service_row.and_then(|service| service.boundary.clone());
    let enrichment = service_row
        .map(|s| s.enrichment.clone())
        .unwrap_or_default();
    let boundary_note = enrichment_note(&enrichment, boundary.as_ref());
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
        hosted: enrichment.hosted,
        hosted_state: enrichment.hosted_state,
        hosted_checked_at: index.hosted_checked_at.clone(),
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

/// Answer about the whole workspace: what is indexed, at which commit, and how
/// far each repo has moved since (carrick#728).
///
/// The shape a session-start surface needs, and deliberately not a `check`
/// response with the file left out: every `carrick.check/0` answer is about one
/// file, and a reader that always has one should not have to defend against a
/// response that does not.
pub fn status(workspace_root: &Path) -> Result<StatusOutput, ReadError> {
    let index = read_index(workspace_root)?;

    // Git is asked once per REPO, not once per service: a monorepo's services
    // share a tree, and asking again per service is the difference between two
    // git calls and thirty.
    let mut services = Vec::new();
    for repo in &index.repos {
        let repo_root = PathBuf::from(&repo.path);
        let commit = repo
            .services
            .first()
            .map(|service| service.commit.clone())
            .unwrap_or_default();
        // No mtime fallback here: `status` answers for a whole repo, and the
        // fallback is a stat of ONE file. Where git cannot answer, the repo
        // reports nothing known to have changed rather than a walk of the tree.
        let mut changed: Vec<String> = changed_since(&repo_root, &commit)
            .unwrap_or_default()
            .into_iter()
            .collect();
        changed.sort();
        let total = changed.len();
        let truncated = total > MAX_STALE_FILES;
        changed.truncate(MAX_STALE_FILES);

        for service in &repo.services {
            let note = enrichment_note(&service.enrichment, service.boundary.as_ref());
            services.push(StatusService {
                hosted: service.enrichment.hosted.clone(),
                hosted_state: service.enrichment.hosted_state.clone(),
                service: service.name.clone(),
                repo: repo.path.clone(),
                index_commit: service.commit.clone(),
                indexed_at: service.indexed_at.clone(),
                routes: service.routes,
                calls: service.calls,
                changed_since_index: total,
                stale_files: changed.clone(),
                stale_files_total: total,
                stale_files_truncated: truncated,
                boundary_lines: boundary_lines(&service.name, &note, service.boundary.as_ref()),
                boundary_note: note,
                boundary: service.boundary.clone(),
            });
        }
    }

    Ok(StatusOutput {
        repos_detected_by: index.repos_detected_by.clone(),
        repos_added: index.repos_added.clone(),
        repos_excluded: index.repos_excluded.clone(),
        hosted_checked_at: index.hosted_checked_at.clone(),
        schema: STATUS_SCHEMA.to_string(),
        workspace: workspace_root.to_string_lossy().into_owned(),
        indexed_at: index.indexed_at.clone(),
        scanner_version: index.scanner_version.clone(),
        services,
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
            remote: counterpart.remote.clone(),
            repo: if counterpart.remote.is_some() {
                None
            } else {
                repo_of_service.get(&counterpart.service).cloned()
            },
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
/// `state` is the type layer's word and only that, in the same vocabulary as
/// `verdict_state` on the PR-result payload (carrick#731): whether the
/// compiler reached a verdict, whether it tried and a side would not resolve,
/// or whether no type verdict bears on the row. Whether the tree has moved
/// since the index is `stale` at the top level; it is said once for the file
/// rather than smuggled into every row's state, and it is repeated in the
/// detail so a reader of one row alone still learns it.
///
/// A deleted producer is decided here rather than at index time: the index
/// records what the tree held, and whether a route's file still exists is a
/// fact about the tree right now.
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
            // A removed producer is a routing fact; no type verdict bears
            // on it.
            state: "not_checked".to_string(),
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
        state: stored.state.clone(),
        result: stored.result.clone(),
        detail: if stale {
            format!(
                "{} (unresolved since your edit: {} has changed since it was indexed, so this \
                 describes the tree the index was built on)",
                stored.detail, repo.name
            )
        } else {
            stored.detail.clone()
        },
    }
}

/// The read model, or why there is no answer.
fn read_index(workspace_root: &Path) -> Result<LocalIndex, ReadError> {
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
    if !super::hosted::can_read_index(&index) {
        eprintln!(
            "carrick: the hosted index belongs to a different or unavailable credential. Run carrick login and carrick index."
        );
        return Err(ReadError::IndexUnreadable);
    }
    Ok(index)
}

/// The repo-relative paths that differ from the commit the index was built at:
/// everything committed since, plus everything uncommitted, plus what git does
/// not track at all.
///
/// Two cheap git calls and no walk of the tree. `None` is "git could not
/// answer" — no commit recorded, a tarball with no repository, a commit a
/// rebase has since dropped, or no `git` on the machine — and is deliberately
/// distinct from `Some(empty)`, "git answered, and nothing has changed". The
/// two used to collapse into one empty set, so a caller could not tell a clean
/// tree from an unanswerable one and had to OR in a signal that is wrong
/// whenever git can speak (carrick#857).
pub(crate) fn changed_since(repo: &Path, commit: &str) -> Option<HashSet<String>> {
    if commit.is_empty() || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let diff = git(
        repo,
        &["diff", "--name-only", "--no-renames", "-z", commit, "--"],
    )?;
    let untracked = git(repo, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut changed = HashSet::new();
    for text in [diff, untracked] {
        changed.extend(
            text.split('\0')
                .map(str::to_string)
                .filter(|l| !l.is_empty()),
        );
    }
    Some(changed)
}

/// Whether the file itself has been written since the index was built.
///
/// The fallback for a tree git cannot answer for, and only that: an mtime says
/// a write happened, never that the bytes differ, so an editor autosave, a
/// formatter that changed nothing, or a `git checkout --` that restored the
/// same content all read as changed. Where git can compare content, its answer
/// is used instead (carrick#857).
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
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
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

/// Who wrote the hosted row, as a clause inside the existing parenthesis.
///
/// Empty on a CI row and on a row stored before the fields existed, so those
/// sentences read exactly as they did. A laptop row says so, and names the
/// login when the row records one; "tree not clean" is a separate condition
/// and appears whenever `dirty` is true, because a CI row is never dirty and
/// a reader should not have to work that out. Wire contract: carrick-cloud
/// `docs/internal/reference/laptop-scan-seam.md` §6.2.
fn hosted_provenance_clause(hosted: &super::hosted::HostedProvenance) -> String {
    let mut clause = String::new();
    if hosted.source.as_deref() == Some("laptop") {
        clause.push_str(" by a laptop scan");
        if let Some(login) = hosted.uploaded_by.as_deref().filter(|s| !s.is_empty()) {
            clause.push_str(&format!(" from @{login}"));
        }
    }
    if hosted.dirty == Some(true) {
        clause.push_str(", tree not clean");
    }
    clause
}

/// Hosted provenance feeds the same boundary renderer used by every local
/// surface. Read commands never probe the network to compose this sentence.
pub fn enrichment_note(
    enrichment: &super::hosted::ServiceEnrichment,
    boundary: Option<&crate::boundary::ServiceBoundary>,
) -> String {
    use super::hosted::HostedState;
    let remote = enrichment.remote.as_deref().unwrap_or("this repo");
    let mut note = match (&enrichment.hosted_state, &enrichment.hosted) {
        (HostedState::Enriched, Some(hosted)) => format!(
            "candidates: from the hosted index at {} (indexed {}{}); {} file(s) changed since then hold facts only.",
            hosted.commit.chars().take(7).collect::<String>(),
            hosted.indexed_at,
            hosted_provenance_clause(hosted),
            boundary
                .and_then(|b| b.candidates_withheld_changed_files)
                .unwrap_or(0)
        ),
        (HostedState::VersionMismatch, Some(hosted)) => format!(
            "hosted index written by carrick {} (cache {:?}), this binary is {} (cache {}); candidates not replayed.",
            hosted.scanner_version.as_deref().unwrap_or("unknown"),
            enrichment.hosted_cache_version,
            env!("CARGO_PKG_VERSION"),
            crate::engine::CACHE_VERSION
        ),
        (HostedState::CommitMissing, Some(hosted)) => format!(
            "hosted index at {}, which this clone does not have; candidates not replayed. Run git fetch.",
            hosted.commit.chars().take(7).collect::<String>()
        ),
        (HostedState::NoIndexYet, _) => format!(
            "{}; {remote} is connected and has no hosted index yet. The first CI run on main writes it, and the next carrick index reads it.",
            super::NOT_CLASSIFIED_LOCALLY
        ),
        (HostedState::NotConnected, _) => format!(
            "{}; {remote} is not connected to a Carrick project.",
            super::NOT_CLASSIFIED_LOCALLY
        ),
        (HostedState::NotSignedIn, _) => format!(
            "{}; not signed in, so the hosted index was not read.",
            boundary_note(boundary)
        ),
        _ => boundary_note(boundary),
    };
    if enrichment.hosted_state == HostedState::VersionMismatch
        && let Some(version) = enrichment
            .hosted
            .as_ref()
            .and_then(|h| h.scanner_version.as_ref())
        && semver::Version::parse(version).is_ok()
    {
        note.push_str(&format!(
            " Run npm i -g carrick@{version}, or re-index main with the current Action."
        ));
    }
    if let Some(failure) = &enrichment.failure {
        if let Some(hosted) = &enrichment.hosted {
            note.push_str(&format!(
                " Hosted copy from {} retained; could not refresh: {failure}.",
                hosted.indexed_at
            ));
        } else {
            note.push_str(&format!(" Could not refresh the hosted index: {failure}."));
        }
    }
    if let Some(allowance) = &enrichment.allowance_sentence {
        note.push(' ');
        note.push_str(allowance);
    }
    note
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

#[cfg(test)]
mod hosted_change_tests {
    use super::*;

    #[test]
    fn hosted_diff_includes_staged_untracked_deleted_and_unusual_paths() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "fixture@carrick.test"],
            vec!["config", "user.name", "fixture"],
        ] {
            assert!(git(repo, &args).is_some());
        }
        for file in ["staged.ts", "deleted.ts", "odd\nname.ts"] {
            std::fs::write(repo.join(file), "export const value = 1;").unwrap();
        }
        git(repo, &["add", "."]).unwrap();
        git(repo, &["commit", "-qm", "base"]).unwrap();
        let commit = git(repo, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(repo.join("staged.ts"), "export const value = 2;").unwrap();
        git(repo, &["add", "staged.ts"]).unwrap();
        std::fs::remove_file(repo.join("deleted.ts")).unwrap();
        std::fs::write(repo.join("odd\nname.ts"), "changed").unwrap();
        std::fs::write(repo.join("new.ts"), "new").unwrap();
        assert_eq!(
            changed_since(repo, commit.trim()).unwrap(),
            HashSet::from([
                "staged.ts".into(),
                "deleted.ts".into(),
                "odd\nname.ts".into(),
                "new.ts".into()
            ])
        );
        assert!(changed_since(repo, "--output=elsewhere").is_none());
    }
    fn hosted_row(
        source: Option<&str>,
        uploaded_by: Option<&str>,
        dirty: Option<bool>,
    ) -> super::super::hosted::HostedProvenance {
        super::super::hosted::HostedProvenance {
            commit: "4f2a1c9000000000000000000000000000000000".to_string(),
            indexed_at: "2026-09-11".to_string(),
            scanner_version: Some("0.3.60".to_string()),
            project: "payments".to_string(),
            source: source.map(str::to_string),
            uploaded_by: uploaded_by.map(str::to_string),
            dirty,
        }
    }

    fn note_for(hosted: super::super::hosted::HostedProvenance) -> String {
        enrichment_note(
            &super::super::hosted::ServiceEnrichment {
                hosted: Some(hosted),
                hosted_state: super::super::hosted::HostedState::Enriched,
                remote: Some("example/api".to_string()),
                failure: None,
                allowance_sentence: None,
                hosted_cache_version: Some(crate::engine::CACHE_VERSION),
            },
            None,
        )
    }

    /// A laptop row says whose laptop and whether the tree was clean, inside
    /// the parenthesis the sentence already had. Nothing else about the
    /// sentence moves (§6.2).
    #[test]
    fn a_laptop_row_names_its_uploader_and_an_unclean_tree() {
        assert_eq!(
            note_for(hosted_row(Some("laptop"), Some("ihor"), Some(true))),
            "candidates: from the hosted index at 4f2a1c9 (indexed 2026-09-11 by a laptop scan \
             from @ihor, tree not clean); 0 file(s) changed since then hold facts only."
        );
    }

    /// A CI row and a row written before the fields existed render exactly
    /// today's sentence. A reader must not be able to tell the two apart,
    /// because the index cannot.
    #[test]
    fn a_ci_row_and_a_pre_field_row_render_the_unchanged_sentence() {
        let today = "candidates: from the hosted index at 4f2a1c9 (indexed 2026-09-11); \
                     0 file(s) changed since then hold facts only.";
        assert_eq!(
            note_for(hosted_row(Some("ci"), Some("ihor"), Some(false))),
            today
        );
        assert_eq!(note_for(hosted_row(None, None, None)), today);
    }

    /// A laptop row with no recorded login still says it was a laptop scan:
    /// where the row came from is the fact that matters, and the login is the
    /// detail.
    #[test]
    fn a_laptop_row_without_a_login_still_says_it_was_a_laptop_scan() {
        let note = note_for(hosted_row(Some("laptop"), None, Some(false)));
        assert!(note.contains("by a laptop scan)"), "{note}");
        assert!(!note.contains('@'), "{note}");
        assert!(!note.contains("tree not clean"), "{note}");
    }
}
