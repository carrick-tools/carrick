//! The callable surface a repo publishes as an npm package.
//!
//! A service that ships a client library serves its own API twice: once as
//! HTTP routes, and once as the methods consumers actually write
//! (`ledger.payments.create(...)`). The scanner already sees both halves —
//! the SDK repo's own outbound calls sit in its mount graph, and a consumer's
//! call through the client is an [`crate::external_call_candidates`] row — but
//! nothing joined them, so an endpoint consumed only through the published
//! client looked unconsumed.
//!
//! This module computes the missing middle: for every service scanned, the
//! member paths its entry module publishes, each anchored to the source span
//! that implements it. The join in [`crate::sdk_edges`] then walks
//! consumer candidate → member → the SDK repo's own call inside that span →
//! the producer endpoint that call already matched.
//!
//! # What is walked
//!
//! The entry module named by the service's own `package.json`
//! (every string leaf under `exports["."]`, however deeply its conditions
//! nest, then `types`, `main`, falling back to `src/index.ts`), and the class
//! graph reachable from its exports:
//!
//! - an exported **class** contributes one member per method, and recurses
//!   into each field whose declared type or `new X(...)` initialiser names a
//!   class declared in the repo (`payments: API.Payments = new
//!   API.Payments(this)` → the `Payments` class, with `payments.` prefixed to
//!   every member it contributes). A constructor **parameter property**
//!   (`constructor(public payments: Payments) {}`) is a field like any other;
//! - a method or function-valued field that **hands back** a sub-resource
//!   (`transfers = () => this.transfersClient`, `transfers(): Transfers { ... }`)
//!   is both a callable member and a hop: the class it returns contributes
//!   `transfers.` — which is the chain a consumer writes as
//!   `client.transfers().send(...)`;
//! - an exported **object literal** contributes the same way over its
//!   properties;
//! - `export { default as Ledger } from './client'` and `export default
//!   Ledger` both publish under the export name `default`, which is what a
//!   consumer's default import binds.
//!
//! `private` and `protected` members are not surface: TypeScript forbids a
//! consumer writing them, so publishing `transfersClient.send` for a
//! `private transfersClient` names a chain nobody can call. They stay fully
//! resolvable internally — a public accessor that returns `this.transfersClient`
//! reaches the class through exactly that field.
//!
//! # Delegates
//!
//! A layered client puts the route one hop below the member: the published
//! `send` calls `this.api.send(...)`, and only THAT method writes
//! `/v1/transfers`. A member therefore carries the spans of the
//! same-repo methods its body calls through a `this.<field>` receiver, up to
//! [`MAX_DELEGATE_DEPTH`] hops, so the join in [`crate::sdk_edges`] can find
//! the SDK's own outbound call wherever in that chain it sits. Only class
//! hops are followed: a field holding an object literal names no declaration
//! to resolve a method in.
//!
//! Resolution runs through [`crate::import_bindings::BindingResolver`], so a
//! barrel in front of the real module is followed rather than guessed at.
//!
//! # Deliberate limits
//!
//! - **Entry modules only.** The walk starts at what the package publishes,
//!   not at every file in the tree: a class nobody exports is not surface.
//! - **Relative hops only.** A field typed by a class imported from another
//!   npm package resolves to nothing. Reaching it needs the sidecar's
//!   tsconfig knowledge, and a wrong hop here silently mis-attributes a
//!   member to the wrong span.
//! - **Source, never build output.** A declared entry that resolves into
//!   `dist/`, `build/`, `node_modules/`, or to a `.d.ts` is rejected in favour
//!   of the source fallback: declaration files carry no method bodies, so the
//!   spans they would contribute point at generated text no consumer can
//!   follow and no data call sits inside.
//! - **Depth 4, with a cycle guard.** A field graph deeper than four hops is
//!   indistinguishable from a mis-resolution, and a class that reaches itself
//!   stops at the repeat.
//! - **No inheritance, no generics.** A method inherited from a base class,
//!   and a field typed `Promise<Payments>` or `Payments | undefined`, name
//!   nothing this reads. Same trade as the annotation rule in
//!   [`crate::external_call_candidates`]: only what a declaration states about
//!   its own shape.

use crate::agents::file_orchestrator::FileOrchestrator;
use crate::import_bindings::BindingResolver;
use crate::parser::parse_file;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use swc_common::{
    BytePos, SourceMap, Span, Spanned,
    errors::{ColorConfig, Handler},
    sync::Lrc,
};
use swc_ecma_ast::{
    Accessibility, ArrowExpr, BindingIdent, BlockStmt, BlockStmtOrExpr, CallExpr, Callee, Class,
    ClassMember, Decl, DefaultDecl, Expr, Function, ImportSpecifier, MethodKind, Module,
    ModuleDecl, ModuleExportName, ModuleItem, ObjectLit, ParamOrTsParamProp, Pat, Prop, PropName,
    PropOrSpread, Stmt, TsEntityName, TsParamPropParam, TsType, TsTypeAnn,
};
use swc_ecma_visit::{Visit, VisitWith};
use tracing::{debug, warn};

/// The export name a default export is published under.
const DEFAULT_EXPORT: &str = "default";

/// How many field hops a member chain may take from an exported root.
const MAX_DEPTH: usize = 4;

/// Hard cap on emitted members. A surface this size is a resolution accident,
/// not a client library; truncating keeps the payload bounded and logs loudly.
const MAX_MEMBERS: usize = 20_000;

/// How many same-repo method hops a member's delegate spans follow. Two covers
/// the layered shape this exists for — published method → api wrapper → the
/// transport that writes the URL — without walking a whole call graph.
const MAX_DELEGATE_DEPTH: usize = 2;

/// Cap on the delegate spans one member carries. A member that reaches more
/// than this is a fan-out, not a delegation chain.
const MAX_DELEGATES: usize = 32;

/// Directories whose contents are build output or dependencies, never the
/// package's own source.
const NON_SOURCE_DIRS: [&str; 4] = ["node_modules", "dist", "build", ".next"];

/// The `exports` conditions read before anything else at a given level, in
/// this order. Not a filter: every other condition at that level is read too,
/// after these. It exists so a package that states its entry plainly is not
/// out-ordered by a private condition sitting beside it.
const KNOWN_CONDITIONS: [&str; 4] = ["types", "import", "require", "default"];

