//! The type sidecar's config rule and the TypeScript compiler as the answer
//! key for alias resolution (carrick#1104, carrick#3497).
//!
//! The scanner reads tsconfig `paths`/`baseUrl`/`extends` and package.json
//! `imports` itself, in Rust, so that call edges do not depend on the type
//! sidecar being present. That is a second implementation of rules the
//! compiler and the sidecar already own, and this test is what keeps them
//! from drifting. Every case is answered twice:
//!
//! - by the sidecar's own rule for which config governs a file (its built
//!   `capture/service-config.js` and `capture/project-references.js`, composed
//!   as its project loader composes them), then `ts.resolveModuleName` under
//!   that config's options, with the compiler the sidecar ships;
//! - by `WorkspaceIndex::governing_config` and `WorkspaceIndex::resolve`,
//!   built with the same `ServiceScope`.
//!
//! The chosen config and the resolved file must both agree.
//!
//! Needs node and the built sidecar (`dist/` and `node_modules/typescript`).
//! CI builds both before this runs, so there a missing build is a failure;
//! locally it is a skip with a note.
//!
//! Deno import maps are not covered: the compiler does not read them. Their
//! answer key is Deno's module graph, and the Deno fixture in
//! `alias_import_callers_test` pins the shapes that matter.

use std::path::{Path, PathBuf};
use std::process::Command;

use carrick::module_aliases::ServiceScope;
use carrick::workspace_resolver::{Resolution, WorkspaceIndex};

/// One case: importer and specifier, both relative to the case's root.
type Case = (&'static str, &'static str);

const RESOLVE_SCRIPT: &str = r#"
import * as path from 'node:path';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';
const capture = path.join(process.env.CARRICK_SIDECAR, 'dist/src/capture');
const load = (name) => import(pathToFileURL(path.join(capture, name)).href);
const { findServiceTsconfig, fileConfigPath } = await load('service-config.js');
const { serviceConfigPath } = await load('project-references.js');
const ts = createRequire(path.join(capture, 'index.js'))('typescript');
const root = process.env.CARRICK_ROOT;
const service = path.join(root, process.env.CARRICK_SERVICE);
const named = process.env.CARRICK_NAMED ? path.join(root, process.env.CARRICK_NAMED) : undefined;
const cases = JSON.parse(process.env.CARRICK_CASES);
// Steps 1-3: the project loader's `findTsConfig`.
const serviceConfig = named && ts.sys.fileExists(named) ? named : findServiceTsconfig(service, root);
const fileConfigs = new Map();
const owners = new Map();
const relative = (p) => path.relative(root, p).split(path.sep).join('/');
const out = cases.map(([from, spec]) => {
  const importer = path.join(root, from);
  // Step 4: per file, when steps 1-3 found nothing.
  const config = serviceConfig ?? fileConfigPath(importer, service, fileConfigs);
  // Step 5: the project of the config's references that owns the file.
  let owner;
  if (config) {
    if (!owners.has(config)) owners.set(config, serviceConfigPath(config));
    owner = owners.get(config)(importer);
  }
  let options = {};
  if (owner) {
    const read = ts.readConfigFile(owner, ts.sys.readFile);
    options = ts.parseJsonConfigFileContent(read.config, ts.sys, path.dirname(owner), undefined, owner).options;
  }
  const resolved = ts.resolveModuleName(spec, importer, options, ts.sys).resolvedModule;
  const file = !resolved || resolved.isExternalLibraryImport ? null : relative(resolved.resolvedFileName);
  return [owner ? relative(owner) : null, file];
});
process.stdout.write(JSON.stringify(out));
"#;

/// The built sidecar the answer key runs from.
fn sidecar() -> Option<PathBuf> {
    // `CARRICK_SIDECAR_DIR` points a checkout with no sidecar build at
    // another built copy of `src/sidecar`.
    let dir = std::env::var_os("CARRICK_SIDECAR_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar"));
    let built = dir.join("dist/src/capture/service-config.js").is_file()
        && dir.join("dist/src/capture/project-references.js").is_file()
        && dir.join("node_modules/typescript").is_dir();
    let node = Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if built && node {
        return Some(dir);
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "CI must build the sidecar and install node before this test: the sidecar's rule and its compiler are its answer key"
    );
    eprintln!(
        "skipping: node or the built sidecar (src/sidecar dist + node_modules) is missing (build the sidecar)"
    );
    None
}

/// Every case of a tree whose root is the service, naming no config.
fn assert_parity(root: &Path, cases: &[Case]) {
    assert_service_parity(root, "", None, cases);
}

