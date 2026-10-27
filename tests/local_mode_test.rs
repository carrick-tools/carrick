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

use serial_test::serial;
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
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", workspace.join(".test-credentials"))
        // No resident type daemon: these tests are about the answer, and a
        // re-check would otherwise start one in the background
        // (`typed_daemon_test.rs` covers it).
        .env("CARRICK_TYPED", "off")
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

/// The same, with the model and the cloud mocked: what a test of `carrick
/// index` needs, since that command always infers (carrick#1008). The variable
/// is inherited by the scan subprocesses the indexer spawns, which is where
/// the model would otherwise be called.
fn run_mocked(workspace: &Path, args: &[&str]) -> String {
    let output = Command::new(carrick())
        .args(args)
        .current_dir(workspace)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", workspace.join(".test-credentials"))
        // No resident type daemon: these tests are about the answer, and a
        // re-check would otherwise start one in the background
        // (`typed_daemon_test.rs` covers it).
        .env("CARRICK_TYPED", "off")
        .env("CARRICK_MOCK_ALL", "1")
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

/// The same as [`run_mocked`], with the narration the command writes to
/// stderr. A build states what it did on stderr and prints the index map on
/// stdout, so a test of what it SAID needs both (carrick#1251).
fn run_mocked_output(workspace: &Path, args: &[&str]) -> (String, String) {
    let output = Command::new(carrick())
        .args(args)
        .current_dir(workspace)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", workspace.join(".test-credentials"))
        // No resident type daemon: these tests are about the answer, and a
        // re-check would otherwise start one in the background
        // (`typed_daemon_test.rs` covers it).
        .env("CARRICK_TYPED", "off")
        .env("CARRICK_MOCK_ALL", "1")
        .output()
        .unwrap_or_else(|e| panic!("carrick {args:?}: {e}"));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "carrick {args:?} exited {:?}:\n{stderr}",
        output.status.code(),
    );
    (
        String::from_utf8(output.stdout).expect("stdout was not UTF-8"),
        stderr,
    )
}

/// Build the index with no model and nothing to pay.
///
/// `refresh`, not `index`: since carrick#1008 `carrick index` is the inferred
/// scan and nothing else, so it asks Carrick Cloud and uploads. `refresh` is
/// the pass these tests are about — deterministic rows from the working tree —
/// and it is what the session-start hook runs.
fn index(workspace: &Path) -> String {
    run(workspace, &["refresh", "--workspace", "."])
}

/// The same as [`run`], with extra environment. What a test of the re-check
/// needs: its budget is the one thing that decides which half of the feature
/// runs, and a wall-clock assertion would be a flake on a loaded machine.
fn run_with_env(workspace: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut command = Command::new(carrick());
    command
        .args(args)
        .current_dir(workspace)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", workspace.join(".test-credentials"))
        // No resident type daemon: these tests are about the answer, and a
        // re-check would otherwise start one in the background
        // (`typed_daemon_test.rs` covers it).
        .env("CARRICK_TYPED", "off");
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command
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
#[serial]
fn local_mode_answers_and_follows_an_edit() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    let route = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";
    let route_file = root.join(route);
    let caller = "inventory-svc/src/inventory.ts";

    let map = index(root);
    let retained_indexed_at = check_json(root, caller)["indexed_at"].clone();
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
    // Staleness is `stale`, never the verdict's state: `state` carries the
    // type layer's word, the same one `verdict_state` uses on the PR payload
    // (carrick#731). The row still says so in its detail, for a reader that
    // sees one row and not the envelope.
    assert_eq!(
        stale["items"][0]["verdict"]["state"],
        serde_json::json!("resolved"),
        "the compiler's verdict is still the compiler's verdict:\n{stale:#}"
    );
    assert!(
        stale["items"][0]["verdict"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("unresolved since your edit")),
        "and the row says the tree has moved:\n{stale:#}"
    );

    // Refresh the one service that changed, and the break is the verdict.
    run(
        root,
        &["refresh", "--service", "catalog-web", "--workspace", "."],
    );
    assert_eq!(
        check_json(root, caller)["indexed_at"],
        retained_indexed_at,
        "a scoped refresh must preserve the unscanned service timestamp"
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

    // The two shapes the compiler compared ride on the row (carrick#1033), so
    // a surface states what the other side declares without opening it. The
    // producer returns and the consumer reads, so this is the response half.
    let broken_json = check_json(root, caller);
    let row = broken_json["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["verdict"]["result"] == serde_json::json!("type_mismatch"))
        .unwrap_or_else(|| panic!("a mismatched row:\n{broken_json:#}"));
    assert_eq!(
        row["direction"],
        serde_json::json!("response"),
        "the half of the contract the check compared:\n{broken_json:#}"
    );
    for side in ["actual_type", "expected_type"] {
        assert!(
            row[side].as_str().is_some_and(|text| !text.is_empty()),
            "{side} states the printed type:\n{broken_json:#}"
        );
    }
    // `touch` compares nothing, so it states neither type.
    let touched = touch(root, caller);
    assert!(
        !touched.contains("actual_type") && !touched.contains("expected_type"),
        "touch states no comparison:\n{touched}"
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
        removed["items"][0]["verdict"]["state"],
        serde_json::json!("not_checked"),
        "a removed producer is a routing fact, not a type verdict:\n{removed:#}"
    );
    // The client's own request line (`client.ts:14`, where `send` is handed
    // the URL and the verb) and the two calls through the client that reach it
    // (carrick#1555).
    assert_eq!(
        removed["items"][0]["counterparts"].as_array().map(Vec::len),
        Some(3),
        "with its consumers listed:\n{removed:#}"
    );
}

/// carrick#1036: an edit is judged against the index before it is committed.
///
/// The break here is invisible to routing — the path, the verb and the handler
/// are untouched, only the response type moved — so nothing but a re-run of
/// the extraction and the type check can catch it, and today's answer is the
/// verdict that was reached before the edit.
#[test]
#[serial]
fn an_edit_is_re_judged_before_the_index_catches_up() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    let route = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";
    let route_file = root.join(route);
    let caller = "inventory-svc/src/inventory.ts";
    // Generous on purpose: this binary is built without optimisation and CI is
    // shared, so the default budget would be the thing under test rather than
    // the re-check. What the budget DOES is proven by the degrade case below.
    let budget = [("CARRICK_RECHECK_BUDGET_MS", "600000")];

    index(root);
    assert!(
        check(root, route).contains("compatible"),
        "the fixture starts compatible"
    );

    // The producer now answers with a string where the consumer reads a
    // number. Nothing is re-indexed.
    edit(&route_file, "activeCount: number", "activeCount: string");
    edit(&route_file, "activeCount: 3", "activeCount: \"3\"");

    let indexed = check(root, route);
    assert!(
        indexed.contains("compatible") && !indexed.contains("type_mismatch"),
        "without a re-check the answer is the one computed before the edit:\n{indexed}"
    );

    let text = run_with_env(
        root,
        &["check", route, "--workspace", ".", "--recheck"],
        &budget,
    );
    assert!(
        text.contains("type_mismatch"),
        "the re-check names the break the edit made:\n{text}"
    );
    assert!(
        text.contains("activeCount"),
        "with the compiler's own reason:\n{text}"
    );
    assert!(
        text.contains("re-check: these verdicts are from your working tree"),
        "and says the rows are this run's:\n{text}"
    );
    assert!(
        !text.contains("unresolved since your edit"),
        "a re-checked row is not an indexed row with a warning on it:\n{text}"
    );

    let fresh: serde_json::Value = serde_json::from_str(&run_with_env(
        root,
        &["check", route, "--workspace", ".", "--recheck", "--json"],
        &budget,
    ))
    .expect("check --recheck --json was not JSON");
    assert_eq!(
        fresh["recheck"]["ran"],
        serde_json::json!("extraction+types"),
        "both halves ran:\n{fresh:#}"
    );
    assert!(
        fresh["recheck"]["elapsed_ms"].as_u64().is_some(),
        "with its cost stated:\n{fresh:#}"
    );
    assert_eq!(
        fresh["recheck"]["stale_since"],
        serde_json::Value::Null,
        "a re-check that ran states no age:\n{fresh:#}"
    );
    let row = &fresh["items"][0];
    assert_eq!(row["verdict"]["result"], serde_json::json!("type_mismatch"));
    assert_eq!(row["direction"], serde_json::json!("response"));
    // The two shapes are this run's, so the producer's half carries the string
    // that only exists in the working tree (carrick#1033 + carrick#1036).
    assert!(
        row["actual_type"]
            .as_str()
            .is_some_and(|text| text.contains("activeCount: string")),
        "the producer's fresh response type:\n{fresh:#}"
    );
    assert!(
        row["expected_type"]
            .as_str()
            .is_some_and(|text| text.contains("activeCount: number")),
        "against what the consumer still reads:\n{fresh:#}"
    );
    // Nothing was written: a plain `check` still answers with the verdict the
    // index holds, which is the one from before the edit.
    assert_eq!(
        check_json(root, route)["items"][0]["verdict"]["result"],
        serde_json::json!("compatible"),
        "the re-check must not have rewritten the index"
    );

    // A file in a repo nothing changed keeps its indexed verdicts, and asks
    // for no scan: the consumer is a file the edit never touched.
    let untouched: serde_json::Value = serde_json::from_str(&run_with_env(
        root,
        &["check", caller, "--workspace", ".", "--recheck", "--json"],
        &budget,
    ))
    .expect("check --recheck --json was not JSON");
    // carrick#1374: the block is stated anyway. A reader told to read
    // `recheck.ran` meets an answer on the commonest case of all rather than
    // an absent key, and the reason says the indexed rows ARE this file's.
    assert_eq!(
        untouched["recheck"]["ran"],
        serde_json::json!("none"),
        "nothing was re-extracted, so the rows are the indexed ones:\n{untouched:#}"
    );
    assert_eq!(
        untouched["recheck"]["reason"],
        serde_json::json!("the file has not changed since the index"),
        "and the block says why nothing ran:\n{untouched:#}"
    );
    assert_eq!(
        untouched["recheck"]["elapsed_ms"],
        serde_json::json!(0),
        "no re-check was attempted, so no time was spent on one:\n{untouched:#}"
    );
    assert_eq!(
        untouched["items"][0]["verdict"]["result"],
        serde_json::json!("compatible"),
        "and keeps the verdict the index holds:\n{untouched:#}"
    );
}

/// The re-check names the functions the file declares that the index does not
/// hold (carrick#1330), and says nothing when an edit added none.
///
/// This is what the end-of-task reuse nudge lists, so both halves are the
/// feature: a silent answer on an edit that added no function is what keeps
/// the nudge from firing on every task.
#[test]
#[serial]
fn a_re_check_names_the_functions_the_index_does_not_hold() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    let route = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";
    let route_file = root.join(route);
    let budget = [("CARRICK_RECHECK_BUDGET_MS", "600000")];

    index(root);

    // An edit that changes a type and adds no function. The file is stale, so
    // the re-check runs — and still has nothing to name.
    edit(&route_file, "activeCount: number", "activeCount: string");
    edit(&route_file, "activeCount: 3", "activeCount: \"3\"");
    let unchanged: serde_json::Value = serde_json::from_str(&run_with_env(
        root,
        &["check", route, "--workspace", ".", "--recheck", "--json"],
        &budget,
    ))
    .expect("check --recheck --json was not JSON");
    // `extraction` or `extraction+types` — which of the two depends on whether
    // a type verdict bears on this file's rows, and this test is about the
    // functions, not the verdicts. What matters is that it is not `none`: the
    // file WAS re-extracted, so naming nothing is an answer.
    assert_ne!(
        unchanged["recheck"]["ran"],
        serde_json::json!("none"),
        "the re-check ran, so its silence is an answer:\n{unchanged:#}"
    );
    assert_eq!(
        unchanged["recheck"]["new_functions"],
        serde_json::Value::Null,
        "an edit that added no function names none:\n{unchanged:#}"
    );

    // Two functions the index has never seen, added below one it holds.
    let existing = std::fs::read_to_string(&route_file).expect("read the route file");
    std::fs::write(
        &route_file,
        format!(
            "{existing}\n\
             export function widgetLabel(widget: Widget): string {{\n  \
             return widget.name;\n}}\n\n\
             function countActive(widgets: Widget[]): number {{\n  \
             return widgets.length;\n}}\n\n\
             export const DEFAULT_LABEL = widgetLabel({{ id: \"\", name: \"\", activeCount: \"0\" }});\n"
        ),
    )
    .expect("write the route file");

    let fresh: serde_json::Value = serde_json::from_str(&run_with_env(
        root,
        &["check", route, "--workspace", ".", "--recheck", "--json"],
        &budget,
    ))
    .expect("check --recheck --json was not JSON");
    let named: Vec<&str> = fresh["recheck"]["new_functions"]
        .as_array()
        .unwrap_or_else(|| panic!("no new_functions in:\n{fresh:#}"))
        .iter()
        .map(|entry| entry["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(
        named,
        ["widgetLabel", "countActive"],
        "both new functions, in line order:\n{fresh:#}"
    );
    // `loader` is in this file and in the index, so an edit to the file does
    // not make it new: the comparison is over names, not over the file.
    assert!(
        !named.contains(&"loader"),
        "a function the index holds is not new:\n{fresh:#}"
    );
    // `<module>` is a synthetic definition for the file's own top-level call
    // sites (carrick#965), never something a person wrote.
    assert!(
        !named.iter().any(|name| name.contains("module")),
        "the module-scope row is not a function:\n{fresh:#}"
    );
    for entry in fresh["recheck"]["new_functions"].as_array().unwrap() {
        assert!(
            entry["line"].as_u64().is_some_and(|line| line > 0),
            "each one says where it starts:\n{fresh:#}"
        );
    }

    // The terminal says the same thing, against the commit it compared with.
    let text = run_with_env(
        root,
        &["check", route, "--workspace", ".", "--recheck"],
        &budget,
    );
    assert!(
        text.contains("widgetLabel") && text.contains("countActive"),
        "the rendered answer names them:\n{text}"
    );
    assert!(
        text.contains("new since the index at"),
        "and says what they are new since:\n{text}"
    );

    // A re-scan that misses its budget still names what the edit added
    // (carrick#2849): the names come from a parse of the file, which does not
    // wait on the re-scan. On a large monorepo the re-scan never fits the
    // budget, and the reuse nudge would otherwise never hear of a function.
    let degraded: serde_json::Value = serde_json::from_str(&run_with_env(
        root,
        &["check", route, "--workspace", ".", "--recheck", "--json"],
        &[("CARRICK_RECHECK_BUDGET_MS", "1")],
    ))
    .expect("check --recheck --json was not JSON");
    assert_eq!(
        degraded["recheck"]["ran"],
        serde_json::json!("none"),
        "the verdicts are the indexed ones:\n{degraded:#}"
    );
    let named_degraded: Vec<&str> = degraded["recheck"]["new_functions"]
        .as_array()
        .unwrap_or_else(|| panic!("a missed budget dropped the new functions:\n{degraded:#}"))
        .iter()
        .map(|entry| entry["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(
        named_degraded,
        ["widgetLabel", "countActive"],
        "the same names as a re-check that finished:\n{degraded:#}"
    );
}

/// The budget is real: past it the answer is the indexed one, said as such,
/// and nothing is left running or lying about on disk.
#[test]
#[serial]
fn a_re_check_that_cannot_finish_in_budget_degrades_instead_of_blocking() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    let route = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";
    let route_file = root.join(route);

    index(root);
    edit(&route_file, "activeCount: number", "activeCount: string");
    edit(&route_file, "activeCount: 3", "activeCount: \"3\"");

    let before = recheck_temp_dirs();
    let started = Instant::now();
    let degraded: serde_json::Value = serde_json::from_str(&run_with_env(
        root,
        &["check", route, "--workspace", ".", "--recheck", "--json"],
        &[("CARRICK_RECHECK_BUDGET_MS", "1")],
    ))
    .expect("check --recheck --json was not JSON");

    assert_eq!(
        degraded["recheck"]["ran"],
        serde_json::json!("none"),
        "no half of the re-check ran:\n{degraded:#}"
    );
    assert!(
        degraded["recheck"]["stale_since"].as_str().is_some(),
        "so the answer says when it was computed:\n{degraded:#}"
    );
    assert!(
        degraded["recheck"]["reason"].as_str().is_some(),
        "and why there is no fresher one:\n{degraded:#}"
    );
    // The indexed rows are still served, verdict and all: a re-check that
    // cannot run must never take the answer away.
    assert_eq!(
        degraded["items"][0]["verdict"]["state"],
        serde_json::json!("resolved"),
        "the indexed verdict stands:\n{degraded:#}"
    );
    assert_eq!(degraded["stale"], serde_json::json!(true));
    // The edit changed a type and added no function, so a degraded answer
    // names none either (carrick#2849).
    assert_eq!(
        degraded["recheck"]["new_functions"],
        serde_json::Value::Null,
        "an edit that added no function names none:\n{degraded:#}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "a missed budget answers at once, not eventually: {:?}",
        started.elapsed()
    );
    assert_eq!(
        recheck_temp_dirs(),
        before,
        "a re-check leaves no temporary generation behind, including one it killed"
    );
}

/// A re-check that missed its budget is not started again against the same
/// index: the next one answers at once with the parse and the indexed
/// verdicts, and says why. A new index tries again. On a large monorepo the
/// re-scan never fits, so without this every edit waits out the whole budget
/// for no verdict.
#[test]
#[serial]
fn a_re_check_that_missed_its_budget_is_not_started_again_until_the_index_moves() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    let route = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";
    let route_file = root.join(route);
    let ample = [("CARRICK_RECHECK_BUDGET_MS", "600000")];
    let ask = |env: &[(&str, &str)]| -> serde_json::Value {
        serde_json::from_str(&run_with_env(
            root,
            &["check", route, "--workspace", ".", "--recheck", "--json"],
            env,
        ))
        .expect("check --recheck --json was not JSON")
    };
    let named = |answer: &serde_json::Value| -> Vec<String> {
        answer["recheck"]["new_functions"]
            .as_array()
            .unwrap_or_else(|| panic!("no new_functions in:\n{answer:#}"))
            .iter()
            .map(|entry| entry["name"].as_str().expect("a name").to_string())
            .collect()
    };

    index(root);
    let existing = std::fs::read_to_string(&route_file).expect("read the route file");
    std::fs::write(
        &route_file,
        format!(
            "{existing}\nexport function widgetLabel(name: string): string {{\n  return name;\n}}\n"
        ),
    )
    .expect("write the route file");

    // The miss itself: an answer of indexed age, and nothing skipped yet.
    let missed = ask(&[("CARRICK_RECHECK_BUDGET_MS", "1")]);
    assert_eq!(
        missed["recheck"]["ran"],
        serde_json::json!("none"),
        "{missed:#}"
    );
    assert_eq!(
        missed["recheck"]["known_miss"],
        serde_json::Value::Null,
        "{missed:#}"
    );

    // Same index, ample budget: the re-scan would fit now, and is still not
    // started, because this repo already missed against this index.
    let skipped = ask(&ample);
    assert_eq!(
        skipped["recheck"]["ran"],
        serde_json::json!("none"),
        "a known miss starts no re-scan:\n{skipped:#}"
    );
    assert_eq!(
        skipped["recheck"]["known_miss"],
        serde_json::json!(true),
        "{skipped:#}"
    );
    assert!(
        skipped["recheck"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("check --changed")),
        "the reason names the fresh read:\n{skipped:#}"
    );
    assert_eq!(
        named(&skipped),
        ["widgetLabel"],
        "the parse still answers:\n{skipped:#}"
    );
    assert_eq!(
        skipped["items"][0]["verdict"]["state"],
        serde_json::json!("resolved"),
        "with the indexed verdicts:\n{skipped:#}"
    );

    // A new index is a new generation: the re-check is tried again, and fits.
    std::thread::sleep(Duration::from_millis(1100));
    index(root);
    let existing = std::fs::read_to_string(&route_file).expect("read the route file");
    std::fs::write(
        &route_file,
        format!("{existing}\nexport function widgetCount(names: string[]): number {{\n  return names.length;\n}}\n"),
    )
    .expect("write the route file");
    let fresh = ask(&ample);
    assert_ne!(
        fresh["recheck"]["ran"],
        serde_json::json!("none"),
        "a new index re-checks again:\n{fresh:#}"
    );
    assert_eq!(
        fresh["recheck"]["known_miss"],
        serde_json::Value::Null,
        "{fresh:#}"
    );
    assert_eq!(named(&fresh), ["widgetCount"], "{fresh:#}");
}

