//! Which same-repo modules a file's call sites reach (carrick#1928).
//!
//! A file's analysis prompt carried the source of every wrapper module the
//! file imports. It now carries one only where a call in the file reaches
//! that module. The rule is stated in call sites, and both halves are
//! answered by what discovery already resolved ([`RequestSummaryInputs`]), so
//! the answer is a function of the tree and never of whether a type sidecar
//! started:
//!
//! 1. a call site reaches a declaration in the module: the call graph
//!    recorded where the site lands ([`RequestSummaryInputs::call_target`]);
//! 2. the call graph recorded no target for the site, and the site's callee
//!    chain is rooted at a binding the file imports from the module
//!    ([`crate::call_graph::ImportedBindings`]): `api.get(…)` where the
//!    module writes `export const api = makeClient(…)`.
//!
//! An import that is only passed as an argument, named in a type, or never
//! used is the root of no call and reaches nothing. A site neither half
//! answers reaches nothing either: a shape that cannot be decided attaches no
//! module.
//!
//! A call site is a call, an optional call, or a `new` expression, which
//! calls the class's constructor. The call graph records no target for a
//! `new`, so one is answered by the second half alone: constructing an
//! imported class keeps the module that declares it, which is what holds
//! `this.client = new Client(base)` together with the methods later called on
//! a field the call graph cannot follow. A JSX element is not a call site:
//! rendering a component hands the component to the JSX factory as an
//! argument, and sends none of that component's requests from this file.
//!
//! The caller says which modules may be reached at all (`attachable`). The
//! analyzer's wrapper pass passes the modules the file's import table already
//! reaches through relative specifiers, so this rule only ever removes a
//! module from a prompt; an import written through an alias or a package name
//! is still not followed there (carrick#474).
//!
//! See `docs/reference/module-resolution.md`, "What a prompt attaches".

use crate::call_graph::ImportedBinding;
use crate::graphql_document_sites::unwrap_expression;
use crate::request_summary::RequestSummaryInputs;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use swc_common::{FileName, GLOBALS, Globals, Mark, SourceMap, Span, sync::Lrc};
use swc_ecma_ast::{
    BindingIdent, CallExpr, Callee, Decl, Expr, Id, ImportSpecifier, MemberExpr, MemberProp,
    Module, ModuleDecl, ModuleItem, NewExpr, OptCall, OptChainBase, Stmt, TsTypeAnn, VarDecl,
};
use swc_ecma_parser::{Parser, StringInput, lexer::Lexer};
use swc_ecma_transforms_base::resolver;
use swc_ecma_visit::{Visit, VisitMutWith, VisitWith};

/// The binding a call's callee chain starts at, when the file's module scope
/// declares it by an import or a variable: `api` in `api.users.list()`, with
/// the member read directly off it (`users`), when that is a plain name.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Root {
    name: String,
    member: Option<String>,
}

/// One call site of a file, in the numbering a candidate's span uses
/// ([`crate::swc_scanner::SWC_SPAN_BASE`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct CallSite {
    span_start: u32,
    span_end: u32,
    root: Option<Root>,
}

/// Collects every call site of a module, with the module-scope binding each
/// one's callee chain is rooted at.
struct CallSites {
    /// The bindings the module scope declares by an import or a variable
    /// declaration, by identity: a parameter or a local that shares an
    /// import's name is another binding and roots nothing.
    module_scope: HashSet<Id>,
    sites: Vec<CallSite>,
}

/// The identifiers a declaration pattern binds.
struct PatternBindings<'a>(&'a mut HashSet<Id>);

impl Visit for PatternBindings<'_> {
    fn visit_binding_ident(&mut self, binding: &BindingIdent) {
        self.0.insert(binding.id.to_id());
    }

    // A default value and a type annotation declare nothing of the pattern's.
    fn visit_expr(&mut self, _: &Expr) {}

    fn visit_ts_type_ann(&mut self, _: &TsTypeAnn) {}
}

/// `expr` as a member read, through an optional chain.
fn as_member(expr: &Expr) -> Option<&MemberExpr> {
    match expr {
        Expr::Member(member) => Some(member),
        Expr::OptChain(chain) => match &*chain.base {
            OptChainBase::Member(member) => Some(member),
            OptChainBase::Call(_) => None,
        },
        _ => None,
    }
}

