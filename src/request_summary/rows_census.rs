//! The census of the compiler rows' lookups (carrick#2413 slice A): for each
//! lookup the request summaries make by name today, what the row at the
//! same position says. A measurement, not a check; offline and model-free.
//!
//! Per repo it counts the rows (by kind, with the `any` and error-type
//! shares), the files the rows name by state (fresh, stale, not in any
//! program), and four sets of lookup sites, each as today's answer against
//! the row's (agree, differ by kind, or hole by reason):
//!
//! 1. `imports`: every expression-position use of an import binding a file's
//!    summaries read ([`FileIr::import_uses`]), against what the call graph's
//!    resolution names ([`crate::call_graph::ImportedBindings::get`]):
//!    the lookups of `LinkedNames`, `LinkedClients` and `GetterHolders`.
//!    The bindings whose uses name more than one declaration are counted.
//! 2. `calls`: every call site the call graph resolved
//!    ([`crate::call_graph::CallSiteTargets::sites_in`]), against the call
//!    row's declaration file and line.
//! 3. `globals`: every expression-position identifier the module neither
//!    declares nor imports: the `fetch`, `URL`, inert-global and collection
//!    names apart from the rest (`Root::Global`'s population).
//! 4. `packages`: every use of a binding imported from a specifier that is
//!    not relative (`import_receivers`), against the row's `decl_package`.
//!
//! Each repo's rows files come from a flag-on scan with a local store
//! (`<store>/positions`):
//!
//! `CARRICK_ROWS_CENSUS='<label>=<repo>=<positions dir>[;…]'
//! [CARRICK_ROWS_CENSUS_OUT=<file>] cargo test --lib rows_census -- --ignored --nocapture`

use serde::Serialize;

use super::row_links::RowLink;
use super::*;
use crate::config::Config;
use crate::position_rows::{FLAG_ANY, FLAG_ERROR_TYPE, Kind, PositionIndex, Row};

/// Counts by key.
#[derive(Debug, Default, Serialize)]
struct Tally(BTreeMap<String, usize>);

impl Tally {
    fn add(&mut self, key: impl Into<String>) {
        *self.0.entry(key.into()).or_default() += 1;
    }
    fn total(&self) -> usize {
        self.0.values().sum()
    }
    fn found(&self) -> usize {
        self.0
            .iter()
            .filter(|(key, _)| !key.contains("hole:"))
            .map(|(_, count)| count)
            .sum()
    }
}

fn hole(reason: HoleReason) -> String {
    format!(
        "hole:{}",
        serde_json::to_value(reason)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default()
    )
}

/// Where a row's declaration is: the default library, a package, a repo
/// file, or unnamed.
fn place(row: &Row) -> &'static str {
    match row.decl_file() {
        None => "unnamed",
        Some(file) if file.starts_with("lib:") => "lib",
        Some(file) if file.starts_with("pkg:") || file.split('/').any(|p| p == "node_modules") => {
            "package"
        }
        Some(_) => "repo",
    }
}

/// Every expression-position identifier of a module, with its span.
#[derive(Default)]
struct AllIdents(Vec<(String, u32, u32)>);

impl Visit for AllIdents {
    fn visit_ts_type(&mut self, _: &TsType) {}
    fn visit_ts_interface_body(&mut self, _: &TsInterfaceBody) {}
    fn visit_ts_expr_with_type_args(&mut self, _: &TsExprWithTypeArgs) {}
    fn visit_expr(&mut self, expr: &Expr) {
        if let Expr::Ident(ident) = expr {
            self.0
                .push((ident.sym.to_string(), ident.span.lo.0, ident.span.hi.0));
        }
        expr.visit_children_with(self);
    }
}

#[derive(Debug, Default, Serialize)]
struct RowCounts {
    rows: usize,
    any: usize,
    error_type: usize,
}

