//! `carrick index` and `carrick refresh`: build the local read model.
//!
//! Two phases, the same two the offline cross-repo harness uses, driven as
//! subprocesses of this binary:
//!
//! 1. **Per repo, isolated.** Each repo is scanned on its own with the
//!    cross-repo download forced empty, so no sibling's data reaches a repo's
//!    own scan, and its index blob is written to `.carrick/repos/`. This is
//!    the slow phase, and it is where `refresh` does its work for one service.
//! 2. **Join.** One more run reads every blob back, builds the analyzer over
//!    all of them, runs the type check, and writes what it found to
//!    `.carrick/join.json`.
//!
//! The indexer then folds the blobs (boundaries, commits) and the join
//! (operations, edges, verdicts) into `.carrick/index.json`, which is the only
//! file the read-only commands open.
//!
//! Subprocesses rather than in-process calls for the same reason the harness
//! uses them: a scan keeps process-global state (the health counters, the
//! sidecar it spawns for one repo's tsconfig), and five repos in one process
//! would share it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use crate::cloud_storage::CloudRepoData;

use super::join::{JoinedFinding, JoinedMatch, JoinedOperation, LocalJoin, Role};
use super::read_model::{
    Counterpart, IndexedItem, IndexedRepo, IndexedService, ItemKind, LocalIndex,
    READ_MODEL_VERSION, Source, StoredVerdict,
};
use super::workspace::Workspace;

/// What one `index` or `refresh` did, for the summary the command prints.
pub struct IndexOutcome {
    pub index: LocalIndex,
    pub scanned: Vec<String>,
    pub elapsed_secs: f64,
}

/// Index every repo in the workspace, or re-index the one holding `only`.
pub fn run(workspace: &Workspace, only: Option<&str>) -> Result<IndexOutcome, String> {
    let started = Instant::now();
    let blobs = workspace.blobs_dir();

    let targets: Vec<PathBuf> = match only {
        None => {
            // A full index starts from nothing, so a repo that has left the
            // workspace leaves the answers with it.
            let _ = std::fs::remove_dir_all(&blobs);
            workspace.repos.clone()
        }
        Some(name) => vec![repo_for_service(workspace, &blobs, name)?],
    };
    std::fs::create_dir_all(&blobs).map_err(|e| format!("{}: {e}", blobs.display()))?;
    super::workspace::write_self_ignore(&workspace.index_dir())
        .map_err(|e| format!("could not write the .carrick/.gitignore: {e}"))?;

    let exe = std::env::current_exe()
        .map_err(|e| format!("could not find the carrick binary to run a scan with: {e}"))?;

    let mut scanned = Vec::new();
    for repo in &targets {
        let name = repo_label(repo);
        eprintln!("indexing {name}...");
        scan_repo(&exe, repo, &blobs)?;
        scanned.push(name);
    }

    let join_target = workspace
        .repos
        .first()
        .ok_or_else(|| "the workspace resolved to no repos".to_string())?;
    let join = join(&exe, join_target, &blobs, &workspace.join_file())?;

    let index = build(workspace, &blobs, &join)?;
    index
        .write(&workspace.index_file())
        .map_err(|e| format!("could not write {}: {e}", workspace.index_file().display()))?;
    // The hand-off has been folded in; leaving it would invite a reader to
    // treat a stale copy of the join as the index.
    let _ = std::fs::remove_file(workspace.join_file());

    Ok(IndexOutcome {
        index,
        scanned,
        elapsed_secs: started.elapsed().as_secs_f64(),
    })
}

/// The repo holding a named service, for `refresh --service`. The blobs know
/// which repo each service belongs to, so a refresh names a service and the
/// index answers with its repo.
fn repo_for_service(workspace: &Workspace, blobs: &Path, service: &str) -> Result<PathBuf, String> {
    let known = read_blobs(blobs)?;
    let owner = known
        .iter()
        .find(|blob| service_id(blob) == service)
        .map(|blob| blob.repo_name.clone());
    let Some(repo_name) = owner else {
        let names: Vec<String> = known.iter().map(service_id).collect();
        return Err(format!(
            "no indexed service named '{service}'. This workspace holds: {}",
            if names.is_empty() {
                "nothing yet — run `carrick index` first".to_string()
            } else {
                names.join(", ")
            }
        ));
    };
    workspace
        .repos
        .iter()
        .find(|repo| repo_label(repo) == repo_name)
        .cloned()
        .ok_or_else(|| {
            format!("service '{service}' was indexed from repo '{repo_name}', which the workspace no longer lists")
        })
}

