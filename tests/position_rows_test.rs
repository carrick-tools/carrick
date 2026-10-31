//! Per-position compiler rows (carrick#2405, move 2 of the fact layer).
//!
//! The type sidecar writes one row for each call, member access, comparison
//! and expression identifier of a service's files, keyed by the TypeScript
//! span. A request summary will look its sites up by the SWC span it parsed,
//! converted with `carrick::utils::sidecar_position`. These tests hold the
//! two sides together over `tests/fixtures/position-rows/`:
//!
//! 1. every SWC call, member, compare and identifier position of the
//!    service's files finds a row of its kind, and each file's `h` is the
//!    sha256 the scanner computes over the same bytes;
//! 2. the committed `api.expected.jsonl` parses with the format's own serde
//!    types, so it is the seam pin both sides read;
//! 3. the walk is record-only: a walk process that dies of memory or misses
//!    its deadline records nothing for the run, restarts nothing and leaves no
//!    rows file;
//! 4. off means off: a scan without `CARRICK_POSITION_ROWS=1` asks no walk
//!    and writes no `positions/` directory, and a scan with it writes the rows
//!    and the same blob;
//! 5. every service's walk has exited before the first analysis sidecar
//!    starts (carrick#2413 slice G1).
//!
//! The census at the end (`#[ignore]`) measures join coverage and what the
//! rows hold on any repo with a rows file.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use carrick::position_rows::{
    FLAG_ANY, FLAG_ERROR_TYPE, FLAG_MODULE_DEFAULT, FLAG_PUBLISHED, Kind, RowsFile, ServiceWalk,
    content_hash, read_rows, read_rows_file, record_service_rows,
};
use carrick::services::type_sidecar::{SidecarRecipe, TypeSidecar};
use carrick::utils::sidecar_position;
use swc_common::errors::Handler;
use swc_common::sync::Lrc;
use swc_common::{FileName, SourceMap, Spanned};
use swc_ecma_ast::{
    BinaryOp, CallExpr, Expr, MemberExpr, NewExpr, OptCall, SuperPropExpr, TaggedTpl,
};
use swc_ecma_visit::{Visit, VisitWith};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/position-rows")
}

fn sidecar_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/sidecar/dist/src/index.js")
}

/// Whether the live sidecar can run. CI builds it before the Rust tests, so
/// a skip there would make a green tick mean nothing.
fn sidecar_available() -> bool {
    let node = Command::new("node").arg("--version").output().is_ok();
    let built = sidecar_script().exists();
    assert!(
        (node && built) || std::env::var("CI").is_err(),
        "node or the built sidecar is missing in CI, so these tests would silently not run"
    );
    node && built
}

/// The fixture laid out in a temp dir: the repo, the workspace link a package
/// manager writes for `@fixture/shared`, and a package installed above the
/// repo root. Returns the temp dir (kept alive by the caller) and the repo.
fn materialise() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = fs::canonicalize(dir.path()).expect("canonical temp dir");
    let repo = root.join("repo");
    copy_tree(&fixture().join("repo"), &repo);
    copy_tree(&fixture().join("hoisted"), &root.join("node_modules"));
    std::os::unix::fs::symlink(
        "../../packages/shared",
        repo.join("node_modules/@fixture/shared"),
    )
    .expect("link the workspace package");
    (dir, repo)
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create dir");
    for entry in fs::read_dir(from).expect("read fixture dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

/// The service's files as discovery walks them, sorted.
fn service_files(repo: &Path) -> Vec<PathBuf> {
    let services = carrick::config::Config::load_services(vec![repo.join("carrick.json")])
        .expect("the fixture's carrick.json loads");
    let service = &services[0];
    let mut files = carrick::file_finder::walk_service(repo.to_str().unwrap(), service, &[]).files;
    files.sort();
    files
}

/// One walk of the fixture's service through the scanner's own driver.
fn walk_fixture(repo: &Path, out: &Path) -> RowsFile {
    let sidecar = TypeSidecar::spawn(&sidecar_script()).expect("spawn the sidecar");
    sidecar.set_scan_root(repo);
    let recipe = sidecar.recipe();
    drop(sidecar);
    let files = service_files(repo);
    let service_root = repo.join("apps/api");
    let walk = ServiceWalk {
        service_root: &service_root,
        tsconfig: Some("tsconfig.json"),
        scan_root: repo,
        files: &files,
        out,
        service: "api",
    };
    record_service_rows(&recipe, &walk).expect("the walk writes its rows");
    read_rows_file(out).expect("the rows file reads")
}

/// SWC positions of one file, by kind: what a request summary would look up.
#[derive(Default)]
struct SwcPositions {
    found: Vec<(Kind, u32, u32)>,
}

impl SwcPositions {
    fn add(&mut self, kind: Kind, span: swc_common::Span) {
        self.found.push((kind, span.lo.0, span.hi.0));
    }
}

impl Visit for SwcPositions {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        self.add(Kind::Call, node.span);
        node.visit_children_with(self);
    }
    fn visit_new_expr(&mut self, node: &NewExpr) {
        self.add(Kind::Call, node.span);
        node.visit_children_with(self);
    }
    fn visit_tagged_tpl(&mut self, node: &TaggedTpl) {
        self.add(Kind::Call, node.span);
        node.visit_children_with(self);
    }
    fn visit_opt_call(&mut self, node: &OptCall) {
        self.add(Kind::Call, node.span);
        node.visit_children_with(self);
    }
    fn visit_member_expr(&mut self, node: &MemberExpr) {
        self.add(Kind::Member, node.span);
        node.visit_children_with(self);
    }
    fn visit_super_prop_expr(&mut self, node: &SuperPropExpr) {
        self.add(Kind::Member, node.span);
        node.visit_children_with(self);
    }
    fn visit_bin_expr(&mut self, node: &swc_ecma_ast::BinExpr) {
        if matches!(
            node.op,
            BinaryOp::EqEq
                | BinaryOp::NotEq
                | BinaryOp::EqEqEq
                | BinaryOp::NotEqEq
                | BinaryOp::Lt
                | BinaryOp::LtEq
                | BinaryOp::Gt
                | BinaryOp::GtEq
                | BinaryOp::InstanceOf
                | BinaryOp::In
        ) {
            self.add(Kind::Compare, node.span);
        }
        node.visit_children_with(self);
    }
    // Pinned: SWC types an interface or type-literal member's key as an
    // `Expr`, and TypeScript calls it a type position, which has no row. No
    // expression in a type position is a row, so the walk stops at types.
    fn visit_ts_type(&mut self, _: &swc_ecma_ast::TsType) {}
    fn visit_ts_interface_decl(&mut self, _: &swc_ecma_ast::TsInterfaceDecl) {}
    // Pinned: `implements X` is an `Expr` in SWC and a type position in
    // TypeScript (`isPartOfTypeNode`); a class's `extends` is an expression
    // on both sides.
    fn visit_ts_expr_with_type_args(&mut self, _: &swc_ecma_ast::TsExprWithTypeArgs) {}
    fn visit_expr(&mut self, node: &Expr) {
        if let Expr::Ident(ident) = node {
            self.add(Kind::Ident, ident.span());
        }
        node.visit_children_with(self);
    }
}