/// `check --each` answers several files in one process, each exactly as the
/// single-file `check` would, as one JSON array in the order named. The
/// post-edit hook asks it for a patch that touches several files.
#[test]
#[serial]
fn check_each_answers_every_file_in_one_call() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    let route = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";
    let client = "inventory-svc/src/client.ts";

    index(root);
    let text = run(
        root,
        &[
            "check",
            "--each",
            route,
            client,
            "--workspace",
            ".",
            "--recheck",
            "--json",
        ],
    );
    let answers: Vec<serde_json::Value> =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("not a JSON array: {e}\n{text}"));
    assert_eq!(answers.len(), 2, "{text}");
    for (answer, file) in answers.iter().zip([route, client]) {
        let single: serde_json::Value = serde_json::from_str(&run(
            root,
            &["check", file, "--workspace", ".", "--recheck", "--json"],
        ))
        .expect("check --json was not JSON");
        assert_eq!(
            answer["schema"],
            serde_json::json!("carrick.check/0"),
            "{answer:#}"
        );
        assert_eq!(answer["file"], single["file"], "{answer:#}");
        assert_eq!(
            answer["items"], single["items"],
            "the same rows as `check <file>`:\n{answer:#}"
        );
        assert_eq!(
            answer["recheck"]["ran"], single["recheck"]["ran"],
            "{answer:#}"
        );
    }
}

/// Temporary generations a re-check has left in the system temp directory.
/// Compared before and after rather than required to be empty: another test
/// binary may be running one of its own (carrick#592).
fn recheck_temp_dirs() -> usize {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("carrick-recheck-")
        })
        .count()
}

/// The JSON is the contract in `docs/local-mode-output.md`, and the hook and
/// the LSP shim are built against it.
#[test]
#[serial]
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
    // A call through a client another module declares, stated from the
    // client's body (carrick#1555).
    assert_eq!(
        item["resolution_source"],
        serde_json::json!("request_summary")
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

    // Who uses the file: an object with two lists, and its lines, always a
    // list (carrick#2067).
    assert!(body["uses"]["functions"].is_array(), "{body:#}");
    assert!(body["uses"]["types"].is_array(), "{body:#}");
    assert!(body["uses_lines"].is_array(), "{body:#}");

    // A file nobody indexed still answers, with the boundary and no rows.
    let unknown = check_json(root, "inventory-svc/src/send.ts");
    assert_eq!(
        unknown["items"].as_array().map(Vec::len),
        Some(0),
        "{unknown:#}"
    );
    assert!(unknown["boundary_note"].as_str().is_some());

    // A file outside every indexed repo is an error body — and from
    // carrick#1023 item 2, a non-zero exit, because a refusal from `check` is
    // the absence of an answer rather than a clean one. The body is printed
    // either way, which is what every reader of it depends on.
    let outside = Command::new(carrick())
        .args(["check", "/nowhere/x.ts", "--workspace", ".", "--json"])
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .output()
        .expect("carrick check");
    assert_eq!(outside.status.code(), Some(1));
    let outside: serde_json::Value =
        serde_json::from_slice(&outside.stdout).expect("an error body");
    assert_eq!(outside["error"], serde_json::json!("not_in_workspace"));
}

/// A file names who uses it from elsewhere in its repo (carrick#2067): the
/// callers of a member reached through a typed parameter, and of a bare
/// imported function, folded into the index when it is built.
#[test]
#[serial]
fn a_file_names_who_calls_its_functions() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);

    let client = check_json(root, "inventory-svc/src/client.ts");
    let functions = client["uses"]["functions"].as_array().expect("functions");
    assert_eq!(functions.len(), 1, "{client:#}");
    assert_eq!(
        functions[0],
        serde_json::json!({
            "name": "CatalogClient.readWidget",
            "line": 12,
            "callers_total": 2,
            "callers": [
                { "service": "inventory-svc", "file": "src/inventory.ts", "line": 9, "function": "countActive" },
                { "service": "inventory-svc", "file": "src/inventory.ts", "line": 17, "function": "nameOf" },
            ],
        })
    );
    // The type the file declares is named with the operations that use it
    // (carrick#2148): `Widget` is a `type` alias, and its declaration site is
    // recorded the same as an interface's.
    let types = client["uses"]["types"].as_array().expect("types");
    assert_eq!(types.len(), 1, "{client:#}");
    assert_eq!(types[0]["symbol"], serde_json::json!("Widget"));
    assert_eq!(types[0]["line"], serde_json::json!(3));
    let operations: Vec<(String, u64)> = types[0]["operations"]
        .as_array()
        .expect("operations")
        .iter()
        .map(|op| {
            (
                op["file"].as_str().expect("file").to_string(),
                op["line"].as_u64().expect("line"),
            )
        })
        .collect();
    assert_eq!(
        operations,
        vec![
            ("src/inventory.ts".to_string(), 9),
            ("src/inventory.ts".to_string(), 17)
        ]
    );
    let type_line = "- type Widget (line 3) is the response type of GET /api/v1/widgets/:encoded at 2 call(s): src/inventory.ts:9, src/inventory.ts:17. Producers: catalog-web app/routes/api.v1.widgets.$widgetId.ts:11";
    let line = "- CatalogClient.readWidget (line 12) has 2 caller(s) outside this file: src/inventory.ts:9, src/inventory.ts:17";
    assert_eq!(client["uses_lines"], serde_json::json!([type_line, line]));

    let send = check_json(root, "inventory-svc/src/send.ts");
    assert_eq!(
        send["uses"]["functions"],
        serde_json::json!([{
            "name": "send",
            "line": 9,
            "callers_total": 1,
            "callers": [{ "service": "inventory-svc", "file": "src/client.ts", "line": 14, "function": "CatalogClient.readWidget" }],
        }])
    );
    assert_eq!(
        send["uses_lines"],
        serde_json::json!(["- send (line 9) has 1 caller(s) outside this file: src/client.ts:14"])
    );

    // The caller file declares nothing another file calls.
    let inventory = check_json(root, "inventory-svc/src/inventory.ts");
    assert_eq!(inventory["uses"]["functions"], serde_json::json!([]));
    assert_eq!(inventory["uses_lines"], serde_json::json!([]));

    // The terminal prints the same bytes, and `touch` carries the same block.
    let printed = check(root, "inventory-svc/src/client.ts");
    assert!(
        printed.contains(&format!(
            "used outside this file, as indexed:\n{type_line}\n{line}\n"
        )),
        "{printed}"
    );
    let touched: serde_json::Value = serde_json::from_str(&run(
        root,
        &[
            "touch",
            "inventory-svc/src/client.ts",
            "--workspace",
            ".",
            "--json",
        ],
    ))
    .expect("touch --json was not JSON");
    assert_eq!(touched["uses"], client["uses"]);

    // An index written before the field still reads, and says nothing.
    let items_before = check_json(root, "inventory-svc/src/inventory.ts")["items"].clone();
    let path = root.join(".carrick/index.json");
    let mut index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for repo in index["repos"].as_array_mut().unwrap() {
        repo.as_object_mut().unwrap().remove("uses");
    }
    std::fs::write(&path, serde_json::to_string_pretty(&index).unwrap()).unwrap();
    let older = check_json(root, "inventory-svc/src/client.ts");
    assert!(older.get("uses").is_none(), "{older:#}");
    assert_eq!(older["uses_lines"], serde_json::json!([]));
    assert_eq!(
        check_json(root, "inventory-svc/src/inventory.ts")["items"],
        items_before
    );
}

/// A type entry built without the model records where its type is declared
/// whenever the repo declares it, for a `type` alias as for an interface
/// (carrick#2148). The fixture declares `Widget` as `type Widget = { … }` in
/// both repos: the compiler gives such a type the anonymous `__type` as its
/// own symbol, and the anchor is the alias's name.
#[test]
#[serial]
fn a_type_alias_records_where_it_is_declared() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);

    let entry = |repo: &str, file: &str, line: u64, kind: &str| -> serde_json::Value {
        let blob = stored_blob(root, repo);
        let found: Vec<serde_json::Value> = blob["type_manifest"]
            .as_array()
            .expect("type_manifest")
            .iter()
            .filter(|e| {
                e["file_path"] == serde_json::json!(file)
                    && e["line_number"] == serde_json::json!(line)
                    && e["type_kind"] == serde_json::json!(kind)
            })
            .cloned()
            .collect();
        assert_eq!(found.len(), 1, "{repo} {file}:{line} {kind}: {found:#?}");
        found.into_iter().next().unwrap()
    };

    // Explicit: the route's annotated return names the alias.
    let route = entry(
        "catalog-web",
        "app/routes/api.v1.widgets.$widgetId.ts",
        11,
        "response",
    );
    assert_eq!(route["is_explicit"], serde_json::json!(true), "{route:#}");
    assert_eq!(
        route["defined_in"],
        serde_json::json!({
            "file_path": "app/routes/api.v1.widgets.$widgetId.ts",
            "line_number": 5,
            "symbol": "Widget",
        }),
        "{route:#}"
    );

    // Inferred: the call's result is the alias the client declares.
    let call = entry("inventory-svc", "src/client.ts", 14, "response");
    assert_eq!(call["is_explicit"], serde_json::json!(false), "{call:#}");
    assert_eq!(
        call["defined_in"],
        serde_json::json!({ "file_path": "src/client.ts", "line_number": 3, "symbol": "Widget" }),
        "{call:#}"
    );

    // No declared type, no declaration site: an inline shape, and a request
    // whose type is unknown.
    let health = entry("catalog-web", "app/routes/api.v1.health.ts", 4, "response");
    assert!(health.get("defined_in").is_none(), "{health:#}");
    assert!(health.get("primary_type_symbol").is_none(), "{health:#}");
    let request = entry("inventory-svc", "src/client.ts", 14, "request");
    assert!(request.get("defined_in").is_none(), "{request:#}");
}

