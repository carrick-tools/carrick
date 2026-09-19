//! Where a code-first GraphQL field's resolver FUNCTION sits, read from the
//! AST at the line the file-analyzer named for the field (carrick#1256).
//!
//! The model reports a root field's `resolver_line` as the line the field is
//! declared on. In a schema built in code that is the field's property
//! (`createInvoice: t.prismaField({`), and the function that actually
//! resolves it is the `resolve` property several lines further down, inside
//! the config object the field call takes. A line-only `FunctionReturn`
//! anchor at the field line cannot reach it: the sidecar binds a bare line to
//! a function starting within two lines, so it finds nothing when the
//! resolver is further away, and it finds the WRONG function when a
//! neighbour is closer: the builder callback one line above (`(t) => ({`),
//! whose return is the whole fields object, or the previous field's resolver.
//! Both answers are confidently about something else.
//!
//! This pass reads the structure instead and hands the sidecar the exact span
//! of the resolver function, which its span locator resolves to that function
//! and nothing else. The shapes are the ones GraphQL-in-JavaScript has: a
//! field property whose value is a call (or object) carrying a config object
//! with a function-valued `resolve` (graphql-js field config, and every
//! builder that mirrors it), a resolver-map property whose value IS the
//! function, a method shorthand, a named function declaration, a class method.
//! No library is named: `resolve` is the field-config vocabulary of the
//! GraphQL reference implementation, and it is used only to break a tie
//! between two function-valued properties (a `validate` beside a `resolve`).
//!
//! A field whose resolver is an identifier (`resolve: listRecentParcels`) is
//! reported as [`ResolverAnchor::Unresolvable`] so the caller sends NO
//! request: a line-only anchor there binds to a neighbour, which is worse than
//! abstaining. Following the identifier to its declaration is a follow-up.

use std::path::Path;

use swc_common::{SourceMap, Spanned, sync::Lrc};
use swc_ecma_ast::{
    ClassMethod, Expr, ExprOrSpread, FnDecl, MethodProp, ObjectLit, Prop, PropName, PropOrSpread,
    VarDeclarator,
};
use swc_ecma_visit::{Visit, VisitWith};

/// Nested calls a field value is read through (`t.field(withAuth({ … }))`).
const CALL_DEPTH: usize = 3;

/// The resolver anchor for a field at a given line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolverAnchor {
    /// A function literal resolves the field. `lo`/`hi` are its SWC span in
    /// the scanner's own units (UTF-8 bytes from
    /// [`crate::swc_scanner::SWC_SPAN_BASE`]); convert at the request
    /// boundary, never store converted (carrick#805).
    Function { lo: u32, hi: u32 },
    /// The field's structure is on the line but its resolver is not a
    /// function literal (an identifier or member, a shorthand property), or
    /// two function-valued properties compete and neither is `resolve`. The
    /// caller must send no anchor at all.
    Unresolvable,
    /// Nothing on the line reads as a field definition or a function
    /// declaration. The caller keeps whatever it did before.
    Absent,
}

/// Read the resolver anchor for the field declared on `line` (1-based) of
/// `content`. A file that does not parse yields [`ResolverAnchor::Absent`].
pub fn resolver_anchor(file_path: &Path, content: &str, line: u32) -> ResolverAnchor {
    let Some((source_map, module)) =
        crate::swc_scanner::parse_standalone_module(file_path, content)
    else {
        return ResolverAnchor::Absent;
    };
    let mut finder = AnchorFinder {
        source_map,
        line,
        from_properties: Vec::new(),
        from_declarations: Vec::new(),
    };
    module.visit_with(&mut finder);

    // A property declared on the line owns the answer: the first one that
    // resolves (or refuses) wins, in source order. The outermost property
    // comes first, so `health: t.string({ resolve: () => 'ok' })` is read from
    // `health` inward, never from the builder callback that also starts here.
    if let Some(anchor) = finder
        .from_properties
        .into_iter()
        .find(|anchor| *anchor != ResolverAnchor::Absent)
    {
        return anchor;
    }
    // Otherwise a named function declared on the line: a resolver-map entry
    // implemented as a function declaration, a `const resolveX = async () =>`
    // binding, a class method. Callbacks passed as call arguments are NOT
    // taken: the builder's `(t) => ({` callback is one, and its return is the
    // fields object, not a field.
    finder
        .from_declarations
        .into_iter()
        .next()
        .unwrap_or(ResolverAnchor::Absent)
}

