//! Registration scopes: routes registered inside an inline plugin
//! (carrick#2092).
//!
//! `app.register(async (api) => { api.register(items) }, { prefix: '/api/v1' })`
//! hands the callback a fresh instance, `api`, whose routes are served under
//! the prefix the registration states. The model reads the rows inside the
//! callback as hanging from `api`, a name every inline plugin in a repo
//! shares, so two plugins collided on one graph node and the prefix was
//! either lost or joined as raw source text (`opts.prefix ?? '/api/v1'`).
//!
//! This pass gives each such callback parameter an identity of its own, the
//! scope id `api@<file>:<line>`, and states the registration's prefix only
//! where the source states it as a literal:
//!
//! * phase 1 ([`collect_registration_sites`]) reads every call that passes one
//!   inline function, with the value each other argument reads to;
//! * after the join ([`apply_registration_scopes`]) the rows hanging from the
//!   callback parameter are re-pointed to the scope id, and the registration
//!   row is corrected or emitted.
//!
//! A prefix the source does not state as a literal is recorded as unread and
//! never guessed. Nothing here reads a framework, a library or an option name.

use std::collections::HashMap;

use swc_common::{SourceMap, SourceMapper, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};
use tracing::debug;

use crate::agents::file_analyzer_agent::{FileAnalysisResult, MountResult};
use crate::binding_scope::{BindingKey, ident_key};
use crate::graphql_document_sites::unwrap_expression;
use crate::request_summary::literal_specifier;

/// How many bindings a prefix read may follow before it gives up.
const MAX_BINDING_DEPTH: usize = 4;

/// What a prefix expression reads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefixRead {
    /// The literal values the expression can take: one, or two for a
    /// conditional whose branches both read.
    Stated(Vec<String>),
    /// The source does not state the value as a literal.
    Unread,
}

/// One non-function argument value of a registration call: the argument
/// itself, or one property value of an object-literal argument.
#[derive(Debug, Clone)]
pub struct OptionValue {
    /// The value's source text, whitespace removed and quotes made one kind,
    /// so the model's copy of it can be found ([`normalise_source_text`]).
    pub text: String,
    pub read: PrefixRead,
}

/// A call `R.m(…)` that passes exactly one inline function, read in phase 1.
#[derive(Debug, Clone)]
pub struct RegistrationSite {
    /// The call's start line.
    pub line: u32,
    /// `R`: the instance the registration is made on.
    pub receiver: String,
    /// `m`, kept for the emitted row's `pattern_matched`.
    pub method: String,
    /// The inline function's first parameter: the instance it is handed.
    pub param: String,
    /// The inline function's first and last lines.
    pub body_start_line: u32,
    pub body_end_line: u32,
    /// Every other argument value, in source order.
    pub option_values: Vec<OptionValue>,
    /// The source text of the first other argument, quoted when the prefix
    /// it may carry is recorded as unread.
    pub options_text: Option<String>,
}

/// A registration scope whose prefix the source does not state as a literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadPrefix {
    /// `<repo-relative file>:<line>` of the registration call.
    pub site: String,
    pub scope_id: String,
    /// The expression the prefix may be in, as written.
    pub expression: String,
}

/// The graph node a registration scope stands for. An identifier never holds
/// `@`, so a scope id cannot collide with a binding name.
pub fn scope_id(param: &str, file: &str, line: u32) -> String {
    format!("{param}@{file}:{line}")
}

/// Whether a graph node is a registration scope, so its owner is already
/// resolved and no name-keyed lookup may override it.
pub fn is_scope_id(node: &str) -> bool {
    node.contains('@')
        && node
            .split('@')
            .next()
            .is_some_and(|name| !name.is_empty() && !name.contains(['/', ' ', '.']))
}

/// Source text as the model may copy it: whitespace removed, `"` read as `'`.
pub fn normalise_source_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| if c == '"' { '\'' } else { c })
        .collect()
}

/// Whether a model's `mount_path` is already a path rather than source text.
fn is_route_literal(text: &str) -> bool {
    text.is_empty() || is_route_path(text) && !text.contains(['\'', '"', '`', '?', '|'])
}