/// The workspace question, with no file in it: what a surface opening a
/// session asks (carrick#728).
#[test]
#[serial]
fn status_answers_for_the_workspace() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);

    let body: serde_json::Value =
        serde_json::from_str(&run(root, &["status", "--workspace", ".", "--json"]))
            .expect("status --json was not JSON");
    assert_eq!(body["schema"], serde_json::json!("carrick.status/0"));
    let services = body["services"].as_array().expect("services");
    assert_eq!(services.len(), 2, "{body:#}");

    let producer = services
        .iter()
        .find(|service| service["service"] == serde_json::json!("catalog-web"))
        .expect("the producer service");
    assert_eq!(producer["routes"], serde_json::json!(2));
    assert_eq!(producer["changed_since_index"], serde_json::json!(0));
    assert_eq!(
        producer["stale_files"].as_array().map(Vec::len),
        Some(0),
        "a clean tree lists nothing changed:\n{producer:#}"
    );
    assert!(
        Path::new(producer["repo"].as_str().expect("the repo")).is_dir(),
        "the repo path is openable:\n{producer:#}"
    );
    assert!(
        !producer["boundary_lines"]
            .as_array()
            .expect("boundary_lines")
            .is_empty(),
        "the boundary rides the session answer too:\n{producer:#}"
    );

    // An edit with no refresh is what a session line exists to report.
    edit(
        &root.join("catalog-web/app/routes/api.v1.widgets.$widgetId.ts"),
        "activeCount: number",
        "activeCount: string",
    );
    let after: serde_json::Value =
        serde_json::from_str(&run(root, &["status", "--workspace", ".", "--json"]))
            .expect("status --json was not JSON");
    let producer = after["services"]
        .as_array()
        .expect("services")
        .iter()
        .find(|service| service["service"] == serde_json::json!("catalog-web"))
        .expect("the producer service");
    assert_eq!(producer["changed_since_index"], serde_json::json!(1));
    assert_eq!(
        producer["stale_files"][0],
        serde_json::json!("app/routes/api.v1.widgets.$widgetId.ts"),
        "and names the file:\n{producer:#}"
    );
    assert_eq!(producer["stale_files_total"], serde_json::json!(1));
    assert_eq!(producer["stale_files_truncated"], serde_json::json!(false));

    // Under the session budget, like every other read.
    let mut timings: Vec<Duration> = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        run(root, &["status", "--workspace", "."]);
        timings.push(started.elapsed());
    }
    timings.sort();
    assert!(
        timings[1] < Duration::from_millis(300),
        "a session read must stay under 300 ms; median of three was {:?} ({timings:?})",
        timings[1]
    );
}

/// A read is a read: it must cost what an editor hook can afford. Asserted on
/// the median of three runs, because a debug binary on a loaded CI box is not
/// a stopwatch.
#[test]
#[serial]
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
#[serial]
fn local_mode_answers_over_a_real_corpus() {
    let workspace = workspace(
        "xrepo-corpus-1",
        &["orders-monorepo", "payments-svc", "web-frontend"],
    );
    let root = workspace.path();
    index(root);

    // A GraphQL producer names its consumer in another repo.
    // A file with no indexed rows still answers, and it answers for the
    // service whose DIRECTORY holds it. A monorepo's rowless files are most of
    // its files, and naming the wrong service there is silent and wrong. Both
    // services are asserted because either one alone would pass on whichever
    // service happened to sort first.
    for (file, service) in [
        ("orders-monorepo/packages/gateway/src/index.ts", "(gateway,"),
        (
            "orders-monorepo/packages/orders-pkg/src/types.ts",
            "(orders-pkg,",
        ),
    ] {
        let rowless = touch(root, file);
        assert!(
            rowless.contains(service),
            "{file} belongs to the service whose directory holds it:\n{rowless}"
        );
    }

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
#[serial]
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

/// The scan's progress reaches the indexer, and nothing else does.
///
/// A scan states how far through it is on stderr, for a parent that is
/// rendering a line rather than reading a log (carrick#955). The indexer
/// consumes those lines, so the one thing that must never happen is a raw
/// `@carrick-progress {...}` landing in front of the user: that is what a
/// marker changed on one side and not the other looks like.
#[test]
#[serial]
fn a_scan_states_its_progress_to_the_indexer_and_not_to_the_user() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();

    let output = Command::new(carrick())
        .args(["refresh", "--workspace", "."])
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .output()
        .expect("carrick refresh");
    assert!(output.status.success(), "carrick refresh failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Every marker, not just the counts: a phase and a summary are gated on
    // the same flag, and one emitted without it would reach a user as JSON
    // (carrick#1315).
    for (name, stream) in [("stdout", &stdout), ("stderr", &stderr)] {
        assert!(
            !stream.contains("@carrick-"),
            "nobody asked for markers; one reached {name} raw:\n{stream}"
        );
    }
    // One line per repo either way, because the animated form is a terminal's
    // and this test is a pipe.
    for repo in ["catalog-web", "inventory-svc"] {
        assert!(
            stderr.contains(&format!("indexing {repo}")),
            "the repo being indexed is named while it happens:\n{stderr}"
        );
        assert!(
            stderr.contains(&format!("indexed {repo}")),
            "and again when it is done:\n{stderr}"
        );
    }
    assert!(
        stdout.contains("indexed 2 repo(s)"),
        "the summary is unchanged:\n{stdout}"
    );
}

/// A build that has a parent of its own states its phases and its counts as
/// markers, and the parent renders them (carrick#1315).
///
/// The npm wrapper is that parent: it pipes this process's stderr, draws a
/// spinner per phase and ends on the counts. Everything it draws has to be a
/// marker, because the alternative is what a first run saw — an indicatif
/// spinner rewriting a pipe, arriving as padded fragments sharing one line.
#[test]
#[serial]
fn a_build_states_its_phases_and_counts_to_a_parent_that_is_reading() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();

    let output = Command::new(carrick())
        .args(["refresh", "--workspace", "."])
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .env(carrick::progress::PROGRESS_ENV, "1")
        .output()
        .expect("carrick refresh");
    assert!(output.status.success(), "carrick refresh failed");
    let stderr = String::from_utf8_lossy(&output.stderr);

    let phases: Vec<carrick::progress::PhaseUpdate> = stderr
        .lines()
        .filter_map(carrick::progress::parse_phase)
        .collect();
    for repo in ["catalog-web", "inventory-svc"] {
        for (label, state) in [
            (
                format!("indexing {repo}"),
                carrick::progress::PhaseState::Started,
            ),
            (
                format!("indexed {repo}"),
                carrick::progress::PhaseState::Done,
            ),
        ] {
            assert!(
                phases
                    .iter()
                    .any(|phase| phase.label == label && phase.state == state),
                "no phase marker for '{label}':\n{stderr}"
            );
        }
    }

    // The scans' own counts are passed on, not consumed: the spinner a
    // renderer draws is the one the scan is filling in.
    assert!(
        stderr
            .lines()
            .any(|line| carrick::progress::parse(line).is_some()),
        "no scan's progress reached the parent:\n{stderr}"
    );

    let summary = stderr
        .lines()
        .find_map(carrick::progress::parse_summary)
        .unwrap_or_else(|| panic!("the build stated no summary:\n{stderr}"));
    assert_eq!(
        summary.services.len(),
        2,
        "one row per indexed service:\n{stderr}"
    );
    assert!(
        summary.services.iter().any(|service| service.routes > 0),
        "the routes the map prints are the routes the summary carries:\n{stderr}"
    );
    assert!(summary.elapsed_secs > 0.0, "{stderr}");
}

/// Rewrite a file with the bytes it already has, and push its mtime forward so
/// the write is visible whatever the filesystem's timestamp granularity is.
///
/// This is what an editor with autosave on does to a buffer nobody edited, what
/// a formatter does when it has nothing to fix, and what `git checkout --` and
/// `git stash pop` do when they restore identical content.
fn rewrite_identically(file: &Path) {
    let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    std::fs::write(file, &text).expect("write the same bytes back");
    let handle = std::fs::File::options()
        .write(true)
        .open(file)
        .expect("reopen to set the mtime");
    handle
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(2))
        .expect("set the mtime forward");
}

/// A write that changes no bytes is not a change: git compares content, and
/// where git can answer, its answer is the whole answer (carrick#857).
///
/// Every other test here edits a file to make it stale. This one rewrites one
/// without changing it, which is the ordinary case on the editor surface: the
/// LSP checks a file on every save, so an autosave of an untouched buffer used
/// to caveat every verdict in that file until the next index.
#[test]
#[serial]
fn an_identical_rewrite_is_not_a_change() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    let route = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";
    let route_file = root.join(route);

    index(root);
    let fresh = check_json(root, route);
    assert_eq!(
        fresh["stale"],
        serde_json::json!(false),
        "a just-indexed file is not stale:\n{fresh:#}"
    );

    rewrite_identically(&route_file);

    let after = check_json(root, route);
    assert_eq!(
        after["changed_since_index"],
        serde_json::json!(0),
        "git sees no change, because there is none:\n{after:#}"
    );
    assert_eq!(
        after["stale"],
        serde_json::json!(false),
        "and `stale` says the same thing the count does:\n{after:#}"
    );
    for item in after["items"].as_array().expect("items") {
        let detail = item["verdict"]["detail"].as_str().unwrap_or_default();
        assert!(
            !detail.contains("has changed since it was indexed"),
            "no verdict carries a caveat this same response denies:\n{after:#}"
        );
    }
}

/// Where git cannot answer at all, the file's mtime is the only signal there
/// is, and it still says the tree has moved. The fallback is not removed by
/// carrick#857, only demoted to the case it was written for.
#[test]
#[serial]
fn without_git_the_mtime_is_the_only_signal() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    let route = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";
    let route_file = root.join(route);

    index(root);
    // The tarball case: the index holds a commit, and nothing on disk can
    // resolve it any more.
    std::fs::remove_dir_all(root.join("catalog-web").join(".git")).expect("remove the repository");

    rewrite_identically(&route_file);

    let after = check_json(root, route);
    assert_eq!(
        after["stale"],
        serde_json::json!(true),
        "an unanswerable tree falls back to the write:\n{after:#}"
    );
    assert_eq!(
        after["changed_since_index"],
        serde_json::json!(1),
        "and the count is the same signal, never a contradicting one:\n{after:#}"
    );
}

/// A file no service's scan reads belongs to the repo, once — not to every
/// service in the monorepo (carrick#997 item 4).
///
/// The monorepo fixture is the shape that shows it: two services with their
/// own directories, and repo-level files that neither of them reads.
///
/// And only the ones a scan reads: a changed workflow or `renovate.json`
/// holds no indexed row, so it cannot make one stale, and counting it read as
/// drift on a tree nobody had touched (carrick#1007 item 5).
#[test]
#[serial]
fn status_attributes_a_repo_level_file_to_the_repo_and_not_to_every_service() {
    let workspace = workspace("xrepo-corpus-1", &["orders-monorepo"]);
    let root = workspace.path();
    index(root);

    let repo = root.join("orders-monorepo");
    // What onboarding leaves behind: no service reads these, and no index row
    // comes from them either.
    std::fs::write(repo.join("renovate.json"), "{}\n").expect("a repo-level file");
    std::fs::create_dir_all(repo.join(".github/workflows")).expect("the workflow directory");
    std::fs::write(repo.join(".github/workflows/carrick.yml"), "on: push\n").expect("a workflow");
    // A source file outside every service: this one IS drift the repo owns.
    std::fs::create_dir_all(repo.join("tools")).expect("the tools directory");
    std::fs::write(repo.join("tools/release.ts"), "export const a = 1;\n").expect("a script");
    // A source file inside one service: the service's own drift. A note or a
    // README beside it would not be — no row comes from one.
    std::fs::write(
        repo.join("packages/gateway/notes.ts"),
        "export const note = 1;\n",
    )
    .expect("a file inside a service");
    std::fs::write(
        repo.join("packages/gateway/notes.txt"),
        "inside one service, and read by no scan\n",
    )
    .expect("a file no scan reads");

    let body: serde_json::Value =
        serde_json::from_str(&run(root, &["status", "--workspace", ".", "--json"]))
            .expect("status --json was not JSON");
    let service = |name: &str| -> serde_json::Value {
        body["services"]
            .as_array()
            .expect("services")
            .iter()
            .find(|service| service["service"] == serde_json::json!(name))
            .unwrap_or_else(|| panic!("no service named {name} in {body:#}"))
            .clone()
    };

    let gateway = service("gateway");
    assert_eq!(
        gateway["changed_since_index"],
        serde_json::json!(1),
        "only the file inside this service:\n{gateway:#}"
    );
    assert_eq!(
        gateway["stale_files"][0],
        serde_json::json!("packages/gateway/notes.ts"),
        "{gateway:#}"
    );
    let sibling = service("orders-pkg");
    assert_eq!(
        sibling["changed_since_index"],
        serde_json::json!(0),
        "a sibling service reads none of those three files:\n{sibling:#}"
    );

    let repos = body["repos"].as_array().expect("repos");
    assert_eq!(repos.len(), 1, "{body:#}");
    assert_eq!(
        repos[0]["outside_every_service"],
        serde_json::json!(1),
        "the one repo-level SOURCE file, stated once:\n{:#}",
        repos[0]
    );
    // Five files moved and two of them are files a scan reads: the one inside
    // the service and the one outside every service. The other three hold no
    // row in any index, so they are not what "changed since this index was
    // built" is about (carrick#1365).
    assert_eq!(repos[0]["changed_since_index"], serde_json::json!(2));
    let outside: Vec<&str> = repos[0]["stale_files"]
        .as_array()
        .expect("stale_files")
        .iter()
        .map(|file| file.as_str().expect("a path"))
        .collect();
    assert_eq!(outside, vec!["tools/release.ts"]);

    let rendered = run(root, &["status", "--workspace", "."]);
    assert!(
        rendered.contains("orders-monorepo: 1 file(s) changed outside every service"),
        "and the human form says it under the repo:\n{rendered}"
    );
    assert!(
        !rendered.contains("carrick.yml") && !rendered.contains("renovate.json"),
        "a file no scan reads is not drift:\n{rendered}"
    );
}

