//! The local read-only path end to end (carrick#708): `index`, `touch`,
//! `check`, `refresh` over real fixtures, with no model and no cloud.
//!
//! Two workspaces, because they prove different halves:
//!
//! * `tests/fixtures/local-mode-workspace` — a producer and a consumer built
//!   so every row is deterministic, which is what lets the test drive a full
//!   edit cycle: a matched pair, a compiler verdict on it, a breaking edit, a
//!   refresh, and the verdict flipping. Its README is its answer key.
//! * `tests/fixtures/xrepo-corpus-1` — a real multi-repo corpus, to prove the
//!   same commands answer over a tree nobody wrote for them.
//!
//! **Why not `xrepo-corpus-2`,** which carrick#708 names: measured on
//! 2026-09-06, corpus-2 yields ZERO rows under a no-model index. Its HTTP
//! producers are bare `app.get("/lit", h)` sites whose route-ness is decided
//! by matching the receiver's declaring package against the framework list the
//! detection MODEL produces; its consumers are `fetch(\`${BASE}/path\`)`,
//! which resolves a URL but no verb and so states no row of its own; and its
//! pub/sub files raise no candidate without `messaging_clients` from the same
//! detection. A corpus with no rows cannot prove a counterpart. Corpus-1's
//! GraphQL and socket edges are deterministic on both sides, so it proves
//! exactly what the ticket asked for.
//!
//! Every workspace is copied to a temp dir and `git init`-ed there: the tests
//! edit producers and delete files, and a fixture is never mutated in place.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn carrick() -> &'static str {
    env!("CARGO_BIN_EXE_carrick")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Copy a fixture's repos into a fresh workspace, commit each one, and write
/// the workspace file. Returns the workspace root, which owns the temp dir for
/// as long as the test holds it.
fn workspace(fixture: &str, repos: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let source = repo_root().join("tests/fixtures").join(fixture);
    for repo in repos {
        copy_tree(&source.join(repo), &dir.path().join(repo));
        git(&dir.path().join(repo), &["init", "-q", "."]);
        git(&dir.path().join(repo), &["add", "-A"]);
        git(
            &dir.path().join(repo),
            &[
                "-c",
                "user.email=fixture@carrick.test",
                "-c",
                "user.name=fixture",
                "commit",
                "-qm",
                "fixture",
            ],
        );
    }
    let listed: Vec<String> = repos.iter().map(|repo| format!("\"./{repo}\"")).collect();
    std::fs::write(
        dir.path().join("carrick-workspace.json"),
        format!("{{ \"repos\": [{}] }}\n", listed.join(", ")),
    )
    .expect("write the workspace file");
    dir
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the destination");
    for entry in std::fs::read_dir(from).unwrap_or_else(|e| panic!("read {}: {e}", from.display()))
    {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy a fixture file");
        }
    }
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", repo.display()));
    assert!(
        status.status.success(),
        "git {args:?} failed in {}:\n{}",
        repo.display(),
        String::from_utf8_lossy(&status.stderr)
    );
}

/// Run a carrick command in the workspace and return its stdout. Read-only
/// commands must always exit 0, which is asserted here rather than in every
/// test: a hook that fails an edit because an index is stale is the failure
/// mode this path exists to avoid.
fn run(workspace: &Path, args: &[&str]) -> String {
    let output = Command::new(carrick())
        .args(args)
        .current_dir(workspace)
        .output()
        .unwrap_or_else(|e| panic!("carrick {args:?}: {e}"));
    assert!(
        output.status.success(),
        "carrick {args:?} exited {:?}:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout was not UTF-8")
}

fn index(workspace: &Path) -> String {
    run(workspace, &["index", "--workspace", "."])
}

fn touch(workspace: &Path, file: &str) -> String {
    run(workspace, &["touch", file, "--workspace", "."])
}

fn check(workspace: &Path, file: &str) -> String {
    run(workspace, &["check", file, "--workspace", "."])
}

fn check_json(workspace: &Path, file: &str) -> serde_json::Value {
    let text = run(workspace, &["check", file, "--workspace", ".", "--json"]);
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("check --json was not JSON: {e}\n{text}"))
}

fn edit(file: &Path, from: &str, to: &str) {
    let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    assert!(
        text.contains(from),
        "the fixture no longer contains {from:?}, so this edit proves nothing:\n{text}"
    );
    std::fs::write(file, text.replace(from, to)).expect("write the edit");
}