/// Each SWC position of `file`, as the sidecar numbers it. Parsed on a source
/// map of its own, as each file of a scan is, and converted against the text
/// as read, BOM included, as the scan converts it (carrick#2407).
fn swc_positions(file: &Path) -> Vec<(Kind, u32, u32)> {
    let content = fs::read_to_string(file).expect("read source");
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_emitter_writer(Box::new(std::io::sink()), None);
    let module = carrick::parser::parse_source(
        file,
        FileName::Real(file.to_path_buf()),
        content.clone(),
        &cm,
        &handler,
    )
    .unwrap_or_else(|| panic!("{} parses", file.display()));
    let mut positions = SwcPositions::default();
    module.visit_with(&mut positions);
    positions
        .found
        .into_iter()
        .map(|(kind, lo, hi)| {
            (
                kind,
                sidecar_position(&content, lo),
                sidecar_position(&content, hi),
            )
        })
        .collect()
}

/// Every SWC position of every walked file finds a row of its kind, and every
/// file's hash is the scanner's sha256 of the same bytes. No allow-list.
#[test]
fn every_swc_position_finds_its_row_and_every_hash_agrees() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let (_dir, repo) = materialise();
    let store = tempfile::tempdir().expect("store dir");
    let out = store.path().join("positions/repo__api.jsonl.gz");
    let rows = walk_fixture(&repo, &out);

    let files = service_files(&repo);
    assert_eq!(rows.files.len(), files.len());
    assert!(
        !files
            .iter()
            .any(|f| f.to_string_lossy().contains("__tests__")),
        "the walk sends test files"
    );

    let mut misses: Vec<String> = Vec::new();
    let mut joined = 0usize;
    for (file, line) in files.iter().zip(&rows.files) {
        let rel = file.strip_prefix(&repo).expect("under the repo");
        assert_eq!(line.f, rel.to_string_lossy(), "request order is file order");
        let bytes = fs::read(file).expect("read source bytes");
        assert_eq!(
            line.h.as_deref(),
            Some(content_hash(&bytes).as_str()),
            "{}: the sidecar's hash is not the scanner's",
            line.f
        );
        assert!(
            line.skip.is_none(),
            "{} was skipped: {:?}",
            line.f,
            line.skip
        );
        let keys: BTreeSet<(Kind, u32, u32)> = line
            .r
            .iter()
            .map(|row| (row.kind(), row.start(), row.end()))
            .collect();
        for position in swc_positions(file) {
            if keys.contains(&position) {
                joined += 1;
            } else {
                misses.push(format!("{} {:?}", line.f, position));
            }
        }
    }
    assert!(
        joined > 50,
        "too few positions joined to prove anything: {joined}"
    );
    // carrick#2407: a BOM file is walked and held to the same join.
    let bom = repo.join("apps/api/src/bom.ts");
    assert!(files.contains(&bom), "bom.ts is walked");
    assert!(
        fs::read(&bom)
            .expect("read bom.ts")
            .starts_with(b"\xEF\xBB\xBF"),
        "bom.ts starts with a BOM"
    );
    assert!(
        misses.is_empty(),
        "SWC positions with no row of their kind:\n{}",
        misses.join("\n")
    );
}