impl CallSites {
    fn of(module: &Module) -> Vec<CallSite> {
        let mut module_scope = HashSet::new();
        let mut declared = |var: &VarDecl| {
            for declarator in &var.decls {
                declarator
                    .name
                    .visit_with(&mut PatternBindings(&mut module_scope));
            }
        };
        let mut imported: Vec<Id> = Vec::new();
        for item in &module.body {
            match item {
                // `import type` binds a type: no call is rooted at one.
                ModuleItem::ModuleDecl(ModuleDecl::Import(import)) if !import.type_only => {
                    for specifier in &import.specifiers {
                        match specifier {
                            ImportSpecifier::Named(named) if !named.is_type_only => {
                                imported.push(named.local.to_id());
                            }
                            ImportSpecifier::Named(_) => {}
                            ImportSpecifier::Default(default) => {
                                imported.push(default.local.to_id());
                            }
                            ImportSpecifier::Namespace(namespace) => {
                                imported.push(namespace.local.to_id());
                            }
                        }
                    }
                }
                ModuleItem::ModuleDecl(ModuleDecl::TsImportEquals(decl)) if !decl.is_type_only => {
                    imported.push(decl.id.to_id());
                }
                // `const api = require("./api")`, and the destructured form.
                ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => {
                    if let Decl::Var(var) = &export.decl {
                        declared(var);
                    }
                }
                ModuleItem::Stmt(Stmt::Decl(Decl::Var(var))) => declared(var),
                _ => {}
            }
        }
        module_scope.extend(imported);
        let mut collector = CallSites {
            module_scope,
            sites: Vec::new(),
        };
        module.visit_with(&mut collector);
        collector
            .sites
            .sort_by_key(|site| (site.span_start, site.span_end));
        collector.sites
    }

    fn record(&mut self, span: Span, root: Option<Root>) {
        if span.is_dummy() {
            return;
        }
        self.sites.push(CallSite {
            span_start: span.lo.0,
            span_end: span.hi.0,
            root,
        });
    }

    /// The module-scope binding `callee`'s chain starts at: every hop a
    /// member read, through parentheses and type-only wrappers. A chain that
    /// starts at what another call returns, at `this`, or at a binding some
    /// inner scope declares has no such root.
    fn root_of(&self, callee: &Expr) -> Option<Root> {
        let mut member: Option<String> = None;
        let mut at = unwrap_expression(callee);
        loop {
            if let Expr::Ident(ident) = at {
                return self.module_scope.contains(&ident.to_id()).then(|| Root {
                    name: ident.sym.to_string(),
                    member,
                });
            }
            let read = as_member(at)?;
            member = match &read.prop {
                MemberProp::Ident(name) => Some(name.sym.to_string()),
                MemberProp::PrivateName(_) | MemberProp::Computed(_) => None,
            };
            at = unwrap_expression(&read.obj);
        }
    }
}

impl Visit for CallSites {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        if let Callee::Expr(callee) = &call.callee {
            let root = self.root_of(callee);
            self.record(call.span, root);
        }
        call.visit_children_with(self);
    }

    /// `api?.get()` and `load?.()` parse as an optional call.
    fn visit_opt_call(&mut self, call: &OptCall) {
        let root = self.root_of(&call.callee);
        self.record(call.span, root);
        call.visit_children_with(self);
    }

    /// `new Client(base)`, `new lib.Client()`: the constructor is called.
    fn visit_new_expr(&mut self, new: &NewExpr) {
        let root = self.root_of(&new.callee);
        self.record(new.span, root);
        new.visit_children_with(self);
    }
}

/// `content` parsed with its scopes resolved, in a source map of its own: a
/// span counts from the start of this file, as a candidate's does.
fn parse_scoped(path: &Path, content: &str) -> Option<Module> {
    let (syntax, is_typescript) = crate::parser::syntax_for_path(path);
    let source_map: Lrc<SourceMap> = Default::default();
    let source = source_map.new_source_file(
        Lrc::new(FileName::Real(path.to_path_buf())),
        content.to_string(),
    );
    let lexer = Lexer::new(
        syntax,
        Default::default(),
        StringInput::from(&*source),
        None,
    );
    let mut module = Parser::new_from(lexer).parse_module().ok()?;
    GLOBALS.set(&Globals::new(), || {
        module.visit_mut_with(&mut resolver(Mark::new(), Mark::new(), is_typescript));
    });
    Some(module)
}

/// The attachable modules one file's call sites reach.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Reach {
    /// The modules, by canonical path, sorted, each once.
    pub modules: Vec<PathBuf>,
    /// Each call site that reaches one, as (where the call starts, the
    /// module's place in `modules`), sorted, each pair once.
    pub sites: Vec<(u32, usize)>,
}

