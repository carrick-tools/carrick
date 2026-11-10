//! A URL written once into a local (`const target = base + path`, then
//! `fetch(target)`) states its request at each caller of the wrapper, as the
//! same expression written inline in the call does (carrick#3679).
//!
//! Deterministic end to end: the LLM is replayed from `__llm__/` with empty
//! cassettes, so every row asserted here is read off the source by the request
//! summaries. See `tests/fixtures/wrapper-callers/README.md` for the shapes.

use std::process::Command;
use std::sync::OnceLock;

const CALLERS: &str = "src/use-local-base.ts";

fn calls() -> &'static [serde_json::Value] {
    static CALLS: OnceLock<Vec<serde_json::Value>> = OnceLock::new();
    CALLS.get_or_init(|| {
        let repo = env!("CARGO_MANIFEST_DIR");
        let fixture = format!("{repo}/tests/fixtures/wrapper-callers");
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

/// The `method target` of the one row stated at the caller on `line`, or
/// `None` where there is no row.
fn row_at(line: i64) -> Option<String> {
    let rows: Vec<_> = calls()
        .iter()
        .filter(|call| {
            call["file"].as_str() == Some(CALLERS) && call["line"].as_i64() == Some(line)
        })
        .collect();
    assert!(
        rows.len() <= 1,
        "more than one row at line {line}: {rows:#?}"
    );
    rows.first().map(|row| {
        assert_eq!(
            row["resolution_source"].as_str(),
            Some("request_summary"),
            "line {line} is read off the source: {row:#}"
        );
        format!(
            "{} {}",
            row["method"].as_str().unwrap_or("?"),
            row["target_url"].as_str().unwrap_or("?")
        )
    })
}

fn assert_local_reads_as_its_twin(local: i64, inline: i64, expected: &str) {
    assert_eq!(
        row_at(inline).as_deref(),
        Some(expected),
        "inline twin, line {inline}"
    );
    assert_eq!(
        row_at(local).as_deref(),
        Some(expected),
        "local, line {local}"
    );
}

#[test]
fn a_url_local_of_an_unstated_base_and_a_parameter_is_read_at_every_caller() {
    // An option beside the field, then the field alone.
    assert_local_reads_as_its_twin(16, 17, "POST ${opts.base}/items.list");
    assert_local_reads_as_its_twin(18, 19, "POST /api/items.list");
    // A getter call and an environment read, each beside the path.
    assert_local_reads_as_its_twin(20, 21, "POST ${getBase()}/items.list");
    assert_local_reads_as_its_twin(22, 23, "POST ${process.env.LOCAL_BASE_URL}/items.list");
    // A template of an option with a fallback.
    assert_local_reads_as_its_twin(24, 25, "POST ${opts.base}/items.list");
}

#[test]
fn a_url_local_that_held_a_literal_or_no_parameter_reads_as_before() {
    assert_eq!(row_at(26).as_deref(), Some("POST /v2/items.list"));
    assert_eq!(
        row_at(27),
        None,
        "no parameter: written by the local's name"
    );
}