/// The committed expected file is the seam pin: it parses with the format's
/// own types, every row's flags are known bits, and the counts are its own.
#[test]
fn the_committed_rows_parse_with_the_format_types() {
    let text = fs::read_to_string(fixture().join("api.expected.jsonl")).expect("expected file");
    let file = read_rows(text.as_bytes()).expect("the expected file parses");
    assert_eq!(file.header.v, carrick::position_rows::FORMAT_VERSION);
    assert_eq!(file.header.service, "api");
    let rows: Vec<_> = file.files.iter().flat_map(|line| &line.r).collect();
    assert!(rows.iter().all(|row| row.flags() & !127 == 0));
    assert!(rows.iter().any(|row| row.flags() & FLAG_PUBLISHED != 0));
    // A publication bit names a declaration: never on a row that has none.
    assert!(rows.iter().all(|row| {
        row.flags() & (FLAG_PUBLISHED | FLAG_MODULE_DEFAULT) == 0 || row.decl_file().is_some()
    }));
    assert!(rows.iter().any(|row| row.flags() & FLAG_ERROR_TYPE != 0));
    assert!(
        rows.iter()
            .all(|row| row.flags() & FLAG_ANY == 0 || row.flags() & FLAG_ERROR_TYPE == 0)
    );
    for kind in [
        Kind::Call,
        Kind::Member,
        Kind::Compare,
        Kind::Ident,
        Kind::Jsx,
    ] {
        assert!(rows.iter().any(|row| row.kind() == kind), "no {kind:?} row");
    }
}

/// A stand-in sidecar: answers `init`, and does to a `position_rows` request
/// what `behaviour` says. Each process notes its pid in `pids`.
fn stand_in(dir: &Path, behaviour: &str) -> PathBuf {
    let script = dir.join("stand-in.cjs");
    fs::write(
        &script,
        format!(
            r#"
const fs = require('fs');
const path = require('path');
const readline = require('readline');
fs.appendFileSync(path.join(__dirname, 'pids'), process.pid + '\n');
const write = (frame) => process.stdout.write(JSON.stringify(frame) + '\n');
readline.createInterface({{ input: process.stdin, terminal: false }}).on('line', (line) => {{
  const request = JSON.parse(line);
  if (request.action === 'shutdown') process.exit(0);
  if (request.action === 'init') return write({{ request_id: request.request_id, status: 'ready' }});
  if (request.action === 'position_rows') {{
    fs.mkdirSync(path.dirname(request.out_path), {{ recursive: true }});
    fs.writeFileSync(request.out_path + '.partial', 'half');
    {behaviour}
  }}
}});
"#
        ),
    )
    .expect("write the stand-in");
    script
}

fn stand_in_recipe(script: &Path, timeout: Duration) -> SidecarRecipe {
    let sidecar = TypeSidecar::spawn(script)
        .expect("spawn the stand-in")
        .with_operation_timeout(timeout);
    let recipe = sidecar.recipe();
    // The count of processes starts at this one. Dropping it before node has
    // run its first line would kill it unrecorded and the count would be short.
    let pids = script.with_file_name("pids");
    let started = std::time::Instant::now();
    while fs::read_to_string(&pids).unwrap_or_default().is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the stand-in never recorded its start"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(sidecar);
    recipe
}

/// Nothing the run reads was recorded: no memory death (the run told to stop
/// on one carries on), no time limit and no service loss.
fn assert_nothing_recorded() {
    // SAFETY: this binary's tests that read the flag read it after setting it.
    unsafe { std::env::set_var("CARRICK_FAIL_ON_TYPE_MEMORY", "1") };
    assert!(
        carrick::scan_health::type_memory_stop().is_ok(),
        "a record-only walk's death stopped the run"
    );
    assert!(carrick::scan_health::time_limits_for(None).is_empty());
    assert!(carrick::scan_health::service_losses(None).is_empty());
}

fn record_only_walk(behaviour: &str, timeout: Duration) -> (tempfile::TempDir, Result<(), String>) {
    let dir = tempfile::tempdir().expect("temp dir");
    let script = stand_in(dir.path(), behaviour);
    let recipe = stand_in_recipe(&script, timeout);
    let out = dir.path().join("store/positions/repo__api.jsonl.gz");
    fs::create_dir_all(out.parent().unwrap()).unwrap();
    fs::write(&out, "rows from an earlier run").unwrap();
    let files = vec![dir.path().join("a.ts")];
    let walk = ServiceWalk {
        service_root: dir.path(),
        tsconfig: None,
        scan_root: dir.path(),
        files: &files,
        out: &out,
        service: "api",
    };
    let result = record_service_rows(&recipe, &walk).map(|_| ());
    assert!(!out.exists(), "a failed walk left a rows file");
    assert!(
        !carrick::position_rows::partial_path(&out).exists(),
        "a failed walk left its temporary file"
    );
    (dir, result)
}

fn pids(dir: &Path) -> usize {
    fs::read_to_string(dir.join("pids"))
        .unwrap_or_default()
        .lines()
        .count()
}

