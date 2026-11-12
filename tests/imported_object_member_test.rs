//! carrick#1733: a call through a client written as an object constant in
//! another module (`ordersApi.addNote(id, note)`, where `export const
//! ordersApi = { addNote: async (...) => apiFetch(`/v1/orders/${id}/notes`,
//! { method: "POST", ... }) }`) takes its method and path from the property's
//! body, not from the model's guess at the call site.
//!
//! Deterministic end to end. The LLM is replayed from `__llm__/`, and the
//! consumer cassettes hold the answers the model gives when nothing supplies
//! the member's body: a path made up from the member's name. Every assertion
//! on a corrected site fails against that cassette on the pre-fix scanner.
//!
//! See `tests/fixtures/imported-object-member/README.md` for the shape.

use std::process::Command;

fn calls() -> Vec<serde_json::Value> {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/imported-object-member");
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

/// The blob the same scan stores. `CARRICK_OUTPUT_JSON` writes none, and
/// the projection carries no unread sites, so this is a second scan.
fn blob() -> serde_json::Value {
    let repo = env!("CARGO_MANIFEST_DIR");
    let fixture = format!("{repo}/tests/fixtures/imported-object-member");
    let mock_dir = format!("{fixture}/__llm__/");
    let store = tempfile::tempdir().expect("store dir");

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(&fixture)
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_MOCK_FIXTURE_DIR", &mock_dir)
        .env("CARRICK_LOCAL_STORAGE_DIR", store.path())
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("CARRICK_OUTPUT_JSON")
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
    std::fs::read_dir(store.path())
        .expect("the store dir is readable")
        .map(|entry| entry.expect("store entry").path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| {
            serde_json::from_str(&std::fs::read_to_string(path).expect("read the blob"))
                .expect("the blob is JSON")
        })
        .expect("the scan stored a blob")
}

fn call_at<'a>(calls: &'a [serde_json::Value], file: &str, line: i64) -> &'a serde_json::Value {
    let matches: Vec<&serde_json::Value> = calls
        .iter()
        .filter(|call| call["file"] == file && call["line"].as_i64() == Some(line))
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one call row at {file}:{line}, got {matches:#?}"
    );
    matches[0]
}

#[test]
fn a_call_through_an_object_constant_states_the_request_its_property_makes() {
    let calls = calls();

    // `ordersApi\n.addNote(orderId, note.trim())\n.then(...)\n.catch(...)`:
    // the cassette says `/note`; the property posts to `/notes`.
    let reply = call_at(&calls, "src/OrderActions.ts", 7);
    assert_eq!(reply["method"], "POST");
    assert_eq!(reply["path"], "/v1/orders/:orderId/notes");
    assert_eq!(reply["resolution_source"], "imported_member");

    // A method shorthand on the same object: the cassette says PATCH on the
    // order itself; the property PUTs its status.
    let approve = call_at(&calls, "src/OrderActions.ts", 12);
    assert_eq!(approve["method"], "PUT");
    assert_eq!(approve["path"], "/v1/orders/:orderId/status");
    assert_eq!(approve["resolution_source"], "imported_member");

    // Another module exports an object of the same name with a member of the
    // same name. The site imports THAT module, so it takes that module's
    // request, never the first one's (which is what the cassette guessed).
    let flag = call_at(&calls, "src/AdminActions.ts", 3);
    assert_eq!(flag["method"], "POST");
    assert_eq!(flag["path"], "/admin/orders/:orderId/notes");
    assert_eq!(flag["resolution_source"], "imported_member");
}

#[test]
fn a_site_the_join_cannot_prove_keeps_the_model_s_answer() {
    let calls = calls();

    // The property's helper call carries no options bag and no verb, so no
    // pass states its request and the site keeps what extraction said.
    let download = call_at(&calls, "src/OrderActions.ts", 4);
    assert_eq!(download["path"], "/v1/orders/:orderId/download");
    assert_eq!(download["resolution_source"], "model");

    // The receiver is a parameter, not an import of the object, so the
    // member's name alone joins nothing.
    let via = call_at(&calls, "src/viaParam.ts", 3);
    assert_eq!(via["path"], "/v1/orders/:orderId/note");
    assert_eq!(via["resolution_source"], "model");

    // The client modules' own request rows are untouched.
    for (file, line, method, path) in [(
        "src/orders.api.ts",
        11,
        "GET",
        "/v1/orders/:orderId/files/:fileId",
    )] {
        let own = call_at(&calls, file, line);
        assert_eq!(own["method"], method, "{file}:{line}");
        assert_eq!(own["path"], path, "{file}:{line}");
    }

    // V1 rule C2 is deleted: the client modules' calls that carry an options
    // bag were rows only because they were awaited with a string argument.
    // Discovery stops at the transport's span callback, which no label
    // covers, so each site is saved as an unread site with its reason.
    let blob = blob();
    let unread = blob["unread_sites"]
        .as_array()
        .expect("the blob carries its unread sites");
    for (file, line) in [
        ("src/orders.api.ts", 21),
        ("src/orders.api.ts", 32),
        ("src/admin.api.ts", 5),
    ] {
        let site = unread
            .iter()
            .find(|site| site["file"] == file && site["line"].as_i64() == Some(line))
            .unwrap_or_else(|| panic!("{file}:{line} is saved as unread: {unread:#?}"));
        assert_eq!(site["reason"], "unresolved_name", "{file}:{line}");
        assert_eq!(site["stop_at"], "src/http.ts:7", "{file}:{line}");
    }

    // Each of those requests is still stated where it is made: at the
    // client object's caller.
    for (file, line, method, path) in [
        (
            "src/OrderActions.ts",
            7,
            "POST",
            "/v1/orders/:orderId/notes",
        ),
        (
            "src/OrderActions.ts",
            12,
            "PUT",
            "/v1/orders/:orderId/status",
        ),
        (
            "src/AdminActions.ts",
            3,
            "POST",
            "/admin/orders/:orderId/notes",
        ),
    ] {
        let caller = call_at(&calls, file, line);
        assert_eq!(caller["method"], method, "{file}:{line}");
        assert_eq!(caller["path"], path, "{file}:{line}");
        assert_eq!(
            caller["resolution_source"], "imported_member",
            "{file}:{line}"
        );
    }
}
