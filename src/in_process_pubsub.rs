//! Pub/sub rows whose call goes into this repo's own code and never leaves the
//! process (carrick#1513).
//!
//! The file-analyzer reads one file at a time. A service that wraps an
//! in-memory stream and exposes `publish(event)` is called from everywhere
//! else as `this.realtime.publish({ type: "order.placed", ... })`, and from the
//! caller's file alone that is indistinguishable from a broker publish: a
//! method named for the protocol, a topic-shaped string, a payload. So the
//! model reports each call as a pub/sub publisher. Nothing subscribes to those
//! topics anywhere, because the stream is read in-process (streamed out over an
//! HTTP endpoint, say, which is already its own contract), so every row lands
//! among the unmatched calls beside the real gaps.
//!
//! What the caller's file cannot say, the wrapper's can. The call graph already
//! resolves such a call to its same-repo definition (`crate::call_graph`), and
//! the definition's own body states where the value goes. This pass reads it.
//!
//! A row is WITHDRAWN when all of these hold:
//!
//! - The row's call site resolves, through the call graph's edges, to exactly
//!   one same-repo function. A call on a client the repo imports from a package
//!   (`client.publish("x", p)`) resolves to nothing here and is never touched.
//! - That function is proven in-process (below).
//! - Nothing in the same service sits on the other side of the topic. An
//!   in-process publisher with an in-process subscriber is a real contract
//!   (the same stance `crate::event_emitter` takes for `emit`/`on`), and the
//!   pair matches itself; only a side that can never be matched is withdrawn.
//!
//! A function is PROVEN in-process when its body reaches at least one sink and
//! every call in it is accounted for:
//!
//! - A call on an instance the class or module CONSTRUCTS itself (`new X()`
//!   as a field or module-scope initialiser, never reassigned) is a sink when
//!   `X` comes from a declared dependency that is not a transport, and is
//!   followed into `X`'s method when `X` is this repo's own class. A transport
//!   is a package framework detection lists as a messaging, socket or
//!   data-fetching client: the model's classification of the repo's own
//!   dependencies, so nothing here names a library. An empty array or object
//!   literal held the same way is a sink too: a listener list the code fills
//!   at run time. So is a call of a function imported from a non-transport
//!   dependency (an operator applied to the stream).
//! - A call resolved by a call-graph edge, or on the class's own method, is
//!   followed, to a bounded depth.
//! - A bare call of a callback handed out by such an instance
//!   (`this.listeners.forEach((fn) => fn(event))`) runs a registered listener
//!   in-process.
//! - A call with no arguments on a global (`Date.now()`,
//!   `new Date().toISOString()`) can carry nothing of the caller's.
//!
//! Anything else leaves the function NOT proven and its callers' rows stand:
//! an injected field (whatever it is, it was handed in, so it may be a broker
//! connection), a parameter, a global called with arguments (`fetch`,
//! `JSON.stringify` alike), a construction from a global (the runtime's own
//! globals include sockets and workers), a Node builtin module (the same
//! reason), and any import the workspace index cannot place. The pass says
//! nothing rather than guess, so a missed in-process wrapper keeps its rows,
//! as it did before this pass existed.
//!
//! Two known limits. The call graph keeps one edge per caller and callee, at
//! the first call site; a second call to the same wrapper from the same
//! function is joined to that edge by its callee text (`this.feed.publish`),
//! which the first site shares. And a transport package the detection step
//! did not list, constructed directly by the wrapper's class, would read as a
//! sink: that is a detection miss, and every other pub/sub gate in the
//! scanner shares it.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use swc_common::errors::{ColorConfig, Handler};
use swc_common::{GLOBALS, Globals, SourceMap, SourceMapper, Span, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};
use tracing::debug;

use crate::agents::file_analyzer_agent::FileAnalysisResult;
use crate::operation::PubsubRole;
use crate::parser::parse_file;
use crate::visitor::{FunctionDefinition, ImportSymbolExtractor, ImportedSymbol, SymbolKind};
use crate::workspace_resolver::{Resolution, WorkspaceIndex};

/// How many same-repo hops a wrapper may be followed through. Deep enough for
/// a facade over a service over a bus; a chain longer than this proves nothing.
const MAX_DEPTH: usize = 4;

/// One model pub/sub row as the engine's folds key it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Site {
    file: PathBuf,
    line: u32,
    topic: String,
    role: PubsubRole,
}

/// The model pub/sub rows this pass withdrew, read by every fold that turns a
/// row into an operation or a manifest anchor, so a withdrawn row leaves
/// neither.
#[derive(Debug, Default)]
pub struct InProcessPubsub {
    withdrawn: HashSet<Site>,
}

impl InProcessPubsub {
    /// Whether the row the file-analyzer reported at `file:line` for `topic`
    /// and `role` was withdrawn. `file` is the `file_results` key.
    pub fn is_withdrawn(&self, file: &Path, line: i32, topic: &str, role: PubsubRole) -> bool {
        let Ok(line) = u32::try_from(line) else {
            return false;
        };
        self.withdrawn.contains(&Site {
            file: normalize(file),
            line,
            topic: topic.to_string(),
            role,
        })
    }

    /// `file_results` without the withdrawn rows, for a reader that builds
    /// something from every pub/sub row (the payload type requests), so a
    /// withdrawn row asks the sidecar for nothing. Borrowed when nothing was
    /// withdrawn.
    pub fn retained<'r>(
        &self,
        file_results: &'r HashMap<String, FileAnalysisResult>,
    ) -> Cow<'r, HashMap<String, FileAnalysisResult>> {
        if self.withdrawn.is_empty() {
            return Cow::Borrowed(file_results);
        }
        let mut kept = file_results.clone();
        for (path, result) in kept.iter_mut() {
            result.pubsub_operations.retain(|op| {
                op.role.is_none_or(|role| {
                    !self.is_withdrawn(Path::new(path), op.line_number, &op.topic, role)
                })
            });
        }
        Cow::Owned(kept)
    }

    pub fn len(&self) -> usize {
        self.withdrawn.len()
    }

    pub fn is_empty(&self) -> bool {
        self.withdrawn.is_empty()
    }
}