#[test]
fn a_walk_that_dies_of_memory_records_nothing_and_leaves_no_rows() {
    if Command::new("node").arg("--version").output().is_err() {
        return;
    }
    let (dir, result) = record_only_walk(
        // V8's own way out: the heap line, then SIGABRT.
        "fs.writeSync(2, 'FATAL ERROR: Reached heap limit Allocation failed - JavaScript heap out of memory\\n'); process.abort();",
        Duration::from_secs(30),
    );
    assert!(result.is_err(), "the walk died and was read as an answer");
    assert_nothing_recorded();
    // The stand-in the recipe was taken from, and the walk's own process:
    // nothing was restarted.
    assert_eq!(pids(dir.path()), 2);
}

#[test]
fn a_walk_that_misses_its_deadline_records_nothing_and_is_not_restarted() {
    if Command::new("node").arg("--version").output().is_err() {
        return;
    }
    let (dir, result) = record_only_walk("/* never answers */", Duration::from_millis(500));
    assert!(
        result.is_err(),
        "the walk timed out and was read as an answer"
    );
    assert_nothing_recorded();
    assert_eq!(pids(dir.path()), 2, "a record-only walk was restarted");
}

/// The shim a scan is pointed at with `CARRICK_SIDECAR_DIR`: it runs the real
/// sidecar behind it and notes every process start and every action asked.
/// With `die_on_walk`, a `position_rows` request prints V8's heap line and
/// the process exits mid-request.
fn shim(dir: &Path, die_on_walk: bool) -> PathBuf {
    let shim = dir.join("shim");
    fs::create_dir_all(shim.join("dist/src")).unwrap();
    fs::write(
        shim.join("dist/src/index.js"),
        format!(
            r#"
const fs = require('fs');
const path = require('path');
const readline = require('readline');
const {{ spawn }} = require('child_process');
const notes = {notes:?};
fs.appendFileSync(path.join(notes, 'starts'), process.pid + '\n');
// Each start, with the earlier processes still alive at it.
const events = path.join(notes, 'events');
const earlier = fs.existsSync(events)
  ? fs.readFileSync(events, 'utf8').split('\n').filter((l) => l.startsWith('start ')).map((l) => Number(l.split(' ')[1]))
  : [];
const alive = earlier.filter((pid) => {{ try {{ process.kill(pid, 0); return true; }} catch {{ return false; }} }});
fs.appendFileSync(events, `start ${{process.pid}} ${{alive.join(',')}}\n`);
const child = spawn(process.execPath, [{real:?}], {{ stdio: ['pipe', 'inherit', 'inherit'] }});
child.on('exit', (code) => process.exit(code ?? 1));
readline.createInterface({{ input: process.stdin, terminal: false }}).on('line', (line) => {{
  const request = JSON.parse(line);
  fs.appendFileSync(path.join(notes, 'actions'), request.action + '\n');
  fs.appendFileSync(events, `action ${{process.pid}} ${{request.action}} ${{request.out_path ?? request.outPath ?? ''}}\n`);
  if (request.action === 'position_rows' && {die_on_walk}) {{
    fs.writeSync(2, 'FATAL ERROR: Reached heap limit Allocation failed - JavaScript heap out of memory\n');
    child.kill('SIGKILL');
    process.abort();
  }}
  child.stdin.write(line + '\n');
}}).on('close', () => child.stdin.end());
"#,
            notes = dir.display().to_string(),
            real = sidecar_script().display().to_string(),
        ),
    )
    .unwrap();
    fs::write(shim.join("package.json"), r#"{ "type": "commonjs" }"#).unwrap();
    shim
}

struct Scan {
    /// Each stored blob, by file name, with the run's own stamps removed.
    blobs: BTreeMap<String, serde_json::Value>,
    positions: Option<PathBuf>,
    starts: usize,
    actions: Vec<String>,
    /// `start <pid> <earlier pids alive>` and `action <pid> <action> <out>`
    /// lines, in order.
    events: Vec<String>,
    /// What each service read its rows as, from the fallbacks lines the scan
    /// states for a parent that reads them.
    rows: BTreeSet<(Option<String>, String)>,
}

/// One scan of `repo`. With no `store` the blobs go to the mock store and
/// none is read back; `tmp`, when given, is the scan's `TMPDIR`.
fn scan(
    repo: &Path,
    rows_on: bool,
    die_on_walk: bool,
    store: Option<&Path>,
    tmp: Option<&Path>,
) -> Scan {
    let notes = tempfile::tempdir().expect("notes dir");
    let shim = shim(notes.path(), die_on_walk);
    let cache = tempfile::tempdir().expect("cache dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    match store {
        Some(store) => cmd.env("CARRICK_LOCAL_STORAGE_DIR", store),
        None => cmd.env_remove("CARRICK_LOCAL_STORAGE_DIR"),
    };
    if let Some(tmp) = tmp {
        cmd.env("TMPDIR", tmp);
    }
    cmd.arg(repo)
        .env("CARRICK_LOCAL_STORAGE_ISOLATE", "1")
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_NO_MODEL", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env("CARRICK_FAIL_ON_TYPE_MEMORY", "1")
        .env("CARRICK_PROGRESS", "1")
        .env("CARRICK_SIDECAR_DIR", &shim);
    if rows_on {
        cmd.env("CARRICK_POSITION_ROWS", "1");
    } else {
        cmd.env_remove("CARRICK_POSITION_ROWS");
    }
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_REF",
        "GITHUB_EVENT_NAME",
        "GITHUB_SHA",
        "GITHUB_RUN_ID",
        "GITHUB_ACTIONS",
        "GITHUB_WORKSPACE",
        "CI",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
        "CARRICK_ALLOW_MISSING_TYPES",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut blobs = BTreeMap::new();
    for entry in store
        .into_iter()
        .flat_map(|store| fs::read_dir(store).expect("read the store"))
    {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let mut blob: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            if let Some(object) = blob.as_object_mut() {
                for stamp in [
                    "scanner_version",
                    "scanner_build",
                    "last_updated",
                    // Measured, so it differs run to run (carrick#3083).
                    "memory_peaks",
                ] {
                    object.remove(stamp);
                }
            }
            blobs.insert(
                path.file_name().unwrap().to_string_lossy().into_owned(),
                blob,
            );
        }
    }
    let positions = store.map(|store| store.join("positions"));
    let read = |name: &str| fs::read_to_string(notes.path().join(name)).unwrap_or_default();
    Scan {
        blobs,
        positions: positions.filter(|positions| positions.exists()),
        starts: read("starts").lines().count(),
        actions: read("actions").lines().map(str::to_string).collect(),
        events: read("events").lines().map(str::to_string).collect(),
        rows: String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter_map(carrick::fallback_tally::parse)
            .map(|fallbacks| {
                let rows = serde_json::to_value(fallbacks.rows).unwrap();
                (fallbacks.service, rows.as_str().unwrap().to_string())
            })
            .collect(),
    }
}