/// A value a route prefix can be: empty, or `/`-led with no whitespace and
/// no scheme.
fn is_route_path(value: &str) -> bool {
    value.is_empty()
        || (value.starts_with('/')
            && !value.contains("://")
            && !value.chars().any(char::is_whitespace))
}

/// A path a route is served at: `/`-led. A row whose path is anything else
/// (a header name read off a request, a content type) registers nothing, so
/// it cannot make a callback a scope.
fn is_served_path(path: &str) -> bool {
    path.starts_with('/')
}

/// Every registration site in a module. `source_map` must be the one the
/// module was parsed with, and the module must have been through the
/// resolver: bindings are read by scope, never by name.
pub fn collect_registration_sites(
    module: &Module,
    source_map: &Lrc<SourceMap>,
) -> Vec<RegistrationSite> {
    // The declarations are collected only for a module that has a site: most
    // files register nothing inline.
    let mut probe = SiteFinder {
        source_map,
        values: None,
        sites: Vec::new(),
    };
    module.visit_with(&mut probe);
    if probe.sites.is_empty() {
        return Vec::new();
    }
    let mut values = DeclaredValues::default();
    module.visit_with(&mut values);
    let mut finder = SiteFinder {
        source_map,
        values: Some(&values),
        sites: Vec::new(),
    };
    module.visit_with(&mut finder);
    finder.sites
}

/// Finds the sites. Without `values` it only finds them (their arguments
/// read as unread); with them it reads each argument value.
struct SiteFinder<'s> {
    source_map: &'s Lrc<SourceMap>,
    values: Option<&'s DeclaredValues>,
    sites: Vec<RegistrationSite>,
}

impl SiteFinder<'_> {
    fn line(&self, pos: swc_common::BytePos) -> u32 {
        u32::try_from(self.source_map.lookup_char_pos(pos).line).unwrap_or(0)
    }

    fn snippet(&self, expr: &Expr) -> String {
        self.source_map
            .span_to_snippet(expr.span())
            .unwrap_or_default()
    }

    fn option_value(&self, expr: &Expr) -> OptionValue {
        OptionValue {
            text: normalise_source_text(&self.snippet(expr)),
            read: match self.values {
                Some(values) => read_prefix(expr, values, 0),
                None => PrefixRead::Unread,
            },
        }
    }

    fn consider(&mut self, call: &CallExpr) {
        let Callee::Expr(callee) = &call.callee else {
            return;
        };
        let Expr::Member(member) = unwrap_expression(callee) else {
            return;
        };
        let Expr::Ident(receiver) = unwrap_expression(&member.obj) else {
            return;
        };
        let MemberProp::Ident(method) = &member.prop else {
            return;
        };
        let mut inline: Vec<(swc_common::Span, String)> = Vec::new();
        let mut others: Vec<&Expr> = Vec::new();
        for arg in &call.args {
            if arg.spread.is_some() {
                continue;
            }
            let first_param = match unwrap_expression(&arg.expr) {
                Expr::Arrow(arrow) => Some(arrow.params.first()),
                Expr::Fn(function) => Some(function.function.params.first().map(|p| &p.pat)),
                _ => None,
            };
            match first_param {
                // An inline function with no plain first parameter hands out
                // no instance this pass can name.
                Some(Some(Pat::Ident(param))) => {
                    inline.push((arg.expr.span(), param.id.sym.to_string()))
                }
                Some(_) => return,
                None => others.push(&arg.expr),
            }
        }
        let [(span, param)] = inline.as_slice() else {
            return;
        };
        let mut option_values = Vec::new();
        for expr in &others {
            match unwrap_expression(expr) {
                Expr::Object(object) => {
                    for prop in &object.props {
                        let PropOrSpread::Prop(prop) = prop else {
                            continue; // a spread names no value of its own
                        };
                        match prop.as_ref() {
                            Prop::KeyValue(kv) => option_values.push(self.option_value(&kv.value)),
                            Prop::Shorthand(ident) => {
                                option_values.push(self.option_value(&Expr::Ident(ident.clone())))
                            }
                            _ => {}
                        }
                    }
                }
                other => option_values.push(self.option_value(other)),
            }
        }
        self.sites.push(RegistrationSite {
            line: self.line(call.span.lo),
            receiver: receiver.sym.to_string(),
            method: method.sym.to_string(),
            param: param.clone(),
            body_start_line: self.line(span.lo),
            body_end_line: self.line(span.hi),
            option_values,
            options_text: others.first().map(|expr| self.snippet(expr)),
        });
    }
}