/// Phase 1 for one repo.
fn scan_repo(exe: &Path, repo: &Path, blobs: &Path) -> Result<(), String> {
    let mut command = Command::new(exe);
    command
        .arg(repo)
        .env(crate::cloud_storage::CACHE_DIR_ENV, blobs)
        .env(crate::cloud_storage::ISOLATE_ENV, "1")
        .env(super::NO_MODEL_ENV, "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("CARRICK_OUTPUT_JSON")
        .env_remove(super::JOIN_OUT_ENV);
    strip_ci_env(&mut command);
    run_scan(command, &format!("scan of {}", repo.display()))
}

/// Phase 2: join every blob, and hand the result back.
fn join(exe: &Path, repo: &Path, blobs: &Path, out: &Path) -> Result<LocalJoin, String> {
    let mut command = Command::new(exe);
    command
        .arg(repo)
        .env(crate::cloud_storage::CACHE_DIR_ENV, blobs)
        .env(super::NO_MODEL_ENV, "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env(super::JOIN_OUT_ENV, out)
        .env_remove(crate::cloud_storage::ISOLATE_ENV)
        .env_remove("CARRICK_OUTPUT_JSON");
    strip_ci_env(&mut command);
    run_scan(command, "workspace join")?;

    let text = std::fs::read_to_string(out)
        .map_err(|e| format!("the join wrote no result to {}: {e}", out.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("could not read the join result: {e}"))
}

/// Run one scan subprocess, and say what it printed if it failed. Output is
/// captured rather than inherited: a scan writes a report to stdout that means
/// nothing here, and the useful half of a failure is the last few lines of
/// stderr.
fn run_scan(mut command: Command, what: &str) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|e| format!("could not start the {what}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail: Vec<&str> = stderr.lines().rev().take(12).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    Err(format!("the {what} failed:\n{}", tail.join("\n")))
}

/// Strip the ambient CI context, exactly as the offline harness does. Without
/// it, `GITHUB_REPOSITORY` names every repo in the workspace after the one
/// whose shell this ran in, and every blob clobbers the last.
fn strip_ci_env(command: &mut Command) {
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "GITHUB_EVENT_PATH",
        "CI",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        command.env_remove(var);
    }
}

/// The directory name a scan of this path records as the repo name.
fn repo_label(repo: &Path) -> String {
    repo.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo.to_string_lossy().into_owned())
}

/// `service_name ?? repo_name`, the identity every cross-repo surface uses.
fn service_id(blob: &CloudRepoData) -> String {
    blob.service_name
        .clone()
        .unwrap_or_else(|| blob.repo_name.clone())
}

/// Every blob in the cache dir, in a stable order.
fn read_blobs(blobs: &Path) -> Result<Vec<CloudRepoData>, String> {
    let Ok(entries) = std::fs::read_dir(blobs) else {
        return Ok(Vec::new());
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    paths.sort();
    let mut blobs = Vec::new();
    for path in paths {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let blob: CloudRepoData = serde_json::from_str(&text)
            .map_err(|e| format!("could not parse {}: {e}", path.display()))?;
        blobs.push(blob);
    }
    Ok(blobs)
}

/// Fold the blobs and the join into the read model.
fn build(workspace: &Workspace, blobs_dir: &Path, join: &LocalJoin) -> Result<LocalIndex, String> {
    let blobs = read_blobs(blobs_dir)?;
    let now = timestamp();

    let mut repos: Vec<IndexedRepo> = Vec::new();
    for repo_path in &workspace.repos {
        let name = repo_label(repo_path);
        if repos.iter().any(|known| known.name == name) {
            return Err(format!(
                "two repos in this workspace are both named '{name}'. Local mode keys a scan's \
                 output by directory name, so one would overwrite the other — rename one \
                 directory, or list only one of them"
            ));
        }
        let services: Vec<IndexedService> = blobs
            .iter()
            .filter(|blob| blob.repo_name == name)
            .map(|blob| IndexedService {
                name: service_id(blob),
                commit: blob.commit_hash.clone(),
                indexed_at: now.clone(),
                boundary: blob.boundary.clone(),
                routes: blob.endpoints.len(),
                calls: blob.calls.len(),
            })
            .collect();
        repos.push(IndexedRepo {
            path: repo_path.to_string_lossy().into_owned(),
            name,
            services,
            files: BTreeMap::new(),
        });
    }

    // Which repo each service belongs to, so an operation lands in the right
    // repo's file map.
    let mut owner: BTreeMap<String, usize> = BTreeMap::new();
    for (position, repo) in repos.iter().enumerate() {
        for service in &repo.services {
            owner.insert(service.name.clone(), position);
        }
    }

    // Where each producer operation is written, keyed as an edge names it.
    let mut producers: BTreeMap<(String, String), Vec<&JoinedOperation>> = BTreeMap::new();
    for operation in &join.operations {
        if operation.role == Role::Producer {
            producers
                .entry((operation.service.clone(), operation.key.clone()))
                .or_default()
                .push(operation);
        }
    }

    for operation in &join.operations {
        let Some(&position) = owner.get(&operation.service) else {
            // A service in the join that belongs to no repo in the workspace
            // is a blob left over from a workspace that has changed; the full
            // index clears them, so this is only reachable mid-refresh.
            continue;
        };
        let counterparts = counterparts_for(operation, &join.matches, &producers);
        let verdict = verdict_for(operation, &join.matches, &join.findings);
        let item = IndexedItem {
            kind: match operation.role {
                Role::Producer => ItemKind::Route,
                Role::Consumer => ItemKind::Call,
            },
            service: operation.service.clone(),
            key: operation.key.clone(),
            method: operation.method.clone(),
            path: operation.path.clone(),
            line: operation.line,
            col: None,
            source: Source::of(operation.resolution_source),
            resolution_source: operation.resolution_source,
            evidence: evidence_for(operation),
            counterparts,
            verdict,
        };
        repos[position]
            .files
            .entry(operation.file.clone())
            .or_default()
            .push(item);
    }

    for repo in &mut repos {
        for items in repo.files.values_mut() {
            items.sort_by(|a, b| {
                (a.line, &a.kind.as_str(), &a.method, &a.path).cmp(&(
                    b.line,
                    &b.kind.as_str(),
                    &b.method,
                    &b.path,
                ))
            });
        }
    }

    Ok(LocalIndex {
        version: READ_MODEL_VERSION,
        scanner_version: join.scanner_version.clone(),
        indexed_at: now,
        repos,
    })
}

/// The other side of this operation's contract, across every repo indexed.
///
/// A producer names its consumers by the edges its key carries. A consumer
/// names its producers through the edges its own CALL SITE carries: an edge
/// records where the consumer's call is, so two calls to the same operation in
/// one service each answer for themselves rather than sharing one list (#260).
fn counterparts_for(
    operation: &JoinedOperation,
    matches: &[JoinedMatch],
    producers: &BTreeMap<(String, String), Vec<&JoinedOperation>>,
) -> Vec<Counterpart> {
    let mut found: Vec<Counterpart> = Vec::new();
    match operation.role {
        Role::Producer => {
            for edge in matches {
                if edge.producer_service != operation.service || edge.producer_key != operation.key
                {
                    continue;
                }
                found.push(Counterpart {
                    role: consumer_role(&edge.relationship).to_string(),
                    service: edge.consumer_service.clone(),
                    file: edge.consumer_file.clone().unwrap_or_default(),
                    line: edge.consumer_line,
                });
            }
        }
        Role::Consumer => {
            for edge in matches {
                if edge.consumer_service != operation.service || edge.consumer_key != operation.key
                {
                    continue;
                }
                if !edge_is_at(edge, operation) {
                    continue;
                }
                // One key can be served by more than one site (carrick#718),
                // and each of them is a real answer to "where does this go".
                let sites = producers
                    .get(&(edge.producer_service.clone(), edge.producer_key.clone()))
                    .cloned()
                    .unwrap_or_default();
                if sites.is_empty() {
                    found.push(Counterpart {
                        role: producer_role(&edge.relationship).to_string(),
                        service: edge.producer_service.clone(),
                        file: String::new(),
                        line: None,
                    });
                }
                for site in sites {
                    found.push(Counterpart {
                        role: producer_role(&edge.relationship).to_string(),
                        service: site.service.clone(),
                        file: site.file.clone(),
                        line: site.line,
                    });
                }
            }
        }
    }
    found.sort_by(|a, b| (&a.service, &a.file, a.line).cmp(&(&b.service, &b.file, b.line)));
    found.dedup();
    found
}

/// Whether an edge is recorded at this consumer row's own site. An edge with
/// no location recorded belongs to every row on its key — that is what the
/// absence means, and dropping it would lose a real counterpart.
fn edge_is_at(edge: &JoinedMatch, operation: &JoinedOperation) -> bool {
    let Some(file) = edge.consumer_file.as_deref() else {
        return true;
    };
    if file != operation.file {
        return false;
    }
    match (edge.consumer_line, operation.line) {
        (Some(edge_line), Some(row_line)) => edge_line == row_line,
        _ => true,
    }
}

/// In a shared external contract neither side serves the other, so neither
/// label is true and the row says `peer` instead (#379).
fn consumer_role(relationship: &str) -> &'static str {
    if relationship == "shared_external_contract" {
        "peer"
    } else {
        "consumer"
    }
}

fn producer_role(relationship: &str) -> &'static str {
    if relationship == "shared_external_contract" {
        "peer"
    } else {
        "producer"
    }
}