/// One callable the package publishes.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SdkMember {
    /// The entry module's export the chain starts from: `default`, or the
    /// exported name. This is the same anchor an
    /// [`crate::external_call_candidates::ExternalCallCandidate`] carries in
    /// `import_symbol`, which is what makes the two joinable.
    pub export: String,
    /// Member path from that export, dotted and without the root binding:
    /// `payments.create`, `scrape`.
    pub chain: String,
    /// Repo-relative path of the file declaring the method.
    pub file: String,
    /// 1-based first line of the method.
    pub line: u32,
    /// 1-based last line of the method. The span a consumer's call maps onto:
    /// the SDK's own outbound call sits somewhere inside it, or inside one of
    /// the [`SdkMember::delegates`].
    pub end_line: u32,
    /// Spans of the same-repo methods this member's body calls through a
    /// `this.<field>` receiver, and what those call in turn. A layered client
    /// writes its route one hop below the published method, so the SDK's own
    /// outbound call is not always inside the member's own span.
    ///
    /// `default` for surfaces written before the field existed: absent is an
    /// unlayered member, which is what every such surface was read as.
    #[serde(default)]
    pub delegates: Vec<SdkSpan>,
}

/// A source range in the SDK repo, repo-relative and 1-based.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SdkSpan {
    pub file: String,
    pub line: u32,
    pub end_line: u32,
}

/// The callable surface `service_root` publishes, computed from the entry
/// module its `package.json` names.
///
/// Cheap and empty for a service that publishes no client: a repo with no
/// resolvable TypeScript entry, or one whose exports reach no class or object
/// literal, produces no members and parses at most a handful of files.
pub fn scan(repo_root: &Path, service_root: &Path) -> Vec<SdkMember> {
    let Some(entry) = resolve_entry_module(service_root) else {
        debug!(
            "No TypeScript entry module under {}; publishing no SDK surface",
            service_root.display()
        );
        return Vec::new();
    };
    let mut scanner = SurfaceScanner::new(repo_root);
    scanner.walk_entry(&entry);
    let mut members = scanner.members;
    members.sort();
    members.dedup();
    members
}

/// The entry module a package publishes, as a TypeScript source path.
///
/// `exports`, `types` and `main` are read in that order, and every candidate
/// is required to resolve to real TS source: a published package points them
/// at `dist/index.js`, which resolves to nothing in a source checkout (the
/// `.js`→`.ts` rewrite probes siblings, and `dist/` is not built here), so the
/// declared fields are best-effort and `src/index.ts` is the fallback that
/// actually fires on most repos.
fn resolve_entry_module(service_root: &Path) -> Option<PathBuf> {
    let manifest = service_root.join("package.json");
    let declared = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .map(|json| declared_entry_specifiers(&json))
        .unwrap_or_default();

    for specifier in declared
        .iter()
        .map(String::as_str)
        .chain(["./src/index.ts", "./index.ts"])
    {
        let relative = if specifier.starts_with("./") || specifier.starts_with("../") {
            specifier.to_string()
        } else {
            format!("./{}", specifier.trim_start_matches('/'))
        };
        if let Some(resolved) = FileOrchestrator::resolve_relative_import(&manifest, &relative)
            && is_typescript_source(&resolved)
        {
            return Some(resolved);
        }
    }
    None
}

/// The entry specifiers a `package.json` declares, most specific first.
///
/// The `"."` subpath of `exports` is read as a bare string or as a condition
/// tree of ANY depth, and every string leaf in that tree is a candidate. A
/// package that publishes both module systems nests its conditions
/// (`{ import: { types, default }, require: { … } }`), and the source path a
/// monorepo resolves internally is usually a private condition key beside
/// them. Reading only three well-known keys at the top level returned nothing
/// at all on that layout, so the package published no surface and every
/// consumer of its client was `member_not_found` (carrick#656).
///
/// A candidate that resolves to build output or to a declaration file is
/// rejected by [`resolve_entry_module`], which is why every leaf can be
/// offered rather than only the ones this recognises.
fn declared_entry_specifiers(manifest: &serde_json::Value) -> Vec<String> {
    let mut specifiers = Vec::new();
    match manifest.get("exports") {
        Some(serde_json::Value::String(entry)) => specifiers.push(entry.clone()),
        Some(serde_json::Value::Object(map)) => {
            if let Some(root) = map.get(".") {
                collect_string_leaves(root, &mut specifiers);
            }
        }
        _ => {}
    }
    for field in ["types", "typings", "main"] {
        if let Some(serde_json::Value::String(entry)) = manifest.get(field) {
            specifiers.push(entry.clone());
        }
    }
    specifiers
}

/// Every string leaf of a condition tree, well-known conditions first.
///
/// [`KNOWN_CONDITIONS`] are walked in their own order at each level, so the
/// entry a package states plainly still wins; whatever else the level holds
/// follows. Order only decides which of several resolvable candidates is
/// taken, and on a real package the conditions of one subpath name one module.
///
/// Arrays are walked for the same reason objects are: a fallback array is a
/// list of candidates, and one that names build output is dropped by the
/// source test rather than by guessing at its position.
fn collect_string_leaves(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(entry) => out.push(entry.clone()),
        serde_json::Value::Object(map) => {
            for condition in KNOWN_CONDITIONS {
                if let Some(nested) = map.get(condition) {
                    collect_string_leaves(nested, out);
                }
            }
            for (condition, nested) in map {
                if !KNOWN_CONDITIONS.contains(&condition.as_str()) {
                    collect_string_leaves(nested, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for nested in items {
                collect_string_leaves(nested, out);
            }
        }
        _ => {}
    }
}

/// Whether a resolved path is TypeScript the package actually authored: a
/// `.ts`/`.tsx`/`.mts`/`.cts` file, not a declaration file, and not inside a
/// dependency or build directory.
fn is_typescript_source(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts") {
        return false;
    }
    let is_ts = Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "ts" | "tsx" | "mts" | "cts"));
    if !is_ts {
        return false;
    }
    !path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|segment| NON_SOURCE_DIRS.contains(&segment))
    })
}

/// A value an export or a field resolved to, paired with the module that
/// declares it (the resolution context for anything nested inside).
enum Declared {
    Class(PathBuf, Rc<Module>, Rc<Class>),
    Object(PathBuf, Rc<Module>, Rc<ObjectLit>),
    /// A function, wherever it was declared. A property or field that names one
    /// is a callable member itself, not a sub-resource to recurse into, so this
    /// variant is emitted at the site that resolved it and never walked.
    Function(PathBuf, Span),
}

impl Declared {
    fn anchor(&self) -> (PathBuf, BytePos) {
        match self {
            Declared::Class(file, _, class) => (file.clone(), class.span.lo),
            Declared::Object(file, _, object) => (file.clone(), object.span.lo),
            Declared::Function(file, span) => (file.clone(), span.lo),
        }
    }

    /// The span to anchor a member at, for a resolved function.
    fn function_span(&self) -> Option<(PathBuf, Span)> {
        match self {
            Declared::Function(file, span) => Some((file.clone(), *span)),
            _ => None,
        }
    }
}

