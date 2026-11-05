//! Whether a changed source file only adds code that reaches no contract
//! (carrick#3364), so `check --changed` can answer it from the index without a
//! re-scan.
//!
//! The scan finds a route, a call, an event or a topic at a call, a `new`, a
//! tagged template, a decorator, a JSX element, a GraphQL text or a file path.
//! New code that holds none of those, that no existing file can reach, and
//! whose every call goes to code that holds none of them either, gives the scan
//! nothing to state: re-reading the repo would repeat the index. Everything
//! here fails closed. A shape it does not recognise is a re-scan.
//!
//! What counts as new code, per file:
//!
//! - **Added**: absent at the index's commit. Every item is new.
//! - **Extended**: the items at the index's commit are the leading items on
//!   disk, read the same ([`super::reads_same::same_reading`] over the copy
//!   cut where the first new item starts), and the items after them are new.
//!   An insertion in the middle, or any edit to an existing item (a fix inside
//!   a helper), is a re-scan: its inferred types can reach a route or a call in
//!   another file, and only a type-reach record could say they do not.
//!
//! Each new item declares new names only (a function, class, variable, type
//! alias, interface or enum, exported or not; in an added file also an import
//! with bindings and a default-exported function or class). Every call, `new`
//! and tagged template in it has a callee root that is a literal, a parameter
//! or local typed as a primitive, a local whose initialiser's root passes, a
//! function or class of the same file whose body passes, or a binding imported
//! from a relative file whose whole module passes in turn; so does every value
//! handed to the call. A free identifier (a global), a package import, `this`
//! and `super` do not pass.
//!
//! The rules that read the index (no row in an extended or reached file, no
//! file-based route for an added one) and the workspace (no other file names
//! the added file or a new export) are applied by the caller,
//! [`super::changed`], with what [`NewCode`] lists.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use swc_common::comments::{CommentKind, SingleThreadedComments};
use swc_common::{BytePos, FileName, SourceMap, Spanned, errors::Handler, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use super::reads_same::{LineMap, is_directive, same_reading};
use crate::binding_scope::{BindingKey, ident_key};

/// How a changed file differs from its copy at the index's commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Shape {
    Added,
    /// The items at the commit lead the file; the line map places them.
    Extended(LineMap),
}

/// New code that passed every rule this module reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NewCode {
    pub shape: Shape,
    /// The names the new items export.
    pub exported: BTreeSet<String>,
    /// Repo-relative files the new code calls into through relative imports,
    /// at any depth: each must hold no indexed row and no possible call.
    pub reaches: BTreeSet<String>,
}