/// A pass that ran no model states what is waiting for the paid one, so
/// `0 route(s) 0 call(s)` is not the same table cell as a service with no API
/// in it (carrick#997 item 8).
#[test]
#[serial]
fn the_free_pass_counts_what_is_waiting_for_the_paid_one() {
    let workspace = workspace("xrepo-corpus-2", &["notifications-svc"]);
    let root = workspace.path();

    let map = index(root);
    let waiting = map
        .lines()
        .find(|line| line.contains("candidate(s) waiting for `carrick index`"))
        .unwrap_or_else(|| panic!("no service line states what is waiting:\n{map}"));
    assert!(
        waiting.contains("route(s)") && waiting.contains("call(s)"),
        "it is on the service's own table line:\n{map}"
    );

    let status = run(root, &["status", "--workspace", "."]);
    assert!(
        status.contains("candidate(s) waiting for `carrick index`"),
        "and the session answer says it too:\n{status}"
    );
}

/// A mistyped command is answered as a command. It used to reach the scan and
/// come back as a missing repository path, which reads as a broken checkout
/// (carrick#997 item 6).
#[test]
#[serial]
fn an_unknown_subcommand_is_not_read_as_a_repository_path() {
    let dir = tempfile::tempdir().expect("temp dir");
    let output = Command::new(carrick())
        .arg("whoami")
        .current_dir(dir.path())
        .output()
        .expect("carrick whoami");
    assert_eq!(output.status.code(), Some(2), "a usage error, not a scan");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not a carrick command"), "{stderr}");
    assert!(
        stderr.contains("status"),
        "and names the commands:\n{stderr}"
    );
    assert!(
        !stderr.contains("does not exist or is not a directory"),
        "never as a path:\n{stderr}"
    );

    // The package's own commands are named, not denied.
    let init = Command::new(carrick())
        .arg("init")
        .current_dir(dir.path())
        .output()
        .expect("carrick init");
    assert!(
        String::from_utf8_lossy(&init.stderr).contains("npm i -g carrick"),
        "{:?}",
        String::from_utf8_lossy(&init.stderr)
    );

    // And a path that does not exist is still a path.
    let path = Command::new(carrick())
        .arg("./no-such-repo")
        .current_dir(dir.path())
        .output()
        .expect("carrick ./no-such-repo");
    assert!(
        String::from_utf8_lossy(&path.stderr).contains("does not exist or is not a directory"),
        "{:?}",
        String::from_utf8_lossy(&path.stderr)
    );
}

/// The scan outlives the command that started it, and the command comes back
/// at once with the id to watch it by (carrick#992).
///
/// The shell an agent runs `carrick index` through caps a command at two
/// minutes by default and ten at most, and a first inferred scan of a
/// mid-sized monorepo takes about fifteen. The model is mocked here — the
/// timing is what is under test, not the classification — so what it proves is
/// the shape: the parent returns, the child finishes the build on its own, its
/// output is in the log, and the state file is gone once it is done.
///
/// `carrick index` is the inferred scan since carrick#1008, so the two things
/// it demands are here: a `carrick.json` in every repo, which is what its
/// refusal checks, and `CARRICK_MOCK_ALL`, which keeps the run off the wire.
#[test]
#[serial]
fn a_detached_scan_outlives_the_command_that_started_it() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    for repo in ["catalog-web", "inventory-svc"] {
        std::fs::write(root.join(repo).join("carrick.json"), "{}\n").expect("write a config");
    }

    let started = Instant::now();
    let stdout = run_mocked(root, &["index", "--workspace", ".", "--detach"]);
    let returned_in = started.elapsed();

    assert!(
        stdout.contains("started in the background"),
        "the command answers with the scan, not with the index:\n{stdout}"
    );
    let scan_id = stdout
        .split_whitespace()
        .nth(1)
        .expect("the id is the second word")
        .to_string();
    assert_eq!(scan_id.len(), 8, "a short id to type: {stdout}");
    assert!(
        stdout.contains(&format!("scan-{scan_id}.log")),
        "and names the log to tail:\n{stdout}"
    );

    let log = root.join(".carrick").join(format!("scan-{scan_id}.log"));
    let state = root.join(".carrick").join(format!("scan-{scan_id}.json"));
    assert!(
        log.is_file(),
        "the log exists before the scan does anything"
    );

    // The build itself takes tens of seconds; returning is immediate. The
    // bound is generous because a debug binary on a loaded box is not a
    // stopwatch — an order of magnitude is the claim, not a millisecond.
    assert!(
        returned_in < Duration::from_secs(10),
        "the command returned in {returned_in:?}, which is not 'at once'"
    );

    // Wait on the index, not on the state file: the child writes that file a
    // moment after it starts, so its absence right now means "not yet", not
    // "done".
    let index = root.join(".carrick").join("index.json");
    let deadline = Instant::now() + Duration::from_secs(300);
    while Instant::now() < deadline && !index.is_file() {
        std::thread::sleep(Duration::from_millis(250));
    }
    // The record is rewritten after the index is written, so give the child
    // the moment between the two.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline
        && !std::fs::read_to_string(&state).is_ok_and(|text| text.contains("\"finished\""))
    {
        std::thread::sleep(Duration::from_millis(250));
    }
    // The word the scaffold tells an agent to poll for. The record is kept
    // until the next build so that `carrick status` can say it once
    // (carrick#1007 item 4).
    let record = std::fs::read_to_string(&state).expect("the finished record is kept");
    assert!(record.contains("\"status\": \"finished\""), "{record}");
    assert!(record.contains("finished_at"), "{record}");
    assert!(
        index.is_file(),
        "the detached child built the index this process never waited for:\n{}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    let printed = std::fs::read_to_string(&log).expect("the log is readable");
    assert!(
        printed.contains("indexed 2 repo(s)"),
        "the whole run is in the log, including the map:\n{printed}"
    );
    // And the index it wrote answers like any other, with the scan's own
    // outcome above it.
    let status = run(root, &["status", "--workspace", "."]);
    assert!(status.contains("catalog-web"), "{status}");
    assert!(
        status.contains("finished after") && status.contains("The index is written"),
        "status says the word the poll waits for:\n{status}"
    );
    // The log's last line is the scan's outcome, not a count that stopped.
    let printed = std::fs::read_to_string(&log).expect("the log is readable");
    assert!(
        printed.lines().any(|line| line.contains("finished after")),
        "the log ends on the outcome:\n{printed}"
    );

    // The next build is what forgets it — `refresh` here, which is the build
    // that pays for nothing (carrick#1008).
    run(root, &["refresh", "--workspace", "."]);
    assert!(
        !state.exists(),
        "a build clears the finished record it found:\n{}",
        std::fs::read_to_string(&state).unwrap_or_default()
    );
}

/// The field report behind carrick#1132: `carrick index` run in the foreground
/// for fifteen minutes, and `carrick status` from a second terminal answered
/// every minute that there was no index and to run `carrick index`.
///
/// So the foreground build is started here the way a terminal starts it — in
/// a process group of its own, which is what a Ctrl-C is delivered to — and
/// asked about while it runs. Then it gets the Ctrl-C, and the record it leaves
/// has to say that, not `running` with a pid that is gone.
#[cfg(unix)]
#[test]
#[serial]
fn a_foreground_scan_is_named_by_status_and_says_it_was_interrupted() {
    use std::os::unix::process::CommandExt;

    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    for repo in ["catalog-web", "inventory-svc"] {
        std::fs::write(root.join(repo).join("carrick.json"), "{}\n").expect("write a config");
    }
    let output = std::fs::File::create(root.join("foreground.out")).expect("output file");
    let mut build = Command::new(carrick())
        .args(["index", "--workspace", "."])
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .env("CARRICK_MOCK_ALL", "1")
        .stdin(std::process::Stdio::null())
        .stdout(output.try_clone().expect("output file"))
        .stderr(output)
        .process_group(0)
        .spawn()
        .expect("start carrick index");
    let pid = build.id();
    let printed = || std::fs::read_to_string(root.join("foreground.out")).unwrap_or_default();

    // The record appears as the build starts, before `.carrick` holds anything
    // else; wait on it rather than on a clock.
    let index_dir = root.join(".carrick");
    let deadline = Instant::now() + Duration::from_secs(120);
    let record = loop {
        let found = std::fs::read_dir(&index_dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("scan-") && name.ends_with(".json"))
            })
            .filter_map(|path| std::fs::read_to_string(&path).ok().map(|text| (path, text)))
            .filter_map(|(path, text)| {
                serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .map(|body| (path, body))
            })
            .find(|(_, body)| body["pid"] == serde_json::json!(pid));
        if let Some(found) = found {
            break found;
        }
        assert!(
            build.try_wait().expect("poll the build").is_none(),
            "the build ended without ever recording its scan:\n{}",
            printed()
        );
        assert!(
            Instant::now() < deadline,
            "no scan record for the foreground build:\n{}",
            printed()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let (state, body) = record;
    let scan_id = body["scan_id"].as_str().expect("a scan id").to_string();
    assert_eq!(scan_id.len(), 8, "{body:#}");

    // A mocked build is over in about a second, which is not long enough to
    // ask it anything or to Ctrl-C it at a known moment. So it is held: the
    // group is stopped at a moment the build has a scan subprocess out, and
    // only the build is let go again. It then waits on a scan that is not
    // moving, which is a fifteen-minute scan as far as anything here can tell.
    let group = libc::pid_t::try_from(pid).expect("a pid that fits");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        // SAFETY: signals to the process group this test created.
        unsafe { libc::kill(-group, libc::SIGSTOP) };
        // A child of the build that is stopped, not one that has exited and
        // is waiting to be reaped: the build is past a zombie.
        let table = Command::new("ps")
            .args(["-A", "-o", "ppid=,stat="])
            .output()
            .expect("ps");
        let holding = String::from_utf8_lossy(&table.stdout).lines().any(|row| {
            let mut columns = row.split_whitespace();
            columns.next() == Some(pid.to_string().as_str())
                && columns.next().is_some_and(|stat| stat.starts_with('T'))
        });
        if holding {
            unsafe { libc::kill(group, libc::SIGCONT) };
            break;
        }
        unsafe { libc::kill(-group, libc::SIGCONT) };
        assert!(
            build.try_wait().expect("poll the build").is_none(),
            "the build ended before it could be held:\n{}",
            printed()
        );
        assert!(
            Instant::now() < deadline,
            "the build never ran a scan:\n{}",
            printed()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let held: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).expect("the record"))
            .expect("the record is JSON");
    assert_eq!(held["status"], serde_json::json!("running"), "{held:#}");

    // What the second terminal reads. The build may have written an index by
    // now or not, so the whole output is read, and neither half may tell the
    // user to start the scan that is running.
    let asked = Command::new(carrick())
        .args(["status", "--workspace", "."])
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .output()
        .expect("carrick status");
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&asked.stdout),
        String::from_utf8_lossy(&asked.stderr)
    );
    assert!(
        said.contains(&format!("scan {scan_id} running for")),
        "status names the foreground scan:\n{said}"
    );
    assert!(
        !said.contains("Run `carrick index"),
        "and never orders the scan that is running:\n{said}"
    );
    assert!(
        !said.contains(&format!("scan-{scan_id}.log")) && !said.contains("scan-<id>.log"),
        "a foreground scan wrote no log in .carrick to point at:\n{said}"
    );
    let text = run(root, &["status", "--workspace", ".", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("status --json was not JSON: {e}\n{text}"));
    assert_eq!(
        json["running_scans"][0]["scan_id"],
        serde_json::json!(scan_id),
        "{json:#}"
    );

    // Ctrl-C, to the whole group, as a terminal sends it.
    // SAFETY: a signal to the process group this test created.
    unsafe { libc::kill(-group, libc::SIGINT) };
    let deadline = Instant::now() + Duration::from_secs(60);
    let exit = loop {
        if let Some(exit) = build.try_wait().expect("poll the build") {
            break exit;
        }
        assert!(
            Instant::now() < deadline,
            "the build ignored the Ctrl-C:\n{}",
            printed()
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    // Whatever of the group is still going is not under test; it must not
    // outlive the workspace it is writing into.
    // SAFETY: as above.
    unsafe { libc::kill(-group, libc::SIGKILL) };
    assert_eq!(exit.code(), Some(130), "{}", printed());

    let ended: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&state).expect("the record is kept"))
            .expect("the record is JSON");
    assert_eq!(ended["status"], serde_json::json!("failed"), "{ended:#}");
    assert!(
        ended["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("interrupted by SIGINT")),
        "{ended:#}"
    );
    let rendered = run(root, &["status", "--workspace", "."]);
    assert!(
        rendered.contains(&format!("scan {scan_id} failed after"))
            && rendered.contains("interrupted by SIGINT"),
        "status says the scan was interrupted, not that it stopped:\n{rendered}"
    );
}

/// A scan killed part-way is the case the flag exists for, so it is the one
/// `carrick status` must name rather than going quiet (carrick#992).
///
/// The state file is written here rather than by killing a real scan: what is
/// under test is the reading, and a test that races a subprocess to kill it at
/// the right moment proves less, not more.
#[test]
#[serial]
fn status_names_a_scan_that_stopped_without_finishing() {
    let workspace = workspace("local-mode-workspace", &["catalog-web"]);
    let root = workspace.path();
    index(root);

    let state = serde_json::json!({
        "scan_id": "5089ed60",
        // Above every platform's pid_max, and positive: `kill` reads a
        // negative argument as a process group rather than a process.
        "pid": i32::MAX,
        "started_at": "2026-09-12T10:00:00Z",
        "updated_at": "2026-09-12T10:09:41Z",
        "infer": true,
        "workspace": root.to_string_lossy(),
        "status": "running",
        "phase": "indexing catalog-web",
        "progress": {
            "service": "catalog-web",
            "service_index": 1,
            "service_total": 2,
            "phase": "files",
            "done": 118,
            "total": 240
        }
    });
    std::fs::write(
        root.join(".carrick").join("scan-5089ed60.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .expect("write the scan state");

    let rendered = run(root, &["status", "--workspace", "."]);
    assert!(
        rendered.contains("scan 5089ed60 stopped without finishing"),
        "a scan whose process is gone is named:\n{rendered}"
    );
    assert!(
        rendered.contains("indexing catalog-web") && rendered.contains("118 of 240 files"),
        "with where it got to:\n{rendered}"
    );
    assert!(
        rendered.contains("run the command again"),
        "and what to do about it:\n{rendered}"
    );

    let body: serde_json::Value =
        serde_json::from_str(&run(root, &["status", "--workspace", ".", "--json"]))
            .expect("status --json was not JSON");
    assert_eq!(
        body["running_scans"][0]["scan_id"],
        serde_json::json!("5089ed60"),
        "and a reader parsing JSON gets it too:\n{body:#}"
    );
}

/// A workspace with no index at all, and a scan building one: the answer is
/// the scan, in whichever form the caller asked for. `--json` must stay
/// parseable — a line of prose in front of the body is not an answer
/// (carrick#992).
#[test]
#[serial]
fn a_scan_is_reported_before_there_is_any_index_to_report() {
    let workspace = workspace("local-mode-workspace", &["catalog-web"]);
    let root = workspace.path();
    std::fs::create_dir_all(root.join(".carrick")).expect("the index directory");
    std::fs::write(
        root.join(".carrick").join("scan-5089ed60.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "scan_id": "5089ed60",
            "pid": std::process::id(),
            "started_at": "2026-09-12T10:00:00Z",
            "updated_at": "2026-09-12T10:00:41Z",
            "infer": true,
            "workspace": root.to_string_lossy(),
            "status": "running",
            "phase": "indexing catalog-web",
        }))
        .unwrap(),
    )
    .expect("write the scan state");

    let rendered = run(root, &["status", "--workspace", "."]);
    assert!(
        rendered.contains("scan 5089ed60 running"),
        "the scan is the answer when there is no index yet:\n{rendered}"
    );

    // And the refusal beside it is written for THIS command: it takes no file,
    // and it must not order the scan that is already running (carrick#1023
    // item 1). The sentence is on stderr, so the whole output is read here.
    let refused = Command::new(carrick())
        .args(["status", "--workspace", "."])
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .output()
        .expect("carrick status");
    let said = String::from_utf8_lossy(&refused.stderr).to_string();
    assert!(
        !said.contains("for this file"),
        "`status` takes no file:\n{said}"
    );
    assert!(
        !said.contains("Run `carrick index"),
        "and never orders the scan that is running:\n{said}"
    );
    assert!(
        said.contains("the scan above is still building it"),
        "it points at the scan it just printed:\n{said}"
    );

    let text = run(root, &["status", "--workspace", ".", "--json"]);
    let body: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("status --json was not JSON: {e}\n{text}"));
    assert_eq!(body["error"], serde_json::json!("not_indexed"));
    assert_eq!(
        body["running_scans"][0]["scan_id"],
        serde_json::json!("5089ed60"),
        "and the error body carries the scan:\n{body:#}"
    );
}