/// The class a `this` refers to, with the module context that resolves its
/// fields. Carried through the walk so an arrow written in a class body — as a
/// field, or inside an object literal a field holds — resolves `this.<field>`
/// against the class that lexically encloses it.
#[derive(Clone)]
struct ClassCtx {
    file: PathBuf,
    module: Rc<Module>,
    class: Rc<Class>,
}

/// A function written as a value: `payments = () => ...` or `payments =
/// function () { ... }`. The two shapes answer the same three questions, and
/// every caller needs all three.
enum FunctionValue<'a> {
    Arrow(&'a ArrowExpr),
    Fn(&'a Function),
}

impl FunctionValue<'_> {
    fn this_calls(&self) -> Vec<Vec<String>> {
        match self {
            FunctionValue::Arrow(arrow) => this_calls_of_arrow(arrow),
            FunctionValue::Fn(function) => this_calls_of_function(function),
        }
    }

    fn return_type(&self) -> Option<&TsTypeAnn> {
        match self {
            FunctionValue::Arrow(arrow) => arrow.return_type.as_deref(),
            FunctionValue::Fn(function) => function.return_type.as_deref(),
        }
    }

    fn returned_expr(&self) -> Option<Expr> {
        match self {
            FunctionValue::Arrow(arrow) => arrow_returned_expr(arrow).cloned(),
            FunctionValue::Fn(function) => function
                .body
                .as_ref()
                .and_then(block_returned_expr)
                .cloned(),
        }
    }
}

/// The function an initialiser expression IS, if any.
fn function_value(expr: Option<&Expr>) -> Option<FunctionValue<'_>> {
    match expr? {
        Expr::Arrow(arrow) => Some(FunctionValue::Arrow(arrow)),
        Expr::Fn(function) => Some(FunctionValue::Fn(&function.function)),
        _ => None,
    }
}

/// The binding a constructor parameter property declares, whether or not it
/// carries a default (`public receipts: Refunds = new Refunds()`).
fn param_prop_ident(param: &TsParamPropParam) -> Option<&BindingIdent> {
    match param {
        TsParamPropParam::Ident(ident) => Some(ident),
        TsParamPropParam::Assign(assign) => match &*assign.left {
            Pat::Ident(ident) => Some(ident),
            _ => None,
        },
    }
}

/// Whether a class member is part of the surface a consumer can write.
/// TypeScript forbids reaching a `private` or `protected` member from outside
/// the class, so publishing one names a chain nobody can call.
fn is_public(accessibility: Option<Accessibility>) -> bool {
    !matches!(
        accessibility,
        Some(Accessibility::Private) | Some(Accessibility::Protected)
    )
}

struct SurfaceScanner {
    repo_root: PathBuf,
    source_map: Lrc<SourceMap>,
    handler: Handler,
    bindings: BindingResolver,
    modules: HashMap<PathBuf, Option<Rc<Module>>>,
    members: Vec<SdkMember>,
    truncated: bool,
}