/// Decide which of the model's pub/sub rows are calls into an in-process
/// wrapper with no counterpart in the service.
///
/// `definitions` is the service's function map with call edges resolved and
/// paths repo-relative (what `CloudRepoData::function_definitions` holds).
/// `transports` is every package framework detection classed as a messaging,
/// socket or data-fetching client. `other_sides` is every other pub/sub row
/// the service carries (the in-process event-bus pass's), so a counterpart
/// there keeps a row too.
pub fn classify(
    repo_root: &Path,
    file_results: &HashMap<String, FileAnalysisResult>,
    definitions: &HashMap<String, FunctionDefinition>,
    workspace: &WorkspaceIndex,
    transports: &[String],
    other_sides: &[(String, PubsubRole)],
) -> InProcessPubsub {
    let mut sites: Vec<Site> = Vec::new();
    for (path, result) in file_results {
        for op in &result.pubsub_operations {
            let (Some(role), Ok(line)) = (op.role, u32::try_from(op.line_number)) else {
                continue;
            };
            if line == 0 {
                continue;
            }
            sites.push(Site {
                file: normalize(Path::new(path)),
                line,
                topic: op.topic.clone(),
                role,
            });
        }
    }
    if sites.is_empty() {
        return InProcessPubsub::default();
    }
    sites.sort_by(|a, b| (&a.file, a.line, &a.topic).cmp(&(&b.file, b.line, &b.topic)));

    let sides: HashSet<(String, PubsubRole)> = sites
        .iter()
        .map(|site| (site.topic.clone(), site.role))
        .chain(other_sides.iter().cloned())
        .collect();

    let globals = Globals::new();
    let withdrawn = GLOBALS.set(&globals, || {
        let mut classifier = Classifier::new(repo_root, definitions, workspace, transports);
        let mut withdrawn = HashSet::new();
        for site in sites {
            if sides.contains(&(site.topic.clone(), opposite(site.role))) {
                continue;
            }
            // `file_results` is keyed by the path as walked (absolute during a
            // scan); definitions are repo-relative by now.
            let file = repo_relative(repo_root, &site.file);
            let Some(target) = classifier.site_target(&file, site.line) else {
                continue;
            };
            if classifier.verdict(&target, 0) == Verdict::InProcess {
                debug!(
                    topic = %site.topic,
                    file = %site.file.display(),
                    line = site.line,
                    wrapper = %target.1,
                    "pub/sub row withdrawn: its call goes into an in-process wrapper and nothing in the service is on the other side"
                );
                withdrawn.insert(site);
            }
        }
        withdrawn
    });
    InProcessPubsub { withdrawn }
}

fn opposite(role: PubsubRole) -> PubsubRole {
    match role {
        PubsubRole::Publisher => PubsubRole::Subscriber,
        PubsubRole::Subscriber => PubsubRole::Publisher,
    }
}

/// `path` relative to the repo root, whether it was given absolute (as walked,
/// or canonicalized) or already relative.
fn repo_relative(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_relative() {
        return path.to_path_buf();
    }
    if let Ok(stripped) = path.strip_prefix(repo_root) {
        return normalize(stripped);
    }
    repo_root
        .canonicalize()
        .ok()
        .and_then(|root| path.strip_prefix(root).ok().map(normalize))
        .unwrap_or_else(|| path.to_path_buf())
}

/// A path with any `./` and `..` components folded, so a `file_results` key,
/// a definition's path and a resolved import compare.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    InProcess,
    NotProven,
}

/// A same-repo function: its repo-relative file and definition name
/// (`Class.method` or a bare name).
type Target = (PathBuf, String);

struct ParsedModule {
    cm: Lrc<SourceMap>,
    module: Module,
    /// Every call in the module: its line, start offset and callee text.
    calls: Vec<CallText>,
}

/// One call's callee as written, whitespace removed (`this.feed.publish`).
struct CallText {
    line: u32,
    lo: u32,
    text: String,
}

impl ParsedModule {
    fn new(cm: Lrc<SourceMap>, module: Module) -> Self {
        struct Collector<'c> {
            cm: &'c SourceMap,
            calls: Vec<CallText>,
        }
        impl Visit for Collector<'_> {
            fn visit_call_expr(&mut self, call: &CallExpr) {
                if let Callee::Expr(callee) = &call.callee
                    && let Ok(text) = self.cm.span_to_snippet(callee.span())
                {
                    self.calls.push(CallText {
                        line: self.cm.lookup_char_pos(call.span.lo).line as u32,
                        lo: call.span.lo.0,
                        text: text.chars().filter(|c| !c.is_whitespace()).collect(),
                    });
                }
                call.visit_children_with(self);
            }
        }
        let mut collector = Collector {
            cm: &cm,
            calls: Vec::new(),
        };
        module.visit_with(&mut collector);
        let calls = collector.calls;
        Self { cm, module, calls }
    }

    /// The callee texts of the outermost call starting on `line` and of the
    /// chain links that start where it does.
    fn outer_callee_texts(&self, line: u32) -> HashSet<&str> {
        let on_line = || self.calls.iter().filter(move |call| call.line == line);
        let Some(first) = on_line().map(|call| call.lo).min() else {
            return HashSet::new();
        };
        on_line()
            .filter(|call| call.lo == first)
            .map(|call| call.text.as_str())
            .collect()
    }

    /// The callee texts of the calls on `line` whose last member is `member`.
    fn callee_texts_named<'s>(
        &'s self,
        line: u32,
        member: &'s str,
    ) -> impl Iterator<Item = &'s str> + 's {
        self.calls
            .iter()
            .filter(move |call| call.line == line && call.text.rsplit('.').next() == Some(member))
            .map(|call| call.text.as_str())
    }
}

