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

/// What a service file's tagged templates extract, whatever the tag is named.
fn tagged(source: &str) -> (Vec<String>, Vec<String>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("docs.ts");
    std::fs::write(&file, source).expect("write source");
    let extraction = scan_repo(&[], &[], std::slice::from_ref(&file));
    let keys = |ops: &[carrick::graphql::GraphqlOp]| {
        let mut keys: Vec<String> = ops.iter().map(|op| op.key.canonical()).collect();
        keys.sort();
        keys
    };
    (keys(&extraction.producers), keys(&extraction.consumers))
}

/// A tag is a GraphQL tag by what it holds, not by what it is called (N17):
/// a member tag and an aliased import both extract, a template that is no
/// GraphQL does not, and the SDL branch still reads SDL.
#[test]
fn a_template_is_read_by_its_text_at_any_tag() {
    let (_, consumers) = tagged("docs.fetch`query Orders { orders { id } }`;\n");
    assert_eq!(consumers, vec!["graphql|query|orders"]);

    let (_, consumers) = tagged("import { gql as q } from 'lib';\nconst A = q`query A { a }`;\n");
    assert_eq!(consumers, vec!["graphql|query|a"]);

    let (producers, consumers) = tagged("css`.a { color: red }`;\n");
    assert!(producers.is_empty() && consumers.is_empty());

    let (producers, _) = tagged("sdl`type Query { a: Int }`;\n");
    assert_eq!(producers, vec!["graphql|query|a"]);

    // The `{ ... }` shorthand parses as a document from plenty of text, so at
    // a tag not named for GraphQL it still extracts nothing.
    let (_, consumers) = tagged("styled`{ color }`;\n");
    assert!(consumers.is_empty());
}

/// The binding of `const NAME = <tag>\`...\`` is recorded at any tag, so a
/// `client.request(NAME)` call can name its operation.
#[test]
fn a_const_bound_to_any_tag_records_its_operation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("docs.ts");
    std::fs::write(
        &file,
        "const ORDERS = docs.fetch`query Orders { orders { id } }`;\n",
    )
    .expect("write source");
    let keys = carrick::graphql::document_operation_keys(&file);
    assert_eq!(
        keys.get("ORDERS").map(String::as_str),
        Some("graphql|query|orders")
    );
}