/// `check` is the scripted read, and until carrick#1023 item 2 a refusal and a
/// clean verdict were the same exit code, so nothing downstream could tell
/// "no contract problems" from "no index at all". `touch` — the editor's read
/// — still exits 0, because an edit must never fail on a missing index.
#[test]
#[serial]
fn a_check_refusal_exits_non_zero_and_a_touch_refusal_does_not() {
    let workspace = workspace("local-mode-workspace", &["catalog-web"]);
    let root = workspace.path();
    let file = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";
    assert!(root.join(file).is_file(), "the fixture moved");

    let refused = Command::new(carrick())
        .args(["check", file, "--workspace", ".", "--json"])
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .output()
        .expect("carrick check");
    assert_eq!(
        refused.status.code(),
        Some(1),
        "a check with no index refuses:\n{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("no local index for this file"),
        "and says why on stderr:\n{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    // The body is still printed, which is what keeps the language server and
    // the edit hook answering through a refusal (carrick#1009).
    let body: serde_json::Value = serde_json::from_slice(&refused.stdout)
        .unwrap_or_else(|e| panic!("check --json was not JSON: {e}"));
    assert_eq!(body["error"], serde_json::json!("not_indexed"));
    assert!(body["message"].as_str().is_some_and(|m| !m.is_empty()));

    let touched = Command::new(carrick())
        .args(["touch", file, "--workspace", "."])
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .output()
        .expect("carrick touch");
    assert_eq!(
        touched.status.code(),
        Some(0),
        "the editor's read never fails an edit:\n{}",
        String::from_utf8_lossy(&touched.stderr)
    );

    // With an index, a verdict never moves the code — that is the half of the
    // old rule that stands.
    index(root);
    let answered = Command::new(carrick())
        .args(["check", file, "--workspace", "."])
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .output()
        .expect("carrick check");
    assert_eq!(
        answered.status.code(),
        Some(0),
        "an answered check exits 0 whatever it found:\n{}",
        String::from_utf8_lossy(&answered.stderr)
    );
}

/// A failed scan's record is the only account of what went wrong, so it is
/// kept — but a later build that succeeded makes it history, and `carrick
/// status` was still leading with it after a good index had landed
/// (carrick#1023 item 13). The sweep is the paid pass's: `refresh` runs from
/// the session-start hook, and a hook must not be the thing that erases the
/// evidence.
#[test]
#[serial]
fn a_successful_index_supersedes_the_record_of_the_scan_before_it() {
    let workspace = workspace("local-mode-workspace", &["catalog-web"]);
    let root = workspace.path();
    std::fs::write(root.join("catalog-web").join("carrick.json"), "{}\n").expect("a config");
    let index_dir = root.join(".carrick");
    std::fs::create_dir_all(&index_dir).expect("the index directory");
    let failed = index_dir.join("scan-12d9106f.json");
    let record = serde_json::json!({
        "scan_id": "12d9106f",
        "pid": i32::MAX,
        "started_at": "2026-09-13T16:00:00Z",
        "updated_at": "2026-09-13T16:00:34Z",
        "finished_at": "2026-09-13T16:00:34Z",
        "infer": true,
        "workspace": root.to_string_lossy(),
        "status": "failed",
        "phase": "indexing catalog-web",
        "error": "the scan of catalog-web failed",
    });
    std::fs::write(&failed, serde_json::to_vec_pretty(&record).unwrap()).expect("write it");

    // A free pass leaves it alone: nothing it did contradicts the failure.
    run(root, &["refresh", "--workspace", "."]);
    assert!(
        failed.is_file(),
        "`refresh` runs from a hook and keeps the only account of the failure"
    );
    let rendered = run(root, &["status", "--workspace", "."]);
    assert!(rendered.contains("scan 12d9106f failed"), "{rendered}");

    // The paid pass wrote an index, so the failure describes a world that is
    // gone.
    run_mocked(root, &["index", "--workspace", "."]);
    assert!(
        !failed.exists(),
        "a successful index supersedes the record before it:\n{}",
        std::fs::read_to_string(&failed).unwrap_or_default()
    );
    let rendered = run(root, &["status", "--workspace", "."]);
    assert!(
        !rendered.contains("12d9106f"),
        "and `status` stops leading with it:\n{rendered}"
    );
}

/// `carrick index --dispatch` hands the analysis over and says so, and the
/// workspace remembers the jobs (carrick#1229).
///
/// The scan that dispatches writes no index — that is the point of it — so the
/// two things that must survive the command are the record of what is being
/// analysed and a line telling the user how it arrives. The offline storage
/// takes the job in place of the cloud, so this runs on a machine with no
/// credential and no network.
#[test]
#[serial]
fn a_dispatched_index_records_its_jobs_and_says_the_analysis_is_elsewhere() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    for repo in ["catalog-web", "inventory-svc"] {
        std::fs::write(root.join(repo).join("carrick.json"), "{}\n").expect("write a config");
    }

    let stdout = run_mocked(root, &["index", "--dispatch", "--workspace", "."]);
    assert!(
        stdout.contains("Carrick Cloud is analysing"),
        "the command says who is doing the work:\n{stdout}"
    );
    assert!(
        stdout.contains("carrick resume"),
        "and how the index arrives:\n{stdout}"
    );
    assert!(
        !stdout.contains('$') && !stdout.to_lowercase().contains("paid"),
        "what the analysis costs us is never a customer's line (carrick#1236):\n{stdout}"
    );
    assert!(
        !stdout.contains("The index is written"),
        "no index was written, and the record must not say one was:\n{stdout}"
    );

    // The scan record says what happened to it. Written by the build rather
    // than by the scan, and not rewritten as "finished" on the way out.
    let record = std::fs::read_dir(root.join(".carrick"))
        .expect(".carrick")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("scan-") && name.ends_with(".json"))
        })
        .and_then(|path| std::fs::read_to_string(path).ok())
        .expect("the build recorded its scan");
    let state: serde_json::Value = serde_json::from_str(&record).expect("scan record is json");
    assert_eq!(
        state["status"], "dispatched",
        "the record says the analysis went elsewhere:\n{record}"
    );
    assert!(
        !state["jobs"].as_array().unwrap_or(&Vec::new()).is_empty(),
        "and names the job it is waiting on:\n{record}"
    );

    let recorded = std::fs::read_to_string(root.join(".carrick/jobs.json"))
        .expect("the jobs outlive the command that dispatched them");
    let jobs: serde_json::Value = serde_json::from_str(&recorded).expect("jobs.json is json");
    let jobs = jobs["jobs"]
        .as_array()
        .expect("one entry per repo handed over");
    // One per repo that had anything for the model. A repo whose files raise
    // no candidate has no job to wait for and was indexed here instead, which
    // is why this is a floor and not an equality.
    assert!(!jobs.is_empty(), "something was handed over:\n{recorded}");
    for job in jobs {
        // `/private/var` on macOS against the `/var` the test built: compare
        // the tail, which is what says this is that repo.
        let path = job["path"].as_str().unwrap_or_default();
        assert!(
            std::path::Path::new(path).ends_with(job["repo"].as_str().unwrap_or("nothing")),
            "each names the tree a resume rebuilds the prompts from:\n{recorded}"
        );
        assert!(
            !job["job_id"].as_str().unwrap_or_default().is_empty(),
            "each names the job to collect:\n{recorded}"
        );
        assert!(
            job["analyze_rows"].as_u64().unwrap_or_default() > 0,
            "and how many files it carries:\n{recorded}"
        );
    }

    // And `status` leads with it. Without the record this reads "no index
    // here", which is the wrong answer while the work that builds it is
    // running somewhere else. The cloud cannot be asked on this machine, so
    // the line says that rather than pretending.
    let rendered = run(root, &["status", "--workspace", "."]);
    assert!(
        rendered.contains("Carrick Cloud is analysing"),
        "status names the analysis in flight:\n{rendered}"
    );
    assert!(
        rendered.contains("could not ask"),
        "and says it could not reach the cloud rather than inventing progress:\n{rendered}"
    );
}

/// `carrick index --dispatch` that has nothing to hand over says so, and says
/// where the index came from instead (carrick#1251).
///
/// This is the ordinary outcome of `--dispatch`, not an edge: a warm analysis
/// cache is the normal state of every scan after the first, and a repo whose
/// files raise no candidate never had anything for the model. It used to print
/// nothing whatever, which is indistinguishable from a flag that was ignored,
/// misspelled or broken — and it cost a session three unnoticed synchronous
/// runs.
///
/// The repo here is the second kind: sources with no candidate in them, so no
/// prompt is built and the collector is empty.
#[test]
#[serial]
fn a_dispatch_with_nothing_to_hand_over_says_so() {
    let workspace = workspace("local-mode-workspace", &["inventory-svc"]);
    let root = workspace.path();
    let repo = root.join("inventory-svc");
    std::fs::write(repo.join("carrick.json"), "{}\n").expect("write a config");
    // Nothing a model would be asked about: no route, no call, no schema.
    for file in std::fs::read_dir(repo.join("src")).expect("the fixture's sources") {
        std::fs::remove_file(file.expect("dir entry").path()).expect("remove it");
    }
    std::fs::write(
        repo.join("src/arithmetic.ts"),
        "export function add(a: number, b: number): number {\n  return a + b;\n}\n",
    )
    .expect("write the one file");

    let (stdout, stderr) = run_mocked_output(root, &["index", "--dispatch", "--workspace", "."]);
    let said = format!("{stdout}\n{stderr}");
    assert!(
        said.contains("nothing was handed to Carrick Cloud for inventory-svc"),
        "the command names the repo it handed nothing over for:\n{said}"
    );
    assert!(
        said.contains("The index was built here."),
        "and says where the index came from instead:\n{said}"
    );
    assert!(
        root.join(".carrick/index.json").is_file(),
        "and it really did build one"
    );
    assert!(
        !root.join(".carrick/jobs.json").exists(),
        "nothing is waiting to be collected:\n{said}"
    );
}

/// The stored blob a laptop `carrick index` wrote for one repo: the copy of
/// exactly what it uploaded, which is what the cloud's compatibility answer is
/// read from.
fn stored_blob(root: &Path, repo: &str) -> serde_json::Value {
    let dir = root.join(".carrick/repos");
    std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .map(|path| {
            serde_json::from_str::<serde_json::Value>(
                &std::fs::read_to_string(&path).expect("read a stored blob"),
            )
            .expect("a stored blob is JSON")
        })
        .find(|blob| blob["repo_name"] == serde_json::json!(repo))
        .unwrap_or_else(|| panic!("no stored blob for {repo} in {}", dir.display()))
}

/// The fixture's two repos indexed with `carrick index`, the consumer in a
/// folder named `consumer`.
///
/// Sibling repos are scanned in name order, so the name is what decides
/// whether the consumer is scanned before or after `catalog-web`, its
/// producer.
fn laptop_index(consumer: &str) -> (tempfile::TempDir, String) {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    if consumer != "inventory-svc" {
        std::fs::rename(root.join("inventory-svc"), root.join(consumer)).expect("rename");
        std::fs::write(
            root.join("carrick-workspace.json"),
            format!("{{ \"repos\": [\"./catalog-web\", \"./{consumer}\"] }}\n"),
        )
        .expect("rewrite the workspace file");
    }
    for repo in ["catalog-web", consumer] {
        std::fs::write(root.join(repo).join("carrick.json"), "{}\n").expect("write a config");
    }
    let (stdout, stderr) = run_mocked_output(root, &["index", "--workspace", "."]);
    (workspace, format!("{stdout}\n{stderr}"))
}

/// carrick#1490: the consumer's upload carries the type verdict for its pair
/// when its producer was indexed earlier in the same build, the way a CI run
/// of the consumer carries it after downloading the producer's index. Before
/// the fix every laptop scan read no sibling at all, so the consumer stored
/// no verdict whatever the order.
#[test]
#[serial]
fn a_laptop_index_stores_the_consumers_type_verdict() {
    let (workspace, said) = laptop_index("inventory-svc");
    let consumer = stored_blob(workspace.path(), "inventory-svc");
    let verdicts = consumer["compat_verdicts"]
        .as_array()
        .unwrap_or_else(|| panic!("the consumer stored no verdict:\n{said}"));
    assert!(
        verdicts
            .iter()
            .any(|row| row["producer_repo"] == "catalog-web"
                && row["producer_key"]
                    .as_str()
                    .is_some_and(|key| key.contains("/api/v1/widgets/"))),
        "the verdict is for the widget pair: {verdicts:#?}"
    );
    assert!(
        said.contains("type check: 1 of 1 pair(s) judged"),
        "the build says the check ran and what it judged:\n{said}"
    );
    assert!(
        !said.contains("not in Carrick Cloud"),
        "nothing judged here is missing from the upload:\n{said}"
    );
}

