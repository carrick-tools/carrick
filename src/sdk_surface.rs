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
//! (`exports["."]`, `types`, `main`, falling back to `src/index.ts`), and the
//! class graph reachable from its exports:
//!
//! - an exported **class** contributes one member per method, and recurses
//!   into each field whose declared type or `new X(...)` initialiser names a
//!   class declared in the repo (`payments: API.Payments = new
//!   API.Payments(this)` → the `Payments` class, with `payments.` prefixed to
//!   every member it contributes);
//! - an exported **object literal** contributes the same way over its
//!   properties;
//! - `export { default as Ledger } from './client'` and `export default
//!   Ledger` both publish under the export name `default`, which is what a
//!   consumer's default import binds.
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
    Class, ClassMember, Decl, DefaultDecl, Expr, ImportSpecifier, MethodKind, Module, ModuleDecl,
    ModuleExportName, ModuleItem, ObjectLit, Pat, Prop, PropName, PropOrSpread, Stmt, TsEntityName,
    TsType, TsTypeAnn,
};
use tracing::{debug, warn};

/// The export name a default export is published under.
const DEFAULT_EXPORT: &str = "default";

/// How many field hops a member chain may take from an exported root.
const MAX_DEPTH: usize = 4;

/// Hard cap on emitted members. A surface this size is a resolution accident,
/// not a client library; truncating keeps the payload bounded and logs loudly.
const MAX_MEMBERS: usize = 20_000;