/// The whole cycle on the purpose-built workspace: a matched pair with a
/// compiler verdict, a breaking edit, a refresh, and the verdict that follows.
#[test]
fn local_mode_answers_and_follows_an_edit() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    let route = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";
    let route_file = root.join(route);
    let caller = "inventory-svc/src/inventory.ts";

    let map = index(root);
    assert!(
        map.contains("catalog-web") && map.contains("inventory-svc"),
        "the map names every service:\n{map}"
    );

    // Touching the producer names both consumer sites, at their own lines.
    // Two calls to one client member are two answers, not one.
    let touched = touch(root, route);
    assert!(
        touched.contains("GET /api/v1/widgets/:widgetId"),
        "the route the file states:\n{touched}"
    );
    assert!(
        touched.contains("src/inventory.ts:9") && touched.contains("src/inventory.ts:17"),
        "both consumer sites, at the right lines:\n{touched}"
    );
    // `touch` states locations and nothing about agreement.
    assert!(
        !touched.contains("verdict"),
        "touch states no verdict:\n{touched}"
    );

    // The consumer sees the producer, and the type check has compared them.
    let checked = check(root, caller);
    assert!(
        checked.contains("app/routes/api.v1.widgets.$widgetId.ts:11"),
        "the producer, at its line:\n{checked}"
    );
    assert!(
        checked.contains("compatible"),
        "the compiler compared both sides:\n{checked}"
    );

    // An edit with no refresh behind it: the rows still describe the tree the
    // index was built on, and say so.
    edit(&route_file, "activeCount: number", "activeCount: string");
    edit(&route_file, "activeCount: 3", "activeCount: \"3\"");
    let stale = touch(root, route);
    assert!(
        stale.contains("unresolved since your edit"),
        "an edited file is stale until it is refreshed:\n{stale}"
    );
    let stale = check_json(root, route);
    assert_eq!(stale["stale"], serde_json::json!(true));
    assert_eq!(
        stale["items"][0]["verdict"]["state"],
        serde_json::json!("unresolved"),
        "a verdict about a tree that has moved is unresolved:\n{stale:#}"
    );

    // Refresh the one service that changed, and the break is the verdict.
    run(
        root,
        &["refresh", "--service", "catalog-web", "--workspace", "."],
    );
    let broken = check(root, caller);
    assert!(
        broken.contains("type_mismatch"),
        "the breaking edit is the verdict:\n{broken}"
    );
    assert!(
        broken.contains("activeCount"),
        "with the compiler's own reason:\n{broken}"
    );

    // An additive edit changes nothing: the field goes back, a new optional
    // one appears on the producer, and both sides agree again.
    edit(&route_file, "activeCount: string", "activeCount: number");
    edit(&route_file, "activeCount: \"3\"", "activeCount: 3");
    edit(
        &route_file,
        "  activeCount: number;",
        "  activeCount: number;\n  label?: string;",
    );
    run(
        root,
        &["refresh", "--service", "catalog-web", "--workspace", "."],
    );
    let additive = check(root, caller);
    assert!(
        additive.contains("compatible") && !additive.contains("type_mismatch"),
        "an added optional field breaks nothing:\n{additive}"
    );

    // A deleted route file, with no re-index behind it: the index still serves
    // the route, and the consumers are what a reader needs to see.
    std::fs::remove_file(&route_file).expect("delete the route module");
    let removed = check_json(root, route);
    assert_eq!(removed["deleted"], serde_json::json!(true));
    assert_eq!(
        removed["items"][0]["verdict"]["result"],
        serde_json::json!("producer_removed"),
        "a route whose file is gone is a removed producer:\n{removed:#}"
    );
    assert_eq!(
        removed["items"][0]["counterparts"].as_array().map(Vec::len),
        Some(2),
        "with its consumers listed:\n{removed:#}"
    );
}