struct AnchorFinder {
    source_map: Lrc<SourceMap>,
    line: u32,
    /// Resolutions of every property whose key starts on `line`, in visit
    /// (source) order.
    from_properties: Vec<ResolverAnchor>,
    /// Spans of named functions declared on `line`, in visit order.
    from_declarations: Vec<ResolverAnchor>,
}

impl AnchorFinder {
    fn starts_on_line(&self, spanned: &dyn Spanned) -> bool {
        self.source_map.lookup_char_pos(spanned.span().lo).line as u32 == self.line
    }
}

impl Visit for AnchorFinder {
    fn visit_prop(&mut self, prop: &Prop) {
        let on_line = match prop {
            Prop::KeyValue(kv) => self.starts_on_line(&kv.key),
            Prop::Method(method) => self.starts_on_line(&method.key),
            Prop::Shorthand(ident) => self.starts_on_line(ident),
            _ => false,
        };
        if on_line {
            self.from_properties.push(anchor_from_prop(prop));
        }
        prop.visit_children_with(self);
    }

    fn visit_fn_decl(&mut self, decl: &FnDecl) {
        if self.starts_on_line(&decl.ident) {
            self.from_declarations
                .push(function_span(decl.function.span));
        }
        decl.visit_children_with(self);
    }

    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        if self.starts_on_line(declarator)
            && let Some(init) = declarator.init.as_deref()
            && let Some(anchor) = function_literal(unwrap_expr(init))
        {
            self.from_declarations.push(anchor);
        }
        declarator.visit_children_with(self);
    }

    fn visit_class_method(&mut self, method: &ClassMethod) {
        if self.starts_on_line(&method.key) {
            self.from_declarations
                .push(function_span(method.function.span));
        }
        method.visit_children_with(self);
    }
}

fn function_span(span: swc_common::Span) -> ResolverAnchor {
    ResolverAnchor::Function {
        lo: span.lo.0,
        hi: span.hi.0,
    }
}

/// The function literal `expr` is, if it is one.
fn function_literal(expr: &Expr) -> Option<ResolverAnchor> {
    match expr {
        Expr::Arrow(arrow) => Some(function_span(arrow.span)),
        Expr::Fn(fn_expr) => Some(function_span(fn_expr.function.span)),
        _ => None,
    }
}

/// Strip the wrappers TypeScript lets a value wear without changing it.
fn unwrap_expr(expr: &Expr) -> &Expr {
    match expr {
        Expr::Paren(paren) => unwrap_expr(&paren.expr),
        Expr::TsAs(as_expr) => unwrap_expr(&as_expr.expr),
        Expr::TsSatisfies(satisfies) => unwrap_expr(&satisfies.expr),
        Expr::TsNonNull(non_null) => unwrap_expr(&non_null.expr),
        Expr::TsTypeAssertion(assertion) => unwrap_expr(&assertion.expr),
        Expr::TsConstAssertion(assertion) => unwrap_expr(&assertion.expr),
        other => other,
    }
}

fn anchor_from_prop(prop: &Prop) -> ResolverAnchor {
    match prop {
        Prop::KeyValue(kv) => anchor_from_value(unwrap_expr(&kv.value), CALL_DEPTH),
        Prop::Method(MethodProp { function, .. }) => function_span(function.span),
        // `{ orders, users }`: the resolver is a binding, not a literal.
        Prop::Shorthand(_) => ResolverAnchor::Unresolvable,
        _ => ResolverAnchor::Absent,
    }
}

