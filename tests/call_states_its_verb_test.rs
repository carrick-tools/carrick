//! carrick-cloud#1365: a call spelled with its verb is indexed under that verb
//! when the model's row for it states none.
//!
//! The screens call an imported client instance with a verb-named member
//! (`client.post(...)`) and a template-literal URL. No deterministic source
//! states a row for that shape: the ones that state a verb all need a target
//! they can read, and a template literal gives them none. So the row is the
//! model's, and the replayed answer states no method for any of them. The
//! scanner used to index each one as a GET, the default for a missing method.
//! The call names its own verb, and that verb is what the row now carries.
//!
//! One test per line shape from the ticket, and every `candidate_id` in the
//! cassettes is a real candidate, so each row joins the site it names:
//!
//! - a request inside `Promise.all(xs.map(...))`, answered once at the request
//!   itself (fixed) and once at the outer `Promise.all` call, which states no
//!   verb of its own (deferred to carrick#1521, recorded as it is today);
//! - a request inside a one-line `try` holding several statements;
//! - a one-line `if/else` with a request on each branch.
//!
//! `src/lib/items.ts` is the other side: calls whose method the model states
//! correctly, sitting next to or around verb-named calls that are not the
//! request (`headers.get`, a `Map`'s `delete` and `get`, `form.get`,
//! `searchParams.get`, a chain whose head is a `fetch` or a member call, a
//! `Promise.all` over two calls, and a request call whose options hold a
//! callback that issues another). Every one keeps the model's method.
//!
//! See `tests/fixtures/call-states-its-verb/README.md` for the answer key.

use std::collections::HashMap;
use std::process::Command;
use std::sync::OnceLock;

/// What one scan of the fixture produced.
struct Scan {
    /// The scanner's JSON projection.
    projection: serde_json::Value,
    /// The analyzer prompt each file was sent, keyed by the dump's file name.
    prompts: HashMap<String, String>,
}

