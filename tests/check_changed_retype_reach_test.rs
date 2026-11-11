//! Does the child scan's retype reach anything `carrick check --changed`
//! prints? ($0, no model, no cloud.)
//!
//! `check --changed` re-scans the changed repo in a child process, which
//! judges the pairs and retypes the consumer calls the judge left to the
//! consumer, then joins the workspace in a second process. The question is
//! whether any printed line, the exit code or the stored index depends on the
//! child's retype. Each case runs the same edit twice on two identical
//! workspaces, once as shipped and once with the test-only switch
//! (`CARRICK_TEST_SKIP_RETYPE`) that makes the retype return before it sends a
//! call, and compares everything the check leaves.
//!
//! The HTTP case is built so the retype decides the pair: the consumer's
//! client returns `any`, so the judge has no consumer type and leaves the
//! pair to the consumer's reads. The GraphQL and socket cases use a corpus
//! whose pairs the judge reads directly. The retype is HTTP-only
//! (`retype_consumer_calls`), so those two are the control that nothing else
//! moves.
//!
//! This records today's behaviour. If a later change makes the join read the
//! child's outcomes, the HTTP cases are the ones that must move, and they
//! should be rewritten to say which printed line now depends on the retype.

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
}

fn command(root: &Path, args: &[&str], skip_retype: bool) -> Command {
    let mut command = Command::new(carrick());
    command
        .args(args)
        .arg("--workspace")
        .arg(root)
        .current_dir(root)
        .env_remove("CARRICK_TOKEN")
        .env_remove("CARRICK_CHECK_RESCAN")
        .env_remove(SKIP_RETYPE_FOR_TEST_ENV)
        // Each run logs under its own home, so a run's log lines are its own.
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join(".test-credentials"))
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_CHECK_BUDGET_MS", "600000");
    if skip_retype {
        command.env(SKIP_RETYPE_FOR_TEST_ENV, "1");
    }
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
fn observe(root: &Path, edit_it: impl Fn(&Path), skip_retype: bool) -> Observed {
    let indexed = command(root, &["refresh"], false)
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

    let text = command(root, &["check", "--changed"], skip_retype)
        .output()
        .expect("spawn check --changed");
    let json = command(root, &["check", "--changed", "--json"], skip_retype)
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
        // the retype.
        retype_ran: log
            .lines()
            .any(|line| line.contains(" Retyping ") && line.contains("'s consumer calls: ")),
        retype_decided: log
            .lines()
            .filter(|line| line.contains("Pair verdict:") && line.contains("gate=retype:"))
            .map(|line| line[line.find("Pair verdict:").unwrap_or(0)..].to_string())
            .collect(),
    }
}

/// Run the same edit on two identical workspaces, as shipped and with the
/// retype off, and return both.
fn both(make: fn() -> tempfile::TempDir, edit_it: impl Fn(&Path) + Copy) -> (Observed, Observed) {
    let shipped_root = make();
    let shipped = observe(shipped_root.path(), edit_it, false);
    let skipped_root = make();
    let skipped = observe(skipped_root.path(), edit_it, true);
    (shipped, skipped)
}

fn assert_unread(shipped: &Observed, skipped: &Observed) {
    assert_eq!(shipped.text, skipped.text, "the printed check");
    assert_eq!(shipped.text_exit, skipped.text_exit, "the exit code");
    assert_eq!(shipped.json, skipped.json, "the printed JSON");
    assert_eq!(shipped.json_exit, skipped.json_exit, "the JSON exit code");
    assert_eq!(shipped.stored, skipped.stored, "the stored verdicts");
}

/// The producer's response loses a field the consumer reads. The child's
/// retype decides this pair as a break (the positive control: it ran and it
/// found one); the check prints the same either way.
#[test]
#[serial]
fn http_producer_edit_retype_is_unread_by_check_changed() {
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
    assert!(skipped.retype_decided.is_empty());
    assert_unread(&shipped, &skipped);
}

/// The consumer reads a field the producer never had. Same answer.
#[test]
#[serial]
fn http_consumer_edit_retype_is_unread_by_check_changed() {
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
    assert_unread(&shipped, &skipped);
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
    let (shipped, skipped) = both(socket_workspace, payload_edit);

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
