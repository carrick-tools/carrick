//! The TypeScript compiler as the answer key for alias resolution
//! (carrick#1104).
//!
//! The scanner reads tsconfig `paths`/`baseUrl`/`extends` and package.json
//! `imports` itself, in Rust, so that call edges do not depend on the type
//! sidecar being present. That is a second implementation of rules the
//! compiler already owns, and this test is what keeps the two from drifting:
//! every case is resolved by `ts.resolveModuleName` with the importer's real
//! tsconfig (the bundled compiler the sidecar ships) and by
//! `WorkspaceIndex::resolve`, and the answers must agree.
//!
//! Needs node and the built sidecar's `node_modules/typescript`. CI builds
//! both before this runs, so there a missing compiler is a failure; locally it
//! is a skip with a note.
//!
//! Deno import maps are not covered: the compiler does not read them. Their
//! answer key is Deno's module graph, and the Deno fixture in
//! `alias_import_callers_test` pins the shapes that matter.

use std::path::{Path, PathBuf};
use std::process::Command;

use carrick::workspace_resolver::{Resolution, WorkspaceIndex};

/// One case: importer and specifier, both relative to the case's root.
type Case = (&'static str, &'static str);

const RESOLVE_SCRIPT: &str = r#"
const ts = require(process.env.CARRICK_TS);
const path = require('path');
const root = process.env.CARRICK_ROOT;
const cases = JSON.parse(process.env.CARRICK_CASES);
const out = cases.map(([from, spec]) => {
  const importer = path.join(root, from);
  const configPath = ts.findConfigFile(path.dirname(importer), ts.sys.fileExists);
  let options = {};
  if (configPath) {
    const read = ts.readConfigFile(configPath, ts.sys.readFile);
    options = ts.parseJsonConfigFileContent(read.config, ts.sys, path.dirname(configPath), undefined, configPath).options;
  }
  const resolved = ts.resolveModuleName(spec, importer, options, ts.sys).resolvedModule;
  if (!resolved || resolved.isExternalLibraryImport) return null;
  return path.relative(root, resolved.resolvedFileName).split(path.sep).join('/');
});
process.stdout.write(JSON.stringify(out));
"#;

fn typescript() -> Option<PathBuf> {
    // `CARRICK_TYPESCRIPT_DIR` points a checkout with no sidecar build at
    // another copy of the compiler package.
    let ts = std::env::var_os("CARRICK_TYPESCRIPT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/node_modules/typescript")
        });
    let node = Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if ts.is_dir() && node {
        return Some(ts);
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "CI must build the sidecar and install node before this test: the compiler is its answer key"
    );
    eprintln!(
        "skipping: node or src/sidecar/node_modules/typescript is missing (build the sidecar)"
    );
    None
}

fn assert_parity(root: &Path, cases: &[Case]) {
    let Some(ts) = typescript() else {
        return;
    };
    let output = Command::new("node")
        .arg("-e")
        .arg(RESOLVE_SCRIPT)
        .env("CARRICK_TS", &ts)
        .env("CARRICK_ROOT", root)
        .env("CARRICK_CASES", serde_json::to_string(cases).unwrap())
        .output()
        .expect("spawn node");
    assert!(
        output.status.success(),
        "compiler script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let compiler: Vec<Option<String>> =
        serde_json::from_slice(&output.stdout).expect("compiler answers");

    let index = WorkspaceIndex::build_with_aliases(root, None);
    for ((from, specifier), expected) in cases.iter().zip(compiler) {
        let scanner = match index.resolve(Path::new(from), specifier) {
            Resolution::Internal(path) => Some(path.to_string_lossy().replace('\\', "/")),
            _ => None,
        };
        assert_eq!(
            scanner, expected,
            "{specifier} from {from}: scanner and compiler disagree"
        );
    }
}

fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    for (path, contents) in files {
        let path = repo.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    repo
}

#[test]
fn the_alias_fixture_resolves_as_the_compiler_resolves_it() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/alias-import-callers/tsconfig")
        .canonicalize()
        .unwrap();
    assert_parity(
        &root,
        &[
            (
                "apps/lockers/src/deliveries/delivery.service.ts",
                "@/slots/availability.ts",
            ),
            ("apps/lockers/src/deliveries/delivery.service.ts", "@/db.ts"),
            (
                "apps/lockers/src/returns/returns.handler.ts",
                "@/slots/mod.ts",
            ),
            (
                "apps/lockers/src/events/event-processor.service.ts",
                "#pickup/queue.ts",
            ),
            (
                "apps/lockers/src/pickup/legacy-pickup.ts",
                "~/pickup/queue.ts",
            ),
            ("apps/lockers/src/main.ts", "hono"),
            ("apps/lockers/src/main.ts", "@/nowhere.ts"),
        ],
    );
}

#[test]
fn extends_chains_resolve_as_the_compiler_resolves_them() {
    let repo = tree(&[
        // paths declared with no baseUrl: relative to the declaring config.
        (
            "config/tsconfig.paths.json",
            r#"{"compilerOptions":{"paths":{"@lib/*":["../lib/*"]}}}"#,
        ),
        (
            "apps/api/tsconfig.json",
            r#"{"extends":"../../config/tsconfig.paths.json"}"#,
        ),
        ("lib/clock.ts", "export const now = 1;"),
        // A later extends entry overrides an earlier one; a nearer paths map
        // replaces the inherited one whole.
        (
            "apps/web/a.json",
            r#"{"compilerOptions":{"paths":{"@a/*":["./from-a/*"],"@x/*":["./from-a/*"]}}}"#,
        ),
        (
            "apps/web/b.json",
            r#"{"compilerOptions":{"paths":{"@x/*":["./from-b/*"]}}}"#,
        ),
        (
            "apps/web/tsconfig.json",
            r#"{"extends":["./a.json","./b.json"]}"#,
        ),
        ("apps/web/from-a/m.ts", ""),
        ("apps/web/from-b/m.ts", ""),
        // baseUrl alone, with JSONC syntax.
        (
            "apps/cli/tsconfig.json",
            "{\n  // comment\n  \"compilerOptions\": { \"baseUrl\": \"./src\", },\n}",
        ),
        ("apps/cli/src/commands/run.ts", ""),
        // An exact key beats a pattern; the longer prefix wins among patterns.
        (
            "apps/jobs/tsconfig.json",
            r#"{"compilerOptions":{"paths":{"@jobs/*":["./src/*"],"@jobs/special/*":["./special/*"],"@jobs/exact":["./src/exact-target.ts"]}}}"#,
        ),
        ("apps/jobs/src/special/a.ts", ""),
        ("apps/jobs/special/a.ts", ""),
        ("apps/jobs/src/exact-target.ts", ""),
        ("apps/jobs/src/exact.ts", ""),
    ]);
    let root = repo.path().canonicalize().unwrap();
    assert_parity(
        &root,
        &[
            ("apps/api/src/x.ts", "@lib/clock"),
            ("apps/web/src/x.ts", "@x/m"),
            ("apps/web/src/x.ts", "@a/m"),
            ("apps/cli/src/index.ts", "commands/run"),
            ("apps/jobs/src/x.ts", "@jobs/special/a"),
            ("apps/jobs/src/x.ts", "@jobs/exact"),
        ],
    );
}