/// The places in a file's attached modules that the call sites written inside
/// `span_start..span_end` reach: what one candidate of the file calls into.
/// `sites` is [`Reach::sites`].
pub(crate) fn places_within(
    sites: &[(u32, usize)],
    span_start: u32,
    span_end: u32,
) -> impl Iterator<Item = usize> + '_ {
    let from = sites.partition_point(|(start, _)| *start < span_start);
    sites[from..]
        .iter()
        .take_while(move |(start, _)| *start < span_end)
        .map(|(_, place)| *place)
}

/// Answers which modules a file's call sites reach, for every file of one
/// service: what discovery resolved, and each path it names made canonical
/// once.
pub(crate) struct ModuleReach<'a> {
    resolution: &'a RequestSummaryInputs,
    canonical: HashMap<PathBuf, Option<PathBuf>>,
}

impl<'a> ModuleReach<'a> {
    pub(crate) fn new(resolution: &'a RequestSummaryInputs) -> Self {
        Self {
            resolution,
            canonical: HashMap::new(),
        }
    }

    fn canonical(&mut self, path: &Path) -> Option<PathBuf> {
        if let Some(known) = self.canonical.get(path) {
            return known.clone();
        }
        let canonical = path.canonicalize().ok();
        self.canonical.insert(path.to_path_buf(), canonical.clone());
        canonical
    }