#[derive(Debug, Default, Serialize)]
struct RepoCensus {
    services: usize,
    services_without_rows: Vec<String>,
    rows: BTreeMap<String, RowCounts>,
    files: BTreeMap<String, usize>,
    imports: Tally,
    /// Import bindings (per file) whose uses name more than one declaration.
    imports_disagreeing: usize,
    import_bindings: usize,
    /// What each import binding reaches through the rows (slice B), against
    /// where today's walk ends: `declared/agree`, `declared/differ`, and each
    /// other link kind marked `/today_declared` where today reads a
    /// declaration the rows do not give (a loss risk).
    links: Tally,
    /// The services whose exported values the rows contest, with why.
    open_services: Vec<String>,
    calls: Tally,
    /// For a call row in the target's file on another line: decl − target.
    call_line_deltas: Tally,
    globals: Tally,
    packages: Tally,
    /// Error-type identifier rows, and import-use holes for an unresolved
    /// import, by service: where #1910's share sits.
    error_idents_by_service: BTreeMap<String, usize>,
    unresolved_import_uses_by_service: BTreeMap<String, usize>,
    /// A few sites per differ kind, for reading by hand.
    examples: BTreeMap<String, Vec<String>>,
}

impl RepoCensus {
    fn example(&mut self, key: &str, text: String) {
        let list = self.examples.entry(key.to_string()).or_default();
        if list.len() < 8 {
            list.push(text);
        }
    }
}

/// Where today's by-name walk ends for `local`, imported in `file`: the
/// module-scope binding, of a file the service reads, that the call graph's
/// resolution names, followed through the modules that re-export it as the
/// summaries' tables follow it.
fn today_declaration(
    inputs: &RequestSummaryInputs,
    file: &Path,
    local: &str,
) -> Option<(PathBuf, String)> {
    let Some(ImportedBinding::Binding { file, name }) = inputs.bindings.get(file, local) else {
        return None;
    };
    let mut at = (file.clone(), name.clone());
    for _ in 0..=MAX_REEXPORT_HOPS {
        let ir = inputs.files.get(&at.0)?;
        if !ir.imported.contains_key(&at.1) {
            return Some(at);
        }
        match inputs.bindings.get(&at.0, &at.1)? {
            ImportedBinding::Binding { file, name } => at = (file.clone(), name.clone()),
            _ => return None,
        }
    }
    None
}

