//! Request targets built with the `URL` constructor's path-plus-base form.
//!
//! `new URL(path, base)` is how a client assembles a request URL when the
//! origin comes from configuration and the path is its own:
//!
//! ```ignore
//! async findThings(query?: string) {
//!   const url = new URL("/api/v2/things", this.baseUrl);
//!   if (query) {
//!     url.searchParams.append("q", query);
//!   }
//!   return send(Schema, url.href, { headers: { … } });
//! }
//! ```
//!
//! Nothing deterministic read that first argument, and the shape hides the
//! path from every signal that looks at the request itself: the argument the
//! request receives is a binding (or a `.href` off one), which is not
//! route-shaped, so the site either states no path at all or states whatever
//! extraction reached for. A neighbouring method on an older API version is
//! enough to make that a plausible wrong answer, and in one measured index it
//! was: a client whose only call on a route was to `v2` was recorded calling
//! `v1`, a string that appears nowhere in the package.
//!
//! This pass records the path, and only the path. The base is left alone
//! deliberately. It is an opaque value here (a field, a parameter, an env
//! read), and asserting a host the source does not state is the failure this
//! module exists to stop. A path with no base is exactly what a host-free call
//! already is everywhere else in the scanner, and it matches by route path.
//!
//! What is read is narrow, and narrow towards saying nothing:
//!
//! - The callee must be the bare identifier `URL`. A local class of that name
//!   would shadow the built-in and be read as one, which is the same bet every
//!   other web-platform shape in the scanner takes (`fetch`, `navigator`).
//! - Exactly two arguments. One argument states a whole URL rather than a path
//!   plus a base, and neither says anything a base could be appended to.
//! - The first argument must be a string literal, or a template whose source
//!   text is kept verbatim so an interpolated segment stays a path parameter.
//! - It must start with `/`. `new URL("things", base)` is resolved against the
//!   base's own path, which is opaque, so a relative first argument states no
//!   path at all.
//!
//! The value reaches the request either directly, or through a binding
//! declared in the same function (or an enclosing one). Scope is tracked with
//! a stack rather than a flat name map, because one client class declares
//! `const url = new URL(…)` in every method it has, and a flat map keyed on
//! `url` would collide on every one of them.

use std::collections::HashMap;
use swc_common::{SourceMap, SourceMapper, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

/// Maps the start byte offset of a call expression to the path stated by the
/// `new URL(path, base)` supplying its target.
///
/// Keyed on the span rather than the target text so the join is to the call
/// site the scanner saw, not to whatever extraction wrote down for it.
pub type NewUrlPathMap = HashMap<u32, String>;

/// Collect every call whose target is built with `new URL(path, base)`.
pub fn collect_new_url_paths(module: &Module, source_map: &Lrc<SourceMap>) -> NewUrlPathMap {
    let mut collector = Collector {
        source_map,
        scopes: vec![HashMap::new()],
        paths: HashMap::new(),
    };
    module.visit_with(&mut collector);
    collector.paths
}

struct Collector<'a> {
    source_map: &'a Lrc<SourceMap>,
    /// One frame per enclosing function, outermost (module) first. Each maps a
    /// binding name to the path the `new URL` it was declared from states.
    scopes: Vec<HashMap<String, String>>,
    paths: NewUrlPathMap,
}

