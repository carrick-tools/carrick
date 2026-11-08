//! The TypeScript compiler as the answer key for which config governs a file
//! and for alias resolution (carrick#1104, carrick#3497 ruling R-N2).
//!
//! The scanner reads tsconfig `paths`/`baseUrl`/`extends` and package.json
//! `imports` itself, in Rust, so that call edges do not depend on the type
//! sidecar being present. That is a second implementation of rules the
//! compiler already owns, and the sidecar is a third reader of the same
//! configs; this test is what keeps all three from drifting. Every case is
//! answered by the compiler, the sidecar and the scanner, and a fourth time by
//! the language service itself (tsserver `projectInfo`) wherever it opens the
//! file in a configured project; a case it opens in an inferred project is
//! skipped there. The hand-written oracle below is an imitation of tsserver,
//! so tsserver is the check on the imitation (carrick#3533). Every case is
//! answered:
//!
//! - by the compiler: from the importer's directory, `ts.findConfigFile` for
//!   the nearest `tsconfig.json` and `jsconfig.json` inside the service, the
//!   first (the tsconfig, then the jsconfig beside it) whose parsed file list
//!   (or a referenced project's, in tsserver's search order) holds the
//!   importer, climbing past a directory where neither does; else
//!   the service config. Then `ts.resolveModuleName` under that config's
//!   options, with the compiler the sidecar ships;
//! - by the sidecar's built rule (`capture/service-config.js`
//!   `GoverningConfigs`, as its project loader builds it);
//! - by `WorkspaceIndex::governing_config` and `WorkspaceIndex::resolve`,
//!   built with the same `ServiceScope`.
//!
//! The chosen config must agree three ways, and the resolved file with the
//! compiler's. The service config itself (the fallback) is the sidecar's
//! `findServiceTsconfig`: the compiler has no notion of a service.
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
import { spawn } from 'node:child_process';
const capture = path.join(process.env.CARRICK_SIDECAR, 'dist/src/capture');
const load = (name) => import(pathToFileURL(path.join(capture, name)).href);
const { findServiceTsconfig, GoverningConfigs } = await load('service-config.js');
const ts = createRequire(path.join(capture, 'index.js'))('typescript');
const root = process.env.CARRICK_ROOT;
const service = path.join(root, process.env.CARRICK_SERVICE);
const named = process.env.CARRICK_NAMED ? path.join(root, process.env.CARRICK_NAMED) : undefined;
const cases = JSON.parse(process.env.CARRICK_CASES);
// The fallback, as the project loader's `findTsConfig` finds it.
const serviceConfig = named && ts.sys.fileExists(named) ? named : findServiceTsconfig(service, root);

// The compiler's answer.
const parsed = new Map();
const parse = (config) => {
  if (!parsed.has(config)) {
    const host = { ...ts.sys, onUnRecoverableConfigFileDiagnostic: () => {} };
    parsed.set(config, ts.getParsedCommandLineOfConfigFile(config, {}, host));
  }
  return parsed.get(config);
};
const lists = (config, file) => (parse(config)?.fileNames ?? []).some((f) => path.resolve(f) === file);
const referencesOf = (config) =>
  (parse(config)?.projectReferences ?? []).map((r) => path.resolve(ts.resolveProjectReferencePath(r)));
// tsserver's default project for a file among a config and its references:
// the first in search order (depth-first, declared order) that lists it
// while no project its own references reach lists it.
const reach = (config) => {
  const seen = new Set([config]);
  const out = [];
  const visit = (c) => {
    for (const r of referencesOf(c)) {
      if (seen.has(r)) continue;
      seen.add(r);
      out.push(r);
      visit(r);
    }
  };
  visit(config);
  return out;
};
const ownerAmong = (config, file) =>
  [config, ...reach(config)].find((c) => lists(c, file) && !reach(c).some((r) => lists(r, file)));
const inService = (dir) => {
  const from = path.relative(service, dir);
  return !from.startsWith('..') && !path.isAbsolute(from);
};
const compilerConfig = (file) => {
  let dir = path.dirname(file);
  for (;;) {
    const found = ['tsconfig.json', 'jsconfig.json']
      .map((name) => ts.findConfigFile(dir, ts.sys.fileExists, name))
      .filter((c) => c !== undefined && inService(path.dirname(c)));
    if (found.length === 0) break;
    // The nearer directory; within it each config in turn, tsconfig.json
    // first, passed over when it lists the file nowhere.
    const config = found.reduce((a, b) => (path.dirname(b).length > path.dirname(a).length ? b : a));
    for (const name of ['tsconfig.json', 'jsconfig.json']) {
      const candidate = path.join(path.dirname(config), name);
      if (!found.includes(candidate)) continue;
      const owner = ownerAmong(candidate, file);
      if (owner) return owner;
    }
    if (path.dirname(config) === service) break;
    dir = path.dirname(path.dirname(config));
  }
  return serviceConfig === undefined ? undefined : ownerAmong(serviceConfig, file) ?? serviceConfig;
};