impl Visit for SiteFinder<'_> {
    fn visit_call_expr(&mut self, call: &CallExpr) {
        self.consider(call);
        call.visit_children_with(self);
    }
}

/// What each binding a prefix can be read through holds: a `const`
/// initialiser, a parameter default, or a destructuring default. Keyed by the
/// binding's scope, never its name.
#[derive(Default)]
struct DeclaredValues {
    values: HashMap<BindingKey, Expr>,
}

impl DeclaredValues {
    /// Record the written defaults inside a pattern. `init` is the value the
    /// whole pattern is bound to, read only when the pattern is one name.
    fn bind(&mut self, pat: &Pat, init: Option<&Expr>) {
        match pat {
            Pat::Ident(ident) => {
                if let Some(init) = init {
                    self.values.insert(ident_key(&ident.id), init.clone());
                }
            }
            // `name = '/x'` or `{ name = '/x' } = {}`: the right side is the
            // written default.
            Pat::Assign(assign) => self.bind(&assign.left, Some(&assign.right)),
            Pat::Object(object) => {
                for prop in &object.props {
                    match prop {
                        ObjectPatProp::Assign(assign) => {
                            if let Some(default) = &assign.value {
                                self.values
                                    .insert(ident_key(&assign.key.id), (**default).clone());
                            }
                        }
                        ObjectPatProp::KeyValue(kv) => self.bind(&kv.value, None),
                        ObjectPatProp::Rest(_) => {}
                    }
                }
            }
            _ => {}
        }
    }
}

impl Visit for DeclaredValues {
    fn visit_var_decl(&mut self, decl: &VarDecl) {
        if decl.kind == VarDeclKind::Const {
            for declarator in &decl.decls {
                self.bind(&declarator.name, declarator.init.as_deref());
            }
        }
        decl.visit_children_with(self);
    }

    fn visit_param(&mut self, param: &Param) {
        self.bind_param(&param.pat);
        param.visit_children_with(self);
    }

    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        for param in &arrow.params {
            self.bind_param(param);
        }
        arrow.visit_children_with(self);
    }
}

impl DeclaredValues {
    /// A parameter states a value only through a default: a plain name holds
    /// whatever the caller passes.
    fn bind_param(&mut self, pat: &Pat) {
        if !matches!(pat, Pat::Ident(_)) {
            self.bind(pat, None);
        }
    }
}

/// Read a prefix expression to the literal values the source states for it.
///
/// * a string literal, or a template with no substitution, is its value;
/// * `a ?? b` and `a || b` read `a`, else the written default `b`;
/// * `c ? a : b` reads both values when both branches read;
/// * an identifier reads through the declaration its scope names (a `const`
///   initialiser or a written default), at most [`MAX_BINDING_DEPTH`] deep;
/// * anything else, and any value that is not a route path, is unread.
fn read_prefix(expr: &Expr, values: &DeclaredValues, depth: usize) -> PrefixRead {
    let expr = unwrap_expression(expr);
    if let Some(value) = literal_specifier(expr) {
        return if is_route_path(&value) {
            PrefixRead::Stated(vec![value])
        } else {
            PrefixRead::Unread
        };
    }
    match expr {
        Expr::Bin(bin) if matches!(bin.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr) => {
            match read_prefix(&bin.left, values, depth) {
                PrefixRead::Unread => read_prefix(&bin.right, values, depth),
                stated => stated,
            }
        }
        Expr::Cond(cond) => match (
            read_prefix(&cond.cons, values, depth),
            read_prefix(&cond.alt, values, depth),
        ) {
            (PrefixRead::Stated(mut cons), PrefixRead::Stated(alt)) => {
                for value in alt {
                    if !cons.contains(&value) {
                        cons.push(value);
                    }
                }
                if cons.len() <= 2 {
                    PrefixRead::Stated(cons)
                } else {
                    PrefixRead::Unread
                }
            }
            _ => PrefixRead::Unread,
        },
        Expr::Ident(ident) if depth < MAX_BINDING_DEPTH => {
            match values.values.get(&ident_key(ident)) {
                Some(init) => read_prefix(init, values, depth + 1),
                None => PrefixRead::Unread,
            }
        }
        _ => PrefixRead::Unread,
    }
}

