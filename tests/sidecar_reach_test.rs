//! carrick#3100 through the Rust client: the sidecar reads the import graph of
//! its program, and answers which files an edit reaches from that graph alone.
//! Needs `node` and the built sidecar (`cd src/sidecar && npm ci && npm run
//! build`), and fails rather than skips in CI without them.

use carrick::services::type_sidecar::{ReachChange, Reached, TypeSidecar};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;

const TYPES: &str = "export interface Order {\n  id: string;\n}\n";
const USER: &str = "import { Order } from './types';\nexport const order: Order = { id: 'a' };\n";
const ALONE: &str = "export const alone = 1;\n";

/// The built sidecar, or `None` where it cannot run outside CI.
fn sidecar_path() -> Option<PathBuf> {
    let node = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js");
    let runnable = node && path.exists();
    assert!(
        runnable || std::env::var("CI").is_err(),
        "node and the built sidecar are needed in CI, or this test would not run"
    );
    runnable.then_some(path)
}

#[test]
fn reach_answers_from_the_graph_the_sidecar_wrote() {
    let Some(sidecar_path) = sidecar_path() else {
        return;
    };
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let files = [
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"target":"ES2022","module":"ESNext","moduleResolution":"Bundler","strict":true,"skipLibCheck":true,"types":[]},"include":["src"]}"#,
        ),
        ("src/types.ts", TYPES),
        ("src/user.ts", USER),
        ("src/alone.ts", ALONE),
    ];
    for (file, text) in files {
        let path = root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    let sidecar = TypeSidecar::spawn(&sidecar_path).expect("spawn sidecar");
    sidecar.start_init(root, None);
    sidecar
        .wait_ready(Duration::from_secs(60))
        .expect("sidecar init");

    // No request has built a program: the graph read builds the default one.
    let graph = sidecar.reach_graph().expect("reach_graph");
    assert_eq!(graph["version"], 1);

    let edited = ReachChange {
        file: "src/types.ts".into(),
        before: TYPES.into(),
        after: TYPES.replace("string", "number"),
    };
    let answer = sidecar.reach(&graph, &[edited]).expect("reach");
    assert_eq!(
        answer.reached,
        Reached::Files(vec!["src/types.ts".into(), "src/user.ts".into()])
    );
    assert!(answer.augmented.is_empty());
    assert!(!answer.exports_moved);

    let json = ReachChange {
        file: "package.json".into(),
        before: "{}".into(),
        after: "{\"a\":1}".into(),
    };
    assert_eq!(
        sidecar.reach(&graph, &[json]).expect("reach").reached,
        Reached::All
    );
}
