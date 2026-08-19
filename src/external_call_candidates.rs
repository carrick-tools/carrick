//! Outbound call candidates: the scanner's egress channel.
//!
//! Two sources feed one row shape.
//!
//! **SDK-mediated calls** ([`scan_files`]) are detected here, deterministically
//! and AST-only. The scanner's existing outbound-call candidates are
//! HTTP-shaped: `fetch` / axios-style calls, env-var-anchored URLs, absolute
//! URLs. A call made through a service client imported from an npm package
//! never looks like that, so it is invisible to candidate generation and to
//! every surface downstream of it.
//!
//! **HTTP-shaped calls** ([`from_data_calls`]) are already extracted and
//! already classified; they are projected into this channel rather than
//! detected again. A call to a host the repo declares in `externalDomains`, or
//! through a base declared in `externalEnvVars`, is excluded from endpoint
//! matching by design — which is correct, and which used to mean the fact of
//! the call was recorded nowhere an egress inventory could read. Projecting the
//! row changes no classification and no matching; it records what the
//! classification already decided.
//!
//! The SDK detection is intentionally narrow:
//!
//! - **Structural, not name-based.** A call becomes a candidate when its callee
//!   traces, through this file's own imports, to a package that the service's
//!   `package.json` declares as a runtime dependency. There is no allowlist and
//!   no denylist of SDK or vendor names anywhere in this module — the
//!   dependency set comes from the repo's own manifests.
//! - **No type checking.** Resolution is limited to what the AST proves:
//!   an import binding, and a variable whose initializer is rooted at one. A
//!   receiver obtained any other way (returned from a helper, read off a DI
//!   container, a class field) is not resolved and emits nothing. Recording
//!   less than the truth is the deliberate choice over guessing.
//! - **Never merged into HTTP matching.** These rows are their own mechanism
//!   and their own payload field. They are not endpoints, they are not consumer
//!   calls, and nothing here touches extraction, matching, or type
//!   compatibility. Folding SDK traffic into HTTP endpoint matching is the
//!   known false-positive class this design exists to avoid.
//!
//! These are candidates, not verified egress. Any declared dependency
//! qualifies, so a web framework, a local crypto library, and a validation
//! library all produce rows alongside genuine service clients, which is the
//! intended trade of recall in the scanner against classification downstream.
//!
//! Known limitations, all deliberate for this slice and all silent rather than
//! guessed:
//!
//! - ESM `import` declarations only. `require()` and TypeScript `import =`
//!   bindings produce no rows.
//! - Type-only imports are excluded, both `import type ...` and per-specifier
//!   `import { type Foo }`.
//! - Destructuring off a resolved binding (`const { charges } = client`) is not
//!   followed.
//! - Constructor calls (`new Client(...)`) resolve the receiver without becoming
//!   rows themselves, because constructing a client is not egress.

use crate::analyzer::Analyzer;
use crate::config::Config;
use crate::mount_graph::DataFetchingCall;
use crate::packages::Packages;
use crate::type_manifest::parse_file_location;
use crate::url_normalizer::UrlNormalizer;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use swc_common::{FileName, GLOBALS, Globals, Mark, SourceMap, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
use swc_ecma_transforms_base::resolver;
use swc_ecma_visit::{Visit, VisitMutWith, VisitWith};
use tracing::warn;

/// Upper bound on the rows one service contributes to the upload payload.
/// Rows are sorted before truncation, so a repo over the cap yields a stable
/// prefix rather than an arbitrary sample. The cap exists so a very large
/// monorepo cannot push the payload towards the Lambda request limit; it is
/// generous enough that no realistic service reaches it.
pub const MAX_CANDIDATES_PER_SERVICE: usize = 5000;

/// How the scanner established that a call site leaves the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallMechanism {
    /// The callee resolves, through an import, to a declared runtime dependency.
    Sdk,
    /// An extracted HTTP call whose target names a host the repo declares in
    /// `externalDomains`.
    ExternalHttp,
    /// An extracted HTTP call whose base URL is an environment variable the
    /// repo declares in `externalEnvVars`.
    EnvVarUrl,
}

/// One outbound call candidate.
///
/// Ordering is `(file, line, callee, package, mechanism)`, which is also the
/// sort key rows are emitted in. SDK rows are byte-identical across runs of the
/// same tree because they are pure AST; the HTTP-shaped rows are projected from
/// LLM extraction and inherit its stability, so the ORDER is fixed but the row
/// set is only as reproducible as the extraction behind it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExternalCallCandidate {
    /// Repo-relative path of the file containing the call.
    pub file: String,
    /// 1-based line number of the call expression.
    pub line: usize,
    /// What was called, in the terms the mechanism knows.
    ///
    /// For [`CallMechanism::Sdk`], the callee as written, dotted
    /// (`ledger.payments.create`), computed from the AST so it carries no
    /// whitespace or comments from the source. For the two HTTP-shaped
    /// mechanisms there is no callee to name — the client is `fetch` or an
    /// axios-alike in every case — so the field carries the HTTP method, which
    /// is the operation the row records.
    pub callee: String,
    /// Where the call goes, in the terms the mechanism knows: the declared
    /// dependency for [`CallMechanism::Sdk`], exactly as the `package.json`
    /// names it (a subpath import such as `pkg/edge` resolves to `pkg`); the
    /// hostname for [`CallMechanism::ExternalHttp`]; the environment variable
    /// name for [`CallMechanism::EnvVarUrl`].
    ///
    /// The field keeps the name it shipped with in carrick#511, when `sdk` was
    /// the only mechanism. cloud#350 calls the same column `target`.
    pub package: String,
    /// Which of the three detections produced this row.
    pub mechanism: CallMechanism,
}