/// The fixture is scanned once; each test reads its own file's rows.
fn scanned() -> &'static Scan {
    static SCAN: OnceLock<Scan> = OnceLock::new();
    SCAN.get_or_init(|| {
        let repo = env!("CARGO_MANIFEST_DIR");
        let fixture = format!("{repo}/tests/fixtures/call-states-its-verb");
        let mock_dir = format!("{fixture}/__llm__/");
        let storage = tempfile::tempdir().expect("temp storage dir");
        let dump = tempfile::tempdir().expect("temp dump dir");

        let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
            .arg(&fixture)
            .env("CARRICK_MOCK_ALL", "1")
            .env("CARRICK_MOCK_FIXTURE_DIR", &mock_dir)
            .env("CARRICK_LOCAL_STORAGE_DIR", storage.path())
            .env("CARRICK_EVAL_DUMP_DIR", dump.path())
            .env("CARRICK_OUTPUT_JSON", "1")
            .env("CARRICK_SKIP_INTENTS", "1")
            .env_remove("GITHUB_REPOSITORY")
            .env_remove("GITHUB_ACTIONS")
            .env_remove("CI")
            .output()
            .expect("failed to spawn carrick binary");
        assert!(
            output.status.success(),
            "scanner exited non-zero:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let prompts = std::fs::read_dir(dump.path())
            .expect("the dump dir is readable")
            .map(|entry| {
                let path = entry.expect("a dump entry").path();
                let dumped: serde_json::Value = serde_json::from_str(
                    &std::fs::read_to_string(&path).expect("a dump file is readable"),
                )
                .expect("a dump file is JSON");
                (
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    dumped["request_user_message"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .collect();
        Scan {
            projection: serde_json::from_slice(&output.stdout)
                .expect("scanner output was not valid JSON"),
            prompts,
        }
    })
}

/// `(line, method)` for every call the scan indexed in `file`, sorted.
fn methods_in(file: &str) -> Vec<(i64, String)> {
    let mut sites: Vec<(i64, String)> = scanned().projection["calls"]
        .as_array()
        .expect("projection carries a calls array")
        .iter()
        .filter(|call| call["file"] == file)
        .map(|call| {
            (
                call["line"].as_i64().unwrap_or_default(),
                call["method"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    sites.sort();
    sites
}

fn expect(sites: &[(i64, &str)]) -> Vec<(i64, String)> {
    sites
        .iter()
        .map(|(line, method)| (*line, method.to_string()))
        .collect()
}

/// Line 8's row names the request inside `Promise.all`, and takes its verb.
/// Line 4's row names the outer `Promise.all` call, which is not
/// request-shaped and states no verb, so the row the model left without a
/// method is still indexed as a GET. That half records today's behaviour, not
/// the right answer: it is carrick#1521, and when that is fixed line 4 expects
/// POST.
///
/// The first half checks the premise: the analyzer was offered both calls on
/// each line, so the rows really are joined to the call each one names.
#[test]
fn a_request_inside_promise_all_over_a_map_keeps_its_verb() {
    let prompt = &scanned().prompts["src_screens_LabelPicker.tsx.json"];
    for (outer, inner) in [
        (
            "- Candidate span:134-229: Line 4 (span 134-229) Promise.all ",
            "- Candidate span:172-227: Line 4 (span 172-227) client.post ",
        ),
        (
            "- Candidate span:347-451: Line 8 (span 347-451) Promise.all ",
            "- Candidate span:385-449: Line 8 (span 385-449) client.put ",
        ),
    ] {
        assert!(
            prompt.contains(outer) && prompt.contains(inner),
            "the analyzer was not offered the outer call and the request inside it; \
             the cassette's ids no longer name the shape this test is about:\n{prompt}"
        );
    }

    assert_eq!(
        methods_in("src/screens/LabelPicker.tsx"),
        expect(&[(4, "GET"), (8, "PUT")]),
        "{:#}",
        scanned().projection
    );
}

/// A one-line `try` holding several statements, with the request's result
/// bound (line 6) and awaited bare (line 10).
#[test]
fn a_request_inside_a_one_line_try_keeps_its_verb() {
    assert_eq!(
        methods_in("src/screens/InviteCard.tsx"),
        expect(&[(6, "POST"), (10, "PATCH")]),
        "{:#}",
        scanned().projection
    );
}

/// Both branches of a one-line `if/else`, each row joined to its own request.
#[test]
fn each_branch_of_a_one_line_if_else_keeps_its_verb() {
    assert_eq!(
        methods_in("src/screens/MemberToggle.tsx"),
        expect(&[(4, "DELETE"), (4, "POST")]),
        "{:#}",
        scanned().projection
    );
}

/// Calls whose method the model states, around verb-named calls that are not
/// the request. Line 6: a chain whose `finally` deletes from a `Map`. Line 12:
/// a chain whose `then` reads a header. Line 18: a form field read inside a
/// wrapper call's arguments. Line 24: a query parameter read inside the
/// request's own URL argument. Line 28: a `Map` read beside a request. Line
/// 32: a `fetch` chain whose `then` issues a DELETE to the same URL, answered
/// once at the `fetch` (GET) and once, with no method, at the DELETE. Line 39:
/// a row at a `Promise.all` over a request with no literal URL and a DELETE
/// whose path ends the row's target. Line 43: a member-call chain head with a
/// variable argument, its `then` issuing a DELETE to the row's target. Line
/// 47: a request call whose options hold a callback issuing a DELETE to the
/// same URL.
#[test]
fn a_verb_named_call_that_is_not_the_request_leaves_the_models_method() {
    assert_eq!(
        methods_in("src/lib/items.ts"),
        expect(&[
            (6, "GET"),
            (12, "PUT"),
            (18, "PATCH"),
            (24, "DELETE"),
            (28, "POST"),
            (32, "DELETE"),
            (32, "GET"),
            (39, "POST"),
            (43, "GET"),
            (47, "POST"),
        ]),
        "{:#}",
        scanned().projection
    );
}
