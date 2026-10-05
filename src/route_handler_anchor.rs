//! Where the handler a route registration names is declared, when that is
//! another file (carrick#1913).
//!
//! A registration that is handed its handler by name
//! (`router.post("/widgets", requireKey, createWidget)`, with `createWidget`
//! imported) holds nothing of the response: the function that returns or
//! sends the body is in the module the import leads to. The model reads the
//! registration file, so the style and the locator it gives such a row are
//! not readings of the handler, and a type request located in the
//! registration file reads the registration.
//!
//! This pass follows the name instead, and the type request is made at the
//! handler's own declaration, in its own file. Three reads, each of one
//! module's syntax tree:
//!
//! 1. the call at the row's span is handed the name the row gives as its
//!    handler, as an argument or as a property of an object argument
//!    ([`registration_is_handed`]);
//! 2. the registration's module imports that name
//!    ([`crate::graphql_resolver_anchor::named_resolver`], the read a named
//!    GraphQL resolver takes), and the import is followed through re-exports
//!    and path aliases to the module that declares it ([`BindingResolver`]);
//! 3. that module declares the name at module scope as a function, or as a
//!    binding of a function or of a call that is handed one
//!    ([`handler_declaration`]).
//!
//! Anything else states no anchor, and the row is asked what it was asked
//! before: a name the registration's own module declares (the model read that
//! handler), a member of an object, a binding of anything but a function or
//! a call handed one, a name two declarations share. No framework, package or
//! identifier is named anywhere here.
//!
//! The spans are the scanner's own (UTF-8 bytes from
//! [`crate::swc_scanner::SWC_SPAN_BASE`]); the request boundary converts them
//! against the source of the file they were read from, never the
//! registration's (carrick#805).
//!
//! What the type sidecar reads at such a span, and what it answers, is stated
//! in `src/sidecar/README.md` under "A request located at a handler".

use std::collections::HashMap;
use std::path::Path;

use swc_ecma_ast::{
    CallExpr, Callee, Decl, DefaultDecl, Expr, ExprOrSpread, ModuleDecl, ModuleItem, Pat, Prop,
    PropOrSpread, Stmt, VarDeclarator,
};
use swc_ecma_visit::{Visit, VisitWith};

use crate::graphql_resolver_anchor::{
    NamedResolver, function_literal_span, named_resolver, unwrap_expr,
};
use crate::import_bindings::{BindingResolver, DEFAULT_EXPORT};

/// Calls a wrapped function is looked for through, counted from the call a
/// binding is initialised with (`limited(guarded(async (req, res) => { … }))`
/// is two). The sidecar follows the same number, so a binding this pass
/// offers is one the sidecar reads.
const WRAPPER_DEPTH: usize = 3;

/// The declaration a type request for a route's response is located at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerAnchor {
    /// Canonical path of the module that declares the handler.
    pub file: String,
    /// 1-based line the declaration opens on.
    pub line: u32,
    /// The declaration's span in the scanner's numbering: the function, or
    /// the whole `name = …` declarator of a binding.
    pub lo: u32,
    pub hi: u32,
}

