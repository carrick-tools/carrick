//! The fact layer's files for a tree, built as a scan builds them
//! (carrick#3711 R1): the sidecar's walk, the store's answer from the tree's
//! own cassette, and discovery on that answer.

#[path = "support/fact_files.rs"]
mod fact_files;

/// A route registered on a table the installed package builds at module
/// scope is a call the fact layer lists for the reader, under the id the
/// orchestrator gives it.
#[tokio::test]
#[serial_test::serial]
async fn the_fact_layer_lists_a_route_on_an_installed_package_s_instance() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fact_files::install_table_package(&root);
    let file = "src/table.ts";
    let path = root.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "import makeTable from \"fixture-table\";\nconst table = makeTable();\ntable.get(\"/one\", () => ({ one: 1 }));\n",
    )
    .unwrap();
    let facts = fact_files::walked(&root).await;
    assert!(
        fact_files::listed_candidate_at(&facts, &root, file, 3).is_some(),
        "{facts:#?}"
    );
}
