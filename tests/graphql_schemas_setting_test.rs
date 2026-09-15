//! Declared GraphQL schemas for a code-first server (carrick#1099).
//!
//! Drives the real scanner binary offline over
//! `tests/fixtures/graphql-schemas-setting/`: a two-service monorepo whose
//! `widgets-api` serves a GraphQL schema built by schema-builder calls in
//! side-effect modules with no exports (nothing in its source is SDL), behind
//! one HTTP route, and also sends a document to a third-party GraphQL API. The
//! printed schema sits under `apps/web/dist/`, a build folder of ANOTHER
//! service, which no SDL walk reads. `carrick.json` names it in
//! `widgets-api`'s `graphqlSchemas` with a glob.
//!
//! The cassettes answer only the Hono routes in `index.ts`; every other file
//! gets an empty answer, so every GraphQL row below is deterministic.
//!
//! Before this setting existed, the ticket's free reproduction measured zero
//! GraphQL producers for this tree shape, and the only GraphQL note in the
//! report was suppressed by any GraphQL row, so the service's consumer row
//! silenced it.
//!
//! Answer key: the five root fields of `apps/web/dist/graphql/schema.graphql`,
//! read by hand.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphql-schemas-setting")
}

/// A copy of the fixture with `widgets-api`'s `graphqlSchemas` replaced, so the
/// fixture on disk stays the answer key and each variant differs from it in
/// exactly that one field. `None` removes the field.
fn variant(graphql_schemas: Option<&[&str]>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp fixture dir");
    copy_tree(&fixture_dir(), dir.path());
    let config_path = dir.path().join("carrick.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config_path).expect("read carrick.json"))
            .expect("parse carrick.json");
    let api = config["services"][0]
        .as_object_mut()
        .expect("first service is an object");
    assert_eq!(api["serviceName"], "widgets-api");
    match graphql_schemas {
        Some(entries) => {
            api.insert("graphqlSchemas".into(), serde_json::json!(entries));
        }
        None => {
            api.remove("graphqlSchemas");
        }
    }
    std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap())
        .expect("write carrick.json");
    dir
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create dir");
    for entry in std::fs::read_dir(from).expect("read dir").flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

struct Scan {
    /// `service -> blob`.
    blobs: Vec<(String, serde_json::Value)>,
    stdout: String,
}

impl Scan {
    fn blob(&self, service: &str) -> &serde_json::Value {
        &self
            .blobs
            .iter()
            .find(|(name, _)| name == service)
            .unwrap_or_else(|| panic!("no blob for {service}"))
            .1
    }

    /// `(kind|field, file)` for every GraphQL row in `section` of a service.
    fn graphql_rows(&self, service: &str, section: &str) -> BTreeSet<(String, String)> {
        self.blob(service)[section]
            .as_array()
            .unwrap_or_else(|| panic!("{service} blob has no {section}"))
            .iter()
            .filter(|row| row["key"]["protocol"] == "graphql")
            .map(|row| {
                let file = row["file_path"].as_str().unwrap_or_default();
                let file = file.split(':').next().unwrap_or_default().to_string();
                (
                    format!(
                        "{}|{}",
                        row["key"]["kind"].as_str().unwrap_or_default(),
                        row["key"]["field"].as_str().unwrap_or_default()
                    ),
                    file,
                )
            })
            .collect()
    }
}

fn scan(repo: &Path) -> Scan {
    let storage = tempfile::tempdir().expect("temp storage dir");
    let cache = tempfile::tempdir().expect("temp cache dir");
    let mut cmd = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_carrick")));
    cmd.arg(repo)
        .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", fixture_dir().join("__llm__").display()),
        )
        .env("CARRICK_SKIP_INTENTS", "1")
        // Rows and report lines are under test, not the type layer.
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
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut blobs = Vec::new();
    for entry in std::fs::read_dir(storage.path())
        .expect("storage dir")
        .flatten()
    {
        let blob: serde_json::Value =
            serde_json::from_slice(&std::fs::read(entry.path()).expect("read blob"))
                .expect("parse blob");
        let service = blob["service_name"]
            .as_str()
            .expect("blob names its service")
            .to_string();
        blobs.push((service, blob));
    }
    Scan { blobs, stdout }
}