/// Where the handler of the registration at `[call_lo, call_hi)` of
/// `registration_file` is declared, when the registration is handed
/// `handler_name` and that name is declared in another file.
///
/// `sources` remembers every file it reads, and `None` for one that could not
/// be read, so a file is read once however many rows name it. `bindings`
/// keeps each module's export table the same way.
pub fn handler_declared_elsewhere(
    registration_file: &str,
    call_lo: u32,
    call_hi: u32,
    handler_name: &str,
    sources: &mut HashMap<String, Option<String>>,
    bindings: &mut BindingResolver,
) -> Option<HandlerAnchor> {
    let registration = Path::new(registration_file);
    let (specifier, imported) = {
        let content = cached_source(sources, registration_file)?;
        // Most rows are handed their handler as a function written in place,
        // and the name the model gives such a row is nowhere in the call. That
        // is read off the call's own text, before any parse.
        let base = crate::swc_scanner::SWC_SPAN_BASE;
        let call_text = content
            .get(call_lo.checked_sub(base)? as usize..call_hi.checked_sub(base)? as usize)?;
        if handler_name.is_empty() || !call_text.contains(handler_name) {
            return None;
        }
        if !registration_is_handed(registration, content, call_lo, call_hi, handler_name) {
            return None;
        }
        // A name the module declares itself answers `Declared` or nothing:
        // that handler is in the file the model read.
        match named_resolver(registration, content, handler_name)? {
            NamedResolver::Imported {
                specifier,
                imported,
            } => (specifier, imported),
            NamedResolver::Declared { .. } => return None,
        }
    };
    let resolved = bindings.resolve(registration, &specifier, &imported)?;
    let declaring_file = resolved.file.to_string_lossy().to_string();
    if resolved.file.as_path() == registration {
        return None;
    }
    // The module may publish the binding under another name
    // (`export { createWidget as create }`); the resolver reports the local
    // one, and none for a default export that is the function itself.
    let local = resolved
        .local_name
        .unwrap_or_else(|| DEFAULT_EXPORT.to_string());
    let content = cached_source(sources, &declaring_file)?;
    let (lo, hi, line) = handler_declaration(&resolved.file, content, &local)?;
    Some(HandlerAnchor {
        file: declaring_file,
        line,
        lo,
        hi,
    })
}

/// The source of `file`, read once and remembered.
fn cached_source<'a>(
    sources: &'a mut HashMap<String, Option<String>>,
    file: &str,
) -> Option<&'a str> {
    sources
        .entry(file.to_string())
        .or_insert_with(|| std::fs::read_to_string(file).ok())
        .as_deref()
}

/// Whether the call spanning exactly `[lo, hi)` in `content` is handed the
/// binding `name`: as an argument (`router.get(path, guard, name)`) or as the
/// value of a property of an object argument (`server.route({ method, path,
/// handler: name })`, or the shorthand `{ name }`).
///
/// The row's own handler name says which argument, so neither a position nor
/// a property's name is read. A member of a binding (`handlers.create`) is
/// not that binding, and a call's result (`build(name)`) is not either.
///
/// A name with a function written after it in the same call is not the
/// handler (`router.get(path, requireKey, async (req, res) => { … })`): the
/// handler is the function written in place, in the file the model read, and
/// a row that names the gate in front of it is asked what it was asked before.
pub fn registration_is_handed(
    file_path: &Path,
    content: &str,
    lo: u32,
    hi: u32,
    name: &str,
) -> bool {
    let Some((_, module)) = crate::swc_scanner::parse_standalone_module(file_path, content) else {
        return false;
    };
    let mut finder = RegistrationFinder {
        lo,
        hi,
        name,
        handed: false,
    };
    module.visit_with(&mut finder);
    finder.handed
}

struct RegistrationFinder<'a> {
    lo: u32,
    hi: u32,
    name: &'a str,
    handed: bool,
}

impl Visit for RegistrationFinder<'_> {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        if call.span.lo.0 == self.lo && call.span.hi.0 == self.hi {
            self.handed = call
                .args
                .iter()
                .rposition(|argument| argument_is_binding(argument, self.name))
                .is_some_and(|handed_at| {
                    call.args[handed_at + 1..]
                        .iter()
                        .all(|later| function_literal_span(unwrap_expr(&later.expr)).is_none())
                });
            return;
        }
        // A chained registration's span is the outermost call's, and a
        // registration inside a callback is a call of its own further in.
        call.visit_children_with(self);
    }
}

fn argument_is_binding(argument: &ExprOrSpread, name: &str) -> bool {
    if argument.spread.is_some() {
        return false;
    }
    match unwrap_expr(&argument.expr) {
        Expr::Ident(ident) => ident.sym.as_ref() == name,
        Expr::Object(object) => object.props.iter().any(|prop| {
            let PropOrSpread::Prop(prop) = prop else {
                return false;
            };
            match &**prop {
                Prop::KeyValue(pair) => {
                    matches!(unwrap_expr(&pair.value), Expr::Ident(ident) if ident.sym.as_ref() == name)
                }
                Prop::Shorthand(ident) => ident.sym.as_ref() == name,
                _ => false,
            }
        }),
        _ => false,
    }
}