/// Read a field property's value down to its resolver function.
fn anchor_from_value(value: &Expr, depth: usize) -> ResolverAnchor {
    if let Some(anchor) = function_literal(value) {
        return anchor;
    }
    match value {
        Expr::Object(object) => anchor_from_config(object),
        Expr::Call(call) if depth > 0 => {
            // The first argument that reads as a field config decides; a call
            // whose arguments carry no config (`t.exposeID('id')`) is not a
            // resolver-bearing field and leaves the line to its neighbours.
            call.args
                .iter()
                .map(|ExprOrSpread { expr, .. }| anchor_from_value(unwrap_expr(expr), depth - 1))
                .find(|anchor| *anchor != ResolverAnchor::Absent)
                .unwrap_or(ResolverAnchor::Absent)
        }
        // A config or resolver passed by name: structure found, function not.
        Expr::Ident(_) | Expr::Member(_) => ResolverAnchor::Unresolvable,
        _ => ResolverAnchor::Absent,
    }
}

/// The resolver inside a field config object: the `resolve` property when
/// there is one, else the sole function-valued property.
fn anchor_from_config(object: &ObjectLit) -> ResolverAnchor {
    let mut functions: Vec<ResolverAnchor> = Vec::new();
    for prop in &object.props {
        let PropOrSpread::Prop(prop) = prop else {
            continue;
        };
        let (name, value): (Option<String>, Option<ResolverAnchor>) = match &**prop {
            Prop::KeyValue(kv) => (prop_name(&kv.key), function_literal(unwrap_expr(&kv.value))),
            Prop::Method(method) => (
                prop_name(&method.key),
                Some(function_span(method.function.span)),
            ),
            Prop::Shorthand(ident) => (Some(ident.sym.to_string()), None),
            _ => (None, None),
        };
        if name.as_deref() == Some("resolve") {
            // The field names its resolver. A non-literal there is the
            // identifier case; a literal is the answer, whatever else the
            // config carries.
            return value.unwrap_or(ResolverAnchor::Unresolvable);
        }
        if let Some(anchor) = value {
            functions.push(anchor);
        }
    }
    match functions.len() {
        0 => ResolverAnchor::Absent,
        1 => functions.remove(0),
        _ => ResolverAnchor::Unresolvable,
    }
}