/// Scan `files` for SDK-mediated call candidates.
///
/// `files` is expected to be the service's already-filtered source list from
/// [`crate::file_finder::find_service_files`], so test trees, story files, and
/// vendored/build directories are excluded by the scanner's existing rules
/// rather than by anything invented here.
pub fn scan_files(
    files: &[PathBuf],
    repo_root: &Path,
    packages: &Packages,
) -> Vec<ExternalCallCandidate> {
    let dependencies = runtime_dependency_names(packages);
    if dependencies.is_empty() {
        return Vec::new();
    }

    let mut rows: BTreeSet<ExternalCallCandidate> = BTreeSet::new();
    for file in files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        let relative = relative_path(file, repo_root);
        rows.extend(scan_content(&relative, file, &content, &dependencies));
    }

    cap(rows.into_iter().collect())
}

/// Project the already-classified HTTP-shaped calls of a service into channel
/// rows.
///
/// `data_calls` are the consumer calls of the built mount graph, so every row
/// here corresponds to a call the scanner already extracted, already keyed, and
/// already classified. Nothing is re-detected and nothing is re-classified:
/// this reads `host` (retained by the mount-graph builder from normalisation)
/// and the same `externalDomains` / `externalEnvVars` declarations the matcher
/// consults, and writes down what they already say.
///
/// Which calls become rows, and the reason in each case:
///
/// - **Declared-external host** → [`CallMechanism::ExternalHttp`]. The repo has
///   said this host is somebody else's.
/// - **Declared-external env-var base** → [`CallMechanism::EnvVarUrl`]. Same
///   statement, made about a base URL the source does not spell out.
/// - **Declared-internal, either shape** → no row. An in-org destination is
///   already carried by the endpoint index, and calling it is not egress.
/// - **Undeclared, either shape** → no row. Where the call goes is exactly what
///   is unknown; the scanner already reports the undeclared env-var bases as
///   `EnvVarCall` findings, and inventing an egress row from a host nobody
///   classified would be a guess. The retained `host` on the call itself is
///   what a later surface would widen from, without another scan.
///
/// A call whose extracted line is not a positive integer is skipped: a row
/// without a location cannot be audited.
pub fn from_data_calls(
    data_calls: &[DataFetchingCall],
    repo_root: &Path,
    config: &Config,
    normalizer: &UrlNormalizer,
) -> Vec<ExternalCallCandidate> {
    let mut rows: BTreeSet<ExternalCallCandidate> = BTreeSet::new();
    for call in data_calls {
        let Some((target, mechanism)) = classify_data_call(call, config, normalizer) else {
            continue;
        };
        let Some(line) = call.line else {
            continue;
        };
        // `file_location` packs the file and the line as `"{file}:{line}"`;
        // only the file half is wanted here, because the line is carried
        // typed. `parse_file_location` is the repo's existing unpacker, and is
        // used rather than a second string split so this file half is the same
        // one every other consumer of the location sees.
        let (file, _) = parse_file_location(&call.file_location);
        rows.insert(ExternalCallCandidate {
            file: relative_location(&file, repo_root),
            line: line as usize,
            // No callee to name on an HTTP call; the method is the operation.
            callee: call.method.to_uppercase(),
            package: target,
            mechanism,
        });
    }
    cap(rows.into_iter().collect())
}

/// The mechanism and target for one already-classified consumer call, or `None`
/// when the call is not declared external.
fn classify_data_call(
    call: &DataFetchingCall,
    config: &Config,
    normalizer: &UrlNormalizer,
) -> Option<(String, CallMechanism)> {
    // Host first: a literal absolute origin is never an env-var base, and the
    // host is only retained for that shape.
    if let Some(host) = &call.host {
        return normalizer
            .is_external_host(host)
            .then(|| (host.clone(), CallMechanism::ExternalHttp));
    }
    // The env-var check runs on `canonical_path` — the key the matcher itself
    // classifies on — so this row names the variable the matcher excluded the
    // call for, not a differently-derived one.
    if !Analyzer::is_env_var_base_url(&call.canonical_path) {
        return None;
    }
    let env_var = Analyzer::extract_env_var_name(&call.canonical_path);
    config
        .is_external_env_var(&env_var)
        .then_some((env_var, CallMechanism::EnvVarUrl))
}

