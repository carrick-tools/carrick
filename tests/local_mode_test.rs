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
    workspace_prepared(fixture, repos, |_| {})
}

/// [`workspace`], with `prepare` run on the workspace after the fixture is
/// copied and before each repo is committed, so a test can add an edge the
/// fixture does not hold without editing the fixture.
fn workspace_prepared(fixture: &str, repos: &[&str], prepare: impl Fn(&Path)) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let source = repo_root().join("tests/fixtures").join(fixture);
    for repo in repos {
        copy_tree(&source.join(repo), &dir.path().join(repo));
        prepare(&dir.path().join(repo));
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

/// A mock fixture dir holding the store's answer for `socket.io` and
/// `socket.io-client` as the parity test's cassettes give it for
/// `xrepo-corpus-1`'s two socket repos.
fn socket_claims_cassette() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut claims = Vec::new();
    for repo in ["payments-svc", "web-frontend"] {
        let path = repo_root()
            .join("tests/fixtures/socket-claims-xrepo/corpus-1")
            .join(repo)
            .join("__llm__/library-claims/default.json");
        let answer: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read cassette"))
                .expect("parse cassette");
        claims.extend(answer["library_claims"].as_array().expect("claims").clone());
    }
    let target = dir.path().join("library-claims");
    std::fs::create_dir_all(&target).expect("create cassette dir");
    std::fs::write(
        target.join("default.json"),
        serde_json::to_string(&serde_json::json!({ "library_claims": claims })).unwrap(),
    )
    .expect("write cassette");
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

/// A re-scanning `check` (`--changed`, named files, `--recheck`) and the
/// `refresh` that writes the index it reads (a refresh with no credential
/// leaves the not-signed-in mark on every service it carries) ask the
/// library store, and a machine with no credential is refused and not checked
/// (carrick#3249). These tests are about what a check finds, so they run as a
/// signed-in machine does: the mock store answers, with no claims, which is
/// the answer the store gave before. The one test that wants the refusal
/// builds its own command.
fn as_signed_in(command: &mut Command, args: &[&str]) {
    if matches!(args.first(), Some(&"check" | &"refresh")) {
        command.env("CARRICK_MOCK_ALL", "1");
    }
}

/// Run a carrick command in the workspace and return its stdout. Read-only
/// commands must always exit 0, which is asserted here rather than in every
/// test: a hook that fails an edit because an index is stale is the failure
/// mode this path exists to avoid.
fn run(workspace: &Path, args: &[&str]) -> String {
    let mut command = Command::new(carrick());
    command
        .args(args)
        .current_dir(workspace)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", workspace.join(".test-credentials"));
    as_signed_in(&mut command, args);
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
    let (stdout, code) = run_with_env_code(workspace, args, env);
    assert_eq!(code, Some(0), "carrick {args:?} exited {code:?}:\n{stdout}");
    stdout
}

/// [`run_with_env`], handing back the exit code rather than asserting it.
fn run_with_env_code(
    workspace: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> (String, Option<i32>) {
    let mut command = Command::new(carrick());
    command
        .args(args)
        .current_dir(workspace)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", workspace.join(".test-credentials"));
    as_signed_in(&mut command, args);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command
        .output()
        .unwrap_or_else(|e| panic!("carrick {args:?}: {e}"));
    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
    }
    (
        String::from_utf8(output.stdout).expect("stdout was not UTF-8"),
        output.status.code(),
    )
}

/// `check <file> --recheck --json`, with the exit code checked against the
/// answer: a re-check that ran out of time, now (`deadline`) or on an earlier
/// edit against this index (`known_miss`), is not a check of the edit and
/// exits 2 (carrick#3860); every other answer exits 0.
fn recheck_json(root: &Path, file: &str, env: &[(&str, &str)]) -> serde_json::Value {
    let args = ["check", file, "--workspace", ".", "--recheck", "--json"];
    let (text, code) = run_with_env_code(root, &args, env);
    let answer: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON: {e}\n{text}"));
    let ran_out = answer["recheck"]["deadline"] == serde_json::json!(true)
        || answer["recheck"]["known_miss"] == serde_json::json!(true);
    assert_eq!(
        code,
        Some(if ran_out { 2 } else { 0 }),
        "the exit code does not match the answer:\n{answer:#}"
    );
    answer
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

/// The local-mode workspace, indexed, with a linked git worktree of
/// `catalog-web` at `catalog-web/.claude/worktrees/b`, where an agent works on
/// a branch while the index stays bound to the main checkout (carrick#3595).
/// The folder is ignored as it is in a real repo, so the main checkout never
/// lists the worktree's files as its own. Returns the worktree's top.
fn indexed_with_a_worktree(root: &Path) -> PathBuf {
    index(root);
    let repo = root.join("catalog-web");
    git(
        &repo,
        &["worktree", "add", "-q", ".claude/worktrees/b", "-b", "b"],
    );
    repo.join(".claude/worktrees/b")
        .canonicalize()
        .expect("the worktree exists")
}

/// A workspace whose repos ignore `.claude/worktrees/`, as a repo that hands
/// agents their own worktrees does.
fn workspace_ignoring_worktrees() -> tempfile::TempDir {
    workspace_prepared(
        "local-mode-workspace",
        &["catalog-web", "inventory-svc"],
        |repo| {
            std::fs::write(repo.join(".gitignore"), ".claude/worktrees/\n")
                .expect("write the ignore file")
        },
    )
}

/// A carrick command run from `dir` against the workspace at `root`, with its
/// stdout. Exit codes are not asserted: `check --changed` exits non-zero on a
/// failing pair, which is what one of these runs is for.
fn run_from(dir: &Path, root: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut command = Command::new(carrick());
    command
        .args(args)
        .arg("--workspace")
        .arg(root)
        .current_dir(dir)
        .env_remove("CARRICK_TOKEN")
        .env_remove("CARRICK_CHECK_RESCAN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .env("CARRICK_MOCK_ALL", "1");
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command
        .output()
        .unwrap_or_else(|e| panic!("carrick {args:?}: {e}"));
    String::from_utf8(output.stdout).expect("stdout was not UTF-8")
}

fn json_of(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON: {e}\n{text}"))
}

/// A file in a linked worktree of an indexed repo is that repo's file, read
/// from the worktree (carrick#3595): the answer names it by its path in the
/// repo and the service that path is in, an edit made only in the worktree
/// makes it stale, and the re-check judges the worktree's bytes. The main
/// checkout's answer for the same file does not move.
#[test]
#[serial]
fn a_file_in_a_linked_worktree_is_checked_as_its_repos_file() {
    let workspace = workspace_ignoring_worktrees();
    let root = workspace.path();
    let route = "app/routes/api.v1.widgets.$widgetId.ts";
    let main_route = format!("catalog-web/{route}");
    let worktree = indexed_with_a_worktree(root);
    let worktree_route = worktree.join(route);
    let budget = [("CARRICK_RECHECK_BUDGET_MS", "600000")];

    let main = check_json(root, &main_route);
    let shown = worktree_route.to_string_lossy().to_string();
    let before = json_of(&run_from(
        &worktree,
        root,
        &["check", &shown, "--json"],
        &[],
    ));
    assert_eq!(
        before["file"],
        serde_json::json!(route),
        "named by its path in the repo, not under the worktree folder:\n{before:#}"
    );
    assert_eq!(
        before["service"], main["service"],
        "in the service that path is in:\n{before:#}"
    );
    assert_eq!(
        before["repo"], main["repo"],
        "of the repo the index holds:\n{before:#}"
    );
    assert_eq!(before["stale"], serde_json::json!(false), "{before:#}");

    edit(
        &worktree_route,
        "activeCount: number",
        "activeCount: string",
    );
    edit(&worktree_route, "activeCount: 3", "activeCount: \"3\"");

    let edited = json_of(&run_from(
        &worktree,
        root,
        &["check", &shown, "--json"],
        &[],
    ));
    assert_eq!(
        edited["stale"],
        serde_json::json!(true),
        "an edit only in the worktree is a change to the file:\n{edited:#}"
    );
    let fresh = json_of(&run_from(
        &worktree,
        root,
        &["check", &shown, "--recheck", "--json"],
        &budget,
    ));
    assert_eq!(
        fresh["recheck"]["ran"],
        serde_json::json!("extraction+types"),
        "{fresh:#}"
    );
    assert_eq!(
        fresh["items"][0]["verdict"]["result"],
        serde_json::json!("type_mismatch"),
        "the re-check judges the worktree's bytes:\n{fresh:#}"
    );

    // Must not change: the main checkout still reads its own copy.
    let main_after = check_json(root, &main_route);
    assert_eq!(
        main_after["stale"],
        serde_json::json!(false),
        "{main_after:#}"
    );
    assert_eq!(
        main_after["items"][0]["verdict"]["result"],
        serde_json::json!("compatible"),
        "{main_after:#}"
    );
}

/// `check --changed` run inside a linked worktree lists the worktree's own
/// changes, because git runs in the worktree; run from the main checkout it
/// lists none of them (carrick#3595).
#[test]
#[serial]
fn check_changed_inside_a_worktree_lists_the_worktrees_changes() {
    let workspace = workspace_ignoring_worktrees();
    let root = workspace.path();
    let route = "app/routes/api.v1.widgets.$widgetId.ts";
    let worktree = indexed_with_a_worktree(root);
    edit(
        &worktree.join(route),
        "activeCount: number",
        "activeCount: string",
    );
    edit(
        &worktree.join(route),
        "activeCount: 3",
        "activeCount: \"3\"",
    );
    let budget = [("CARRICK_CHECK_BUDGET_MS", "600000")];
    let files_of = |json: &serde_json::Value| -> serde_json::Value {
        json["repos"]
            .as_array()
            .and_then(|repos| {
                repos.iter().find(|repo| {
                    repo["repo"]
                        .as_str()
                        .is_some_and(|r| r.ends_with("catalog-web"))
                })
            })
            .map(|repo| repo["files"].clone())
            .unwrap_or(serde_json::Value::Null)
    };

    let from_main = json_of(&run_from(
        root,
        root,
        &["check", "--changed", "--json"],
        &budget,
    ));
    assert_eq!(
        files_of(&from_main),
        serde_json::json!([]),
        "the main checkout has no change of its own:\n{from_main:#}"
    );

    let from_worktree = json_of(&run_from(
        &worktree,
        root,
        &["check", "--changed", "--json"],
        &budget,
    ));
    assert_eq!(
        files_of(&from_worktree),
        serde_json::json!([route]),
        "the worktree's edit, by its path in the repo:\n{from_worktree:#}"
    );
    assert!(
        from_worktree["problems"]
            .as_array()
            .is_some_and(|problems| problems
                .iter()
                .any(|problem| problem["kind"] == "incompatible")),
        "judged from the worktree's bytes:\n{from_worktree:#}"
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
    let degraded = recheck_json(root, route, &[("CARRICK_RECHECK_BUDGET_MS", "1")]);
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

    let temp = private_temp();
    let tmpdir = temp.path().to_str().expect("a UTF-8 temp path");
    let before = recheck_temp_dirs(temp.path());
    let started = Instant::now();
    let degraded = recheck_json(
        root,
        route,
        &[("CARRICK_RECHECK_BUDGET_MS", "1"), ("TMPDIR", tmpdir)],
    );

    assert_eq!(
        degraded["recheck"]["ran"],
        serde_json::json!("none"),
        "no half of the re-check ran:\n{degraded:#}"
    );
    assert_eq!(
        degraded["recheck"]["deadline"],
        serde_json::json!(true),
        "and it says time ran out, so the exit is not a clean one:\n{degraded:#}"
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
        recheck_temp_dirs(temp.path()),
        before,
        "a re-check leaves no temporary generation behind, including one it killed"
    );
}

/// carrick#3860: a re-check stopped at its budget answers from the index, and
/// that is never a clean result. Before any row the terminal says the file was
/// not checked and names the index commit it answered from, and the command
/// exits 2, as a check that covered nothing does. The edit breaks the
/// consumer's type, and the indexed `compatible` verdict below the line is
/// from before it.
#[test]
#[serial]
fn a_re_check_stopped_at_its_budget_is_never_a_clean_result() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    let route = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";
    let route_file = root.join(route);

    index(root);
    edit(&route_file, "activeCount: number", "activeCount: string");
    edit(&route_file, "activeCount: 3", "activeCount: \"3\"");

    let (text, code) = run_with_env_code(
        root,
        &["check", route, "--workspace", ".", "--recheck"],
        &[("CARRICK_RECHECK_BUDGET_MS", "1")],
    );
    assert_eq!(code, Some(2), "a deadline fallback is not a check:\n{text}");
    let header = text.lines().next().expect("a header line");
    let commit = header
        .rsplit_once("indexed at ")
        .map(|(_, at)| at.trim_end_matches(')'))
        .unwrap_or_else(|| panic!("no index commit in the header:\n{text}"));
    let line = text
        .lines()
        .find(|line| line.starts_with("re-check: not checked, "))
        .unwrap_or_else(|| panic!("the answer does not say it was not checked:\n{text}"));
    assert!(
        line.contains(&format!("from the index at {commit}")),
        "it names the index it answered from:\n{text}"
    );
    let said = text.find(line).expect("the line");
    let verdict = text
        .find("    verdict   ")
        .unwrap_or_else(|| panic!("no indexed verdict row:\n{text}"));
    assert!(said < verdict, "it says so before any verdict:\n{text}");
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
    let ask = |env: &[(&str, &str)]| -> serde_json::Value { recheck_json(root, route, env) };
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

    // The same budget again: the miss holds and no re-scan starts. A LONGER
    // budget is not told the service does not fit, so it re-scans.
    let skipped = ask(&[("CARRICK_RECHECK_BUDGET_MS", "1")]);
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

    let longer = ask(&ample);
    assert_ne!(
        longer["recheck"]["ran"],
        serde_json::json!("none"),
        "a miss at 1 ms does not silence a call with ten minutes:\n{longer:#}"
    );
    assert_eq!(
        longer["recheck"]["known_miss"],
        serde_json::Value::Null,
        "{longer:#}"
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

/// An edit the scan reads the same as the indexed file (a comment, a reflow)
/// is answered from the index at once: no re-scan starts, the rows keep their
/// verdicts, and each row's line follows its node.
#[test]
#[serial]
fn a_comment_only_edit_is_answered_without_a_re_scan() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    let route = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";
    let route_file = root.join(route);

    index(root);
    let args = ["check", route, "--workspace", ".", "--recheck", "--json"];
    let indexed: serde_json::Value = serde_json::from_str(&run(root, &args)).expect("JSON");
    let line = indexed["items"][0]["line"].as_u64().expect("a row line");

    edit(
        &route_file,
        "export async function",
        "// a note\nexport async function",
    );

    let temp = private_temp();
    let tmpdir = temp.path().to_str().expect("a UTF-8 temp path");
    let before = recheck_temp_dirs(temp.path());
    let answered: serde_json::Value =
        serde_json::from_str(&run_with_env(root, &args, &[("TMPDIR", tmpdir)])).expect("JSON");
    assert_eq!(answered["stale"], serde_json::json!(false), "{answered:#}");
    assert!(
        answered["changed_since_index"].as_u64() >= Some(1),
        "the tree did move: {answered:#}"
    );
    assert_eq!(
        answered["recheck"]["ran"],
        serde_json::json!("none"),
        "{answered:#}"
    );
    assert_eq!(
        answered["recheck"]["reason"],
        serde_json::json!(
            "the edit changed nothing the scan reads (comments or layout), or no service reads the file; verdicts are the index's"
        ),
        "{answered:#}"
    );
    assert_eq!(
        answered["items"][0]["line"].as_u64(),
        Some(line + 1),
        "{answered:#}"
    );
    assert_eq!(
        answered["items"][0]["verdict"]["state"], indexed["items"][0]["verdict"]["state"],
        "{answered:#}"
    );
    assert_eq!(
        answered["items"][0]["verdict"]["detail"], indexed["items"][0]["verdict"]["detail"],
        "no \"unresolved since your edit\" caveat on an unmoved row: {answered:#}"
    );
    assert_eq!(
        recheck_temp_dirs(temp.path()),
        before,
        "no re-scan generation was started"
    );
}

/// carrick#3204: a comment-only edit re-runs nothing, so its `--json` answer
/// carries the index's own `type_check`, ran or skipped with its reason.
#[test]
#[serial]
fn a_comment_only_edit_answers_with_the_indexs_type_check() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    let route = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";
    index(root);
    let args = ["check", route, "--workspace", ".", "--recheck", "--json"];

    for skipped in [None, Some("type checker not found")] {
        if let Some(reason) = skipped {
            let path = root.join(".carrick/index.json");
            let mut stored: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).expect("read the index"))
                    .expect("the index is JSON");
            stored["type_check"]["skipped"] = serde_json::json!(reason);
            std::fs::write(&path, stored.to_string()).expect("write the index");
        }
        let indexed: serde_json::Value =
            serde_json::from_str(&run(root, &["check", route, "--workspace", ".", "--json"]))
                .expect("JSON");
        edit(
            &root.join(route),
            "export async function",
            "// a note\nexport async function",
        );
        let answered: serde_json::Value = serde_json::from_str(&run(root, &args)).expect("JSON");
        assert_eq!(answered["recheck"]["ran"], "none", "{answered:#}");
        assert_eq!(
            answered["type_check"], indexed["type_check"],
            "{answered:#}"
        );
        assert_eq!(
            answered["type_check"]["ran"],
            skipped.is_none(),
            "{answered:#}"
        );
        assert_eq!(
            answered["type_check"]["reason"],
            skipped.map_or(serde_json::Value::Null, serde_json::Value::from),
            "{answered:#}"
        );
        edit(
            &root.join(route),
            "// a note\nexport async function",
            "export async function",
        );
    }
}

/// A token the scan reads, or a comment that carries a type, is not an edit
/// that reads the same: both still take the re-scan.
#[test]
#[serial]
fn an_edit_the_scan_reads_differently_is_still_re_scanned() {
    for (from, to) in [
        ("hex-key", "other-key"),
        (
            "export async function",
            "/** @type {number} */\nexport async function",
        ),
    ] {
        let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
        let root = workspace.path();
        let route = "catalog-web/app/routes/api.v1.widgets.$widgetId.ts";

        index(root);
        edit(&root.join(route), from, to);
        let answered: serde_json::Value = serde_json::from_str(&run_with_env(
            root,
            &["check", route, "--workspace", ".", "--recheck", "--json"],
            &[("CARRICK_RECHECK_BUDGET_MS", "600000")],
        ))
        .expect("JSON");
        assert_ne!(
            answered["recheck"]["ran"],
            serde_json::json!("none"),
            "{to:?} must re-scan:\n{answered:#}"
        );
    }
}

/// A temp directory of the test's own, handed to the re-check as `TMPDIR`.
///
/// The re-check stages its generation under the system temp directory, and
/// nextest runs every other test in a process of its own beside this one: a
/// count taken in the shared directory moved whenever another test's re-check
/// came or went between the two reads (carrick#3456).
fn private_temp() -> tempfile::TempDir {
    tempfile::tempdir().expect("a private temp dir")
}

/// Temporary generations a re-check has left in `temp`, the directory it was
/// handed as `TMPDIR` by [`private_temp`].
fn recheck_temp_dirs(temp: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(temp) else {
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
    // No installed model runs here, but the store's answer for the two socket
    // packages is read as a signed-in machine reads it: the socket rows are the
    // fact layer's. The answers are the parity test's cassettes for these repos.
    let claims = socket_claims_cassette();
    run_with_env(
        root,
        &["refresh", "--workspace", "."],
        &[(
            "CARRICK_MOCK_FIXTURE_DIR",
            claims.path().to_str().expect("utf-8 path"),
        )],
    );

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

    let routes = check(root, "notifications-svc/src/http/routes.ts");
    assert!(
        routes.contains("no routes or calls indexed in this file"),
        "the file's rows, or the absence of them:\n{routes}"
    );
    assert!(
        routes.contains("not classified locally"),
        "with the reason beside it, in the answer a hook reads:\n{routes}"
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

    // The answer's boundary rides stdout, once, before the map (carrick#4357):
    // the wrapper tells the two apart by it and not by which pipe it read first.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let boundaries: Vec<usize> = stdout
        .lines()
        .enumerate()
        .filter(|(_, line)| carrick::progress::parse_answered(line))
        .map(|(at, _)| at)
        .collect();
    assert_eq!(
        boundaries.len(),
        1,
        "exactly one answer boundary on stdout:\n{stdout}"
    );
    let map = stdout
        .lines()
        .position(|line| line.contains("indexed 2 repo(s)"))
        .unwrap_or_else(|| panic!("the map is missing from stdout:\n{stdout}"));
    assert!(
        boundaries[0] < map,
        "the boundary comes before the map:\n{stdout}"
    );
    assert!(
        !stderr.lines().any(carrick::progress::parse_answered),
        "the boundary is never written on stderr:\n{stderr}"
    );
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
        .find(|line| line.contains("candidate(s) waiting for Carrick's cloud scan"))
        .unwrap_or_else(|| panic!("no service line states what is waiting:\n{map}"));
    assert!(
        waiting.contains("route(s)") && waiting.contains("call(s)"),
        "it is on the service's own table line:\n{map}"
    );

    let status = run(root, &["status", "--workspace", "."]);
    assert!(
        status.contains("candidate(s) waiting for Carrick's cloud scan"),
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
        // Blobs only: a type-reach record sits beside each (carrick#3101).
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
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

/// carrick#3204: an index built without type checking says so in the check
/// answer, with the reason, and an index that checked says it did. Before, the
/// reason was only in `carrick status`, so a reader of one file's answer could
/// not tell "no pair here was compared" from "nothing was ever type-checked".
#[test]
#[serial]
fn a_check_answer_states_whether_the_index_was_type_checked() {
    let (workspace, said) = laptop_index("inventory-svc");
    let root = workspace.path();
    let file = "inventory-svc/src/inventory.ts";

    let ran = check_json(root, file);
    assert_eq!(ran["type_check"]["ran"], true, "{ran:#}\n{said}");
    assert!(ran["type_check"]["reason"].is_null(), "{ran:#}");

    // The same index, as a build on a machine with no type checker leaves it.
    let path = root.join(".carrick/index.json");
    let mut index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read the index"))
            .expect("the index is JSON");
    index["type_check"]["skipped"] = serde_json::json!("type checker not found");
    std::fs::write(&path, index.to_string()).expect("write the index");

    let skipped = check_json(root, file);
    assert_eq!(skipped["type_check"]["ran"], false, "{skipped:#}");
    assert_eq!(
        skipped["type_check"]["reason"], "type checker not found",
        "{skipped:#}"
    );
    assert!(skipped["type_check"]["pairs"].is_number(), "{skipped:#}");
    assert!(skipped["type_check"]["judged"].is_number(), "{skipped:#}");
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
        .env("CARRICK_CHECK_BUDGET_MS", "600000")
        .env("CARRICK_MOCK_ALL", "1")
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
        .env("CARRICK_CHECK_BUDGET_MS", "600000")
        .env("CARRICK_MOCK_ALL", "1")
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
        .env("CARRICK_CHECK_BUDGET_MS", "600000")
        .env("CARRICK_MOCK_ALL", "1")
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

/// The consumer gains a member that hands its origin to a helper from another
/// file (`sendTo` in send.ts), `sendTo(this.baseUrl, "/path", ...)`, and a caller that makes it a
/// member the repo uses. The path is bare but the origin is a run-time value.
fn add_a_call_through_a_handed_base(root: &Path) {
    let client = root.join("inventory-svc/src/client.ts");
    edit(
        &client,
        "import { send } from",
        "import { send, sendTo } from",
    );
    edit(
        &client,
        "  readWidget(widgetId: string)",
        "  readGadget(gadgetId: string): Promise<Widget> {\n    return sendTo<Widget>(this.baseUrl, `/api/v1/gadgets/${gadgetId}`, {\n      method: \"GET\",\n    });\n  }\n\n  readWidget(widgetId: string)",
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

/// The per-edit re-check states the same no-route call `check --changed`
/// lists for the same edit: the edited file's call gets `no_route`, not a null
/// verdict that the hook then prints nothing for.
#[test]
#[serial]
fn a_re_check_states_a_new_call_to_a_route_that_does_not_exist_as_no_route() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    add_a_call_to_no_route(root, RELATIVE);
    // The call is resolved where the wrapper is used: the caller's file.
    let client = "inventory-svc/src/inventory.ts";
    let budget = [("CARRICK_RECHECK_BUDGET_MS", "600000")];

    let (_, changed) = check_changed(root, false);
    assert!(
        changed
            .lines()
            .any(|line| line.starts_with("inventory-svc/src/inventory.ts:")
                && line.contains("  no route  ")),
        "check --changed lists the call:\n{changed}"
    );

    let fresh: serde_json::Value = serde_json::from_str(&run_with_env(
        root,
        &["check", client, "--workspace", ".", "--recheck", "--json"],
        &budget,
    ))
    .expect("check --recheck --json was not JSON");
    let gadget = fresh["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["path"] == "/api/v1/gadgets/:gadgetId")
        .unwrap_or_else(|| panic!("the new call is an item:\n{fresh:#}"));
    assert_eq!(
        gadget["verdict"]["result"],
        serde_json::json!("no_route"),
        "the edited file's call reaching no route is stated:\n{fresh:#}"
    );
    assert_eq!(gadget["verdict"]["state"], serde_json::json!("not_checked"));
    assert!(
        gadget["verdict"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("no service in this workspace serves it")),
        "with judge's reason:\n{fresh:#}"
    );
    // The calls that match keep what they had.
    assert!(
        fresh["items"]
            .as_array()
            .expect("items")
            .iter()
            .filter(|item| item["path"] != "/api/v1/gadgets/:gadgetId")
            .all(|item| item["verdict"]["result"] != serde_json::json!("no_route")),
        "no other row is stated as no route:\n{fresh:#}"
    );
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
        .env("CARRICK_MOCK_ALL", "1")
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
        .env("CARRICK_MOCK_ALL", "1")
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

/// A new call through an imported member whose helper is handed the origin
/// (`sendTo(this.baseUrl, "/path", ...)`): the base is injected, so the call
/// is printed as unconfirmed and does not fail with "no service serves it".
/// The row exists only with the fact layer on, which `check` runs.
#[test]
#[serial]
fn check_changed_prints_a_new_call_through_a_helper_handed_its_base_as_unconfirmed() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    add_a_call_through_a_handed_base(root);

    let (code, text) = check_changed_all(root);
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("/api/v1/gadgets") && line.contains("  no route  "))
        .collect();
    assert!(!lines.is_empty(), "the new call is printed:\n{text}");
    for line in lines {
        assert!(
            line.contains("[unconfirmed: the base is set at run time, does not fail]"),
            "as unconfirmed:\n{text}"
        );
    }
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

/// `check --changed --json` with extra environment: the exit code, the JSON
/// and stderr, where the narrator says whether a repo was re-read.
fn check_changed_json_with(
    workspace: &Path,
    env: &[(&str, &str)],
) -> (Option<i32>, serde_json::Value, String) {
    let mut command = Command::new(carrick());
    command
        .args(["check", "--changed", "--workspace", ".", "--json"])
        .current_dir(workspace)
        .env_remove("CARRICK_TOKEN")
        .env_remove("CARRICK_CHECK_RESCAN")
        .env("XDG_CONFIG_HOME", workspace.join(".test-credentials"))
        .env("CARRICK_CHECK_BUDGET_MS", "600000");
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().expect("run carrick check --changed");
    let body = String::from_utf8(output.stdout).expect("stdout was not UTF-8");
    let json = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("check --changed --json was not JSON: {e}\n{body}"));
    (
        output.status.code(),
        json,
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// What `rescan` says for the repo whose folder is `name`.
fn rescan_of(json: &serde_json::Value, name: &str) -> Option<String> {
    json["repos"]
        .as_array()?
        .iter()
        .find(|repo| repo["repo"].as_str().is_some_and(|r| r.ends_with(name)))?["rescan"]
        .as_str()
        .map(str::to_string)
}

/// The answer without the two fields that say how it was reached.
fn without_how(json: &serde_json::Value) -> serde_json::Value {
    let mut json = json.clone();
    json.as_object_mut()
        .expect("an object")
        .remove("elapsed_ms");
    for repo in json["repos"].as_array_mut().expect("repos") {
        repo.as_object_mut().expect("a repo").remove("rescan");
    }
    json
}

/// A branch whose main already holds a type mismatch below a route: the
/// producer stopped sending a field the consumer reads. The index is built
/// from that commit, then the work moves to a branch.
fn a_mismatch_the_index_already_holds() -> (tempfile::TempDir, PathBuf) {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    let producer = root.join("catalog-web");
    let route = producer.join("app/routes/api.v1.widgets.$widgetId.ts");
    edit(&route, "  activeCount: number;\n", "");
    edit(&route, ", activeCount: 3", "");
    commit_all(&producer, "drop activeCount, on main");
    index(root);
    git(&producer, &["checkout", "-qb", "feature"]);
    (workspace, route)
}

/// carrick#3245: a comment nothing reads, added above the route, starts no
/// re-scan. The answer is the re-scan's answer, with the mismatch at the line
/// the code moved to.
#[test]
#[serial]
fn check_changed_answers_a_comment_only_edit_without_a_re_scan() {
    let (workspace, route) = a_mismatch_the_index_already_holds();
    let root = workspace.path();
    edit(
        &route,
        "export async function loader",
        "// A plain note.\nexport async function loader",
    );

    // Where the route's handler is written, once the note is above it.
    let loader_line = std::fs::read_to_string(&route)
        .expect("read the route")
        .lines()
        .position(|line| line.starts_with("export async function loader"))
        .expect("the loader") as u64
        + 1;

    let (code, json, stderr) = check_changed_json_with(root, &[]);
    assert_eq!(
        rescan_of(&json, "catalog-web").as_deref(),
        Some("not_needed"),
        "{json:#}"
    );
    assert!(
        stderr.contains("nothing the scan reads changed in catalog-web"),
        "said so:\n{stderr}"
    );
    assert!(
        !stderr.contains("re-reading"),
        "no re-scan started:\n{stderr}"
    );
    let problem = &json["problems"][0];
    assert_eq!(problem["kind"], "incompatible", "{json:#}");
    assert_eq!(problem["already_indexed"], true, "{json:#}");
    assert_eq!(
        problem["line"], loader_line,
        "printed at the line the handler moved to, not where the index wrote it:\n{json:#}"
    );
    assert_eq!(code, Some(0), "{json:#}");

    let (_, always, stderr) = check_changed_json_with(root, &[("CARRICK_CHECK_RESCAN", "always")]);
    assert_eq!(
        rescan_of(&always, "catalog-web").as_deref(),
        Some("ran"),
        "{always:#}"
    );
    assert!(
        stderr.contains("re-reading"),
        "the switch re-scans:\n{stderr}"
    );
    assert!(always["problems"][0]["line"].is_u64(), "{always:#}");
    // This machine is not signed in (carrick#3249), so a forced re-scan says
    // its library facts were unavailable; the answer from the index has no
    // such line. What the two must agree on is the problems and their lines.
    assert!(
        always["not_checked"]
            .as_str()
            .is_some_and(|line| line.contains("not signed in")),
        "{always:#}"
    );
    let mut always = without_how(&always);
    always
        .as_object_mut()
        .expect("an object")
        .remove("not_checked");
    assert_eq!(
        without_how(&json),
        always,
        "the answer without the re-scan is the re-scan's, at the moved line"
    );
}

/// A comment the scan reads (`@ts-expect-error`) is not a comment nothing
/// reads: the repo is re-scanned.
#[test]
#[serial]
fn check_changed_re_scans_a_directive_comment_edit() {
    let (workspace, route) = a_mismatch_the_index_already_holds();
    let root = workspace.path();
    edit(
        &route,
        "export async function loader",
        "// @ts-expect-error\nexport async function loader",
    );
    let (_, json, stderr) = check_changed_json_with(root, &[]);
    assert_eq!(
        rescan_of(&json, "catalog-web").as_deref(),
        Some("ran"),
        "{json:#}"
    );
    assert!(stderr.contains("re-reading"), "{stderr}");
}

/// A string the code states changed: the repo is re-scanned and the answer is
/// the re-scan's.
#[test]
#[serial]
fn check_changed_re_scans_a_string_edit() {
    let (workspace, route) = a_mismatch_the_index_already_holds();
    let root = workspace.path();
    edit(&route, "\"hex-key\"", "\"allen-key\"");
    let (_, json, _) = check_changed_json_with(root, &[]);
    assert_eq!(
        rescan_of(&json, "catalog-web").as_deref(),
        Some("ran"),
        "{json:#}"
    );
    let (_, always, _) = check_changed_json_with(root, &[("CARRICK_CHECK_RESCAN", "always")]);
    assert_eq!(without_how(&json), without_how(&always));
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

/// carrick#3411: an agent adds one new file with no route, call or event in
/// one package; socket emits in an untouched file of another package are not
/// named as left with no route.
#[test]
#[serial]
fn check_changed_does_not_name_untouched_socket_emits_for_an_unrelated_new_file() {
    let workspace = workspace(
        "xrepo-corpus-1",
        &["orders-monorepo", "payments-svc", "web-frontend"],
    );
    let root = workspace.path();
    index(root);
    // The known positive: the emit and its listener are paired in the index.
    let socket = touch(root, "payments-svc/realtime/server.ts");
    assert!(socket.contains("payment:settled"), "{socket}");

    std::fs::write(
        root.join("orders-monorepo/packages/orders-pkg/src/slug.ts"),
        "export function slug(name: string): string {\n  return name.toLowerCase().replace(/ /g, \"-\");\n}\n",
    )
    .expect("add an unrelated file");

    let (code, text) = check_changed(root, false);
    assert!(
        !text.contains("payment:settled") && !text.contains("realtime"),
        "the untouched emits are not named:\n{text}"
    );
    assert!(
        text.contains(": 0 incompatible, 0 reach no route"),
        "and nothing is left with no route:\n{text}"
    );
    assert_eq!(code, Some(0), "{text}");
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
    carry_workspace_with(|_| {})
}

/// [`carry_workspace`] with `prepare` run on the workspace before it is
/// committed and indexed, so the index holds the prepared state.
fn carry_workspace_with(prepare: impl FnOnce(&Path)) -> tempfile::TempDir {
    let workspace = workspace("edited-handler-carry", &["items-api", "items-web"]);
    let root = workspace.path();
    prepare(root);
    // Commit only what `prepare` changed: an empty commit on top would leave a
    // later `commit --amend` with nothing to amend.
    for repo in ["items-api", "items-web"] {
        let status = Command::new("git")
            .arg("-C")
            .arg(root.join(repo))
            .args(["status", "--porcelain"])
            .output()
            .expect("git status");
        if status.stdout.is_empty() {
            continue;
        }
        git(&root.join(repo), &["add", "-A"]);
        git(
            &root.join(repo),
            &[
                "-c",
                "user.email=fixture@carrick.test",
                "-c",
                "user.name=fixture",
                "commit",
                "-qm",
                "prepared",
            ],
        );
    }
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
    let mut command = Command::new(carrick());
    command
        .args(args)
        .current_dir(workspace)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", workspace.join(".test-credentials"))
        .env("CARRICK_CHECK_BUDGET_MS", "600000")
        .env("CARRICK_RECHECK_BUDGET_MS", "600000");
    as_signed_in(&mut command, args);
    let output = command
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

/// The fixture's routes with a field typed through an uninstalled import added
/// to the first handler, and to the other two when `all` is set, indexed in
/// that state. Those rows have no usable type on the producer side before the
/// edit and after it. The line numbers do not move: the import goes last.
fn carry_workspace_without_types(all: bool) -> tempfile::TempDir {
    carry_workspace_without_types_with(all, false)
}

/// A minimal install of the framework the fixture's routes register on: its
/// declarations and a public lockfile entry, as a lockfile-carrying checkout
/// holds them.
fn install_framework_stand_in(root: &Path) {
    let package = root.join("items-api/node_modules/fastify");
    std::fs::create_dir_all(&package).expect("make the package folder");
    std::fs::write(
        package.join("package.json"),
        "{ \"name\": \"fastify\", \"version\": \"4.28.1\", \"types\": \"index.d.ts\" }\n",
    )
    .expect("write the manifest");
    std::fs::write(
        package.join("index.d.ts"),
        "export interface FastifyRequest { params: unknown; query: unknown; body: unknown; }\n\
export interface FastifyReply { send(payload?: unknown): FastifyReply; }\n\
export type RouteHandlerMethod = (request: FastifyRequest, reply: FastifyReply) => unknown | Promise<unknown>;\n\
export interface FastifyInstance {\n  get(path: string, handler: RouteHandlerMethod): FastifyInstance;\n}\n\
declare function fastify(): FastifyInstance;\nexport default fastify;\n",
    )
    .expect("write the declarations");
    std::fs::write(
        root.join("items-api/package-lock.json"),
        "{\n  \"name\": \"items-api\",\n  \"lockfileVersion\": 3,\n  \"requires\": true,\n  \"packages\": {\n    \"\": { \"name\": \"items-api\", \"dependencies\": { \"fastify\": \"^4.0.0\" } },\n    \"node_modules/fastify\": { \"version\": \"4.28.1\", \"resolved\": \"https://registry.npmjs.org/fastify/-/fastify-4.28.1.tgz\" }\n  }\n}\n",
    )
    .expect("write the lockfile");
}

fn carry_workspace_without_types_with(all: bool, framework_installed: bool) -> tempfile::TempDir {
    carry_workspace_with(|root| {
        if framework_installed {
            install_framework_stand_in(root);
        }
        let file = root.join(CARRY_ROUTES);
        edit(
            &file,
            "return { id, connected: true };",
            "return { id, connected: true, owner: ownerOf(id) };",
        );
        if all {
            edit(
                &file,
                "reply.send({ id, state: 'ready' });",
                "reply.send({ id, state: 'ready', owner: ownerOf(id) });",
            );
            edit(
                &file,
                "[{ id: '1', connected: true }]",
                "[{ id: '1', connected: true, owner: ownerOf('1') }]",
            );
        }
        let mut text = std::fs::read_to_string(&file).expect("read the routes");
        text.push_str("import { ownerOf } from 'owners-db';\n");
        std::fs::write(&file, text).expect("append the import");
    })
}

fn named_lines(changed: &serde_json::Value) -> Vec<String> {
    changed["not_type_checked"]
        .as_array()
        .expect("not_type_checked is a list")
        .iter()
        .map(|row| format!("{}:{}", row["file"], row["line"]))
        .collect()
}

/// Every matched route in the changed file has no usable type, so a type edit
/// on one can be neither seen nor failed. The check names the rows and exits 2
/// instead of reading as a pass (carrick#3012).
#[test]
#[serial]
fn an_edit_to_a_pair_with_no_usable_type_is_named_not_passed() {
    let workspace = carry_workspace_without_types(true);
    let root = workspace.path();
    edit(
        &root.join(CARRY_ROUTES),
        "connected: true, owner",
        "connected: 1, owner",
    );

    let (code, changed) = carry_check_changed(root);
    assert_eq!(
        code,
        Some(2),
        "nothing was type-checked and nothing fails:\n{changed:#}"
    );
    let named = named_lines(&changed);
    assert!(
        named.contains(&format!("\"{CARRY_ROUTES}\":4")),
        "the edited route is named: {named:?}\n{changed:#}"
    );
    assert!(
        named.contains(&format!("\"{CARRY_ROUTES}\":9")),
        "the other typed-through-import route is named too: {named:?}\n{changed:#}"
    );
    assert_eq!(changed["type_checked"], 0, "{changed:#}");
    assert_eq!(changed["failing"], 0, "{changed:#}");
}

/// With the framework installed and no store answer, a refused reading of a
/// route's reply must not fall to the registration call's own type: the capture
/// would publish the framework's instance type as the reply and the edit would
/// read as a failure. The rows are named unchecked instead (carrick#4079).
#[test]
#[serial]
fn an_installed_framework_instance_type_is_not_published_as_a_route_reply() {
    let workspace = carry_workspace_without_types_with(true, true);
    let root = workspace.path();
    edit(
        &root.join(CARRY_ROUTES),
        "connected: true, owner",
        "connected: 1, owner",
    );

    let (code, changed) = carry_check_changed(root);
    assert_eq!(
        code,
        Some(2),
        "nothing was type-checked and nothing fails:\n{changed:#}"
    );
    let named = named_lines(&changed);
    for line in [4, 9] {
        assert!(
            named.contains(&format!("\"{CARRY_ROUTES}\":{line}")),
            "the route at line {line} is named: {named:?}\n{changed:#}"
        );
    }
    assert_eq!(changed["failing"], 0, "{changed:#}");
}

/// A partial run exits by the rows it did judge: two routes keep a verdict, so
/// the run passes, and the one it could not judge is still named.
#[test]
#[serial]
fn a_partly_judged_file_names_the_unjudged_row_and_exits_by_the_rest() {
    let workspace = carry_workspace_without_types(false);
    let root = workspace.path();
    edit(
        &root.join(CARRY_ROUTES),
        "connected: true, owner",
        "connected: 1, owner",
    );

    let (code, changed) = carry_check_changed(root);
    assert_eq!(code, Some(0), "{changed:#}");
    assert_eq!(
        named_lines(&changed),
        vec![format!("\"{CARRY_ROUTES}\":4")],
        "{changed:#}"
    );
    assert_eq!(changed["type_checked"], 2, "{changed:#}");
}

/// The plain edit judges its pair: no row is left unjudged, and it fails.
#[test]
#[serial]
fn an_edit_to_a_judged_pair_names_no_unjudged_row() {
    let workspace = carry_workspace();
    let root = workspace.path();
    edit(
        &root.join(CARRY_ROUTES),
        "return { id, connected: true };",
        "return { id, connected: 1 };",
    );
    let (code, changed) = carry_check_changed(root);
    assert_eq!(code, Some(1), "{changed:#}");
    assert_eq!(
        changed["not_type_checked"],
        serde_json::json!([]),
        "{changed:#}"
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

/// Point every stored blob at `commit`, as an index written elsewhere would be.
fn stored_blobs_say_commit(root: &Path, commit: &str) {
    let dir = root.join(".carrick/repos");
    for entry in std::fs::read_dir(&dir).expect("read the stored blobs") {
        let path = entry.expect("dir entry").path();
        // Blobs only: a type-reach record sits beside each (carrick#3101).
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let mut blob: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read a blob"))
                .expect("a stored blob is JSON");
        blob["commit_hash"] = serde_json::json!(commit);
        std::fs::write(&path, serde_json::to_vec(&blob).expect("serialise")).expect("write");
    }
}

/// The index was written at a commit this clone does not hold and cannot
/// fetch: the re-scan cannot replay the model's answers, so every route only
/// the model stated is gone. That used to exit 0 with a clean report. It is
/// not a pass (carrick#3000).
#[test]
#[serial]
fn an_index_commit_missing_from_the_clone_is_not_a_pass() {
    let workspace = carry_workspace();
    let root = workspace.path();
    let missing = "f".repeat(40);
    stored_blobs_say_commit(root, &missing);
    rename_connected(root);

    let (code, changed) = carry_check_changed(root);
    assert_eq!(code, Some(2), "{changed:#}");
    let said = changed["not_checked"].as_str().unwrap_or_default();
    assert!(
        said.contains("fffffff") && said.contains("routes and calls are missing"),
        "{changed:#}"
    );
    assert!(said.contains("git fetch origin"), "{said}");
}

/// The per-edit re-check meets the same index: it re-scans the edited repo
/// with the stored answers, cannot replay them, and so answers as it does for
/// a library store it cannot ask: `ran: none`, the reason, exit 2
/// (carrick#3413).
#[test]
#[serial]
fn the_recheck_of_an_index_commit_missing_from_the_clone_is_not_a_pass() {
    let workspace = carry_workspace();
    let root = workspace.path();
    stored_blobs_say_commit(root, &"f".repeat(40));
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
    let json: serde_json::Value = serde_json::from_str(&text).expect("--json is JSON");
    assert_eq!(json["recheck"]["ran"], "none", "{json:#}");
    let said = json["recheck"]["reason"].as_str().unwrap_or_default();
    assert!(
        said.contains("fffffff") && said.contains("routes and calls are missing"),
        "{json:#}"
    );
    assert!(said.contains("git fetch origin"), "{said}");
    assert_eq!(code, Some(2), "{text}");
}

/// The same index commit, held by `origin` and gone from the clone (a PR
/// squash-merged on the host before the user pulled): the scan fetches that one
/// commit by its hash, keeps it under a ref Carrick owns, replays the model's
/// answers and judges the edit as it does when the commit is present.
#[test]
#[serial]
fn an_index_commit_origin_holds_is_fetched_and_the_edit_judged() {
    let workspace = carry_workspace();
    let root = workspace.path();
    // Judged once at the indexed commit, as the test above does.
    check_json(root, CARRY_ROUTES);
    let api = root.join("items-api");
    let indexed = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&api)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_string();
    // origin holds the indexed commit; the clone then loses it.
    let origin = root.join("items-api-origin.git");
    git(
        root,
        &["clone", "-q", "--bare", "items-api", "items-api-origin.git"],
    );
    git(
        &api,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    let mut amend = COMMIT.to_vec();
    amend.insert(5, "--amend");
    amend.push("amended");
    git(&api, &amend);
    git(&api, &["reflog", "expire", "--expire=now", "--all"]);
    git(&api, &["gc", "-q", "--prune=now"]);
    assert!(
        !Command::new("git")
            .arg("-C")
            .arg(&api)
            .args(["cat-file", "-e", &format!("{indexed}^{{commit}}")])
            .output()
            .expect("cat-file")
            .status
            .success(),
        "the clone must not hold the indexed commit"
    );
    rename_connected(root);
    // The per-edit re-check first, as the test above does: the pass below
    // reads the stubs it leaves.
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

    let (code, changed) = carry_check_changed(root);
    assert_eq!(code, Some(1), "{changed:#}");
    assert!(changed["not_checked"].is_null(), "{changed:#}");
    let kept = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&api)
            .args(["rev-parse", "refs/carrick/indexed"])
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .expect("utf8");
    assert_eq!(kept.trim(), indexed, "kept under the ref Carrick owns");
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

// --- carrick#3198: the per-edit re-check reads the edited file's service ---

const SHOP: &str = "recheck-one-repo-two-services";
const SHOP_ROUTE: &str = "shop/catalog/app/routes/api.v1.widgets.$widgetId.ts";
const SHOP_HEALTH: &str = "shop/catalog/app/routes/api.v1.health.ts";
const SHOP_CLIENT: &str = "shop/inventory/src/client.ts";
const SHOP_LEDGER: &str = "shop/ledger/app/routes/api.v1.entries.ts";

/// An ample budget, so a re-check that fits is the thing under test.
const AMPLE: [(&str, &str); 1] = [("CARRICK_RECHECK_BUDGET_MS", "600000")];

fn shop() -> tempfile::TempDir {
    workspace(SHOP, &["shop"])
}

fn reread(answer: &serde_json::Value) -> Vec<String> {
    answer["recheck"]["scope"]["reread"]
        .as_array()
        .unwrap_or_else(|| panic!("no recheck.scope.reread in:\n{answer:#}"))
        .iter()
        .map(|id| id.as_str().expect("a service id").to_string())
        .collect()
}

/// Append a statement that moves no type and no row. Not a comment: an edit
/// the scan reads the same is answered from the index with no re-check, and
/// these tests are about the re-check.
fn statement_in(file: &Path, text: &str) {
    let existing = std::fs::read_to_string(file).expect("read the file");
    std::fs::write(file, format!("{existing}\nvoid {text:?};\n")).expect("write the statement");
}

/// Make the recorded miss read as one that ran under an ample budget. A miss
/// is remembered only against calls whose budget is no longer than the one it
/// ran under (B3-R), and a test cannot make a re-scan miss ten minutes.
fn miss_ran_under_an_ample_budget(root: &Path) {
    let path = root.join(".carrick/recheck-misses.json");
    let text = std::fs::read_to_string(&path).expect("the miss was recorded");
    let mut misses: serde_json::Value = serde_json::from_str(&text).expect("misses JSON");
    for miss in misses["services"]
        .as_object_mut()
        .expect("services")
        .values_mut()
    {
        miss["budget_ms"] = serde_json::json!(600_000);
    }
    std::fs::write(&path, misses.to_string()).expect("write the misses");
}

/// A re-check reads the edited file's service and judges every other service
/// at the newest copy it has: here the client, as the last re-check left it.
/// Reading the whole repo again would give the same verdict by a different
/// road, so the scope is what pins this (carrick#3198).
#[test]
#[serial]
fn a_re_check_reads_only_the_edited_files_service_and_judges_against_the_last_re_check() {
    let workspace = shop();
    let root = workspace.path();
    index(root);

    // The client now reads a string where the route serves a number.
    edit(
        &root.join(SHOP_CLIENT),
        "activeCount: number",
        "activeCount: string",
    );
    let client = recheck_json(root, SHOP_CLIENT, &AMPLE);
    assert_eq!(reread(&client), ["inventory"], "{client:#}");
    assert_eq!(
        client["items"][0]["verdict"]["result"],
        serde_json::json!("type_mismatch"),
        "{client:#}"
    );

    // The route is changed to match. The client is NOT re-read: it stands in
    // at the copy the last re-check left, which already reads a string.
    let route = root.join(SHOP_ROUTE);
    edit(&route, "activeCount: number", "activeCount: string");
    edit(&route, "activeCount: 3", "activeCount: \"3\"");
    let answer = recheck_json(root, SHOP_ROUTE, &AMPLE);
    assert_eq!(reread(&answer), ["catalog"], "{answer:#}");
    assert_eq!(
        answer["items"][0]["verdict"]["result"],
        serde_json::json!("compatible"),
        "judged against the client as the last re-check left it:\n{answer:#}"
    );
    let not_reread = answer["recheck"]["scope"]["not_reread"]
        .as_array()
        .expect("not_reread");
    let inventory = not_reread
        .iter()
        .find(|entry| entry["service"] == "inventory")
        .unwrap_or_else(|| panic!("inventory is named as not re-read:\n{answer:#}"));
    assert_eq!(
        inventory["reason"],
        serde_json::json!("judged at your last re-check of it"),
        "{answer:#}"
    );
    assert!(
        answer["recheck"]["scope"]["stale"]
            .as_array()
            .is_none_or(|stale| stale.is_empty()),
        "{answer:#}"
    );
}

/// `check --recheck --no-rescan` is the probe the post-edit hook asks first:
/// what needs no re-scan is answered as before, and an edit that does comes
/// back `deferred` at once with the indexed rows, no re-scan started and no
/// miss remembered (B3-R).
#[test]
#[serial]
fn a_no_rescan_recheck_answers_at_once_and_starts_nothing() {
    let workspace = shop();
    let root = workspace.path();
    index(root);
    let temp = private_temp();
    let tmpdir = temp.path().to_str().expect("a UTF-8 temp path");
    let probe_env = [AMPLE[0], ("TMPDIR", tmpdir)];
    let probe = |file: &str| -> serde_json::Value {
        let text = run_with_env(
            root,
            &[
                "check",
                file,
                "--workspace",
                ".",
                "--recheck",
                "--no-rescan",
                "--json",
            ],
            &probe_env,
        );
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON: {e}\n{text}"))
    };

    // A comment-only edit reads the same: answered from the index, as B5 does.
    edit(&root.join(SHOP_HEALTH), "export", "// a note\nexport");
    let same = probe(SHOP_HEALTH);
    assert_eq!(
        same["recheck"]["ran"],
        serde_json::json!("none"),
        "{same:#}"
    );
    assert!(
        same["recheck"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("changed nothing the scan reads")),
        "{same:#}"
    );
    assert!(same["recheck"].get("deferred").is_none(), "{same:#}");
    // Put the file back, so the full re-check below judges a clean sibling.
    edit(&root.join(SHOP_HEALTH), "// a note\nexport", "export");

    // An edit that moves a type needs a re-scan: the probe starts none.
    // The verdict of each indexed row, as `check` reads it before the edit.
    let results = |answer: &serde_json::Value| -> Vec<serde_json::Value> {
        answer["items"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|item| item["verdict"]["result"].clone())
                    .collect()
            })
            .unwrap_or_default()
    };
    let indexed_results = results(&check_json(root, SHOP_CLIENT));
    edit(
        &root.join(SHOP_CLIENT),
        "activeCount: number",
        "activeCount: string",
    );
    let before = recheck_temp_dirs(temp.path());
    let deferred = probe(SHOP_CLIENT);
    assert_eq!(
        deferred["recheck"]["deferred"],
        serde_json::json!(true),
        "{deferred:#}"
    );
    assert_eq!(
        deferred["recheck"]["ran"],
        serde_json::json!("none"),
        "{deferred:#}"
    );
    assert!(
        deferred["recheck"]["scope"].is_null(),
        "no service was read: {deferred:#}"
    );
    assert_eq!(
        results(&deferred),
        indexed_results,
        "the verdicts are the indexed ones, not a re-check's:\n{deferred:#}"
    );
    assert_eq!(
        recheck_temp_dirs(temp.path()),
        before,
        "no staged generation was left"
    );
    for kept in ["recheck-misses.json", "working"] {
        assert!(
            !root.join(".carrick").join(kept).exists(),
            "the probe left .carrick/{kept}"
        );
    }

    // The same edit through a full re-check still re-scans and answers.
    let full = recheck_json(root, SHOP_CLIENT, &AMPLE);
    assert!(full["recheck"].get("deferred").is_none(), "{full:#}");
    assert_ne!(
        full["recheck"]["ran"],
        serde_json::json!("none"),
        "{full:#}"
    );
}

const SHOP_TEST: &str = "shop/inventory/src/__tests__/client.test.ts";
const SHOP_MOCK: &str = "shop/inventory/src/__mocks__/client.ts";

/// [`shop`] with a test file and a mock beside the client, committed with the
/// rest. Each loads the client and nothing a test did not already load.
fn shop_with_tests() -> tempfile::TempDir {
    workspace_prepared(SHOP, &["shop"], |repo| {
        let src = repo.join("inventory/src");
        for (dir, name) in [("__tests__", "client.test.ts"), ("__mocks__", "client.ts")] {
            std::fs::create_dir_all(src.join(dir)).expect("create the folder");
            std::fs::write(
                src.join(dir).join(name),
                "import { CatalogClient } from \"../client.js\";\n\nexport const made = new CatalogClient(\"http://catalog.test\");\n",
            )
            .expect("write the test file");
        }
    })
}

/// Append a statement to a test file that keeps what it imports.
fn add_a_case(file: &Path) {
    let existing = std::fs::read_to_string(file).expect("read the file");
    std::fs::write(file, format!("{existing}\nexport const again = made;\n"))
        .expect("write the case");
}

/// An edit to a test file that still loads what it loaded is read by no
/// service (the carry rule `--changed-only` uses), so the full re-check
/// answers it from the index and starts nothing; the probe defers it, as it
/// defers every edit that does not read the same. An edit that adds an import
/// is read, so it is deferred on the probe too.
#[test]
#[serial]
fn a_test_edit_that_keeps_its_imports_starts_no_re_scan() {
    let workspace = shop_with_tests();
    let root = workspace.path();
    index(root);
    let temp = private_temp();
    let tmpdir = temp.path().to_str().expect("a UTF-8 temp path");
    let probe = |file: &str| -> serde_json::Value {
        let text = run_with_env(
            root,
            &[
                "check",
                file,
                "--workspace",
                ".",
                "--recheck",
                "--no-rescan",
                "--json",
            ],
            &[AMPLE[0], ("TMPDIR", tmpdir)],
        );
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON: {e}\n{text}"))
    };
    let before = recheck_temp_dirs(temp.path());

    // (1) a case added to the test, imports kept: the full re-check answers
    // from the index.
    add_a_case(&root.join(SHOP_TEST));
    let kept = recheck_json(root, SHOP_TEST, &[AMPLE[0], ("TMPDIR", tmpdir)]);
    assert_eq!(
        kept["recheck"]["ran"],
        serde_json::json!("none"),
        "{kept:#}"
    );
    assert!(kept["recheck"].get("deferred").is_none(), "{kept:#}");
    assert!(
        kept["recheck"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("no service reads the file")),
        "{kept:#}"
    );
    assert_eq!(recheck_temp_dirs(temp.path()), before, "nothing was staged");

    // (3) the same edit to a file under `__mocks__`: the same answer.
    add_a_case(&root.join(SHOP_MOCK));
    let mock = recheck_json(root, SHOP_MOCK, &[AMPLE[0], ("TMPDIR", tmpdir)]);
    assert_eq!(
        mock["recheck"]["ran"],
        serde_json::json!("none"),
        "{mock:#}"
    );
    assert!(mock["recheck"].get("deferred").is_none(), "{mock:#}");

    // (2) an import added to the test: the edit changes what is loaded, so it
    // needs a re-scan the probe does not start.
    edit(
        &root.join(SHOP_TEST),
        "export const made",
        "import { send } from \"../send.js\";\nexport const sent = send;\nexport const made",
    );
    let imports = probe(SHOP_TEST);
    assert_eq!(
        imports["recheck"]["deferred"],
        serde_json::json!(true),
        "{imports:#}"
    );
}

/// A new test file that declares nothing program-wide and gives no source its
/// first importer is read by no service, so the full re-check answers from the
/// index and starts nothing. One that would make a source file test-only (it
/// is that file's first importer), declares a global, or is a script is read,
/// so the probe defers it.
#[test]
#[serial]
fn a_new_test_file_starts_no_re_scan() {
    let workspace = shop_with_tests();
    let root = workspace.path();
    index(root);
    let temp = private_temp();
    let tmpdir = temp.path().to_str().expect("a UTF-8 temp path");
    let probe = |file: &str| -> serde_json::Value {
        let text = run_with_env(
            root,
            &[
                "check",
                file,
                "--workspace",
                ".",
                "--recheck",
                "--no-rescan",
                "--json",
            ],
            &[AMPLE[0], ("TMPDIR", tmpdir)],
        );
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON: {e}\n{text}"))
    };
    let add = |name: &str, text: &str| -> String {
        let relative = format!("shop/inventory/src/__tests__/{name}");
        std::fs::write(root.join(&relative), text).expect("write the new test");
        relative
    };
    let before = recheck_temp_dirs(temp.path());

    // (1) it imports the client, which `inventory.ts` (production) imports.
    let kept = add(
        "second.test.ts",
        "import { CatalogClient } from \"../client.js\";\n\nexport const again = new CatalogClient(\"http://catalog.test\");\n",
    );
    let kept = recheck_json(root, &kept, &[AMPLE[0], ("TMPDIR", tmpdir)]);
    assert_eq!(
        kept["recheck"]["ran"],
        serde_json::json!("none"),
        "{kept:#}"
    );
    assert!(kept["recheck"].get("deferred").is_none(), "{kept:#}");
    assert_eq!(recheck_temp_dirs(temp.path()), before, "nothing was staged");

    // (2) it is the first importer of `inventory.ts`, which would turn
    // test-only.
    let orphan = add(
        "third.test.ts",
        "import { countActive } from \"../inventory.js\";\nexport const c = countActive;\n",
    );
    let orphan = probe(&orphan);
    assert_eq!(
        orphan["recheck"]["deferred"],
        serde_json::json!(true),
        "{orphan:#}"
    );

    // (3) it declares a global.
    let global = add(
        "fourth.test.ts",
        "declare global { var flagged: boolean }\nexport {};\n",
    );
    let global = probe(&global);
    assert_eq!(
        global["recheck"]["deferred"],
        serde_json::json!(true),
        "{global:#}"
    );

    // (4) it is a script: no import, no export.
    let script = add("fifth.test.ts", "var shared = 1;\n");
    let script = probe(&script);
    assert_eq!(
        script["recheck"]["deferred"],
        serde_json::json!(true),
        "{script:#}"
    );
}

/// The probe never walks every service's imports (carrick#4034): on a large
/// workspace that walk alone outlasted the hook's limit. An edit no service
/// reads comes back `deferred` like any other edit that does not read the
/// same, and the full re-check is where "no service reads it" is answered.
/// The walk states a `recheck-timing no-service-reads` line when it runs, so
/// this counts the work, not the time.
#[test]
#[serial]
fn the_probe_walks_no_service_imports() {
    let workspace = shop_with_tests();
    let root = workspace.path();
    index(root);
    let temp = private_temp();
    let tmpdir = temp.path().to_str().expect("a UTF-8 temp path");
    const WALK: &str = "recheck-timing no-service-reads";
    // The answer and the stderr of one `check`, with the timing lines on.
    let asked = |args: &[&str]| -> (serde_json::Value, String) {
        let mut command = Command::new(carrick());
        command
            .args(args)
            .current_dir(root)
            .env_remove("CARRICK_TOKEN")
            .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
            .env(AMPLE[0].0, AMPLE[0].1)
            .env("TMPDIR", tmpdir)
            .env("CARRICK_RECHECK_TIMING", "1");
        as_signed_in(&mut command, args);
        let output = command.output().expect("run check");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "carrick {args:?} exited {:?}:\n{stderr}",
            output.status.code()
        );
        let answer =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("not JSON: {e}\n{stdout}"));
        (answer, stderr)
    };
    let probe = |file: &str| {
        asked(&[
            "check",
            file,
            "--workspace",
            ".",
            "--recheck",
            "--no-rescan",
            "--json",
        ])
    };
    let full = |file: &str| asked(&["check", file, "--workspace", ".", "--recheck", "--json"]);

    // (1) A test edit that keeps its imports: no service reads it, and the
    // probe does not walk to learn that.
    add_a_case(&root.join(SHOP_TEST));
    let (kept, said) = probe(SHOP_TEST);
    assert_eq!(
        kept["recheck"]["deferred"],
        serde_json::json!(true),
        "{kept:#}"
    );
    assert!(
        !said.contains(WALK),
        "the probe walked every service's imports:\n{said}"
    );
    // The full re-check asks, and answers from the index without a re-scan.
    let (answered, said) = full(SHOP_TEST);
    assert!(
        said.contains(WALK),
        "the full re-check asks who reads the edit:\n{said}"
    );
    assert_eq!(
        answered["recheck"]["ran"],
        serde_json::json!("none"),
        "{answered:#}"
    );
    assert!(
        answered["recheck"].get("deferred").is_none(),
        "{answered:#}"
    );
    assert!(
        answered["recheck"]["reason"]
            .as_str()
            .is_some_and(|r| r.contains("no service reads the file")),
        "{answered:#}"
    );

    // (2) A source edit that needs a re-scan: deferred, and still no walk.
    edit(
        &root.join(SHOP_CLIENT),
        "activeCount: number",
        "activeCount: string",
    );
    let (deferred, said) = probe(SHOP_CLIENT);
    assert_eq!(
        deferred["recheck"]["deferred"],
        serde_json::json!(true),
        "{deferred:#}"
    );
    assert!(
        !said.contains(WALK),
        "the probe walked every service's imports:\n{said}"
    );
}

/// The [`shop`] repo one folder below its git top level: the workspace lists
/// `./mono/shop`, and the git repository is `mono`, which also holds a file
/// outside the repo. `mono` itself is excluded, so the workspace covers the
/// nested folder only.
fn shop_below_its_git_top() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let top = dir.path().join("mono");
    copy_tree(
        &repo_root().join("tests/fixtures").join(SHOP).join("shop"),
        &top.join("shop"),
    );
    std::fs::write(top.join("NOTES.txt"), "A file beside the nested repo.\n")
        .expect("write the note");
    git(&top, &["init", "-q", "."]);
    git(&top, &["add", "-A"]);
    git(
        &top,
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
    std::fs::write(
        dir.path().join("carrick-workspace.json"),
        "{ \"repos\": [\"./mono/shop\"], \"exclude\": [\"mono\"] }\n",
    )
    .expect("write the workspace file");
    dir
}

const NESTED_SHOP_CLIENT: &str = "mono/shop/inventory/src/client.ts";

/// carrick#4390: in a repo whose folder sits below its git top level, an edit
/// a service reads is still read. Neither the full re-check nor
/// `check --changed` answers "no service reads the file" for it. Failing
/// first: red until `recheck::no_service_reads` compares git's top-level
/// paths in the scan-relative form its caller keeps.
#[test]
#[serial]
fn an_edit_in_a_repo_below_its_git_top_is_read_by_its_service() {
    let edited = |workspace: &Path| {
        index(workspace);
        git(&workspace.join("mono"), &["checkout", "-qb", "feature"]);
        edit(
            &workspace.join(NESTED_SHOP_CLIENT),
            "activeCount: number",
            "activeCount: string",
        );
    };

    let workspace = shop_below_its_git_top();
    let root = workspace.path();
    edited(root);
    let temp = private_temp();
    let tmpdir = temp.path().to_str().expect("a UTF-8 temp path");
    let full = recheck_json(root, NESTED_SHOP_CLIENT, &[AMPLE[0], ("TMPDIR", tmpdir)]);

    let other = shop_below_its_git_top();
    edited(other.path());
    let (_, changed, stderr) = check_changed_json_with(other.path(), &[]);

    // Both answers are read before either is judged, so a failure names both.
    let both = format!("check --recheck:\n{full:#}\ncheck --changed:\n{changed:#}\n{stderr}");
    assert!(
        full["recheck"]["ran"].is_string(),
        "the re-check answered:\n{both}"
    );
    let rescan = rescan_of(&changed, "shop");
    assert!(rescan.is_some(), "check --changed read the repo:\n{both}");
    assert!(
        !(full["recheck"]["ran"] == serde_json::json!("none")
            && full["recheck"]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("no service reads the file"))),
        "the full re-check skipped an edit the inventory service reads:\n{both}"
    );
    assert_ne!(
        rescan.as_deref(),
        Some("not_needed"),
        "check --changed skipped an edit the inventory service reads:\n{both}"
    );
}

/// A test edit no service reads, beside a source edit the scan reads the same
/// (a comment), starts no re-scan in `check --changed`, and the answer is the
/// forced re-scan's. The test file is no changed file of its own (the scan
/// skips test sources), but it used to take the whole repo to a re-scan.
#[test]
#[serial]
fn check_changed_answers_a_test_edit_without_a_re_scan() {
    let workspace = shop_with_tests();
    let root = workspace.path();
    index(root);
    edit(&root.join(SHOP_HEALTH), "export", "// a note\nexport");
    add_a_case(&root.join(SHOP_TEST));

    let (code, json, stderr) = check_changed_json_with(root, &[]);
    assert_eq!(
        rescan_of(&json, "shop").as_deref(),
        Some("not_needed"),
        "{json:#}\n{stderr}"
    );
    assert!(
        !stderr.contains("re-reading"),
        "no re-scan started:\n{stderr}"
    );

    let (_, always, stderr) = check_changed_json_with(root, &[("CARRICK_CHECK_RESCAN", "always")]);
    assert_eq!(
        rescan_of(&always, "shop").as_deref(),
        Some("ran"),
        "{always:#}"
    );
    assert!(
        stderr.contains("re-reading"),
        "the switch re-scans:\n{stderr}"
    );
    // This machine is not signed in (carrick#3249): the forced run says so and
    // the answer from the index has no such line.
    let mut always = without_how(&always);
    always
        .as_object_mut()
        .expect("an object")
        .remove("not_checked");
    assert_eq!(without_how(&json), always, "the same answer, no re-scan");
    assert_eq!(code, Some(0), "{json:#}");
}

/// A re-check says how long each phase before, around and after its re-scan
/// took, when asked to, and says nothing more when not. The lines are what
/// names the cut that makes the pre-scan cost small, so each phase the cost
/// can sit in has one and the scan's start is named.
#[test]
#[serial]
fn a_re_check_states_how_long_each_phase_took_when_asked() {
    let workspace = shop();
    let root = workspace.path();
    index(root);
    edit(
        &root.join(SHOP_CLIENT),
        "activeCount: number",
        "activeCount: string",
    );
    let args = [
        "check",
        SHOP_CLIENT,
        "--workspace",
        ".",
        "--recheck",
        "--json",
    ];
    let run_it = |timing: bool| {
        let mut command = Command::new(carrick());
        command
            .args(args)
            .current_dir(root)
            .env_remove("CARRICK_TOKEN")
            .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
            .env("CARRICK_RECHECK_BUDGET_MS", "600000");
        as_signed_in(&mut command, &args);
        if timing {
            command.env("CARRICK_RECHECK_TIMING", "1");
        }
        let output = command.output().expect("run the re-check");
        String::from_utf8_lossy(&output.stderr).into_owned()
    };

    let quiet = run_it(false);
    assert!(
        !quiet.contains("recheck-timing"),
        "no timing lines unless asked:\n{quiet}"
    );
    let said = run_it(true);
    for phase in [
        "held-read",
        "plan-make/readers-of-the-edit",
        "plan-make/digests-and-working-reads",
        "stage/retained-and-hosted-blobs",
        "stage/hosted-and-previous-lists",
        "scan-starts",
        "scan ",
        "after-scan/read-fresh-and-keep",
        "after-scan/read-staged-lists",
        "after-scan/carried-and-kept-walks",
        "after-scan/write-local-blobs",
        "join ",
        "fold ",
    ] {
        assert!(
            said.contains(&format!("recheck-timing {phase}")),
            "a timing line for {phase}:\n{said}"
        );
    }
}

/// A service that imports the edited file is never judged unannounced. The
/// type-use record names only the operations that read a type the file
/// declares, so a constant the client imports is invisible to it; the import
/// closure names the reader, the scope calls it stale, and a service that
/// imports nothing keeps its plain reason (carrick#3198).
#[test]
#[serial]
fn a_re_check_names_a_service_that_reads_the_edited_file() {
    let workspace = workspace_prepared(SHOP, &["shop"], |repo| {
        std::fs::write(
            repo.join("catalog/app/shared.ts"),
            "export const widgetBase = \"/api/v1/widgets\";\n",
        )
        .expect("write the shared module");
        let client = repo.join("inventory/src/client.ts");
        edit(
            &client,
            "import { send } from \"./send.js\";",
            "import { send } from \"./send.js\";\nimport { widgetBase } from \"../../catalog/app/shared\";",
        );
        edit(
            &client,
            "/api/v1/widgets/${encoded}",
            "${widgetBase}/${encoded}",
        );
    });
    let root = workspace.path();
    index(root);

    let shared = "shop/catalog/app/shared.ts";
    edit(&root.join(shared), "/api/v1/widgets", "/api/v2/widgets");
    let answer = recheck_json(root, shared, &AMPLE);
    assert_eq!(reread(&answer), ["catalog"], "{answer:#}");
    let reason_of = |service: &str| -> String {
        answer["recheck"]["scope"]["not_reread"]
            .as_array()
            .and_then(|entries| entries.iter().find(|entry| entry["service"] == service))
            .and_then(|entry| entry["reason"].as_str())
            .unwrap_or_else(|| panic!("{service} is named as not re-read:\n{answer:#}"))
            .to_string()
    };
    assert!(
        reason_of("inventory").starts_with("reads catalog/app/shared.ts, edited since the index"),
        "{answer:#}"
    );
    assert_eq!(
        answer["recheck"]["scope"]["stale"],
        serde_json::json!(["inventory"]),
        "{answer:#}"
    );
    assert_eq!(
        reason_of("ledger"),
        "no edits since the index; judged at its indexed version",
        "ledger imports nothing, so the edit is none of its business:\n{answer:#}"
    );
}

/// The reason a re-check gives for a service it did not re-read.
fn not_reread_reason(answer: &serde_json::Value, service: &str) -> String {
    answer["recheck"]["scope"]["not_reread"]
        .as_array()
        .and_then(|entries| entries.iter().find(|entry| entry["service"] == service))
        .and_then(|entry| entry["reason"].as_str())
        .unwrap_or_else(|| panic!("{service} is named as not re-read:\n{answer:#}"))
        .to_string()
}

/// What `carrick init` writes at a repo root (agent skills, hook config, an
/// agents file) is read by no service, so it leaves every other service at its
/// indexed version with nothing to re-check. A root manifest is a repo input,
/// and still makes every other service stale, even when the walk of what each
/// service reads carries it (a bump of its `version` alone) (carrick#3465).
#[test]
#[serial]
fn a_re_check_leaves_other_services_alone_for_root_files_no_service_reads() {
    let workspace = workspace_prepared(SHOP, &["shop"], |repo| {
        std::fs::write(
            repo.join("package.json"),
            "{ \"name\": \"shop\", \"version\": \"1.0.0\", \"private\": true }\n",
        )
        .expect("write the root manifest");
    });
    let root = workspace.path();
    index(root);

    for (file, text) in [
        (
            "shop/.claude/skills/carrick-impact/SKILL.md",
            "# carrick-impact\n",
        ),
        ("shop/.codex/hooks.json", "{ \"hooks\": {} }\n"),
        ("shop/AGENTS.md", "# Agents\n"),
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("make the dir");
        std::fs::write(path, text).expect("write the root file");
    }
    statement_in(&root.join(SHOP_LEDGER), "ledger edit");
    let answer = recheck_json(root, SHOP_LEDGER, &AMPLE);
    assert_eq!(reread(&answer), ["ledger"], "{answer:#}");
    assert_eq!(
        answer["recheck"]["scope"]["stale"],
        serde_json::json!([]),
        "{answer:#}"
    );
    for service in ["catalog", "inventory"] {
        assert_eq!(
            not_reread_reason(&answer, service),
            "no edits since the index; judged at its indexed version",
            "{answer:#}"
        );
    }

    edit(
        &root.join("shop/package.json"),
        "\"version\": \"1.0.0\"",
        "\"version\": \"1.0.1\"",
    );
    let answer = recheck_json(root, SHOP_LEDGER, &AMPLE);
    assert_eq!(reread(&answer), ["ledger"], "{answer:#}");
    assert_eq!(
        answer["recheck"]["scope"]["stale"],
        serde_json::json!(["catalog", "inventory"]),
        "{answer:#}"
    );
}

/// A root config one service reads by name (its tsconfig extends it) still
/// counts for that service, and only for it (carrick#3465).
#[test]
#[serial]
fn a_re_check_names_the_service_whose_tsconfig_extends_an_edited_root_config() {
    let workspace = workspace_prepared(SHOP, &["shop"], |repo| {
        std::fs::write(
            repo.join("tsconfig.base.json"),
            "{ \"compilerOptions\": { \"strict\": true } }\n",
        )
        .expect("write the root tsconfig");
        std::fs::write(
            repo.join("catalog/tsconfig.json"),
            "{ \"extends\": \"../tsconfig.base.json\" }\n",
        )
        .expect("write the catalog tsconfig");
    });
    let root = workspace.path();
    index(root);

    edit(
        &root.join("shop/tsconfig.base.json"),
        "\"strict\": true",
        "\"strict\": false",
    );
    statement_in(&root.join(SHOP_LEDGER), "ledger edit");
    let answer = recheck_json(root, SHOP_LEDGER, &AMPLE);
    assert_eq!(reread(&answer), ["ledger"], "{answer:#}");
    assert_eq!(
        answer["recheck"]["scope"]["stale"],
        serde_json::json!(["catalog"]),
        "{answer:#}"
    );
    assert_eq!(
        not_reread_reason(&answer, "inventory"),
        "no edits since the index; judged at its indexed version",
        "{answer:#}"
    );
}

/// A miss in one service leaves the other services of the repo re-checked, and
/// a new index tries the missed one again.
#[test]
#[serial]
fn a_miss_in_one_service_leaves_the_others_re_checked() {
    let workspace = shop();
    let root = workspace.path();
    index(root);
    statement_in(&root.join(SHOP_ROUTE), "catalog edit one");
    statement_in(&root.join(SHOP_LEDGER), "ledger edit");

    let missed = recheck_json(root, SHOP_ROUTE, &[("CARRICK_RECHECK_BUDGET_MS", "1")]);
    miss_ran_under_an_ample_budget(root);
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

    // Another service of the same repo is still re-checked.
    let ledger = recheck_json(root, SHOP_LEDGER, &AMPLE);
    assert_ne!(
        ledger["recheck"]["ran"],
        serde_json::json!("none"),
        "one service's miss does not silence the repo:\n{ledger:#}"
    );
    assert_eq!(reread(&ledger), ["ledger"], "{ledger:#}");

    // The missed service starts no re-scan, and the reason names it.
    statement_in(&root.join(SHOP_ROUTE), "catalog edit two");
    let skipped = recheck_json(root, SHOP_ROUTE, &AMPLE);
    assert_eq!(
        skipped["recheck"]["ran"],
        serde_json::json!("none"),
        "{skipped:#}"
    );
    assert_eq!(skipped["recheck"]["known_miss"], serde_json::json!(true));
    assert!(
        skipped["recheck"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("catalog") && reason.contains("check --changed")),
        "the reason names the service and the fresh read:\n{skipped:#}"
    );

    // A new index is a new generation: catalog is re-checked again.
    std::thread::sleep(Duration::from_millis(1100));
    index(root);
    statement_in(&root.join(SHOP_ROUTE), "catalog edit three");
    let fresh = recheck_json(root, SHOP_ROUTE, &AMPLE);
    assert_ne!(
        fresh["recheck"]["ran"],
        serde_json::json!("none"),
        "{fresh:#}"
    );
}

/// `check --each` reads the memory per file: after a service misses, its other
/// files answer at once with the reason while another service's file is still
/// re-checked.
#[test]
#[serial]
fn check_each_answers_a_missed_services_other_files_at_once() {
    let workspace = shop();
    let root = workspace.path();
    index(root);
    statement_in(&root.join(SHOP_ROUTE), "catalog route");
    statement_in(&root.join(SHOP_HEALTH), "catalog health");
    statement_in(&root.join(SHOP_LEDGER), "ledger");

    let missed = recheck_json(root, SHOP_ROUTE, &[("CARRICK_RECHECK_BUDGET_MS", "1")]);
    miss_ran_under_an_ample_budget(root);
    assert_eq!(missed["recheck"]["ran"], serde_json::json!("none"));

    let text = run_with_env(
        root,
        &[
            "check",
            "--each",
            SHOP_HEALTH,
            SHOP_LEDGER,
            "--workspace",
            ".",
            "--recheck",
            "--json",
        ],
        &AMPLE,
    );
    let answers: Vec<serde_json::Value> =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("not a JSON array: {e}\n{text}"));
    assert_eq!(answers.len(), 2, "{text}");
    assert_eq!(answers[0]["recheck"]["known_miss"], serde_json::json!(true));
    assert!(
        answers[0]["recheck"]["elapsed_ms"].as_u64().unwrap() < 1000,
        "the missed service's other file answers at once:\n{:#}",
        answers[0]
    );
    assert_ne!(
        answers[1]["recheck"]["ran"],
        serde_json::json!("none"),
        "the other service's file is re-checked:\n{:#}",
        answers[1]
    );
}

/// The type checker's answers are kept per index, and a re-check of an edit
/// that moves no type reads the index's own answer instead of judging again.
///
/// Proved by planting: the one answer `refresh` kept is rewritten to say the
/// pair is unverifiable, and a comment-only edit's re-check then reports that
/// verdict, so it can only have come from the kept file. The folder still
/// holds that one key afterwards, which also shows the capture's stubs are
/// byte-stable across a re-scan.
///
/// The join judges here with its settle off (`CARRICK_JOIN_SETTLE=off`): in
/// a one-repo workspace the join otherwise takes every pair's outcome from the
/// scan's stored record and judges none (carrick#3884), so it keeps no answer.
/// The kept answers still serve every pair the join judges.
#[test]
#[serial]
fn a_type_neutral_edit_reuses_the_indexs_type_check_answer() {
    let workspace = shop();
    let root = workspace.path();
    let judging_join = [("CARRICK_JOIN_SETTLE", "off")];
    run_with_env(root, &["refresh", "--workspace", "."], &judging_join);

    let kept = |root: &Path| -> Vec<PathBuf> {
        let mut found = Vec::new();
        let Ok(generations) = std::fs::read_dir(root.join(".carrick/judged")) else {
            return found;
        };
        for generation in generations.filter_map(Result::ok) {
            for answer in std::fs::read_dir(generation.path())
                .expect("read a generation")
                .filter_map(Result::ok)
            {
                found.push(answer.path());
            }
        }
        found
    };
    let before = kept(root);
    assert_eq!(
        before.len(),
        1,
        "`refresh` keeps the one answer its join reached: {before:?}"
    );
    let text = std::fs::read_to_string(&before[0]).expect("read the kept answer");
    assert!(text.contains("\"bucket\":\"compatible\""), "{text}");
    std::fs::write(
        &before[0],
        text.replace("\"bucket\":\"compatible\"", "\"bucket\":\"unverifiable\""),
    )
    .expect("plant the answer");

    statement_in(&root.join(SHOP_ROUTE), "moves no type");
    let answer = recheck_json(root, SHOP_ROUTE, &[AMPLE[0], judging_join[0]]);
    assert_eq!(
        answer["recheck"]["ran"],
        serde_json::json!("extraction+types")
    );
    assert_eq!(
        answer["items"][0]["verdict"]["state"],
        serde_json::json!("unresolved"),
        "the verdict is the kept answer's, not a new judgement:\n{answer:#}"
    );
    assert_eq!(kept(root), before, "no second key: the stubs did not move");
}

/// The consumer's `Widget` type moves into its own file, which `client.ts`
/// imports. Committed before the index is built, so the edit that follows is
/// to a file holding no route and no call.
fn move_the_consumers_type_to_its_own_file(inventory: &Path) {
    let client = inventory.join("src/client.ts");
    edit(
        &client,
        "export type Widget = {\n  id: string;\n  name: string;\n  activeCount: number;\n};\n",
        "import type { Widget } from \"./types.js\";\n",
    );
    std::fs::write(
        inventory.join("src/types.ts"),
        "export type Widget = {\n  id: string;\n  name: string;\n  activeCount: number;\n};\n",
    )
    .expect("write types.ts");
    commit_all(inventory, "move the type");
}

/// The problem rows of a `--json` answer that sit in a file ending `suffix`.
fn problems_in(json: &serde_json::Value, suffix: &str) -> Vec<serde_json::Value> {
    json["problems"]
        .as_array()
        .expect("problems")
        .iter()
        .filter(|p| p["file"].as_str().is_some_and(|f| f.ends_with(suffix)))
        .cloned()
        .collect()
}

/// A type edited in a file that holds no route and no call breaks the call
/// that reads it (carrick#2998): the check reports the call in the untouched
/// file, marked as outside the change, and fails the run.
#[test]
#[serial]
fn a_shared_type_edit_fails_the_rows_it_breaks() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    move_the_consumers_type_to_its_own_file(&root.join("inventory-svc"));
    index(root);
    edit(
        &root.join("inventory-svc/src/types.ts"),
        "activeCount: number",
        "activeCount: string",
    );

    let (code, body) = check_changed(root, true);
    let json: serde_json::Value = serde_json::from_str(&body).expect("--json is JSON");
    assert_eq!(code, Some(1), "{json:#}");
    let call = problems_in(&json, "src/client.ts")
        .into_iter()
        .find(|p| p["role"] == "call" && p["kind"] == "incompatible")
        .unwrap_or_else(|| panic!("the call in client.ts is reported:\n{json:#}"));
    assert_eq!(call["outside_the_change"], true, "{call:#}");
    assert_eq!(call["fails"], true, "{call:#}");
    assert!(json["outside_moved"].as_u64() >= Some(1), "{json:#}");
    assert_eq!(json["routes"], 0, "the counts stay about the edited file");
    assert_eq!(json["calls"], 0, "the counts stay about the edited file");

    let (_, text) = check_changed(root, false);
    assert!(
        text.contains(" outside the changed files whose verdict this change moved"),
        "{text}"
    );
    assert!(
        text.contains("[outside the changed files: this change moved its verdict]"),
        "{text}"
    );
}

/// A comment is not a type change: nothing outside the edited file moves.
#[test]
#[serial]
fn a_comment_in_a_shared_type_file_moves_nothing_outside_it() {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    move_the_consumers_type_to_its_own_file(&root.join("inventory-svc"));
    index(root);
    edit(
        &root.join("inventory-svc/src/types.ts"),
        "export type Widget",
        "// what the catalog sends\nexport type Widget",
    );

    let (code, body) = check_changed(root, true);
    let json: serde_json::Value = serde_json::from_str(&body).expect("--json is JSON");
    assert_eq!(code, Some(0), "{json:#}");
    assert_eq!(json["outside_moved"], 0, "{json:#}");
}

/// The environment a check or a refresh needs to walk a repo's services and
/// read the sibling a service calls into, with the models mocked and the
/// store's label for the default library coming from the repo's cassette.
fn walking_env(repo: &Path) -> Vec<(String, String)> {
    let sidecar = repo_root().join("src/sidecar");
    vec![
        ("CARRICK_MOCK_ALL".into(), "1".into()),
        (
            "CARRICK_MOCK_FIXTURE_DIR".into(),
            format!("{}/", repo.join("__llm__").display()),
        ),
        ("CARRICK_SKIP_INTENTS".into(), "1".into()),
        ("CARRICK_ALLOW_UNPREPARED".into(), "1".into()),
        ("CARRICK_NO_UPDATE_CHECK".into(), "1".into()),
        ("CARRICK_SIDECAR_DIR".into(), sidecar.display().to_string()),
    ]
}

/// A file's identity and write time: changed by every copy that replaces it.
fn identity(path: &Path) -> (u64, std::time::SystemTime) {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    (meta.ino(), meta.modified().expect("a write time"))
}

/// The `__client` rows file in `dir`, if there is one.
fn client_rows(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .map(|entry| entry.unwrap().path())
        .find(|path| path.to_string_lossy().ends_with("__client.jsonl.gz"))
}

/// carrick#3245: a check that had to walk a service it carries keeps the
/// walk, so the next check reuses it. The index here stands for a hosted one
/// (its kept rows are gone), the app is edited, and the client it reads is
/// carried; the first check walks the client and keeps the rows with the
/// record of the commit; the second leaves them alone; and rows whose build
/// stamp, commit or file hash no longer holds are walked and kept again.
#[test]
#[serial]
fn check_changed_keeps_a_carried_walk_for_the_next_check() {
    if !repo_root().join("src/sidecar/dist/src/index.js").exists() {
        assert!(
            std::env::var("CI").is_err(),
            "the built sidecar is missing in CI"
        );
        eprintln!("Skipping: the built sidecar is missing");
        return;
    }
    let workspace = workspace("discovery-siblings", &["repo"]);
    let root = workspace.path();
    let env = walking_env(&root.join("repo"));
    let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    run_with_env(root, &["refresh", "--workspace", "."], &env_refs);
    let positions = root.join(".carrick/positions");
    assert!(
        client_rows(&positions).is_some(),
        "the refresh kept the client's rows"
    );
    // A hosted index has no kept rows.
    std::fs::remove_dir_all(&positions).expect("remove the kept rows");

    let main = root.join("repo/services/app/src/main.ts");
    let mut text = std::fs::read_to_string(&main).expect("read the app");
    text.push_str("\nexport function more(): number {\n  return 1;\n}\n");
    std::fs::write(&main, text).expect("edit the app");
    let check = || {
        let output = Command::new(carrick())
            .args(["check", "--changed", "--workspace", "."])
            .current_dir(root)
            .env_remove("CARRICK_TOKEN")
            .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
            .env("CARRICK_CHECK_BUDGET_MS", "600000")
            .envs(env_refs.iter().copied())
            .output()
            .expect("spawn carrick");
        assert!(
            output.status.code().is_some_and(|code| code <= 1),
            "check failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };

    check();
    let rows = client_rows(&positions).expect("the first check kept the client's rows");
    let record = {
        let mut name = rows.clone().into_os_string();
        name.push(".at");
        PathBuf::from(name)
    };
    let text =
        std::fs::read_to_string(&record).expect("and the record of what they were walked from");
    let lines: Vec<&str> = text.lines().collect();
    let head = git_head(&root.join("repo"));
    assert_eq!(lines.len(), 2, "{text:?}");
    assert_eq!(
        lines[1], head,
        "the record names the commit it was carried at"
    );
    assert!(
        std::fs::read_dir(&positions).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("__app")),
        "an analysed service's rows are not kept"
    );

    check();
    let kept = identity(&rows);
    check();
    assert_eq!(identity(&rows), kept, "the next check reused the walk");

    // Rows that no longer hold are walked again and kept again.
    let honest = std::fs::read_to_string(&record).unwrap();
    let tampered: [String; 2] = [
        format!("0{honest}"),
        honest.replacen(&head, &"0".repeat(head.len()), 1),
    ];
    for bad in tampered {
        std::fs::write(&record, &bad).unwrap();
        let before = identity(&rows);
        check();
        assert_ne!(identity(&rows), before, "refused rows were walked again");
        assert_eq!(std::fs::read_to_string(&record).unwrap(), honest);
    }
}

fn git_head(repo: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// carrick#3249: a machine that is not signed in cannot ask the library
/// store, so a route registered through a server package does not exist
/// locally. `check --changed` must not read that as a pass.
///
/// The fixture's one route is registered through a package whose claims the
/// store cassette holds. The index is written with the store answering (the
/// mock stands for a signed-in machine). Then the route's path is renamed on a
/// branch and the check runs twice: with no credential, which is a real
/// `oidc_unavailable` before any request leaves the process, and with the
/// store answering, which behaves as before.
#[test]
#[serial]
fn check_changed_says_when_the_library_store_cannot_be_asked() {
    let workspace = workspace("receiver-trace", &["repo"]);
    let root = workspace.path();
    let cassettes = root.join("cassettes");
    std::fs::create_dir_all(cassettes.join("library-claims")).unwrap();
    std::fs::copy(
        repo_root().join("tests/fixtures/receiver-trace/library-claims.json"),
        cassettes.join("library-claims/default.json"),
    )
    .unwrap();
    let cassettes = format!("{}/", cassettes.display());
    let signed_in = |args: &[&str]| {
        let output = Command::new(carrick())
            .args(args)
            .current_dir(root)
            .env_remove("CARRICK_TOKEN")
            .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
            .env("CARRICK_CHECK_BUDGET_MS", "600000")
            .env("CARRICK_MOCK_ALL", "1")
            .env("CARRICK_MOCK_FIXTURE_DIR", &cassettes)
            .output()
            .unwrap_or_else(|e| panic!("carrick {args:?}: {e}"));
        (
            output.status.code(),
            String::from_utf8(output.stdout).expect("stdout was not UTF-8"),
        )
    };
    let repo = root.join("repo");
    // One root service, found as `carrick init` finds it.
    git(&repo, &["rm", "-q", "carrick.json"]);
    commit_all(&repo, "no config");
    git(&repo, &["branch", "-M", "main"]);
    let (code, _) = signed_in(&["refresh", "--workspace", "."]);
    assert_eq!(code, Some(0));

    git(&repo, &["checkout", "-qb", "feature"]);
    edit(&repo.join("src/routes.ts"), "\"/c\"", "\"/d\"");
    commit_all(&repo, "rename the route");

    let unsigned = |args: &[&str]| {
        let output = Command::new(carrick())
            .args(args)
            .current_dir(root)
            .env_remove("CARRICK_TOKEN")
            .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
            .env("CARRICK_CHECK_BUDGET_MS", "600000")
            .env("CARRICK_RECHECK_BUDGET_MS", "600000")
            .output()
            .unwrap_or_else(|e| panic!("carrick {args:?}: {e}"));
        (
            output.status.code(),
            String::from_utf8(output.stdout).expect("stdout was not UTF-8"),
        )
    };
    let (code, text) = unsigned(&["check", "--changed", "--workspace", "."]);
    assert_eq!(code, Some(2), "not signed in is not a pass:\n{text}");
    assert_eq!(
        text,
        "carrick check: not checked: library facts unavailable: this machine is not signed \
         in. Run carrick login.\n"
    );
    let (_, body) = unsigned(&["check", "--changed", "--workspace", ".", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&body).expect("--json is JSON");
    assert_eq!(
        json["not_checked"],
        "library facts unavailable: this machine is not signed in. Run carrick login.",
        "{json:#}"
    );

    let (code, text) = unsigned(&[
        "check",
        "repo/src/routes.ts",
        "--recheck",
        "--workspace",
        ".",
        "--json",
    ]);
    let json: serde_json::Value = serde_json::from_str(&text).expect("--json is JSON");
    assert_eq!(json["recheck"]["ran"], "none", "{json:#}");
    assert_eq!(
        json["recheck"]["reason"],
        "library facts unavailable: this machine is not signed in. Run carrick login.",
        "{json:#}"
    );
    assert_eq!(
        code,
        Some(2),
        "a re-check that read nothing exits 2:\n{text}"
    );

    let (_, text) = signed_in(&["check", "--changed", "--workspace", "."]);
    assert!(
        text.starts_with("carrick check: checked 1 route") && !text.contains("not signed in"),
        "a signed-in run reads the route through the store's claims:\n{text}"
    );

    // Signed in, and the store cannot be reached: the same refusal to read a
    // pass, with its own cause. The fixture directory stands in for the dead
    // network with a message for the store's route.
    let down = root.join("down");
    std::fs::create_dir_all(down.join("library-claims")).unwrap();
    std::fs::write(
        down.join("library-claims/unreachable.txt"),
        "network is down",
    )
    .unwrap();
    let offline = |args: &[&str]| {
        let output = Command::new(carrick())
            .args(args)
            .current_dir(root)
            .env_remove("CARRICK_TOKEN")
            .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
            .env("CARRICK_CHECK_BUDGET_MS", "600000")
            .env("CARRICK_RECHECK_BUDGET_MS", "600000")
            .env("CARRICK_MOCK_ALL", "1")
            .env("CARRICK_MOCK_FIXTURE_DIR", format!("{}/", down.display()))
            .output()
            .unwrap_or_else(|e| panic!("carrick {args:?}: {e}"));
        (
            output.status.code(),
            String::from_utf8(output.stdout).expect("stdout was not UTF-8"),
        )
    };
    let (code, text) = offline(&["check", "--changed", "--workspace", "."]);
    assert_eq!(code, Some(2), "an unreachable store is not a pass:\n{text}");
    assert_eq!(
        text,
        "carrick check: not checked: library facts unavailable: Carrick's library store could \
         not be reached.\n"
    );
    let (code, text) = offline(&[
        "check",
        "repo/src/routes.ts",
        "--recheck",
        "--workspace",
        ".",
        "--json",
    ]);
    let json: serde_json::Value = serde_json::from_str(&text).expect("--json is JSON");
    assert_eq!(
        json["recheck"]["reason"],
        "library facts unavailable: Carrick's library store could not be reached.",
        "{json:#}"
    );
    assert_eq!(code, Some(2), "{text}");
}

// ---------------------------------------------------------------------------
// carrick#3192: a call whose handler moved, kept alive only by documentation,
// reaches no route. The routes package states its operations twice: the
// handlers in `widgets.ts`, and a handler-less table in `openapi.ts` that the
// scan folds onto them. Move a handler's path and the table row has no
// partner, stays a route, and keeps the call in `web` matched.
// ---------------------------------------------------------------------------

const DOC_WIDGETS: &str = "repo/packages/routes/src/widgets.ts";
const DOC_TABLE: &str = "repo/packages/routes/src/openapi.ts";

/// A command in the documented-route workspace with the library store
/// replayed from the fixture's cassette: the re-scan `check --changed` runs
/// reads the handlers as facts only when the store answers it too.
fn documented_command(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(carrick());
    command
        .args(args)
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_CHECK_BUDGET_MS", "600000")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", root.join(".test-cassettes").display()),
        );
    command
}

/// `check --changed --json` in the documented-route workspace.
fn documented_check(root: &Path) -> (Option<i32>, String) {
    let output = documented_command(root, &["check", "--changed", "--workspace", ".", "--json"])
        .output()
        .expect("spawn carrick check");
    (
        output.status.code(),
        String::from_utf8(output.stdout).expect("stdout was not UTF-8"),
    )
}

/// The fixture's repo, committed and indexed with the library store replayed
/// from its cassette and no model, so the handlers are read as facts.
fn documented_route_workspace() -> tempfile::TempDir {
    let workspace = workspace("documented-route-moved", &["repo"]);
    let root = workspace.path();
    let cassettes = root.join(".test-cassettes");
    std::fs::create_dir_all(cassettes.join("library-claims")).expect("cassette dir");
    std::fs::copy(
        repo_root().join("tests/fixtures/documented-route-moved/library-claims.json"),
        cassettes.join("library-claims/default.json"),
    )
    .expect("copy the store cassette");
    let output = documented_command(root, &["refresh", "--workspace", "."])
        .output()
        .expect("spawn carrick refresh");
    assert!(
        output.status.success(),
        "refresh failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    workspace
}

/// The problems `check --changed --json` names, as `file:line reason`.
fn problems_of(json: &str) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(json)
        .unwrap_or_else(|e| panic!("check --changed --json was not JSON: {e}\n{json}"));
    value["problems"]
        .as_array()
        .map(|problems| {
            problems
                .iter()
                .map(|p| {
                    format!(
                        "{}:{} {} fails={} unconfirmed={} {}",
                        p["file"].as_str().unwrap_or_default(),
                        p["line"],
                        p["kind"].as_str().unwrap_or_default(),
                        p["fails"],
                        p["unconfirmed"],
                        p["reason"].as_str().unwrap_or_default()
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
#[serial]
fn check_changed_names_a_call_only_documentation_still_serves_after_its_handler_moved() {
    let workspace = documented_route_workspace();
    let root = workspace.path();
    edit(&root.join(DOC_WIDGETS), "'/widgets'", "'/gadgets'");

    let (code, text) = documented_check(root);
    let problems = problems_of(&text);
    let named: Vec<&String> = problems
        .iter()
        .filter(|p| p.starts_with("repo/packages/web/src/client.ts:") && p.contains("fails=true"))
        .collect();
    assert_eq!(named.len(), 1, "{problems:#?}");
    assert!(
        named[0].contains("widgets.ts") && named[0].contains("openapi.ts"),
        "the reason names both places: {}",
        named[0]
    );
    assert_eq!(code, Some(1), "{text}");
}

/// The known positive: with the table gone the same rename reaches no route
/// through the unmatched loop already, which proves the harness can fail.
#[test]
#[serial]
fn check_changed_fails_the_moved_handler_with_no_documentation() {
    let workspace = documented_route_workspace();
    let root = workspace.path();
    std::fs::remove_file(root.join(DOC_TABLE)).expect("delete the table");
    edit(&root.join(DOC_WIDGETS), "'/widgets'", "'/gadgets'");

    let (code, text) = documented_check(root);
    assert!(
        problems_of(&text)
            .iter()
            .any(|p| p.starts_with("repo/packages/web/src/client.ts:") && p.contains("fails=true")),
        "{text}"
    );
    assert_eq!(code, Some(1), "{text}");
}

/// An edit to the table alone moves no handler: nothing new is named.
#[test]
#[serial]
fn check_changed_does_not_name_a_call_when_only_the_documentation_changed() {
    let workspace = documented_route_workspace();
    let root = workspace.path();
    edit(&root.join(DOC_TABLE), "List widgets", "List every widget");

    let (code, text) = documented_check(root);
    assert!(
        !problems_of(&text)
            .iter()
            .any(|p| p.starts_with("repo/packages/web/src/client.ts:")),
        "{text}"
    );
    assert_ne!(code, Some(1), "{text}");
}

/// The handler's line moved as well, so the re-read cannot say it is the same
/// handler: a candidate, which does not fail.
#[test]
#[serial]
fn check_changed_lists_a_moved_handler_it_cannot_find_again_as_a_candidate() {
    let workspace = documented_route_workspace();
    let root = workspace.path();
    let widgets = root.join(DOC_WIDGETS);
    edit(
        &widgets,
        "export async function widgetRoutes",
        "// A note.\n// Another.\nexport async function widgetRoutes",
    );
    edit(&widgets, "'/widgets'", "'/gadgets'");

    let (code, text) = documented_check(root);
    let problems = problems_of(&text);
    let named: Vec<&String> = problems
        .iter()
        .filter(|p| p.starts_with("repo/packages/web/src/client.ts:"))
        .collect();
    assert!(
        named
            .iter()
            .all(|p| p.contains("fails=false") && p.contains("unconfirmed=true")),
        "{problems:#?}"
    );
    assert_ne!(code, Some(1), "{text}");
}

/// A route the model stated and the re-read does not carry is no fact of this
/// work: the fixture of that case has no failing problem on the untouched
/// consumer.
#[test]
#[serial]
fn check_changed_does_not_fail_a_model_row_that_was_not_carried_over_documentation() {
    let workspace = carry_workspace();
    let root = workspace.path();
    edit(
        &root.join(CARRY_ROUTES),
        "'/items/:id/status'",
        "'/items/:id/state'",
    );

    let (_, text) = check_changed(root, true);
    assert!(
        !problems_of(&text)
            .iter()
            .any(|p| p.starts_with("items-web/") && p.contains("fails=true")),
        "{text}"
    );
}

/// One repo, three services: `contracts` holds a type and no route or call,
/// `api` serves a route whose handler returns it through a helper, and
/// `client` keeps its own copy of the type and calls the route.
///
/// `linked` writes the link a pnpm install makes for a workspace dependency:
/// `services/api/node_modules/@acme/contracts` pointing at the package, with
/// `node_modules` ignored by git. Without it the tree is a bare checkout, and
/// the route's import of `@acme/contracts` resolves for no compiler (carrick#3261).
fn shared_package_repo(root: &Path, linked: bool) -> PathBuf {
    let repo = root.join("monorepo");
    let write = |relative: &str, contents: &str| {
        let path = repo.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    };
    write(
        "carrick.json",
        "{ \"services\": [\n  { \"name\": \"contracts\", \"directory\": \"packages/contracts\" },\n  { \"name\": \"api\", \"directory\": \"services/api\" },\n  { \"name\": \"client\", \"directory\": \"services/client\" }\n] }",
    );
    write(
        "package.json",
        "{ \"name\": \"monorepo\", \"private\": true, \"workspaces\": [\"packages/*\", \"services/*\"] }",
    );
    write(
        "pnpm-workspace.yaml",
        "packages:\n  - \"packages/*\"\n  - \"services/*\"\n",
    );
    write(".gitignore", "node_modules\n");
    write(
        "tsconfig.base.json",
        "{ \"compilerOptions\": { \"strict\": true, \"target\": \"es2022\", \"module\": \"esnext\", \"moduleResolution\": \"bundler\", \"skipLibCheck\": true } }",
    );
    for service in ["services/api", "services/client"] {
        write(
            &format!("{service}/tsconfig.json"),
            "{ \"extends\": \"../../tsconfig.base.json\", \"include\": [\"app\", \"src\"] }",
        );
    }
    write(
        "packages/contracts/package.json",
        "{ \"name\": \"@acme/contracts\", \"version\": \"0.0.0\", \"main\": \"src/index.ts\" }",
    );
    write(
        "packages/contracts/src/index.ts",
        "export type WidgetDto = { id: string; activeCount: number };\n",
    );
    write(
        "services/api/package.json",
        "{ \"name\": \"api\", \"private\": true, \"dependencies\": { \"@remix-run/node\": \"^2.0.0\", \"@acme/contracts\": \"*\" } }",
    );
    write(
        "services/api/app/routes/api.v1.widgets.$widgetId.ts",
        "import type { WidgetDto } from \"@acme/contracts\";\n\n\
         function build(id: string): WidgetDto {\n  return { id, activeCount: 3 };\n}\n\n\
         export async function loader({\n  params,\n}: {\n  params: { widgetId: string };\n}): Promise<WidgetDto> {\n  return build(String(params.widgetId));\n}\n",
    );
    write(
        "services/client/package.json",
        "{ \"name\": \"client\", \"private\": true, \"dependencies\": { \"undici\": \"^6.0.0\" } }",
    );
    write(
        "services/client/src/send.ts",
        "export type RequestOptions = {\n  method: string;\n  headers?: Record<string, string>;\n};\n\n\
         export async function send<T>(url: string, options: RequestOptions): Promise<T> {\n  const response = await fetch(url, options);\n  return (await response.json()) as T;\n}\n",
    );
    write(
        "services/client/src/inventory.ts",
        "import type { CatalogClient } from \"./client.js\";\n\n\
         export async function countActive(\n  widgetId: string,\n  client: CatalogClient\n): Promise<number> {\n  const widget = await client.readWidget(widgetId);\n  return widget.activeCount;\n}\n",
    );
    write(
        "services/client/src/client.ts",
        "import { send } from \"./send.js\";\n\n\
         export type WidgetDto = { id: string; activeCount: number };\n\n\
         export class CatalogClient {\n  constructor(private readonly baseUrl: string) {}\n\n\
         \x20 readWidget(widgetId: string): Promise<WidgetDto> {\n    const encoded = encodeURIComponent(widgetId);\n    return send<WidgetDto>(`${this.baseUrl}/api/v1/widgets/${encoded}`, {\n      method: \"GET\",\n    });\n  }\n}\n",
    );
    if linked {
        let scope = repo.join("services/api/node_modules/@acme");
        std::fs::create_dir_all(&scope).expect("the package scope directory");
        std::os::unix::fs::symlink("../../../../packages/contracts", scope.join("contracts"))
            .expect("link the workspace package");
    }
    std::fs::write(
        root.join("carrick-workspace.json"),
        "{ \"repos\": [\"./monorepo\"] }\n",
    )
    .expect("write the workspace file");
    git(&repo, &["init", "-q", "."]);
    commit_all(&repo, "fixture");
    repo
}

const SHARED_PACKAGE_ROUTE: &str = "monorepo/services/api/app/routes/api.v1.widgets.$widgetId.ts";
const SHARED_PACKAGE_CALL: &str = "monorepo/services/client/src/client.ts";
const SHARED_PACKAGE_TYPE: &str = "monorepo/packages/contracts/src/index.ts";

/// The customer shape of carrick#2998: the edit is in a package holding no
/// route and no call, and the pair it breaks is a route and a call in other
/// services, neither in a changed file.
#[test]
#[serial]
fn a_shared_package_edit_fails_the_pair_it_breaks() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    shared_package_repo(root, true);
    index(root);

    let (code, body) = check_files(root, &[SHARED_PACKAGE_ROUTE, SHARED_PACKAGE_CALL, "--json"]);
    let pair: serde_json::Value = serde_json::from_str(&body).expect("--json is JSON");
    assert_eq!(
        code,
        Some(0),
        "the fixture's pair must type before the edit means anything (carrick#3261):\n{pair:#}"
    );
    assert_eq!(pair["type_checked"], 2, "{pair:#}");
    assert!(
        pair["not_type_checked"]
            .as_array()
            .is_none_or(|rows| rows.is_empty()),
        "{pair:#}"
    );

    edit(
        &root.join(SHARED_PACKAGE_TYPE),
        "activeCount: number",
        "activeCount: string",
    );
    let (code, body) = check_changed(root, true);
    let json: serde_json::Value = serde_json::from_str(&body).expect("--json is JSON");
    assert_eq!(code, Some(1), "{json:#}");
    for (suffix, role) in [
        ("api.v1.widgets.$widgetId.ts", "route"),
        ("src/client.ts", "call"),
    ] {
        let row = problems_in(&json, suffix)
            .into_iter()
            .find(|p| p["role"] == role && p["kind"] == "incompatible")
            .unwrap_or_else(|| panic!("the {role} in {suffix} is reported:\n{json:#}"));
        assert_eq!(row["outside_the_change"], true, "{row:#}");
        assert_eq!(row["fails"], true, "{row:#}");
    }
    assert!(json["outside_moved"].as_u64() >= Some(2), "{json:#}");
    assert_eq!(json["routes"], 0, "the counts stay about the edited file");
    assert_eq!(json["calls"], 0, "the counts stay about the edited file");
}

/// A comment in the shared package is not a type change: nothing outside it
/// moves, even when the repo is re-read.
#[test]
#[serial]
fn a_comment_in_a_shared_package_moves_nothing_outside_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    shared_package_repo(root, true);
    index(root);

    // The premise, as in the edit test: the pair types before the comment, so
    // "nothing moved" is not also true of a pair that never types.
    let (code, body) = check_files(root, &[SHARED_PACKAGE_ROUTE, SHARED_PACKAGE_CALL, "--json"]);
    let pair: serde_json::Value = serde_json::from_str(&body).expect("--json is JSON");
    assert_eq!(
        code,
        Some(0),
        "the fixture's pair must type before the comment means anything (carrick#3261):\n{pair:#}"
    );
    assert_eq!(pair["type_checked"], 2, "{pair:#}");
    assert!(
        pair["not_type_checked"]
            .as_array()
            .is_none_or(|rows| rows.is_empty()),
        "{pair:#}"
    );

    edit(
        &root.join(SHARED_PACKAGE_TYPE),
        "export type",
        "// the widget\nexport type",
    );

    let (code, json, _) = check_changed_json_with(
        root,
        &[
            ("CARRICK_MOCK_ALL", "1"),
            ("CARRICK_CHECK_RESCAN", "always"),
        ],
    );
    assert_eq!(code, Some(0), "{json:#}");
    assert_eq!(json["outside_moved"], 0, "{json:#}");
    assert!(
        json["problems"]
            .as_array()
            .is_none_or(|rows| rows.is_empty()),
        "{json:#}"
    );
    assert_eq!(
        rescan_of(&json, "monorepo").as_deref(),
        Some("ran"),
        "{json:#}"
    );
}

/// The same repo without the install's link is a bare checkout: the pair is
/// named not type-checked, never called compatible (carrick#3261).
#[test]
#[serial]
fn a_shared_package_with_no_install_is_named_not_type_checked() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    shared_package_repo(root, false);
    index(root);

    let (code, body) = check_files(root, &[SHARED_PACKAGE_ROUTE, SHARED_PACKAGE_CALL, "--json"]);
    let json: serde_json::Value = serde_json::from_str(&body).expect("--json is JSON");
    assert_eq!(code, Some(2), "{json:#}");
    assert_eq!(json["type_checked"], 0, "{json:#}");
    assert!(
        json["problems"]
            .as_array()
            .is_none_or(|rows| rows.is_empty()),
        "{json:#}"
    );
    let rows = json["not_type_checked"]
        .as_array()
        .expect("not_type_checked");
    assert_eq!(rows.len(), 2, "{json:#}");
    let text = json["not_type_checked"].to_string();
    assert!(
        text.contains("api.v1.widgets.$widgetId.ts") && text.contains("client.ts"),
        "{json:#}"
    );
}

// --- carrick#1049: a re-check names the rows outside the edited file that the
// edit moved ---

fn moved_of(answer: &serde_json::Value) -> Vec<serde_json::Value> {
    answer["recheck"]["moved"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Renaming a route in its own file leaves its callers, in another service,
/// reaching no route. The re-check of the route file names them.
#[test]
#[serial]
fn a_route_rename_names_the_callers_it_strands() {
    let workspace = shop();
    let root = workspace.path();
    index(root);

    // A flat route's path is its file name.
    let renamed = "shop/catalog/app/routes/api.v2.widgets.$widgetId.ts";
    std::fs::rename(root.join(SHOP_ROUTE), root.join(renamed)).expect("rename the route file");
    let answer = recheck_json(root, renamed, &AMPLE);
    assert_ne!(
        answer["recheck"]["ran"],
        serde_json::json!("none"),
        "{answer:#}"
    );
    let moved = moved_of(&answer);
    let stranded = moved
        .iter()
        .find(|row| row["after"] == "no_route")
        .unwrap_or_else(|| panic!("the inventory call is named:\n{answer:#}"));
    assert_eq!(stranded["role"], serde_json::json!("call"), "{answer:#}");
    assert_eq!(
        stranded["file"],
        serde_json::json!("shop/inventory/src/client.ts"),
        "{answer:#}"
    );
    assert!(stranded["line"].as_u64().is_some(), "{answer:#}");
    assert_eq!(stranded["before"], serde_json::json!("matched"));
}

/// The shop gains a call to a route nobody serves, committed with the fixture
/// so it is not a change since the index. The index is then rewritten to hold
/// a pairing the fresh read does not produce: the call matched to the widgets
/// route, a file nothing has touched (carrick#3411).
fn shop_whose_index_pairs_a_gadgets_call_with_an_untouched_route() -> tempfile::TempDir {
    let workspace = workspace_prepared(SHOP, &["shop"], |repo| {
        let client = repo.join("inventory/src/client.ts");
        edit(
            &client,
            "  readWidget(widgetId: string)",
            "  readGadget(gadgetId: string): Promise<Widget> {\n    return send<Widget>(`${this.baseUrl}/api/v1/gadgets/${gadgetId}`, {\n      method: \"GET\",\n    });\n  }\n\n  readWidget(widgetId: string)",
        );
        let caller = repo.join("inventory/src/inventory.ts");
        let mut text = std::fs::read_to_string(&caller).expect("read the caller");
        text.push_str(
            "\nexport async function gadgetName(\n  gadgetId: string,\n  client: CatalogClient\n): Promise<string> {\n  const gadget = await client.readGadget(gadgetId);\n  return gadget.name;\n}\n",
        );
        std::fs::write(&caller, text).expect("write the caller");
    });
    let root = workspace.path();
    index(root);
    let path = root.join(".carrick/index.json");
    let mut index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read the index"))
            .expect("the index is JSON");
    let route_file = "catalog/app/routes/api.v1.widgets.$widgetId.ts";
    let route_line = index["repos"][0]["files"][route_file]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["kind"] == "route"))
        .and_then(|row| row["line"].as_u64())
        .unwrap_or_else(|| panic!("the widgets route is indexed:\n{index:#}"));
    let gadgets = index["repos"][0]["files"]
        .as_object_mut()
        .expect("the files")
        .values_mut()
        .flat_map(|rows| rows.as_array_mut().expect("rows").iter_mut())
        .find(|row| {
            row["path"]
                .as_str()
                .is_some_and(|p| p.contains("/gadgets/"))
        })
        .expect("the gadgets call is a row");
    gadgets["counterparts"] = serde_json::json!([{
        "role": "producer", "service": "catalog", "file": route_file, "line": route_line
    }]);
    std::fs::write(&path, index.to_string()).expect("write the index");
    workspace
}

/// carrick#3411: a call the index paired with a route in a file nothing has
/// touched, that the re-scan no longer pairs, is re-scan drift. A new file
/// elsewhere did not take that route away, so the re-check does not name the
/// call.
#[test]
#[serial]
fn a_new_file_names_no_call_an_untouched_producers_rescan_left_with_no_route() {
    let workspace = shop_whose_index_pairs_a_gadgets_call_with_an_untouched_route();
    let root = workspace.path();

    let util = "shop/catalog/app/util.ts";
    std::fs::write(root.join(util), "export const unrelated = 1;\n").expect("write the util");
    let answer = recheck_json(root, util, &AMPLE);
    assert_ne!(
        answer["recheck"]["ran"],
        serde_json::json!("none"),
        "{answer:#}"
    );
    let stranded: Vec<_> = moved_of(&answer)
        .into_iter()
        .filter(|row| row["after"] == "no_route")
        .collect();
    assert!(stranded.is_empty(), "{stranded:?}\n{answer:#}");
}

/// The same drift through `check --changed`: no problem row for the call, and
/// the run does not fail.
#[test]
#[serial]
fn check_changed_does_not_name_a_call_an_untouched_producers_rescan_left_with_no_route() {
    let workspace = shop_whose_index_pairs_a_gadgets_call_with_an_untouched_route();
    let root = workspace.path();

    std::fs::write(
        root.join("shop/catalog/app/util.ts"),
        "export const unrelated = 1;\n",
    )
    .expect("write the util");
    let (code, json, _) = check_changed_json_with(
        root,
        &[
            ("CARRICK_MOCK_ALL", "1"),
            ("CARRICK_CHECK_RESCAN", "always"),
        ],
    );
    let named: Vec<_> = json["problems"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| {
            row["file"]
                .as_str()
                .is_some_and(|f| f.ends_with("client.ts"))
        })
        .collect();
    assert!(named.is_empty(), "{named:?}\n{json:#}");
    assert_ne!(code, Some(1), "{json:#}");
}

/// The carry fixture with its handler-less route descriptors removed before
/// the index, so only the handlers in `routes.ts` serve the web's calls, and
/// its mount prefix then edited in `server.ts`: the routes in `routes.ts`, a
/// file the work did not touch, are read at the same lines under another
/// path (carrick#3412).
fn carry_workspace_with_a_moved_mount() -> tempfile::TempDir {
    let workspace = carry_workspace_with(|root| {
        std::fs::remove_file(root.join("items-api/src/openapi.ts")).expect("remove the docs");
    });
    edit(
        &workspace.path().join("items-api/src/server.ts"),
        "prefix: '/api/v1'",
        "prefix: '/api/v2'",
    );
    workspace
}

/// The rows in the web's client, whose calls the moved routes served.
fn web_client_rows<'a>(
    rows: impl IntoIterator<Item = &'a serde_json::Value>,
) -> Vec<&'a serde_json::Value> {
    rows.into_iter()
        .filter(|row| row["file"] == "items-web/src/client.ts")
        .collect()
}

/// A mount prefix edit in `server.ts` moves the routes `routes.ts` declares.
/// That file is unchanged, yet the work took the web's routes away, so
/// `check --changed` names the calls.
#[test]
#[serial]
fn check_changed_names_the_calls_a_mount_prefix_edit_strands() {
    let workspace = carry_workspace_with_a_moved_mount();
    let root = workspace.path();

    let (_, json) = carry_check_changed(root);
    let problems = json["problems"].as_array().into_iter().flatten();
    let stranded = web_client_rows(problems);
    assert!(
        !stranded.is_empty(),
        "the client calls are named:\n{json:#}"
    );
    for row in stranded {
        assert_eq!(row["kind"], "no_route", "{json:#}");
        assert_eq!(row["outside_the_change"], true, "{json:#}");
    }
}

/// The same edit through the per-edit re-check of `server.ts`.
#[test]
#[serial]
fn a_recheck_of_a_mount_prefix_edit_names_the_calls_it_strands() {
    let workspace = carry_workspace_with_a_moved_mount();
    let root = workspace.path();

    let answer = recheck_json(root, "items-api/src/server.ts", &AMPLE);
    assert_ne!(
        answer["recheck"]["ran"],
        serde_json::json!("none"),
        "{answer:#}"
    );
    let moved = moved_of(&answer);
    let stranded = web_client_rows(&moved);
    assert!(
        !stranded.is_empty(),
        "the client calls are named:\n{answer:#}"
    );
    for row in stranded {
        assert_eq!(row["before"], "matched", "{answer:#}");
        assert_eq!(row["after"], "no_route", "{answer:#}");
    }
}

/// The shop with `Widget` moved out of the route file into `types.ts`.
fn shop_with_widget_in_a_types_file() -> tempfile::TempDir {
    workspace_prepared(SHOP, &["shop"], |repo| {
        let route = repo.join("catalog/app/routes/api.v1.widgets.$widgetId.ts");
        let text = std::fs::read_to_string(&route).expect("read the route");
        let (types, rest) = text
            .split_once("export type Widget = {")
            .expect("the route declares Widget");
        let (body, rest) = rest.split_once("};\n").expect("the type ends");
        let _ = types;
        std::fs::write(
            repo.join("catalog/app/types.ts"),
            format!("export type Widget = {{{body}}};\n"),
        )
        .expect("write the types file");
        std::fs::write(
            &route,
            format!(
                "import type {{ Widget }} from \"../types\";\nexport type {{ Widget }};\n{rest}"
            ),
        )
        .expect("rewrite the route");
    })
}

/// A type moved out of the route file into a types file: editing the type
/// moves the route's verdict against its consumer, and the re-check of the
/// types file (which holds no row of its own) names the route.
#[test]
#[serial]
fn a_types_file_edit_names_the_route_and_the_consumer_it_breaks() {
    let workspace = shop_with_widget_in_a_types_file();
    let root = workspace.path();
    index(root);

    let types = "shop/catalog/app/types.ts";
    edit(
        &root.join(types),
        "activeCount: number",
        "activeCount: string",
    );
    let answer = recheck_json(root, types, &AMPLE);
    let moved = moved_of(&answer);
    let route = moved
        .iter()
        .find(|row| row["role"] == "route")
        .unwrap_or_else(|| panic!("the route is named:\n{answer:#}"));
    assert_eq!(
        route["after"],
        serde_json::json!("type_mismatch"),
        "{answer:#}"
    );
    assert_eq!(
        route["before"],
        serde_json::json!("compatible"),
        "{answer:#}"
    );
    assert!(
        route["counterparts"]
            .as_array()
            .is_some_and(|names| names.iter().any(|n| n
                .as_str()
                .is_some_and(|n| n.contains("inventory") && n.contains("client.ts")))),
        "the consumer it breaks is named:\n{answer:#}"
    );
}

/// The index was written by another carrick build, so a verdict that moved
/// may have moved because of the build and not the edit: the row says so.
#[test]
#[serial]
fn a_moved_row_of_another_builds_index_says_the_build_may_be_the_cause() {
    let workspace = shop_with_widget_in_a_types_file();
    let root = workspace.path();
    index(root);
    as_written_by_an_older_scanner(root);

    let types = "shop/catalog/app/types.ts";
    edit(
        &root.join(types),
        "activeCount: number",
        "activeCount: string",
    );
    let answer = recheck_json(root, types, &AMPLE);
    let moved = moved_of(&answer);
    let route = moved
        .iter()
        .find(|row| row["role"] == "route")
        .unwrap_or_else(|| panic!("the route is named:\n{answer:#}"));
    assert_eq!(route["other_build"], serde_json::json!(true), "{answer:#}");
}

/// The same edit against an index this build wrote carries no `other_build`.
#[test]
#[serial]
fn a_moved_row_of_this_builds_index_carries_no_other_build() {
    let workspace = shop_with_widget_in_a_types_file();
    let root = workspace.path();
    index(root);

    let types = "shop/catalog/app/types.ts";
    edit(
        &root.join(types),
        "activeCount: number",
        "activeCount: string",
    );
    let answer = recheck_json(root, types, &AMPLE);
    let route = moved_of(&answer)
        .into_iter()
        .find(|row| row["role"] == "route")
        .unwrap_or_else(|| panic!("the route is named:\n{answer:#}"));
    assert!(route.get("other_build").is_none(), "{answer:#}");
}

/// An edit the scan reads differently that moves no type and no route moves nothing.
#[test]
#[serial]
fn a_type_neutral_edit_moves_no_row() {
    let workspace = shop();
    let root = workspace.path();
    index(root);
    statement_in(&root.join(SHOP_ROUTE), "no contract change");
    let answer = recheck_json(root, SHOP_ROUTE, &AMPLE);
    assert_ne!(
        answer["recheck"]["ran"],
        serde_json::json!("none"),
        "{answer:#}"
    );
    assert!(moved_of(&answer).is_empty(), "{answer:#}");
}

/// Every kept judge answer under `.carrick/judged`, with its size and last
/// write time: a file written again changes its time.
fn kept_judge_answers(
    root: &Path,
) -> std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime)> {
    fn walk(
        dir: &Path,
        out: &mut std::collections::BTreeMap<PathBuf, (u64, std::time::SystemTime)>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "json") {
                let meta = entry.metadata().expect("kept answer metadata");
                out.insert(path, (meta.len(), meta.modified().expect("mtime")));
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(&root.join(".carrick/judged"), &mut out);
    out
}

/// A `check --changed` answer without the time it took.
fn without_time(mut json: serde_json::Value) -> serde_json::Value {
    json["elapsed_ms"] = serde_json::Value::Null;
    json
}

/// The workspace with a contract field dropped from the producer, uncommitted,
/// and an index built before the edit.
fn workspace_with_a_dropped_field() -> tempfile::TempDir {
    let workspace = workspace("local-mode-workspace", &["catalog-web", "inventory-svc"]);
    let root = workspace.path();
    index(root);
    let route = root.join("catalog-web/app/routes/api.v1.widgets.$widgetId.ts");
    edit(&route, "  activeCount: number;\n", "");
    edit(&route, ", activeCount: 3", "");
    workspace
}

/// A second identical `check --changed` takes the type checker's answer the
/// first one kept: nothing is judged or written again, and the answer is the
/// one a cold judge gives (verdicts, exit code and JSON identical), the planted
/// break included.
#[test]
#[serial]
fn a_repeated_check_reuses_the_kept_judge_answer_and_says_what_a_cold_judge_says() {
    let workspace = workspace_with_a_dropped_field();
    let root = workspace.path();
    let before = kept_judge_answers(root);

    let (code_first, first, _) = check_changed_json_with(root, &[("CARRICK_MOCK_ALL", "1")]);
    let kept = kept_judge_answers(root);
    assert!(
        kept.len() > before.len(),
        "the first check kept the answer it judged under .carrick/judged:\n{before:#?}\n{kept:#?}"
    );
    let (code_second, second, _) = check_changed_json_with(root, &[("CARRICK_MOCK_ALL", "1")]);
    assert_eq!(
        kept_judge_answers(root),
        kept,
        "the second check read the kept answer and wrote nothing"
    );
    assert_eq!(code_second, code_first);
    assert_eq!(without_time(second.clone()), without_time(first.clone()));

    // A cold judge: no kept answer anywhere.
    std::fs::remove_dir_all(root.join(".carrick/judged")).expect("clear the kept answers");
    let (code_cold, cold, _) = check_changed_json_with(root, &[("CARRICK_MOCK_ALL", "1")]);
    assert_eq!(code_cold, code_first);
    assert_eq!(without_time(cold), without_time(second.clone()));
    assert!(
        second["problems"]
            .as_array()
            .is_some_and(|problems| problems.iter().any(|p| p["fails"] == true)),
        "the planted break fails the check, kept answer or not:\n{second:#}"
    );
}

/// The parent writes no log, so `CARRICK_RECHECK_TIMING` is where its seconds
/// are read: a line for each step of `check --changed`.
#[test]
#[serial]
fn check_changed_times_each_step_of_its_parent() {
    let workspace = workspace_with_a_dropped_field();
    let (_, _, stderr) = check_changed_json_with(
        workspace.path(),
        &[("CARRICK_MOCK_ALL", "1"), ("CARRICK_RECHECK_TIMING", "1")],
    );
    for step in [
        "check/read-index",
        "check/load-workspace",
        "check/changed-files",
        "check/reads-the-same",
        "held-read",
        "stage/retained-and-hosted-blobs",
        "check/rejoin",
        "check/judge",
        "check/remap-lines",
        "check/unread",
    ] {
        assert!(
            stderr.contains(&format!("recheck-timing {step} took ")),
            "no timing line for {step}:\n{stderr}"
        );
    }
}