struct Classifier<'a> {
    repo_root: &'a Path,
    /// (file, definition name) -> the definition.
    defs: HashMap<(PathBuf, String), &'a FunctionDefinition>,
    /// file -> the definitions in it, for finding a call site's owner.
    defs_by_file: HashMap<PathBuf, Vec<&'a FunctionDefinition>>,
    workspace: &'a WorkspaceIndex,
    transports: &'a [String],
    modules: HashMap<PathBuf, Option<Rc<ParsedModule>>>,
    memo: HashMap<Target, Verdict>,
    in_progress: HashSet<Target>,
}

impl<'a> Classifier<'a> {
    fn new(
        repo_root: &'a Path,
        definitions: &'a HashMap<String, FunctionDefinition>,
        workspace: &'a WorkspaceIndex,
        transports: &'a [String],
    ) -> Self {
        let mut defs = HashMap::new();
        let mut defs_by_file: HashMap<PathBuf, Vec<&FunctionDefinition>> = HashMap::new();
        for def in definitions.values() {
            let file = normalize(&def.file_path);
            defs.insert((file.clone(), def.name.clone()), def);
            defs_by_file.entry(file).or_default().push(def);
        }
        Self {
            repo_root,
            defs,
            defs_by_file,
            workspace,
            transports,
            modules: HashMap::new(),
            memo: HashMap::new(),
            in_progress: HashSet::new(),
        }
    }

    fn module(&mut self, file: &Path) -> Option<Rc<ParsedModule>> {
        if let Some(cached) = self.modules.get(file) {
            return cached.clone();
        }
        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
        let parsed = parse_file(&self.repo_root.join(file), &cm, &handler)
            .map(|module| Rc::new(ParsedModule::new(cm, module)));
        self.modules.insert(file.to_path_buf(), parsed.clone());
        parsed
    }

    /// The one same-repo function the call written at `file:line` resolves to.
    ///
    /// The row's call is the outermost call expression starting on its line
    /// (with the links of its chain: `bus.publish(x).catch(...)`), never a
    /// call nested in its arguments. Its owner is one of the definitions
    /// containing the line (a callback written on the same line is a
    /// definition of its own, which line numbers cannot tell apart from the
    /// call's owner), and the owners' call-graph edges say what each callee
    /// text resolves to. An edge is matched by the callee text written at its
    /// own site, which also joins a REPEAT call: the call graph keeps one edge
    /// per caller and callee, at the first site, and a second
    /// `this.feed.publish(...)` in the same function names the same callee.
    fn site_target(&mut self, file: &Path, line: u32) -> Option<Target> {
        let owners: Vec<&'a FunctionDefinition> = self
            .defs_by_file
            .get(file)?
            .iter()
            .filter(|def| def.line_number <= line && line <= def.end_line)
            .copied()
            .collect();
        if owners.iter().all(|def| def.calls.is_empty()) {
            return None;
        }
        let parsed = self.module(file)?;
        let outer = parsed.outer_callee_texts(line);
        if outer.is_empty() {
            return None;
        }
        let targets: HashSet<Target> = owners
            .iter()
            .flat_map(|def| def.calls.iter())
            .filter(|edge| {
                let member = edge.name.rsplit('.').next().unwrap_or(&edge.name);
                parsed
                    .callee_texts_named(edge.call_site_line, member)
                    .any(|text| outer.contains(text))
            })
            .map(|edge| (normalize(Path::new(&edge.file_path)), edge.name.clone()))
            .collect();
        single(targets)
    }

    fn verdict(&mut self, target: &Target, depth: usize) -> Verdict {
        if let Some(known) = self.memo.get(target) {
            return *known;
        }
        if depth > MAX_DEPTH || !self.in_progress.insert(target.clone()) {
            return Verdict::NotProven;
        }
        let verdict = self.evaluate(target, depth);
        self.in_progress.remove(target);
        self.memo.insert(target.clone(), verdict);
        verdict
    }

    fn evaluate(&mut self, target: &Target, depth: usize) -> Verdict {
        let (file, name) = target;
        let Some(def) = self.defs.get(target).copied() else {
            return Verdict::NotProven;
        };
        let Some(parsed) = self.module(file) else {
            return Verdict::NotProven;
        };
        let imports = import_table(&parsed.module);
        let Some(located) = locate(&parsed.module, name, def.line_number, &parsed.cm) else {
            return Verdict::NotProven;
        };

        let scope = Scope::build(&parsed.module, located.class, &imports);
        let edges: HashMap<u32, HashSet<Target>> =
            def.calls.iter().fold(HashMap::new(), |mut acc, edge| {
                acc.entry(edge.call_site_line)
                    .or_default()
                    .insert((normalize(Path::new(&edge.file_path)), edge.name.clone()));
                acc
            });

        let mut walker = BodyWalker {
            classifier: &*self,
            file,
            cm: &parsed.cm,
            scope: &scope,
            class_name: located.class_name.as_deref(),
            edges: &edges,
            params: located.params.clone(),
            locals: located.locals.clone(),
            elements: Vec::new(),
            this_rebound: 0,
            follows: Vec::new(),
            sinks: 0,
            unproven: false,
        };
        located.body.visit(&mut walker);
        let BodyWalker {
            follows,
            sinks,
            unproven,
            ..
        } = walker;
        if unproven {
            return Verdict::NotProven;
        }
        let mut reached = sinks;
        for follow in follows {
            if self.verdict(&follow, depth + 1) != Verdict::InProcess {
                return Verdict::NotProven;
            }
            reached += 1;
        }
        if reached == 0 {
            return Verdict::NotProven;
        }
        Verdict::InProcess
    }