/// carrick#2413: the walk runs first, in a process of its own that has
/// exited before the next process (the service's analysis sidecar) starts.
/// Returns the out path the walk was asked to write.
fn assert_walk_first(events: &[String]) -> PathBuf {
    let walk: Vec<&str> = events
        .iter()
        .map(|line| line.split(' ').collect::<Vec<_>>())
        .find(|parts| parts.len() == 4 && parts[0] == "action" && parts[2] == "position_rows")
        .expect("the walk was asked");
    let (pid, out) = (walk[1], walk[3]);
    let starts: Vec<Vec<&str>> = events
        .iter()
        .filter(|line| line.starts_with("start "))
        .map(|line| line.split(' ').collect())
        .collect();
    let walk_start = starts
        .iter()
        .position(|start| start[1] == pid)
        .expect("the walk's process started");
    // The analysis sidecar is the process after the walk's: before
    // carrick#2413 the walk was the scan's last process.
    let next = starts
        .get(walk_start + 1)
        .unwrap_or_else(|| panic!("no process started after the walk: {events:?}"));
    assert!(
        !next
            .get(2)
            .is_some_and(|alive| alive.split(',').any(|alive| alive == pid)),
        "the walk's process was alive when the analysis sidecar started: {events:?}"
    );
    PathBuf::from(out)
}

/// A repo of two services, each a directory with its own manifest and one
/// source file that calls the other.
fn two_service_repo(root: &Path) -> PathBuf {
    let repo = root.join("two");
    let write = |relative: &str, contents: &str| {
        let path = repo.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    };
    write(
        "carrick.json",
        r#"{ "services": [
  { "name": "orders", "directory": "services/orders" },
  { "name": "billing", "directory": "services/billing" }
] }"#,
    );
    write("package.json", r#"{ "name": "two", "private": true }"#);
    for (service, other) in [("orders", "billing"), ("billing", "orders")] {
        write(
            &format!("services/{service}/package.json"),
            &format!(r#"{{ "name": "{service}" }}"#),
        );
        write(
            &format!("services/{service}/src/index.ts"),
            &format!(
                "export async function ask{other}(id: string): Promise<number> {{\n  \
                 const response = await fetch(`http://{other}/items/${{id}}`);\n  \
                 return response.status;\n}}\n"
            ),
        );
    }
    repo
}

/// carrick#2413 slice G1: every service's walk runs before the loop, so
/// each walk's process has exited before the first analysis sidecar starts,
/// and the blobs are the ones a scan without walks writes.
#[test]
fn every_walk_ends_before_the_first_analysis_sidecar_starts() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = two_service_repo(&fs::canonicalize(dir.path()).unwrap());

    let off_store = tempfile::tempdir().unwrap();
    let off = scan(&repo, false, false, Some(off_store.path()), None);
    let on_store = tempfile::tempdir().unwrap();
    let on = scan(&repo, true, false, Some(on_store.path()), None);

    let split = |line: &String| line.split(' ').map(str::to_string).collect::<Vec<_>>();
    let walks: BTreeSet<String> = on
        .events
        .iter()
        .map(split)
        .filter(|parts| parts.len() == 4 && parts[0] == "action" && parts[2] == "position_rows")
        .map(|parts| parts[1].clone())
        .collect();
    assert_eq!(walks.len(), 2, "one walk a service: {:?}", on.events);
    let starts: Vec<Vec<String>> = on
        .events
        .iter()
        .filter(|line| line.starts_with("start "))
        .map(split)
        .collect();
    let first_walk = starts
        .iter()
        .position(|start| walks.contains(&start[1]))
        .expect("the walks' processes started");
    // Every process after the first walk that is not a walk is an analysis
    // sidecar; the run's own sidecar started before any walk.
    let analyses: Vec<&Vec<String>> = starts[first_walk..]
        .iter()
        .filter(|start| !walks.contains(&start[1]))
        .collect();
    assert_eq!(
        analyses.len(),
        2,
        "one analysis sidecar a service: {starts:?}"
    );
    let first_analysis = starts
        .iter()
        .position(|start| start == analyses[0])
        .unwrap();
    assert!(
        starts[first_analysis..]
            .iter()
            .all(|start| !walks.contains(&start[1])),
        "a walk started after the first analysis sidecar: {starts:?}"
    );
    for analysis in analyses {
        let alive = analysis.get(2).map(String::as_str).unwrap_or("");
        assert!(
            !alive.split(',').any(|pid| walks.contains(pid)),
            "a walk's process was alive when an analysis sidecar started: {starts:?}"
        );
    }
    assert_eq!(off.blobs.len(), 2, "one blob a service");
    assert_eq!(
        on.blobs, off.blobs,
        "walking ahead of the loop moved a blob"
    );
}

