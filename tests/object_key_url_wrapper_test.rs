//! carrick#1950: a request wrapper whose URL arrives as a key of an object
//! parameter states its request at each caller that writes the URL, and a
//! query one of the service's own functions builds says nothing about the
//! route.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, and every
//! cassette is empty, which is what the model honestly says of these files:
//! the only candidate each wrapper raises is its own `fetch(target, …)`, whose
//! target is a parameter with one definition per caller. So every row asserted
//! here is read off the source by the request summaries, and every site
//! asserted to have none has none from any layer.
//!
//! See `tests/fixtures/object-key-url-wrapper/README.md` for the shape and
//! the answer key.

use std::process::Command;
use std::sync::OnceLock;

/// The base every function in the fixture's plain modules reads: an
/// environment variable no `carrick.json` declares, kept as the source
/// writes it.
const THINGS: &str = "${process.env.THINGS_API_URL}";

fn calls() -> &'static [serde_json::Value] {
    static CALLS: OnceLock<Vec<serde_json::Value>> = OnceLock::new();
    CALLS.get_or_init(|| {
        let repo = env!("CARGO_MANIFEST_DIR");
        let fixture = format!("{repo}/tests/fixtures/object-key-url-wrapper");
        let mock_dir = format!("{fixture}/__llm__/");

        let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
            .arg(&fixture)
            .env("CARRICK_MOCK_ALL", "1")
            .env("CARRICK_MOCK_FIXTURE_DIR", &mock_dir)
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
        let stdout = String::from_utf8(output.stdout).expect("scanner stdout was not UTF-8");
        let projection: serde_json::Value =
            serde_json::from_str(&stdout).expect("scanner output was not valid JSON");
        projection["calls"]
            .as_array()
            .expect("projection carries a calls array")
            .clone()
    })
}

fn rows_in(file: &str) -> Vec<&'static serde_json::Value> {
    calls()
        .iter()
        .filter(|call| call["file"].as_str() == Some(file))
        .collect()
}

/// The one row at `file:line`, stated by the summaries, sending `method` to
/// `target`.
fn assert_row(file: &str, line: i64, method: &str, target: &str) {
    let rows: Vec<_> = rows_in(file)
        .into_iter()
        .filter(|call| call["line"].as_i64() == Some(line))
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "expected one row at {file}:{line} ({method} {target}), got {rows:#?}"
    );
    let row = rows[0];
    assert_eq!(
        row["resolution_source"].as_str(),
        Some("request_summary"),
        "{file}:{line} is read off the source: {row:#}"
    );
    assert_eq!(
        row["method"].as_str(),
        Some(method),
        "{file}:{line}: {row:#}"
    );
    assert_eq!(
        row["target_url"].as_str(),
        Some(target),
        "{file}:{line}: {row:#}"
    );
}

/// The lines in `file` that carry a row, in order.
fn lines_with_rows(file: &str) -> Vec<i64> {
    let mut lines: Vec<i64> = rows_in(file)
        .into_iter()
        .filter_map(|call| call["line"].as_i64())
        .collect();
    lines.sort_unstable();
    lines
}

#[test]
fn a_class_wrapper_states_its_request_at_each_caller_that_writes_the_url() {
    let file = "src/portal-client.ts";
    // Inline in the object, ending in a query the class's own method builds:
    // the base is the field the class holds it in, the value before the
    // query is a path parameter, and the query is no part of the route.
    assert_row(file, 14, "POST", "${this.base}/sessions/open/${kind}");
    // Held in a constant first.
    assert_row(file, 23, "POST", "${this.base}/sessions/close/${kind}");
    // No query at all.
    assert_row(file, 29, "POST", "${this.base}/sessions");

    let open = rows_in(file)
        .into_iter()
        .find(|call| call["line"].as_i64() == Some(14))
        .expect("the row at line 14");
    assert_eq!(
        open["path"].as_str(),
        Some("/sessions/open/:kind"),
        "the route the row is matched by: {open:#}"
    );
}