fn census_repo(root: &Path, positions: &Path) -> RepoCensus {
    let root = std::fs::canonicalize(root).expect("the repo exists");
    let config = root.join("carrick.json");
    let services = if config.exists() {
        Config::load_services(vec![config]).expect("carrick.json loads")
    } else {
        vec![Config::default()]
    };
    // Each rows file by the service its header names.
    let mut by_service: BTreeMap<String, PathBuf> = BTreeMap::new();
    for entry in std::fs::read_dir(positions).expect("the positions dir reads") {
        let path = entry.expect("dir entry").path();
        if path.to_string_lossy().ends_with(".jsonl.gz")
            && let Ok(rows) = crate::position_rows::read_rows_file(&path)
        {
            by_service.insert(rows.header.service.clone(), path);
        }
    }
    let mut census = RepoCensus {
        services: services.len(),
        ..RepoCensus::default()
    };
    for service in &services {
        let label = service
            .service_name
            .clone()
            .unwrap_or_else(|| "(root)".to_string());
        let index = match by_service.get(&label) {
            Some(path) => {
                let rows = crate::position_rows::read_rows_file(path).expect("rows read");
                for file in &rows.files {
                    for row in &file.r {
                        let counts = census.rows.entry(format!("{:?}", row.kind())).or_default();
                        counts.rows += 1;
                        counts.any += usize::from(row.flags() & FLAG_ANY != 0);
                        let error = row.flags() & FLAG_ERROR_TYPE != 0;
                        counts.error_type += usize::from(error);
                        if error && row.kind() == Kind::Ident {
                            *census
                                .error_idents_by_service
                                .entry(label.clone())
                                .or_default() += 1;
                        }
                    }
                }
                PositionIndex::of(&root, rows)
            }
            None => {
                census.services_without_rows.push(label.clone());
                PositionIndex::missing(&root)
            }
        };
        let inputs = crate::engine::discover_service_inputs(&root, service, Some(index));
        let positions = inputs.positions.as_ref().expect("bound");
        for (state, count) in positions.file_states() {
            *census.files.entry(state.to_string()).or_default() += count;
        }
        let relative = |file: &Path| {
            file.strip_prefix(&root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/")
        };
        // 1b. What each import binding reaches through the rows (slice B),
        // against where today's by-name walk ends.
        let links = inputs.links.as_ref().expect("linked with rows");
        if links.open() {
            census
                .open_services
                .push(format!("{label}: {}", links.describe(&root)));
        }
        let mut files: Vec<&PathBuf> = inputs.files.keys().collect();
        files.sort();
        for file in &files {
            let mut locals: Vec<&String> = inputs.files[*file].imported.keys().collect();
            locals.sort();
            for local in locals {
                let today = today_declaration(&inputs, file, local);
                let link = links.get(file, local).expect("every import is linked");
                let kind = match link {
                    RowLink::Declared(at, name) => match &today {
                        Some(today) if today == &(at.clone(), name.clone()) => "declared/agree",
                        Some(_) => {
                            census.example(
                                "links/declared/differ",
                                format!(
                                    "{} {local}: today {today:?}, row {}:{name}",
                                    relative(file),
                                    relative(at)
                                ),
                            );
                            "declared/differ"
                        }
                        None => "declared/today_none",
                    },
                    other => {
                        let kind = match other {
                            RowLink::Unrecorded => "unrecorded",
                            RowLink::Module(_) => "module",
                            RowLink::Outside(_) => "outside",
                            RowLink::OtherService => "other_service",
                            RowLink::Hole(reason) => {
                                census.links.add(format!(
                                    "{}{}",
                                    hole(*reason),
                                    if today.is_some() {
                                        "/today_declared"
                                    } else {
                                        ""
                                    }
                                ));
                                continue;
                            }
                            RowLink::Declared(..) => unreachable!(),
                        };
                        if today.is_some() {
                            census.example(
                                &format!("links/{kind}/today_declared"),
                                format!("{} {local}: today {today:?}", relative(file)),
                            );
                            census.links.add(format!("{kind}/today_declared"));
                            continue;
                        }
                        kind
                    }
                };
                census.links.add(kind);
            }
        }
        for file in files {
            let ir = &inputs.files[file];
            let rel = relative(file);

            // 1. Imports.
            for (name, spans) in ir.import_uses() {
                census.import_bindings += 1;
                let today = match inputs.bindings.get(file, name) {
                    Some(ImportedBinding::Binding {
                        file: declaring, ..
                    }) => Some(relative(declaring)),
                    _ => None,
                };
                let today_kind = match inputs.bindings.get(file, name) {
                    Some(ImportedBinding::Binding { .. }) => "binding",
                    Some(ImportedBinding::Module(_)) => "module",
                    Some(ImportedBinding::Unfollowable) => "unfollowable",
                    Some(ImportedBinding::Unresolved) => "unresolved",
                    None => "unfollowed",
                };
                let mut declarations = BTreeSet::new();
                for (lo, hi) in spans {
                    let key = match positions.declaration(file, lo, hi, Kind::Ident, true) {
                        Err(reason) => {
                            if reason == HoleReason::UnresolvedImport {
                                *census
                                    .unresolved_import_uses_by_service
                                    .entry(label.clone())
                                    .or_default() += 1;
                            }
                            hole(reason)
                        }
                        Ok(row) => {
                            declarations
                                .insert((row.decl_file().map(str::to_string), row.decl_line()));
                            // Read only through the other-file rule (slice B):
                            // before it, this use was `hole:unresolved_import`.
                            let error_type = if row.flags() & FLAG_ERROR_TYPE != 0 {
                                "/error_type"
                            } else {
                                ""
                            };
                            let key = match (&today, place(row)) {
                                (Some(declaring), "repo")
                                    if row.decl_file() == Some(declaring.as_str()) =>
                                {
                                    "same_file".to_string()
                                }
                                (Some(declaring), "repo") => {
                                    census.example(
                                        "imports/binding/other_file",
                                        format!(
                                            "{rel} {name}: today {declaring}, row {}:{}",
                                            row.decl_file().unwrap_or_default(),
                                            row.decl_line().unwrap_or_default()
                                        ),
                                    );
                                    "other_file".to_string()
                                }
                                (_, place) => place.to_string(),
                            };
                            format!("{key}{error_type}")
                        }
                    };
                    census.imports.add(format!("{today_kind}/{key}"));
                }
                if declarations.len() > 1 {
                    census.imports_disagreeing += 1;
                    census.example(
                        "imports/disagreeing",
                        format!("{rel} {name}: {declarations:?}"),
                    );
                }
            }

            // 2. Calls.
            for (lo, hi, target, line) in inputs.sites.sites_in(file) {
                let (lo, hi) = (ir.swc_start(lo), ir.swc_start(hi));
                let key = match positions.declaration(file, lo, hi, Kind::Call, false) {
                    Err(reason) => {
                        // A JSX element is a call site to the call graph and
                        // no row kind to the walk.
                        let mut key = hole(reason);
                        if reason == HoleReason::RowsMissing
                            && let Ok(text) = std::fs::read_to_string(file)
                        {
                            let body = text.strip_prefix('\u{feff}').unwrap_or(&text);
                            let start = (lo - SWC_SPAN_BASE) as usize;
                            let end = ((hi - SWC_SPAN_BASE) as usize).min(start + 60);
                            let snippet = body.get(start..end).unwrap_or_default();
                            if snippet.starts_with('<') {
                                key.push_str("/jsx_element");
                            } else {
                                census.example(
                                    "calls/rows_missing",
                                    format!("{rel}:{lo}-{hi}: {snippet:?}"),
                                );
                            }
                        }
                        key
                    }
                    Ok(row) => {
                        let target = relative(target);
                        if row.decl_file() == Some(target.as_str()) {
                            if row.decl_line() == Some(line) {
                                "agree".to_string()
                            } else {
                                let delta =
                                    i64::from(row.decl_line().unwrap_or(0)) - i64::from(line);
                                census.call_line_deltas.add(delta.clamp(-5, 5).to_string());
                                census.example(
                                    "calls/other_line",
                                    format!(
                                        "{rel}: target {target}:{line}, row line {:?}",
                                        row.decl_line()
                                    ),
                                );
                                "other_line".to_string()
                            }
                        } else {
                            census.example(
                                "calls/other_file",
                                format!(
                                    "{rel}: target {target}:{line}, row {:?}:{:?} ({})",
                                    row.decl_file(),
                                    row.decl_line(),
                                    row.symbol().unwrap_or_default()
                                ),
                            );
                            format!("other_{}", place(row))
                        }
                    }
                };
                census.calls.add(key);
            }

            // 3 and 4 read the module again, standalone, as the rows were.
            let Some(text) = crate::parser::read_source(file) else {
                continue;
            };
            let Some((_, module)) = crate::swc_scanner::parse_standalone_module(file, &text) else {
                continue;
            };
            let declared = crate::binding_scope::Declarations::of(&module);
            let (imports, _) = import_bindings(&module);
            let mut idents = AllIdents::default();
            module.visit_with(&mut idents);
            for (name, lo, hi) in &idents.0 {
                if let Some(import) = imports.get(name) {
                    // 4. Packages.
                    if import.namespace
                        || import.specifier.starts_with('.')
                        || import.specifier.starts_with('/')
                    {
                        continue;
                    }
                    let key = match positions.declaration(file, *lo, *hi, Kind::Ident, true) {
                        Err(reason) => hole(reason),
                        Ok(row) => {
                            let wanted = package_name(&import.specifier);
                            match (place(row), row.decl_package()) {
                                ("repo", _) => {
                                    census.example(
                                        "packages/repo",
                                        format!(
                                            "{rel} {name} from {}: row {:?}",
                                            import.specifier,
                                            row.decl_file()
                                        ),
                                    );
                                    "repo".to_string()
                                }
                                (_, Some(package)) if package == wanted => {
                                    "same_package".to_string()
                                }
                                (_, Some(package)) => {
                                    census.example(
                                        "packages/other_package",
                                        format!(
                                            "{rel} {name} from {}: row package {package}",
                                            import.specifier
                                        ),
                                    );
                                    "other_package".to_string()
                                }
                                (place, None) => format!("no_package_{place}"),
                            }
                        }
                    };
                    census.packages.add(key);
                    continue;
                }
                if declared.declares_name(name) {
                    continue;
                }
                // 3. Globals.
                let group = if name == "fetch" || name == "URL" {
                    name.as_str()
                } else if INERT_GLOBALS.contains(&name.as_str()) {
                    "inert_global"
                } else if COLLECTIONS.contains(&name.as_str()) {
                    "collection"
                } else {
                    "other_free_name"
                };
                let key = match positions.declaration(file, *lo, *hi, Kind::Ident, false) {
                    Err(reason) => hole(reason),
                    Ok(row) => {
                        let place = place(row);
                        if place == "repo" {
                            census.example(
                                "globals/repo",
                                format!("{rel} {name}: row {:?}", row.decl_file()),
                            );
                        }
                        place.to_string()
                    }
                };
                census.globals.add(format!("{group}/{key}"));
            }
        }
    }
    census
}

fn pct(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        100.0 * part as f64 / whole as f64
    }
}