/// Sort order is already fixed by the `BTreeSet` the rows arrive in; this
/// applies the per-service cap, so a service over it yields a stable prefix
/// rather than an arbitrary sample. The cap spans mechanisms, because the cap
/// exists to bound the payload and the payload carries one list.
fn cap(mut rows: Vec<ExternalCallCandidate>) -> Vec<ExternalCallCandidate> {
    if rows.len() > MAX_CANDIDATES_PER_SERVICE {
        warn!(
            "External call candidates truncated to {} of {} rows for this service",
            MAX_CANDIDATES_PER_SERVICE,
            rows.len()
        );
        rows.truncate(MAX_CANDIDATES_PER_SERVICE);
    }
    rows
}

/// Merge the rows of both sources into the single list the payload carries.
///
/// Sorted and deduplicated as one set, then capped once, so the channel has one
/// order and one bound whatever produced a given row.
pub fn merge(
    sdk_rows: Vec<ExternalCallCandidate>,
    http_rows: Vec<ExternalCallCandidate>,
) -> Vec<ExternalCallCandidate> {
    let merged: BTreeSet<ExternalCallCandidate> = sdk_rows.into_iter().chain(http_rows).collect();
    cap(merged.into_iter().collect())
}

/// Package names that count as external runtime dependencies of this service.
///
/// `dependencies`, `peerDependencies`, and `optionalDependencies` — the three
/// maps whose contents can be present at runtime. `devDependencies` is excluded:
/// carrick#510 asks for calls into "the repo's package.json dependency set" and
/// says nothing about dev dependencies, and a test harness or build tool calling
/// out is not service egress.
///
/// Workspace-internal packages are subtracted. A monorepo member is normally
/// declared as a dependency of its siblings (`"workspace:*"`), and a call into
/// one is an internal call, not egress. `Packages::internal_names` holds the
/// name of every `package.json` in the repo tree, which is what makes this
/// structural rather than a naming convention.
fn runtime_dependency_names(packages: &Packages) -> HashSet<String> {
    packages
        .package_jsons
        .iter()
        .flat_map(|pkg| {
            pkg.dependencies
                .keys()
                .chain(pkg.peer_dependencies.keys())
                .chain(pkg.optional_dependencies.keys())
        })
        .filter(|name| !packages.internal_names.contains(*name))
        .cloned()
        .collect()
}

/// Rows are cloud-bound, so `file` must be repo-relative.
///
/// A file the scan root does not contain is reported verbatim rather than
/// dropped, but loudly: an absolute path in the inventory means the walk root
/// and the payload root have diverged, and that is a bug worth seeing rather
/// than a row worth silently mangling.
fn relative_path(file: &Path, repo_root: &Path) -> String {
    match file.strip_prefix(repo_root) {
        Ok(relative) => relative.to_string_lossy().to_string(),
        Err(_) => {
            warn!(
                "External call candidate file {} is not under the scan root {}; \
                 reporting the path as-is",
                file.display(),
                repo_root.display()
            );
            file.to_string_lossy().to_string()
        }
    }
}

/// Repo-relative file path for a projected HTTP row.
///
/// The mount graph keys `file_location` as the analysis scanned it: already
/// repo-relative on the incremental path (the engine normalises the keys before
/// rebuilding the graph) and as-scanned on the full path. Strip the scan root
/// when it is present, and a leading `./` either way, so the two paths produce
/// the same row for the same file — and so an HTTP row is comparable with an
/// SDK row, which is relative by construction.
fn relative_location(file: &str, repo_root: &Path) -> String {
    let path = Path::new(file);
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .trim_start_matches("./")
        .to_string()
}

/// Which declared dependency does this module specifier import from?
///
/// Exact match, or a subpath under it (`"pkg/edge"` resolves to `"pkg"`). This
/// is the same matching convention the messaging-client and data-fetcher import
/// gates use, kept identical so a package is recognized the same way
/// everywhere. A relative specifier (`"./x"`), a bare Node builtin (`"fs"`,
/// `"node:fs"`), and a workspace-internal package all fail it, because none of
/// them appears in the dependency set.
fn resolve_specifier<'a>(specifier: &str, dependencies: &'a HashSet<String>) -> Option<&'a String> {
    if let Some(exact) = dependencies.get(specifier) {
        return Some(exact);
    }
    dependencies
        .iter()
        .filter(|dep| specifier.starts_with(&format!("{}/", dep)))
        // A specifier can only match one declared package by prefix in
        // practice, but pick the longest deterministically rather than relying
        // on HashSet order.
        .max_by_key(|dep| dep.len())
}

fn syntax_for(file_path: &Path) -> (Syntax, bool) {
    match file_path.extension().and_then(|e| e.to_str()) {
        Some("ts") => (
            Syntax::Typescript(TsSyntax {
                decorators: true,
                ..Default::default()
            }),
            true,
        ),
        Some("tsx") => (
            Syntax::Typescript(TsSyntax {
                tsx: true,
                decorators: true,
                ..Default::default()
            }),
            true,
        ),
        Some("jsx") => (
            Syntax::Es(EsSyntax {
                jsx: true,
                ..Default::default()
            }),
            false,
        ),
        _ => (Syntax::Es(Default::default()), false),
    }
}