    /// The modules `attachable` accepts (by canonical path) that a call site
    /// of `file` reaches. `file` is the path discovery walked the file by,
    /// and `content` its text. The file itself is never one of them, and a
    /// file that does not parse reaches nothing.
    pub(crate) fn of(
        &mut self,
        file: &Path,
        content: &str,
        attachable: impl Fn(&Path) -> bool,
    ) -> Reach {
        let Some(module) = parse_scoped(file, content) else {
            return Reach::default();
        };
        let own = self.canonical(file);
        // Copied out, so what it answers borrows the inputs and not `self`.
        let resolution = self.resolution;
        let mut reached: BTreeSet<(u32, PathBuf)> = BTreeSet::new();
        for site in CallSites::of(&module) {
            let targets: Vec<&Path> =
                match resolution.call_target(file, site.span_start, site.span_end) {
                    // The call graph says where the site lands, and that is
                    // the whole answer for it, wherever it is.
                    Some((target, _)) => vec![target.as_path()],
                    None => match &site.root {
                        Some(root) => match resolution.bindings.get(file, &root.name) {
                            Some(ImportedBinding::Binding { file, .. }) => vec![file.as_path()],
                            // A whole module: the member called off it names
                            // the binding, and so the module that declares it.
                            Some(ImportedBinding::Module(published)) => published
                                .iter()
                                .filter(|binding| {
                                    root.member.as_deref() == Some(binding.published.as_str())
                                })
                                .map(|binding| binding.file.as_path())
                                .collect(),
                            Some(ImportedBinding::Unfollowable | ImportedBinding::Unresolved)
                            | None => Vec::new(),
                        },
                        None => Vec::new(),
                    },
                };
            for target in targets {
                let Some(target) = self.canonical(target) else {
                    continue;
                };
                if own.as_ref() != Some(&target) && attachable(&target) {
                    reached.insert((site.span_start, target));
                }
            }
        }
        let modules: Vec<PathBuf> = reached
            .iter()
            .map(|(_, module)| module.clone())
            .collect::<BTreeSet<PathBuf>>()
            .into_iter()
            .collect();
        let sites = reached
            .into_iter()
            .filter_map(|(start, module)| Some((start, modules.binary_search(&module).ok()?)))
            .collect();
        Reach { modules, sites }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swc_scanner::SWC_SPAN_BASE;

    /// A root as a test states it: the binding, and the member read off it.
    type Rooted = Option<(String, Option<String>)>;

    /// The call sites of `source`, each as the text it spans and its root.
    fn sites(path: &str, source: &str) -> Vec<(String, Rooted)> {
        let module = parse_scoped(Path::new(path), source).expect("the source parses");
        CallSites::of(&module)
            .into_iter()
            .map(|site| {
                let text = &source[(site.span_start - SWC_SPAN_BASE) as usize
                    ..(site.span_end - SWC_SPAN_BASE) as usize];
                (
                    text.to_string(),
                    site.root.map(|root| (root.name, root.member)),
                )
            })
            .collect()
    }

    fn root(name: &str, member: Option<&str>) -> Rooted {
        Some((name.to_string(), member.map(str::to_string)))
    }

    #[test]
    fn a_call_is_rooted_at_the_import_its_callee_chain_starts_at() {
        let source = "\
import send from \"./send\";
import { api as client } from \"./api\";
import * as lib from \"./lib\";
send(\"/a\");
client.get(\"/b\");
lib.clients.orders.list();
client?.get(\"/c\");
(client as any).post!(\"/d\");
send.call(null, \"/e\");
client[\"get\"](\"/f\");
class Local {}
new lib.Worker(\"/g\");
new Local();
";
        assert_eq!(
            sites("src/a.ts", source),
            [
                ("send(\"/a\")".to_string(), root("send", None)),
                (
                    "client.get(\"/b\")".to_string(),
                    root("client", Some("get"))
                ),
                (
                    "lib.clients.orders.list()".to_string(),
                    root("lib", Some("clients"))
                ),
                (
                    "client?.get(\"/c\")".to_string(),
                    root("client", Some("get"))
                ),
                (
                    "(client as any).post!(\"/d\")".to_string(),
                    root("client", Some("post"))
                ),
                (
                    "send.call(null, \"/e\")".to_string(),
                    root("send", Some("call"))
                ),
                // A computed member names nothing, and the root still stands.
                ("client[\"get\"](\"/f\")".to_string(), root("client", None)),
                // A constructor call is rooted like any other. A class this
                // file declares is no import.
                (
                    "new lib.Worker(\"/g\")".to_string(),
                    root("lib", Some("Worker"))
                ),
                ("new Local()".to_string(), None),
            ]
        );
    }

    #[test]
    fn an_import_that_is_not_the_root_of_a_call_roots_nothing() {
        let source = "\
import { api } from \"./api\";
import type { Api } from \"./api\";
import { type Options, make } from \"./make\";
declare function register(path: string, handler: unknown): void;
declare function local(): { get(path: string): void };
register(\"/a\", api);
function inner(api: Api) {
  api.get(\"/shadowed\");
}
local().get(\"/chained\");
const options: Options = {};
make(options).get(\"/built\");
";
        assert_eq!(
            sites("src/a.ts", source),
            [
                // Passed as an argument: the call is `register`'s.
                ("register(\"/a\", api)".to_string(), None),
                // A parameter that shares the import's name is another binding.
                ("api.get(\"/shadowed\")".to_string(), None),
                // What a call returns is no binding; the call that returns
                // it is its own site.
                ("local()".to_string(), None),
                ("local().get(\"/chained\")".to_string(), None),
                ("make(options)".to_string(), root("make", None)),
                ("make(options).get(\"/built\")".to_string(), None),
            ]
        );
    }

    #[test]
    fn a_required_binding_is_a_root_and_a_rendered_component_is_no_call() {
        let source = "\
const http = require(\"./http\");
const { post } = require(\"./http\");
import { Panel } from \"./panel\";
import * as ui from \"./ui\";
http.get(\"/a\");
post(\"/b\");
export const page = () => <ui.Card title={ui.title()}><Panel /><div /></ui.Card>;
";
        assert_eq!(
            sites("src/a.jsx", source),
            [
                ("require(\"./http\")".to_string(), None),
                ("require(\"./http\")".to_string(), None),
                ("http.get(\"/a\")".to_string(), root("http", Some("get"))),
                ("post(\"/b\")".to_string(), root("post", None)),
                // The elements are no call sites. A call written inside one
                // is.
                ("ui.title()".to_string(), root("ui", Some("title"))),
            ]
        );
    }

    #[test]
    fn a_candidate_holds_the_sites_written_inside_its_span() {
        let sites = [(10, 0), (10, 2), (25, 1), (40, 0)];
        let within =
            |from, to| -> Vec<usize> { places_within(&sites, from, to).collect::<Vec<usize>>() };
        assert_eq!(within(10, 30), [0, 2, 1]);
        assert_eq!(within(11, 25), Vec::<usize>::new());
        assert_eq!(within(25, 41), [1, 0]);
        assert_eq!(within(41, 90), Vec::<usize>::new());
    }

    #[test]
    fn a_file_that_does_not_parse_reaches_nothing() {
        let resolution = RequestSummaryInputs::default();
        let mut reach = ModuleReach::new(&resolution);
        assert_eq!(
            reach.of(Path::new("src/a.ts"), "send(\"/a\";\n", |_| true),
            Reach::default()
        );
    }
}
