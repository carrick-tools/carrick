//! A request DTO's enum field is not an operation to declare
//! (carrick-cloud#1366).
//!
//! The fixture holds two things the model reads the same way. A service
//! method branches on `dto.scope`, an enum field of the request DTO its
//! controller hands it. A lambda-style entry point switches on `action`, the
//! field that names what the caller wants done. The cassettes state both as
//! dispatch tables, which is the misreading seen in production; the scanner
//! must advise the second and not the first.
//!
//! What separates them is where the handler sits: the service method is
//! called by the controller (the call graph resolves `this.notices.create`
//! through the constructor parameter property), and the entry point is called
//! by nothing in the service. Removing the caller check in
//! `dispatch::selects_the_operation` fails
//! `a_dto_enum_field_gets_no_advisory_and_a_real_dispatch_gets_one`.

use std::path::PathBuf;
use std::process::Command;

const NOTHING: &str = r#"{"mounts":[],"endpoints":[],"data_calls":[],"dispatch_tables":[]}"#;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dispatch-dto-enum")
}

fn tables(field: &str, values: &[&str], handler: &str, line: u32) -> String {
    let values: Vec<String> = values.iter().map(|v| format!("\"{v}\"")).collect();
    format!(
        r#"{{"mounts":[],"endpoints":[],"data_calls":[],"dispatch_tables":[
            {{"location":"body","field":"{field}","values":[{}],"handler_name":"{handler}","line_number":{line}}}]}}"#,
        values.join(",")
    )
}

/// Scan the fixture with every file's model answer supplied, and return the
/// report the run prints.
fn scan() -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let analyze = dir.path().join("analyze-file");
    std::fs::create_dir_all(&analyze).expect("create analyze-file dir");
    let answers = [
        ("notices.dto", NOTHING.to_string()),
        ("notices.controller", NOTHING.to_string()),
        // The misreading: the service method's enum branch as a dispatch.
        (
            "notices.service",
            tables("scope", &["all", "specific"], "create", 4),
        ),
        // The real dispatcher.
        (
            "jobs.handler",
            tables("action", &["archive", "publish"], "handler", 3),
        ),
    ];
    for (stem, answer) in answers {
        std::fs::write(analyze.join(format!("{stem}.json")), answer).expect("write cassette");
    }

    let output = Command::new(env!("CARGO_BIN_EXE_carrick"))
        .arg(fixture_dir())
        .env("CARRICK_MOCK_ALL", "1")
        .env(
            "CARRICK_MOCK_FIXTURE_DIR",
            format!("{}/", dir.path().display()),
        )
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
    String::from_utf8(output.stdout).expect("scanner stdout was not UTF-8")
}

#[test]
fn a_dto_enum_field_gets_no_advisory_and_a_real_dispatch_gets_one() {
    let report = scan();
    assert!(
        report.contains("Operations behind one route (1)"),
        "exactly one operations block is advised:\n{report}"
    );
    assert_eq!(
        report.matches("switches on the `action` body").count(),
        1,
        "the entry point switching on `action` is advised once:\n{report}"
    );
    assert!(
        !report.contains("`scope`") && !report.contains("\"field\": \"scope\""),
        "the DTO enum field the service method branches on is not advised:\n{report}"
    );
}
