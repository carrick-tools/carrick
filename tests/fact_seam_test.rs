//! The fact layer's files for a fixture tree, built as a scan builds them
//! (carrick#3711 R1): the sidecar's walk, the store's answer from the tree's
//! own cassette, and discovery on that answer.

#[path = "support/fact_files.rs"]
mod fact_files;

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/inline-registration-scope")
}

/// Every route call on an instance of the fixture's installed server package
/// is a listed call, and nothing else in the file is: the handler's
/// `c.get('user')` reads a context, not an instance.
#[tokio::test]
#[serial_test::serial]
async fn the_fact_layer_lists_the_routes_of_an_installed_server_package() {
    let facts = fact_files::walked(&root()).await;
    let lines = |file: &str| -> Vec<usize> {
        fact_files::listed_sites(&facts, &root(), file)
            .candidates
            .iter()
            .map(|candidate| candidate.line_number)
            .collect()
    };
    let every: Vec<(String, Vec<usize>)> = facts
        .listed
        .keys()
        .map(|file| (file.clone(), lines(file)))
        .collect();
    assert_eq!(
        lines("src/items.ts"),
        vec![4],
        "{every:?}\n{:#?}",
        facts.calls
    );
    assert_eq!(
        lines("src/handler.ts"),
        vec![5],
        "{every:?}\n{:#?}",
        facts.calls
    );
}
