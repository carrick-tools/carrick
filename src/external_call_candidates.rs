//! Deterministic, AST-only detection of SDK-mediated outbound call candidates.
//!
//! The scanner's existing outbound-call candidates are HTTP-shaped: `fetch` /
//! axios-style calls, env-var-anchored URLs, absolute URLs. A call made through
//! a service client imported from an npm package never looks like that, so it
//! is invisible to candidate generation and to every surface downstream of it.
//!
//! This module adds a parallel data channel for that class. It is intentionally
//! narrow:
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

use crate::packages::Packages;
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
///
/// Only [`CallMechanism::Sdk`] is produced today. The env-var-URL and
/// external-domain HTTP classes are detected elsewhere in the scanner and are
/// not yet projected into this channel; the enum is shaped so they join without
/// changing the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallMechanism {
    /// The callee resolves, through an import, to a declared runtime dependency.
    Sdk,
}

/// One outbound call candidate.
///
/// Ordering is `(file, line, callee, package)`, which is also the sort key the
/// scan emits in, so the row list is byte-identical across runs of the same
/// tree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExternalCallCandidate {
    /// Repo-relative path of the file containing the call.
    pub file: String,
    /// 1-based line number of the call expression.
    pub line: usize,
    /// The callee as written, dotted (`ledger.payments.create`). Computed from
    /// the AST, so it carries no whitespace or comments from the source.
    pub callee: String,
    /// The declared dependency the callee resolves to, exactly as the
    /// `package.json` names it (a subpath import such as `pkg/edge` resolves to
    /// `pkg`).
    pub package: String,
    /// Always [`CallMechanism::Sdk`] for now.
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

    let mut rows: Vec<ExternalCallCandidate> = rows.into_iter().collect();
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
}