impl Collector<'_> {
    fn scoped<F: FnOnce(&mut Self)>(&mut self, visit: F) {
        self.scopes.push(HashMap::new());
        visit(self);
        self.scopes.pop();
    }

    /// The path an expression handed to a request states, if any: a `new URL`
    /// written inline, or a binding holding one, read directly or through the
    /// accessors a `URL` is normally turned back into a string with.
    fn stated_path(&self, expr: &Expr) -> Option<String> {
        match unwrap_transparent(expr) {
            Expr::New(new_expr) => self.new_url_path(new_expr),
            Expr::Ident(ident) => self.lookup(ident.sym.as_ref()),
            // `url.href`.
            Expr::Member(member) if is_prop(&member.prop, "href") => self.stated_path(&member.obj),
            Expr::Call(call) => match &call.callee {
                // `url.toString()`.
                Callee::Expr(callee) => match &**callee {
                    Expr::Member(member) if is_prop(&member.prop, "toString") => call
                        .args
                        .is_empty()
                        .then(|| self.stated_path(&member.obj))?,
                    // `String(url)`.
                    Expr::Ident(ident) if ident.sym.as_ref() == "String" => {
                        let arg = call.args.first().filter(|arg| arg.spread.is_none())?;
                        (call.args.len() == 1).then(|| self.stated_path(&arg.expr))?
                    }
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        }
    }

    fn lookup(&self, name: &str) -> Option<String> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name))
            .cloned()
    }

    /// The absolute path `new URL(path, base)` states in its first argument.
    fn new_url_path(&self, new_expr: &NewExpr) -> Option<String> {
        let Expr::Ident(callee) = &*new_expr.callee else {
            return None;
        };
        if callee.sym.as_ref() != "URL" {
            return None;
        }
        // Exactly two arguments: a path and the base it is resolved against.
        let args = new_expr.args.as_ref()?;
        if args.len() != 2 || args.iter().any(|arg| arg.spread.is_some()) {
            return None;
        }
        let path = self.argument_text(&args[0].expr)?;
        path.starts_with('/').then_some(path)
    }

    /// A string argument's value, or a template's source with its
    /// interpolations left intact so an interpolated segment survives as a
    /// path parameter.
    fn argument_text(&self, expr: &Expr) -> Option<String> {
        match unwrap_transparent(expr) {
            Expr::Lit(Lit::Str(literal)) => Some(literal.value.to_string()),
            Expr::Tpl(tpl) => {
                let snippet = self.source_map.span_to_snippet(tpl.span).ok()?;
                let trimmed = snippet.trim();
                trimmed
                    .strip_prefix('`')
                    .and_then(|rest| rest.strip_suffix('`'))
                    .map(str::to_string)
            }
            _ => None,
        }
    }
}

impl Visit for Collector<'_> {
    fn visit_function(&mut self, node: &Function) {
        self.scoped(|this| node.visit_children_with(this));
    }

    fn visit_arrow_expr(&mut self, node: &ArrowExpr) {
        self.scoped(|this| node.visit_children_with(this));
    }

    fn visit_constructor(&mut self, node: &Constructor) {
        self.scoped(|this| node.visit_children_with(this));
    }

    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        if let Pat::Ident(binding) = &node.name
            && let Some(init) = &node.init
            && let Some(path) = self.stated_path(init)
        {
            let frame = self
                .scopes
                .last_mut()
                .expect("the module frame is never popped");
            frame.insert(binding.id.sym.to_string(), path);
        }
        node.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, node: &CallExpr) {
        // `String(url)` takes the URL as an argument but is a conversion, not
        // a request. Recording it would put a row on a span no candidate ever
        // joins to; the request wrapping it is recorded on its own span.
        if !is_string_conversion(node) {
            // The URL sits at whatever argument position the callee puts it
            // in: a request helper routinely takes a schema or a client first.
            for arg in &node.args {
                if arg.spread.is_some() {
                    continue;
                }
                if let Some(path) = self.stated_path(&arg.expr) {
                    self.paths.insert(node.span.lo.0, path);
                    break;
                }
            }
        }
        node.visit_children_with(self);
    }
}

/// Look through the wrappers that change nothing about the value.
fn unwrap_transparent(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(paren) => unwrap_transparent(&paren.expr),
        Expr::TsAs(as_expr) => unwrap_transparent(&as_expr.expr),
        Expr::TsNonNull(non_null) => unwrap_transparent(&non_null.expr),
        Expr::TsConstAssertion(assertion) => unwrap_transparent(&assertion.expr),
        other => other,
    }
}