/// Why new code takes the re-scan. One per refusal, so a test can name the
/// rule that refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refusal {
    NotSource,
    Unreadable,
    NotParsed,
    /// Neither added nor a run of items appended after the existing ones.
    NotAppended,
    /// The file is a script, so its top-level names are global.
    NotAModule,
    /// An item that is not a declaration of new names.
    Item(&'static str),
    /// A new name the existing code already declares or reads.
    NameInUse(String),
    /// A comment the scan reads, in the new code.
    Directive,
    /// A string the scan's GraphQL readers read.
    GraphqlText,
    /// A callee root that is not known to reach no contract.
    Callee(String),
    /// A form that states a contract without a call: JSX, a decorator, a
    /// dynamic import, a directive prologue.
    Form(&'static str),
    /// A relative import whose module does not pass.
    Reached(String),
}

/// Read file `file` of `repo` against its copy at `commit`.
pub(super) fn read(repo: &Path, commit: &str, file: &str) -> Result<NewCode, Refusal> {
    if !crate::file_finder::is_scanned_source(&repo.join(file), repo) || is_declaration_file(file) {
        return Err(Refusal::NotSource);
    }
    let now = std::fs::read_to_string(repo.join(file)).map_err(|_| Refusal::Unreadable)?;
    let repo_text = repo.to_string_lossy();
    let before = match crate::git_state::file_at_commit(&repo_text, commit, file) {
        Some(before) => Some(before),
        // Absent at the commit only when the clone holds the commit and its
        // tree does not list the path; anything else cannot be told apart
        // from a read that failed.
        None if crate::git_state::has_commit(&repo_text, commit)
            && crate::git_state::paths_at_commit(&repo_text, commit, &[file.to_string()])
                .is_ok_and(|held| held.is_empty()) =>
        {
            None
        }
        None => return Err(Refusal::Unreadable),
    };
    let reached = |specifier: &str| resolve(repo, file, specifier);
    let reaches = RefCell::new(BTreeSet::new());
    let code = classify(Path::new(file), before.as_deref(), &now, &reached, &reaches)?;
    let mut seen = BTreeSet::from([file.to_string()]);
    let mut queue: Vec<String> = reaches.into_inner().into_iter().collect();
    let mut all = BTreeSet::new();
    while let Some(next) = queue.pop() {
        if !seen.insert(next.clone()) {
            continue;
        }
        if seen.len() > MAX_REACHED {
            return Err(Refusal::Reached(next));
        }
        all.insert(next.clone());
        let more = pure_module(repo, &next).map_err(|_| Refusal::Reached(next.clone()))?;
        queue.extend(more);
    }
    Ok(NewCode {
        reaches: all,
        ..code
    })
}

/// The most files the new code may reach through relative imports before the
/// check stops reading and takes the re-scan.
const MAX_REACHED: usize = 64;

fn is_declaration_file(file: &str) -> bool {
    [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|suffix| file.ends_with(suffix))
}

/// The repo-relative file a relative specifier in `importer` resolves to, as
/// the scan resolves it; `None` for a package, an alias, a file outside the
/// repo, a declaration file, or nothing on disk.
fn resolve(repo: &Path, importer: &str, specifier: &str) -> Option<String> {
    let found = crate::agents::file_orchestrator::FileOrchestrator::resolve_relative_import(
        &repo.join(importer),
        specifier,
    )?;
    let root = repo.canonicalize().ok()?;
    let relative = found.strip_prefix(&root).ok()?;
    let relative = relative.to_string_lossy().replace('\\', "/");
    (!is_declaration_file(&relative)
        && crate::file_finder::is_scanned_source(&root.join(&relative), &root))
    .then_some(relative)
}

/// Whether every call in the module at `file` passes, read from disk: what it
/// imports from relative files, which must pass in turn.
fn pure_module(repo: &Path, file: &str) -> Result<BTreeSet<String>, Refusal> {
    if is_declaration_file(file) {
        return Err(Refusal::NotSource);
    }
    let text = std::fs::read_to_string(repo.join(file)).map_err(|_| Refusal::Unreadable)?;
    let parsed = Parsed::new(Path::new(file), &text).ok_or(Refusal::NotParsed)?;
    let table = Table::of(&parsed.module);
    let reached = |specifier: &str| resolve(repo, file, specifier);
    let reaches = RefCell::new(BTreeSet::new());
    let leg = Leg {
        table: &table,
        reached: &reached,
        reaches: &reaches,
        memo: RefCell::new(HashMap::new()),
    };
    leg.walk(&parsed.module, false)?;
    Ok(reaches.into_inner())
}

/// One copy of a file, parsed with its comments.
struct Parsed {
    module: Module,
    comments: SingleThreadedComments,
    start: BytePos,
    typescript: bool,
}

impl Parsed {
    fn new(path: &Path, text: &str) -> Option<Self> {
        let source_map: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_emitter_writer(Box::new(std::io::sink()), None);
        let comments = SingleThreadedComments::default();
        let module = crate::parser::parse_source(
            path,
            FileName::Custom("new".to_string()),
            text.to_string(),
            &source_map,
            &handler,
            Some(&comments),
        )?;
        if handler.has_errors() {
            return None;
        }
        let start = source_map.files().first()?.start_pos;
        let (_, typescript) = crate::parser::syntax_for_path(path);
        Some(Self {
            module,
            comments,
            start,
            typescript,
        })
    }

    /// Where the text after the first `kept` items begins, as a byte offset:
    /// the end of the last kept item. Whatever follows it (a comment, a
    /// decorator) belongs to the new items.
    fn after(&self, kept: usize) -> usize {
        kept.checked_sub(1)
            .and_then(|last| self.module.body.get(last))
            .map_or(0, |item| (item.span().hi.0 - self.start.0) as usize)
    }
}

/// The rules over one file, with its copy at the commit (`None` when it was
/// absent there). `reached` resolves a relative specifier; what the new code
/// calls into is added to `reaches`.
fn classify(
    path: &Path,
    before: Option<&str>,
    now: &str,
    reached: &dyn Fn(&str) -> Option<String>,
    reaches: &RefCell<BTreeSet<String>>,
) -> Result<NewCode, Refusal> {
    let parsed = Parsed::new(path, now).ok_or(Refusal::NotParsed)?;
    let items = &parsed.module.body;
    let (shape, kept, cut) = match before {
        None => (Shape::Added, 0, 0),
        Some(before) => {
            let base = Parsed::new(path, before).ok_or(Refusal::NotParsed)?;
            let kept = base.module.body.len();
            if items.len() <= kept {
                return Err(Refusal::NotAppended);
            }
            let cut = parsed.after(kept);
            let head = now.get(..cut).ok_or(Refusal::NotAppended)?;
            let lines = same_reading(path, before, head).ok_or(Refusal::NotAppended)?;
            (Shape::Extended(lines), kept, cut)
        }
    };
    let added = &items[kept..];
    let added_file = shape == Shape::Added;

    // A script's top-level names are global, and adding the first export to
    // one makes it a module: either way other files read it differently.
    let is_module = |items: &[ModuleItem]| items.iter().any(ModuleItem::is_module_decl);
    if !is_module(if added_file { added } else { &items[..kept] }) {
        return Err(Refusal::NotAModule);
    }
    // A directive prologue (`"use server"`) makes the file's exports
    // something other than functions.
    if items[..kept].iter().any(is_string_statement) {
        return Err(Refusal::Form("directive prologue"));
    }

    let mut declared: BTreeSet<String> = BTreeSet::new();
    let mut exported: BTreeSet<String> = BTreeSet::new();
    for item in added {
        item_names(item, added_file, &mut declared, &mut exported)?;
    }
    // A new name the existing items mention would change what they read: a
    // shadowed global, a merged declaration, an overload.
    let mut mentioned = Mentioned::default();
    for item in &items[..kept] {
        item.visit_with(&mut mentioned);
    }
    if let Some(name) = declared.iter().find(|name| mentioned.0.contains(*name)) {
        return Err(Refusal::NameInUse(name.clone()));
    }

    // Comments the scan reads, in the new code: only a doc comment in a
    // TypeScript file, which types nothing.
    let start = parsed.start.0 as usize;
    let (leading, trailing) = parsed.comments.borrow_all();
    let directive = leading
        .values()
        .chain(trailing.values())
        .flatten()
        .filter(|comment| (comment.span.lo.0 as usize).saturating_sub(start) >= cut)
        .filter(|comment| is_directive(comment))
        .any(|comment| {
            let marked = comment.text.contains("@ts-") || comment.text.contains("@jsx");
            !(parsed.typescript
                && comment.kind == CommentKind::Block
                && comment.text.starts_with('*')
                && !marked)
        });
    drop((leading, trailing));
    if directive {
        return Err(Refusal::Directive);
    }

    let table = Table::of(&parsed.module);
    let leg = Leg {
        table: &table,
        reached,
        reaches,
        memo: RefCell::new(HashMap::new()),
    };
    for item in added {
        leg.walk(item, true)?;
    }
    Ok(NewCode {
        shape,
        exported,
        reaches: BTreeSet::new(),
    })
}

fn is_string_statement(item: &ModuleItem) -> bool {
    matches!(
        item,
        ModuleItem::Stmt(Stmt::Expr(ExprStmt { expr, .. })) if matches!(&**expr, Expr::Lit(Lit::Str(_)))
    )
}

/// The names one new item declares, and which of them it exports; `Err` for
/// an item that is not a declaration of new names.
fn item_names(
    item: &ModuleItem,
    added_file: bool,
    declared: &mut BTreeSet<String>,
    exported: &mut BTreeSet<String>,
) -> Result<(), Refusal> {
    match item {
        ModuleItem::Stmt(Stmt::Decl(decl)) => decl_names(decl, declared).map(|_| ()),
        ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
            exported.extend(decl_names(&export.decl, declared)?);
            Ok(())
        }
        // An extended file that gains a default export, or an import, reads
        // differently to every file that already imports it.
        ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export)) if added_file => {
            let ident = match &export.decl {
                DefaultDecl::Fn(f) => f.ident.as_ref(),
                DefaultDecl::Class(c) => c.ident.as_ref(),
                DefaultDecl::TsInterfaceDecl(i) => Some(&i.id),
            };
            if matches!(&export.decl, DefaultDecl::Fn(f) if f.function.body.is_none()) {
                return Err(Refusal::Item("default export without a body"));
            }
            declared.extend(ident.map(|i| i.sym.to_string()));
            exported.insert("default".to_string());
            Ok(())
        }
        ModuleItem::ModuleDecl(ModuleDecl::Import(import)) if added_file => {
            if import.specifiers.is_empty() {
                return Err(Refusal::Item("side-effect import"));
            }
            for specifier in &import.specifiers {
                let local = match specifier {
                    ImportSpecifier::Named(named) => &named.local,
                    ImportSpecifier::Default(default) => &default.local,
                    ImportSpecifier::Namespace(namespace) => &namespace.local,
                };
                declared.insert(local.sym.to_string());
            }
            Ok(())
        }
        ModuleItem::ModuleDecl(ModuleDecl::Import(_)) => Err(Refusal::Item("import")),
        ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(_)) => {
            Err(Refusal::Item("default export"))
        }
        ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(_)) => {
            Err(Refusal::Item("default export of an expression"))
        }
        ModuleItem::ModuleDecl(ModuleDecl::ExportAll(_)) => Err(Refusal::Item("export *")),
        ModuleItem::ModuleDecl(ModuleDecl::ExportNamed(_)) => Err(Refusal::Item("export list")),
        ModuleItem::ModuleDecl(_) => Err(Refusal::Item("module declaration")),
        ModuleItem::Stmt(Stmt::Expr(_)) => Err(Refusal::Item("expression statement")),
        ModuleItem::Stmt(_) => Err(Refusal::Item("statement")),
    }
}