fn scan_content(
    relative_file: &str,
    file_path: &Path,
    content: &str,
    dependencies: &HashSet<String>,
) -> Vec<ExternalCallCandidate> {
    let (syntax, is_typescript) = syntax_for(file_path);

    // A fresh SourceMap per file keeps byte offsets — and therefore the line
    // lookups below — file-local.
    let source_map: Lrc<SourceMap> = Default::default();
    let source_file = source_map.new_source_file(
        Lrc::new(FileName::Real(file_path.to_path_buf())),
        content.to_string(),
    );

    let lexer = Lexer::new(
        syntax,
        Default::default(),
        StringInput::from(&*source_file),
        None,
    );
    let mut parser = Parser::new_from(lexer);
    let Ok(mut module) = parser.parse_module() else {
        // An unparseable file contributes nothing. The scanner already reports
        // parse failures on its own path; this channel stays quiet.
        return Vec::new();
    };

    // The resolver stamps syntax contexts, so bindings are compared by
    // (symbol, context) rather than by name. A local `const stripe = ...`
    // inside a function therefore does not collide with a module-level import
    // of the same name.
    GLOBALS.set(&Globals::new(), || {
        let unresolved_mark = Mark::new();
        let top_level_mark = Mark::new();
        let mut pass = resolver(unresolved_mark, top_level_mark, is_typescript);
        module.visit_mut_with(&mut pass);
    });

    let mut bindings = import_bindings(&module, dependencies);
    if bindings.is_empty() {
        return Vec::new();
    }
    propagate_aliases(&module, &mut bindings);

    let mut visitor = CandidateVisitor {
        source_map: source_map.clone(),
        file: relative_file.to_string(),
        bindings: &bindings,
        rows: Vec::new(),
    };
    module.visit_with(&mut visitor);
    visitor.rows
}

/// Local binding -> declared package, for every value import from a dependency.
fn import_bindings(module: &Module, dependencies: &HashSet<String>) -> HashMap<Id, String> {
    let mut bindings = HashMap::new();
    for item in &module.body {
        let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = item else {
            continue;
        };
        // `import type { X } from 'pkg'` binds nothing at runtime.
        if import.type_only {
            continue;
        }
        let Some(package) = resolve_specifier(import.src.value.as_ref(), dependencies) else {
            continue;
        };
        for specifier in &import.specifiers {
            let local = match specifier {
                // `import { type X, y }` — the inline type specifier is erased.
                ImportSpecifier::Named(named) if named.is_type_only => continue,
                ImportSpecifier::Named(named) => named.local.to_id(),
                ImportSpecifier::Default(default) => default.local.to_id(),
                ImportSpecifier::Namespace(namespace) => namespace.local.to_id(),
            };
            bindings.insert(local, package.clone());
        }
    }
    bindings
}

/// Extend `bindings` with variables whose initializer is rooted at an existing
/// binding, so an instance held in a local resolves to the package that
/// produced it.
///
/// Covers the shapes static resolution can actually prove:
/// `const c = new Client()`, `const c = createClient()`,
/// `const c = sdk.clients.build()`, `const c = sdk.storage`. The declarator
/// name must be a plain identifier; destructuring is not followed.
///
/// Runs to a fixed point so a chain declared out of order resolves the same way
/// as one declared in order, and so the result does not depend on traversal
/// order. Each round can only add bindings, and the binding set is bounded by
/// the number of declarators, so this terminates.
fn propagate_aliases(module: &Module, bindings: &mut HashMap<Id, String>) {
    let mut collector = AliasCollector {
        aliases: Vec::new(),
    };
    module.visit_with(&mut collector);

    loop {
        let mut added = false;
        for (local, init) in &collector.aliases {
            if bindings.contains_key(local) {
                continue;
            }
            if let Some((root, _)) = callee_chain(init)
                && let Some(package) = bindings.get(&root).cloned()
            {
                bindings.insert(local.clone(), package);
                added = true;
            }
        }
        if !added {
            break;
        }
    }
}

struct AliasCollector {
    /// `(bound identifier, expression it is initialized from)`, in source order.
    aliases: Vec<(Id, Box<Expr>)>,
}

impl Visit for AliasCollector {
    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        if let (Pat::Ident(name), Some(init)) = (&declarator.name, &declarator.init) {
            let source = match &**init {
                // `new Client(...)` and `createClient(...)` both hand back an
                // instance owned by whatever the callee resolves to.
                Expr::New(new_expr) => Some(new_expr.callee.clone()),
                Expr::Call(call) => match &call.callee {
                    Callee::Expr(callee) => Some(callee.clone()),
                    _ => None,
                },
                // `const storage = sdk.storage` — a handle reached off a
                // resolved binding stays owned by the same package.
                other @ (Expr::Member(_) | Expr::Ident(_)) => Some(Box::new(other.clone())),
                _ => None,
            };
            if let Some(source) = source {
                self.aliases.push((name.id.to_id(), source));
            }
        }
        declarator.visit_children_with(self);
    }
}

