//! Does the child scan's retype reach what `carrick check --changed` prints?
//! ($0, no model, no cloud.)
//!
//! `check --changed` re-scans the changed repo in a child process, which
//! judges the pairs and retypes the consumer calls the judge left to the
//! consumer, then joins the workspace in a second process. Since carrick#3884
//! the join takes each pair's outcome the child stored on its consumer's blob
//! instead of judging the pair again, so the retype's verdict is what prints.
//!
//! The HTTP cases run the same edit twice on two identical workspaces, once
//! as shipped and once with the test-only switch (`CARRICK_TEST_SKIP_RETYPE`)
//! that makes the retype return before it sends a call, and compare what the
//! check leaves. The consumer's client returns `any`, so the judge has no
//! consumer type and leaves the pair to the consumer's reads.
//!
//! The socket, GraphQL and pub/sub cases run with the join's settle on and
//! `off` (`CARRICK_JOIN_SETTLE`): the judge decides those pairs and the
//! retype is HTTP-only, so nothing they print may move.

/// The switch in `retype_consumer_calls` (`src/engine/type_compat_v2.rs`).
const SKIP_RETYPE_FOR_TEST_ENV: &str = "CARRICK_TEST_SKIP_RETYPE";

use serde_json::Value;
use serial_test::serial;
use std::path::{Path, PathBuf};
use std::process::Command;

fn carrick() -> &'static str {
    env!("CARGO_BIN_EXE_carrick")
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn sidecar_built() -> bool {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/sidecar/dist/src/index.js")
        .exists()
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

/// Commit at a fixed time, so the base commit both runs print is the same.
fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn edit(file: &Path, from: &str, to: &str) {
    let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
    assert!(
        text.contains(from),
        "the fixture no longer contains {from:?}, so this edit proves nothing:\n{text}"
    );
    std::fs::write(file, text.replace(from, to)).expect("write the edit");
}

/// One repo with two services, as a workspace of one repo, committed.
fn workspace(populate: impl Fn(&Path)) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = dir.path().join("mono");
    populate(&repo);
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["add", "-A"]);
    git(
        &repo,
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
        "{ \"repos\": [\"./mono\"] }\n",
    )
    .expect("write the workspace file");
    dir
}

/// `api` serves GET /api/v1/widgets/:widgetId (a flat-route module); `web`
/// calls it through a client whose response type defaults to `any`, so the
/// judge leaves the pair to the consumer and the retype check decides it.
fn http_workspace() -> tempfile::TempDir {
    workspace(|repo| copy_tree(&fixtures().join("retype-reach-mono"), repo))
}

/// A socket pair across two services of one repo, from the cross-repo
/// corpus: `payments` emits `payment:settled`, `frontend` listens. Both
/// sides state their payload types, so the judge reads the pair itself.
fn socket_workspace() -> tempfile::TempDir {
    workspace(|repo| {
        let corpus = fixtures().join("xrepo-corpus-1");
        copy_tree(&corpus.join("payments-svc"), &repo.join("apps/payments"));
        copy_tree(&corpus.join("web-frontend"), &repo.join("apps/frontend"));
        for service in ["payments", "frontend"] {
            for stale in ["carrick.json", "expected.json"] {
                let _ = std::fs::remove_file(repo.join("apps").join(service).join(stale));
            }
        }
        std::fs::write(
            repo.join("carrick.json"),
            "{ \"services\": [ { \"name\": \"payments\", \"directory\": \"apps/payments\" }, \
             { \"name\": \"frontend\", \"directory\": \"apps/frontend\" } ] }\n",
        )
        .expect("write carrick.json");
    })
}

