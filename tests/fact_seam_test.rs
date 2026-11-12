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
    assert_eq!(lines("src/items.ts"), vec![4], "{every:?}\n{facts:#?}");
    assert_eq!(lines("src/handler.ts"), vec![5], "{every:?}\n{facts:#?}");
}

/// The installed route table a test writes into its own tree lists a route
/// on the table, both verbs of a chain on one path, a route on a table
/// handed in as a parameter, and a handler module's `fetch`.
#[tokio::test]
#[serial_test::serial]
async fn the_route_table_package_lists_its_routes_in_a_written_tree() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fact_files::install_table_package(&root);
    let sources = [
        (
            "src/table.ts",
            "import makeTable from \"fixture-table\";\nimport { h0 } from \"./handlers/h0\";\nconst table = makeTable();\ntable.get(\"/one\", h0);\ntable.route(\"/c/0\").get(h0).post(h0);\n",
        ),
        (
            "src/handlers/h0.ts",
            "export function h0(req: any, res: any) {\n  return fetch(\"/upstream/0\");\n}\n",
        ),
        (
            "src/plugin.ts",
            "import type { Table } from \"fixture-table\";\nexport async function plugin(app: Table) {\n  app.get(\"/p\", async () => ({ plugin: 0 }));\n}\n",
        ),
    ];
    for (relative, text) in sources {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let facts = fact_files::walked(&root).await;
    let listed: Vec<(String, usize)> = sources
        .iter()
        .flat_map(|(file, _)| {
            fact_files::listed_sites(&facts, &root, file)
                .candidates
                .into_iter()
                .map(move |candidate| (file.to_string(), candidate.line_number))
        })
        .collect();
    assert_eq!(
        listed,
        vec![
            ("src/table.ts".to_string(), 4),
            ("src/table.ts".to_string(), 5),
            ("src/table.ts".to_string(), 5),
            ("src/plugin.ts".to_string(), 3),
        ],
        "{facts:#?}"
    );
}