/// Every case of one service: `service` is its directory and `named` the
/// config its `carrick.json` names, both relative to `root`.
fn assert_service_parity(root: &Path, service: &str, named: Option<&str>, cases: &[Case]) {
    let Some(sidecar) = sidecar() else {
        return;
    };
    let output = Command::new("node")
        .arg("--input-type=module")
        .arg("-e")
        .arg(RESOLVE_SCRIPT)
        .env("CARRICK_SIDECAR", &sidecar)
        .env("CARRICK_ROOT", root)
        .env("CARRICK_SERVICE", service)
        .env("CARRICK_NAMED", named.unwrap_or(""))
        .env("CARRICK_CASES", serde_json::to_string(cases).unwrap())
        .output()
        .expect("spawn node");
    assert!(
        output.status.success(),
        "sidecar rule script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let answers: Vec<(Option<String>, Option<String>)> =
        serde_json::from_slice(&output.stdout).expect("sidecar answers");

    let scope = ServiceScope {
        dir: PathBuf::from(service),
        named: named.map(PathBuf::from),
    };
    let index = WorkspaceIndex::build_with_aliases(root, Some(&scope));
    let slash = |path: PathBuf| path.to_string_lossy().replace('\\', "/");
    for ((from, specifier), (config, expected)) in cases.iter().zip(answers) {
        assert_eq!(
            index.governing_config(Path::new(from)).map(slash),
            config,
            "{from}: scanner and sidecar read it under different configs"
        );
        let scanner = match index.resolve(Path::new(from), specifier) {
            Resolution::Internal(path) => Some(slash(path)),
            _ => None,
        };
        assert_eq!(
            scanner, expected,
            "{specifier} from {from}: scanner and compiler disagree"
        );
    }
}

fn fixture(variant: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/governing-config")
        .join(variant)
        .canonicalize()
        .unwrap()
}

/// The carrick#3497 layouts: the config each side picks, and the alias
/// resolved under it.
#[test]
fn the_governing_config_is_the_sidecars_on_every_layout() {
    // Step 2: a member-root `tsconfig.app.json` outranks the root `{}`.
    for variant in ["a-ws", "a-cj"] {
        assert_service_parity(
            &fixture(variant),
            "web",
            None,
            &[("web/src/App.ts", "@/lib/api")],
        );
    }
    // Step 1: a named config governs, `paths` or not.
    assert_service_parity(
        &fixture("a-ws"),
        "web",
        Some("tsconfig.json"),
        &[("web/src/App.ts", "@/lib/api")],
    );
    // Step 2 with the service at the root.
    assert_parity(&fixture("a-solo"), &[("src/App.ts", "@/lib/api")]);
    // Step 5: a config that lists nothing hands its files to a reference.
    assert_service_parity(
        &fixture("b-ws"),
        "web",
        Some("web/tsconfig.json"),
        &[("web/src/App.ts", "@/lib/api")],
    );
    assert_parity(&fixture("b-solo"), &[("src/App.ts", "@/lib/api")]);
    assert_service_parity(
        &fixture("refs-named"),
        "",
        Some("tsconfig.json"),
        &[
            ("apps/one/src/App.ts", "@one/lib/api"),
            ("apps/two/src/App.ts", "@two/lib/api"),
            ("apps/two/src/App.ts", "@one/lib/api"),
        ],
    );
    // Step 3: a member-root jsconfig outranks the ancestor tsconfig.
    assert_service_parity(
        &fixture("j-under-cj"),
        "web",
        None,
        &[("web/src/App.js", "@/lib/api")],
    );
    // Step 4: a lone member jsconfig, read per file.
    assert_service_parity(
        &fixture("j-only"),
        "web",
        None,
        &[("web/src/App.js", "@/lib/api")],
    );
}

/// A lone service-root `jsconfig.json` with a nested `tsconfig.json`: no
/// config governs the whole service, so each file takes its own nearest one
/// (step 4), as the sidecar types it (carrick#3497, S0's narrowing).
#[test]
fn a_lone_root_jsconfig_leaves_nested_configs_their_own_files() {
    let repo = tree(&[
        (
            "web/jsconfig.json",
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["src/*"]}}}"#,
        ),
        (
            "web/src/feature/tsconfig.json",
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["own/*"]}}}"#,
        ),
        ("web/src/lib/api.js", "export function get() {}"),
        ("web/src/feature/own/lib/api.ts", "export function get() {}"),
        ("web/src/App.js", ""),
        ("web/src/feature/x.ts", ""),
    ]);
    let root = repo.path().canonicalize().unwrap();
    assert_service_parity(
        &root,
        "web",
        None,
        &[
            ("web/src/App.js", "@/lib/api"),
            ("web/src/feature/x.ts", "@/lib/api"),
        ],
    );
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