/// carrick#2413: with no local store (the Action, the cloud runner) the flag
/// still walks, into a directory of the run's own under the OS temp dir,
/// outside the repo, and the rows file and the directory are gone when the
/// scan ends.
#[test]
fn without_a_store_the_walk_writes_to_the_temp_dir_and_cleans_up() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let (_dir, repo) = materialise();
    let tmp = tempfile::tempdir().unwrap();
    let tmp_root = fs::canonicalize(tmp.path()).unwrap();
    let on = scan(&repo, true, false, None, Some(&tmp_root));
    // `TMPDIR` is canonical, so the path the scan sends is under it as
    // written.
    let out = assert_walk_first(&on.events);
    assert!(
        out.starts_with(&tmp_root),
        "the rows went outside the temp dir: {}",
        out.display()
    );
    assert!(
        !out.starts_with(&repo),
        "the rows went inside the repo: {}",
        out.display()
    );
    let left: Vec<_> = fs::read_dir(&tmp_root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .filter(|name| name.to_string_lossy().starts_with("carrick-positions-"))
        .collect();
    assert!(
        left.is_empty(),
        "the run's rows directory outlived it: {left:?}"
    );
}

/// Off means off; on writes the rows and the same blob; a walk that dies of
/// memory under `CARRICK_FAIL_ON_TYPE_MEMORY=1` leaves the scan to finish,
/// with the same blob and no rows. Each scan states, for a parent reading
/// its stderr, what its service read the rows as (`off`, `read`, `missing`).
#[test]
fn a_scan_writes_rows_only_when_asked_and_its_blob_never_moves() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let (_dir, repo) = materialise();

    let off_store = tempfile::tempdir().unwrap();
    let off = scan(&repo, false, false, Some(off_store.path()), None);
    assert!(
        off.positions.is_none(),
        "a scan without the flag wrote rows"
    );
    assert!(
        !off.actions.iter().any(|action| action == "position_rows"),
        "a scan without the flag asked for a walk"
    );
    assert!(!off.blobs.is_empty(), "the scan stored no blob");
    let read_as = |scan: &Scan| -> BTreeSet<String> {
        scan.rows.iter().map(|(_, rows)| rows.clone()).collect()
    };
    assert_eq!(read_as(&off), BTreeSet::from(["off".to_string()]));

    let on_store = tempfile::tempdir().unwrap();
    let on = scan(&repo, true, false, Some(on_store.path()), None);
    let rows_file = on
        .positions
        .as_ref()
        .expect("a scan with the flag wrote rows")
        .join(format!(
            "{}.jsonl.gz",
            on.blobs.keys().next().unwrap().trim_end_matches(".json")
        ));
    let rows = read_rows_file(&rows_file).expect("the scan's rows file reads");
    assert_eq!(rows.files.len(), service_files(&repo).len());
    assert_eq!(
        on.starts,
        off.starts + 1,
        "the walk is one process of its own"
    );
    assert_walk_first(&on.events);
    assert_eq!(on.blobs, off.blobs, "writing rows moved the blob");
    assert_eq!(read_as(&on), BTreeSet::from(["read".to_string()]));
    assert_eq!(
        on.rows
            .iter()
            .map(|(service, _)| service)
            .collect::<Vec<_>>(),
        off.rows
            .iter()
            .map(|(service, _)| service)
            .collect::<Vec<_>>(),
        "one line a service"
    );

    let died_store = tempfile::tempdir().unwrap();
    let died = scan(&repo, true, true, Some(died_store.path()), None);
    assert!(
        died.actions.iter().any(|action| action == "position_rows"),
        "the walk was not asked"
    );
    let left: Vec<_> = died
        .positions
        .iter()
        .flat_map(|dir| fs::read_dir(dir).unwrap())
        .collect();
    assert!(left.is_empty(), "a walk that died left files: {left:?}");
    assert_eq!(died.blobs, off.blobs, "a walk that died moved the blob");
    assert_eq!(read_as(&died), BTreeSet::from(["missing".to_string()]));
}