// The language service's answer: the config of the project tsserver opens the
// file in (`projectInfo`). null when it opens an inferred project (no config
// lists the file) or the file is missing; the test skips those cases, since
// the service fallback is Carrick's notion and tsserver has none.
const tsserverConfigs = async (files) => {
  const server = spawn(
    process.execPath,
    [path.join(path.dirname(createRequire(path.join(capture, 'index.js')).resolve('typescript')), 'tsserver.js'), '--disableAutomaticTypingAcquisition'],
    { stdio: ['pipe', 'pipe', 'inherit'] },
  );
  const pending = new Map();
  let buffer = '';
  server.stdout.on('data', (chunk) => {
    buffer += chunk.toString();
    const lines = buffer.split(/\r?\n/);
    buffer = lines.pop();
    for (const line of lines) {
      if (!line.startsWith('{')) continue;
      try {
        const message = JSON.parse(line);
        if (message.type === 'response' && pending.has(message.request_seq)) {
          pending.get(message.request_seq)(message.body);
          pending.delete(message.request_seq);
        }
      } catch {}
    }
  });
  let seq = 0;
  const ask = (command, args) =>
    new Promise((resolve, reject) => {
      pending.set(++seq, resolve);
      server.stdin.write(JSON.stringify({ seq, type: 'request', command, arguments: args }) + '\n');
      // No answer is a failure, never a skip: a tsserver that did not start
      // must not turn this column into a pass.
      setTimeout(() => reject(new Error(`tsserver gave no ${command} answer in 30 s`)), 30000).unref();
    });
  const answers = [];
  for (const file of files) {
    if (!ts.sys.fileExists(file)) {
      answers.push(null);
      continue;
    }
    server.stdin.write(JSON.stringify({ seq: ++seq, type: 'request', command: 'open', arguments: { file } }) + '\n');
    const info = await ask('projectInfo', { file, needFileNameList: false });
    const name = info?.configFileName;
    answers.push(name && path.isAbsolute(name) && ts.sys.fileExists(name) ? path.resolve(name) : null);
  }
  server.kill();
  return answers;
};