/// Is this call `String(x)`, the conversion rather than a request?
fn is_string_conversion(call: &CallExpr) -> bool {
    matches!(
        &call.callee,
        Callee::Expr(callee) if matches!(&**callee, Expr::Ident(ident) if ident.sym.as_ref() == "String")
    )
}

fn is_prop(prop: &MemberProp, name: &str) -> bool {
    matches!(prop, MemberProp::Ident(ident) if ident.sym.as_ref() == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_file;
    use swc_common::errors::{ColorConfig, Handler};

    /// The paths collected from `source`, in source order.
    fn paths(source: &str) -> Vec<String> {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = tmp_dir.path().join("input.ts");
        std::fs::write(&file_path, source).expect("write file");

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));
        let module = parse_file(&file_path, &cm, &handler).expect("parsed module");

        let collected = collect_new_url_paths(&module, &cm);
        let mut rows: Vec<(u32, String)> = collected.into_iter().collect();
        rows.sort_by_key(|(span, _)| *span);
        rows.into_iter().map(|(_, path)| path).collect()
    }

    #[test]
    fn the_url_object_can_be_the_request_argument_itself() {
        assert_eq!(
            paths(r#"fetch(new URL("/api/v2/things", base), { method: "GET" });"#),
            vec!["/api/v2/things"]
        );
    }

    #[test]
    fn a_binding_reaches_the_request_directly_and_through_its_accessors() {
        let source = r#"
            async function run(base: string) {
              const a = new URL("/api/v2/a", base);
              await fetch(a);
              const b = new URL("/api/v2/b", base);
              await fetch(b.href);
              const c = new URL("/api/v2/c", base);
              await fetch(c.toString());
              const d = new URL("/api/v2/d", base);
              await fetch(String(d));
            }
        "#;
        assert_eq!(
            paths(source),
            vec!["/api/v2/a", "/api/v2/b", "/api/v2/c", "/api/v2/d"]
        );
    }

    #[test]
    fn the_url_may_sit_at_any_argument_position() {
        let source = r#"
            function run(base: string) {
              const url = new URL("/api/v2/things", base);
              return send(Schema, url.href, { headers: {} });
            }
        "#;
        assert_eq!(paths(source), vec!["/api/v2/things"]);
    }

    #[test]
    fn a_template_keeps_its_interpolated_segment() {
        let source = r#"
            function run(base: string, id: string) {
              const url = new URL(`/api/v2/things/${id}/archive`, base);
              return fetch(url, { method: "POST" });
            }
        "#;
        assert_eq!(paths(source), vec!["/api/v2/things/${id}/archive"]);
    }

    #[test]
    fn one_name_per_method_does_not_collide_across_methods() {
        let source = r#"
            class Client {
              constructor(private readonly base: string) {}
              async first() {
                const url = new URL("/api/v2/first", this.base);
                return fetch(url.href);
              }
              async second() {
                const url = new URL("/api/v2/second", this.base);
                return fetch(url.href);
              }
              async third() {
                return fetch(url.href);
              }
            }
        "#;
        // Two rows, each carrying its own method's path, and nothing for the
        // third: its `url` is declared in neither its own scope nor an
        // enclosing one.
        assert_eq!(paths(source), vec!["/api/v2/first", "/api/v2/second"]);
    }

    #[test]
    fn a_relative_first_argument_states_no_path() {
        // Resolved against whatever path the base carries, which is opaque.
        assert_eq!(
            paths(r#"fetch(new URL("things", base));"#),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_whole_url_in_one_argument_is_not_a_path_plus_a_base() {
        assert_eq!(
            paths(r#"fetch(new URL(configuredEndpoint));"#),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_computed_first_argument_states_nothing() {
        assert_eq!(
            paths(r#"fetch(new URL(pathFor(kind), base));"#),
            Vec::<String>::new()
        );
    }

    #[test]
    fn another_constructor_of_two_arguments_is_not_a_url() {
        assert_eq!(
            paths(r#"send(new Request("/api/v2/things", init));"#),
            Vec::<String>::new()
        );
    }
}