/// carrick#1490: a consumer indexed before its producer had nothing to pair
/// with when it uploaded, and the build says so by name rather than leaving
/// the cloud to report the pair as never compared. `carrick status` says it
/// too, because a detached build's output is a log nobody may read.
#[test]
#[serial]
fn a_verdict_the_upload_missed_is_named_by_the_build_and_by_status() {
    let (workspace, said) = laptop_index("basket-svc");
    let root = workspace.path();
    assert!(
        stored_blob(root, "basket-svc")["compat_verdicts"].is_null(),
        "indexed first, the consumer had no producer to pair with:\n{said}"
    );
    let line = "1 verdict(s) for basket-svc are not in Carrick Cloud: it was indexed before \
                catalog-web.";
    assert!(said.contains(line), "the build names the gap:\n{said}");
    assert!(
        line.split_whitespace().count() <= 20,
        "a scanner line carries the count, the thing and what is missing, nothing more"
    );
    let status = run(root, &["status"]);
    assert!(status.contains(line), "status names it too:\n{status}");
    let json: serde_json::Value =
        serde_json::from_str(&run(root, &["status", "--json"])).expect("status --json");
    assert_eq!(
        json["type_check"]["not_stored"][0]["consumer"], "basket-svc",
        "{json:#}"
    );
}

/// `carrick check --changed`, with its exit code and its stdout. The budget is
/// generous for the same reason the re-check tests' is: this binary is built
/// without optimisation on a shared runner.
fn check_changed(workspace: &Path, json: bool) -> (Option<i32>, String) {
    let mut args = vec!["check", "--changed", "--workspace", "."];
    if json {
        args.push("--json");
    }
    let output = Command::new(carrick())
        .args(&args)
        .current_dir(workspace)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", workspace.join(".test-credentials"))
        // No resident type daemon: these tests are about the answer, and a
        // re-check would otherwise start one in the background
        // (`typed_daemon_test.rs` covers it).
        .env("CARRICK_TYPED", "off")
        .env("CARRICK_CHECK_BUDGET_MS", "600000")
        .output()
        .unwrap_or_else(|e| panic!("carrick {args:?}: {e}"));
    (
        output.status.code(),
        String::from_utf8(output.stdout).expect("stdout was not UTF-8"),
    )
}

/// The same with `--all`: the problems that do not fail the run are listed
/// too, each with why.
fn check_changed_all(workspace: &Path) -> (Option<i32>, String) {
    let output = Command::new(carrick())
        .args(["check", "--changed", "--all", "--workspace", "."])
        .current_dir(workspace)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", workspace.join(".test-credentials"))
        // No resident type daemon: these tests are about the answer, and a
        // re-check would otherwise start one in the background
        // (`typed_daemon_test.rs` covers it).
        .env("CARRICK_TYPED", "off")
        .env("CARRICK_CHECK_BUDGET_MS", "600000")
        .output()
        .unwrap_or_else(|e| panic!("carrick check --changed --all: {e}"));
    (
        output.status.code(),
        String::from_utf8(output.stdout).expect("stdout was not UTF-8"),
    )
}

/// `carrick check <file>...`, with its exit code and its stdout.
fn check_files(workspace: &Path, files: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(carrick())
        .arg("check")
        .args(files)
        .args(["--workspace", "."])
        .current_dir(workspace)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", workspace.join(".test-credentials"))
        // No resident type daemon: these tests are about the answer, and a
        // re-check would otherwise start one in the background
        // (`typed_daemon_test.rs` covers it).
        .env("CARRICK_TYPED", "off")
        .env("CARRICK_CHECK_BUDGET_MS", "600000")
        .output()
        .unwrap_or_else(|e| panic!("carrick check {files:?}: {e}"));
    (
        output.status.code(),
        String::from_utf8(output.stdout).expect("stdout was not UTF-8"),
    )
}

fn commit_all(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(
        repo,
        &[
            "-c",
            "user.email=fixture@carrick.test",
            "-c",
            "user.name=fixture",
            "commit",
            "-qm",
            message,
        ],
    );
}

/// Where a test's new call sends its request: a bare path, which goes to the
/// origin the client already holds, or a base set at run time.
const RELATIVE: &str = "";
const INJECTED: &str = "${this.baseUrl}";

/// The consumer gains a member that calls a route no service serves, and a
/// caller that makes it a member the repo uses.
fn add_a_call_to_no_route(root: &Path, base: &str) {
    let client = root.join("inventory-svc/src/client.ts");
    edit(
        &client,
        "  readWidget(widgetId: string)",
        &format!(
            "  readGadget(gadgetId: string): Promise<Widget> {{\n    return send<Widget>(`{base}/api/v1/gadgets/${{gadgetId}}`, {{\n      method: \"GET\",\n    }});\n  }}\n\n  readWidget(widgetId: string)"
        ),
    );
    let caller = root.join("inventory-svc/src/inventory.ts");
    let mut text = std::fs::read_to_string(&caller).expect("read the caller");
    text.push_str(
        "\nexport async function gadgetName(\n  gadgetId: string,\n  client: CatalogClient\n): Promise<string> {\n  const gadget = await client.readGadget(gadgetId);\n  return gadget.name;\n}\n",
    );
    std::fs::write(&caller, text).expect("write the caller");
}

/// A handler committed on a branch stops sending a field the consumer reads.
/// The consumer is untouched, so only a pass that re-extracts the producer and
/// re-judges it against the index can see it, and it is a fact on both sides.
#[test]
#[serial]
fn check_changed_fails_when_a_changed_handler_drops_a_field_a_consumer_reads() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    let producer = root.join("catalog-web");
    let route = producer.join("app/routes/api.v1.widgets.$widgetId.ts");
    git(&producer, &["checkout", "-qb", "feature"]);
    edit(&route, "  activeCount: number;\n", "");
    edit(&route, ", activeCount: 3", "");
    commit_all(&producer, "drop activeCount");

    let (code, text) = check_changed(root, false);
    let summary = text.lines().next().unwrap_or_default();
    assert!(
        summary.starts_with("carrick check: checked 1 route and 0 calls in 1 changed file"),
        "one summary line, about the one file the branch changed:\n{text}"
    );
    let problem = text
        .lines()
        .find(|line| {
            line.starts_with("catalog-web/app/routes/api.v1.widgets.$widgetId.ts:")
                && line.contains("  incompatible  ")
        })
        .unwrap_or_else(|| panic!("the route is named at its line:\n{text}"));
    assert!(
        problem.contains("with inventory-svc src/client.ts:") && problem.contains("activeCount"),
        "with the other side and the reason:\n{text}"
    );
    assert_eq!(code, Some(1), "a fact fails the run:\n{text}");

    let (_, body) = check_changed(root, true);
    let json: serde_json::Value = serde_json::from_str(&body).expect("--json is JSON");
    assert_eq!(json["schema"], "carrick.check-changed/0");
    assert_eq!(json["files"], 1, "{json:#}");
    assert_eq!(json["problems"][0]["kind"], "incompatible", "{json:#}");
    assert_eq!(json["problems"][0]["fails"], true, "{json:#}");
}

/// A new call, uncommitted, to a route no service in the workspace serves.
/// Its path is bare, so it goes to the origin the client already talks to.
#[test]
#[serial]
fn check_changed_fails_on_a_new_call_to_a_route_that_does_not_exist() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    add_a_call_to_no_route(root, RELATIVE);

    let (code, text) = check_changed(root, false);
    let problem = text
        .lines()
        .find(|line| {
            line.starts_with("inventory-svc/src/client.ts:") && line.contains("  no route  ")
        })
        .unwrap_or_else(|| panic!("the new call is named at its line:\n{text}"));
    assert!(
        problem.contains("GET /api/v1/gadgets/:gadgetId"),
        "with the operation it calls:\n{text}"
    );
    assert!(
        !text.contains("api.v1.widgets"),
        "the untouched producer is not reported:\n{text}"
    );
    assert_eq!(code, Some(1), "a fact fails the run:\n{text}");
}

/// A `check --changed` stopped by SIGTERM takes its re-scan and the re-scan's
/// type sidecar with it, even a sidecar too busy to hear a signal. Before,
/// only the check ended: the re-scan was a process of its own that nothing
/// told, and it ran on with its sidecar.
#[cfg(unix)]
#[test]
#[serial]
fn a_signalled_check_ends_its_re_scan_and_the_re_scans_sidecar() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    add_a_call_to_no_route(root, RELATIVE);

    // Busy as the real one is while it builds a program: its handlers never
    // run, because its event loop never turns.
    let sidecar_dir = root.join(".busy-sidecar/dist/src");
    std::fs::create_dir_all(&sidecar_dir).expect("a directory for the sidecar");
    let pid_file = root.join(".busy-sidecar.pid");
    std::fs::write(
        sidecar_dir.join("index.js"),
        format!(
            "require('fs').writeFileSync({:?}, String(process.pid));\n\
             process.on('SIGTERM', () => process.exit(0));\n\
             process.stdin.on('end', () => process.exit(0));\n\
             for (;;) {{}}\n",
            pid_file.display().to_string()
        ),
    )
    .expect("write the busy sidecar");

    let mut check = Command::new(carrick())
        .args(["check", "--changed", "--workspace", "."])
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .env("CARRICK_CHECK_BUDGET_MS", "600000")
        .env("CARRICK_SIDECAR_DIR", root.join(".busy-sidecar"))
        .env("CARRICK_SIDECAR_READY_TIMEOUT_SECS", "600")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start the check");

    let alive = |pid: libc::pid_t| unsafe { libc::kill(pid, 0) == 0 };
    let started = Instant::now();
    let sidecar = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<libc::pid_t>().ok())
        {
            break pid;
        }
        if started.elapsed() > Duration::from_secs(120) {
            let _ = check.kill();
            panic!("the check's re-scan started no sidecar in 120s");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let listed = Command::new("pgrep")
        .args(["-P", &check.id().to_string(), "-x", "carrick"])
        .output()
        .expect("pgrep lists a process's children");
    let rescan: libc::pid_t = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .find_map(|line| line.trim().parse().ok())
        .expect("the check is waiting on its re-scan");

    // SAFETY: a signal to a child this test spawned and has not reaped.
    assert_eq!(
        unsafe { libc::kill(check.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let ended = Instant::now();
    let status = loop {
        if let Some(status) = check.try_wait().expect("poll the check") {
            break status;
        }
        assert!(
            ended.elapsed() < Duration::from_secs(60),
            "the signalled check was still running"
        );
        std::thread::sleep(Duration::from_millis(20));
    };

    for (pid, what) in [(rescan, "the re-scan"), (sidecar, "the busy sidecar")] {
        let start = Instant::now();
        while alive(pid) {
            if start.elapsed() > Duration::from_secs(15) {
                // SAFETY: descendants this test started, left running by the
                // failure it is about to report.
                unsafe {
                    libc::kill(rescan, libc::SIGKILL);
                    libc::kill(sidecar, libc::SIGKILL);
                }
                panic!("{what} was still running 15s after the check ended ({status:?})");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    // It ended by passing the signal on, and says which signal it was.
    assert_eq!(status.code(), Some(143), "{status:?}");
}

/// How long the silence-watching caller below lets a command say nothing
/// before killing it, as an agent harness does with a command it reads as
/// hung. Longer than the check's heartbeat, shorter than the stall the stub
/// sidecar makes.
const SILENCE_LIMIT: Duration = Duration::from_secs(20);

/// A check that works for longer than a caller will wait in silence keeps
/// saying so on stderr, and its answer on stdout stays the answer alone.
///
/// The type sidecar here never becomes ready, so the re-scan and the join
/// each wait out the readiness budget: a stretch with no output that is
/// longer than [`SILENCE_LIMIT`], as a large repo's re-scan is. The caller is
/// what an agent harness does: it kills the command after that long without a
/// byte on either stream. Before the narration, the check printed nothing
/// until its answer, and this caller killed it.
#[test]
#[serial]
fn check_changed_keeps_talking_to_a_caller_that_kills_silent_commands() {
    use std::io::Read;
    use std::process::Stdio;
    use std::sync::mpsc;

    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    add_a_call_to_no_route(root, RELATIVE);

    let sidecar = root.join(".stalled-sidecar/dist/src");
    std::fs::create_dir_all(&sidecar).expect("a directory for the sidecar");
    std::fs::write(
        sidecar.join("index.js"),
        "process.stdin.on('end', () => process.exit(0));\nprocess.stdin.resume();\n",
    )
    .expect("write the sidecar that never answers");

    let mut child = Command::new(carrick())
        .args(["check", "--changed", "--workspace", ".", "--json"])
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .env("CARRICK_CHECK_BUDGET_MS", "600000")
        .env("CARRICK_SIDECAR_DIR", root.join(".stalled-sidecar"))
        .env("CARRICK_SIDECAR_READY_TIMEOUT_SECS", "30")
        // The stalled sidecar types nothing; the run is about the wait.
        .env("CARRICK_ALLOW_MISSING_TYPES", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the check");

    // Each stream read on its own thread; every chunk is a sign of life.
    let (heard, hearing) = mpsc::channel::<(bool, Vec<u8>)>();
    let readers: Vec<_> = [
        (
            true,
            Box::new(child.stdout.take().expect("stdout")) as Box<dyn Read + Send>,
        ),
        (false, Box::new(child.stderr.take().expect("stderr"))),
    ]
    .into_iter()
    .map(|(is_stdout, mut stream)| {
        let heard = heard.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while let Ok(read) = stream.read(&mut buffer) {
                if read == 0 || heard.send((is_stdout, buffer[..read].to_vec())).is_err() {
                    break;
                }
            }
        })
    })
    .collect();
    drop(heard);

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let started = Instant::now();
    let mut longest_silence = Duration::ZERO;
    let mut last = Instant::now();
    loop {
        match hearing.recv_timeout(Duration::from_millis(100)) {
            Ok((is_stdout, bytes)) => {
                longest_silence = longest_silence.max(last.elapsed());
                last = Instant::now();
                if is_stdout {
                    stdout.extend(bytes);
                } else {
                    stderr.extend(bytes);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if last.elapsed() >= SILENCE_LIMIT {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "killed as hung: {:.0}s without output, {:.0}s in; it had said:\n{}",
                        last.elapsed().as_secs_f64(),
                        started.elapsed().as_secs_f64(),
                        String::from_utf8_lossy(&stderr)
                    );
                }
            }
        }
    }
    for reader in readers {
        let _ = reader.join();
    }
    let status = child.wait().expect("wait for the check");
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    let stdout = String::from_utf8(stdout).expect("stdout is UTF-8");

    eprintln!("what the check said while it worked:\n{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is the JSON answer alone ({e}):\n{stdout}"));
    assert_eq!(json["schema"], "carrick.check-changed/0", "{json:#}");
    assert!(
        status.code().is_some(),
        "the check ended on its own: {status:?}"
    );
    assert!(
        stderr.contains("carrick check: re-reading inventory-svc..."),
        "it named the repo it re-read:\n{stderr}"
    );
    assert!(
        stderr.contains("carrick check: still "),
        "and said it was still working through the stall:\n{stderr}"
    );
    assert!(
        stderr.contains("carrick check: comparing routes and calls across 2 services..."),
        "and named the join:\n{stderr}\n{stdout}"
    );
    assert!(
        longest_silence < SILENCE_LIMIT,
        "never silent for the limit: {longest_silence:?}"
    );
}

/// The same new call through a base set at run time: it could be a service
/// of this workspace or an outside API, so it is printed as unconfirmed and
/// does not fail the run.
#[test]
#[serial]
fn check_changed_prints_a_new_call_through_a_runtime_base_as_unconfirmed() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    add_a_call_to_no_route(root, INJECTED);

    // The member and its caller: two call sites through the runtime base.
    let (code, quiet) = check_changed(root, false);
    assert!(
        quiet.contains(": 0 incompatible, 0 reach no route, plus 2 unconfirmed, run with --all"),
        "counted, not listed:\n{quiet}"
    );
    assert_eq!(code, Some(0), "{quiet}");
    let (code, text) = check_changed_all(root);
    let problem = text
        .lines()
        .find(|line| {
            line.starts_with("inventory-svc/src/client.ts:") && line.contains("  no route  ")
        })
        .unwrap_or_else(|| panic!("the new call is still printed:\n{text}"));
    assert!(
        problem.contains("[unconfirmed: the base is set at run time, does not fail]"),
        "as unconfirmed:\n{text}"
    );
    assert_eq!(code, Some(0), "{text}");
}

/// A call the index already held with no route, in a file the work touched,
/// is printed and was there before the work: it does not fail the run.
#[test]
#[serial]
fn check_changed_does_not_fail_on_a_problem_the_index_already_held() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    add_a_call_to_no_route(root, RELATIVE);
    let consumer = root.join("inventory-svc");
    commit_all(&consumer, "a call to no route, on main");
    index(root);
    git(&consumer, &["checkout", "-qb", "feature"]);
    edit(
        &consumer.join("src/client.ts"),
        "import { send }",
        "// The catalog's client.\nimport { send }",
    );

    let (code, quiet) = check_changed(root, false);
    assert!(
        quiet.contains("plus 1 already in the index, run with --all to list them"),
        "counted, not listed:\n{quiet}"
    );
    assert_eq!(code, Some(0), "{quiet}");
    let (code, text) = check_changed_all(root);
    let problem = text
        .lines()
        .find(|line| {
            line.starts_with("inventory-svc/src/client.ts:") && line.contains("  no route  ")
        })
        .unwrap_or_else(|| panic!("the old call is printed:\n{text}"));
    assert!(
        problem.contains("[already in the index, does not fail]"),
        "as one the index held:\n{text}"
    );
    assert_eq!(code, Some(0), "{text}");
}

/// A problem that was already on main, in a file the work did not touch, is
/// not this work's: only the changed file is reported and counted.
#[test]
#[serial]
fn check_changed_does_not_re_check_a_file_the_work_did_not_touch() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    // The unmatched call is committed on main before the index is built.
    add_a_call_to_no_route(root, RELATIVE);
    commit_all(&root.join("inventory-svc"), "a call to no route, on main");
    index(root);

    let producer = root.join("catalog-web");
    git(&producer, &["checkout", "-qb", "feature"]);
    edit(
        &producer.join("app/routes/api.v1.health.ts"),
        "// A second route",
        "// Health.\n// A second route",
    );

    let (code, text) = check_changed(root, false);
    assert!(
        text.starts_with("carrick check: checked 1 route and 0 calls in 1 changed file"),
        "only the changed file is counted:\n{text}"
    );
    assert!(
        text.contains(": 0 incompatible, 0 reach no route"),
        "and nothing outside it is reported:\n{text}"
    );
    assert_eq!(code, Some(0), "{text}");
}

/// The one exception: a call in an untouched file that the index matched, and
/// that the work left with no route by deleting the route it called.
#[test]
#[serial]
fn check_changed_names_the_untouched_calls_a_deleted_route_leaves_with_no_route() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    std::fs::remove_file(root.join("catalog-web/app/routes/api.v1.widgets.$widgetId.ts"))
        .expect("delete the route");

    let (code, text) = check_changed(root, false);
    assert!(
        text.lines()
            .any(|line| line.starts_with("inventory-svc/src/client.ts:")
                && line.contains("  no route  ")
                && line.contains("the index matched this call")),
        "the consumer the deletion broke is named:\n{text}"
    );
    assert_eq!(code, Some(1), "{text}");
}

