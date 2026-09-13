//! `carrick touch` and `carrick check`: answer about one file, from the index.
//!
//! The index holds the rows, their counterparts and the verdicts the type
//! check reached at index time; this module finds the file in it, says how far
//! the tree has moved since, and shapes the answer into the contract. Nothing
//! here calls out, and the only process it starts is `git`.
//!
//! One exception, and it is asked for by name: `check --recheck` on a file the
//! tree has moved past hands off to [`super::recheck`], which re-extracts that
//! file's repo and re-judges it before this module shapes the answer
//! (carrick#1036). The rows in one answer are all fresh or all indexed, and the
//! `recheck` block is what says which.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::contract::{
    CheckOutput, Counterpart, Item, MAX_STALE_FILES, ReadError, ReadFailure, Recheck, SCHEMA,
    STATUS_SCHEMA, StatusOutput, StatusRepo, StatusService, Verdict,
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

/// Whether this call may re-judge a file the tree has moved past, at the cost
/// of a scan of its repo (carrick#1036).
///
/// Off by default and asked for per call rather than inferred from staleness,
/// because the language server runs `check` on every save and a file being
/// edited is always newer than the index: the surface that wants the cost is
/// the post-edit hook, which fires once per completed edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Indexed,
    Recheck,
}

/// Answer about one file.
pub fn answer(
    workspace_root: &Path,
    file: &Path,
    mode: Mode,
    freshness: Freshness,
) -> Result<CheckOutput, ReadFailure> {
    let index = read_index(workspace_root)?;

    let (repo, relative) = index
        .locate_file(file)
        .ok_or_else(|| ReadFailure::new(ReadError::NotInWorkspace))?;
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

    // The one place the answer can stop being the index's. Only `check` asks
    // (a `touch` states no verdict, so a fresh one would cost a scan for
    // nothing), only for a file the tree has moved past, and never for a file
    // that is gone — a deleted producer is already answered from the index,
    // and there is nothing left to extract from it.
    let recheck = match (freshness, mode, stale && !deleted) {
        (Freshness::Recheck, Mode::Check, true) => Some(rechecked(
            workspace_root,
            &repo_root,
            &relative,
            &indexed_at,
        )),
        _ => None,
    };
    let items = match &recheck {
        Some((Some(fresh), _)) => fresh.clone(),
        _ => items,
    };
    // Rows this run computed are not "unresolved since your edit": they ARE
    // the edit. The file is still reported as changed at the top level, which
    // is a fact about the tree either way.
    let rows_are_indexed = stale && !matches!(&recheck, Some((Some(_), _)));
    let recheck = recheck.map(|(_, block)| block);

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
            .map(|item| {
                project(
                    item,
                    mode,
                    rows_are_indexed,
                    deleted,
                    repo,
                    &repo_of_service,
                )
            })
            .collect(),
        recheck,
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
pub fn status(workspace_root: &Path) -> Result<StatusOutput, ReadFailure> {
    let index = read_index(workspace_root)?;

    // Git is asked once per REPO, not once per service: a monorepo's services
    // share a tree, and asking again per service is the difference between two
    // git calls and thirty. What each service is told about, though, is only
    // the part of that answer its own scan reads: a repo-level file
    // (`carrick.json`, a workflow, an editor's settings) belongs to no service
    // and used to be counted under every one of them (carrick#997 item 4).
    let mut services = Vec::new();
    let mut repos = Vec::new();
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

        for service in &repo.services {
            let note = enrichment_note(&service.enrichment, service.boundary.as_ref());
            let mut owned: Vec<String> = changed
                .iter()
                .filter(|file| service.covers(file))
                // Source files only, for the reason the repo line below is
                // filtered: this count is about indexed rows going out of
                // date. It matters MOST here — a single-service repo has no
                // service directory, so its service covers `carrick.json`,
                // the workflow and the `.claude` files, and onboarding read
                // as three files of drift against a minute-old index
                // (carrick#1007 item 5).
                .filter(|file| {
                    crate::file_finder::is_scanned_source(&repo_root.join(file), &repo_root)
                })
                .cloned()
                .collect();
            let total = owned.len();
            let truncated = total > MAX_STALE_FILES;
            owned.truncate(MAX_STALE_FILES);
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
                stale_files: owned,
                stale_files_total: total,
                stale_files_truncated: truncated,
                boundary_lines: boundary_lines(&service.name, &note, service.boundary.as_ref()),
                boundary_note: note,
                boundary: service.boundary.clone(),
            });
        }

        // Stated once, under the repo, and only when there is something to
        // state: these are the files no service's scan reads, so no service
        // should be reporting them.
        //
        // And only the files a scan reads AT ALL. A changed `carrick.json`, a
        // workflow, an editor's settings hold no indexed row, so none of them
        // can make the index stale; counting them read as drift on a tree
        // nobody had touched, which is what onboarding leaves behind
        // (carrick#1007 item 5). `changed_since_index` below is still the whole
        // repo's count, so nothing is hidden — only this line is about rows.
        let mut outside: Vec<String> = changed
            .iter()
            .filter(|file| !repo.services.iter().any(|service| service.covers(file)))
            .filter(|file| crate::file_finder::is_scanned_source(&repo_root.join(file), &repo_root))
            .cloned()
            .collect();
        let outside_total = outside.len();
        let outside_truncated = outside_total > MAX_STALE_FILES;
        outside.truncate(MAX_STALE_FILES);
        repos.push(StatusRepo {
            repo: repo.path.clone(),
            name: repo.name.clone(),
            changed_since_index: changed.len(),
            outside_every_service: outside_total,
            stale_files: outside,
            stale_files_truncated: outside_truncated,
        });
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
        repos,
        // Filled in by the caller: whether a scan is running is a question
        // about this machine right now, and what the last one cost is a file
        // beside the index, not a read of it (carrick#992, carrick#995).
        running_scans: Vec::new(),
        last_scan: None,
        services,
    })
}