    /// Whether `specifier`, imported by `file`, names a package detection
    /// classed as a transport. Matched as every other detection gate matches
    /// a specifier: the entry itself or a path under it.
    fn is_transport_import(&self, file: &Path, specifier: &str) -> bool {
        let named = |candidate: &str| {
            self.transports
                .iter()
                .any(|entry| candidate == entry || candidate.starts_with(&format!("{entry}/")))
        };
        if named(specifier) {
            return true;
        }
        match self.workspace.resolve(file, specifier) {
            Resolution::External { package, .. } => named(&package),
            _ => false,
        }
    }

    /// What an imported binding is, for a call on it.
    fn import_origin(&self, file: &Path, import: &ImportedSymbol) -> ImportOrigin {
        if self.is_transport_import(file, &import.source) {
            return ImportOrigin::Unproven;
        }
        match self.workspace.resolve(file, &import.source) {
            Resolution::External { .. } => ImportOrigin::Dependency,
            Resolution::Internal(path) if import.kind == SymbolKind::Named => {
                ImportOrigin::Repo(normalize(&path), import.imported_name.clone())
            }
            _ => ImportOrigin::Unproven,
        }
    }
}

enum ImportOrigin {
    /// A declared dependency detection did not class as a transport.
    Dependency,
    /// A named export of this repo's own module.
    Repo(PathBuf, String),
    Unproven,
}

fn single(targets: HashSet<Target>) -> Option<Target> {
    if targets.len() == 1 {
        targets.into_iter().next()
    } else {
        None
    }
}

/// The ESM and module-scope `require` bindings of a module, the table call
/// resolution reads.
fn import_table(module: &Module) -> HashMap<String, ImportedSymbol> {
    let mut extractor = ImportSymbolExtractor::new();
    module.visit_with(&mut extractor);
    crate::call_graph::call_resolution_imports(module, extractor.imported_symbols).0
}

/// The function a definition names, found in its module.
struct Located<'m> {
    body: FunctionBody<'m>,
    class: Option<&'m Class>,
    class_name: Option<String>,
    params: HashSet<String>,
    locals: HashSet<String>,
}

#[derive(Clone, Copy)]
enum FunctionBody<'m> {
    Block(&'m BlockStmt),
    Expr(&'m Expr),
}

impl FunctionBody<'_> {
    fn visit(self, visitor: &mut impl Visit) {
        match self {
            FunctionBody::Block(block) => block.visit_with(visitor),
            FunctionBody::Expr(expr) => expr.visit_with(visitor),
        }
    }
}

fn locate<'m>(module: &'m Module, name: &str, line: u32, cm: &SourceMap) -> Option<Located<'m>> {
    let line_of = |span: Span| cm.lookup_char_pos(span.lo).line as u32;
    let mut found: Vec<Located<'m>> = Vec::new();
    match name.split_once('.') {
        Some((class_name, member)) => {
            let member = member.strip_prefix("static.").unwrap_or(member);
            for (ident, class) in module_classes(module) {
                if ident != class_name {
                    continue;
                }
                for item in &class.body {
                    let callable = match item {
                        ClassMember::Method(method)
                            if prop_name(&method.key).as_deref() == Some(member) =>
                        {
                            Some(Callable::Function(&method.function))
                        }
                        ClassMember::PrivateMethod(method)
                            if format!("#{}", method.key.name) == member =>
                        {
                            Some(Callable::Function(&method.function))
                        }
                        ClassMember::ClassProp(prop)
                            if prop_name(&prop.key).as_deref() == Some(member) =>
                        {
                            prop.value.as_deref().and_then(callable_expr)
                        }
                        _ => None,
                    };
                    if let Some(callable) = callable
                        && let Some(mut located) = callable.located()
                    {
                        located.class = Some(class);
                        located.class_name = Some(class_name.to_string());
                        if callable.span().is_some_and(|span| line_of(span) == line) {
                            return Some(located);
                        }
                        found.push(located);
                    }
                }
            }
        }
        None => {
            for item in &module.body {
                let decl = match item {
                    ModuleItem::Stmt(Stmt::Decl(decl)) => decl,
                    ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => &export.decl,
                    _ => continue,
                };
                let callables: Vec<Callable> = match decl {
                    Decl::Fn(fn_decl) if fn_decl.ident.sym == *name => {
                        vec![Callable::Function(&fn_decl.function)]
                    }
                    Decl::Var(var) => var
                        .decls
                        .iter()
                        .filter(|d| matches!(&d.name, Pat::Ident(id) if id.id.sym == *name))
                        .filter_map(|d| d.init.as_deref().and_then(callable_expr))
                        .collect(),
                    _ => Vec::new(),
                };
                for callable in callables {
                    if let Some(located) = callable.located() {
                        if callable.span().is_some_and(|span| line_of(span) == line) {
                            return Some(located);
                        }
                        found.push(located);
                    }
                }
            }
        }
    }
    if found.len() == 1 { found.pop() } else { None }
}