/// The names a declaration binds; `Err` for one that is ambient, global or
/// destructured.
fn decl_names(decl: &Decl, declared: &mut BTreeSet<String>) -> Result<Vec<String>, Refusal> {
    let names = match decl {
        Decl::Fn(f) if f.declare || f.function.body.is_none() => {
            return Err(Refusal::Item("function without a body"));
        }
        Decl::Fn(f) => vec![f.ident.sym.to_string()],
        Decl::Class(c) if c.declare => return Err(Refusal::Item("ambient class")),
        Decl::Class(c) => vec![c.ident.sym.to_string()],
        Decl::Var(var) if var.declare => return Err(Refusal::Item("ambient variable")),
        Decl::Var(var) => var
            .decls
            .iter()
            .map(|d| match &d.name {
                Pat::Ident(binding) => Ok(binding.id.sym.to_string()),
                _ => Err(Refusal::Item("destructured variable")),
            })
            .collect::<Result<_, _>>()?,
        Decl::TsInterface(i) => vec![i.id.sym.to_string()],
        Decl::TsTypeAlias(t) => vec![t.id.sym.to_string()],
        Decl::TsEnum(e) if e.declare => return Err(Refusal::Item("ambient enum")),
        Decl::TsEnum(e) => vec![e.id.sym.to_string()],
        Decl::TsModule(_) => return Err(Refusal::Item("namespace or module declaration")),
        Decl::Using(_) => return Err(Refusal::Item("using declaration")),
    };
    declared.extend(names.iter().cloned());
    Ok(names)
}

/// Every identifier an item spells, bound or free, value or type.
#[derive(Default)]
struct Mentioned(BTreeSet<String>);

impl Visit for Mentioned {
    fn visit_ident(&mut self, ident: &Ident) {
        self.0.insert(ident.sym.to_string());
    }
}

/// What one binding of the file is, for the callee-root rule.
enum Binding<'a> {
    Function(&'a Function),
    Class(&'a Class),
    /// A plain `const`/`let`/`var` name.
    Local {
        annotation: Option<&'a TsType>,
        init: Option<&'a Expr>,
    },
    /// A plain parameter name.
    Param(Option<&'a TsType>),
    Import {
        specifier: String,
        type_only: bool,
    },
    /// Anything else: destructured, a catch parameter, a loop binding, an
    /// enum, a name bound twice.
    Other,
}

/// Every binding of a module, keyed as the resolver marked it.
struct Table<'a>(HashMap<BindingKey, Binding<'a>>);

impl<'a> Table<'a> {
    fn of(module: &'a Module) -> Self {
        let mut table = TableBuilder(HashMap::new());
        table.read(module);
        Table(table.0)
    }
}

struct TableBuilder<'a>(HashMap<BindingKey, Binding<'a>>);

impl<'a> TableBuilder<'a> {
    fn insert(&mut self, ident: &Ident, binding: Binding<'a>) {
        let key = ident_key(ident);
        if self.0.contains_key(&key) {
            self.0.insert(key, Binding::Other);
        } else {
            self.0.insert(key, binding);
        }
    }

    fn param(&mut self, pat: &'a Pat) {
        match pat {
            Pat::Ident(binding) => self.insert(
                &binding.id,
                Binding::Param(binding.type_ann.as_deref().map(|ann| &*ann.type_ann)),
            ),
            other => self.others(other),
        }
    }

    /// Every name a pattern binds, as [`Binding::Other`].
    fn others(&mut self, pat: &Pat) {
        struct Names(Vec<Ident>);
        impl Visit for Names {
            fn visit_binding_ident(&mut self, binding: &BindingIdent) {
                self.0.push(binding.id.clone());
            }
            fn visit_expr(&mut self, _: &Expr) {}
        }
        let mut names = Names(Vec::new());
        pat.visit_with(&mut names);
        for ident in names.0 {
            self.insert(&ident, Binding::Other);
        }
    }

    /// A hand-written walk, because the bindings borrow from the module.
    fn read(&mut self, module: &'a Module) {
        let mut stack: Vec<Node<'a>> = module.body.iter().map(Node::Item).collect();
        while let Some(node) = stack.pop() {
            node.bind(self, &mut stack);
        }
    }
}