/// Give every registration scope in one file's joined rows its own node, and
/// state its prefix where the source does. Returns the scopes whose prefix
/// the source does not state, each logged once.
///
/// `file` is the repo-relative path the index stores, never an absolute one:
/// the scope id is part of the graph every reader sees.
pub fn apply_registration_scopes(
    result: &mut FileAnalysisResult,
    sites: &[RegistrationSite],
    file: &str,
) -> Vec<UnreadPrefix> {
    let mut unread = Vec::new();
    if sites.is_empty() {
        return unread;
    }
    // Innermost first, so a nested scope that rebinds the parameter's name
    // claims its own rows before the scope around it.
    let mut order: Vec<&RegistrationSite> = sites.iter().collect();
    order.sort_by_key(|site| (site.body_end_line - site.body_start_line, site.line));

    let mut mount_claimed = vec![false; result.mounts.len()];
    let mut registration_rows = vec![false; result.mounts.len()];
    let mut endpoint_claimed = vec![false; result.endpoints.len()];
    // Rows to append once every scope is read, and rows to replace.
    let mut emitted: Vec<MountResult> = Vec::new();
    let mut fanned: Vec<(usize, Vec<String>)> = Vec::new();

    for site in order {
        let inside = |line: i32| {
            u32::try_from(line)
                .is_ok_and(|line| line >= site.body_start_line && line <= site.body_end_line)
        };
        // A call that itself registers a route hands its callback a request,
        // not an instance: `router.get('/me', async (c) => c.get('user'))`.
        if result.endpoints.iter().any(|endpoint| {
            u32::try_from(endpoint.line_number) == Ok(site.line)
                && endpoint.owner_node == site.receiver
                && is_served_path(&endpoint.path)
        }) {
            continue;
        }
        let registrations: Vec<usize> = result
            .mounts
            .iter()
            .enumerate()
            .filter(|(index, mount)| {
                !registration_rows[*index]
                    && u32::try_from(mount.line_number) == Ok(site.line)
                    && mount.parent_node == site.receiver
            })
            .map(|(index, _)| index)
            .collect();
        let hanging_mounts: Vec<usize> = result
            .mounts
            .iter()
            .enumerate()
            .filter(|(index, mount)| {
                !mount_claimed[*index]
                    && !registrations.contains(index)
                    && mount.parent_node == site.param
                    && inside(mount.line_number)
            })
            .map(|(index, _)| index)
            .collect();
        let hanging_endpoints: Vec<usize> = result
            .endpoints
            .iter()
            .enumerate()
            .filter(|(index, endpoint)| {
                !endpoint_claimed[*index]
                    && endpoint.owner_node == site.param
                    && is_served_path(&endpoint.path)
                    && inside(endpoint.line_number)
            })
            .map(|(index, _)| index)
            .collect();
        if hanging_mounts.is_empty() && hanging_endpoints.is_empty() {
            continue; // a callback that heads no rows is not a scope
        }

        let id = scope_id(&site.param, file, site.line);
        for index in hanging_mounts {
            mount_claimed[index] = true;
            result.mounts[index].parent_node = id.clone();
        }
        for index in hanging_endpoints {
            endpoint_claimed[index] = true;
            result.endpoints[index].owner_node = id.clone();
        }

        let site_text = format!("{file}:{}", site.line);
        if registrations.is_empty() {
            // The model stated no row for the registration: the scope still
            // hangs from the instance it was registered on.
            emitted.push(MountResult {
                line_number: i32::try_from(site.line).unwrap_or(i32::MAX),
                parent_node: site.receiver.clone(),
                child_node: id.clone(),
                mount_path: String::new(),
                import_source: None,
                pattern_matched: format!("{}.{}(", site.receiver, site.method),
            });
            if let Some(text) = &site.options_text {
                unread.push(UnreadPrefix {
                    site: site_text.clone(),
                    scope_id: id.clone(),
                    expression: text.clone(),
                });
            }
            continue;
        }
        for index in registrations {
            registration_rows[index] = true;
            let mount = &mut result.mounts[index];
            mount.child_node = id.clone();
            if is_route_literal(&mount.mount_path) {
                continue;
            }
            // The model wrote the prefix as source text: read the argument
            // value it names.
            let wanted = normalise_source_text(&mount.mount_path);
            let read = site
                .option_values
                .iter()
                .find(|value| value.text == wanted)
                .map(|value| value.read.clone())
                .unwrap_or(PrefixRead::Unread);
            match read {
                PrefixRead::Stated(values) => fanned.push((index, values)),
                PrefixRead::Unread => {
                    unread.push(UnreadPrefix {
                        site: site_text.clone(),
                        scope_id: id.clone(),
                        expression: mount.mount_path.clone(),
                    });
                    mount.mount_path = String::new();
                }
            }
        }
    }

    // A prefix read to two values serves the scope under each of them.
    for (index, values) in fanned {
        let mut values = values.into_iter();
        let Some(first) = values.next() else {
            continue;
        };
        for value in values {
            let mut copy = result.mounts[index].clone();
            copy.mount_path = value;
            emitted.push(copy);
        }
        result.mounts[index].mount_path = first;
    }
    result.mounts.extend(emitted);

    for record in &unread {
        debug!(
            "Mount prefix unread at {}: '{}' is not a literal the source states; scope {} keeps \
             no prefix",
            record.site, record.expression, record.scope_id
        );
    }
    unread
}