/// Marks the workspace's index as written by an older scanner, as an upgrade
/// leaves it: same rows, another version.
fn as_written_by_an_older_scanner(root: &Path) {
    let path = root.join(".carrick/index.json");
    let mut index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read the index"))
            .expect("the index is JSON");
    index["scanner_version"] = serde_json::json!("0.0.1");
    std::fs::write(&path, index.to_string()).expect("write the index");
}

/// carrick#2911: an index an older scanner wrote holds the rows that scanner
/// read. A row this build reads and that one did not looks new, so a check
/// against it cannot tell a new problem from an upgrade: it does not fail,
/// says why in one line, and exits 2.
#[test]
#[serial]
fn check_changed_does_not_fail_on_a_row_an_older_scanners_index_cannot_date() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    as_written_by_an_older_scanner(root);
    add_a_call_to_no_route(root, RELATIVE);

    let (code, text) = check_changed(root, false);
    let problem = text
        .lines()
        .find(|line| {
            line.starts_with("inventory-svc/src/client.ts:") && line.contains("  no route  ")
        })
        .unwrap_or_else(|| panic!("the call is listed, not hidden:\n{text}"));
    assert!(
        problem.contains("[not checked: the index is from another scanner build]"),
        "marked as not checked:\n{text}"
    );
    assert!(
        text.contains("written by carrick 0.0.1")
            && text.contains("Run `carrick refresh` on the main branch"),
        "one line says which build wrote the index and what to run:\n{text}"
    );
    assert_eq!(code, Some(2), "not a pass and not a failure:\n{text}");

    let (_, body) = check_changed(root, true);
    let json: serde_json::Value = serde_json::from_str(&body).expect("--json is JSON");
    assert_eq!(json["failing"], 0, "{json:#}");
    assert_eq!(json["index_scanner"], "0.0.1", "{json:#}");
    assert!(
        json["problems"].as_array().is_some_and(|problems| problems
            .iter()
            .any(|p| p["other_build"] == true && p["fails"] == false)),
        "{json:#}"
    );
}

/// carrick#2911: what the older index did read still decides. It matched this
/// call to a route, and the work deleted the route: that fails whichever
/// build wrote the index.
#[test]
#[serial]
fn check_changed_still_fails_a_call_an_older_scanners_index_matched() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    as_written_by_an_older_scanner(root);
    std::fs::remove_file(root.join("catalog-web/app/routes/api.v1.widgets.$widgetId.ts"))
        .expect("delete the route");

    let (code, text) = check_changed(root, false);
    assert!(
        text.lines()
            .any(|line| line.starts_with("inventory-svc/src/client.ts:")
                && line.contains("  no route  ")
                && line.contains("the index matched this call")
                && !line.contains("not checked")),
        "the consumer the deletion broke still fails:\n{text}"
    );
    assert_eq!(code, Some(1), "{text}");
}

/// The two fixture repos as two services of one monorepo, committed and
/// listed as the workspace's one repo.
fn monorepo_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let source = repo_root().join("tests/fixtures/local-mode-workspace");
    let mono = dir.path().join("mono");
    for service in ["catalog-web", "inventory-svc"] {
        copy_tree(&source.join(service), &mono.join(service));
    }
    std::fs::write(
        mono.join("carrick.json"),
        r#"{ "services": [
  { "name": "catalog-web", "directory": "catalog-web" },
  { "name": "inventory-svc", "directory": "inventory-svc" }
] }
"#,
    )
    .expect("write carrick.json");
    git(&mono, &["init", "-q", "."]);
    commit_all(&mono, "fixture");
    std::fs::write(
        dir.path().join("carrick-workspace.json"),
        "{ \"repos\": [\"./mono\"] }\n",
    )
    .expect("write the workspace file");
    dir
}

/// carrick#2826: only the services that own a changed file are re-extracted;
/// every other service of the same repo stands in from the index, and is
/// still judged against. Which service is carried is the change scope's
/// decision, unit-tested in `change_scope` (a run that asks no model carries
/// a service whose stored blob owes only model work); this proves the carried
/// consumer still reaches the join and fails the run as a fact.
#[test]
#[serial]
fn check_changed_re_extracts_only_the_services_that_own_a_changed_file() {
    let workspace = monorepo_workspace();
    let root = workspace.path();
    index(root);
    let route = root.join("mono/catalog-web/app/routes/api.v1.widgets.$widgetId.ts");
    edit(&route, "  activeCount: number;\n", "");
    edit(&route, ", activeCount: 3", "");

    let (code, body) = check_changed(root, true);
    let json: serde_json::Value = serde_json::from_str(&body).expect("--json is JSON");
    assert!(
        json["problems"]
            .as_array()
            .is_some_and(|problems| problems.iter().any(|p| {
                p["kind"] == "incompatible"
                    && p["other"].as_array().is_some_and(|other| {
                        other
                            .iter()
                            .any(|o| o == "inventory-svc inventory-svc/src/client.ts:14")
                    })
                    && p["fails"] == true
            })),
        "the carried consumer is still judged against, as a fact:\n{json:#}"
    );
    assert_eq!(code, Some(1), "{json:#}");
}

/// carrick#2827: several named files are one pass across repos, with the
/// summary line and the exit code `--changed` gives. The handler edit is
/// uncommitted and on no branch: the files are named, not found by git.
#[test]
#[serial]
fn check_several_files_judges_them_in_one_pass_and_fails_on_a_fact() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    let route = root.join("catalog-web/app/routes/api.v1.widgets.$widgetId.ts");
    edit(&route, "  activeCount: number;\n", "");
    edit(&route, ", activeCount: 3", "");
    commit_all(&root.join("catalog-web"), "drop activeCount");
    add_a_call_to_no_route(root, RELATIVE);

    let (code, text) = check_files(
        root,
        &[
            "catalog-web/app/routes/api.v1.widgets.$widgetId.ts",
            "inventory-svc/src/client.ts",
            "/not/in/the/workspace/x.ts",
        ],
    );
    let summary = text.lines().next().unwrap_or_default();
    assert!(
        summary.starts_with("carrick check: checked 1 route and ")
            && summary.contains("in 2 changed files"),
        "one summary line for the two files found:\n{text}"
    );
    assert!(
        text.lines()
            .any(|line| line.contains("  incompatible  route GET")),
        "the handler's break is named:\n{text}"
    );
    assert!(
        text.lines()
            .any(|line| line.starts_with("inventory-svc/src/client.ts:")
                && line.contains("  no route  ")),
        "and the consumer's new call:\n{text}"
    );
    assert!(
        text.contains("not checked: /not/in/the/workspace/x.ts: not in a repo of this workspace"),
        "a file outside the workspace is named, not fatal:\n{text}"
    );
    assert_eq!(code, Some(1), "a fact fails the run:\n{text}");
}

/// Naming only files the index cannot check is not a pass: it exits 2, a code
/// no verdict uses, and says which files and what fixes it (carrick#2850).
#[test]
#[serial]
fn check_named_files_exits_two_when_none_was_covered() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    let (code, text) = check_files(
        root,
        &["/not/in/the/workspace/x.ts", "/not/in/the/workspace/y.ts"],
    );
    assert!(
        text.contains("not checked: covered none of the 2 files asked about"),
        "{text}"
    );
    assert!(
        text.contains("not checked: /not/in/the/workspace/x.ts: not in a repo of this workspace")
            && text.contains("carrick refresh"),
        "the file, the reason and the fix are named:\n{text}"
    );
    assert_eq!(code, Some(2), "nothing covered is not a pass:\n{text}");
}

/// Some files covered and some not: the covered ones decide the exit code and
/// the skipped one is listed.
#[test]
#[serial]
fn check_a_mix_of_covered_and_uncovered_files_exits_by_the_covered_ones() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    let (code, text) = check_files(
        root,
        &[
            "inventory-svc/src/inventory.ts",
            "/not/in/the/workspace/x.ts",
        ],
    );
    assert!(
        text.contains("not checked: /not/in/the/workspace/x.ts"),
        "the skipped file is listed:\n{text}"
    );
    assert_eq!(
        code,
        Some(0),
        "one file was covered and holds no problem:\n{text}"
    );
}

/// An index whose paths belong to another folder covers nothing of the files
/// changed here. That is the pilot copy that read as a pass.
#[test]
#[serial]
fn check_changed_exits_two_when_the_index_is_not_bound_to_this_folder() {
    let original = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    index(original.path());
    let copy = tempfile::tempdir().expect("temp dir");
    copy_tree(original.path(), copy.path());
    drop(original);
    let root = copy.path();
    // The folders carry other names than the ones the index was built under.
    std::fs::rename(root.join("catalog-web"), root.join("web-copy")).expect("rename");
    std::fs::rename(root.join("inventory-svc"), root.join("svc-copy")).expect("rename");
    std::fs::write(
        root.join("carrick-workspace.json"),
        r#"{"repos": ["./web-copy", "./svc-copy"]}"#,
    )
    .expect("workspace file");
    edit(
        &root.join("svc-copy/src/inventory.ts"),
        "export",
        "// edited\nexport",
    );
    let (code, text) = check_changed(root, false);
    assert!(text.contains("not checked"), "{text}");
    assert_eq!(code, Some(2), "nothing was covered:\n{text}");
}

/// Naming only files that hold no problem exits 0.
#[test]
#[serial]
fn check_several_files_exits_zero_when_none_is_a_problem() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    let (code, text) = check_files(
        root,
        &[
            "catalog-web/app/routes/api.v1.health.ts",
            "inventory-svc/src/inventory.ts",
        ],
    );
    assert!(
        text.contains(": 0 incompatible, 0 reach no route"),
        "{text}"
    );
    assert_eq!(code, Some(0), "{text}");
}

const COMMIT: [&str; 6] = [
    "-c",
    "user.email=fixture@carrick.test",
    "-c",
    "user.name=fixture",
    "commit",
    "-qm",
];

