//! A GraphQL document against a schema file several services walk is decided
//! once every one of them has voted on the file (carrick#3663), and a pass
//! that removes rows fails closed (carrick#3888): what the index keeps must
//! not depend on which services this run analysed.
//!
//! Each case writes its own workspace and drives the real scanner binary
//! offline (cassettes as the model, a local store, no spend):
//!
//! - `schema-api` walks `shared/` through `include` and serves an HTTP route,
//!   so it serves `shared/schema.graphql`;
//! - `schema-copy` walks `shared/` too and serves nothing; it also holds a
//!   vendor copy of another API's schema that declares `balance`;
//! - `client` has two documents: `accounts` (only `shared` declares it) and
//!   `balance` (`shared` and the vendor copy both declare it).
//!
//! Answer key, read by hand:
//!
//! | case | `client` calls |
//! |---|---|
//! | full scan | `accounts` served; `balance` unresolved (a served and an external schema both hold it, and the transport names neither), so not a call |
//! | `--changed-only` with `schema-api` carried | the same as the full scan |
//! | a run stopped by a failing service, `schema-api` absent | `accounts` and `balance` external: the lone walker serves nothing |
//! | a run stopped by `schema-api` failing | `accounts` and `balance` kept: a walker was never heard |

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const REPO: &str = "fixture/held-documents";

const SERVER: &str = "import { Hono } from 'hono';\n\nconst app = new Hono();\n\napp.get('/health', (c) => c.json({ ok: true }));\n\nexport default app;\n";

const SERVER_ANSWER: &str = r#"{
  "mounts": [],
  "endpoints": [
    {
      "candidate_id": "@line:5",
      "line_number": 5,
      "owner_node": "app",
      "method": "GET",
      "path": "/health",
      "handler_name": "anonymous",
      "pattern_matched": ".get(",
      "payload_expression_text": null,
      "payload_expression_line": null,
      "response_expression_text": "{ ok: true }",
      "response_expression_line": 5,
      "primary_type_symbol": null,
      "type_import_source": null
    }
  ],
  "data_calls": []
}"#;

/// One service of a workspace: its name, directory, and the `carrick.json`
/// fields beyond them.
struct Service {
    name: &'static str,
    dir: &'static str,
    include_shared: bool,
}

const API: Service = Service {
    name: "schema-api",
    dir: "apps/api",
    include_shared: true,
};
const COPY: Service = Service {
    name: "schema-copy",
    dir: "apps/copy",
    include_shared: true,
};
const CLIENT: Service = Service {
    name: "client",
    dir: "apps/client",
    include_shared: false,
};
const BROKEN: Service = Service {
    name: "broken",
    dir: "apps/broken",
    include_shared: false,
};

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git failed to spawn");
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

struct Workspace {
    tmp: tempfile::TempDir,
}

/// What one scan printed and how it exited.
struct Scan {
    output: String,
    success: bool,
}

impl Scan {
    fn scoped(&self, how: &str) -> BTreeSet<String> {
        let prefix = "scope: ";
        self.output
            .lines()
            .filter_map(|line| {
                let rest = &line[line.find(prefix)? + prefix.len()..];
                let (service, verdict) = rest.split_once(' ')?;
                (verdict == how || verdict.starts_with(&format!("{how} (")))
                    .then(|| service.to_string())
            })
            .collect()
    }
}