/// Split a callee expression into its root identifier and the property names
/// applied to it, or `None` when any part is not statically nameable
/// (computed access, a call in the middle of the chain, a literal receiver).
fn callee_chain(expr: &Expr) -> Option<(Id, Vec<String>)> {
    match expr {
        Expr::Ident(ident) => Some((ident.to_id(), Vec::new())),
        Expr::Paren(paren) => callee_chain(&paren.expr),
        Expr::TsNonNull(non_null) => callee_chain(&non_null.expr),
        Expr::TsAs(as_expr) => callee_chain(&as_expr.expr),
        Expr::Member(member) => {
            let MemberProp::Ident(prop) = &member.prop else {
                return None;
            };
            let (root, mut props) = callee_chain(&member.obj)?;
            props.push(prop.sym.to_string());
            Some((root, props))
        }
        Expr::OptChain(opt_chain) => match &*opt_chain.base {
            // `a?.b.c()` — the receiver chain is nameable even though it is
            // guarded. A call inside the chain is not.
            OptChainBase::Member(member) => {
                let MemberProp::Ident(prop) = &member.prop else {
                    return None;
                };
                let (root, mut props) = callee_chain(&member.obj)?;
                props.push(prop.sym.to_string());
                Some((root, props))
            }
            OptChainBase::Call(_) => None,
        },
        _ => None,
    }
}

fn format_callee(root: &Id, props: &[String]) -> String {
    let mut text = root.0.to_string();
    for prop in props {
        text.push('.');
        text.push_str(prop);
    }
    text
}

struct CandidateVisitor<'a> {
    source_map: Lrc<SourceMap>,
    file: String,
    bindings: &'a HashMap<Id, String>,
    rows: Vec<ExternalCallCandidate>,
}

impl CandidateVisitor<'_> {
    fn record(&mut self, callee: &Expr, span: swc_common::Span) {
        let Some((root, props)) = callee_chain(callee) else {
            return;
        };
        let Some(package) = self.bindings.get(&root) else {
            return;
        };
        self.rows.push(ExternalCallCandidate {
            file: self.file.clone(),
            line: self.source_map.lookup_char_pos(span.lo).line,
            callee: format_callee(&root, &props),
            package: package.clone(),
            mechanism: CallMechanism::Sdk,
        });
    }
}