/// A node the table builder still has to read.
enum Node<'a> {
    Item(&'a ModuleItem),
    Stmt(&'a Stmt),
    Expr(&'a Expr),
    Function(&'a Function),
    Class(&'a Class),
    Pat(&'a Pat),
}

impl<'a> Node<'a> {
    fn bind(self, table: &mut TableBuilder<'a>, stack: &mut Vec<Node<'a>>) {
        // What the hand-written cases do not reach is read by a borrowed-free
        // pass: every binding name it finds is `Other` unless bound above.
        match self {
            Node::Item(ModuleItem::ModuleDecl(ModuleDecl::Import(import))) => {
                for specifier in &import.specifiers {
                    let (local, type_only) = match specifier {
                        ImportSpecifier::Named(named) => (&named.local, named.is_type_only),
                        ImportSpecifier::Default(default) => (&default.local, false),
                        ImportSpecifier::Namespace(namespace) => (&namespace.local, false),
                    };
                    table.insert(
                        local,
                        Binding::Import {
                            specifier: import.src.value.to_string_lossy().into_owned(),
                            type_only: type_only || import.type_only,
                        },
                    );
                }
            }
            Node::Item(ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export))) => {
                Node::decl(&export.decl, table, stack);
            }
            Node::Item(ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export))) => {
                match &export.decl {
                    DefaultDecl::Fn(f) => {
                        if let Some(ident) = &f.ident {
                            table.insert(ident, Binding::Function(&f.function));
                        }
                        stack.push(Node::Function(&f.function));
                    }
                    DefaultDecl::Class(c) => {
                        if let Some(ident) = &c.ident {
                            table.insert(ident, Binding::Class(&c.class));
                        }
                        stack.push(Node::Class(&c.class));
                    }
                    DefaultDecl::TsInterfaceDecl(_) => {}
                }
            }
            Node::Item(ModuleItem::ModuleDecl(other)) => Node::rest(other, table),
            Node::Item(ModuleItem::Stmt(stmt)) | Node::Stmt(stmt) => match stmt {
                Stmt::Decl(decl) => Node::decl(decl, table, stack),
                other => Node::rest(other, table),
            },
            Node::Function(function) => {
                for param in &function.params {
                    table.param(&param.pat);
                }
                if let Some(body) = &function.body {
                    stack.extend(body.stmts.iter().map(Node::Stmt));
                }
            }
            Node::Class(class) => Node::rest(class, table),
            Node::Expr(expr) => Node::rest(expr, table),
            Node::Pat(pat) => table.others(pat),
        }
    }

    fn decl(decl: &'a Decl, table: &mut TableBuilder<'a>, stack: &mut Vec<Node<'a>>) {
        match decl {
            Decl::Fn(f) => {
                table.insert(&f.ident, Binding::Function(&f.function));
                stack.push(Node::Function(&f.function));
            }
            Decl::Class(c) => {
                table.insert(&c.ident, Binding::Class(&c.class));
                stack.push(Node::Class(&c.class));
            }
            Decl::Var(var) => {
                for declarator in &var.decls {
                    match &declarator.name {
                        Pat::Ident(binding) => table.insert(
                            &binding.id,
                            Binding::Local {
                                annotation: binding.type_ann.as_deref().map(|a| &*a.type_ann),
                                init: declarator.init.as_deref(),
                            },
                        ),
                        other => stack.push(Node::Pat(other)),
                    }
                    if let Some(init) = &declarator.init {
                        stack.push(Node::Expr(init));
                    }
                }
            }
            Decl::TsEnum(e) => table.insert(&e.id, Binding::Other),
            other => Node::rest(other, table),
        }
    }

    /// The bindings below a node the hand-written cases do not open: plain
    /// parameters of arrows and functions in it are read as parameters, every
    /// other name as [`Binding::Other`]. Owned copies, so nothing borrows.
    fn rest<N: VisitWith<Rest>>(node: &N, table: &mut TableBuilder<'a>) {
        let mut rest = Rest::default();
        node.visit_with(&mut rest);
        for (ident, primitive) in rest.params {
            let binding = match primitive {
                Some(true) => Binding::Param(Some(&PRIMITIVE)),
                _ => Binding::Param(None),
            };
            table.insert(&ident, binding);
        }
        for (ident, local) in rest.locals {
            let binding = match local {
                LocalRead::Primitive => Binding::Param(Some(&PRIMITIVE)),
                LocalRead::Other => Binding::Other,
            };
            table.insert(&ident, binding);
        }
        for ident in rest.others {
            table.insert(&ident, Binding::Other);
        }
    }
}

/// A stand-in annotation for a binding read by [`Rest`] as typed primitive.
static PRIMITIVE: TsType = TsType::TsKeywordType(TsKeywordType {
    span: swc_common::DUMMY_SP,
    kind: TsKeywordTypeKind::TsStringKeyword,
});

enum LocalRead {
    Primitive,
    Other,
}

/// Bindings below a node, by owned identifier: parameters with whether they
/// are typed primitive, locals typed primitive, and the rest.
#[derive(Default)]
struct Rest {
    params: Vec<(Ident, Option<bool>)>,
    locals: Vec<(Ident, LocalRead)>,
    others: Vec<Ident>,
    seen: BTreeSet<BindingKey>,
}

impl Rest {
    fn param(&mut self, pat: &Pat) {
        if let Pat::Ident(binding) = pat {
            self.seen.insert(ident_key(&binding.id));
            self.params.push((
                binding.id.clone(),
                Some(
                    binding
                        .type_ann
                        .as_deref()
                        .is_some_and(|ann| is_primitive(&ann.type_ann)),
                ),
            ));
        }
    }
}

impl Visit for Rest {
    fn visit_param(&mut self, param: &Param) {
        self.param(&param.pat);
        param.visit_children_with(self);
    }

    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        for pat in &arrow.params {
            self.param(pat);
        }
        arrow.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        // A local inside a nested function is read only by its annotation:
        // its initialiser is not kept, so an unannotated one does not pass.
        if let Pat::Ident(binding) = &declarator.name {
            self.seen.insert(ident_key(&binding.id));
            let primitive = binding
                .type_ann
                .as_deref()
                .is_some_and(|ann| is_primitive(&ann.type_ann));
            self.locals.push((
                binding.id.clone(),
                if primitive {
                    LocalRead::Primitive
                } else {
                    LocalRead::Other
                },
            ));
        }
        declarator.visit_children_with(self);
    }

    fn visit_binding_ident(&mut self, binding: &BindingIdent) {
        if self.seen.insert(ident_key(&binding.id)) {
            self.others.push(binding.id.clone());
        }
        binding.visit_children_with(self);
    }

    fn visit_fn_decl(&mut self, decl: &FnDecl) {
        self.others.push(decl.ident.clone());
        decl.visit_children_with(self);
    }

    fn visit_class_decl(&mut self, decl: &ClassDecl) {
        self.others.push(decl.ident.clone());
        decl.visit_children_with(self);
    }
}