impl Workspace {
    /// The services in declared order, committed once.
    fn new(services: &[&Service]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = Self { tmp };
        let repo = workspace.repo();
        write(
            &repo,
            "package.json",
            "{ \"name\": \"held-documents\", \"private\": true }\n",
        );
        write(
            &repo,
            "shared/schema.graphql",
            "type Query {\n  accounts: [String]\n  balance: Int\n}\n",
        );
        let entries: Vec<String> = services
            .iter()
            .map(|service| {
                let include = if service.include_shared {
                    ", \"include\": [\"shared\"]"
                } else {
                    ""
                };
                format!(
                    "    {{ \"serviceName\": \"{}\", \"directory\": \"{}\"{include} }}",
                    service.name, service.dir
                )
            })
            .collect();
        write(
            &repo,
            "carrick.json",
            &format!("{{\n  \"services\": [\n{}\n  ]\n}}\n", entries.join(",\n")),
        );
        for service in services {
            let manifest = if service.name == BROKEN.name {
                "{ \"name\": \"broken\", ".to_string()
            } else {
                format!("{{ \"name\": \"{}\", \"private\": true }}\n", service.name)
            };
            write(&repo, &format!("{}/package.json", service.dir), &manifest);
            write(
                &repo,
                &format!("{}/tsconfig.json", service.dir),
                "{ \"compilerOptions\": { \"strict\": true, \"target\": \"es2022\", \"module\": \
                 \"esnext\", \"moduleResolution\": \"bundler\", \"skipLibCheck\": true }, \
                 \"include\": [\"src\"] }\n",
            );
        }
        write(&repo, "apps/api/src/server.ts", SERVER);
        write(&repo, "apps/copy/src/index.ts", "export const copy = 1;\n");
        write(
            &repo,
            "apps/copy/src/vendor/ledger.graphql",
            "type Query {\n  balance: Int\n}\n",
        );
        write(
            &repo,
            "apps/client/src/index.ts",
            "export const client = 1;\n",
        );
        write(
            &repo,
            "apps/client/src/accounts.gql",
            "query Accounts {\n  accounts\n}\n",
        );
        write(
            &repo,
            "apps/client/src/balance.gql",
            "query Balance {\n  balance\n}\n",
        );
        write(
            &repo,
            "apps/broken/src/index.ts",
            "export const broken = 1;\n",
        );
        write(
            &workspace.cassettes(),
            "analyze-file/server.json",
            SERVER_ANSWER,
        );
        git(&repo, &["init", "-q", "-b", "main"]);
        workspace.commit("A");
        workspace
    }

    fn repo(&self) -> PathBuf {
        self.tmp.path().join("repo")
    }

    fn cassettes(&self) -> PathBuf {
        self.tmp.path().join("llm")
    }

    fn commit(&self, message: &str) {
        git(&self.repo(), &["add", "-A"]);
        git(&self.repo(), &["commit", "-q", "-m", message]);
    }

    fn scan(&self, store: &Path, flags: &[&str]) -> Scan {
        std::fs::create_dir_all(store).unwrap();
        let home = self.tmp.path().join("home");
        let _ = std::fs::remove_dir_all(&home);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
        cmd.arg(self.repo())
            .args(["--repo", REPO])
            .args(flags)
            .env("HOME", &home)
            .env("CARRICK_MOCK_ALL", "1")
            .env(
                "CARRICK_MOCK_FIXTURE_DIR",
                format!("{}/", self.cassettes().display()),
            )
            .env("CARRICK_SKIP_INTENTS", "1")
            .env("CARRICK_ALLOW_UNPREPARED", "1")
            .env("CARRICK_ALLOW_MISSING_TYPES", "1")
            .env("CARRICK_NO_UPDATE_CHECK", "1")
            // One service at a time, in declared order, so `client`
            // attributes its documents before any walker votes.
            .env("CARRICK_SERVICE_CONCURRENCY", "1")
            .env("CARRICK_LOCAL_STORAGE_DIR", store);
        for var in [
            "GITHUB_REPOSITORY",
            "GITHUB_EVENT_NAME",
            "GITHUB_REF",
            "GITHUB_ACTIONS",
            "GITHUB_SHA",
            "GITHUB_RUN_ID",
            "GITHUB_WORKSPACE",
            "CI",
            "CARRICK_OUTPUT_JSON",
            "CARRICK_LOCAL_STORAGE_ISOLATE",
            "CARRICK_CACHE_DIR",
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        ] {
            cmd.env_remove(var);
        }
        let result = cmd.output().expect("failed to spawn carrick");
        Scan {
            output: format!(
                "{}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            ),
            success: result.status.success(),
        }
    }
}

/// Each stored blob, by service.
fn blobs(store: &Path) -> BTreeMap<String, serde_json::Value> {
    std::fs::read_dir(store)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension()? == "json").then_some(path)
        })
        .map(|path| {
            let blob: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            (blob["service_name"].as_str().unwrap().to_string(), blob)
        })
        .collect()
}