/// Everything one `check --changed` leaves that a reader could depend on.
struct Observed {
    text: String,
    text_exit: Option<i32>,
    json: String,
    json_exit: Option<i32>,
    /// What the index stores about the pairs: the judge's answers and each
    /// service's kept verdicts and pair outcomes.
    stored: String,
    /// The child scan's own log lines about its retype.
    retype_ran: bool,
    retype_decided: Vec<String>,
    /// The child's `scope: <service> carried and written` lines: a carried
    /// consumer whose pair outcomes moved is written where the join reads it.
    carried_written: Vec<String>,
    /// The whole log of the check's processes, for the cases that read more.
    log: String,
}

/// The join's settle switch (`src/engine/mod.rs`, carrick#3884): `off` judges
/// every pair in the join as before.
const JOIN_SETTLE_ENV: &str = "CARRICK_JOIN_SETTLE";

fn command(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Command {
    let mut command = Command::new(carrick());
    command
        .args(args)
        .arg("--workspace")
        .arg(root)
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env_remove("CARRICK_CHECK_RESCAN")
        .env_remove(SKIP_RETYPE_FOR_TEST_ENV)
        .env_remove(JOIN_SETTLE_ENV)
        // Each run logs under its own home, so a run's log lines are its own.
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_CHECK_BUDGET_MS", "600000");
    command.envs(env.iter().copied());
    command
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn files_under(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == extension) {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The index's stored judgments, with only the fields that state a verdict.
fn stored_verdicts(root: &Path) -> String {
    let mut stored = Vec::new();
    for path in files_under(&root.join(".carrick/judged"), "json") {
        let answer = read_json(&path);
        for verdict in answer["verdicts"].as_array().into_iter().flatten() {
            stored.push(serde_json::json!({
                "judged": [verdict["pair_key"], verdict["bucket"], verdict["gate"]],
            }));
        }
    }
    for path in files_under(&root.join(".carrick/repos"), "json") {
        let blob = read_json(&path);
        stored.push(serde_json::json!({
            "service": blob["service_name"],
            "compat": blob["compat_verdicts"],
            "outcomes": blob["pair_outcomes"]["outcomes"].as_array().map(|all| {
                all.iter()
                    .map(|o| serde_json::json!([o["pair_key"], o["bucket"], o["resolved"], o["notes"]]))
                    .collect::<Vec<_>>()
            }),
        }));
    }
    stored.sort_by_key(|value| value.to_string());
    serde_json::to_string_pretty(&stored).expect("serialise")
}

fn without_root(text: &str, root: &Path) -> String {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    text.replace(&canonical.display().to_string(), "<root>")
        .replace(&root.display().to_string(), "<root>")
}

/// Index the workspace as shipped, apply the edit, and run `check --changed`
/// twice (as text and as JSON), with or without the retype.
fn observe(
    root: &Path,
    edit_it: impl Fn(&Path),
    refresh_env: &[(&str, &str)],
    check_env: &[(&str, &str)],
) -> Observed {
    let indexed = command(root, &["refresh"], refresh_env)
        .output()
        .expect("spawn refresh");
    assert!(
        indexed.status.success(),
        "refresh: {}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    // The index's own scan retypes too; the log read below is the check's.
    let _ = std::fs::remove_dir_all(root.join("home/.carrick/logs"));
    edit_it(&root.join("mono"));

    let text = command(root, &["check", "--changed"], check_env)
        .output()
        .expect("spawn check --changed");
    let json = command(root, &["check", "--changed", "--json"], check_env)
        .output()
        .expect("spawn check --changed --json");

    let mut payload: Value = serde_json::from_slice(&json.stdout)
        .unwrap_or_else(|e| panic!("not JSON: {e}\n{}", String::from_utf8_lossy(&json.stdout)));
    payload
        .as_object_mut()
        .expect("an object")
        .remove("elapsed_ms");

    let mut log = String::new();
    for path in files_under(&root.join("home/.carrick/logs"), "log") {
        log.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
    }
    Observed {
        text: without_root(&String::from_utf8_lossy(&text.stdout), root),
        text_exit: text.status.code(),
        json: without_root(
            &serde_json::to_string_pretty(&payload).expect("serialise"),
            root,
        ),
        json_exit: json.status.code(),
        stored: without_root(&stored_verdicts(root), root),
        // "Prescreened ...'s consumer calls" runs before the retype and is not
        // the retype. "Retyping ..." is logged only when the calls split over
        // more than one request; "Retyped ..." is logged whenever the retype
        // ran (a call the retype abstains at before any rewrite, as at a
        // generic wrapper, leaves a single request).
        retype_ran: log
            .lines()
            .any(|line| line.contains(" Retyped ") && line.contains("'s consumer calls: ")),
        retype_decided: log
            .lines()
            .filter(|line| line.contains("Pair verdict:") && line.contains("gate=retype:"))
            .map(|line| line[line.find("Pair verdict:").unwrap_or(0)..].to_string())
            .collect(),
        carried_written: log
            .lines()
            .filter(|line| line.contains("carried and written at this commit"))
            .map(|line| line[line.find("scope: ").unwrap_or(0)..].to_string())
            .collect(),
        log,
    }
}

/// Run the same edit on two identical workspaces, as shipped and with the
/// retype off, and return both.
fn both(make: fn() -> tempfile::TempDir, edit_it: impl Fn(&Path) + Copy) -> (Observed, Observed) {
    both_with_env(make, edit_it, &[])
}

/// `both`, with an environment every command of both runs shares.
fn both_with_env(
    make: fn() -> tempfile::TempDir,
    edit_it: impl Fn(&Path) + Copy,
    env: &[(&str, &str)],
) -> (Observed, Observed) {
    let shipped_root = make();
    let shipped = observe(shipped_root.path(), edit_it, env, env);
    let skipped_root = make();
    let mut skip_env = env.to_vec();
    skip_env.push((SKIP_RETYPE_FOR_TEST_ENV, "1"));
    let skipped = observe(skipped_root.path(), edit_it, env, &skip_env);
    (shipped, skipped)
}

/// A mock fixture dir holding the library store's answer for `socket.io` and
/// `socket.io-client` as the parity test's cassettes give it for the corpus's
/// two socket repos. Socket rows are the fact layer's alone (carrick#3854), so
/// a refresh with no claims states none and no socket pair forms.
fn socket_claims_cassette() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut claims = Vec::new();
    for repo in ["payments-svc", "web-frontend"] {
        let path = fixtures()
            .join("socket-claims-xrepo/corpus-1")
            .join(repo)
            .join("__llm__/library-claims/default.json");
        let answer = read_json(&path);
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

/// Run the same edit on two identical workspaces, with the join's settle on
/// (as shipped) and `off`, every command of each, and return both.
fn settle_on_off(
    make: fn() -> tempfile::TempDir,
    edit_it: impl Fn(&Path) + Copy,
    env: &[(&str, &str)],
) -> (Observed, Observed) {
    let on_root = make();
    let on = observe(on_root.path(), edit_it, env, env);
    let off_root = make();
    let mut off_env = env.to_vec();
    off_env.push((JOIN_SETTLE_ENV, "off"));
    let off = observe(off_root.path(), edit_it, &off_env, &off_env);
    (on, off)
}

fn assert_unread(shipped: &Observed, skipped: &Observed) {
    assert_eq!(shipped.text, skipped.text, "the printed check");
    assert_eq!(shipped.text_exit, skipped.text_exit, "the exit code");
    assert_eq!(shipped.json, skipped.json, "the printed JSON");
    assert_eq!(shipped.json_exit, skipped.json_exit, "the JSON exit code");
    assert_eq!(shipped.stored, skipped.stored, "the stored verdicts");
}

/// The join's `join settle: S of N pairs taken ...` lines (carrick#3884).
fn join_settled_counts(log: &str) -> Vec<(usize, usize)> {
    log.lines()
        .filter_map(|line| {
            let rest = &line[line.find("join settle: ")? + "join settle: ".len()..];
            let mut words = rest.split_whitespace();
            let settled = words.next()?.parse().ok()?;
            let total = words.nth(1)?.parse().ok()?;
            Some((settled, total))
        })
        .collect()
}

/// The producer's response loses a field the consumer reads. The child's
/// retype decides this pair as a break, and the join takes the child's
/// stored outcome instead of judging the pair again (carrick#3884), so the
/// check prints the break and fails. With the retype off, the child leaves
/// the pair to the consumer and the check cannot say.
#[test]
#[serial]
fn http_producer_edit_retype_is_read_by_check_changed() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let producer_edit = |repo: &Path| {
        let route = repo.join("apps/api/app/routes/api.v1.widgets.$widgetId.ts");
        edit(&route, "activeCount: number", "active: number");
        edit(&route, "activeCount: 3", "active: 3");
    };
    let (shipped, skipped) = both(http_workspace, producer_edit);

    assert!(shipped.retype_ran, "the child retypes the consumer's calls");
    assert!(
        shipped
            .retype_decided
            .iter()
            .any(|line| line.contains("incompatible resolved=true")),
        "the child's retype decides the pair as a break: {:#?}",
        shipped.retype_decided
    );
    assert!(!skipped.retype_ran, "the switch turns the retype off");
    assert!(
        !skipped
            .retype_decided
            .iter()
            .any(|line| line.contains("gate=retype:")),
        "{:#?}",
        skipped.retype_decided
    );
    // Step 0 (carrick#3884): the child writes the consumer whose outcome the
    // retype moved, so its record is on the blob the join reads.
    assert!(
        shipped
            .carried_written
            .iter()
            .any(|line| line.starts_with("scope: web ")),
        "the child writes web's blob with the retype's outcome: {:#?}",
        shipped.carried_written
    );
    // The stored hash survives the write and the read-back: the join settles.
    let counts = join_settled_counts(&shipped.log);
    assert!(
        counts.iter().any(|(settled, _)| *settled >= 1),
        "the join takes at least one stored outcome: {counts:?}\n{}",
        shipped
            .log
            .lines()
            .filter(|line| line.contains("join settle"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    assert!(
        shipped.text.lines().any(|line| {
            line.starts_with("mono/apps/web/src/inventory.ts:7 ")
                && line.contains("  incompatible  ")
                && line.contains("'activeCount' does not exist")
        }),
        "the check prints the consumer's break the edit made:\n{}",
        shipped.text
    );
    assert_eq!(shipped.text_exit, Some(1), "and fails:\n{}", shipped.text);
    assert_eq!(shipped.json_exit, Some(1), "the JSON run fails too");
    assert_ne!(
        shipped.text, skipped.text,
        "the printed check reads the retype"
    );
    assert_ne!(
        shipped.json, skipped.json,
        "the printed JSON reads the retype"
    );
    assert_ne!(
        shipped.text_exit, skipped.text_exit,
        "the exit reads the retype"
    );
    assert_eq!(shipped.stored, skipped.stored, "the index is not written");
}

/// The consumer reads a field the producer never had. The check prints the
/// child's verdict for the edited call (carrick#3884).
#[test]
#[serial]
fn http_consumer_edit_prints_the_childs_verdict() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let consumer_edit = |repo: &Path| {
        edit(
            &repo.join("apps/web/src/inventory.ts"),
            "widget.activeCount",
            "widget.activeCounts",
        );
    };
    let (shipped, skipped) = both(http_workspace, consumer_edit);

    assert!(shipped.retype_ran, "the child retypes the consumer's calls");
    assert!(
        !shipped.retype_decided.is_empty(),
        "and decides at least one pair"
    );
    assert!(!skipped.retype_ran, "the switch turns the retype off");
    // The edited call's pair, as the check's child decided it.
    let site = "apps/web/src/inventory.ts:7";
    let buckets = child_buckets(&shipped.log);
    let bucket = buckets
        .get(site)
        .unwrap_or_else(|| panic!("the child logs {site}: {buckets:#?}"));
    let printed = shipped
        .text
        .lines()
        .find(|line| line.starts_with(&format!("mono/{site} ")))
        .unwrap_or_default();
    if bucket == "incompatible" {
        assert!(
            printed.contains("  incompatible  "),
            "the check prints the child's break:\n{}",
            shipped.text
        );
        assert_eq!(shipped.text_exit, Some(1), "and fails:\n{}", shipped.text);
    } else {
        assert!(
            !printed.contains("  incompatible  "),
            "the check prints no break the child did not decide ({bucket}):\n{}",
            shipped.text
        );
    }
    assert_eq!(shipped.stored, skipped.stored, "the index is not written");
}

/// Each of web's response pairs against api by its consumer site
/// (`file:line`), with the bucket the scan child logged for it. Only a scan
/// logs `Pair verdict:` lines: the join holds no local consumers, so it logs
/// none.
fn child_buckets(log: &str) -> std::collections::BTreeMap<String, String> {
    log.lines()
        .filter_map(|line| {
            let (_, verdict) = line.split_once("Pair verdict: ")?;
            let (_, site) = verdict.split_once(" response (")?;
            let (site, rest) = site.split_once(" in web against api): ")?;
            Some((
                site.to_string(),
                rest.split_whitespace().next()?.to_string(),
            ))
        })
        .collect()
}

fn read_logs(root: &Path) -> String {
    let mut log = String::new();
    for path in files_under(&root.join("home/.carrick/logs"), "log") {
        log.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
    }
    let _ = std::fs::remove_dir_all(root.join("home/.carrick/logs"));
    log
}

/// carrick#3884: `refresh` reads the retype too. With no edit, the index's
/// rows for web's calls hold the refresh child's buckets, so a retype-decided
/// break is in the index (today it is "not judged"). Then on the producer
/// edit, a consumer row outside the change that the refresh child held
/// compatible and the check child decides incompatible is counted as moved.
#[test]
#[serial]
fn refresh_indexes_the_retype_and_check_changed_counts_what_it_moved() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let dir = http_workspace();
    let root = dir.path();
    let indexed = command(root, &["refresh"], &[]).output().expect("refresh");
    assert!(
        indexed.status.success(),
        "{}",
        String::from_utf8_lossy(&indexed.stderr)
    );
    let before = child_buckets(&read_logs(root));
    assert!(!before.is_empty(), "the refresh child judges web's pairs");

    let files = [
        "mono/apps/web/src/inventory.ts",
        "mono/apps/web/src/client.ts",
    ];
    let mut args = vec!["check"];
    args.extend(files);
    args.push("--json");
    let index = command(root, &args, &[]).output().expect("check files");
    let index: Value = serde_json::from_slice(&index.stdout).expect("JSON");
    // The retype abstains at `client.ts:8`: its call sits in a generic
    // wrapper whose stated type is the enclosing type parameter (carrick#3897),
    // so nothing is claimed about it. That pair is the one unresolved row,
    // carrying the reason; every other pair the refresh child judged is a
    // checked row, as each was before the join read the stored outcomes.
    let wrapped = "apps/web/src/client.ts:8";
    assert!(
        before.contains_key(wrapped) && before[wrapped] != "incompatible",
        "the refresh child does not call the wrapper's pair a break: {before:#?}"
    );
    assert_eq!(
        index["type_checked"],
        serde_json::json!(before.len() - 1),
        "{index:#}"
    );
    let unchecked = index["not_type_checked"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(unchecked.len(), 1, "{index:#}");
    assert_eq!(
        format!(
            "{}:{}",
            unchecked[0]["file"]
                .as_str()
                .unwrap_or_default()
                .trim_start_matches("mono/"),
            unchecked[0]["line"]
        ),
        wrapped,
        "{index:#}"
    );
    assert_eq!(
        unchecked[0]["reason"],
        "the pair was compared and a side of it did not resolve to a usable type, so nothing is claimed about it",
        "{index:#}"
    );
    let listed: Vec<String> = index["problems"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|problem| problem["kind"] == "incompatible")
        .map(|problem| {
            let file = problem["file"].as_str().unwrap_or_default();
            format!("{}:{}", file.trim_start_matches("mono/"), problem["line"])
        })
        .collect();
    let mut expected: Vec<String> = before
        .iter()
        .filter(|(_, bucket)| *bucket == "incompatible")
        .map(|(site, _)| site.clone())
        .collect();
    expected.sort();
    let mut listed_sorted = listed.clone();
    listed_sorted.sort();
    assert_eq!(
        listed_sorted, expected,
        "the index lists exactly the refresh child's breaks: {before:#?}\n{index:#}"
    );
    let _ = read_logs(root);

    let route = root.join("mono/apps/api/app/routes/api.v1.widgets.$widgetId.ts");
    edit(&route, "activeCount: number", "active: number");
    edit(&route, "activeCount: 3", "active: 3");
    let changed = command(root, &["check", "--changed", "--json"], &[])
        .output()
        .expect("check --changed");
    let changed: Value = serde_json::from_slice(&changed.stdout).expect("JSON");
    let after = child_buckets(&read_logs(root));
    let moved = before
        .iter()
        .filter(|(site, bucket)| {
            *bucket == "compatible" && after.get(*site).map(String::as_str) == Some("incompatible")
        })
        .count();
    assert!(moved >= 1, "the edit moves a row: {before:#?} {after:#?}");
    assert_eq!(
        changed["outside_moved"],
        serde_json::json!(moved),
        "{before:#?} {after:#?}\n{changed:#}"
    );
}

/// A socket payload the judge reads itself. The retype is HTTP-only, so it
/// never runs; the check prints a real break, and the same one either way.
#[test]
#[serial]
fn socket_pair_check_changed_is_the_same_without_the_retype() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let payload_edit = |repo: &Path| {
        edit(
            &repo.join("apps/payments/src/types/index.ts"),
            "  amountCents: number;\n  status:",
            "  amountCents: string;\n  status:",
        );
    };
    let claims = socket_claims_cassette();
    let cassettes = claims.path().to_str().expect("utf-8 path");
    let (shipped, skipped) = both_with_env(
        socket_workspace,
        payload_edit,
        &[("CARRICK_MOCK_FIXTURE_DIR", cassettes)],
    );

    assert!(
        shipped
            .text
            .contains("incompatible  route SERVER->CLIENT payment:settled"),
        "the check prints the socket break:\n{}",
        shipped.text
    );
    assert_eq!(shipped.text_exit, Some(1), "and fails the run");
    assert_unread(&shipped, &skipped);
}

/// A GraphQL pair across two services of one repo, from the cross-repo
/// corpus: `gateway` serves `subscription orderUpdated`, `web` reads it.
fn graphql_workspace() -> tempfile::TempDir {
    workspace(|repo| {
        let corpus = fixtures().join("xrepo-corpus-1");
        copy_tree(
            &corpus.join("orders-monorepo/packages/gateway"),
            &repo.join("apps/gateway"),
        );
        copy_tree(&corpus.join("web-frontend"), &repo.join("apps/web"));
        for service in ["gateway", "web"] {
            for stale in ["carrick.json", "expected.json"] {
                let _ = std::fs::remove_file(repo.join("apps").join(service).join(stale));
            }
        }
        std::fs::write(
            repo.join("carrick.json"),
            "{ \"services\": [ { \"name\": \"gateway\", \"directory\": \"apps/gateway\" }, \
             { \"name\": \"web\", \"directory\": \"apps/web\" } ] }\n",
        )
        .expect("write carrick.json");
    })
}

/// The pub/sub wrapper fixture: `dispatch` publishes, `relay` subscribes.
fn pubsub_workspace() -> tempfile::TempDir {
    workspace(|repo| copy_tree(&fixtures().join("pubsub-wrapper-monorepo"), repo))
}

/// The child's and the join's `Pair verdict:` lines for operations that are
/// not HTTP: what the case actually judged.
fn non_http_verdicts(observed: &Observed) -> Vec<String> {
    observed
        .log
        .lines()
        .filter_map(|line| Some(&line[line.find("Pair verdict: ")?..]))
        .filter(|line| {
            !line.starts_with("Pair verdict: GET ") && !line.starts_with("Pair verdict: POST ")
        })
        .map(str::to_string)
        .collect()
}

fn assert_settle_moves_nothing(on: &Observed, off: &Observed) {
    assert_eq!(on.text, off.text, "the printed check");
    assert_eq!(on.text_exit, off.text_exit, "the exit code");
    assert_eq!(on.json, off.json, "the printed JSON");
    assert_eq!(on.json_exit, off.json_exit, "the JSON exit code");
}

/// carrick#3884 must not move a socket pair: the judge decides it, so the
/// join's settle prints exactly what the join's own judge printed.
#[test]
#[serial]
fn socket_pair_prints_the_same_with_the_join_settle_off() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let payload_edit = |repo: &Path| {
        edit(
            &repo.join("apps/payments/src/types/index.ts"),
            "  amountCents: number;\n  status:",
            "  amountCents: string;\n  status:",
        );
    };
    let claims = socket_claims_cassette();
    let cassettes = claims.path().to_str().expect("utf-8 path");
    let (on, off) = settle_on_off(
        socket_workspace,
        payload_edit,
        &[("CARRICK_MOCK_FIXTURE_DIR", cassettes)],
    );
    assert!(
        on.text
            .contains("incompatible  route SERVER->CLIENT payment:settled"),
        "the check prints the socket break:\n{}",
        on.text
    );
    assert_settle_moves_nothing(&on, &off);
}

/// The same for a GraphQL pair.
#[test]
#[serial]
fn graphql_pair_prints_the_same_with_the_join_settle_off() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let schema_edit = |repo: &Path| {
        edit(
            &repo.join("apps/gateway/src/orders.resolver.ts"),
            "  note?: string;",
            "  note?: number;",
        );
    };
    let (on, off) = settle_on_off(graphql_workspace, schema_edit, &[]);
    // With no model the producer's types are unknown, so the pairs are judged
    // unverifiable: this proves the settle moves nothing, not that a GraphQL
    // break would print.
    assert!(
        non_http_verdicts(&on)
            .iter()
            .any(|line| line.starts_with("Pair verdict: GRAPHQL ")),
        "the child judges the GraphQL pairs:\n{}",
        on.log
    );
    assert_settle_moves_nothing(&on, &off);
}

/// The same for a pub/sub pair.
#[test]
#[serial]
fn pubsub_pair_prints_the_same_with_the_join_settle_off() {
    if !sidecar_built() {
        eprintln!("Skipping test: sidecar not built (cd src/sidecar && npm run build)");
        return;
    }
    let payload_edit = |repo: &Path| {
        edit(
            &repo.join("services/dispatch/src/dispatch.ts"),
            "{ approved: boolean; reviewer: string }",
            "{ approved: string; reviewer: string }",
        );
    };
    // The fixture's recorded extraction answers, read by file name.
    let cassettes = format!(
        "{}/",
        fixtures().join("pubsub-wrapper-monorepo/__llm__").display()
    );
    let (on, off) = settle_on_off(
        pubsub_workspace,
        payload_edit,
        &[("CARRICK_MOCK_FIXTURE_DIR", cassettes.as_str())],
    );
    // With no model this fixture yields no judged pair (the call is "not
    // type-checked"), so pub/sub stays unmeasured here (carrick#3887): this
    // only proves the settle moves nothing.
    assert_settle_moves_nothing(&on, &off);
}