#[derive(Clone, Copy)]
enum Callable<'m> {
    Function(&'m Function),
    Arrow(&'m ArrowExpr),
}

impl<'m> Callable<'m> {
    fn span(self) -> Option<Span> {
        match self {
            Callable::Function(function) => Some(function.span),
            Callable::Arrow(arrow) => Some(arrow.span),
        }
    }

    fn located(self) -> Option<Located<'m>> {
        let (body, params): (FunctionBody<'m>, Vec<&Pat>) = match self {
            Callable::Function(function) => (
                FunctionBody::Block(function.body.as_ref()?),
                function.params.iter().map(|p| &p.pat).collect(),
            ),
            Callable::Arrow(arrow) => (
                match &*arrow.body {
                    BlockStmtOrExpr::BlockStmt(block) => FunctionBody::Block(block),
                    BlockStmtOrExpr::Expr(expr) => FunctionBody::Expr(expr),
                },
                arrow.params.iter().collect(),
            ),
        };
        let mut param_names = HashSet::new();
        for pat in params {
            collect_pat_names(pat, &mut param_names);
        }
        let mut declared = DeclaredNames::default();
        body.visit(&mut declared);
        Some(Located {
            body,
            class: None,
            class_name: None,
            params: param_names,
            locals: declared.names,
        })
    }
}

fn callable_expr(expr: &Expr) -> Option<Callable<'_>> {
    match unwrap(expr) {
        Expr::Arrow(arrow) => Some(Callable::Arrow(arrow)),
        Expr::Fn(fn_expr) => Some(Callable::Function(&fn_expr.function)),
        _ => None,
    }
}

/// Every named class the module declares, exported or not.
fn module_classes(module: &Module) -> Vec<(String, &Class)> {
    let mut classes = Vec::new();
    for item in &module.body {
        match item {
            ModuleItem::Stmt(Stmt::Decl(Decl::Class(class)))
            | ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(ExportDecl {
                decl: Decl::Class(class),
                ..
            })) => classes.push((class.ident.sym.to_string(), &*class.class)),
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(ExportDefaultDecl {
                decl:
                    DefaultDecl::Class(ClassExpr {
                        ident: Some(ident),
                        class,
                    }),
                ..
            })) => classes.push((ident.sym.to_string(), &**class)),
            _ => {}
        }
    }
    classes
}

fn prop_name(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(s) => Some(s.value.to_string()),
        _ => None,
    }
}

fn unwrap(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(inner) => unwrap(&inner.expr),
        Expr::TsAs(inner) => unwrap(&inner.expr),
        Expr::TsNonNull(inner) => unwrap(&inner.expr),
        Expr::TsSatisfies(inner) => unwrap(&inner.expr),
        Expr::TsConstAssertion(inner) => unwrap(&inner.expr),
        Expr::TsTypeAssertion(inner) => unwrap(&inner.expr),
        other => other,
    }
}

fn collect_pat_names(pat: &Pat, names: &mut HashSet<String>) {
    struct Names<'n>(&'n mut HashSet<String>);
    impl Visit for Names<'_> {
        fn visit_binding_ident(&mut self, ident: &BindingIdent) {
            self.0.insert(ident.id.sym.to_string());
        }
        // A default value is an expression, not a binding.
        fn visit_expr(&mut self, _: &Expr) {}
    }
    pat.visit_with(&mut Names(names));
}

/// Every name bound anywhere inside a function body: declarations, nested
/// functions' parameters, loop and catch bindings.
#[derive(Default)]
struct DeclaredNames {
    names: HashSet<String>,
}

impl Visit for DeclaredNames {
    fn visit_pat(&mut self, pat: &Pat) {
        collect_pat_names(pat, &mut self.names);
        pat.visit_children_with(self);
    }
    fn visit_fn_decl(&mut self, decl: &FnDecl) {
        self.names.insert(decl.ident.sym.to_string());
        decl.visit_children_with(self);
    }
    fn visit_class_decl(&mut self, decl: &ClassDecl) {
        self.names.insert(decl.ident.sym.to_string());
        decl.visit_children_with(self);
    }
}

/// How a held value came to be, read off its one initialiser.
#[derive(Debug, Clone)]
enum Origin {
    /// `new X(...)` with `X` a plain identifier.
    Constructed(String),
    /// An empty array or object literal.
    EmptyContainer,
    /// Anything else, or a value that is assigned again after it is declared.
    Unknown,
}

fn origin_of(init: Option<&Expr>) -> Origin {
    let Some(init) = init else {
        return Origin::Unknown;
    };
    match unwrap(init) {
        Expr::New(new) => match unwrap(&new.callee) {
            Expr::Ident(ident) => Origin::Constructed(ident.sym.to_string()),
            _ => Origin::Unknown,
        },
        Expr::Array(array) if array.elems.is_empty() => Origin::EmptyContainer,
        Expr::Object(object) if object.props.is_empty() => Origin::EmptyContainer,
        _ => Origin::Unknown,
    }
}

/// What the module and the enclosing class state about the names a body uses.
struct Scope<'a> {
    imports: &'a HashMap<String, ImportedSymbol>,
    /// Module-scope value bindings and how each was made.
    values: HashMap<String, Origin>,
    /// Module-scope function declarations, callable by name.
    functions: HashSet<String>,
    /// Classes the module declares.
    classes: HashSet<String>,
    /// The enclosing class's own fields. A constructor parameter property is
    /// an injected value and reads `Unknown`, as does any field the class
    /// assigns after declaring it.
    fields: HashMap<String, Origin>,
    /// The class's own method names.
    methods: HashSet<String>,
    /// The enclosing class's superclass, when it is a plain identifier.
    super_class: Option<String>,
}