/// carrick#1604: a service with no config at its root, scanned with rows on,
/// types each file under the nearest `tsconfig.json` or `jsconfig.json` on its
/// way up. Every use of an imported name then has a row whose declaration is
/// the repo file that exports it, and no row is the error type.
#[test]
fn imports_resolve_through_the_nearest_config_of_a_service_with_none_at_its_root() {
    if !sidecar_available() {
        eprintln!("Skipping: node or the built sidecar is missing");
        return;
    }
    // (fixture, caller, declared name, declaring file, calls the caller makes)
    let uses: [(&str, &str, &str, &str, usize); 3] = [
        (
            "configs-below-root",
            "web/src/api/accounts.ts",
            "getJson",
            "web/src/lib/http.ts",
            3,
        ),
        (
            "configs-below-root",
            "cli/src/main.ts",
            "fetchStatus",
            "cli/src/util.ts",
            1,
        ),
        (
            "jsconfig-root",
            "src/page.js",
            "request",
            "src/utils/http.js",
            2,
        ),
    ];
    for fixture_name in ["configs-below-root", "jsconfig-root"] {
        let dir = tempfile::tempdir().expect("temp dir");
        let repo = fs::canonicalize(dir.path()).unwrap().join("repo");
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(fixture_name)
                .join("repo"),
            &repo,
        );
        let store = tempfile::tempdir().unwrap();
        let on = scan(&repo, true, false, Some(store.path()), None);
        let rows_file = on
            .positions
            .as_ref()
            .expect("a scan with the flag wrote rows")
            .join(format!(
                "{}.jsonl.gz",
                on.blobs.keys().next().unwrap().trim_end_matches(".json")
            ));
        let rows = read_rows_file(&rows_file).expect("the scan's rows file reads");
        for file in &rows.files {
            assert!(file.skip.is_none(), "{} is not in a program", file.f);
            for row in &file.r {
                assert_eq!(
                    row.flags() & FLAG_ERROR_TYPE,
                    0,
                    "{}: a row is the error type: {row:?}",
                    file.f
                );
            }
        }
        for (_, file, name, declared_in, calls) in
            uses.iter().filter(|u| u.0 == fixture_name && u.4 > 0)
        {
            let rows_of = rows.files.iter().find(|f| f.f == *file).unwrap();
            let found = rows_of
                .r
                .iter()
                .filter(|row| {
                    row.kind() == Kind::Call
                        && row.symbol() == Some(*name)
                        && row.decl_file() == Some(*declared_in)
                })
                .count();
            assert_eq!(
                found, *calls,
                "{file}: calls of {name} declared in {declared_in}"
            );
        }
    }
}

/// The census (`#[ignore]`): for each `<repo>:<rows file>` in
/// `CARRICK_POSITION_ROWS_CENSUS` (`;`-separated), parse every file the rows
/// name with SWC and print, per kind, the share of SWC positions that found a
/// row, and of the rows: resolved %, any %, error-type % and bytes per row.
///
/// `CARRICK_POSITION_ROWS_CENSUS=/repo:/store/positions/repo__api.jsonl.gz \
///  cargo test --test position_rows_test census -- --ignored --nocapture`
#[test]
#[ignore]
fn census() {
    let spec = std::env::var("CARRICK_POSITION_ROWS_CENSUS")
        .expect("CARRICK_POSITION_ROWS_CENSUS=<repo>:<rows file>[;...]");
    for entry in spec.split(';').filter(|entry| !entry.is_empty()) {
        let (repo, rows_path) = entry.split_once(':').expect("<repo>:<rows file>");
        let repo = Path::new(repo);
        let rows_path = Path::new(rows_path);
        let bytes = fs::metadata(rows_path).map(|m| m.len()).unwrap_or(0);
        let mut raw = String::new();
        std::io::Read::read_to_string(
            &mut flate2::read::GzDecoder::new(fs::File::open(rows_path).expect("open rows")),
            &mut raw,
        )
        .expect("decompress rows");
        let rows = read_rows(raw.as_bytes()).expect("the rows file reads");
        let mut swc: HashMap<Kind, (usize, usize)> = HashMap::new();
        let mut held: HashMap<Kind, [usize; 4]> = HashMap::new();
        let mut misses_shown: HashMap<Kind, usize> = HashMap::new();
        let mut stale = 0usize;
        let mut unparsed = 0usize;
        let mut total_rows = 0usize;
        for line in &rows.files {
            for row in &line.r {
                total_rows += 1;
                let counts = held.entry(row.kind()).or_default();
                counts[0] += 1;
                counts[1] += usize::from(row.symbol().is_some());
                counts[2] += usize::from(row.flags() & FLAG_ANY != 0);
                counts[3] += usize::from(row.flags() & FLAG_ERROR_TYPE != 0);
            }
            let file = repo.join(&line.f);
            let Ok(source) = fs::read(&file) else {
                stale += 1;
                continue;
            };
            if line.h.as_deref() != Some(content_hash(&source).as_str()) {
                stale += 1;
                continue;
            }
            if fs::read_to_string(&file).is_err() || line.skip.is_some() {
                unparsed += 1;
                continue;
            }
            let keys: BTreeSet<(Kind, u32, u32)> = line
                .r
                .iter()
                .map(|row| (row.kind(), row.start(), row.end()))
                .collect();
            let positions = std::panic::catch_unwind(|| swc_positions(&file));
            let Ok(positions) = positions else {
                unparsed += 1;
                continue;
            };
            for position in positions {
                let counts = swc.entry(position.0).or_default();
                counts.0 += 1;
                let found = keys.contains(&position);
                counts.1 += usize::from(found);
                if !found && misses_shown.get(&position.0).copied().unwrap_or(0) < 5 {
                    *misses_shown.entry(position.0).or_default() += 1;
                    let text = String::from_utf8_lossy(&source);
                    let snippet: String = text
                        .encode_utf16()
                        .skip(position.1 as usize)
                        .take((position.2 - position.1).min(60) as usize)
                        .map(|unit| char::from_u32(u32::from(unit)).unwrap_or('?'))
                        .collect();
                    println!(
                        "    miss {:?} {}:{}-{} {:?}",
                        position.0, line.f, position.1, position.2, snippet
                    );
                }
            }
        }
        let pct = |part: usize, whole: usize| {
            if whole == 0 {
                0.0
            } else {
                100.0 * part as f64 / whole as f64
            }
        };
        println!(
            "{}: {} file(s), {} row(s), {:.1} B/row raw, {:.1} B/row gzipped, {} stale, {} unparsed",
            rows_path.display(),
            rows.files.len(),
            total_rows,
            raw.len() as f64 / total_rows.max(1) as f64,
            bytes as f64 / total_rows.max(1) as f64,
            stale,
            unparsed
        );
        for kind in [Kind::Call, Kind::Member, Kind::Compare, Kind::Ident] {
            let (positions, found) = swc.get(&kind).copied().unwrap_or_default();
            let [count, resolved, any, error] = held.get(&kind).copied().unwrap_or_default();
            println!(
                "  {kind:?}: join {:.2}% of {} SWC positions; rows {}: resolved {:.1}%, any {:.1}%, error type {:.1}%",
                pct(found, positions),
                positions,
                count,
                pct(resolved, count),
                pct(any, count),
                pct(error, count)
            );
        }
    }
}