/// No base to diff against used to print an empty "not checked:" and exit 0.
/// Unrelated history: the branch shares no commit with the main branch.
#[test]
#[serial]
fn check_changed_with_no_merge_base_exits_two_and_names_the_fix() {
    let workspace = workspace("local-mode-workspace", &["catalog-web"]);
    let root = workspace.path();
    index(root);
    let repo = root.join("catalog-web");
    git(&repo, &["branch", "-M", "main"]);
    git(&repo, &["checkout", "-q", "--orphan", "work"]);
    git(&repo, &["add", "-A"]);
    let mut args = COMMIT.to_vec();
    args.push("unrelated");
    git(&repo, &args);

    let (code, text) = check_changed(root, false);
    assert_eq!(code, Some(2), "{text}");
    assert!(
        text.contains("no merge base with main") && text.contains("run `git fetch origin main`"),
        "{text}"
    );
    assert!(!text.contains("not checked: \n"), "no bare reason:\n{text}");
}

/// A shallow clone whose history stops short of the main branch is told to
/// unshallow it.
#[test]
#[serial]
fn check_changed_in_a_shallow_clone_says_to_unshallow() {
    let workspace = workspace("local-mode-workspace", &["catalog-web"]);
    let root = workspace.path();
    let source = root.join("origin-repo");
    std::fs::rename(root.join("catalog-web"), &source).expect("move the repo");
    git(&source, &["branch", "-M", "main"]);
    git(&source, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(source.join("a.txt"), "a\n").expect("write");
    git(&source, &["add", "-A"]);
    let mut args = COMMIT.to_vec();
    args.push("feature work");
    git(&source, &args);
    git(&source, &["checkout", "-q", "main"]);
    std::fs::write(source.join("b.txt"), "b\n").expect("write");
    git(&source, &["add", "-A"]);
    let mut args = COMMIT.to_vec();
    args.push("main moved");
    git(&source, &args);

    let url = format!("file://{}", source.display());
    git(
        root,
        &[
            "clone",
            "-q",
            "--depth=1",
            "--branch",
            "feature",
            &url,
            "catalog-web",
        ],
    );
    let clone = root.join("catalog-web");
    git(
        &clone,
        &[
            "fetch",
            "-q",
            "--depth=1",
            "origin",
            "main:refs/remotes/origin/main",
        ],
    );
    git(&clone, &["remote", "set-head", "origin", "-d"]);
    index(root);

    let (code, text) = check_changed(root, false);
    assert_eq!(code, Some(2), "{text}");
    assert!(
        text.contains("this clone is shallow") && text.contains("run `git fetch --unshallow`"),
        "{text}"
    );
}

// ---------------------------------------------------------------------------
// carrick#2894: an edited file keeps the model's answer for the lines the edit
// left alone. The producer registers its routes on a typed receiver, which no
// deterministic reader states, so the rows below exist only as the model's
// answer (the cassettes in the fixture), replayed from the index.
// ---------------------------------------------------------------------------

/// The fixture's cassettes: the model's answers, keyed by file stem.
fn carry_cassettes() -> String {
    format!(
        "{}/",
        repo_root()
            .join("tests/fixtures/edited-handler-carry/cassettes")
            .display()
    )
}

/// The producer's route module, as the cassette's line numbers see it.
const CARRY_ROUTES: &str = "items-api/src/routes.ts";

/// The fixture's two repos, indexed with the model mocked to the fixture's
/// cassettes, so the index holds the model's answers a check later replays.
fn carry_workspace() -> tempfile::TempDir {
    let workspace = workspace("edited-handler-carry", &["items-api", "items-web"]);
    let root = workspace.path();
    for repo in ["items-api", "items-web"] {
        std::fs::write(root.join(repo).join("carrick.json"), "{}\n").expect("write a config");
    }
    let output = Command::new(carrick())
        .args(["index", "--workspace", "."])
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_MOCK_FIXTURE_DIR", carry_cassettes())
        .output()
        .expect("spawn carrick index");
    assert!(
        output.status.success(),
        "index failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    workspace
}

/// A command with its exit code, since a failing check is the point here.
fn carrick_code(workspace: &Path, args: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(carrick())
        .args(args)
        .current_dir(workspace)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", workspace.join(".test-credentials"))
        .env("CARRICK_CHECK_BUDGET_MS", "600000")
        .env("CARRICK_RECHECK_BUDGET_MS", "600000")
        .output()
        .unwrap_or_else(|e| panic!("carrick {args:?}: {e}"));
    (
        output.status.code(),
        String::from_utf8(output.stdout).expect("stdout was not UTF-8"),
    )
}

/// `check --changed --json`, against a base that is the commit indexed.
fn carry_check_changed(workspace: &Path) -> (Option<i32>, serde_json::Value) {
    for repo in ["items-api", "items-web"] {
        git(
            &workspace.join(repo),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
    }
    let (code, text) = carrick_code(
        workspace,
        &["check", "--changed", "--workspace", ".", "--json"],
    );
    let json = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("check --changed --json was not JSON: {e}\n{text}"));
    (code, json)
}

/// The producer's route module with two lines above the plugin and the first
/// route's field renamed: the edit every test below starts from.
fn rename_connected(root: &Path) {
    let file = root.join(CARRY_ROUTES);
    edit(
        &file,
        "export async function itemRoutes",
        "// A note.\n// Another.\nexport async function itemRoutes",
    );
    edit(
        &file,
        "return { id, connected: true };",
        "return { id, isConnected: true };",
    );
}

/// The no-model re-scan the re-check and `refresh` run for the producer, with
/// the stored index as its previous generation, and the blob it writes.
fn no_model_rescan(root: &Path, changed_only: bool) -> serde_json::Value {
    let out = root.join("rescan-out");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).expect("create the scan dir");
    let previous = root.join("rescan-previous.json");
    std::fs::write(
        &previous,
        serde_json::to_vec(&vec![stored_blob(root, "items-api")]).expect("serialise"),
    )
    .expect("write the previous generation");
    let output = Command::new(carrick())
        .arg(root.join("items-api"))
        .args(changed_only.then_some("--changed-only"))
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        // As the indexer launches a scan: a CI runner's variables would make
        // this one read as a CI scan and ignore its previous generation.
        .env_remove("GITHUB_REPOSITORY")
        .env_remove("GITHUB_REF")
        .env_remove("GITHUB_EVENT_NAME")
        .env_remove("GITHUB_SHA")
        .env_remove("GITHUB_RUN_ID")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("GITHUB_WORKSPACE")
        .env_remove("GITHUB_EVENT_PATH")
        .env_remove("CI")
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_URL")
        .env_remove("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
        .env("CARRICK_NO_MODEL", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_SKIP_SIGNATURES", "1")
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_LOCAL_STORAGE_DIR", &out)
        .env("CARRICK_LOCAL_HOSTED_PREVIOUS", &previous)
        .output()
        .expect("spawn the re-scan");
    assert!(
        output.status.success(),
        "the re-scan failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The blob is named for the service, which differs by checkout; the
    // scan wrote exactly one.
    let mut written: Vec<PathBuf> = std::fs::read_dir(&out)
        .expect("read the scan dir")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(written.len(), 1, "one blob expected in {written:?}");
    let text = std::fs::read_to_string(written.remove(0)).expect("read the blob");
    serde_json::from_str(&text).expect("the re-scan blob is JSON")
}

/// The paths of the producer's endpoint rows in a blob, as `path@line`.
fn carried_routes(blob: &serde_json::Value) -> Vec<String> {
    blob["endpoints"]
        .as_array()
        .expect("endpoints")
        .iter()
        .map(|row| {
            format!(
                "{}@{}",
                row["key"]["path"].as_str().unwrap_or_default(),
                row["file_path"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

fn not_carried(blob: &serde_json::Value) -> Vec<String> {
    blob["boundary"]["model_rows_not_carried"]["reasons"]
        .as_array()
        .map(|reasons| {
            reasons
                .iter()
                .filter_map(|r| r.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A producer registers its routes on a typed receiver, so only the model's
/// answer states them. Editing the file used to drop all three from the one scan
/// meant to judge the edit; now the lines the edit left alone keep their rows,
/// and a field renamed inside a handler fails the check.
#[test]
#[serial]
fn an_edited_handler_keeps_its_route_and_is_re_judged() {
    let workspace = carry_workspace();
    let root = workspace.path();

    let before = check_json(root, CARRY_ROUTES);
    let results: Vec<&str> = before["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["verdict"]["result"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        results,
        vec!["compatible"; 3],
        "the fixture starts compatible, which is the known positive:\n{before:#}"
    );

    rename_connected(root);

    let (code, text) = carrick_code(
        root,
        &[
            "check",
            CARRY_ROUTES,
            "--workspace",
            ".",
            "--recheck",
            "--json",
        ],
    );
    assert_eq!(code, Some(0), "{text}");
    let rechecked: serde_json::Value = serde_json::from_str(&text).expect("recheck is JSON");
    let first = &rechecked["items"][0];
    assert_eq!(
        first["line"], 6,
        "the route moved with the two inserted lines:\n{rechecked:#}"
    );
    assert_eq!(first["verdict"]["result"], "type_mismatch", "{rechecked:#}");
    assert!(
        first["actual_type"]
            .as_str()
            .is_some_and(|t| t.contains("isConnected")),
        "with the producer's fresh type:\n{rechecked:#}"
    );

    let (code, changed) = carry_check_changed(root);
    assert_eq!(code, Some(1), "a fact fails the run:\n{changed:#}");
    let problem = &changed["problems"][0];
    assert_eq!(problem["kind"], "incompatible", "{changed:#}");
    assert_eq!(problem["file"], CARRY_ROUTES, "{changed:#}");
    assert_eq!(problem["line"], 6, "{changed:#}");
    assert_eq!(problem["source"], "fact", "{changed:#}");
    assert_eq!(problem["fails"], true, "{changed:#}");

    let blob = no_model_rescan(root, false);
    assert_eq!(
        blob["boundary"]["model_rows_carried"], 3,
        "{:?}",
        blob["boundary"]
    );
    assert!(
        blob["boundary"].get("model_rows_not_carried").is_none(),
        "every row was carried and typed: {:?}",
        blob["boundary"]
    );
}

/// A registration whose own line the edit replaced is not carried: nothing
/// proves it still states the route the answer named. It is counted, with the
/// line, and its neighbours are still carried.
#[test]
#[serial]
fn a_changed_registration_line_is_not_carried_and_is_counted() {
    let workspace = carry_workspace();
    let root = workspace.path();
    edit(
        &root.join(CARRY_ROUTES),
        "'/items/:id/status'",
        "'/items/:id/state'",
    );

    let blob = no_model_rescan(root, false);
    assert_eq!(
        blob["boundary"]["model_rows_carried"], 2,
        "{:?}",
        blob["boundary"]
    );
    assert_eq!(
        not_carried(&blob),
        vec!["src/routes.ts:9 registration_line_changed".to_string()],
        "{:?}",
        blob["boundary"]
    );
    let routes = carried_routes(&blob);
    assert!(
        routes.iter().any(|r| r.starts_with("/api/v1/items/:id@"))
            && routes.iter().any(|r| r.starts_with("/api/v1/items@")),
        "the untouched registrations kept their routes: {routes:?}"
    );
}

/// A handler named at the registration whose body the edit changed is not
/// carried: its response is read where the model said, and that place moved
/// under it. Left out rather than risk a false failure, and counted.
#[test]
#[serial]
fn a_named_handler_whose_body_changed_is_not_carried() {
    let workspace = carry_workspace();
    let root = workspace.path();
    edit(
        &root.join(CARRY_ROUTES),
        "return [{ id: '1', connected: true }];",
        "return [{ id: '1', isConnected: true }];",
    );

    let blob = no_model_rescan(root, false);
    assert_eq!(
        blob["boundary"]["model_rows_carried"], 2,
        "{:?}",
        blob["boundary"]
    );
    assert_eq!(
        not_carried(&blob),
        vec!["src/routes.ts:14 hint_outside_registration_changed".to_string()],
        "{:?}",
        blob["boundary"]
    );
    let (code, changed) = carry_check_changed(root);
    assert_eq!(
        code,
        Some(0),
        "no row was judged against the wrong function:\n{changed:#}"
    );
    assert!(
        changed["problems"]
            .as_array()
            .expect("problems")
            .iter()
            .all(|p| p["fails"] != true),
        "{changed:#}"
    );
}

/// A carried answer is joined and never stored: it is keyed by this tree's
/// candidate ids and mapped from the previous generation's commit, so a stored
/// copy would be mapped again from the wrong base.
#[test]
#[serial]
fn an_edited_file_keeps_no_carried_answer_in_the_blob() {
    let workspace = carry_workspace();
    let root = workspace.path();
    assert!(
        stored_blob(root, "items-api")["file_results"]
            .get("src/routes.ts")
            .is_some(),
        "the model scan stored its answer for the file"
    );
    rename_connected(root);

    let blob = no_model_rescan(root, false);
    assert_eq!(
        blob["boundary"]["model_rows_carried"], 3,
        "{:?}",
        blob["boundary"]
    );
    assert!(
        blob["file_results"].get("src/routes.ts").is_none(),
        "the edited file stores no answer: {:?}",
        blob["file_results"]
    );
    assert!(blob["file_results"].get("src/server.ts").is_some());
}

/// Nothing changes where nothing was edited, and a model scan is not touched.
#[test]
#[serial]
fn carrying_leaves_a_clean_tree_and_a_model_scan_alone() {
    let workspace = carry_workspace();
    let root = workspace.path();
    let indexed = stored_blob(root, "items-api");

    let clean = no_model_rescan(root, false);
    assert!(
        clean["boundary"].get("model_rows_carried").is_none(),
        "{:?}",
        clean["boundary"]
    );
    assert!(clean["boundary"].get("model_rows_not_carried").is_none());
    assert_eq!(clean["endpoints"], indexed["endpoints"]);
    assert_eq!(clean["file_results"], indexed["file_results"]);

    // The same edit, scanned with the model: the cassette answers again and
    // nothing is carried. (A dirty tree stores no answer for the file it
    // changed, with or without this change.)
    edit(
        &root.join(CARRY_ROUTES),
        "return { id, connected: true };",
        "return { id, isConnected: true };",
    );
    let output = Command::new(carrick())
        .args(["index", "--workspace", "."])
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_MOCK_FIXTURE_DIR", carry_cassettes())
        .output()
        .expect("spawn carrick index");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rescanned = stored_blob(root, "items-api");
    assert!(rescanned["boundary"].get("model_rows_carried").is_none());
    assert!(
        rescanned["boundary"]
            .get("model_rows_not_carried")
            .is_none()
    );
    assert_eq!(
        carried_routes(&rescanned).len(),
        3,
        "the model answered for the edited file again"
    );
}