/// What the type check concluded about this row, at index time.
///
/// A type mismatch is stated with the compiler's own reason; a method mismatch
/// comes from the finding that named this call site. A pair the check never
/// evaluated gets no verdict at all rather than a reassuring one — `check`
/// renders that absence as "not checked".
fn verdict_for(
    operation: &JoinedOperation,
    matches: &[JoinedMatch],
    findings: &[JoinedFinding],
) -> Option<StoredVerdict> {
    // A finding first, where one names this row. The report's own detail has
    // been through the alias -> display-name pass, so it says `Widget` where
    // the raw pair outcome says `Endpoint_44785e_Response_At0c682a`. Same
    // verdict, readable by whoever reads it.
    if let Some(finding) = findings
        .iter()
        .find(|finding| finding_names(finding, operation))
    {
        return Some(StoredVerdict {
            result: finding.kind.clone(),
            detail: finding.detail.clone(),
        });
    }

    let mut verdict: Option<StoredVerdict> = None;
    for edge in matches {
        let mine = match operation.role {
            Role::Producer => {
                edge.producer_service == operation.service && edge.producer_key == operation.key
            }
            Role::Consumer => {
                edge.consumer_service == operation.service
                    && edge.consumer_key == operation.key
                    && edge_is_at(edge, operation)
            }
        };
        if !mine {
            continue;
        }
        match edge.type_verdict {
            Some(crate::operation::TypeVerdict::Incompatible) => {
                return Some(StoredVerdict {
                    result: "type_mismatch".to_string(),
                    detail: edge.mismatch_reason.clone().unwrap_or_else(|| {
                        "the compiler found the two types incompatible".to_string()
                    }),
                });
            }
            Some(crate::operation::TypeVerdict::Compatible) => {
                verdict = Some(StoredVerdict {
                    result: "compatible".to_string(),
                    detail: "the compiler compared both sides and found them compatible"
                        .to_string(),
                });
            }
            // Unverifiable and "never evaluated" are the same thing to a
            // reader: nothing was compared, so nothing is claimed.
            _ => {}
        }
    }
    verdict
}