/// The calls a scan of `tests/fixtures/position-rows-names/` states, run as
/// the Action runs it (`.` from the repo root) with the real sidecar, with
/// or without the walk, and with or without its module that imports a
/// package nothing declares (`src/other.ts`). Each call as its file, line,
/// source, method and URL.
fn names_scan_calls(rows_on: bool, unfollowable: bool) -> BTreeSet<String> {
    let dir = tempfile::tempdir().expect("temp dir");
    let repo = fs::canonicalize(dir.path())
        .expect("canonical temp dir")
        .join("repo");
    copy_tree(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/position-rows-names/repo"),
        &repo,
    );
    if !unfollowable {
        fs::remove_file(repo.join("services/names/src/other.ts")).expect("remove other.ts");
    }
    let cache = tempfile::tempdir().expect("cache dir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_carrick"));
    cmd.arg(".")
        .current_dir(&repo)
        .env("CARRICK_CACHE_DIR", cache.path())
        .env("CARRICK_MOCK_ALL", "1")
        .env("CARRICK_OUTPUT_JSON", "1")
        .env("CARRICK_SKIP_INTENTS", "1")
        .env_remove("CARRICK_SIDECAR_DIR")
        .env_remove("CARRICK_LOCAL_STORAGE_DIR");
    if rows_on {
        cmd.env("CARRICK_POSITION_ROWS", "1");
    } else {
        cmd.env_remove("CARRICK_POSITION_ROWS");
    }
    for var in [
        "GITHUB_REPOSITORY",
        "GITHUB_ACTIONS",
        "CI",
        "ACTIONS_ID_TOKEN_REQUEST_URL",
        "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    let output = cmd.output().expect("spawn carrick");
    assert!(
        output.status.success(),
        "scan exited non-zero:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let projection: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("the scan prints its projection");
    projection["calls"]
        .as_array()
        .expect("the projection carries calls")
        .iter()
        .map(|call| {
            format!(
                "{}:{} {} {} {}",
                call["file"].as_str().unwrap_or_default(),
                call["line"],
                call["resolution_source"].as_str().unwrap_or_default(),
                call["method"].as_str().unwrap_or_default(),
                call["target_url"].as_str().unwrap_or_default()
            )
        })
        .collect()
}

/// carrick#2413 slice B, end to end (carrick#2402): a request whose URL is
/// built from a constant another module declares (`api.ts`, called from
/// `page.ts`). Without the walk, a module that imports a package nothing
/// declares and only calls it turns every imported constant off, so the
/// summary states the call with the constant unread. With the walk, that
/// module changes nothing: the scan states what it states without it.
#[test]
fn an_unrelated_unfollowable_import_changes_no_call_with_rows() {
    if !sidecar_available() {
        return;
    }
    let unread = "services/names/src/page.ts:4 request_summary GET ${API_BASE}/users";
    let followable = names_scan_calls(false, false);
    assert!(!followable.contains(unread), "{followable:#?}");
    let off = names_scan_calls(false, true);
    assert!(off.contains(unread), "{off:#?}");
    assert_eq!(names_scan_calls(true, true), followable);
    assert_eq!(names_scan_calls(true, false), followable);
}