/// The JSON is the contract in `docs/local-mode-output.md`, and the hook and
/// the LSP shim are built against it.
#[test]
fn the_json_matches_the_published_contract() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);

    let body = check_json(root, "inventory-svc/src/inventory.ts");
    assert_eq!(body["schema"], serde_json::json!("carrick.check/0"));
    assert_eq!(body["file"], serde_json::json!("src/inventory.ts"));
    assert_eq!(body["service"], serde_json::json!("inventory-svc"));
    assert!(body["index_commit"].as_str().is_some_and(|c| !c.is_empty()));
    assert!(body["scanner_version"].as_str().is_some());
    assert_eq!(body["changed_since_index"], serde_json::json!(0));
    assert_eq!(body["stale"], serde_json::json!(false));
    assert_eq!(body["deleted"], serde_json::json!(false));

    let item = &body["items"][0];
    assert_eq!(item["kind"], serde_json::json!("call"));
    assert_eq!(item["method"], serde_json::json!("GET"));
    assert_eq!(item["line"], serde_json::json!(9));
    // A local index holds no model rows at all, so every row is a fact.
    assert_eq!(item["source"], serde_json::json!("fact"));
    assert_eq!(
        item["resolution_source"],
        serde_json::json!("imported_member")
    );
    let counterpart = &item["counterparts"][0];
    assert_eq!(counterpart["role"], serde_json::json!("producer"));
    assert_eq!(counterpart["service"], serde_json::json!("catalog-web"));
    // A counterpart's file is relative to ITS OWN repo, so the payload names
    // that repo: a reader opens `repo/file` instead of guessing which
    // directory the path hangs off (carrick#709).
    let counterpart_repo = counterpart["repo"]
        .as_str()
        .expect("the counterpart's repo");
    assert!(
        Path::new(counterpart_repo)
            .join(counterpart["file"].as_str().unwrap())
            .exists(),
        "repo + file must name a file on disk: {counterpart_repo}"
    );
    let own_repo = body["repo"].as_str().expect("the queried file's repo");
    assert!(
        Path::new(own_repo)
            .join(body["file"].as_str().unwrap())
            .exists(),
        "the queried file's repo + file must name it too: {own_repo}"
    );

    // The boundary is pre-rendered, and it is the same bytes the terminal
    // prints, so a hook and a developer read one sentence about one number.
    let lines: Vec<String> = body["boundary_lines"]
        .as_array()
        .expect("boundary_lines")
        .iter()
        .map(|line| line.as_str().expect("a line").to_string())
        .collect();
    assert!(
        lines[0].starts_with("boundary (inventory-svc):"),
        "the note leads: {lines:?}"
    );
    let printed = check(root, "inventory-svc/src/inventory.ts");
    for line in &lines {
        assert!(
            printed.contains(line.as_str()),
            "every rendered line is in the terminal output verbatim: {line}\n{printed}"
        );
    }

    // `touch` emits the same shape with every verdict null.
    let touched: serde_json::Value = serde_json::from_str(&run(
        root,
        &[
            "touch",
            "inventory-svc/src/inventory.ts",
            "--workspace",
            ".",
            "--json",
        ],
    ))
    .expect("touch --json was not JSON");
    assert_eq!(touched["items"][0]["verdict"], serde_json::Value::Null);

    // A file nobody indexed still answers, with the boundary and no rows.
    let unknown = check_json(root, "inventory-svc/src/send.ts");
    assert_eq!(
        unknown["items"].as_array().map(Vec::len),
        Some(0),
        "{unknown:#}"
    );
    assert!(unknown["boundary_note"].as_str().is_some());

    // A file outside every indexed repo is an error body, and still exit 0.
    let outside = run(
        root,
        &["check", "/nowhere/x.ts", "--workspace", ".", "--json"],
    );
    let outside: serde_json::Value = serde_json::from_str(&outside).expect("an error body");
    assert_eq!(outside["error"], serde_json::json!("not_in_workspace"));
}

/// A read is a read: it must cost what an editor hook can afford. Asserted on
/// the median of three runs, because a debug binary on a loaded CI box is not
/// a stopwatch.
#[test]
fn a_read_costs_what_a_hook_can_afford() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);

    let mut timings: Vec<Duration> = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        check(root, "inventory-svc/src/inventory.ts");
        timings.push(started.elapsed());
    }
    timings.sort();
    let median = timings[1];
    assert!(
        median < Duration::from_millis(300),
        "a read must stay under 300 ms; median of three was {median:?} ({timings:?})"
    );
}

/// The same commands over a corpus written for something else: the counterpart
/// a reader wants is named, across repos, from facts alone.
#[test]
fn local_mode_answers_over_a_real_corpus() {
    let workspace = workspace(
        "xrepo-corpus-1",
        &["orders-monorepo", "payments-svc", "web-frontend"],
    );
    let root = workspace.path();
    index(root);

    // A GraphQL producer names its consumer in another repo.
    let schema = touch(root, "orders-monorepo/packages/gateway/src/schema.graphql");
    assert!(
        schema.contains("QUERY order"),
        "the schema's root fields:\n{schema}"
    );
    assert!(
        schema.contains("web-frontend") && schema.contains("lib/graphql.ts:50"),
        "the consumer of `query order`, at its line:\n{schema}"
    );

    // And the consumer names the producer, from the other side.
    let client = touch(root, "web-frontend/lib/graphql.ts");
    assert!(
        client.contains("packages/gateway/src/schema.graphql:43"),
        "the producer of `query order`, at its line:\n{client}"
    );

    // A socket edge is a contract like any other: the subscriber is the
    // producer, and the emitter is the consumer.
    let socket = touch(root, "payments-svc/realtime/server.ts");
    assert!(
        socket.contains("payment:settled") && socket.contains("lib/realtime.ts:32"),
        "the socket counterpart, at its line:\n{socket}"
    );
}

/// A service written the ordinary way — routes registered on a typed receiver
/// — is where a local index is thinnest, and the one place it must not read as
/// "there is no API here".
#[test]
fn a_thin_index_says_what_it_could_not_classify() {
    let workspace = workspace("xrepo-corpus-2", &["notifications-svc"]);
    let root = workspace.path();

    let map = index(root);
    assert!(
        map.contains("not classified locally"),
        "the map states why it is thin:\n{map}"
    );
    assert!(
        map.contains("2 route-literal call site(s) counted and unclassified"),
        "and counts what it declined, rather than reporting nothing:\n{map}"
    );

    let routes = check(root, "notifications-svc/src/http/routes.ts");
    assert!(
        routes.contains("no routes or calls indexed in this file"),
        "the file's rows, or the absence of them:\n{routes}"
    );
    assert!(
        routes.contains("2 route-literal call site(s) counted and unclassified"),
        "with the count beside it, in the answer a hook reads:\n{routes}"
    );
}