/// Directories whose contents are build output or dependencies, never the
/// package's own source.
const NON_SOURCE_DIRS: [&str; 4] = ["node_modules", "dist", "build", ".next"];

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
    /// the SDK's own outbound call sits somewhere inside it.
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
/// `exports` is read narrowly on purpose: a bare string, or the `"."` subpath
/// as a string or as a condition map (`types` / `import` / `default`, in that
/// order). Conditional trees beyond that shape name build output in every
/// layout worth guessing at, so they are skipped rather than walked.
fn declared_entry_specifiers(manifest: &serde_json::Value) -> Vec<String> {
    let mut specifiers = Vec::new();
    match manifest.get("exports") {
        Some(serde_json::Value::String(entry)) => specifiers.push(entry.clone()),
        Some(serde_json::Value::Object(map)) => match map.get(".") {
            Some(serde_json::Value::String(entry)) => specifiers.push(entry.clone()),
            Some(serde_json::Value::Object(conditions)) => {
                for condition in ["types", "import", "default"] {
                    if let Some(serde_json::Value::String(entry)) = conditions.get(condition) {
                        specifiers.push(entry.clone());
                    }
                }
            }
            _ => {}
        },
        _ => {}
    }
    for field in ["types", "typings", "main"] {
        if let Some(serde_json::Value::String(entry)) = manifest.get(field) {
            specifiers.push(entry.clone());
        }
    }
    specifiers
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
            self.walk(&export, "", declared, 0, &mut visited);
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

    fn walk(
        &mut self,
        export: &str,
        prefix: &str,
        declared: Declared,
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
                let (file, module, class) = (file.clone(), module.clone(), class.clone());
                self.walk_class(export, prefix, &file, &module, &class, depth, visited);
            }
            Declared::Object(file, module, object) => {
                let (file, module, object) = (file.clone(), module.clone(), object.clone());
                self.walk_object(export, prefix, &file, &module, &object, depth, visited);
            }
            // A bare exported function has no member chain under it: the
            // consumer calls the export itself, which `import_symbol` already
            // names.
            Declared::Function(..) => {}
        }
        visited.remove(&anchor);
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_class(
        &mut self,
        export: &str,
        prefix: &str,
        file: &Path,
        module: &Rc<Module>,
        class: &Class,
        depth: usize,
        visited: &mut HashSet<(PathBuf, BytePos)>,
    ) {
        for member in &class.body {
            match member {
                // An overload signature carries no body and no implementation
                // to anchor to; the implementation that follows it does.
                ClassMember::Method(method)
                    if method.function.body.is_some() && method.kind == MethodKind::Method =>
                {
                    let Some(name) = member_name(&method.key) else {
                        continue;
                    };
                    self.emit(export, prefix, &name, file, method.span);
                }
                ClassMember::ClassProp(property) => {
                    let Some(name) = member_name(&property.key) else {
                        continue;
                    };
                    // A field holding a function IS a callable member.
                    if matches!(
                        property.value.as_deref(),
                        Some(Expr::Arrow(_) | Expr::Fn(_))
                    ) {
                        self.emit(export, prefix, &name, file, property.span);
                        continue;
                    }
                    // Otherwise the field is a sub-resource when its declared
                    // type or its `new X(...)` initialiser names a class this
                    // can reach. The annotation is read first: the shape this
                    // exists for declares the type and assigns the instance on
                    // one line, and the two always agree there.
                    let declared = annotation_path(property.type_ann.as_deref())
                        .and_then(|path| self.resolve_type_path(file, module, &path))
                        .or_else(|| {
                            property
                                .value
                                .as_deref()
                                .and_then(|value| self.value_declaration(file, module, value))
                        });
                    let Some(declared) = declared else {
                        continue;
                    };
                    self.walk_member(export, prefix, &name, declared, depth, visited);
                }
                _ => {}
            }
        }
    }

    /// A resolved field or property: a function is the member itself, anything
    /// else is a sub-resource whose own members compose under this name.
    fn walk_member(
        &mut self,
        export: &str,
        prefix: &str,
        name: &str,
        declared: Declared,
        depth: usize,
        visited: &mut HashSet<(PathBuf, BytePos)>,
    ) {
        if let Some((file, span)) = declared.function_span() {
            self.emit(export, prefix, name, &file, span);
            return;
        }
        let nested = format!("{}{}.", prefix, name);
        self.walk(export, &nested, declared, depth + 1, visited);
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_object(
        &mut self,
        export: &str,
        prefix: &str,
        file: &Path,
        module: &Rc<Module>,
        object: &ObjectLit,
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
                    self.emit(export, prefix, &name, file, method.function.span);
                }
                Prop::KeyValue(entry) => {
                    let Some(name) = member_name(&entry.key) else {
                        continue;
                    };
                    if matches!(&*entry.value, Expr::Arrow(_) | Expr::Fn(_)) {
                        self.emit(export, prefix, &name, file, entry.value.span());
                        continue;
                    }
                    let Some(declared) = self.value_declaration(file, module, &entry.value) else {
                        continue;
                    };
                    self.walk_member(export, prefix, &name, declared, depth, visited);
                }
                // `{ payments }` is `{ payments: payments }`.
                Prop::Shorthand(ident) => {
                    let name = ident.sym.to_string();
                    let Some(declared) =
                        self.resolve_type_path(file, module, std::slice::from_ref(&name))
                    else {
                        continue;
                    };
                    self.walk_member(export, prefix, &name, declared, depth, visited);
                }
                _ => {}
            }
        }
    }

    fn emit(&mut self, export: &str, prefix: &str, name: &str, file: &Path, span: Span) {
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
        });
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

    /// `exports` is read narrowly: a bare string, the `"."` string, or the
    /// `"."` condition map. Anything else names build output in every layout
    /// worth guessing at.
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

        // A conditional tree this does not understand contributes nothing
        // rather than a guess.
        let nested = serde_json::json!({ "exports": { "./sub": "./src/sub.ts" } });
        assert!(declared_entry_specifiers(&nested).is_empty());
    }

    /// A repo that publishes no client resolves an entry whose exports reach
    /// no class or object literal, and contributes nothing.
    #[test]
    fn a_repo_with_no_entry_module_publishes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(scan(tmp.path(), tmp.path()).is_empty());
    }
}