fn prop_name(name: &PropName) -> Option<String> {
    match name {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(string) => Some(string.value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn at(content: &str, line: u32) -> ResolverAnchor {
        resolver_anchor(&PathBuf::from("schema.ts"), content, line)
    }

    /// The bytes an anchor names, so a test reads the function it points at.
    fn text<'a>(content: &'a str, anchor: &ResolverAnchor) -> &'a str {
        let ResolverAnchor::Function { lo, hi } = anchor else {
            panic!("expected a function anchor, got {anchor:?}");
        };
        let base = crate::swc_scanner::SWC_SPAN_BASE as usize;
        &content[*lo as usize - base..*hi as usize - base]
    }

    const BUILDER_FIELDS: &str = "\
import { kit } from '../kit.ts';
import { findParcel, listParcels, listRecentParcels } from './store.ts';

// Colis — lecture (multi-byte text above every site, so byte and UTF-16 units differ below)
kit.queryFields((t) => ({
  parcels: t.field({
    type: ['Parcel'],
    resolve: () => listParcels(),
  }),
  parcel: t.field({
    type: 'Parcel',
    nullable: true,
    args: { id: t.arg.id({ required: true }) },
    resolve: async (_root, args) => findParcel(String(args.id)),
  }),
  heaviestParcel: t.field({
    type: 'Parcel',
    validate: (_args: unknown) => true,
    resolve: () => listParcels()[0],
  }),
  recentParcels: t.field({
    type: ['Parcel'],
    resolve: listRecentParcels,
  }),
}));
";

    /// The field line names the field; the anchor is the `resolve` arrow
    /// several lines below it, in BYTE units (the em dash above shifts UTF-16
    /// positions, and this span must not move with them).
    #[test]
    fn a_field_property_anchors_its_resolve_function() {
        let anchor = at(BUILDER_FIELDS, 6);
        assert_eq!(text(BUILDER_FIELDS, &anchor), "() => listParcels()");
        let anchor = at(BUILDER_FIELDS, 10);
        assert_eq!(
            text(BUILDER_FIELDS, &anchor),
            "async (_root, args) => findParcel(String(args.id))",
            "an async arrow's span starts at `async`"
        );
    }

    /// Two function-valued properties: `resolve` is the resolver, whatever
    /// the other is called.
    #[test]
    fn resolve_wins_over_another_function_valued_property() {
        let anchor = at(BUILDER_FIELDS, 16);
        assert_eq!(text(BUILDER_FIELDS, &anchor), "() => listParcels()[0]");
    }

    /// A resolver passed by name is structure the pass can see but not a
    /// function it can span: the caller must send nothing.
    #[test]
    fn an_identifier_resolver_is_unresolvable() {
        assert_eq!(at(BUILDER_FIELDS, 21), ResolverAnchor::Unresolvable);
    }

    /// The model pointed at the `resolve:` line itself.
    #[test]
    fn the_resolve_line_anchors_the_same_function() {
        let anchor = at(BUILDER_FIELDS, 8);
        assert_eq!(text(BUILDER_FIELDS, &anchor), "() => listParcels()");
    }

    /// The builder callback starts on line 5 and is a call argument: it is
    /// not a resolver, and a line with no field property and no declaration
    /// leaves the caller its line-only anchor.
    #[test]
    fn a_builder_callback_line_is_absent() {
        assert_eq!(at(BUILDER_FIELDS, 5), ResolverAnchor::Absent);
        assert_eq!(at(BUILDER_FIELDS, 3), ResolverAnchor::Absent);
    }

    /// `type: ['Parcel']` is a property on its line whose value is a literal:
    /// not a field definition, so the line-only anchor is kept.
    #[test]
    fn a_config_literal_line_is_absent() {
        assert_eq!(at(BUILDER_FIELDS, 7), ResolverAnchor::Absent);
    }

    /// Same-line resolver: the outermost property on the line is read inward
    /// to the resolver, not to the builder callback that also starts there.
    #[test]
    fn a_one_line_field_reads_inward_to_its_resolver() {
        let content = "\
kit.queryType({
  fields: (t) => ({
    health: t.string({ resolve: () => 'ok' }),
  }),
});
";
        assert_eq!(text(content, &at(content, 3)), "() => 'ok'");
    }

    /// graphql-js field config: an object value with a method shorthand, and
    /// a resolver map whose property value IS the function.
    #[test]
    fn object_configs_and_resolver_maps_anchor_their_functions() {
        let content = "\
const Query = new GraphQLObjectType({
  name: 'Query',
  fields: {
    orders: {
      type: new GraphQLList(Order),
      resolve(_parent, _args, ctx) { return ctx.orders; },
    },
  },
});
const resolvers = {
  Query: {
    users: async (_parent: unknown, _args: unknown, ctx: Ctx) => ctx.users(),
    accounts,
  },
};
";
        assert_eq!(
            text(content, &at(content, 4)),
            "resolve(_parent, _args, ctx) { return ctx.orders; }",
            "a method shorthand's function span carries its name"
        );
        assert_eq!(
            text(content, &at(content, 12)),
            "async (_parent: unknown, _args: unknown, ctx: Ctx) => ctx.users()"
        );
        assert_eq!(at(content, 13), ResolverAnchor::Unresolvable);
    }

    /// Named resolvers: a function declaration, an arrow binding, a class
    /// method. Each is anchored at its own function.
    #[test]
    fn declared_functions_on_the_line_are_anchored() {
        let content = "\
export async function orders(): Promise<Order[]> {
  return [];
}
export const users = async (): Promise<User[]> => [];
class OrderResolver {
  @Query(() => [Order])
  orders(): Order[] {
    return [];
  }
}
";
        assert_eq!(
            text(content, &at(content, 1)),
            "async function orders(): Promise<Order[]> {\n  return [];\n}",
            "a declaration's function span carries the keyword and name"
        );
        assert_eq!(
            text(content, &at(content, 4)),
            "async (): Promise<User[]> => []"
        );
        assert_eq!(
            text(content, &at(content, 7)),
            "@Query(() => [Order])\n  orders(): Order[] {\n    return [];\n  }",
            "a class method's span carries its decorators, as the sidecar's node does"
        );
    }

    /// A file that does not parse states nothing.
    #[test]
    fn an_unparseable_file_is_absent() {
        assert_eq!(at("const = ;;; {{{", 1), ResolverAnchor::Absent);
    }
}