#[cfg(test)]
mod tests {
    use super::*;
    use swc_common::{
        FileName, GLOBALS, Globals, Mark,
        errors::{ColorConfig, Handler},
    };
    use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
    use swc_ecma_transforms_base::resolver;
    use swc_ecma_visit::VisitMutWith;

    fn sites(source: &str) -> Vec<RegistrationSite> {
        GLOBALS.set(&Globals::new(), || {
            let cm: Lrc<SourceMap> = Default::default();
            let _handler =
                Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));
            let fm = cm.new_source_file(
                Lrc::new(FileName::Custom("t.ts".into())),
                source.to_string(),
            );
            let lexer = Lexer::new(
                Syntax::Typescript(TsSyntax::default()),
                Default::default(),
                StringInput::from(&*fm),
                None,
            );
            let mut module = Parser::new_from(lexer).parse_module().expect("parses");
            module.visit_mut_with(&mut resolver(Mark::new(), Mark::new(), true));
            collect_registration_sites(&module, &cm)
        })
    }

    fn read_of(source: &str) -> PrefixRead {
        let found = sites(source);
        let [site] = found.as_slice() else {
            panic!("one site, got {found:?}");
        };
        site.option_values
            .iter()
            .find(|value| value.text.contains("prefix") || value.read != PrefixRead::Unread)
            .map(|value| value.read.clone())
            .unwrap_or(PrefixRead::Unread)
    }

    fn stated(values: &[&str]) -> PrefixRead {
        PrefixRead::Stated(values.iter().map(|v| v.to_string()).collect())
    }

    #[test]
    fn a_nullish_default_reads_the_written_default() {
        assert_eq!(
            read_of("app.register(async (api) => {}, { prefix: opts.prefix ?? '/api/v1' })"),
            stated(&["/api/v1"])
        );
    }

    #[test]
    fn a_logical_or_default_reads_the_written_default() {
        assert_eq!(
            read_of("app.register(async (api) => {}, { prefix: opts.prefix || `/api/v2` })"),
            stated(&["/api/v2"])
        );
    }

    #[test]
    fn a_ternary_of_literals_reads_both_branches() {
        assert_eq!(
            read_of("app.register(async (api) => {}, { prefix: o.legacy ? '/legacy' : '/v3' })"),
            stated(&["/legacy", "/v3"])
        );
        assert_eq!(
            read_of("app.register(async (api) => {}, { prefix: o.legacy ? o.p : '/v3' })"),
            PrefixRead::Unread
        );
    }

    #[test]
    fn a_const_and_a_parameter_default_are_read_by_scope() {
        assert_eq!(
            read_of(
                "const prefix = '/api/v5';\napp.register(async (api) => {}, { prefix: prefix })"
            ),
            stated(&["/api/v5"])
        );
        assert_eq!(
            read_of(
                "export function setup(app, { prefix = '/api/v4' } = {}) {\n  app.register(async (api) => {}, { prefix });\n}"
            ),
            stated(&["/api/v4"])
        );
        assert_eq!(
            read_of(
                "export function setup(app, prefix = '/api/v6') {\n  app.register(async (api) => {}, { prefix });\n}"
            ),
            stated(&["/api/v6"])
        );
    }

    #[test]
    fn a_nested_const_does_not_answer_for_the_outer_binding() {
        let source = "export function setup(app, prefix) {\n  { const prefix = '/inner'; }\n  app.register(async (api) => {}, { prefix });\n}";
        assert_eq!(read_of(source), PrefixRead::Unread);
    }

    #[test]
    fn a_member_read_a_call_and_a_substitution_are_unread() {
        for value in ["opts.prefix", "makePrefix()", "`/api/${version}`"] {
            let source = format!("app.register(async (api) => {{}}, {{ prefix: {value} }})");
            assert_eq!(read_of(&source), PrefixRead::Unread, "{value}");
        }
    }

    #[test]
    fn a_value_that_is_not_a_route_path_is_unread() {
        for value in ["'https://example.test/api'", "'/a b'", "'api'"] {
            let source = format!("app.register(async (api) => {{}}, {{ prefix: {value} }})");
            assert_eq!(read_of(&source), PrefixRead::Unread, "{value}");
        }
    }

    #[test]
    fn the_binding_depth_is_capped() {
        let source = "const a = '/deep';\nconst b = a;\nconst c = b;\nconst d = c;\nconst e = d;\napp.register(async (api) => {}, { prefix: e })";
        assert_eq!(read_of(source), PrefixRead::Unread);
        let source = "const a = '/deep';\nconst b = a;\nconst c = b;\nconst d = c;\napp.register(async (api) => {}, { prefix: d })";
        assert_eq!(read_of(source), stated(&["/deep"]));
    }

    #[test]
    fn a_call_with_two_inline_functions_or_none_is_not_a_site() {
        assert!(sites("app.addHook('onRequest', async (req) => {}, async (res) => {})").is_empty());
        assert!(sites("app.register(plugin, { prefix: '/x' })").is_empty());
        assert!(sites("run(async (api) => {})").is_empty());
    }

    #[test]
    fn a_site_records_its_lines_and_names() {
        let found = sites(
            "app.register(\n  async (api) => {\n    api.get('/x', h);\n  },\n  { prefix: '/p' },\n);",
        );
        let [site] = found.as_slice() else {
            panic!("one site");
        };
        assert_eq!(
            (site.line, site.receiver.as_str(), site.param.as_str()),
            (1, "app", "api")
        );
        assert_eq!((site.body_start_line, site.body_end_line), (2, 4));
        assert_eq!(site.options_text.as_deref(), Some("{ prefix: '/p' }"));
    }

    #[test]
    fn scope_ids_are_told_from_binding_names() {
        assert!(is_scope_id(&scope_id("api", "src/a.ts", 3)));
        assert!(!is_scope_id("api"));
        assert!(!is_scope_id("@scope/pkg"));
    }
}