/// Whether a finding is about THIS row: the consumer site it names, or the
/// operation a producer serves.
fn finding_names(finding: &JoinedFinding, operation: &JoinedOperation) -> bool {
    match operation.role {
        Role::Consumer => {
            let site = match operation.line {
                Some(line) => format!("{}:{line}", operation.file),
                None => operation.file.clone(),
            };
            // A finding names the consumer's service only when the report had
            // one to name; absent, the call site is the whole attribution, and
            // requiring a service that was never stated drops every verdict.
            let service_agrees = finding
                .service
                .as_deref()
                .is_none_or(|named| named == operation.service);
            service_agrees
                && finding
                    .call_sites
                    .iter()
                    .any(|call_site| call_site == &site || call_site.starts_with(&site))
        }
        // A finding names the PRODUCER's method and path, and the consumer's
        // service. So a producer row matches on the operation it serves, which
        // is the identity the finding was raised about.
        Role::Producer => {
            finding.method.eq_ignore_ascii_case(&operation.method) && finding.path == operation.path
        }
    }
}

/// One line naming what the row was read off. Written for a model reading a
/// terminal: the pass that stated the row, and the handler it names.
fn evidence_for(operation: &JoinedOperation) -> Option<String> {
    let source = operation.resolution_source.map(|source| {
        serde_json::to_value(source)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{source:?}"))
    });
    match (source, operation.handler.as_deref()) {
        (Some(source), Some(handler)) if !handler.is_empty() => {
            Some(format!("{source}, handler {handler}"))
        }
        (Some(source), _) => Some(source),
        (None, Some(handler)) if !handler.is_empty() => Some(format!("handler {handler}")),
        (None, _) => None,
    }
}

/// RFC 3339, to the second.
fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