/// One indexed row, in the contract's shape.
/// Re-judge one file from the working tree, and say what happened either way.
///
/// The rows come back only when the whole re-check finished: a run that missed
/// its budget leaves the caller with the indexed rows and a block that says
/// when they were computed, because half a re-check is not a fresher answer,
/// it is an answer of unknown age (carrick#1036).
fn rechecked(
    workspace_root: &Path,
    repo_root: &Path,
    relative: &str,
    indexed_at: &str,
) -> (Option<Vec<IndexedItem>>, Recheck) {
    let started = std::time::Instant::now();
    let workspace = match super::workspace::Workspace::load(workspace_root) {
        Ok(workspace) => workspace,
        Err(reason) => {
            return (
                None,
                Recheck {
                    ran: super::recheck::Ran::None.as_str().to_string(),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    stale_since: Some(indexed_at.to_string()),
                    reason: Some(reason),
                },
            );
        }
    };
    match super::recheck::run(&workspace, repo_root, relative) {
        Ok(fresh) => (
            Some(fresh.items),
            Recheck {
                ran: fresh.ran.as_str().to_string(),
                elapsed_ms: fresh.elapsed.as_millis() as u64,
                stale_since: None,
                reason: None,
            },
        ),
        Err(degraded) => (
            None,
            Recheck {
                ran: super::recheck::Ran::None.as_str().to_string(),
                elapsed_ms: degraded.elapsed.as_millis() as u64,
                stale_since: Some(indexed_at.to_string()),
                reason: Some(degraded.reason),
            },
        ),
    }
}

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
    let typed = matches!(mode, Mode::Check) && !deleted;

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
        // The two type texts belong to the comparison, so they ride with the
        // verdict and not beside it: `touch` states neither, and a row whose
        // producer is gone from disk states a removed producer rather than a
        // comparison nothing can still be made (carrick#1033).
        expected_type: typed.then(|| item.expected_type.clone()).flatten(),
        actual_type: typed.then(|| item.actual_type.clone()).flatten(),
        direction: typed.then(|| item.direction.clone()).flatten(),
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
fn read_index(workspace_root: &Path) -> Result<LocalIndex, ReadFailure> {
    let index_file = workspace_root
        .join(super::workspace::INDEX_DIR)
        .join("index.json");
    if !index_file.is_file() {
        return Err(ReadFailure::new(ReadError::NotIndexed));
    }
    // Each refusal carries the sentence that names it. A format mismatch is
    // the one every release that moves READ_MODEL_VERSION creates for every
    // existing workspace, and "the index could not be read" is not an answer a
    // user can act on (carrick#1009).
    let index = LocalIndex::read(&index_file)
        .map_err(|e| ReadFailure::detailed(ReadError::IndexUnreadable, e))?;
    if !super::hosted::can_read_index(&index) {
        return Err(ReadFailure::detailed(
            ReadError::IndexUnreadable,
            "the hosted index belongs to a different or unavailable credential. Run carrick login and carrick index.",
        ));
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
        // The writer named here is the one the ruled first run uses
        // (carrick-cloud#799): the user's own `carrick index`, not a CI run
        // that may be days away or may never be wired up. A CI run on main
        // writes the same index, and says so when it does; what this sentence
        // owes the reader is the next move available to them (carrick#997).
        (HostedState::NoIndexYet, _) => format!(
            "{}; {remote} is connected and has no hosted index yet. Run `carrick index` once to classify them.",
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
        // "No model runs on this machine" is `refresh`'s sentence. A paid scan
        // runs one here, and its hosted row can still be unreplayable
        // afterwards — a dirty tree, which is the ordinary state of a first
        // run — so this fell to the fallback and denied, in the output of the
        // run that had just paid for them, the model rows in the index it was
        // writing (carrick#1023 item 14).
        _ if enrichment.classified_here => {
            "candidates: classified by the model in this machine's own scan, the run that wrote \
             this index."
                .to_string()
        }
        _ => boundary_note(boundary),
    };
    // Not "install the version that wrote the blob": `CACHE_VERSION` moves
    // most weeks, so a hosted blob is behind the installed CLI far more often
    // than the CLI is wrong, and asking for a downgrade asks the reader to
    // give up every fix since. The move available to them is to write a newer
    // blob, from main, where a laptop scan replaces nobody else's row
    // (carrick#1012 item 1, carrick#1020).
    if enrichment.hosted_state == HostedState::VersionMismatch {
        note.push_str(
            " The hosted index is older than this CLI; run `carrick index --detach` once from \
             main to refresh it.",
        );
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

    /// carrick#1012 item 1. `CACHE_VERSION` moves most weeks, so a hosted
    /// blob is behind the installed CLI far more often than the CLI is wrong,
    /// and the sentence that named a version asked the reader to downgrade.
    #[test]
    fn a_hosted_index_behind_this_cli_asks_for_a_newer_blob_not_an_older_cli() {
        let enrichment = crate::local_mode::hosted::ServiceEnrichment {
            hosted_state: crate::local_mode::hosted::HostedState::VersionMismatch,
            ..Default::default()
        };
        let note = enrichment_note(&enrichment, None);
        assert!(
            note.contains(
                "The hosted index is older than this CLI; run `carrick index --detach` once from \
                 main to refresh it."
            ),
            "{note}"
        );
        assert!(!note.contains("npm i -g carrick@"), "{note}");
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
                classified_here: false,
            },
            None,
        )
    }

    /// After a paid scan the model ran HERE, so the fallback sentence — which
    /// is `refresh`'s, and says no model runs on this machine — is false about
    /// the rows in the index that scan just wrote. A dirty tree is the
    /// ordinary first run and lands in exactly this state (carrick#1023
    /// item 14).
    #[test]
    fn a_scan_that_classified_here_never_says_no_model_ran() {
        let note = enrichment_note(
            &super::super::hosted::ServiceEnrichment {
                hosted: Some(hosted_row(Some("laptop"), None, Some(true))),
                hosted_state: super::super::hosted::HostedState::ReadFailed,
                remote: Some("example/api".to_string()),
                failure: Some(
                    "Hosted index was written from a tree with uncommitted changes".to_string(),
                ),
                allowance_sentence: None,
                hosted_cache_version: Some(crate::engine::CACHE_VERSION),
                classified_here: true,
            },
            None,
        );
        assert!(
            !note.contains("not classified locally"),
            "the run that classified them said they were not classified:\n{note}"
        );
        assert!(
            note.contains("classified by the model in this machine's own scan"),
            "{note}"
        );
        // And the hosted row's own problem is still stated: it is why the next
        // read of this checkout cannot replay these answers.
        assert!(note.contains("uncommitted changes"), "{note}");
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

#[cfg(test)]
mod drift_tests {
    use super::*;
    use crate::local_mode::read_model::{IndexedRepo, IndexedService, LocalIndex};

    fn service(directory: Option<&str>, commit: &str) -> IndexedService {
        IndexedService {
            enrichment: Default::default(),
            name: "gateway".to_string(),
            directory: directory.map(str::to_string),
            include: Vec::new(),
            commit: commit.to_string(),
            indexed_at: "2026-09-12T10:00:00Z".to_string(),
            boundary: None,
            routes: 0,
            calls: 0,
        }
    }

    /// The same, in the shape the first run of a single-service repo has: no
    /// service directory, so that service COVERS `carrick.json`, the workflow
    /// and the `.claude` files, and they were three files of drift on its own
    /// line against an index a minute old (carrick#1007 item 5).
    #[test]
    fn onboarding_artefacts_are_not_drift_for_a_single_service_repo_either() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "fixture@carrick.test"],
            vec!["config", "user.name", "fixture"],
        ] {
            assert!(git(repo, &args).is_some());
        }
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/main.ts"), "export const a = 1;").unwrap();
        git(repo, &["add", "."]).unwrap();
        git(repo, &["commit", "-qm", "base"]).unwrap();
        let commit = git(repo, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();

        std::fs::create_dir_all(repo.join(".github/workflows")).unwrap();
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::fs::write(repo.join("carrick.json"), "{}").unwrap();
        std::fs::write(repo.join(".github/workflows/carrick.yml"), "on: push").unwrap();
        std::fs::write(repo.join(".claude/settings.json"), "{}").unwrap();
        std::fs::write(repo.join("src/main.ts"), "export const a = 2;").unwrap();

        let index = LocalIndex {
            hosted_identity: None,
            hosted_workspace: None,
            repos_detected_by: None,
            repos_added: Vec::new(),
            repos_excluded: Vec::new(),
            hosted_source_key: None,
            hosted_checked_at: None,
            version: crate::local_mode::read_model::READ_MODEL_VERSION,
            scanner_version: "test".to_string(),
            indexed_at: "2026-09-12T10:00:00Z".to_string(),
            repos: vec![IndexedRepo {
                path: repo.to_string_lossy().into_owned(),
                // No directory: this service is the whole repo, which is what
                // makes it cover every one of those files.
                services: vec![service(None, &commit)],
                name: "service-repo".to_string(),
                files: Default::default(),
            }],
        };
        let index_dir = repo.join(crate::local_mode::workspace::INDEX_DIR);
        std::fs::create_dir_all(&index_dir).unwrap();
        crate::local_mode::workspace::write_self_ignore(&index_dir).unwrap();
        index.write(&index_dir.join("index.json")).unwrap();

        let answer = status(repo).expect("the index is readable");
        let service = &answer.services[0];
        assert_eq!(service.changed_since_index, 1, "{:?}", service.stale_files);
        assert_eq!(service.stale_files, vec!["src/main.ts".to_string()]);
        // And nothing is quietly moved to the repo line instead.
        assert_eq!(answer.repos[0].outside_every_service, 0);
        let text = answer.render();
        assert!(!text.contains("carrick.json"), "{text}");
        assert!(!text.contains(".claude"), "{text}");
    }

    /// Straight after onboarding, `status` and the session-start hook reported
    /// the files onboarding had just written — `carrick.json`, the workflow,
    /// the editor settings — as the index drifting. None of them holds an
    /// indexed row, so none of them can make one stale (carrick#1007 item 5).
    #[test]
    fn onboarding_artefacts_are_not_reported_as_drift() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "fixture@carrick.test"],
            vec!["config", "user.name", "fixture"],
        ] {
            assert!(git(repo, &args).is_some());
        }
        std::fs::create_dir_all(repo.join("apps/gateway")).unwrap();
        std::fs::write(repo.join("apps/gateway/main.ts"), "export const a = 1;").unwrap();
        git(repo, &["add", "."]).unwrap();
        git(repo, &["commit", "-qm", "base"]).unwrap();
        let commit = git(repo, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();

        // What onboarding leaves behind, plus one real source edit outside
        // every service.
        std::fs::create_dir_all(repo.join(".github/workflows")).unwrap();
        std::fs::create_dir_all(repo.join(".claude")).unwrap();
        std::fs::write(repo.join("carrick.json"), "{}").unwrap();
        std::fs::write(repo.join(".github/workflows/carrick.yml"), "on: push").unwrap();
        std::fs::write(repo.join(".claude/settings.json"), "{}").unwrap();
        std::fs::create_dir_all(repo.join("tools")).unwrap();
        std::fs::write(repo.join("tools/release.ts"), "export const b = 2;").unwrap();

        let index = LocalIndex {
            hosted_identity: None,
            hosted_workspace: None,
            repos_detected_by: None,
            repos_added: Vec::new(),
            repos_excluded: Vec::new(),
            hosted_source_key: None,
            hosted_checked_at: None,
            version: crate::local_mode::read_model::READ_MODEL_VERSION,
            scanner_version: "test".to_string(),
            indexed_at: "2026-09-12T10:00:00Z".to_string(),
            repos: vec![IndexedRepo {
                path: repo.to_string_lossy().into_owned(),
                name: "monorepo".to_string(),
                services: vec![service(Some("apps/gateway"), &commit)],
                files: Default::default(),
            }],
        };
        let index_dir = repo.join(crate::local_mode::workspace::INDEX_DIR);
        std::fs::create_dir_all(&index_dir).unwrap();
        // As a real build does, so `.carrick/` itself is not one of the
        // changes this answer is about.
        crate::local_mode::workspace::write_self_ignore(&index_dir).unwrap();
        index.write(&index_dir.join("index.json")).unwrap();

        let answer = status(repo).expect("the index is readable");
        let stated = &answer.repos[0];
        // Four files moved and one of them is a file a scan reads.
        assert_eq!(stated.changed_since_index, 4, "{:?}", stated.stale_files);
        assert_eq!(stated.outside_every_service, 1, "{:?}", stated.stale_files);
        assert_eq!(stated.stale_files, vec!["tools/release.ts".to_string()]);
        let text = answer.render();
        assert!(!text.contains("carrick.json"), "{text}");
        assert!(!text.contains(".claude"), "{text}");
        assert!(
            text.contains("monorepo: 1 file(s) changed outside every service"),
            "{text}"
        );
    }
}