/// The module-scope declaration of `name` in `content`, as `(lo, hi, line)`,
/// when it is one of the shapes a handler is declared in:
///
/// - a function declaration with a body: the function's span;
/// - a binding initialised with a function: the declarator's span;
/// - a binding initialised with a call of a function that is handed exactly
///   one function, itself or through calls nested in its arguments
///   (`guarded<Body>(async (req, res) => { … })`): the declarator's span,
///   never the call's, which reads as a registration;
/// - for `default`, a default export of a function.
///
/// `None` for anything else, and for a name two module-scope declarations
/// with a body share: one of them would be a guess. A function nested in
/// another is a different binding and is never read.
///
/// A binding initialised with a METHOD call is not offered
/// (`createRouter().get("/inner", (req, res) => { … })`, `tools.guarded(…)`).
/// A registration bound to a name is written that way, and its value is the
/// router. The sidecar answers a span it reads as a handler or as a function;
/// a binding of any other value it would print as the body, and whether a
/// call's value is a function is not in the tree.
pub fn handler_declaration(file_path: &Path, content: &str, name: &str) -> Option<(u32, u32, u32)> {
    let (source_map, module) = crate::swc_scanner::parse_standalone_module(file_path, content)?;
    let mut found: Vec<swc_common::Span> = Vec::new();
    let mut other_binding = false;
    for item in &module.body {
        let decl = match item {
            ModuleItem::Stmt(Stmt::Decl(decl)) => decl,
            ModuleItem::ModuleDecl(ModuleDecl::ExportDecl(export)) => &export.decl,
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultDecl(export)) => {
                if let DefaultDecl::Fn(function) = &export.decl
                    && function.function.body.is_some()
                    && (name == DEFAULT_EXPORT
                        || function
                            .ident
                            .as_ref()
                            .is_some_and(|ident| ident.sym.as_ref() == name))
                {
                    found.push(function.function.span);
                }
                continue;
            }
            ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(export))
                if name == DEFAULT_EXPORT =>
            {
                match function_literal_span(unwrap_expr(&export.expr)) {
                    Some(span) => found.push(span),
                    None => other_binding = true,
                }
                continue;
            }
            _ => continue,
        };
        match decl {
            // An overload signature has no body and is not the function.
            Decl::Fn(function)
                if function.ident.sym.as_ref() == name && function.function.body.is_some() =>
            {
                found.push(function.function.span);
            }
            Decl::Var(var) => {
                for declarator in &var.decls {
                    if !matches!(&declarator.name, Pat::Ident(ident) if ident.id.sym.as_ref() == name)
                    {
                        continue;
                    }
                    if binds_a_handler(declarator) {
                        found.push(declarator.span);
                    } else {
                        other_binding = true;
                    }
                }
            }
            Decl::Class(class) if class.ident.sym.as_ref() == name => other_binding = true,
            _ => {}
        }
    }
    if other_binding || found.len() != 1 {
        return None;
    }
    let span = found[0];
    Some((
        span.lo.0,
        span.hi.0,
        source_map.lookup_char_pos(span.lo).line as u32,
    ))
}

/// Whether a declarator binds a function, or a call of a function that is
/// handed one.
fn binds_a_handler(declarator: &VarDeclarator) -> bool {
    let Some(init) = declarator.init.as_deref() else {
        return false;
    };
    match unwrap_expr(init) {
        value if is_function(value) => true,
        Expr::Call(call) => calls_a_function(call) && functions_handed_to(call, 0) == 1,
        _ => false,
    }
}

/// Whether a call applies a function rather than a method: its callee is a
/// name (`guarded(…)`), or a call that is one in turn (`limited(options)(…)`,
/// whose first call configured the wrapper).
fn calls_a_function(call: &CallExpr) -> bool {
    let Callee::Expr(callee) = &call.callee else {
        return false;
    };
    match unwrap_expr(callee) {
        Expr::Ident(_) => true,
        Expr::Call(inner) => calls_a_function(inner),
        _ => false,
    }
}