impl Visit for CandidateVisitor<'_> {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        if let Callee::Expr(callee) = &call.callee {
            self.record(callee, call.span);
        }
        call.visit_children_with(self);
    }

    fn visit_opt_chain_expr(&mut self, opt_chain: &OptChainExpr) {
        // `client?.send(...)` is an optional call, not a `CallExpr`, so it
        // needs its own arm or the row is silently lost.
        if let OptChainBase::Call(call) = &*opt_chain.base {
            let callee = call.callee.clone();
            self.record(&callee, opt_chain.span);
        }
        opt_chain.visit_children_with(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::file_finder::find_service_files;
    use crate::packages::{Packages, collect_internal_package_names};

    const IGNORE_PATTERNS: &[&str] = &["node_modules", "dist", "build", ".next"];

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/external-call-candidates")
    }

    /// The fixture is a workspace whose scanned service is `apps/api`, matching
    /// the monorepo shape `carrick.json` declares.
    fn fixture_service() -> Config {
        Config {
            directory: Some("apps/api".to_string()),
            ..Default::default()
        }
    }

    /// Run the scan exactly as the engine does: the service's file list comes
    /// from `find_service_files`, so the scanner's own test-file and
    /// artifact-directory exclusions are what filter the input, and the
    /// dependency set comes from the service's `package.json` plus the
    /// repo-wide internal-name sweep.
    fn scan_fixture() -> Vec<ExternalCallCandidate> {
        let root = fixture_root();
        let root_str = root.to_string_lossy().to_string();
        let (files, package_json) =
            find_service_files(&root_str, &fixture_service(), IGNORE_PATTERNS);
        let mut packages =
            Packages::new(package_json.into_iter().collect()).expect("fixture package.json parses");
        packages.internal_names = collect_internal_package_names(&root);
        scan_files(&files, &root, &packages)
    }

    fn rows_for(rows: &[ExternalCallCandidate], file: &str) -> Vec<(usize, String, String)> {
        rows.iter()
            .filter(|row| row.file == file)
            .map(|row| (row.line, row.callee.clone(), row.package.clone()))
            .collect()
    }

    #[test]
    fn fixture_scan_matches_expected_rows() {
        let rows = scan_fixture();
        let actual: Vec<(String, usize, String, String)> = rows
            .iter()
            .map(|row| {
                (
                    row.file.clone(),
                    row.line,
                    row.callee.clone(),
                    row.package.clone(),
                )
            })
            .collect();

        let expected: Vec<(String, usize, String, String)> = vec![
            (
                "apps/api/src/direct-call.ts",
                4,
                "sendNotice",
                "courier-sdk",
            ),
            (
                "apps/api/src/member-call.ts",
                6,
                "ledger.payments.create",
                "ledger-client",
            ),
            (
                "apps/api/src/member-call.ts",
                10,
                "createInvoice",
                "ledger-client",
            ),
            (
                "apps/api/src/namespace-call.ts",
                3,
                "telemetry.createSink",
                "telemetry-sink",
            ),
            (
                "apps/api/src/namespace-call.ts",
                6,
                "telemetry.emit",
                "telemetry-sink",
            ),
            (
                "apps/api/src/namespace-call.ts",
                7,
                "sink.flush",
                "telemetry-sink",
            ),
            (
                "apps/api/src/optional-dep.ts",
                6,
                "uplink.put",
                "storage-uplink",
            ),
            (
                "apps/api/src/subpath-import.ts",
                4,
                "publishEdge",
                "courier-sdk",
            ),
        ]
        .into_iter()
        .map(|(file, line, callee, package)| {
            (
                file.to_string(),
                line,
                callee.to_string(),
                package.to_string(),
            )
        })
        .collect();

        assert_eq!(actual, expected);
        assert!(rows.iter().all(|row| row.mechanism == CallMechanism::Sdk));
    }

    #[test]
    fn direct_imported_function_call_emits_a_row() {
        let rows = scan_fixture();
        assert_eq!(
            rows_for(&rows, "apps/api/src/direct-call.ts"),
            vec![(4, "sendNotice".to_string(), "courier-sdk".to_string())]
        );
    }

    /// The receiver is a local holding a client constructed from an imported
    /// class, which is as far as static resolution reaches.
    #[test]
    fn member_call_on_constructed_client_emits_a_row() {
        let rows = scan_fixture();
        let member = rows_for(&rows, "apps/api/src/member-call.ts");
        assert!(
            member.contains(&(
                6,
                "ledger.payments.create".to_string(),
                "ledger-client".to_string()
            )),
            "expected the instance member call, got {:?}",
            member
        );
    }

    #[test]
    fn namespace_import_call_emits_a_row() {
        let rows = scan_fixture();
        let namespace = rows_for(&rows, "apps/api/src/namespace-call.ts");
        assert!(
            namespace.contains(&(
                6,
                "telemetry.emit".to_string(),
                "telemetry-sink".to_string()
            )),
            "expected the namespace call, got {:?}",
            namespace
        );
        assert!(
            namespace.contains(&(7, "sink.flush".to_string(), "telemetry-sink".to_string())),
            "a handle returned from a namespace call keeps its package: {:?}",
            namespace
        );
    }

    /// `peerDependencies` and `optionalDependencies` count as runtime
    /// dependencies; `telemetry-sink` is a peer, `storage-uplink` optional.
    #[test]
    fn peer_and_optional_dependencies_resolve() {
        let rows = scan_fixture();
        assert!(rows.iter().any(|row| row.package == "telemetry-sink"));
        assert_eq!(
            rows_for(&rows, "apps/api/src/optional-dep.ts"),
            vec![(6, "uplink.put".to_string(), "storage-uplink".to_string())]
        );
    }

    /// `courier-sdk/edge` resolves to the declared package `courier-sdk`.
    #[test]
    fn subpath_import_resolves_to_the_declared_package() {
        let rows = scan_fixture();
        assert_eq!(
            rows_for(&rows, "apps/api/src/subpath-import.ts"),
            vec![(4, "publishEdge".to_string(), "courier-sdk".to_string())]
        );
    }

    /// Relative imports, Node builtins (bare and `node:`-prefixed), and a
    /// workspace-internal package all fail the structural rule.
    #[test]
    fn relative_builtin_and_workspace_imports_emit_nothing() {
        let rows = scan_fixture();
        assert_eq!(rows_for(&rows, "apps/api/src/no-rows.ts"), Vec::new());
    }

    /// carrick#510 is silent on dev dependencies, so they are excluded: a call
    /// into a build or test tool is not service egress.
    #[test]
    fn dev_dependency_only_import_emits_nothing() {
        let rows = scan_fixture();
        assert_eq!(rows_for(&rows, "apps/api/src/dev-only.ts"), Vec::new());
    }

    /// A type-only declaration binds nothing, while the value specifier in a
    /// mixed declaration still does.
    #[test]
    fn type_only_imports_emit_nothing() {
        let rows = scan_fixture();
        assert_eq!(rows_for(&rows, "apps/api/src/type-only.ts"), Vec::new());
        assert!(
            rows_for(&rows, "apps/api/src/member-call.ts").contains(&(
                10,
                "createInvoice".to_string(),
                "ledger-client".to_string()
            )),
            "the value half of a mixed type/value import still resolves"
        );
    }

    /// Direct guard on the type-only filter. A type binding in call position is
    /// not valid TypeScript, but the parser accepts it, so the filter is
    /// asserted rather than assumed.
    #[test]
    fn type_only_binding_is_never_callable() {
        let dependencies: HashSet<String> = ["ledger-client".to_string()].into_iter().collect();
        assert_eq!(
            scan_content(
                "src/t.ts",
                Path::new("t.ts"),
                "import type { createInvoice } from \"ledger-client\";\ncreateInvoice();\n",
                &dependencies,
            ),
            Vec::new()
        );
        assert_eq!(
            scan_content(
                "src/t.ts",
                Path::new("t.ts"),
                "import { type createInvoice } from \"ledger-client\";\ncreateInvoice();\n",
                &dependencies,
            ),
            Vec::new()
        );
    }

    /// Test trees and build-artifact directories are excluded by the scanner's
    /// existing walk rules, not by anything this module defines.
    #[test]
    fn test_and_artifact_files_are_never_scanned() {
        let rows = scan_fixture();
        assert!(
            rows.iter().all(|row| !row.file.contains("__tests__")
                && !row.file.contains("/dist/")
                && !row.file.ends_with(".test.ts")),
            "excluded trees leaked into the rows: {:?}",
            rows
        );
    }

    /// A local declaration shadowing an import must not inherit its package.
    #[test]
    fn shadowed_binding_does_not_resolve() {
        let dependencies: HashSet<String> = ["courier-sdk".to_string()].into_iter().collect();
        let rows = scan_content(
            "src/shadow.ts",
            Path::new("shadow.ts"),
            r#"import { sendNotice } from "courier-sdk";

export function local() {
  const sendNotice = (m: string) => m;
  return sendNotice("hi");
}

export function real() {
  return sendNotice("hi");
}
"#,
            &dependencies,
        );
        assert_eq!(
            rows.iter().map(|r| r.line).collect::<Vec<_>>(),
            vec![9],
            "only the unshadowed call resolves: {:?}",
            rows
        );
    }

    /// Two scans of the same tree produce byte-identical rows.
    #[test]
    fn scan_is_deterministic() {
        assert_eq!(scan_fixture(), scan_fixture());
    }

    #[test]
    fn empty_dependency_set_emits_nothing() {
        let root = fixture_root();
        let root_str = root.to_string_lossy().to_string();
        let (files, _) = find_service_files(&root_str, &fixture_service(), IGNORE_PATTERNS);
        assert_eq!(
            scan_files(&files, &root, &Packages::default()),
            Vec::new(),
            "no declared dependencies means no candidates"
        );
    }

    #[test]
    fn mechanism_serializes_as_snake_case() {
        let row = ExternalCallCandidate {
            file: "src/a.ts".to_string(),
            line: 3,
            callee: "client.send".to_string(),
            package: "courier-sdk".to_string(),
            mechanism: CallMechanism::Sdk,
        };
        assert_eq!(
            serde_json::to_value(&row).unwrap(),
            serde_json::json!({
                "file": "src/a.ts",
                "line": 3,
                "callee": "client.send",
                "package": "courier-sdk",
                "mechanism": "sdk"
            })
        );
    }

    /// The HTTP-shaped mechanisms ride the row shape `sdk` shipped with — same
    /// five keys, same casing — so one reader handles all three.
    #[test]
    fn http_mechanisms_serialize_in_the_same_row_shape() {
        let row = ExternalCallCandidate {
            file: "src/pay.ts".to_string(),
            line: 12,
            callee: "POST".to_string(),
            package: "api.vendor.test".to_string(),
            mechanism: CallMechanism::ExternalHttp,
        };
        assert_eq!(
            serde_json::to_value(&row).unwrap(),
            serde_json::json!({
                "file": "src/pay.ts",
                "line": 12,
                "callee": "POST",
                "package": "api.vendor.test",
                "mechanism": "external_http"
            })
        );
        assert_eq!(
            serde_json::to_value(ExternalCallCandidate {
                mechanism: CallMechanism::EnvVarUrl,
                package: "BILLING_API".to_string(),
                ..row
            })
            .unwrap()["mechanism"],
            serde_json::json!("env_var_url")
        );
    }

    mod http_rows {
        use super::*;
        use crate::mount_graph::DataFetchingCall;

        fn config() -> Config {
            Config {
                internal_domains: ["orders.internal.test".to_string()].into_iter().collect(),
                external_domains: ["api.vendor.test".to_string()].into_iter().collect(),
                internal_env_vars: ["ORDERS_URL".to_string()].into_iter().collect(),
                external_env_vars: ["BILLING_API".to_string()].into_iter().collect(),
                ..Config::default()
            }
        }

        /// A consumer call as the mount-graph builder records one: the host and
        /// the typed line are the fields it now retains from normalisation and
        /// extraction.
        fn call(target: &str, host: Option<&str>, line: Option<u32>) -> DataFetchingCall {
            DataFetchingCall {
                method: "get".to_string(),
                target_url: target.to_string(),
                canonical_path: target.to_string(),
                client: "fetch(".to_string(),
                file_location: format!("src/client.ts:{}", line.unwrap_or(1)),
                call_kind: None,
                repo_name: None,
                service_name: None,
                host: host.map(str::to_string),
                line,
            }
        }

        fn rows(calls: &[DataFetchingCall]) -> Vec<ExternalCallCandidate> {
            let config = config();
            from_data_calls(
                calls,
                Path::new("/repo"),
                &config,
                &crate::url_normalizer::UrlNormalizer::new(&config),
            )
        }

        /// The domain and the line the scanner used to discard both reach the
        /// row, and the HTTP method stands in for the callee.
        #[test]
        fn declared_external_host_becomes_a_row() {
            assert_eq!(
                rows(&[call(
                    "https://api.vendor.test/v1/charges",
                    Some("api.vendor.test"),
                    Some(12)
                )]),
                vec![ExternalCallCandidate {
                    file: "src/client.ts".to_string(),
                    line: 12,
                    callee: "GET".to_string(),
                    package: "api.vendor.test".to_string(),
                    mechanism: CallMechanism::ExternalHttp,
                }]
            );
        }

        /// A subdomain of a declared external domain is classified external by
        /// the normalizer, and the row names the host as written, not the
        /// declaration that matched it.
        #[test]
        fn subdomain_of_a_declared_host_keeps_its_own_name() {
            let rows = rows(&[call(
                "https://eu.api.vendor.test/v1/charges",
                Some("eu.api.vendor.test"),
                Some(3),
            )]);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].package, "eu.api.vendor.test");
        }

        /// The env-var name reaches the row for a variable the repo declares
        /// external — the pair the matcher drops on its way past the call.
        #[test]
        fn declared_external_env_var_becomes_a_row() {
            assert_eq!(
                rows(&[call("${process.env.BILLING_API}/invoices", None, Some(7))]),
                vec![ExternalCallCandidate {
                    file: "src/client.ts".to_string(),
                    line: 7,
                    callee: "GET".to_string(),
                    package: "BILLING_API".to_string(),
                    mechanism: CallMechanism::EnvVarUrl,
                }]
            );
        }

        /// The canonical `ENV_VAR:NAME:/path` shape resolves to the same row as
        /// the raw interpolation.
        #[test]
        fn canonical_env_var_route_becomes_a_row() {
            let rows = rows(&[call("ENV_VAR:BILLING_API:/invoices", None, Some(4))]);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].package, "BILLING_API");
            assert_eq!(rows[0].mechanism, CallMechanism::EnvVarUrl);
        }

        /// An undeclared destination is the one thing the scanner does not
        /// know. It stays out of the inventory rather than being guessed into
        /// it; the unclassified env-var case is already reported as an
        /// `EnvVarCall` finding, which this channel does not touch.
        #[test]
        fn undeclared_destinations_emit_nothing() {
            assert_eq!(
                rows(&[
                    call(
                        "https://api.unlisted.test/v1/x",
                        Some("api.unlisted.test"),
                        Some(2)
                    ),
                    call("${process.env.UNKNOWN_API}/x", None, Some(3)),
                ]),
                Vec::new()
            );
        }

        /// An in-org destination is not egress: it is already in the endpoint
        /// index as a call, and the inventory would double-count it.
        #[test]
        fn declared_internal_destinations_emit_nothing() {
            assert_eq!(
                rows(&[
                    call(
                        "https://orders.internal.test/orders",
                        Some("orders.internal.test"),
                        Some(2)
                    ),
                    call("${process.env.ORDERS_URL}/orders", None, Some(3)),
                ]),
                Vec::new()
            );
        }

        /// A relative call has no destination to name at all.
        #[test]
        fn relative_call_emits_nothing() {
            assert_eq!(rows(&[call("/api/orders", None, Some(9))]), Vec::new());
        }

        /// A row with no line cannot be audited, so it is dropped rather than
        /// carried with a guessed location.
        #[test]
        fn call_without_a_line_emits_nothing() {
            assert_eq!(
                rows(&[call(
                    "https://api.vendor.test/v1/charges",
                    Some("api.vendor.test"),
                    None
                )]),
                Vec::new()
            );
        }

        /// The file half of `file_location` is repo-relative in the row
        /// whichever analysis path recorded it — the incremental path
        /// normalises its keys, the full path does not.
        #[test]
        fn absolute_scan_paths_are_relativized() {
            let mut absolute = call(
                "https://api.vendor.test/v1/charges",
                Some("api.vendor.test"),
                Some(5),
            );
            absolute.file_location = "/repo/src/client.ts:5".to_string();
            assert_eq!(rows(&[absolute])[0].file, "src/client.ts");
        }

        /// Two calls to the same host on the same line collapse; the same host
        /// on different lines does not.
        #[test]
        fn rows_are_deduplicated_and_sorted() {
            let hit = call(
                "https://api.vendor.test/v1/charges",
                Some("api.vendor.test"),
                Some(12),
            );
            let mut earlier = hit.clone();
            earlier.line = Some(4);
            earlier.file_location = "src/client.ts:4".to_string();
            let rows = rows(&[hit.clone(), earlier, hit]);
            assert_eq!(
                rows.iter().map(|row| row.line).collect::<Vec<_>>(),
                vec![4, 12]
            );
        }

        /// The merged list is one sorted, deduplicated set, so the payload has
        /// a single order whatever produced a given row.
        #[test]
        fn merge_sorts_both_sources_as_one_set() {
            let sdk = ExternalCallCandidate {
                file: "src/client.ts".to_string(),
                line: 20,
                callee: "ledger.charge".to_string(),
                package: "ledger-client".to_string(),
                mechanism: CallMechanism::Sdk,
            };
            let http = ExternalCallCandidate {
                file: "src/client.ts".to_string(),
                line: 12,
                callee: "GET".to_string(),
                package: "api.vendor.test".to_string(),
                mechanism: CallMechanism::ExternalHttp,
            };
            let merged = merge(vec![sdk.clone(), sdk.clone()], vec![http.clone()]);
            assert_eq!(merged, vec![http, sdk]);
        }
    }
}