impl SurfaceScanner {
    fn new(repo_root: &Path) -> Self {
        let source_map: Lrc<SourceMap> = Default::default();
        // Quiet diagnostics: an unparseable module is a resolution miss here,
        // the same way it is for the binding resolver.
        let handler =
            Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(source_map.clone()));
        Self {
            repo_root: repo_root.to_path_buf(),
            source_map,
            handler,
            bindings: BindingResolver::new(),
            modules: HashMap::new(),
            members: Vec::new(),
            truncated: false,
        }
    }

    fn walk_entry(&mut self, entry: &Path) {
        for export in self.bindings.export_names(entry) {
            let Some(binding) = self.bindings.resolve_export(entry, &export) else {
                continue;
            };
            let Some(declared) = self.declaration_of(&binding.file, binding.local_name.as_deref())
            else {
                continue;
            };
            let mut visited = HashSet::new();
            self.walk(&export, "", declared, None, 0, &mut visited);
        }
    }

    /// The class or object literal an export's local binding names in `file`.
    ///
    /// `local_name` is what the resolver read off the export table; `None` is
    /// an anonymous default (`export default class { ... }`), which is found
    /// through the default declaration itself.
    fn declaration_of(&mut self, file: &Path, local_name: Option<&str>) -> Option<Declared> {
        let module = self.module_of(file)?;
        match local_name {
            Some(name) => self.named_declaration(file, &module, name),
            None => anonymous_default(file, &module),
        }
    }

    /// A top-level `class <name>` or `const <name> = ...` in `module`.
    fn named_declaration(
        &mut self,
        file: &Path,
        module: &Rc<Module>,
        name: &str,
    ) -> Option<Declared> {
        for item in &module.body {
            let decl = match item {
                ModuleItem::Stmt(Stmt::Decl(decl)) => decl,
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => &export.decl,
                ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export)) => {
                    if let DefaultDecl::Class(class) = &export.decl
                        && class
                            .ident
                            .as_ref()
                            .is_some_and(|id| id.sym.as_ref() == name)
                    {
                        return Some(Declared::Class(
                            file.to_path_buf(),
                            module.clone(),
                            Rc::new((*class.class).clone()),
                        ));
                    }
                    continue;
                }
                _ => continue,
            };
            match decl {
                Decl::Fn(function) if function.ident.sym.as_ref() == name => {
                    return Some(Declared::Function(
                        file.to_path_buf(),
                        function.function.span,
                    ));
                }
                Decl::Class(class) if class.ident.sym.as_ref() == name => {
                    return Some(Declared::Class(
                        file.to_path_buf(),
                        module.clone(),
                        Rc::new((*class.class).clone()),
                    ));
                }
                Decl::Var(var) => {
                    for declarator in &var.decls {
                        let Pat::Ident(ident) = &declarator.name else {
                            continue;
                        };
                        if ident.id.sym.as_ref() != name {
                            continue;
                        }
                        if let Some(init) = declarator.init.as_deref() {
                            return self.value_declaration(file, module, init);
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// What an initialiser expression resolves to: an inline object literal, a
    /// `new X(...)` whose class this can reach, or another binding.
    fn value_declaration(
        &mut self,
        file: &Path,
        module: &Rc<Module>,
        expr: &Expr,
    ) -> Option<Declared> {
        match expr {
            Expr::Object(object) => Some(Declared::Object(
                file.to_path_buf(),
                module.clone(),
                Rc::new(object.clone()),
            )),
            Expr::New(new_expr) => {
                let path = value_path(&new_expr.callee)?;
                self.resolve_type_path(file, module, &path)
            }
            Expr::Ident(_) | Expr::Member(_) => {
                let path = value_path(expr)?;
                self.resolve_type_path(file, module, &path)
            }
            Expr::Arrow(arrow) => Some(Declared::Function(file.to_path_buf(), arrow.span)),
            Expr::Fn(function) => Some(Declared::Function(
                file.to_path_buf(),
                function.function.span,
            )),
            Expr::TsAs(cast) => self.value_declaration(file, module, &cast.expr),
            Expr::TsNonNull(inner) => self.value_declaration(file, module, &inner.expr),
            Expr::Paren(inner) => self.value_declaration(file, module, &inner.expr),
            _ => None,
        }
    }

    /// Resolve a dotted name written in `file` — `Payments`, or `API.Payments`
    /// through a namespace import — to the declaration it names.
    ///
    /// A declaration in this very file wins; otherwise the file's own import
    /// table is consulted, and only a relative specifier is followed.
    fn resolve_type_path(
        &mut self,
        file: &Path,
        module: &Rc<Module>,
        path: &[String],
    ) -> Option<Declared> {
        let root = path.first()?;
        if path.len() == 1
            && let Some(found) = self.named_declaration(file, module, root)
        {
            return Some(found);
        }
        let (specifier, exported) = import_source(module, root, path.get(1).map(String::as_str))?;
        let binding = self.bindings.resolve(file, &specifier, &exported)?;
        self.declaration_of(&binding.file, binding.local_name.as_deref())
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        export: &str,
        prefix: &str,
        declared: Declared,
        this_ctx: Option<&ClassCtx>,
        depth: usize,
        visited: &mut HashSet<(PathBuf, BytePos)>,
    ) {
        if depth > MAX_DEPTH || self.truncated {
            return;
        }
        let anchor = declared.anchor();
        if !visited.insert(anchor.clone()) {
            return; // a field graph that reaches back into itself
        }
        match &declared {
            Declared::Class(file, module, class) => {
                let ctx = ClassCtx {
                    file: file.clone(),
                    module: module.clone(),
                    class: class.clone(),
                };
                self.walk_class(export, prefix, &ctx, depth, visited);
            }
            Declared::Object(file, module, object) => {
                let (file, module, object) = (file.clone(), module.clone(), object.clone());
                self.walk_object(
                    export, prefix, &file, &module, &object, this_ctx, depth, visited,
                );
            }
            // A bare exported function has no member chain under it: the
            // consumer calls the export itself, which `import_symbol` already
            // names.
            Declared::Function(..) => {}
        }
        visited.remove(&anchor);
    }

    fn walk_class(
        &mut self,
        export: &str,
        prefix: &str,
        ctx: &ClassCtx,
        depth: usize,
        visited: &mut HashSet<(PathBuf, BytePos)>,
    ) {
        let class = ctx.class.clone();
        for member in &class.body {
            match member {
                // An overload signature carries no body and no implementation
                // to anchor to; the implementation that follows it does.
                ClassMember::Method(method)
                    if method.function.body.is_some()
                        && method.kind == MethodKind::Method
                        && is_public(method.accessibility) =>
                {
                    let Some(name) = member_name(&method.key) else {
                        continue;
                    };
                    let delegates =
                        self.delegates_of(ctx, this_calls_of_function(&method.function));
                    self.emit(export, prefix, &name, &ctx.file, method.span, delegates);
                    let returned = self.returned_declaration(
                        ctx,
                        method.function.return_type.as_deref(),
                        method
                            .function
                            .body
                            .as_ref()
                            .and_then(block_returned_expr)
                            .cloned(),
                    );
                    self.hop(export, prefix, &name, returned, ctx, depth, visited);
                }
                ClassMember::ClassProp(property) if is_public(property.accessibility) => {
                    let Some(name) = member_name(&property.key) else {
                        continue;
                    };
                    // A field holding a function IS a callable member — and, if
                    // that function hands back a sub-resource, a hop as well.
                    if let Some(function) = function_value(property.value.as_deref()) {
                        let delegates = self.delegates_of(ctx, function.this_calls());
                        self.emit(export, prefix, &name, &ctx.file, property.span, delegates);
                        let returned = self.returned_declaration(
                            ctx,
                            function.return_type(),
                            function.returned_expr(),
                        );
                        self.hop(export, prefix, &name, returned, ctx, depth, visited);
                        continue;
                    }
                    // An inline object literal keeps this class's `this`: the
                    // arrows inside it are declared in its body.
                    if let Some(Expr::Object(object)) = property.value.as_deref() {
                        let declared = Declared::Object(
                            ctx.file.clone(),
                            ctx.module.clone(),
                            Rc::new(object.clone()),
                        );
                        self.walk_member(
                            export,
                            prefix,
                            &name,
                            declared,
                            Some(ctx),
                            depth,
                            visited,
                        );
                        continue;
                    }
                    // Otherwise the field is a sub-resource when its declared
                    // type or its `new X(...)` initialiser names a class this
                    // can reach. The annotation is read first: the shape this
                    // exists for declares the type and assigns the instance on
                    // one line, and the two always agree there.
                    let declared = annotation_path(property.type_ann.as_deref())
                        .and_then(|path| self.resolve_type_path(&ctx.file, &ctx.module, &path))
                        .or_else(|| {
                            property.value.as_deref().and_then(|value| {
                                self.value_declaration(&ctx.file, &ctx.module, value)
                            })
                        });
                    let Some(declared) = declared else {
                        continue;
                    };
                    self.walk_member(export, prefix, &name, declared, None, depth, visited);
                }
                // `constructor(public payments: Payments) {}` declares a field
                // as surely as a `ClassProp` does.
                ClassMember::Constructor(constructor) => {
                    for parameter in constructor.params.clone() {
                        let ParamOrTsParamProp::TsParamProp(property) = parameter else {
                            continue;
                        };
                        if !is_public(property.accessibility) {
                            continue;
                        }
                        let Some(ident) = param_prop_ident(&property.param) else {
                            continue;
                        };
                        let name = ident.id.sym.to_string();
                        let Some(declared) = annotation_path(ident.type_ann.as_deref())
                            .and_then(|path| self.resolve_type_path(&ctx.file, &ctx.module, &path))
                        else {
                            continue;
                        };
                        self.walk_member(export, prefix, &name, declared, None, depth, visited);
                    }
                }
                _ => {}
            }
        }
    }

    /// Compose a returned sub-resource's members under `name`, so a consumer's
    /// `client.transfers().send(...)` finds `transfers.send`. The
    /// member itself was already emitted: this only walks what it hands back.
    #[allow(clippy::too_many_arguments)]
    fn hop(
        &mut self,
        export: &str,
        prefix: &str,
        name: &str,
        returned: Option<Declared>,
        ctx: &ClassCtx,
        depth: usize,
        visited: &mut HashSet<(PathBuf, BytePos)>,
    ) {
        let Some(declared) = returned else {
            return;
        };
        // A returned function is not a resource to compose under: it is called
        // by the member's own name.
        if declared.function_span().is_some() {
            return;
        }
        let nested = format!("{}{}.", prefix, name);
        self.walk(export, &nested, declared, Some(ctx), depth + 1, visited);
    }

    /// A resolved field or property: a function is the member itself, anything
    /// else is a sub-resource whose own members compose under this name.
    #[allow(clippy::too_many_arguments)]
    fn walk_member(
        &mut self,
        export: &str,
        prefix: &str,
        name: &str,
        declared: Declared,
        this_ctx: Option<&ClassCtx>,
        depth: usize,
        visited: &mut HashSet<(PathBuf, BytePos)>,
    ) {
        if let Some((file, span)) = declared.function_span() {
            self.emit(export, prefix, name, &file, span, Vec::new());
            return;
        }
        let nested = format!("{}{}.", prefix, name);
        self.walk(export, &nested, declared, this_ctx, depth + 1, visited);
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_object(
        &mut self,
        export: &str,
        prefix: &str,
        file: &Path,
        module: &Rc<Module>,
        object: &ObjectLit,
        this_ctx: Option<&ClassCtx>,
        depth: usize,
        visited: &mut HashSet<(PathBuf, BytePos)>,
    ) {
        for property in &object.props {
            let PropOrSpread::Prop(prop) = property else {
                continue;
            };
            match &**prop {
                Prop::Method(method) => {
                    let Some(name) = member_name(&method.key) else {
                        continue;
                    };
                    let delegates = self.object_delegates(this_ctx, &method.function);
                    self.emit(export, prefix, &name, file, method.function.span, delegates);
                }
                Prop::KeyValue(entry) => {
                    let Some(name) = member_name(&entry.key) else {
                        continue;
                    };
                    if let Some(function) = function_value(Some(&entry.value)) {
                        let delegates = match this_ctx {
                            Some(ctx) => self.delegates_of(ctx, function.this_calls()),
                            None => Vec::new(),
                        };
                        self.emit(export, prefix, &name, file, entry.value.span(), delegates);
                        continue;
                    }
                    let Some(declared) = self.value_declaration(file, module, &entry.value) else {
                        continue;
                    };
                    self.walk_member(export, prefix, &name, declared, this_ctx, depth, visited);
                }
                // `{ payments }` is `{ payments: payments }`.
                Prop::Shorthand(ident) => {
                    let name = ident.sym.to_string();
                    let Some(declared) =
                        self.resolve_type_path(file, module, std::slice::from_ref(&name))
                    else {
                        continue;
                    };
                    self.walk_member(export, prefix, &name, declared, this_ctx, depth, visited);
                }
                _ => {}
            }
        }
    }

    fn object_delegates(
        &mut self,
        this_ctx: Option<&ClassCtx>,
        function: &Function,
    ) -> Vec<SdkSpan> {
        match this_ctx {
            Some(ctx) => self.delegates_of(ctx, this_calls_of_function(function)),
            None => Vec::new(),
        }
    }

    fn emit(
        &mut self,
        export: &str,
        prefix: &str,
        name: &str,
        file: &Path,
        span: Span,
        delegates: Vec<SdkSpan>,
    ) {
        if self.members.len() >= MAX_MEMBERS {
            if !self.truncated {
                warn!(
                    "SDK surface hit the {} member cap; the remainder is not published",
                    MAX_MEMBERS
                );
                self.truncated = true;
            }
            return;
        }
        self.members.push(SdkMember {
            export: export.to_string(),
            chain: format!("{}{}", prefix, name),
            file: self.repo_relative(file),
            line: self.line_of(span.lo),
            end_line: self.line_of(span.hi),
            delegates,
        });
    }

    /// What a method or function-valued field hands back, when that is a
    /// declaration this can reach.
    ///
    /// The declared return type is read first — same rule as a field's
    /// annotation — and the returned expression is the fallback for the
    /// commonest accessor of all, `transfers = () => this.transfersClient`, which
    /// declares nothing. A `Promise<Payments>` return type names nothing here,
    /// by the same no-generics rule fields follow.
    fn returned_declaration(
        &mut self,
        ctx: &ClassCtx,
        return_type: Option<&TsTypeAnn>,
        returned: Option<Expr>,
    ) -> Option<Declared> {
        if let Some(path) = annotation_path(return_type)
            && let Some(declared) = self.resolve_type_path(&ctx.file, &ctx.module, &path)
        {
            return Some(declared);
        }
        let expr = returned?;
        if let Some(field) = this_field_name(&expr) {
            return self.class_field(ctx, &field);
        }
        self.value_declaration(&ctx.file, &ctx.module, &expr)
    }

    /// The declaration a `this.<name>` receiver holds: a field, or a
    /// constructor parameter property (`constructor(private api: Api) {}`),
    /// which is the shape every layered client in the wild writes.
    ///
    /// Accessibility is deliberately not consulted. This resolves what the
    /// SDK's own code reaches, not what it publishes: a public accessor
    /// returning `this.transfersClient` has to find that private field.
    fn class_field(&mut self, ctx: &ClassCtx, name: &str) -> Option<Declared> {
        let class = ctx.class.clone();
        for member in &class.body {
            match member {
                ClassMember::ClassProp(property)
                    if member_name(&property.key).as_deref() == Some(name) =>
                {
                    return annotation_path(property.type_ann.as_deref())
                        .and_then(|path| self.resolve_type_path(&ctx.file, &ctx.module, &path))
                        .or_else(|| {
                            property.value.as_deref().and_then(|value| {
                                self.value_declaration(&ctx.file, &ctx.module, value)
                            })
                        });
                }
                ClassMember::Constructor(constructor) => {
                    for parameter in &constructor.params {
                        let ParamOrTsParamProp::TsParamProp(property) = parameter else {
                            continue;
                        };
                        let Some(ident) = param_prop_ident(&property.param) else {
                            continue;
                        };
                        if ident.id.sym.as_ref() != name {
                            continue;
                        }
                        let path = annotation_path(ident.type_ann.as_deref())?;
                        return self.resolve_type_path(&ctx.file, &ctx.module, &path);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The spans of the same-repo methods a member's body calls, and what those
    /// call in turn. See the module docs on delegates.
    fn delegates_of(&mut self, ctx: &ClassCtx, calls: Vec<Vec<String>>) -> Vec<SdkSpan> {
        let mut spans = Vec::new();
        let mut seen = HashSet::new();
        self.collect_delegates(ctx, calls, 0, &mut seen, &mut spans);
        spans.sort();
        spans.dedup();
        spans
    }

    fn collect_delegates(
        &mut self,
        ctx: &ClassCtx,
        calls: Vec<Vec<String>>,
        depth: usize,
        seen: &mut HashSet<(PathBuf, BytePos)>,
        spans: &mut Vec<SdkSpan>,
    ) {
        if depth >= MAX_DELEGATE_DEPTH {
            return;
        }
        for path in calls {
            if spans.len() >= MAX_DELEGATES {
                return;
            }
            let Some((target, span)) = self.resolve_this_call(ctx, &path) else {
                continue;
            };
            if !seen.insert((target.file.clone(), span.lo)) {
                continue;
            }
            spans.push(SdkSpan {
                file: self.repo_relative(&target.file),
                line: self.line_of(span.lo),
                end_line: self.line_of(span.hi),
            });
            let Some(name) = path.last() else {
                continue;
            };
            let nested = callable_this_calls(&target.class, name);
            self.collect_delegates(&target, nested, depth + 1, seen, spans);
        }
    }

    /// Follow a `this`-rooted call path — `["api", "send"]` — to
    /// the class method it names, and to the context that method's own `this`
    /// resolves against. Only class hops are followed: an intermediate field
    /// holding an object literal names no declaration to look a method up in.
    fn resolve_this_call(&mut self, ctx: &ClassCtx, path: &[String]) -> Option<(ClassCtx, Span)> {
        let (name, fields) = path.split_last()?;
        let mut current = ctx.clone();
        for field in fields {
            let Declared::Class(file, module, class) = self.class_field(&current, field)? else {
                return None;
            };
            current = ClassCtx {
                file,
                module,
                class,
            };
        }
        let span = class_callable_span(&current.class, name)?;
        Some((current, span))
    }

    fn line_of(&self, pos: BytePos) -> u32 {
        self.source_map.lookup_char_pos(pos).line as u32
    }

    fn repo_relative(&self, file: &Path) -> String {
        file.strip_prefix(&self.repo_root)
            .unwrap_or(file)
            .to_string_lossy()
            .to_string()
    }

    fn module_of(&mut self, file: &Path) -> Option<Rc<Module>> {
        if !self.modules.contains_key(file) {
            let parsed = parse_file(file, &self.source_map, &self.handler).map(Rc::new);
            self.modules.insert(file.to_path_buf(), parsed);
        }
        self.modules.get(file).and_then(Clone::clone)
    }
}

/// The class or object literal an anonymous `export default` declares.
fn anonymous_default(file: &Path, module: &Rc<Module>) -> Option<Declared> {
    for item in &module.body {
        match item {
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export)) => match &export.decl {
                DefaultDecl::Class(class) => {
                    return Some(Declared::Class(
                        file.to_path_buf(),
                        module.clone(),
                        Rc::new((*class.class).clone()),
                    ));
                }
                DefaultDecl::Fn(function) => {
                    return Some(Declared::Function(
                        file.to_path_buf(),
                        function.function.span,
                    ));
                }
                DefaultDecl::TsInterfaceDecl(_) => {}
            },
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(export)) => {
                if let Expr::Object(object) = &*export.expr {
                    return Some(Declared::Object(
                        file.to_path_buf(),
                        module.clone(),
                        Rc::new(object.clone()),
                    ));
                }
            }
            _ => {}
        }
    }
    None
}

/// The span of the callable `name` a class declares: a method with a body, or
/// a field holding a function.
fn class_callable_span(class: &Class, name: &str) -> Option<Span> {
    for member in &class.body {
        match member {
            ClassMember::Method(method)
                if method.function.body.is_some()
                    && member_name(&method.key).as_deref() == Some(name) =>
            {
                return Some(method.span);
            }
            ClassMember::ClassProp(property)
                if member_name(&property.key).as_deref() == Some(name)
                    && function_value(property.value.as_deref()).is_some() =>
            {
                return Some(property.span);
            }
            _ => {}
        }
    }
    None
}

/// The `this`-rooted calls in the body of the callable `name` a class declares.
fn callable_this_calls(class: &Class, name: &str) -> Vec<Vec<String>> {
    for member in &class.body {
        match member {
            ClassMember::Method(method)
                if method.function.body.is_some()
                    && member_name(&method.key).as_deref() == Some(name) =>
            {
                return this_calls_of_function(&method.function);
            }
            ClassMember::ClassProp(property)
                if member_name(&property.key).as_deref() == Some(name) =>
            {
                if let Some(function) = function_value(property.value.as_deref()) {
                    return function.this_calls();
                }
            }
            _ => {}
        }
    }
    Vec::new()
}

fn this_calls_of_function(function: &Function) -> Vec<Vec<String>> {
    let mut collector = ThisCallCollector::default();
    if let Some(body) = &function.body {
        body.visit_with(&mut collector);
    }
    collector.paths
}

fn this_calls_of_arrow(arrow: &ArrowExpr) -> Vec<Vec<String>> {
    let mut collector = ThisCallCollector::default();
    arrow.body.visit_with(&mut collector);
    collector.paths
}

/// Every call whose receiver chain starts at `this`, as the property names it
/// walks: `this.api.send(...)` -> `["api", "send"]`.
#[derive(Default)]
struct ThisCallCollector {
    paths: Vec<Vec<String>>,
}

impl Visit for ThisCallCollector {
    fn visit_call_expr(&mut self, node: &CallExpr) {
        if let Callee::Expr(callee) = &node.callee
            && let Some(path) = this_rooted_path(callee)
        {
            self.paths.push(path);
        }
        node.visit_children_with(self);
    }
}

fn this_rooted_path(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::Member(member) => {
            let name = member.prop.as_ident()?.sym.to_string();
            match &*member.obj {
                Expr::This(_) => Some(vec![name]),
                other => {
                    let mut path = this_rooted_path(other)?;
                    path.push(name);
                    Some(path)
                }
            }
        }
        Expr::Paren(inner) => this_rooted_path(&inner.expr),
        _ => None,
    }
}

/// The field name a `this.<field>` expression reads, and nothing else.
fn this_field_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Member(member) if matches!(&*member.obj, Expr::This(_)) => {
            Some(member.prop.as_ident()?.sym.to_string())
        }
        Expr::Paren(inner) => this_field_name(&inner.expr),
        Expr::TsAs(cast) => this_field_name(&cast.expr),
        Expr::TsNonNull(inner) => this_field_name(&inner.expr),
        _ => None,
    }
}

/// The expression an arrow hands back: its expression body, or the last
/// top-level `return` in its block.
fn arrow_returned_expr(arrow: &ArrowExpr) -> Option<&Expr> {
    match &*arrow.body {
        BlockStmtOrExpr::Expr(expr) => Some(expr),
        BlockStmtOrExpr::BlockStmt(block) => block_returned_expr(block),
    }
}

fn block_returned_expr(block: &BlockStmt) -> Option<&Expr> {
    block.stmts.iter().rev().find_map(|stmt| match stmt {
        Stmt::Return(statement) => statement.arg.as_deref(),
        _ => None,
    })
}

fn member_name(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(name) if !name.value.contains('.') => Some(name.value.to_string()),
        _ => None,
    }
}

/// The dotted name a type annotation writes: `["Payments"]`, or
/// `["API", "Payments"]`. Anything generic, unioned or inline names nothing.
fn annotation_path(annotation: Option<&TsTypeAnn>) -> Option<Vec<String>> {
    let TsType::TsTypeRef(reference) = &*annotation?.type_ann else {
        return None;
    };
    if reference.type_params.is_some() {
        return None;
    }
    Some(entity_path(&reference.type_name))
}

fn entity_path(name: &TsEntityName) -> Vec<String> {
    match name {
        TsEntityName::Ident(ident) => vec![ident.sym.to_string()],
        TsEntityName::TsQualifiedName(qualified) => {
            let mut path = entity_path(&qualified.left);
            path.push(qualified.right.sym.to_string());
            path
        }
    }
}

/// The dotted name a value expression writes: `Payments` or `API.Payments`.
fn value_path(expr: &Expr) -> Option<Vec<String>> {
    match expr {
        Expr::Ident(ident) => Some(vec![ident.sym.to_string()]),
        Expr::Member(member) => {
            let mut path = value_path(&member.obj)?;
            path.push(member.prop.as_ident()?.sym.to_string());
            Some(path)
        }
        _ => None,
    }
}

/// Where a name written in `module` was imported from, and under which export
/// name it lives in that module.
///
/// Three import shapes bind a name a class can be reached through: a named
/// import (`import { Payments } from './payments'`), a default import, and a
/// namespace import (`import * as API from './resources'`), where the class is
/// the SECOND segment of the written path (`API.Payments`). Only relative
/// specifiers are returned — a package import is another repo's business.
fn import_source(module: &Module, root: &str, qualifier: Option<&str>) -> Option<(String, String)> {
    for item in &module.body {
        let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = item else {
            continue;
        };
        if import.type_only {
            continue;
        }
        let specifier = import.src.value.to_string();
        if !(specifier.starts_with("./") || specifier.starts_with("../")) {
            continue;
        }
        for spec in &import.specifiers {
            match spec {
                ImportSpecifier::Named(named)
                    if !named.is_type_only && named.local.sym.as_ref() == root =>
                {
                    let exported = named
                        .imported
                        .as_ref()
                        .map(|name| match name {
                            ModuleExportName::Ident(ident) => ident.sym.to_string(),
                            ModuleExportName::Str(text) => text.value.to_string(),
                        })
                        .unwrap_or_else(|| named.local.sym.to_string());
                    return Some((specifier, exported));
                }
                ImportSpecifier::Default(default) if default.local.sym.as_ref() == root => {
                    return Some((specifier, DEFAULT_EXPORT.to_string()));
                }
                ImportSpecifier::Namespace(namespace) if namespace.local.sym.as_ref() == root => {
                    // `API.Payments` names the module's `Payments` export; a
                    // bare `API` names the module itself, not a class.
                    return qualifier.map(|name| (specifier, name.to_string()));
                }
                _ => {}
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sdk-surface")
    }

    fn member<'a>(members: &'a [SdkMember], export: &str, chain: &str) -> Option<&'a SdkMember> {
        members
            .iter()
            .find(|m| m.export == export && m.chain == chain)
    }

    /// The published layout the join is built for: a default-exported client
    /// class whose fields are resource classes reached through a namespace
    /// import and a barrel, each contributing its methods under the field name.
    #[test]
    fn walks_a_default_class_through_namespace_fields_and_a_barrel() {
        let root = fixture();
        let members = scan(&root, &root);

        let create = member(&members, "default", "payments.create").expect("payments.create");
        assert_eq!(create.file, "src/resources/payments.ts");
        // The overload IMPLEMENTATION, not either signature above it.
        assert_eq!((create.line, create.end_line), (10, 15));

        assert!(member(&members, "default", "payments.list").is_some());

        // A method on the root class itself carries no field prefix.
        let scrape = member(&members, "default", "scrape").expect("scrape");
        assert_eq!(scrape.file, "src/index.ts");
        assert!(scrape.line < scrape.end_line);
    }

    /// A resource that itself holds a resource: the chain keeps composing, and
    /// the span stays with the file that declares the method.
    #[test]
    fn nested_resource_fields_compose_the_chain() {
        let root = fixture();
        let members = scan(&root, &root);

        let issue =
            member(&members, "default", "payments.refunds.issue").expect("payments.refunds.issue");
        assert_eq!(issue.file, "src/resources/refunds.ts");
    }

    /// A field holding an arrow function IS a callable member, not a
    /// sub-resource to recurse into.
    #[test]
    fn a_function_valued_field_is_a_member() {
        let root = fixture();
        let members = scan(&root, &root);
        let monthly = member(&members, "default", "reports.monthly").expect("reports.monthly");
        assert_eq!(monthly.file, "src/resources/reports.ts");
    }

    /// `export const admin = { settings, purge() {} }` publishes under its own
    /// export name, and a shorthand property naming another object recurses.
    #[test]
    fn an_exported_object_literal_publishes_its_properties() {
        let root = fixture();
        let members = scan(&root, &root);
        assert!(member(&members, "admin", "purge").is_some());
        assert!(member(&members, "admin", "settings.read").is_some());
    }

    /// A property naming a function is a callable member, whether the function
    /// is declared in the same file or imported from another one. Without this
    /// the commonest published shape of all — a bag of exported functions —
    /// contributes nothing, while the equivalent class field already does.
    #[test]
    fn an_object_property_naming_a_function_is_a_member() {
        let root = fixture();
        let members = scan(&root, &root);

        // Shorthand naming a function imported from another module.
        let charge = member(&members, "admin", "chargeCard").expect("admin.chargeCard");
        assert_eq!(charge.file, "src/util/direct.ts");

        // Key/value naming a function declared in the same file.
        let cancel = member(&members, "admin", "cancel").expect("admin.cancel");
        assert_eq!(cancel.file, "src/index.ts");

        // And a function is never recursed into as if it were a resource.
        assert!(!members.iter().any(|m| m.chain.starts_with("chargeCard.")));
    }

    /// The shape a layered client publishes: `ledger.transfers().send(...)`.
    /// The accessor is a callable member of its own AND a hop, so the resource
    /// it hands back composes under its name — which is the chain the consumer
    /// writes, with the call in the middle.
    #[test]
    fn a_member_that_hands_back_a_resource_composes_under_its_name() {
        let root = fixture();
        let members = scan(&root, &root);

        // The arrow-valued field, resolved through the private field it reads.
        let send = member(&members, "default", "transfers.send").expect("transfers.send");
        assert_eq!(send.file, "src/resources/transfers.ts");
        // And the accessor itself stays callable.
        assert!(member(&members, "default", "transfers").is_some());

        // The method form, resolved through its declared return type.
        let issue = member(&members, "default", "refunds.issue").expect("refunds.issue");
        assert_eq!(issue.file, "src/resources/refunds.ts");
    }

    /// A layered client writes its route one hop below the published method:
    /// `Transfers.send` calls `this.api.send(...)`, and only THAT method holds
    /// the URL. The member carries that span so the join can find the call.
    #[test]
    fn a_member_carries_the_spans_it_delegates_to() {
        let root = fixture();
        let members = scan(&root, &root);

        let send = member(&members, "default", "transfers.send").expect("transfers.send");
        let delegate = send
            .delegates
            .iter()
            .find(|span| span.file == "src/api/transfers.ts")
            .expect("the api wrapper's span");
        assert!(delegate.line < delegate.end_line);

        // A member that calls nothing of its own delegates to nothing.
        let list = member(&members, "default", "payments.list").expect("payments.list");
        assert!(list.delegates.is_empty());
    }

    /// TypeScript forbids a consumer writing a `private` member, so publishing
    /// one names a chain nobody can call. It stays resolvable internally: the
    /// public accessor above reaches its resource through exactly that field.
    #[test]
    fn private_members_are_not_surface() {
        let root = fixture();
        let members = scan(&root, &root);
        assert!(
            !members
                .iter()
                .any(|m| m.chain == "transfersClient" || m.chain.starts_with("transfersClient.")),
            "{:?}",
            members
        );
        assert!(member(&members, "default", "transfers.send").is_some());
    }

    /// `constructor(public receipts: Refunds)` declares a field as surely as a
    /// `ClassProp` does, and a consumer writes it the same way.
    #[test]
    fn a_public_constructor_parameter_property_is_a_field() {
        let root = fixture();
        let members = scan(&root, &root);
        assert!(member(&members, "default", "receipts.issue").is_some());
        // The private parameter property beside it is not surface.
        assert!(!members.iter().any(|m| m.chain.starts_with("baseUrl")));
    }

    /// A getter is not a call, a constructor is not a member, and neither is
    /// an overload signature — only bodies that implement something.
    #[test]
    fn getters_constructors_and_overload_signatures_are_not_members() {
        let root = fixture();
        let members = scan(&root, &root);
        assert!(member(&members, "default", "baseHost").is_none());
        assert!(member(&members, "default", "constructor").is_none());
        assert_eq!(
            members
                .iter()
                .filter(|m| m.chain == "payments.create")
                .count(),
            1
        );
    }

    /// The declared entries point at `dist/`, which does not exist in a source
    /// checkout — the walk must fall back to `src/index.ts` rather than
    /// resolving nothing, and must never anchor a member in build output.
    #[test]
    fn build_output_entries_fall_back_to_the_source_index() {
        let root = fixture();
        let entry = resolve_entry_module(&root).expect("entry module");
        assert!(entry.ends_with("src/index.ts"));

        assert!(!is_typescript_source(Path::new("/repo/dist/index.d.ts")));
        assert!(!is_typescript_source(Path::new("/repo/dist/index.ts")));
        assert!(!is_typescript_source(Path::new("/repo/src/index.js")));
        assert!(is_typescript_source(Path::new("/repo/src/index.ts")));
    }

    /// `exports` is read from the `"."` subpath only — a bare string, a string,
    /// or a condition tree — and the well-known conditions are read first.
    #[test]
    fn entry_specifiers_are_read_in_declared_order() {
        let manifest = serde_json::json!({
            "exports": { ".": { "types": "./t.d.ts", "import": "./i.mjs" } },
            "types": "./types.d.ts",
            "main": "./main.js",
        });
        assert_eq!(
            declared_entry_specifiers(&manifest),
            vec!["./t.d.ts", "./i.mjs", "./types.d.ts", "./main.js"]
        );

        let bare = serde_json::json!({ "exports": "./src/index.ts" });
        assert_eq!(declared_entry_specifiers(&bare), vec!["./src/index.ts"]);

        // Only the root subpath. A package's other entries publish other
        // modules, and walking one of them would anchor members to a surface
        // no root import reaches.
        let nested = serde_json::json!({ "exports": { "./sub": "./src/sub.ts" } });
        assert!(declared_entry_specifiers(&nested).is_empty());
    }

    /// carrick#656: a package that publishes both module systems nests its
    /// conditions, and the source path sits under a private condition beside
    /// them. Reading three keys at the top level found nothing, so the package
    /// published no surface at all and every consumer of its client was
    /// `member_not_found`.
    #[test]
    fn a_nested_condition_tree_offers_every_leaf_it_holds() {
        let manifest = serde_json::json!({
            "exports": {
                ".": {
                    "import": {
                        "@scope/source": "./src/v3/index.ts",
                        "types": "./dist/esm/index.d.ts",
                        "default": "./dist/esm/index.js",
                    },
                    "require": {
                        "types": "./dist/cjs/index.d.ts",
                        "default": "./dist/cjs/index.js",
                    },
                },
                "./v3": { "import": { "@scope/source": "./src/v3/index.ts" } },
            },
            "main": "./dist/cjs/index.js",
        });
        assert_eq!(
            declared_entry_specifiers(&manifest),
            vec![
                // `import` before `require`, and inside each the well-known
                // conditions before the private one. `./v3` is another
                // subpath and contributes nothing; `main` comes last.
                "./dist/esm/index.d.ts",
                "./dist/esm/index.js",
                "./src/v3/index.ts",
                "./dist/cjs/index.d.ts",
                "./dist/cjs/index.js",
                "./dist/cjs/index.js",
            ]
        );
    }

    /// The same tree end to end: only the source leaf resolves, so it is the
    /// entry, and the `dist` and `.d.ts` leaves beside it are dropped.
    #[test]
    fn a_nested_condition_tree_resolves_to_the_source_leaf() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src/v3")).expect("mkdir");
        std::fs::write(root.join("src/v3/index.ts"), "export const x = 1;\n").expect("write");
        std::fs::write(
            root.join("package.json"),
            serde_json::json!({
                "name": "@fixture/nested",
                "main": "./dist/cjs/index.js",
                "types": "./dist/cjs/index.d.ts",
                "exports": {
                    ".": {
                        "import": {
                            "@fixture/source": "./src/v3/index.ts",
                            "types": "./dist/esm/index.d.ts",
                            "default": "./dist/esm/index.js",
                        },
                    },
                },
            })
            .to_string(),
        )
        .expect("write");

        let entry = resolve_entry_module(root).expect("an entry resolves");
        assert!(
            entry.ends_with("src/v3/index.ts"),
            "resolved {}",
            entry.display()
        );
    }

    /// A repo that publishes no client resolves an entry whose exports reach
    /// no class or object literal, and contributes nothing.
    #[test]
    fn a_repo_with_no_entry_module_publishes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(scan(tmp.path(), tmp.path()).is_empty());
    }
}