/// A service's GraphQL call rows and its GraphQL consumer manifest entries,
/// as `field file` lines.
fn graphql_calls(store: &Path, service: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let blobs = blobs(store);
    let blob = blobs
        .get(service)
        .unwrap_or_else(|| panic!("no blob for {service}: {:?}", blobs.keys()));
    let rows = |section: &str, file_field: &str| -> BTreeSet<String> {
        blob[section]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| row["protocol"] == "graphql" || row["key"]["protocol"] == "graphql")
            .filter(|row| section != "type_manifest" || row["role"] == "consumer")
            .map(|row| {
                let field = row["key"]["field"]
                    .as_str()
                    .or_else(|| row["field"].as_str())
                    .unwrap_or_default();
                let file = row[file_field].as_str().unwrap_or_default();
                format!("{field} {}", file.split(':').next().unwrap_or_default())
            })
            .collect()
    };
    (
        rows("calls", "file_path"),
        rows("type_manifest", "file_path"),
    )
}

fn lines(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| item.to_string()).collect()
}

/// carrick#3888: a walker `--changed-only` carries votes from its stored
/// generation, so the documents it decides read as a full scan reads them.
#[test]
fn a_carried_walker_votes_as_a_full_scan_does() {
    let workspace = Workspace::new(&[&API, &COPY, &CLIENT]);
    let store = workspace.tmp.path().join("store");
    let first = workspace.scan(&store, &[]);
    assert!(first.success, "the scan at A failed:\n{}", first.output);

    // Commit B touches only `schema-copy` and `client`.
    let repo = workspace.repo();
    write(&repo, "apps/copy/src/index.ts", "export const copy = 2;\n");
    write(
        &repo,
        "apps/client/src/index.ts",
        "export const client = 2;\n",
    );
    workspace.commit("B");

    let carried_store = workspace.tmp.path().join("carried-store");
    copy_dir(&store, &carried_store);
    let carried = workspace.scan(&carried_store, &["--changed-only"]);
    assert!(carried.success, "{}", carried.output);
    assert_eq!(
        carried.scoped("carried"),
        lines(&["schema-api"]),
        "{}",
        carried.output
    );
    assert_eq!(
        carried.scoped("analysed"),
        lines(&["schema-copy", "client"]),
        "{}",
        carried.output
    );

    let full_store = workspace.tmp.path().join("full-store");
    let full = workspace.scan(&full_store, &[]);
    assert!(full.success, "{}", full.output);
    let expected = graphql_calls(&full_store, "client");
    assert_eq!(
        expected.0,
        lines(&["accounts apps/client/src/accounts.gql"]),
        "the full scan's answer key"
    );
    assert_eq!(
        graphql_calls(&carried_store, "client"),
        expected,
        "the carried run keeps what a full scan keeps:\n{}",
        carried.output
    );
}

/// carrick#3888: when a service stops the run, the services that finished
/// land with their held documents decided as at a run's end.
#[test]
fn a_stopped_run_lands_its_held_documents_decided() {
    let workspace = Workspace::new(&[&CLIENT, &COPY, &BROKEN]);
    let store = workspace.tmp.path().join("store");
    let scan = workspace.scan(&store, &[]);
    assert!(!scan.success, "`broken` stops the run:\n{}", scan.output);
    // The lone walker serves nothing: both documents are another API's.
    assert_eq!(
        graphql_calls(&store, "client"),
        (BTreeSet::new(), BTreeSet::new()),
        "{}",
        scan.output
    );
}

/// carrick#3888: a document against a file a walker was never heard on (it
/// failed) stays served: a pass that removes rows fails closed.
#[test]
fn a_walker_never_heard_keeps_the_documents() {
    let workspace = Workspace::new(&[
        &CLIENT,
        &COPY,
        &Service {
            name: "broken",
            dir: "apps/api",
            include_shared: true,
        },
    ]);
    let store = workspace.tmp.path().join("store");
    let scan = workspace.scan(&store, &[]);
    assert!(!scan.success, "the walker stops the run:\n{}", scan.output);
    let kept = lines(&[
        "accounts apps/client/src/accounts.gql",
        "balance apps/client/src/balance.gql",
    ]);
    assert_eq!(
        graphql_calls(&store, "client"),
        (kept.clone(), kept),
        "{}",
        scan.output
    );
}