const sidecar = new GoverningConfigs(service, serviceConfig);
const relative = (p) => (p === undefined ? null : path.relative(root, p).split(path.sep).join('/'));
const language = await tsserverConfigs(cases.map(([from]) => path.join(root, from)));
const out = cases.map(([from, spec], i) => {
  const importer = path.join(root, from);
  const config = compilerConfig(importer);
  const options = config === undefined ? {} : parse(config)?.options ?? {};
  const resolved = ts.resolveModuleName(spec, importer, options, ts.sys).resolvedModule;
  const file = !resolved || resolved.isExternalLibraryImport ? null : relative(resolved.resolvedFileName);
  return [relative(config), relative(sidecar.configFor(importer)), file, language[i] === null ? null : relative(language[i])];
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
        "CI must build the sidecar and install node before this test: the compiler it ships is the answer key, and its rule is held to it"
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
        "compiler script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let answers: Vec<(
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = serde_json::from_slice(&output.stdout).expect("compiler answers");

    let scope = ServiceScope {
        dir: PathBuf::from(service),
        named: named.map(PathBuf::from),
    };
    let index = WorkspaceIndex::build_with_aliases(root, Some(&scope));
    let slash = |path: PathBuf| path.to_string_lossy().replace('\\', "/");
    for ((from, specifier), (config, sidecar_config, expected, language)) in
        cases.iter().zip(answers)
    {
        // The language service itself, where it can answer. It cannot for a
        // file no config lists (an inferred project, None), and its ancestor
        // search climbs past the service root to a config the service does
        // not own (carrick#3533 finding: the a-ws/a-cj fallback, where the
        // oracle takes the member's `tsconfig.app.json` and tsserver the
        // repo-root `tsconfig.json`); the service is Carrick's notion, so a
        // config outside it is skipped, not faked.
        match language {
            Some(language) if Path::new(&language).starts_with(service) => {
                eprintln!("tsserver answered {from}: {language}");
                assert_eq!(
                    Some(language),
                    config,
                    "{from}: tsserver (projectInfo) and the oracle read it under different configs"
                );
            }
            Some(language) => {
                eprintln!("tsserver skipped {from}: {language} is outside the service")
            }
            None => eprintln!("tsserver skipped {from}: inferred project"),
        }
        assert_eq!(
            sidecar_config, config,
            "{from}: the sidecar and the compiler read it under different configs"
        );
        assert_eq!(
            index.governing_config(Path::new(from)).map(slash),
            config,
            "{from}: the scanner and the compiler read it under different configs"
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
fn the_governing_config_is_the_compilers_on_every_layout() {
    // No config inside the service lists the file: the fallback, where a
    // member-root `tsconfig.app.json` outranks the root `{}`.
    for variant in ["a-ws", "a-cj"] {
        assert_service_parity(
            &fixture(variant),
            "web",
            None,
            &[("web/src/App.ts", "@/lib/api")],
        );
    }
    // A named config is the fallback, `paths` or not.
    assert_service_parity(
        &fixture("a-ws"),
        "web",
        Some("tsconfig.json"),
        &[("web/src/App.ts", "@/lib/api")],
    );
    // The same with the service at the root.
    assert_parity(&fixture("a-solo"), &[("src/App.ts", "@/lib/api")]);
    // A solution config that lists nothing hands its files to a reference.
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
    // A member-root jsconfig lists the file before the ancestor tsconfig.
    assert_service_parity(
        &fixture("j-under-cj"),
        "web",
        None,
        &[("web/src/App.js", "@/lib/api")],
    );
    // A lone member jsconfig, with no fallback.
    assert_service_parity(
        &fixture("j-only"),
        "web",
        None,
        &[("web/src/App.js", "@/lib/api")],
    );
    // A tsconfig that lists only TypeScript sources hands a JavaScript file to
    // the jsconfig beside it; a file the tsconfig lists stays with the tsconfig.
    assert_parity(
        &fixture("ts-beside-js"),
        &[
            ("web/src/App.js", "@/lib/api"),
            ("web/src/typed.ts", "@/lib/api"),
        ],
    );
}

/// carrick#3497 ruling R-N1: a nested config inside a service with a root
/// config governs the files it lists; one that does not list a file (by
/// `include`, or a JavaScript file without `allowJs`) is passed over for
/// the next config up. The layout is `alias_shaped_type_imports_test`'s.
#[test]
fn a_nested_config_inside_a_service_governs_the_files_it_lists() {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/alias-shaped-type-imports")
        .canonicalize()
        .unwrap();
    assert_parity(
        &fixture,
        &[
            ("src/invoicing/invoices.ts", "@invoicing/contracts/invoice"),
            ("src/billing/receipts.ts", "~receipts/receipt"),
        ],
    );
    let repo = tree(&[
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"allowJs":true,"baseUrl":".","paths":{"@/*":["src/*"]}},"include":["src"]}"#,
        ),
        (
            "src/only/tsconfig.json",
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["own/*"]}},"include":["listed"]}"#,
        ),
        (
            "src/ts/tsconfig.json",
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["own/*"]}}}"#,
        ),
        ("src/lib/api.ts", "export function get() {}"),
        ("src/only/own/lib/api.ts", "export function get() {}"),
        ("src/ts/own/lib/api.ts", "export function get() {}"),
        ("src/only/x.ts", ""),
        ("src/ts/x.ts", ""),
        ("src/ts/y.js", ""),
    ]);
    let root = repo.path().canonicalize().unwrap();
    assert_parity(
        &root,
        &[
            ("src/only/x.ts", "@/lib/api"),
            ("src/ts/x.ts", "@/lib/api"),
            ("src/ts/y.js", "@/lib/api"),
        ],
    );
}

/// A lone service-root `jsconfig.json` with a nested `tsconfig.json`: no
/// config governs the whole service, so each file takes the nearest one that
/// lists it, as the compiler and the sidecar pick it (carrick#3497).
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
        // The importers: a config governs a file its program lists, and the
        // compiler lists only files that exist.
        ("apps/api/src/x.ts", ""),
        ("apps/web/src/x.ts", ""),
        ("apps/cli/src/index.ts", ""),
        ("apps/jobs/src/x.ts", ""),
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
