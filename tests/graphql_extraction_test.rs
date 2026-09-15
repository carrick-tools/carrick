//! Fixture-driven test of GraphQL contract extraction over a real directory
//! tree: SDL schema discovery, `gql` template documents, and the skip rules
//! for vendored and generated sources.

use carrick::graphql::scan_repo;
use std::path::PathBuf;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphql-service")
}

#[test]
fn scan_repo_extracts_schema_and_documents() {
    let root = fixture_root();
    let service_files = vec![root.join("src/client.ts")];

    let extraction = scan_repo(std::slice::from_ref(&root), &[], &service_files);

    let mut producers: Vec<String> = extraction
        .producers
        .iter()
        .map(|op| op.key.canonical())
        .collect();
    producers.sort();
    assert_eq!(
        producers,
        vec![
            "graphql|mutation|createOrder",
            "graphql|query|order",
            "graphql|query|orders",
        ],
        "schema.graphql root fields should be indexed as producers"
    );

    let mut consumers: Vec<String> = extraction
        .consumers
        .iter()
        .map(|op| op.key.canonical())
        .collect();
    consumers.sort();
    assert_eq!(
        consumers,
        vec!["graphql|query|invoices", "graphql|query|order"],
        "gql documents (including fragment-composed ones) should be indexed as consumers"
    );

    // node_modules and __generated__ sources must never contribute.
    assert!(
        !extraction
            .producers
            .iter()
            .any(|op| op.key.canonical().contains("vendored")),
        "vendored schemas in node_modules must be skipped"
    );
    assert!(
        !extraction
            .consumers
            .iter()
            .any(|op| op.key.canonical().contains("generatedField")),
        "Relay __generated__ artifacts must be skipped"
    );

    // Locations point at real files for issue strings.
    let order_producer = extraction
        .producers
        .iter()
        .find(|op| op.key.canonical() == "graphql|query|order")
        .unwrap();
    assert!(order_producer.file_path.ends_with("schema.graphql"));
    assert!(order_producer.line > 1);
}