fn top3(by_service: &BTreeMap<String, usize>) -> Vec<(String, usize)> {
    let mut sorted: Vec<_> = by_service
        .iter()
        .map(|(service, count)| (service.clone(), *count))
        .collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    sorted.truncate(3);
    sorted
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// The reader over `tests/fixtures/position-rows-reader/`, walked by the
/// real sidecar: an imported constant and instance, a member call through a
/// typed receiver, the global `fetch` and `URL`, a shadowed `URL`, a name
/// nothing declares, a file edited after the walk and a `.mjs` file in a
/// service whose tsconfig has no `allowJs`, which the walk reads in the
/// config's `scripts` program (carrick#2413). Each lookup reads what the
/// compiler says there, or a hole with its reason.
#[test]
fn the_reader_answers_each_lookup_or_says_why_not() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = manifest.join("src/sidecar/dist/src/index.js");
    if !script.exists()
        || std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_err()
    {
        assert!(
            std::env::var("CI").is_err(),
            "node or the built sidecar is missing in CI"
        );
        crate::errln!("Skipping: node or the built sidecar is missing");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(dir.path()).unwrap().join("repo");
    copy_tree(
        &manifest.join("tests/fixtures/position-rows-reader/repo"),
        &repo,
    );
    let service = Config::load_services(vec![repo.join("carrick.json")])
        .unwrap()
        .remove(0);
    let mut files = crate::file_finder::walk_service(&repo.to_string_lossy(), &service, &[]).files;
    files.sort();
    let positions = dir.path().join("positions");
    let out = positions.join("repo__api.jsonl.gz");
    let sidecar = crate::services::TypeSidecar::spawn(&script).expect("spawn the sidecar");
    sidecar.set_scan_root(&repo);
    let recipe = sidecar.recipe();
    drop(sidecar);
    let service_root = repo.join("apps/api");
    crate::position_rows::record_service_rows(
        &recipe,
        &crate::position_rows::ServiceWalk {
            service_root: &service_root,
            tsconfig: Some("tsconfig.json"),
            scan_root: &repo,
            files: &files,
            out: &out,
            service: "api",
        },
    )
    .expect("the walk writes its rows");
    // Edited after the walk: its rows are stale.
    let edited = service_root.join("src/edited.ts");
    let mut text = std::fs::read_to_string(&edited).unwrap();
    text.push_str("export const later = 1;\n");
    std::fs::write(&edited, text).unwrap();

    let census = census_repo(&repo, &positions);
    assert!(census.services_without_rows.is_empty());
    assert_eq!(census.files.get("stale"), Some(&1), "{:?}", census.files);
    assert_eq!(census.files.get("fresh"), Some(&4), "{:?}", census.files);
    assert_eq!(
        census.files.get("not_in_program"),
        None,
        "{:?}",
        census.files
    );
    let count = |tally: &Tally, key: &str| tally.0.get(key).copied().unwrap_or(0);
    // `BASE_PATH` twice and `items` once in main.ts, and once in the `.mjs`
    // file, reach client.ts, as the call graph says; the use in the edited
    // file is stale.
    assert_eq!(
        count(&census.imports, "binding/same_file"),
        4,
        "{:?}",
        census.imports
    );
    assert_eq!(
        count(&census.imports, "binding/hole:rows_stale"),
        1,
        "{:?}",
        census.imports
    );
    assert_eq!(
        count(&census.imports, "binding/hole:not_in_program"),
        0,
        "{:?}",
        census.imports
    );
    assert_eq!(census.imports_disagreeing, 0);
    // The globals main.ts uses are the default library's; a name nothing
    // declares is unresolved. The shadowed `URL` is declared, so it is no
    // global today and is not counted as one.
    assert_eq!(
        count(&census.globals, "fetch/lib"),
        2,
        "{:?}",
        census.globals
    );
    assert_eq!(count(&census.globals, "URL/lib"), 1, "{:?}", census.globals);
    assert_eq!(
        count(&census.globals, "other_free_name/hole:unresolved_name"),
        1,
        "{:?}",
        census.globals
    );
    // Every call the call graph resolved finds its call row.
    assert!(census.calls.total() > 0, "{:?}", census.calls);
    assert_eq!(
        census.calls.found(),
        census.calls.total(),
        "{:?}",
        census.calls
    );
}

#[test]
#[ignore = "a measurement over the repos named by CARRICK_ROWS_CENSUS"]
fn census() {
    let Ok(spec) = std::env::var("CARRICK_ROWS_CENSUS") else {
        return;
    };
    let mut all = BTreeMap::new();
    for entry in spec.split(';').filter(|entry| !entry.is_empty()) {
        let mut parts = entry.splitn(3, '=');
        let (Some(label), Some(root), Some(positions)) = (parts.next(), parts.next(), parts.next())
        else {
            panic!("<label>=<repo>=<positions dir>: {entry}");
        };
        let census = census_repo(Path::new(root), Path::new(positions));
        crate::outln!(
            "== {label}: {} service(s), {} without rows {:?}",
            census.services,
            census.services_without_rows.len(),
            census.services_without_rows
        );
        let total: usize = census.rows.values().map(|c| c.rows).sum();
        crate::outln!("  rows {total}");
        for (kind, counts) in &census.rows {
            crate::outln!(
                "    {kind}: {} rows, any {:.1}%, error type {:.1}%",
                counts.rows,
                pct(counts.any, counts.rows),
                pct(counts.error_type, counts.rows)
            );
        }
        crate::outln!("  files {:?}", census.files);
        for (name, tally) in [
            ("imports", &census.imports),
            ("calls", &census.calls),
            ("globals", &census.globals),
            ("packages", &census.packages),
        ] {
            crate::outln!(
                "  {name}: {} site(s), {} find a row ({:.1}%)",
                tally.total(),
                tally.found(),
                pct(tally.found(), tally.total())
            );
            for (key, count) in &tally.0 {
                crate::outln!("    {key}: {count}");
            }
        }
        crate::outln!(
            "  import bindings {}, whose uses disagree {}",
            census.import_bindings,
            census.imports_disagreeing
        );
        crate::outln!("  links (slice B): {} binding(s)", census.links.total());
        for (key, count) in &census.links.0 {
            crate::outln!("    {key}: {count}");
        }
        crate::outln!(
            "  services whose exported values are contested: {}",
            census.open_services.len()
        );
        for service in &census.open_services {
            crate::outln!("    {service}");
        }
        crate::outln!("  call line deltas {:?}", census.call_line_deltas.0);
        crate::outln!(
            "  error-type ident rows, top 3 services {:?}",
            top3(&census.error_idents_by_service)
        );
        crate::outln!(
            "  unresolved-import use holes, top 3 services {:?}",
            top3(&census.unresolved_import_uses_by_service)
        );
        for (key, examples) in &census.examples {
            crate::outln!("  e.g. {key}:");
            for example in examples {
                crate::outln!("      {example}");
            }
        }
        all.insert(label.to_string(), census);
    }
    if let Ok(out) = std::env::var("CARRICK_ROWS_CENSUS_OUT") {
        std::fs::write(out, serde_json::to_string_pretty(&all).unwrap()).unwrap();
    }
}
