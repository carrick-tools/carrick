//! carrick#1151: a request wrapper that holds its URL in a `const` and issues
//! the request through a second (and third) same-file wrapper yields one row
//! per call site, with the site's method and path.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, and the
//! cassette holds the one candidate the file raises, the innermost `fetch(url,
//! …)`, whose target is a bare parameter that states no route. The sites that
//! name the endpoints raise no candidate at all, so before this change the file
//! produced no row: the wrapper pass read a URL only when it was the wrapper's
//! own parameter, written in the request call itself, with the method stated
//! beside it.
//!
//! See `tests/fixtures/wrapper-chain-const-url/README.md` for the shape.

use std::process::Command;

fn calls() -> Vec<serde_json::Value> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/wrapper-chain-const-url");
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
}

#[test]
fn every_site_through_the_chain_is_one_row_with_its_method_and_path() {
    let calls = calls();

    let mut rows: Vec<(i64, String, String)> = calls
        .iter()
        .map(|call| {
            assert_eq!(call["file"], "src/client.ts", "{call:#?}");
            assert_eq!(
                call["resolution_source"], "same_file_wrapper",
                "every row is read off the chain, not the model: {call:#?}"
            );
            assert_eq!(
                call["base"]["env_var"], "ORDERS_API_URL",
                "the base the outer wrapper closes over resolves as it always does: {call:#?}"
            );
            (
                call["line"].as_i64().expect("line"),
                call["method"].as_str().expect("method").to_string(),
                call["target_url"].as_str().expect("target").to_string(),
            )
        })
        .collect();
    rows.sort();

    assert_eq!(
        rows,
        vec![
            (
                33,
                "GET".to_string(),
                "${process.env.ORDERS_API_URL}/v1/orders".to_string()
            ),
            (
                34,
                "GET".to_string(),
                "${process.env.ORDERS_API_URL}/v1/orders/${orderId}".to_string()
            ),
            (
                35,
                "PATCH".to_string(),
                "${process.env.ORDERS_API_URL}/v1/orders/${orderId}/cancel".to_string()
            ),
            (
                40,
                "GET".to_string(),
                "${process.env.ORDERS_API_URL}/v1/orders/${orderId}/invoice".to_string()
            ),
            // The query string is sent only when there is one. The wrapper
            // pass carries the site's ternary tail verbatim, and the
            // query-tail fold (carrick#1150) turns it into a plain query
            // string before the route-shape gate.
            (
                43,
                "GET".to_string(),
                "${process.env.ORDERS_API_URL}/v1/orders/search?${query}".to_string()
            ),
        ],
        "one row per site and none for the wrappers' own internal requests: {calls:#?}"
    );
}
