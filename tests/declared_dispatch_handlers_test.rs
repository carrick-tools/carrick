//! carrick#2076 through the whole scanner, offline ($0, no model), with the
//! type sidecar live: a `carrick.json` operation that names its handler as
//! `file:symbol` is placed at that handler's declaration line, and the type
//! layer reads the case's own contract there.
//!
//! Fixture: `tests/fixtures/declared-dispatch-handlers/` (answer key in its
//! `README.md` and `expected.json`). Two routeless services dispatch on a body
//! field and declare every case in `carrick.json`; a third calls them. Until
//! this change every declared row sat at `file:0`, so all of a block's cases
//! shared one site, one alias pair and no type request, and every pair on
//! them read `unresolved`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const WEB: &str = "notes-web/src/api.ts";

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/declared-dispatch-handlers")
}

fn expected() -> serde_json::Value {
    let path = fixture_dir().join("expected.json");
    serde_json::from_str(&std::fs::read_to_string(&path).expect("read the answer key"))
        .expect("the answer key is JSON")
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "carrick-test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "carrick-test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .status()
        .expect("git runs");
    assert!(status.success(), "git {args:?} failed");
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// Every service's blob, keyed by service name, from one offline scan of a
/// committed copy of the fixture, run on `.` from inside it as the Action
/// runs it.
fn blobs() -> &'static BTreeMap<String, serde_json::Value> {
    static SCAN: OnceLock<BTreeMap<String, serde_json::Value>> = OnceLock::new();
    SCAN.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("temp dir");
        let repo = tmp.path().join("workspace");
        copy_dir(&fixture_dir(), &repo);
        run_git(&repo, &["init", "-q"]);
        run_git(&repo, &["add", "-A"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        let storage = tmp.path().join("storage");
        std::fs::create_dir_all(&storage).unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
        cmd.arg(".")
            .current_dir(&repo)
            .env("CARRICK_LOCAL_STORAGE_DIR", &storage)
            .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
            .env("CARRICK_CACHE_DIR", tmp.path().join("cache"))
            .env("HOME", &home)
            .env("CARRICK_MOCK_ALL", "1")
            .env(
                "CARRICK_MOCK_FIXTURE_DIR",
                format!("{}/", repo.join("__llm__").display()),
            )
            .env("CARRICK_SKIP_INTENTS", "1");
        for var in [
            "GITHUB_REPOSITORY",
            "GITHUB_REF",
            "GITHUB_EVENT_NAME",
            "GITHUB_SHA",
            "GITHUB_RUN_ID",
            "GITHUB_ACTIONS",
            "GITHUB_WORKSPACE",
            "CI",
            "CARRICK_OUTPUT_JSON",
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
            // The type layer is under test: a sidecar that cannot start must
            // fail the scan, not pass as "types unavailable".
            "CARRICK_ALLOW_MISSING_TYPES",
        ] {
            cmd.env_remove(var);
        }
        let output = cmd.output().expect("failed to spawn carrick");
        assert!(
            output.status.success(),
            "fixture scan exited non-zero:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let blobs: BTreeMap<String, serde_json::Value> = std::fs::read_dir(&storage)
            .expect("storage dir")
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            // Blobs only: a type-reach record sits beside each (carrick#3101).
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .filter_map(|path| {
                let blob: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
                Some((blob["service_name"].as_str()?.to_string(), blob))
            })
            .collect();
        assert_eq!(
            blobs.keys().map(String::as_str).collect::<Vec<_>>(),
            ["ledger-fn", "notes-fn", "notes-web"],
            "every service uploads one blob"
        );
        blobs
    })
}

fn text(value: &serde_json::Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

/// `value -> (file_location, handler_span as (start, end))` for every
/// declared row of `service`'s mount graph.
fn declared_rows(service: &str) -> BTreeMap<String, (String, Option<(u64, u64)>)> {
    blobs()[service]["mount_graph"]["endpoints"]
        .as_array()
        .expect("mount_graph.endpoints")
        .iter()
        .filter(|row| row["resolution_source"] == "declared_operation")
        .map(|row| {
            let span = row.get("handler_span").filter(|span| !span.is_null());
            (
                text(&row["dispatch"]["value"]),
                (
                    text(&row["file_location"]),
                    span.map(|span| {
                        (
                            span["start_line"].as_u64().unwrap_or_default(),
                            span["end_line"].as_u64().unwrap_or_default(),
                        )
                    }),
                ),
            )
        })
        .collect()
}

/// The producer manifest entries of `service` of one kind, by line.
fn producer_entries(service: &str, kind: &str) -> BTreeMap<u64, Vec<serde_json::Value>> {
    let mut by_line: BTreeMap<u64, Vec<serde_json::Value>> = BTreeMap::new();
    for entry in blobs()[service]["type_manifest"]
        .as_array()
        .expect("type_manifest")
    {
        if entry["role"] == "producer" && entry["type_kind"] == kind {
            by_line
                .entry(entry["line_number"].as_u64().unwrap_or_default())
                .or_default()
                .push(entry.clone());
        }
    }
    by_line
}

/// The one producer entry of `service` of `kind` at `line`.
fn producer_entry(service: &str, kind: &str, line: u64) -> serde_json::Value {
    let entries = producer_entries(service, kind);
    let at = entries
        .get(&line)
        .unwrap_or_else(|| panic!("no {kind} entry at {service}:{line}: {entries:#?}"));
    assert_eq!(at.len(), 1, "one {kind} entry at {service}:{line}: {at:#?}");
    at[0].clone()
}

/// The type a producer entry publishes, or `None` when it is `unknown`.
fn published(entry: &serde_json::Value) -> Option<String> {
    if entry["type_state"] == "unknown" {
        return None;
    }
    entry["expanded_definition"]
        .as_str()
        .or_else(|| entry["resolved_definition"].as_str())
        .map(str::to_string)
}

/// The `any_provenance` reasons on an entry.
fn reasons(entry: &serde_json::Value) -> BTreeSet<String> {
    entry["any_provenance"]
        .as_array()
        .map(|rows| rows.iter().map(|row| text(&row["reason"])).collect())
        .unwrap_or_default()
}

/// `(half, verdict)` stored for the consumer call at `notes-web`'s `line`,
/// over every producer blob, with the producer operation it was judged
/// against.
fn verdicts_at(line: u64) -> BTreeSet<(String, String)> {
    let site = format!("{WEB}:{line}");
    let mut found = BTreeSet::new();
    for blob in blobs().values() {
        for pair in blob["compat_verdicts"].as_array().into_iter().flatten() {
            for at in pair["sites"].as_array().into_iter().flatten() {
                if at["consumer_location"] != site.as_str() {
                    continue;
                }
                for half in ["request", "response"] {
                    if let Some(verdict) = at[half]["verdict"].as_str() {
                        found.insert((half.to_string(), verdict.to_string()));
                    }
                }
            }
        }
    }
    found
}

/// Test 1. Every declared operation whose handler is defined once in its file
/// sits at that handler's declaration line, with the function's span. The one
/// whose handler symbol is not in its file keeps line 0 and gets no span.
#[test]
fn a_declared_operation_sits_at_its_handlers_declaration_line() {
    let key = expected();
    for service in ["ledger-fn", "notes-fn"] {
        let wanted: BTreeMap<String, (String, Option<(u64, u64)>)> = key["placed"][service]
            .as_object()
            .expect("placed rows per service")
            .iter()
            .map(|(value, row)| {
                let span = row["span"]
                    .as_array()
                    .map(|span| (span[0].as_u64().unwrap(), span[1].as_u64().unwrap()));
                (value.clone(), (text(&row["at"]), span))
            })
            .collect();
        assert_eq!(declared_rows(service), wanted, "{service}");
    }
}

/// Test 2. One producer request and response alias per handler site: `list`
/// and `search` name one handler and share its alias, and no two handlers
/// share one.
#[test]
fn each_handler_site_has_its_own_alias() {
    for (service, lines) in [
        ("ledger-fn", vec![36u64, 41]),
        ("notes-fn", vec![1, 48, 55, 61, 67]),
    ] {
        for kind in ["request", "response"] {
            let entries = producer_entries(service, kind);
            assert_eq!(
                entries.keys().copied().collect::<Vec<_>>(),
                lines,
                "{service} {kind} entries sit at the handler lines (the unplaced row at 1)"
            );
            let aliases: BTreeSet<String> = entries
                .values()
                .flatten()
                .map(|entry| text(&entry["type_alias"]))
                .collect();
            assert_eq!(
                aliases.len(),
                lines.len(),
                "{service} {kind}: one alias per site: {entries:#?}"
            );
        }
    }
}

/// Test 3. What each case publishes, read at its own handler.
#[test]
fn each_case_publishes_its_own_handlers_contract() {
    let has = |entry: &serde_json::Value, members: &[&str], what: &str| {
        let published = published(entry).unwrap_or_else(|| panic!("{what} is typed: {entry:#}"));
        for member in members {
            assert!(
                published.contains(member),
                "{what} states `{member}`: {published}"
            );
        }
        published
    };

    // The ledger's handlers return plain values the platform serialises, and
    // each takes its own command type.
    has(
        &producer_entry("ledger-fn", "response", 36),
        &["account: string", "balance: number"],
        "credit's response",
    );
    has(
        &producer_entry("ledger-fn", "request", 36),
        &["op: \"credit\"", "account: string", "amount: number"],
        "credit's request",
    );
    has(
        &producer_entry("ledger-fn", "response", 41),
        &["overdrawn: boolean"],
        "debit's response",
    );
    has(
        &producer_entry("ledger-fn", "request", 41),
        &["op: \"debit\"", "reason: string"],
        "debit's request",
    );

    // `add` answers its 2xx body; the 400 branch is not the contract.
    let add = has(
        &producer_entry("notes-fn", "response", 48),
        &["note:", "id: string", "text: string"],
        "add's response",
    );
    assert!(!add.contains("error"), "the 400 branch drops out: {add}");
    has(
        &producer_entry("notes-fn", "request", 48),
        &["text: string", "tags: string[]"],
        "add's request",
    );

    // `list` (and `search`) answer the body serialised inline.
    has(
        &producer_entry("notes-fn", "response", 55),
        &["notes:"],
        "list's response",
    );
    has(
        &producer_entry("notes-fn", "request", 55),
        &["limit: number"],
        "list's request",
    );

    // `purge` and `archive` take the dispatcher's own input type, so neither
    // states a request of its own.
    for line in [61, 67] {
        assert_eq!(
            published(&producer_entry("notes-fn", "request", line)),
            None,
            "a type both handlers take is the dispatcher's, not the case's"
        );
    }
    has(
        &producer_entry("notes-fn", "response", 61),
        &["purged: number"],
        "purge's response",
    );
    // `archive` returns a pre-built envelope by name, which the inferrer does
    // not follow: decided unknown, never the envelope.
    let archive = producer_entry("notes-fn", "response", 67);
    assert_eq!(published(&archive), None, "{archive:#}");
    assert!(
        reasons(&archive).contains("serialised_body_unread"),
        "{archive:#}"
    );

    // No producer response anywhere is the envelope itself.
    for service in ["ledger-fn", "notes-fn"] {
        for entry in producer_entries(service, "response").values().flatten() {
            if let Some(published) = published(entry) {
                assert!(
                    !published.contains("statusCode") && !published.contains("body: string"),
                    "an envelope is never the contract: {published}"
                );
            }
        }
    }
}

/// Test 4. Each consumer call is judged against its own case.
#[test]
fn each_call_is_judged_against_its_own_case() {
    let compatible = |half: &str| (half.to_string(), "compatible".to_string());
    let incompatible = |half: &str| (half.to_string(), "incompatible".to_string());

    // `credit`: both halves typed and agreeing. The request half is the proof
    // that the handler's parameter anchored in the capture surface.
    let credit = verdicts_at(50);
    assert!(credit.contains(&compatible("request")), "{credit:?}");
    assert!(credit.contains(&compatible("response")), "{credit:?}");

    // `add`: the consumer's reply type is wrong on purpose; the break is
    // found at that call only.
    let add = verdicts_at(57);
    assert!(add.contains(&incompatible("response")), "{add:?}");
    for line in [50, 64, 71] {
        assert!(
            !verdicts_at(line).contains(&incompatible("response")),
            "{WEB}:{line} reads no break of another case's: {:?}",
            verdicts_at(line)
        );
    }

    // `list` and `purge`: responses agree.
    assert!(verdicts_at(64).contains(&compatible("response")));
    assert!(verdicts_at(71).contains(&compatible("response")));
}