/// How many functions a call is handed: a function written as an argument
/// counts one, and an argument that is itself a call counts what that call is
/// handed, to [`WRAPPER_DEPTH`]. A name in an argument is not counted, since
/// the tree does not say what it binds, and neither is what a callee that is
/// itself a call was handed.
fn functions_handed_to(call: &CallExpr, depth: usize) -> usize {
    if depth >= WRAPPER_DEPTH {
        return 0;
    }
    call.args
        .iter()
        .filter(|argument| argument.spread.is_none())
        .map(|argument| match unwrap_expr(&argument.expr) {
            value if is_function(value) => 1,
            Expr::Call(inner) => functions_handed_to(inner, depth + 1),
            _ => 0,
        })
        .sum()
}

fn is_function(expr: &Expr) -> bool {
    function_literal_span(expr).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn declared(content: &str, name: &str) -> Option<String> {
        handler_declaration(&PathBuf::from("handlers.ts"), content, name).map(|(lo, hi, _)| {
            let base = crate::swc_scanner::SWC_SPAN_BASE;
            content[(lo - base) as usize..(hi - base) as usize].to_string()
        })
    }

    /// The span of the first call in `content` whose text starts with
    /// `opens`, in the scanner's numbering.
    fn call_span(content: &str, opens: &str, closes: &str) -> (u32, u32) {
        let base = crate::swc_scanner::SWC_SPAN_BASE;
        let start = content.find(opens).expect("the call is in the source");
        let end = start + content[start..].find(closes).expect("the call closes") + closes.len();
        (start as u32 + base, end as u32 + base)
    }

    fn handed(content: &str, opens: &str, closes: &str, name: &str) -> bool {
        let (lo, hi) = call_span(content, opens, closes);
        registration_is_handed(&PathBuf::from("routes.ts"), content, lo, hi, name)
    }

    #[test]
    fn a_function_declaration_is_anchored_at_the_function() {
        let content = "import { rows } from './rows';\n\nexport async function listRows(_req, res) {\n  res.json(await rows());\n}\n";
        assert_eq!(
            declared(content, "listRows").as_deref(),
            Some("async function listRows(_req, res) {\n  res.json(await rows());\n}"),
            "the span leaves the export keyword out, as a function's own span does"
        );
        assert_eq!(
            handler_declaration(&PathBuf::from("handlers.ts"), content, "listRows").map(|d| d.2),
            Some(3)
        );
    }

    #[test]
    fn a_bound_function_is_anchored_at_its_declarator() {
        let content =
            "export const showRow = async (req, res) => {\n  return res.json(req.params);\n};\n";
        assert_eq!(
            declared(content, "showRow").as_deref(),
            Some("showRow = async (req, res) => {\n  return res.json(req.params);\n}")
        );
    }

    #[test]
    fn a_binding_of_a_call_handed_one_function_is_anchored_at_its_declarator() {
        let content = "export const renameRow = guarded<Body>(async (req, res) => {\n  res.json(req.body);\n});\nexport const nested = limited(guarded(async (req, res) => { res.json(1); }));\n";
        assert_eq!(
            declared(content, "renameRow").as_deref(),
            Some("renameRow = guarded<Body>(async (req, res) => {\n  res.json(req.body);\n})"),
            "the declarator, never the call alone"
        );
        assert!(declared(content, "nested").is_some());
    }

    #[test]
    fn a_binding_that_is_no_handler_states_no_anchor() {
        let content = "\
export const fromOptions = build({ kind: 'rows' });
export const twoFunctions = both((req, res, next) => next(), async (req, res) => { res.json(1); });
export const alias = listRows;
export const table = { list: (req, res) => { res.json([]); } };
export const built = new Builder();
export class Rows {}
export const wrappedName = guarded(listRows);
export const configured = limited(options)(async (req, res) => { res.json(1); });
export const boundRegistration = createRouter().get('/inner', (req, res) => { res.json(1); });
export const methodWrapped = tools.guarded(async (req, res) => { res.json(1); });
function listRows(req, res) { res.json([]); }
";
        for name in [
            "fromOptions",
            "twoFunctions",
            "alias",
            "table",
            "built",
            "Rows",
            "wrappedName",
            "boundRegistration",
            "methodWrapped",
            "missing",
        ] {
            assert_eq!(
                declared(content, name),
                None,
                "{name} is not a handler's declaration"
            );
        }
        assert!(
            declared(content, "configured").is_some(),
            "a wrapper built by a call is still handed one function"
        );
    }

    #[test]
    fn a_name_two_declarations_share_states_no_anchor() {
        let overloads = "export function pick(a: string): string;\nexport function pick(a: number): number;\nexport function pick(a: unknown) { return a; }\n";
        assert!(
            declared(overloads, "pick").is_some(),
            "overload signatures have no body and are not counted"
        );
        let nested = "export function outer() {\n  function inner(req, res) { res.json(1); }\n  return inner;\n}\n";
        assert_eq!(
            declared(nested, "inner"),
            None,
            "a nested function is another binding"
        );
    }

    #[test]
    fn a_default_export_is_anchored_at_the_function() {
        let arrow = "export default async (req, res) => {\n  res.json({ ok: true });\n};\n";
        assert_eq!(
            declared(arrow, DEFAULT_EXPORT).as_deref(),
            Some("async (req, res) => {\n  res.json({ ok: true });\n}")
        );
        let named =
            "export default async function ping(req, res) {\n  res.json({ ok: true });\n}\n";
        assert!(declared(named, DEFAULT_EXPORT).is_some());
        assert!(declared(named, "ping").is_some());
        let call = "export default guarded(async (req, res) => { res.json(1); });\n";
        assert_eq!(
            declared(call, DEFAULT_EXPORT),
            None,
            "a default export of a call has no declarator to send"
        );
    }

    #[test]
    fn a_registration_is_handed_the_name_as_an_argument_or_a_property() {
        let content = "\
router.get('/rows', listRows);
router.post('/rows', requireKey, validate(schema), createRow);
server.route({ method: 'GET', path: '/rows/:id', handler: showRow });
server.route({ method: 'DELETE', path: '/rows/:id', removeRow });
router.put('/rows/:id', handlers.replaceRow);
router.patch('/rows/:id', build(patchRow));
";
        assert!(handed(content, "router.get(", ")", "listRows"));
        assert!(handed(content, "router.post(", "createRow)", "createRow"));
        assert!(handed(content, "router.post(", "createRow)", "requireKey"));
        assert!(!handed(content, "router.post(", "createRow)", "schema"));
        assert!(handed(
            content,
            "server.route({ method: 'GET'",
            "})",
            "showRow"
        ));
        assert!(handed(
            content,
            "server.route({ method: 'DELETE'",
            "})",
            "removeRow"
        ));
        assert!(
            !handed(content, "router.put(", "replaceRow)", "replaceRow"),
            "a member of a binding is not the binding"
        );
        assert!(
            !handed(content, "router.patch(", "(patchRow))", "patchRow"),
            "what a call returns is not the name it was handed"
        );
        assert!(
            !handed(content, "router.get(", ")", "createRow"),
            "the name belongs to another registration"
        );
    }

    #[test]
    fn a_name_in_front_of_a_function_written_in_place_is_not_the_handler() {
        let content = "\
router.get('/rows/open', requireKey, async (req, res) => { res.json([]); });
router.get('/rows/shut', async (req, res, next) => next(), listRows);
";
        assert!(
            !handed(content, "router.get('/rows/open'", "})", "requireKey"),
            "the handler is the function the call is handed after the gate"
        );
        assert!(
            handed(content, "router.get('/rows/shut'", "listRows)", "listRows"),
            "a function written in front of the name is a gate, and the name is the handler"
        );
    }

    #[test]
    fn a_chained_registration_is_read_at_its_own_span() {
        let content = "api.route('/rows').get(listRows).post(requireKey, createRow);\n";
        let whole = call_span(content, "api.route(", "createRow)");
        assert!(registration_is_handed(
            &PathBuf::from("routes.ts"),
            content,
            whole.0,
            whole.1,
            "createRow"
        ));
        let inner = call_span(content, "api.route(", "(listRows)");
        assert!(registration_is_handed(
            &PathBuf::from("routes.ts"),
            content,
            inner.0,
            inner.1,
            "listRows"
        ));
        assert!(!registration_is_handed(
            &PathBuf::from("routes.ts"),
            content,
            whole.0,
            whole.1,
            "listRows"
        ));
    }
}