/// A primitive keyword, a literal type, or a union of them: a value whose
/// members are the language's own.
fn is_primitive(ty: &TsType) -> bool {
    match ty {
        TsType::TsKeywordType(keyword) => matches!(
            keyword.kind,
            TsKeywordTypeKind::TsStringKeyword
                | TsKeywordTypeKind::TsNumberKeyword
                | TsKeywordTypeKind::TsBooleanKeyword
                | TsKeywordTypeKind::TsBigIntKeyword
                | TsKeywordTypeKind::TsSymbolKeyword
                | TsKeywordTypeKind::TsUndefinedKeyword
                | TsKeywordTypeKind::TsNullKeyword
                | TsKeywordTypeKind::TsVoidKeyword
                | TsKeywordTypeKind::TsNeverKeyword
        ),
        TsType::TsLitType(_) => true,
        TsType::TsParenthesizedType(inner) => is_primitive(&inner.type_ann),
        TsType::TsUnionOrIntersectionType(TsUnionOrIntersectionType::TsUnionType(union)) => {
            union.types.iter().all(|ty| is_primitive(ty))
        }
        _ => false,
    }
}

/// The callee-root rule over one module's table.
struct Leg<'t, 'a> {
    table: &'t Table<'a>,
    reached: &'t dyn Fn(&str) -> Option<String>,
    reaches: &'t RefCell<BTreeSet<String>>,
    /// A function or class binding's verdict, `None` while it is being read.
    /// A verdict reached on a reading in progress can be too kind, which is
    /// sound only because any refusal ends the whole file's reading.
    memo: RefCell<HashMap<BindingKey, Option<Result<(), Refusal>>>>,
}

impl Leg<'_, '_> {
    /// Walk a subtree; `graphql` also refuses strings the GraphQL readers
    /// read (new code only: an unchanged module holds no document that a
    /// call into it could add).
    fn walk<N: VisitWith<Walker>>(&self, node: &N, graphql: bool) -> Result<(), Refusal> {
        let mut walker = Walker {
            graphql,
            refusal: None,
            roots: Vec::new(),
        };
        node.visit_with(&mut walker);
        if let Some(refusal) = walker.refusal {
            return Err(refusal);
        }
        for root in walker.roots {
            self.root(&root)?;
        }
        Ok(())
    }

    /// A function's verdict, read once. A call back into a function whose
    /// reading is in progress passes on that reading: its own calls are
    /// being read already.
    fn memoized(
        &self,
        key: BindingKey,
        read: impl FnOnce() -> Result<(), Refusal>,
    ) -> Result<(), Refusal> {
        if let Some(state) = self.memo.borrow().get(&key) {
            return match state {
                None | Some(Ok(())) => Ok(()),
                Some(Err(refusal)) => Err(refusal.clone()),
            };
        }
        self.memo.borrow_mut().insert(key.clone(), None);
        let verdict = read();
        self.memo.borrow_mut().insert(key, Some(verdict.clone()));
        verdict
    }

    fn root(&self, root: &Root) -> Result<(), Refusal> {
        match root {
            Root::Literal => Ok(()),
            Root::Other(what) => Err(Refusal::Callee(what.clone())),
            Root::Ident(ident) => self.ident(ident),
        }
    }

    fn ident(&self, ident: &Ident) -> Result<(), Refusal> {
        let refused = || Err(Refusal::Callee(ident.sym.to_string()));
        let key = ident_key(ident);
        let Some(binding) = self.table.0.get(&key) else {
            return refused();
        };
        match binding {
            Binding::Function(function) if function.body.is_none() => refused(),
            Binding::Function(function) => self.memoized(key, || self.walk(*function, false)),
            Binding::Class(class) => self.memoized(key, || self.walk(*class, false)),
            Binding::Local { annotation, init } => match (annotation, init) {
                (Some(annotation), _) if is_primitive(annotation) => Ok(()),
                // `const helper = (s: string) => ...`: read as a function.
                (None, Some(init @ (Expr::Arrow(_) | Expr::Fn(_)))) => {
                    self.memoized(key, || self.walk(*init, false))
                }
                (None, Some(init)) => roots_of(init).iter().try_for_each(|root| self.root(root)),
                _ => refused(),
            },
            Binding::Param(Some(annotation)) if is_primitive(annotation) => Ok(()),
            Binding::Import {
                specifier,
                type_only: false,
            } => match (self.reached)(specifier) {
                Some(file) => {
                    self.reaches.borrow_mut().insert(file);
                    Ok(())
                }
                None => refused(),
            },
            _ => refused(),
        }
    }
}

/// What a callee is rooted at, past calls, `await`, member access, `new`,
/// parentheses and TypeScript assertions (the walk
/// [`crate::receiver_origin::origin_root`] takes, which drops literal roots).
enum Root {
    Literal,
    Ident(Ident),
    Other(String),
}

/// Every root a callee's value can come from. A call the walk passes through
/// adds its arguments' roots, and an array or object literal its elements':
/// `pick(client).get('/x')` and `[client][0].get('/x')` reach `client`, which
/// must pass as well.
fn roots_of(expr: &Expr) -> Vec<Root> {
    let mut roots = Vec::new();
    collect_roots(expr, &mut roots);
    roots
}