impl<'a> Scope<'a> {
    fn build(
        module: &Module,
        class: Option<&Class>,
        imports: &'a HashMap<String, ImportedSymbol>,
    ) -> Self {
        let mut values = HashMap::new();
        let mut functions = HashSet::new();
        let mut classes = HashSet::new();
        for item in &module.body {
            let decl = match item {
                ModuleItem::Stmt(Stmt::Decl(decl)) => decl,
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => &export.decl,
                _ => continue,
            };
            match decl {
                Decl::Fn(fn_decl) => {
                    functions.insert(fn_decl.ident.sym.to_string());
                }
                Decl::Class(class_decl) => {
                    classes.insert(class_decl.ident.sym.to_string());
                }
                Decl::Var(var) => {
                    for declarator in &var.decls {
                        if let Pat::Ident(ident) = &declarator.name {
                            let origin = if var.kind == VarDeclKind::Const {
                                origin_of(declarator.init.as_deref())
                            } else {
                                Origin::Unknown
                            };
                            values.insert(ident.id.sym.to_string(), origin);
                        }
                    }
                }
                _ => {}
            }
        }

        let mut fields = HashMap::new();
        let mut methods = HashSet::new();
        let mut super_class = None;
        if let Some(class) = class {
            if let Some(parent) = class.super_class.as_deref()
                && let Expr::Ident(ident) = unwrap(parent)
            {
                super_class = Some(ident.sym.to_string());
            }
            for member in &class.body {
                match member {
                    ClassMember::ClassProp(prop) if !prop.is_static => {
                        if let Some(name) = prop_name(&prop.key) {
                            let origin = match prop.value.as_deref().and_then(callable_expr) {
                                Some(_) => {
                                    methods.insert(name.clone());
                                    continue;
                                }
                                None => origin_of(prop.value.as_deref()),
                            };
                            fields.insert(name, origin);
                        }
                    }
                    ClassMember::PrivateProp(prop) if !prop.is_static => {
                        fields.insert(
                            format!("#{}", prop.key.name),
                            origin_of(prop.value.as_deref()),
                        );
                    }
                    ClassMember::Method(method) if !method.is_static => {
                        if let Some(name) = prop_name(&method.key) {
                            methods.insert(name);
                        }
                    }
                    ClassMember::PrivateMethod(method) if !method.is_static => {
                        methods.insert(format!("#{}", method.key.name));
                    }
                    ClassMember::Constructor(constructor) => {
                        for param in &constructor.params {
                            if let ParamOrTsParamProp::TsParamProp(prop) = param {
                                let name = match &prop.param {
                                    TsParamPropParam::Ident(ident) => {
                                        Some(ident.id.sym.to_string())
                                    }
                                    TsParamPropParam::Assign(assign) => match &*assign.left {
                                        Pat::Ident(ident) => Some(ident.id.sym.to_string()),
                                        _ => None,
                                    },
                                };
                                if let Some(name) = name {
                                    fields.insert(name, Origin::Unknown);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            // A field assigned anywhere in the class holds whatever was
            // assigned last, which its declaration cannot say.
            let mut assigned = AssignedFields::default();
            class.body.visit_with(&mut assigned);
            for name in assigned.names {
                fields.insert(name, Origin::Unknown);
            }
        }

        Scope {
            imports,
            values,
            functions,
            classes,
            fields,
            methods,
            super_class,
        }
    }
}

/// `this.x = ...` and `this.#x = ...` anywhere in a class body.
#[derive(Default)]
struct AssignedFields {
    names: HashSet<String>,
}

impl Visit for AssignedFields {
    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        if let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &assign.left
            && matches!(&*member.obj, Expr::This(_))
            && let Some(name) = member_prop_name(&member.prop)
        {
            self.names.insert(name);
        }
        assign.visit_children_with(self);
    }
}

fn member_prop_name(prop: &MemberProp) -> Option<String> {
    match prop {
        MemberProp::Ident(ident) => Some(ident.sym.to_string()),
        MemberProp::PrivateName(private) => Some(format!("#{}", private.name)),
        MemberProp::Computed(computed) => match &*computed.expr {
            Expr::Lit(Lit::Str(s)) => Some(s.value.to_string()),
            _ => None,
        },
    }
}

/// Where a call's receiver chain starts.
enum Root {
    /// `this.field...` — the field, and whether the call is directly on it
    /// (`this.field.method(...)`, carrying the method).
    ThisField {
        field: String,
        method: Option<String>,
    },
    /// `this.method(...)`.
    ThisMethod(String),
    /// `name(...)` — a bare call.
    Call(String),
    /// `name.member...(...)` — carrying the method when it is one hop.
    Member {
        name: String,
        method: Option<String>,
    },
    /// `new X(...).member(...)` or a literal: a fresh value, and whether it
    /// was constructed from a global.
    Fresh {
        global_ctor: Option<String>,
    },
    Other,
}

fn call_root(callee: &Expr) -> Root {
    match unwrap(callee) {
        Expr::Ident(ident) => Root::Call(ident.sym.to_string()),
        Expr::Member(member) => {
            let method = member_prop_name(&member.prop);
            match unwrap(&member.obj) {
                Expr::This(_) => match method {
                    Some(name) => Root::ThisMethod(name),
                    None => Root::Other,
                },
                Expr::Member(inner) if matches!(unwrap(&inner.obj), Expr::This(_)) => {
                    match member_prop_name(&inner.prop) {
                        Some(field) => Root::ThisField { field, method },
                        None => Root::Other,
                    }
                }
                Expr::Ident(ident) => Root::Member {
                    name: ident.sym.to_string(),
                    method,
                },
                obj => deeper(obj),
            }
        }
        _ => Root::Other,
    }
}

/// The root of a chain more than one hop long: the method is no longer the
/// root's own, so it is dropped.
fn deeper(expr: &Expr) -> Root {
    match unwrap(expr) {
        Expr::This(_) => Root::Other,
        Expr::Ident(ident) => Root::Member {
            name: ident.sym.to_string(),
            method: None,
        },
        Expr::Member(member) => match unwrap(&member.obj) {
            Expr::This(_) => match member_prop_name(&member.prop) {
                Some(field) => Root::ThisField {
                    field,
                    method: None,
                },
                None => Root::Other,
            },
            obj => deeper(obj),
        },
        Expr::Call(call) => match &call.callee {
            Callee::Expr(callee) => match call_root(callee) {
                Root::ThisField { field, .. } => Root::ThisField {
                    field,
                    method: None,
                },
                Root::Member { name, .. } => Root::Member { name, method: None },
                Root::Fresh { global_ctor } => Root::Fresh { global_ctor },
                _ => Root::Other,
            },
            _ => Root::Other,
        },
        Expr::New(new) => match unwrap(&new.callee) {
            Expr::Ident(ident) => Root::Fresh {
                global_ctor: Some(ident.sym.to_string()),
            },
            _ => Root::Other,
        },
        Expr::Lit(_) | Expr::Array(_) | Expr::Object(_) | Expr::Tpl(_) => {
            Root::Fresh { global_ctor: None }
        }
        _ => Root::Other,
    }
}

/// What one call contributes to its function's verdict.
enum Need {
    /// A sink in this process.
    Sink,
    /// Nothing either way.
    Nothing,
    /// Proven only if this same-repo function is.
    Follow(Target),
    NotProven,
}

struct BodyWalker<'w, 'a> {
    classifier: &'w Classifier<'a>,
    file: &'w Path,
    cm: &'w SourceMap,
    scope: &'w Scope<'w>,
    class_name: Option<&'w str>,
    /// Call-graph edges out of this function, by call-site line.
    edges: &'w HashMap<u32, HashSet<Target>>,
    params: HashSet<String>,
    locals: HashSet<String>,
    /// Bindings that hold what an in-process instance hands out: a listener
    /// taken from a listener list.
    elements: Vec<HashSet<String>>,
    /// Depth of non-arrow functions entered: `this` is not the class there.
    this_rebound: usize,
    follows: Vec<Target>,
    sinks: usize,
    unproven: bool,
}

impl BodyWalker<'_, '_> {
    fn is_element(&self, name: &str) -> bool {
        self.elements.iter().any(|set| set.contains(name))
    }

    /// Whether `expr` is a listener list this class or module holds: a field
    /// or module-scope `const` that starts as an empty literal.
    fn holds_empty_container(&self, expr: &Expr) -> bool {
        match unwrap(expr) {
            Expr::Member(member) if matches!(unwrap(&member.obj), Expr::This(_)) => {
                self.this_rebound == 0
                    && member_prop_name(&member.prop).is_some_and(|field| {
                        matches!(self.scope.fields.get(&field), Some(Origin::EmptyContainer))
                    })
            }
            Expr::Ident(ident) => {
                let name = ident.sym.to_string();
                !self.locals.contains(&name)
                    && !self.params.contains(&name)
                    && matches!(self.scope.values.get(&name), Some(Origin::EmptyContainer))
            }
            _ => false,
        }
    }

    fn edge_at(&self, span: Span) -> Option<Target> {
        let line = self.cm.lookup_char_pos(span.lo).line as u32;
        self.edges.get(&line).cloned().and_then(single)
    }

    fn record(&mut self, need: Need) {
        match need {
            Need::Sink => self.sinks += 1,
            Need::Nothing => {}
            Need::Follow(target) => self.follows.push(target),
            Need::NotProven => self.unproven = true,
        }
    }

    /// What a call on a held value contributes, from how the value was made.
    fn held(&self, origin: &Origin, method: Option<&str>, span: Span) -> Need {
        match origin {
            Origin::EmptyContainer => Need::Sink,
            Origin::Constructed(ctor) => {
                if let Some(import) = self.scope.imports.get(ctor) {
                    return match self.classifier.import_origin(self.file, import) {
                        ImportOrigin::Dependency => Need::Sink,
                        ImportOrigin::Repo(file, class) => match method {
                            Some(method) => Need::Follow((file, format!("{class}.{method}"))),
                            None => Need::NotProven,
                        },
                        ImportOrigin::Unproven => Need::NotProven,
                    };
                }
                if self.scope.classes.contains(ctor)
                    && let Some(method) = method
                {
                    return Need::Follow((self.file.to_path_buf(), format!("{ctor}.{method}")));
                }
                // A global constructor: the runtime's own include sockets and
                // workers, so it proves nothing.
                Need::NotProven
            }
            Origin::Unknown => match self.edge_at(span) {
                Some(target) => Need::Follow(target),
                None => Need::NotProven,
            },
        }
    }

    fn classify(&self, callee: &Expr, has_args: bool, span: Span) -> Need {
        match call_root(callee) {
            Root::ThisField { field, method } => {
                if self.this_rebound > 0 {
                    return Need::NotProven;
                }
                match self.scope.fields.get(&field) {
                    Some(origin) => self.held(origin, method.as_deref(), span),
                    None => Need::NotProven,
                }
            }
            Root::ThisMethod(method) => {
                if self.this_rebound > 0 {
                    return Need::NotProven;
                }
                let Some(class) = self.class_name else {
                    return Need::NotProven;
                };
                if self.scope.methods.contains(&method) {
                    return Need::Follow((self.file.to_path_buf(), format!("{class}.{method}")));
                }
                // A method the class inherits: what it is depends on the class
                // it extends.
                match self
                    .scope
                    .super_class
                    .as_ref()
                    .and_then(|parent| self.scope.imports.get(parent))
                {
                    Some(import) => match self.classifier.import_origin(self.file, import) {
                        ImportOrigin::Dependency => Need::Sink,
                        _ => Need::NotProven,
                    },
                    None => Need::NotProven,
                }
            }
            Root::Call(name) => {
                if self.is_element(&name) {
                    return Need::Sink;
                }
                if self.locals.contains(&name) || self.params.contains(&name) {
                    return Need::NotProven;
                }
                if let Some(target) = self.edge_at(span) {
                    return Need::Follow(target);
                }
                if let Some(import) = self.scope.imports.get(&name) {
                    return match self.classifier.import_origin(self.file, import) {
                        ImportOrigin::Dependency => Need::Sink,
                        ImportOrigin::Repo(file, export) => Need::Follow((file, export)),
                        ImportOrigin::Unproven => Need::NotProven,
                    };
                }
                if self.scope.functions.contains(&name) {
                    return Need::Follow((self.file.to_path_buf(), name));
                }
                if self.scope.values.contains_key(&name) {
                    return Need::NotProven;
                }
                self.global(has_args)
            }
            Root::Member { name, method } => {
                if self.is_element(&name)
                    || self.locals.contains(&name)
                    || self.params.contains(&name)
                {
                    return Need::NotProven;
                }
                if let Some(origin) = self.scope.values.get(&name) {
                    return self.held(origin, method.as_deref(), span);
                }
                if let Some(target) = self.edge_at(span) {
                    return Need::Follow(target);
                }
                if let Some(import) = self.scope.imports.get(&name) {
                    return match self.classifier.import_origin(self.file, import) {
                        ImportOrigin::Dependency => Need::Sink,
                        _ => Need::NotProven,
                    };
                }
                if self.scope.functions.contains(&name) || self.scope.classes.contains(&name) {
                    return Need::NotProven;
                }
                self.global(has_args)
            }
            Root::Fresh { global_ctor } => match global_ctor {
                Some(ctor) if self.scope.imports.contains_key(&ctor) => Need::NotProven,
                _ if has_args => Need::NotProven,
                _ => Need::Nothing,
            },
            Root::Other => Need::NotProven,
        }
    }

    /// A call on a name nothing in the file binds. With no arguments it can
    /// carry nothing of the caller's (`Date.now()`, `crypto.randomUUID()`);
    /// with arguments it is as likely `fetch` as `JSON.stringify`.
    fn global(&self, has_args: bool) -> Need {
        if has_args {
            Need::NotProven
        } else {
            Need::Nothing
        }
    }

    /// Visit a call's arguments, treating a function passed to an in-process
    /// sink as a listener callback: whatever it calls by its parameter names
    /// is a listener running here.
    fn visit_args(&mut self, args: &[ExprOrSpread], sink: bool) {
        for arg in args {
            let params: Option<Vec<&Pat>> = if sink {
                match unwrap(&arg.expr) {
                    Expr::Arrow(arrow) => Some(arrow.params.iter().collect()),
                    Expr::Fn(fn_expr) => {
                        Some(fn_expr.function.params.iter().map(|p| &p.pat).collect())
                    }
                    _ => None,
                }
            } else {
                None
            };
            match params {
                Some(params) => {
                    let mut names = HashSet::new();
                    for pat in params {
                        collect_pat_names(pat, &mut names);
                    }
                    self.elements.push(names);
                    arg.visit_with(self);
                    self.elements.pop();
                }
                None => arg.visit_with(self),
            }
        }
    }

    fn visit_call(&mut self, callee: &Expr, args: &[ExprOrSpread], span: Span) {
        let need = self.classify(callee, !args.is_empty(), span);
        let sink = matches!(need, Need::Sink);
        self.record(need);
        callee.visit_with(self);
        self.visit_args(args, sink);
    }
}

impl Visit for BodyWalker<'_, '_> {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        match &call.callee {
            Callee::Expr(callee) => self.visit_call(callee, &call.args, call.span),
            // `super(...)` and `import(...)`: nothing this pass can place.
            _ => {
                self.unproven = true;
                call.visit_children_with(self);
            }
        }
    }

    fn visit_opt_chain_expr(&mut self, chain: &OptChainExpr) {
        match &*chain.base {
            OptChainBase::Call(call) => self.visit_call(&call.callee, &call.args, chain.span),
            OptChainBase::Member(_) => chain.visit_children_with(self),
        }
    }

    fn visit_new_expr(&mut self, new: &NewExpr) {
        let has_args = new.args.as_ref().is_some_and(|args| !args.is_empty());
        let need = match unwrap(&new.callee) {
            Expr::Ident(ident) => {
                let name = ident.sym.to_string();
                match self.scope.imports.get(&name) {
                    Some(import) => match self.classifier.import_origin(self.file, import) {
                        ImportOrigin::Dependency => Need::Nothing,
                        _ => Need::NotProven,
                    },
                    None if self.scope.classes.contains(&name)
                        || self.locals.contains(&name)
                        || self.params.contains(&name) =>
                    {
                        Need::NotProven
                    }
                    // `new Date()`: a global constructed with nothing to
                    // connect to.
                    None => self.global(has_args),
                }
            }
            _ => Need::NotProven,
        };
        self.record(need);
        new.visit_children_with(self);
    }

    fn visit_tagged_tpl(&mut self, tagged: &TaggedTpl) {
        // A tag is a call with the template as its arguments.
        self.unproven = true;
        tagged.visit_children_with(self);
    }

    fn visit_for_of_stmt(&mut self, stmt: &ForOfStmt) {
        let over_sink = self.holds_empty_container(&stmt.right);
        stmt.right.visit_with(self);
        let mut names = HashSet::new();
        if over_sink {
            match &stmt.left {
                ForHead::VarDecl(var) => {
                    for declarator in &var.decls {
                        collect_pat_names(&declarator.name, &mut names);
                    }
                }
                ForHead::Pat(pat) => collect_pat_names(pat, &mut names),
                ForHead::UsingDecl(_) => {}
            }
        }
        self.elements.push(names);
        stmt.body.visit_with(self);
        self.elements.pop();
    }

    fn visit_function(&mut self, function: &Function) {
        self.this_rebound += 1;
        function.visit_children_with(self);
        self.this_rebound -= 1;
    }

    fn visit_class(&mut self, class: &Class) {
        self.this_rebound += 1;
        class.visit_children_with(self);
        self.this_rebound -= 1;
    }
}
