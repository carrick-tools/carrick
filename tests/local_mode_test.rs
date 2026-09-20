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
        .env("XDG_CONFIG_HOME", workspace.join(".test-credentials"));
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
    assert_eq!(
        removed["items"][0]["counterparts"].as_array().map(Vec::len),
        Some(2),
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
    assert_eq!(
        untouched["recheck"],
        serde_json::Value::Null,
        "an unchanged file is answered from the index, with no re-check at all:\n{untouched:#}"
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

    // Nothing is claimed when no re-check ran: a degraded answer extracted
    // nothing, so it cannot say whether a function is new.
    let degraded: serde_json::Value = serde_json::from_str(&run_with_env(
        root,
        &["check", route, "--workspace", ".", "--recheck", "--json"],
        &[("CARRICK_RECHECK_BUDGET_MS", "1")],
    ))
    .expect("check --recheck --json was not JSON");
    assert_eq!(degraded["recheck"]["ran"], serde_json::json!("none"));
    assert_eq!(
        degraded["recheck"]["new_functions"],
        serde_json::Value::Null,
        "a re-check that did not run names nothing:\n{degraded:#}"
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