fn rows(items: &[(&str, &str)]) -> BTreeSet<(String, String)> {
    items
        .iter()
        .map(|(key, file)| (key.to_string(), file.to_string()))
        .collect()
}

const PRINTED: &str = "apps/web/dist/graphql/schema.graphql";

#[test]
fn declared_schema_attributes_its_root_fields_to_the_serving_service() {
    let scan = scan(&fixture_dir());

    assert_eq!(
        scan.graphql_rows("widgets-api", "endpoints"),
        rows(&[
            ("mutation|createWidget", PRINTED),
            ("mutation|disposeWidget", PRINTED),
            ("query|health", PRINTED),
            ("query|widget", PRINTED),
            ("query|widgets", PRINTED),
        ]),
        "every root field of the declared schema is a widgets-api producer"
    );
    // The printed schema sits in widgets-web's directory, and that service
    // declares nothing: it serves none of it.
    assert!(
        scan.graphql_rows("widgets-web", "endpoints").is_empty(),
        "widgets-web must not be credited with the schema under its dist/"
    );
    // Consumer documents are attributed exactly as before.
    assert_eq!(
        scan.graphql_rows("widgets-api", "calls"),
        rows(&[("query|openInvoices", "apps/api/src/billing/invoices.ts")])
    );
    assert_eq!(
        scan.graphql_rows("widgets-web", "calls"),
        rows(&[("query|widgets", "apps/web/src/catalogue.ts")])
    );
    assert!(
        !scan.stdout.contains("graphqlSchemas"),
        "a declaration that worked prints no hint and no warning:\n{}",
        scan.stdout
    );
}

#[test]
fn without_the_setting_the_report_hints_at_it_despite_a_consumer_row() {
    let repo = variant(None);
    let scan = scan(repo.path());

    assert!(scan.graphql_rows("widgets-api", "endpoints").is_empty());
    // The consumer row that used to suppress the only GraphQL note.
    assert_eq!(
        scan.graphql_rows("widgets-api", "calls"),
        rows(&[("query|openInvoices", "apps/api/src/billing/invoices.ts")])
    );
    let hints: Vec<&str> = scan
        .stdout
        .lines()
        .filter(|line| line.contains("uses GraphQL"))
        .collect();
    assert_eq!(
        hints.len(),
        1,
        "exactly one code-first hint, for the server only (widgets-web serves no route):\n{}",
        scan.stdout
    );
    assert!(
        hints[0].contains("Service 'widgets-api' uses GraphQL (`graphql`, `graphql-request`)"),
        "{}",
        hints[0]
    );
    // Undeclared, the printed schema is a schema no service serves
    // (carrick#1134), so widgets-web's document against it is not a call, and
    // the report says which file to declare.
    assert!(scan.graphql_rows("widgets-web", "calls").is_empty());
    let external: Vec<&str> = scan
        .stdout
        .lines()
        .filter(|line| line.contains("which no service in this repository serves"))
        .collect();
    assert_eq!(external.len(), 1, "{}", scan.stdout);
    assert!(
        external[0].contains(
            "Service 'widgets-web': 1 GraphQL document operation(s) are written against \
             'apps/web/dist/graphql/schema.graphql'"
        ),
        "{}",
        external[0]
    );
}

#[test]
fn a_declared_entry_that_matches_nothing_is_reported() {
    let repo = variant(Some(&[
        "apps/api/schema.graphql",
        "apps/web/dist/**/*.graphql",
    ]));
    let scan = scan(repo.path());

    // The entry that matched still declares its fields.
    assert_eq!(scan.graphql_rows("widgets-api", "endpoints").len(), 5);
    let warnings: Vec<&str> = scan
        .stdout
        .lines()
        .filter(|line| line.contains("matches no file"))
        .collect();
    assert_eq!(warnings.len(), 1, "{}", scan.stdout);
    assert!(
        warnings[0].contains(
            "Service 'widgets-api': `graphqlSchemas` entry 'apps/api/schema.graphql' matches no \
             file in this repository"
        ),
        "{}",
        warnings[0]
    );
}