fn collect_roots(expr: &Expr, roots: &mut Vec<Root>) {
    let arguments = |args: &[ExprOrSpread], roots: &mut Vec<Root>| {
        for arg in args {
            argument_roots(arg, roots);
        }
    };
    match expr {
        Expr::Ident(ident) => roots.push(Root::Ident(ident.clone())),
        Expr::Lit(_) | Expr::Tpl(_) => roots.push(Root::Literal),
        Expr::Array(array) => {
            roots.push(Root::Literal);
            for element in array.elems.iter().flatten() {
                argument_roots(element, roots);
            }
        }
        Expr::Object(object) => {
            roots.push(Root::Literal);
            for prop in &object.props {
                match prop {
                    PropOrSpread::Prop(prop) => match &**prop {
                        Prop::KeyValue(kv) => value_roots(&kv.value, roots),
                        Prop::Shorthand(ident) => roots.push(Root::Ident(ident.clone())),
                        // Methods and accessors are bodies the walk reads.
                        Prop::Method(_) | Prop::Getter(_) | Prop::Setter(_) => {}
                        Prop::Assign(_) => roots.push(Root::Other("an assignment".to_string())),
                    },
                    PropOrSpread::Spread(_) => roots.push(Root::Other("a spread".to_string())),
                }
            }
        }
        Expr::Await(e) => collect_roots(&e.arg, roots),
        Expr::Paren(e) => collect_roots(&e.expr, roots),
        Expr::TsAs(e) => collect_roots(&e.expr, roots),
        Expr::TsNonNull(e) => collect_roots(&e.expr, roots),
        Expr::TsSatisfies(e) => collect_roots(&e.expr, roots),
        Expr::TsTypeAssertion(e) => collect_roots(&e.expr, roots),
        Expr::TsConstAssertion(e) => collect_roots(&e.expr, roots),
        Expr::TsInstantiation(e) => collect_roots(&e.expr, roots),
        Expr::Member(member) => collect_roots(&member.obj, roots),
        Expr::New(new) => {
            collect_roots(&new.callee, roots);
            arguments(new.args.as_deref().unwrap_or_default(), roots);
        }
        Expr::TaggedTpl(tagged) => {
            collect_roots(&tagged.tag, roots);
            for value in &tagged.tpl.exprs {
                value_roots(value, roots);
            }
        }
        Expr::Call(call) => {
            match &call.callee {
                Callee::Expr(callee) => collect_roots(callee, roots),
                Callee::Super(_) => roots.push(Root::Other("super".to_string())),
                Callee::Import(_) => roots.push(Root::Other("import()".to_string())),
            }
            arguments(&call.args, roots);
        }
        Expr::OptChain(chain) => match &*chain.base {
            OptChainBase::Member(member) => collect_roots(&member.obj, roots),
            OptChainBase::Call(call) => {
                collect_roots(&call.callee, roots);
                arguments(&call.args, roots);
            }
        },
        // A logical operator and a conditional give one of their operands.
        Expr::Bin(bin)
            if matches!(
                bin.op,
                BinaryOp::LogicalOr | BinaryOp::LogicalAnd | BinaryOp::NullishCoalescing
            ) =>
        {
            value_roots(&bin.left, roots);
            value_roots(&bin.right, roots);
        }
        Expr::Cond(cond) => {
            value_roots(&cond.cons, roots);
            value_roots(&cond.alt, roots);
        }
        // Every other operator gives a primitive.
        Expr::Bin(_) | Expr::Unary(_) | Expr::Update(_) => roots.push(Root::Literal),
        Expr::This(_) => roots.push(Root::Other("this".to_string())),
        Expr::SuperProp(_) => roots.push(Root::Other("super".to_string())),
        _ => roots.push(Root::Other("an expression".to_string())),
    }
}

fn argument_roots(arg: &ExprOrSpread, roots: &mut Vec<Root>) {
    if arg.spread.is_some() {
        roots.push(Root::Other("a spread".to_string()));
    } else {
        value_roots(&arg.expr, roots);
    }
}

/// The roots of a value handed to a call: a function written in place is a
/// body the walk reads, anything else is rooted as a callee is.
fn value_roots(value: &Expr, roots: &mut Vec<Root>) {
    match value {
        Expr::Arrow(_) | Expr::Fn(_) => roots.push(Root::Literal),
        other => collect_roots(other, roots),
    }
}

/// Collects every callee root in a subtree, and refuses the forms that state
/// a contract without a call.
pub(super) struct Walker {
    graphql: bool,
    refusal: Option<Refusal>,
    roots: Vec<Root>,
}

impl Walker {
    fn refuse(&mut self, refusal: Refusal) {
        self.refusal.get_or_insert(refusal);
    }

    fn graphql_text(&mut self, text: &str) {
        if !self.graphql {
            return;
        }
        let document = graphql_parser::parse_query::<String>(text)
            .is_ok_and(|document| !document.definitions.is_empty());
        if document || !crate::graphql_selection::schema_text_types(text).is_empty() {
            self.refuse(Refusal::GraphqlText);
        }
    }
}