#[test]
fn a_function_that_passes_its_options_on_leaves_the_request_to_its_own_caller() {
    let file = "src/portal-client.ts";
    assert_row(file, 34, "POST", "${this.base}/sessions/renew/${kind}");
    assert_eq!(
        lines_with_rows(file),
        vec![14, 23, 29, 34],
        "one row per caller that writes the URL: none where the options are passed on (line 39) \
         and none at the wrapper's own fetch (line 43)"
    );
}

#[test]
fn every_way_a_function_reads_a_key_of_its_parameter_is_read() {
    let file = "src/things.ts";
    // Destructured in the parameter list. The caller's own base is what says
    // where the request goes.
    assert_row(file, 45, "PUT", &format!("{THINGS}/things/${{id}}/name"));
    // Read by member.
    assert_row(file, 49, "PATCH", &format!("{THINGS}/things/${{id}}/touch"));
    // Unpacked in the body.
    assert_row(file, 53, "DELETE", &format!("{THINGS}/things/${{id}}"));
    // Bound under another name.
    assert_row(file, 57, "POST", &format!("{THINGS}/things/${{id}}/copies"));
}

#[test]
fn a_verb_the_caller_writes_beside_the_url_is_the_rows_verb() {
    let file = "src/things.ts";
    // The verb is written, and the URL ends in a query a module function
    // builds.
    assert_row(file, 61, "GET", &format!("{THINGS}/things"));
    // No verb written: the request the wrapper issues states none either,
    // which is a GET.
    assert_row(file, 65, "GET", &format!("{THINGS}/things/count"));
    assert_eq!(lines_with_rows(file), vec![45, 49, 53, 57, 61, 65]);
}

#[test]
fn a_query_builder_is_read_in_the_module_that_declares_it() {
    let file = "src/query.ts";
    assert_row(file, 14, "GET", &format!("{THINGS}/reports"));
    assert_eq!(lines_with_rows(file), vec![14]);
}

/// Every caller in `refused.ts` hands the wrapper a URL the source does not
/// state as written, or ends its URL in a value no rule reads as a query. Each
/// must state nothing: a row there would name a route the source does not.
#[test]
fn a_url_the_source_does_not_state_has_no_row() {
    let rows = rows_in("src/refused.ts");
    assert!(
        rows.is_empty(),
        "refused.ts states no request: a spread or a computed key after the URL, a key the \
         caller does not write, an object written through, a wrapper that rewrites the URL or \
         defaults the verb, and a tail that is not provably a query (a builder with one other \
         return, another module's builder, an async one, a recursive one, one that can return \
         nothing, a plain value, a query in the middle of the URL). Got {rows:#?}"
    );
}

#[test]
fn a_query_builder_a_class_can_replace_is_not_read() {
    let rows = rows_in("src/refused-class.ts");
    assert!(
        rows.is_empty(),
        "a builder a subclass in the file overrides, or one the class assigns over, may build \
         anything: {rows:#?}"
    );
}

/// A request that states its route without the caller (the key is a base
/// before a literal path, or a whole path segment) is stated where it was
/// before keys were read: at its own line, with the key under the name the
/// function reads it by. The released scanner states these two rows exactly
/// so; reading keys only adds rows, at callers of a wrapper that stated none.
#[test]
fn a_request_that_states_its_route_without_the_caller_stays_at_its_own_line() {
    let file = "src/keyed-base.ts";
    assert_row(file, 7, "GET", "${apiUrl}/widgets");
    assert_row(file, 18, "GET", "${options.apiUrl}/widgets/${options.id}");
    assert_eq!(
        lines_with_rows(file),
        vec![7, 18],
        "none at the caller on line 12: its callee's own line states the request"
    );
}

#[test]
fn the_fixture_states_exactly_its_answer_key() {
    assert_eq!(
        calls().len(),
        13,
        "four class callers, six function callers, one in the builder's own module and the two \
         requests that state their own route: {:#?}",
        calls()
    );
}
