//! A service's GraphQL walk leaves its nested services' directories out, as
//! its source walk does (carrick#553): a schema or document file inside a
//! nested service is that service's alone.
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/graphql-nested-walk/`: a root service (`directory: "."`,
//! its own file is `scripts/version.ts`) over three nested services, and it
//! is scanned first. `catalog-api` serves a schema-first GraphQL API from
//! `apps/api/src/schema.graphql`, a walked file (no `graphqlSchemas` entry),
//! behind its `POST /graphql` route, so whether the file is served comes from
//! the votes of the services that walk it; `catalog-web` executes a document
//! against it.
//!
//! Answer key, read by hand from the fixture:
//!
//! | service | GraphQL producer rows | GraphQL consumer rows |
//! |---|---|---|
//! | `workspace-root` | none | none |
//! | `catalog-api` | `query|items`, `mutation|renameItem`, `query|adminStats` | none |
//! | `documents` | none | none |
//! | `catalog-web` | none | `query|items` at `apps/web/src/list.ts:5` |
//!
//! Before the fix the root walked the whole tree: it read the server's
//! schema, served nothing, so voted it "not served" and printed a notice
//! naming the file, and its blob held the documents package's rows.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// `(service, protocol row kind, kind|field, file:line)` for every GraphQL row.
type Row = (String, &'static str, String, String);

struct Scan {
    rows: BTreeSet<Row>,
    output: String,
}

fn scan() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(|| {
        let repo =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphql-nested-walk");
        let storage = tempfile::tempdir().expect("temp storage dir");
        let cache = tempfile::tempdir().expect("temp cache dir");
        let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
        cmd.arg(&repo)
            .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
            .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
            .env("CARRICK_CACHE_DIR", cache.path())
            .env("CARRICK_MOCK_ALL", "1")
            .env(
                "CARRICK_MOCK_FIXTURE_DIR",
                format!("{}/", repo.join("__llm__").display()),
            )
            .env("CARRICK_SKIP_INTENTS", "1")
            .env("CARRICK_ALLOW_MISSING_TYPES", "1");
        for var in [
            "GITHUB_REPOSITORY",
            "GITHUB_REF",
            "GITHUB_EVENT_NAME",
            "GITHUB_SHA",
            "GITHUB_RUN_ID",
            "GITHUB_ACTIONS",
            "GITHUB_WORKSPACE",
            "CI",
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        ] {
            cmd.env_remove(var);
        }
        let output = cmd.output().expect("failed to spawn carrick");
        let printed = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "scan exited non-zero:\n{printed}");

        let mut rows = BTreeSet::new();
        for entry in std::fs::read_dir(storage.path())
            .expect("storage dir")
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        {
            let blob: serde_json::Value =
                serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
                    .expect("parse blob");
            let service = blob["service_name"].as_str().unwrap_or_default();
            for (section, side) in [("calls", "consumer"), ("endpoints", "producer")] {
                for row in blob[section].as_array().expect("blob section") {
                    if row["key"]["protocol"] == "graphql" {
                        rows.insert((
                            service.to_string(),
                            side,
                            format!(
                                "{}|{}",
                                row["key"]["kind"].as_str().unwrap_or_default(),
                                row["key"]["field"].as_str().unwrap_or_default()
                            ),
                            row["file_path"].as_str().unwrap_or_default().to_string(),
                        ));
                    }
                }
            }
        }
        Scan {
            rows,
            output: printed,
        }
    })
}

#[test]
fn a_root_service_reads_no_graphql_file_inside_a_nested_service() {
    let Scan { rows, output } = scan();
    // A boundary line sits under its service's header (`<service> at <hash>:`),
    // so the nested schema's own unread sites are `catalog-api`'s, not the root's.
    let mut section = "";
    let mut root_notices: Vec<&str> = Vec::new();
    for line in output.lines() {
        if let Some((service, _)) = line.split_once(" at ").filter(|_| !line.starts_with(' ')) {
            section = service;
        }
        if line.contains("apps/api/src/schema.graphql")
            && !line.contains("'catalog-api'")
            && section != "catalog-api"
        {
            root_notices.push(line);
        }
    }
    assert!(root_notices.is_empty(), "{root_notices:#?}");

    let root_rows: Vec<&Row> = rows
        .iter()
        .filter(|(service, ..)| service == "workspace-root")
        .collect();
    assert!(root_rows.is_empty(), "{root_rows:#?}");

    assert!(
        rows.contains(&(
            "catalog-web".to_string(),
            "consumer",
            "query|items".to_string(),
            "apps/web/src/list.ts:5".to_string()
        )),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|(service, side, key, _)| service == "catalog-api"
                && *side == "producer"
                && key == "query|items"),
        "{rows:#?}"
    );
}