// Each call's callee and its arguments are rooted: a function handed to a
// call (`["/a"].forEach(fetch)`) is called by it.
impl Visit for Walker {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        match &call.callee {
            Callee::Expr(callee) => collect_roots(callee, &mut self.roots),
            Callee::Super(_) => self.refuse(Refusal::Callee("super".to_string())),
            Callee::Import(_) => self.refuse(Refusal::Form("dynamic import")),
        }
        for arg in &call.args {
            argument_roots(arg, &mut self.roots);
        }
        call.visit_children_with(self);
    }

    fn visit_opt_call(&mut self, call: &OptCall) {
        collect_roots(&call.callee, &mut self.roots);
        for arg in &call.args {
            argument_roots(arg, &mut self.roots);
        }
        call.visit_children_with(self);
    }

    fn visit_new_expr(&mut self, new: &NewExpr) {
        collect_roots(&new.callee, &mut self.roots);
        for arg in new.args.iter().flatten() {
            argument_roots(arg, &mut self.roots);
        }
        new.visit_children_with(self);
    }

    fn visit_tagged_tpl(&mut self, tagged: &TaggedTpl) {
        collect_roots(&tagged.tag, &mut self.roots);
        for value in &tagged.tpl.exprs {
            value_roots(value, &mut self.roots);
        }
        tagged.visit_children_with(self);
    }

    fn visit_class(&mut self, class: &Class) {
        // A subclass runs its base class's constructor.
        if let Some(base) = &class.super_class {
            collect_roots(base, &mut self.roots);
        }
        class.visit_children_with(self);
    }

    fn visit_decorator(&mut self, _: &Decorator) {
        self.refuse(Refusal::Form("decorator"));
    }

    fn visit_jsx_element(&mut self, _: &JSXElement) {
        self.refuse(Refusal::Form("JSX"));
    }

    fn visit_jsx_fragment(&mut self, _: &JSXFragment) {
        self.refuse(Refusal::Form("JSX"));
    }

    fn visit_expr_stmt(&mut self, stmt: &ExprStmt) {
        if matches!(&*stmt.expr, Expr::Lit(Lit::Str(_))) {
            self.refuse(Refusal::Form("directive prologue"));
        }
        stmt.visit_children_with(self);
    }

    fn visit_str(&mut self, string: &Str) {
        self.graphql_text(&string.value.to_string_lossy());
    }

    fn visit_tpl(&mut self, tpl: &Tpl) {
        let text = tpl
            .quasis
            .iter()
            .map(|quasi| {
                quasi
                    .cooked
                    .as_ref()
                    .map(|c| c.to_string_lossy().into_owned())
                    .unwrap_or_else(|| quasi.raw.to_string())
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.graphql_text(&text);
        tpl.visit_children_with(self);
    }
}

/// Whether any file of `repo` other than `except` spells one of `words` as a
/// whole word: tracked or untracked, ignored files left out. A failure to ask
/// reads as yes.
pub(super) fn spelled_elsewhere(repo: &Path, words: &BTreeSet<String>, except: &[String]) -> bool {
    if words.is_empty() {
        return false;
    }
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(["grep", "-l", "-I", "-w", "-F", "--untracked"]);
    for word in words {
        command.arg("-e").arg(word);
    }
    command.args(["--", "."]);
    for file in except {
        command.arg(format!(":(exclude,literal){file}"));
    }
    match command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
    {
        Ok(output) => !(output.status.code() == Some(1) && output.stdout.is_empty()),
        Err(_) => true,
    }
}

/// The words another file would spell to import `file`: its stem, and its
/// folder's name when the stem is `index`.
pub(super) fn import_words(file: &str) -> BTreeSet<String> {
    let path = PathBuf::from(file);
    let mut words = BTreeSet::new();
    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
        words.insert(stem.to_string());
        if stem == "index" {
            if let Some(folder) = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
            {
                words.insert(folder.to_string());
            }
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rules over one file, with relative imports resolving to `pure.ts`
    /// beside it and nothing else.
    fn rules(before: Option<&str>, now: &str) -> Result<NewCode, Refusal> {
        let reaches = RefCell::new(BTreeSet::new());
        let reached = |specifier: &str| (specifier == "./pure.js").then(|| "pure.ts".to_string());
        classify(Path::new("a.ts"), before, now, &reached, &reaches).map(|code| NewCode {
            reaches: reaches.into_inner(),
            ..code
        })
    }

    const BASE: &str =
        "export function trimmed(value: string): string {\n  return value.trim();\n}\n";

    fn appended(item: &str) -> Result<NewCode, Refusal> {
        rules(Some(BASE), &format!("{BASE}\n{item}\n"))
    }

    #[test]
    fn an_added_helper_over_strings_regexes_and_a_relative_import_passes() {
        let code = rules(
            None,
            "import { squash } from './pure.js';\n\
             const SLUG = /^[a-z-]+$/;\n\
             /** Whether the name is a slug. */\n\
             export function isSlug(name: string, mode: 'strict' | 'loose'): boolean {\n\
               const lowered = name.toLowerCase();\n\
               return SLUG.test(squash(lowered)) && mode.length > 0 && [1, 2].includes(1);\n\
             }\n",
        )
        .expect("passes");
        assert_eq!(code.shape, Shape::Added);
        assert_eq!(code.exported, BTreeSet::from(["isSlug".to_string()]));
        assert_eq!(code.reaches, BTreeSet::from(["pure.ts".to_string()]));
    }

    #[test]
    fn an_appended_export_passes_and_keeps_the_line_map() {
        let code = appended(
            "export function shout(value: string): string {\n  return trimmed(value).toUpperCase();\n}",
        )
        .expect("passes");
        assert!(matches!(code.shape, Shape::Extended(_)));
        assert_eq!(code.exported, BTreeSet::from(["shout".to_string()]));
    }

    #[test]
    fn an_edit_to_an_existing_item_is_not_appended() {
        let edited = BASE.replace("trim()", "trimStart()");
        assert_eq!(
            rules(Some(BASE), &format!("{edited}\nexport const x = 1;\n")),
            Err(Refusal::NotAppended)
        );
        let inserted = format!("export const x = 1;\n{BASE}");
        assert_eq!(rules(Some(BASE), &inserted), Err(Refusal::NotAppended));
    }

    #[test]
    fn leg_five_refuses_what_is_not_a_declaration_of_new_names() {
        for (item, why) in [
            ("trimmed('a');", "expression statement"),
            ("export * from './pure.js';", "export *"),
            ("export { trimmed as t };", "export list"),
            ("export default 1;", "default export of an expression"),
            ("export default function f() {}", "default export"),
            ("import { squash } from './pure.js';", "import"),
            (
                "declare global { interface W { a: 1 } }",
                "namespace or module declaration",
            ),
            ("declare module 'x' { }", "namespace or module declaration"),
            (
                "export declare function f(): void;",
                "function without a body",
            ),
            ("if (true) {}", "statement"),
        ] {
            assert_eq!(appended(item), Err(Refusal::Item(why)), "{item}");
        }
        assert_eq!(
            rules(None, "import './pure.js';\nexport const a = 1;\n"),
            Err(Refusal::Item("side-effect import"))
        );
    }

    #[test]
    fn leg_five_refuses_a_name_the_file_already_declares_or_reads() {
        assert_eq!(
            appended("export function trimmed(value: number): string;"),
            Err(Refusal::Item("function without a body")),
            "an overload signature"
        );
        assert_eq!(
            appended(
                "export function trimmed2(value: string) { return value; }\nexport const value = 1;"
            ),
            Err(Refusal::NameInUse("value".to_string())),
            "a name an existing item spells"
        );
        let reads_global = "export function load(id: string) {\n  return fetch(id);\n}\n";
        assert_eq!(
            rules(
                Some(reads_global),
                &format!("{reads_global}export function fetch(id: string) {{ return id; }}\n")
            ),
            Err(Refusal::NameInUse("fetch".to_string())),
            "a global the existing code calls, shadowed"
        );
    }

    #[test]
    fn leg_five_refuses_a_script() {
        assert_eq!(
            rules(None, "function helper(a: string) { return a; }\n"),
            Err(Refusal::NotAModule)
        );
    }

    #[test]
    fn leg_six_refuses_a_callee_root_not_known_to_reach_no_contract() {
        for (item, root) in [
            ("export function a(u: string) { return fetch(u); }", "fetch"),
            ("export function a(u: string) { return new URL(u); }", "URL"),
            ("export function a(c: Client) { return c.get('/x'); }", "c"),
            ("export function a(c) { return c.get('/x'); }", "c"),
            (
                "export function a(u: string) { const f = makeIt(); return f(u); }",
                "makeIt",
            ),
            (
                "export const a = (u: string) => u.trim();\nexport const b = (u) => a(u) && u.x();",
                "u",
            ),
        ] {
            assert_eq!(
                appended(item),
                Err(Refusal::Callee(root.to_string())),
                "{item}"
            );
        }
        assert_eq!(
            appended("export class A { b() { return this.c(); } c() { return 1; } }"),
            Err(Refusal::Callee("this".to_string()))
        );
        assert_eq!(
            rules(
                None,
                "import axios from 'axios';\nexport function a(u: string) { return axios.get(u); }\n"
            ),
            Err(Refusal::Callee("axios".to_string())),
            "a package import"
        );
        assert_eq!(
            rules(
                None,
                "import { send } from './other.js';\nexport function a(u: string) { return send(u); }\n"
            ),
            Err(Refusal::Callee("send".to_string())),
            "a relative import the resolver does not find"
        );
        assert_eq!(
            rules(
                None,
                "import type { squash } from './pure.js';\nexport function a(u: string) { return squash(u); }\n"
            ),
            Err(Refusal::Callee("squash".to_string())),
            "a type-only import"
        );
        assert_eq!(
            appended("export function a(u: string) { return tag`plain ${u}`; }"),
            Err(Refusal::Callee("tag".to_string())),
            "a tagged template"
        );
        assert!(
            appended(
                "export const a = (u: string) => u.trim();\nexport const b = (u: string) => a(u);"
            )
            .is_ok(),
            "an arrow-function helper of the same file"
        );
        assert_eq!(
            appended("export async function a() { return import('./pure.js'); }"),
            Err(Refusal::Form("dynamic import"))
        );
    }

    #[test]
    fn leg_six_roots_the_values_a_call_is_handed() {
        for (item, root) in [
            (
                "function pick(c: Client) { return c; }\nexport function a(c: Client) { const api = pick(c); return api.get('/x'); }",
                "c",
            ),
            (
                "function pick(c: Client) { return c; }\nexport function a(c: Client) { return pick(c).get('/x'); }",
                "c",
            ),
            (
                "export function a(c: Client) { return [c][0].get('/x'); }",
                "c",
            ),
            (
                "export function a(c: Client) { return { c }.c.get('/x'); }",
                "c",
            ),
            (
                "export function a(u: string) { return ['/x'].forEach(fetch); }",
                "fetch",
            ),
            (
                "export function a(u: string, c: Client) { return (u || c).get(); }",
                "c",
            ),
        ] {
            assert_eq!(
                appended(item),
                Err(Refusal::Callee(root.to_string())),
                "{item}"
            );
        }
        assert!(
            appended("export function a(u: string, n: number) { return [1, 2].map((x) => x * 2).concat(u.slice(0, n - 1).length); }")
                .is_ok(),
            "a callback written in place and operators over primitives"
        );
    }

    #[test]
    fn leg_six_follows_a_function_of_the_same_file() {
        assert_eq!(
            rules(
                Some("export function load(u: string) { return fetch(u); }\n"),
                "export function load(u: string) { return fetch(u); }\nexport function again(u: string) { return load(u); }\n"
            ),
            Err(Refusal::Callee("fetch".to_string())),
            "an existing function that calls a global"
        );
        assert!(appended("export function a(u: string) { return trimmed(u); }").is_ok());
        assert!(
            appended("function even(n: number): boolean { return n === 0 || odd(n - 1); }\nfunction odd(n: number): boolean { return n !== 0 && even(n - 1); }\nexport const e = even(2);").is_ok(),
            "mutual recursion"
        );
    }

    #[test]
    fn forms_that_state_a_contract_without_a_call_are_refused() {
        let tsx = |text: &str| {
            let reaches = RefCell::new(BTreeSet::new());
            classify(Path::new("a.tsx"), None, text, &|_| None, &reaches)
        };
        assert_eq!(
            tsx("export function A() { return <div />; }\n"),
            Err(Refusal::Form("JSX"))
        );
        assert_eq!(
            appended("@Controller('/x')\nexport class A {}"),
            Err(Refusal::Form("decorator"))
        );
        assert_eq!(
            appended("export function a() { 'use server'; return 1; }"),
            Err(Refusal::Form("directive prologue"))
        );
        let served = format!("'use server';\n{BASE}");
        assert_eq!(
            rules(Some(&served), &format!("{served}export const x = 1;\n")),
            Err(Refusal::Form("directive prologue"))
        );
        assert_eq!(
            appended("export const Q = 'query Widgets { widgets { id } }';"),
            Err(Refusal::GraphqlText)
        );
        assert_eq!(
            appended("export const S = `type Widget { id: ID! }`;"),
            Err(Refusal::GraphqlText)
        );
    }

    #[test]
    fn comments_the_scan_reads_in_new_code_are_refused() {
        assert_eq!(
            appended("// @ts-ignore\nexport const x: string = 1;"),
            Err(Refusal::Directive)
        );
        assert!(appended("/** A doc comment. */\nexport const x = 1;").is_ok());
        let js = |text: &str| {
            let reaches = RefCell::new(BTreeSet::new());
            classify(Path::new("a.js"), None, text, &|_| None, &reaches)
        };
        assert_eq!(
            js("/** @type {number} */\nexport const x = 1;\n"),
            Err(Refusal::Directive),
            "a JSDoc type in a JavaScript file"
        );
    }

    #[test]
    fn import_words_name_the_file_and_an_index_folder() {
        assert_eq!(
            import_words("src/text/index.ts"),
            BTreeSet::from(["index".to_string(), "text".to_string()])
        );
        assert_eq!(
            import_words("src/slug.ts"),
            BTreeSet::from(["slug".to_string()])
        );
    }
}
