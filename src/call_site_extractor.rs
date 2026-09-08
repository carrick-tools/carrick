use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use swc_common::{SourceMap, SourceMapper, Span, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

/// Type information for the result of a call expression
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultTypeInfo {
    pub type_string: String,
    /// UTF-16 character offset (compatible with ts-morph)
    pub utf16_offset: u32,
}

/// Information about an originating call expression that can be correlated with
/// follow-up member calls (e.g., response parsing or decode steps).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelatedCallInfo {
    pub callee: String,
    pub url: Option<String>,
    pub method: Option<String>,
    pub location: String,
}

/// Represents a potential call site that could be an endpoint or mount
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallSite {
    pub callee_object: String,
    pub callee_property: String,
    pub args: Vec<CallArgument>,
    pub definition: Option<String>,
    pub location: String,
    /// Type annotation from variable declaration when this call is the initializer
    pub result_type: Option<ResultTypeInfo>,
    pub correlated_call: Option<CorrelatedCallInfo>,
    pub context_slice: Option<String>,
}

/// Represents an argument to a call site
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallArgument {
    pub arg_type: ArgumentType,
    pub value: Option<String>,
    pub resolved_value: Option<String>,
    /// For function/arrow arguments: type annotations on parameters
    /// Format: [(param_name, type_string, byte_offset), ...]
    pub handler_param_types: Option<Vec<HandlerParamType>>,
}

/// Type information for a handler parameter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandlerParamType {
    pub param_name: String,
    pub type_string: String,
    /// UTF-16 character offset (compatible with ts-morph)
    pub utf16_offset: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArgumentType {
    StringLiteral,
    Identifier,
    FunctionExpression,
    ArrowFunction,
    ObjectLiteral,
    ArrayLiteral,
    TemplateLiteral,
    Other,
}

#[allow(dead_code)]
type DefinitionId = swc_ecma_ast::Id;

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
struct DefinitionIndex {
    defs: HashMap<DefinitionId, DefinitionInfo>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct DefinitionInfo {
    source: DefinitionSource,
    deps: Vec<DefinitionId>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
enum DefinitionSource {
    VariableDecl(Span, Box<Expr>),
    Import(Span),
    CallbackParam {
        param_span: Span,
        parent_call_span: Span,
    },
}

#[derive(Default)]
#[allow(dead_code)]
struct DefinitionIndexBuilder {
    index: DefinitionIndex,
}

impl DefinitionIndexBuilder {
    fn insert(&mut self, id: DefinitionId, source: DefinitionSource, deps: Vec<DefinitionId>) {
        self.index.defs.insert(id, DefinitionInfo { source, deps });
    }
}

#[derive(Default)]
#[allow(dead_code)]
struct UsedIdCollector {
    ids: HashSet<DefinitionId>,
}

impl Visit for UsedIdCollector {
    fn visit_ident(&mut self, ident: &Ident) {
        self.ids.insert(ident.to_id());
    }

    fn visit_member_expr(&mut self, member: &MemberExpr) {
        member.obj.visit_with(self);
        if let MemberProp::Computed(computed) = &member.prop {
            computed.expr.visit_with(self);
        }
    }

    fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
        arrow.body.visit_with(self);
    }

    fn visit_function(&mut self, function: &Function) {
        if let Some(body) = &function.body {
            body.visit_with(self);
        }
    }

    fn visit_catch_clause(&mut self, catch: &CatchClause) {
        catch.body.visit_with(self);
    }
}

#[allow(dead_code)]
fn collect_bound_ids_from_pat(pat: &Pat, out: &mut Vec<DefinitionId>) {
    match pat {
        Pat::Ident(binding) => out.push(binding.id.to_id()),
        Pat::Array(array) => {
            for elem in array.elems.iter().flatten() {
                collect_bound_ids_from_pat(elem, out);
            }
        }
        Pat::Object(object) => {
            for prop in &object.props {
                match prop {
                    ObjectPatProp::Assign(assign) => out.push(assign.key.to_id()),
                    ObjectPatProp::KeyValue(kv) => collect_bound_ids_from_pat(&kv.value, out),
                    ObjectPatProp::Rest(rest) => collect_bound_ids_from_pat(&rest.arg, out),
                }
            }
        }
        Pat::Assign(assign) => collect_bound_ids_from_pat(&assign.left, out),
        Pat::Rest(rest) => collect_bound_ids_from_pat(&rest.arg, out),
        _ => {}
    }
}

#[allow(dead_code)]
fn collect_used_ids_in_expr_set(expr: &Expr, out: &mut HashSet<DefinitionId>) {
    let mut collector = UsedIdCollector::default();
    expr.visit_with(&mut collector);
    out.extend(collector.ids);
}

#[allow(dead_code)]
fn collect_used_ids_in_pat_defaults(pat: &Pat, out: &mut HashSet<DefinitionId>) {
    match pat {
        Pat::Assign(assign) => {
            collect_used_ids_in_expr_set(&assign.right, out);
            collect_used_ids_in_pat_defaults(&assign.left, out);
        }
        Pat::Array(array) => {
            for elem in array.elems.iter().flatten() {
                collect_used_ids_in_pat_defaults(elem, out);
            }
        }
        Pat::Object(object) => {
            for prop in &object.props {
                match prop {
                    ObjectPatProp::Assign(assign) => {
                        if let Some(value) = &assign.value {
                            collect_used_ids_in_expr_set(value, out);
                        }
                    }
                    ObjectPatProp::KeyValue(kv) => collect_used_ids_in_pat_defaults(&kv.value, out),
                    ObjectPatProp::Rest(rest) => collect_used_ids_in_pat_defaults(&rest.arg, out),
                }
            }
        }
        Pat::Rest(rest) => collect_used_ids_in_pat_defaults(&rest.arg, out),
        _ => {}
    }
}

#[allow(dead_code)]
fn collect_used_ids_from_call_context(call: &CallExpr) -> Vec<DefinitionId> {
    let mut ids = HashSet::new();

    if let Callee::Expr(expr) = &call.callee {
        collect_used_ids_in_expr_set(expr, &mut ids);
    }

    for arg in &call.args {
        match &*arg.expr {
            Expr::Arrow(_) | Expr::Fn(_) => {}
            _ => collect_used_ids_in_expr_set(&arg.expr, &mut ids),
        }
    }

    ids.into_iter().collect()
}

impl Visit for DefinitionIndexBuilder {
    fn visit_import_decl(&mut self, import: &ImportDecl) {
        for specifier in &import.specifiers {
            let id = match specifier {
                ImportSpecifier::Named(named) => named.local.to_id(),
                ImportSpecifier::Default(default) => default.local.to_id(),
                ImportSpecifier::Namespace(namespace) => namespace.local.to_id(),
            };

            self.insert(id, DefinitionSource::Import(import.span), Vec::new());
        }

        import.visit_children_with(self);
    }

    fn visit_var_decl(&mut self, var_decl: &VarDecl) {
        for decl in &var_decl.decls {
            let Some(init) = &decl.init else {
                continue;
            };

            let mut bound_ids = Vec::new();
            collect_bound_ids_from_pat(&decl.name, &mut bound_ids);

            if bound_ids.is_empty() {
                continue;
            }

            let mut deps = HashSet::new();
            collect_used_ids_in_expr_set(init, &mut deps);
            collect_used_ids_in_pat_defaults(&decl.name, &mut deps);

            let deps: Vec<DefinitionId> = deps.into_iter().collect();

            for id in bound_ids {
                self.insert(
                    id,
                    DefinitionSource::VariableDecl(var_decl.span, init.clone()),
                    deps.clone(),
                );
            }
        }

        var_decl.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        let deps = collect_used_ids_from_call_context(call);

        for arg in &call.args {
            match &*arg.expr {
                Expr::Arrow(arrow) => {
                    for param in &arrow.params {
                        let mut bound_ids = Vec::new();
                        collect_bound_ids_from_pat(param, &mut bound_ids);

                        for id in bound_ids {
                            self.insert(
                                id,
                                DefinitionSource::CallbackParam {
                                    param_span: param.span(),
                                    parent_call_span: call.span,
                                },
                                deps.clone(),
                            );
                        }
                    }
                }
                Expr::Fn(fn_expr) => {
                    for param in &fn_expr.function.params {
                        let mut bound_ids = Vec::new();
                        collect_bound_ids_from_pat(&param.pat, &mut bound_ids);

                        for id in bound_ids {
                            self.insert(
                                id,
                                DefinitionSource::CallbackParam {
                                    param_span: param.pat.span(),
                                    parent_call_span: call.span,
                                },
                                deps.clone(),
                            );
                        }
                    }
                }
                _ => {}
            }
        }

        call.visit_children_with(self);
    }
}

#[allow(dead_code)]
fn build_definition_index(module: &Module) -> DefinitionIndex {
    let mut builder = DefinitionIndexBuilder::default();
    module.visit_with(&mut builder);
    builder.index
}

/// Framework-agnostic visitor that extracts ALL member call expressions
#[allow(dead_code)]
pub struct CallSiteExtractor {
    pub call_sites: Vec<CallSite>,
    pub variable_definitions: HashMap<String, String>,
    pub argument_values: HashMap<String, String>,
    /// Maps call expression spans to their result type info (from variable declarations)
    call_span_to_result_type: HashMap<Span, ResultTypeInfo>,
    /// Maps variables initialized from a call expression to originating call info.
    call_result_vars: HashMap<String, CorrelatedCallInfo>,
    definition_index: Option<DefinitionIndex>,
    current_file: PathBuf,
    source_map: Lrc<SourceMap>,
}

#[allow(dead_code)]
impl CallSiteExtractor {
    pub fn new(file_path: PathBuf, source_map: Lrc<SourceMap>) -> Self {
        Self {
            call_sites: Vec::new(),
            variable_definitions: HashMap::new(),
            argument_values: HashMap::new(),
            call_span_to_result_type: HashMap::new(),
            call_result_vars: HashMap::new(),
            definition_index: None,
            current_file: file_path,
            source_map,
        }
    }

    fn get_line_and_column(&self, span: swc_common::Span) -> (usize, usize) {
        let loc = self.source_map.lookup_char_pos(span.lo);
        (loc.line, loc.col_display)
    }

    /// Convert an expression to a string representation.
    /// Recursively handles nested member expressions like `process.env.API_URL`.
    #[allow(clippy::only_used_in_recursion)]
    fn expr_to_string(&self, expr: &Expr) -> String {
        match expr {
            Expr::Ident(ident) => ident.sym.to_string(),
            Expr::This(_) => "this".to_string(),
            Expr::Member(member) => {
                let obj_str = self.expr_to_string(&member.obj);
                match &member.prop {
                    MemberProp::Ident(ident) => format!("{}.{}", obj_str, ident.sym),
                    MemberProp::PrivateName(name) => format!("{}.#{}", obj_str, name.name),
                    MemberProp::Computed(computed) => {
                        format!("{}[{}]", obj_str, self.expr_to_string(&computed.expr))
                    }
                }
            }
            Expr::Call(call) => {
                let callee_str = match &call.callee {
                    Callee::Expr(callee) => self.expr_to_string(callee),
                    Callee::Super(_) => "super".to_string(),
                    Callee::Import(_) => "import".to_string(),
                };
                format!("{}()", callee_str)
            }
            Expr::New(new_expr) => {
                let callee_str = self.expr_to_string(&new_expr.callee);
                format!("new {}()", callee_str)
            }
            Expr::Await(await_expr) => format!("await {}", self.expr_to_string(&await_expr.arg)),
            Expr::Paren(paren) => self.expr_to_string(&paren.expr),
            Expr::TsAs(ts_as) => self.expr_to_string(&ts_as.expr),
            Expr::TsNonNull(non_null) => self.expr_to_string(&non_null.expr),
            Expr::TsTypeAssertion(type_assertion) => self.expr_to_string(&type_assertion.expr),
            Expr::Lit(Lit::Str(s)) => s.value.to_string(),
            Expr::Lit(Lit::Num(n)) => n.value.to_string(),
            Expr::Lit(Lit::Bool(b)) => b.value.to_string(),
            _ => "...".to_string(),
        }
    }

    fn extract_template_literal(&self, tpl: &Tpl) -> String {
        let mut value = String::new();
        for (i, quasi) in tpl.quasis.iter().enumerate() {
            value.push_str(&quasi.raw);
            if i < tpl.exprs.len() {
                let expr = &tpl.exprs[i];
                let expr_str = self.expr_to_string(expr);
                value.push_str(&format!("${{{}}}", expr_str));
            }
        }
        value
    }

    /// Extract type annotations from function parameters (for function expressions)
    fn extract_function_param_types(&self, params: &[Param]) -> Vec<HandlerParamType> {
        params
            .iter()
            .filter_map(|param| {
                let name = match &param.pat {
                    Pat::Ident(ident) => ident.id.sym.to_string(),
                    _ => return None,
                };

                // Get type annotation if present
                if let Pat::Ident(ident) = &param.pat
                    && let Some(type_ann) = &ident.type_ann
                {
                    let type_string = self
                        .source_map
                        .span_to_snippet(type_ann.type_ann.span())
                        .unwrap_or_else(|_| "unknown".to_string());

                    // Calculate file-relative UTF-16 offset
                    let span = type_ann.type_ann.span();
                    let loc = self.source_map.lookup_char_pos(span.lo);
                    let file_start = loc.file.start_pos;
                    let file_relative_byte = (span.lo - file_start).0 as usize;

                    // Convert to UTF-16 using the source already held in the SourceMap
                    // (no per-param disk re-read).
                    let utf16_offset =
                        crate::utils::utf16_offset(&loc.file.src, file_relative_byte);

                    return Some(HandlerParamType {
                        param_name: name,
                        type_string,
                        utf16_offset,
                    });
                }
                None
            })
            .collect()
    }

    /// Extract type annotations from arrow function parameters
    fn extract_arrow_param_types(&self, params: &[Pat]) -> Vec<HandlerParamType> {
        params
            .iter()
            .filter_map(|pat| {
                if let Pat::Ident(ident) = pat {
                    let name = ident.id.sym.to_string();
                    if let Some(type_ann) = &ident.type_ann {
                        let type_string = self
                            .source_map
                            .span_to_snippet(type_ann.type_ann.span())
                            .unwrap_or_else(|_| "unknown".to_string());

                        // Calculate file-relative UTF-16 offset
                        let span = type_ann.type_ann.span();
                        let loc = self.source_map.lookup_char_pos(span.lo);
                        let file_start = loc.file.start_pos;
                        let file_relative_byte = (span.lo - file_start).0 as usize;

                        // Convert to UTF-16 using the source already held in the SourceMap
                        // (no per-param disk re-read).
                        let utf16_offset =
                            crate::utils::utf16_offset(&loc.file.src, file_relative_byte);

                        return Some(HandlerParamType {
                            param_name: name,
                            type_string,
                            utf16_offset,
                        });
                    }
                }
                None
            })
            .collect()
    }

    /// Find the call expression span within an expression
    /// Handles: call(), await call(), (await call()), etc.
    fn find_call_span_in_expr(expr: &Expr) -> Option<Span> {
        match expr {
            Expr::Call(call) => Some(call.span),
            Expr::Await(await_expr) => {
                // Unwrap the await to find the call inside
                Self::find_call_span_in_expr(&await_expr.arg)
            }
            Expr::Paren(paren) => {
                // Unwrap parentheses
                Self::find_call_span_in_expr(&paren.expr)
            }
            _ => None,
        }
    }

    /// Find a call expression within an expression (unwrapping await, paren, etc.)
    fn find_call_expr_in_expr(expr: &Expr) -> Option<&CallExpr> {
        match expr {
            Expr::Call(call) => Some(call),
            Expr::Await(await_expr) => Self::find_call_expr_in_expr(&await_expr.arg),
            Expr::Paren(paren) => Self::find_call_expr_in_expr(&paren.expr),
            _ => None,
        }
    }

    fn is_http_verb(name: &str) -> bool {
        matches!(
            name.to_ascii_lowercase().as_str(),
            "get" | "post" | "put" | "patch" | "delete" | "head" | "options"
        )
    }

    fn extract_string_value(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Lit(Lit::Str(s)) => Some(s.value.to_string()),
            Expr::Tpl(tpl) => Some(self.extract_template_literal(tpl)),
            Expr::Ident(ident) => {
                let var_name = ident.sym.to_string();
                self.argument_values.get(&var_name).cloned()
            }
            _ => None,
        }
    }

    fn extract_http_method_from_object(&self, obj: &ObjectLit) -> Option<String> {
        for prop in &obj.props {
            let PropOrSpread::Prop(prop) = prop else {
                continue;
            };
            let Prop::KeyValue(kv) = &**prop else {
                continue;
            };

            let key_name = match &kv.key {
                PropName::Ident(ident) => Some(ident.sym.as_ref()),
                PropName::Str(s) => Some(s.value.as_ref()),
                _ => None,
            };
            if key_name != Some("method") {
                continue;
            }

            match &*kv.value {
                Expr::Lit(Lit::Str(s)) => return Some(s.value.to_string().to_uppercase()),
                Expr::Ident(ident) => {
                    let var_name = ident.sym.to_string();
                    if let Some(value) = self.argument_values.get(&var_name) {
                        return Some(value.to_string().to_uppercase());
                    }
                }
                _ => {}
            }
        }

        None
    }

    fn extract_http_method_from_callee(&self, call: &CallExpr) -> Option<String> {
        let Callee::Expr(expr) = &call.callee else {
            return None;
        };

        match &**expr {
            Expr::Ident(ident) => {
                let name = ident.sym.as_ref();
                if Self::is_http_verb(name) {
                    Some(name.to_uppercase())
                } else {
                    None
                }
            }
            Expr::Member(member) => {
                let prop_name = match &member.prop {
                    MemberProp::Ident(ident) => ident.sym.to_string(),
                    MemberProp::PrivateName(name) => name.name.to_string(),
                    MemberProp::Computed(computed) => match &*computed.expr {
                        Expr::Lit(Lit::Str(s)) => s.value.to_string(),
                        Expr::Ident(ident) => ident.sym.to_string(),
                        _ => self.expr_to_string(&computed.expr),
                    },
                };

                if Self::is_http_verb(&prop_name) {
                    Some(prop_name.to_uppercase())
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn extract_path_from_url(&self, url: &str) -> Option<String> {
        let mut path = if url.starts_with('/') {
            url.to_string()
        } else if url.starts_with("${") {
            // Leading `${BASE}/...` interpolation: strip the base-URL variable by taking
            // everything from the first `}/`. Gated on the leading `${` so that a literal
            // URL containing brace params (e.g. `api/{id}/items`) is NOT truncated to
            // `/items` — that case falls through to the branches below.
            match url.find("}/") {
                Some(idx) => url[idx + 1..].to_string(),
                None => url.to_string(),
            }
        } else if url.starts_with("http://") || url.starts_with("https://") {
            if let Some(path_start) = url.find("://").and_then(|i| url[i + 3..].find('/')) {
                let path_idx = url.find("://").unwrap() + 3 + path_start;
                url[path_idx..].to_string()
            } else {
                url.to_string()
            }
        } else {
            url.to_string()
        };

        if let Some(query_idx) = path.find('?') {
            path.truncate(query_idx);
        }
        if let Some(fragment_idx) = path.find('#') {
            path.truncate(fragment_idx);
        }

        Some(Self::normalize_template_params(&path))
    }

    fn normalize_template_params(path: &str) -> String {
        let mut result = String::new();
        let mut chars = path.chars().peekable();

        while let Some(c) = chars.next() {
            if c == '$' && chars.peek() == Some(&'{') {
                chars.next();

                let mut var_name = String::new();
                for inner_c in chars.by_ref() {
                    if inner_c == '}' {
                        break;
                    }
                    if inner_c == '.' {
                        var_name.clear();
                    } else {
                        var_name.push(inner_c);
                    }
                }

                if !var_name.is_empty() {
                    result.push(':');
                    result.push_str(&var_name);
                }
            } else {
                result.push(c);
            }
        }

        result
    }

    fn extract_url_from_object(&self, obj: &ObjectLit) -> Option<String> {
        for prop in &obj.props {
            let PropOrSpread::Prop(prop) = prop else {
                continue;
            };
            let Prop::KeyValue(kv) = &**prop else {
                continue;
            };

            let key_name = match &kv.key {
                PropName::Ident(ident) => Some(ident.sym.as_ref()),
                PropName::Str(s) => Some(s.value.as_ref()),
                _ => None,
            };

            let Some(key_name) = key_name else {
                continue;
            };

            if !matches!(key_name, "url" | "path" | "href") {
                continue;
            }

            if let Some(raw) = self.extract_string_value(&kv.value) {
                return self.extract_path_from_url(&raw);
            }
        }

        None
    }

    fn extract_url_from_call(&self, call: &CallExpr) -> Option<String> {
        if call.args.is_empty() {
            return None;
        }

        let first = &call.args[0].expr;
        match &**first {
            Expr::Object(obj) => self.extract_url_from_object(obj),
            _ => self
                .extract_string_value(first)
                .and_then(|raw| self.extract_path_from_url(&raw)),
        }
    }

    fn extract_http_method_from_call(&self, call: &CallExpr) -> Option<String> {
        for arg in &call.args {
            if let Expr::Object(obj) = &*arg.expr
                && let Some(method) = self.extract_http_method_from_object(obj)
            {
                return Some(method);
            }
        }

        self.extract_http_method_from_callee(call)
    }

    fn build_correlated_call_info(&self, call: &CallExpr) -> Option<CorrelatedCallInfo> {
        let callee = match &call.callee {
            Callee::Expr(expr) => self.expr_to_string(expr),
            Callee::Super(_) => "super".to_string(),
            Callee::Import(_) => "import".to_string(),
        };

        let url = self.extract_url_from_call(call);
        let mut method = self.extract_http_method_from_call(call);
        if method.is_none() && url.is_some() {
            method = Some("GET".to_string());
        }

        if url.is_none() && method.is_none() {
            return None;
        }

        let (line, column) = self.get_line_and_column(call.span);
        let location = format!("{}:{}:{}", self.current_file.display(), line, column);

        Some(CorrelatedCallInfo {
            callee,
            url,
            method,
            location,
        })
    }

    fn span_contains(outer: Span, inner: Span) -> bool {
        outer.lo <= inner.lo && outer.hi >= inner.hi
    }

    fn merge_overlapping_spans(mut spans: Vec<Span>) -> Vec<Span> {
        spans.sort_by_key(|span| span.lo);

        let mut merged: Vec<Span> = Vec::new();
        for span in spans {
            if let Some(last) = merged.last_mut()
                && span.lo <= last.hi
            {
                if span.hi > last.hi {
                    last.hi = span.hi;
                }
                continue;
            }

            merged.push(span);
        }

        merged
    }

    fn compute_context_slice(&self, call_span: Span, seed_expr: Option<&Expr>) -> Option<String> {
        const MAX_DEPTH: usize = 20;
        const MAX_SPANS: usize = 50;

        let definition_index = self.definition_index.as_ref()?;

        let mut collected_spans: HashSet<Span> = HashSet::new();
        collected_spans.insert(call_span);

        let mut worklist: std::collections::VecDeque<(DefinitionId, usize)> =
            std::collections::VecDeque::new();
        if let Some(expr) = seed_expr {
            let seed_span = expr.span();
            if !Self::span_contains(call_span, seed_span) {
                collected_spans.insert(seed_span);
            }

            let mut used_ids = HashSet::new();
            collect_used_ids_in_expr_set(expr, &mut used_ids);
            for id in used_ids {
                worklist.push_back((id, 0));
            }
        }

        let mut visited_ids: HashSet<DefinitionId> = HashSet::new();

        while let Some((id, depth)) = worklist.pop_front() {
            if collected_spans.len() >= MAX_SPANS {
                break;
            }

            if depth > MAX_DEPTH {
                continue;
            }

            if !visited_ids.insert(id.clone()) {
                continue;
            }

            let Some(info) = definition_index.defs.get(&id) else {
                continue;
            };

            match &info.source {
                DefinitionSource::VariableDecl(span, _) => {
                    collected_spans.insert(*span);
                    for dep in &info.deps {
                        worklist.push_back((dep.clone(), depth + 1));
                    }
                }
                DefinitionSource::CallbackParam {
                    parent_call_span, ..
                } => {
                    collected_spans.insert(*parent_call_span);
                    for dep in &info.deps {
                        worklist.push_back((dep.clone(), depth + 1));
                    }
                }
                DefinitionSource::Import(span) => {
                    collected_spans.insert(*span);
                }
            }
        }

        let mut spans: Vec<Span> = collected_spans.into_iter().collect();
        spans.sort_by_key(|span| span.lo);
        let spans = Self::merge_overlapping_spans(spans);

        let mut snippets = Vec::new();
        for span in spans {
            if let Ok(snippet) = self.source_map.span_to_snippet(span) {
                let snippet = snippet.trim();
                if !snippet.is_empty() {
                    snippets.push(snippet.to_string());
                }
            }
        }

        if snippets.is_empty() {
            None
        } else {
            Some(snippets.join("\n"))
        }
    }

    fn extract_argument(&self, expr: &Expr) -> CallArgument {
        match expr {
            Expr::Lit(Lit::Str(str_lit)) => CallArgument {
                arg_type: ArgumentType::StringLiteral,
                value: Some(str_lit.value.to_string()),
                resolved_value: Some(str_lit.value.to_string()),
                handler_param_types: None,
            },
            Expr::Ident(ident) => {
                let name = ident.sym.to_string();
                let resolved = self.argument_values.get(&name).cloned();
                CallArgument {
                    arg_type: ArgumentType::Identifier,
                    value: Some(name),
                    resolved_value: resolved,
                    handler_param_types: None,
                }
            }
            Expr::Fn(fn_expr) => {
                let param_types = self.extract_function_param_types(&fn_expr.function.params);
                CallArgument {
                    arg_type: ArgumentType::FunctionExpression,
                    value: None,
                    resolved_value: None,
                    handler_param_types: if param_types.is_empty() {
                        None
                    } else {
                        Some(param_types)
                    },
                }
            }
            Expr::Arrow(arrow) => {
                let param_types = self.extract_arrow_param_types(&arrow.params);
                CallArgument {
                    arg_type: ArgumentType::ArrowFunction,
                    value: None,
                    resolved_value: None,
                    handler_param_types: if param_types.is_empty() {
                        None
                    } else {
                        Some(param_types)
                    },
                }
            }
            Expr::Object(_) => CallArgument {
                arg_type: ArgumentType::ObjectLiteral,
                value: None,
                resolved_value: None,
                handler_param_types: None,
            },
            Expr::Array(_) => CallArgument {
                arg_type: ArgumentType::ArrayLiteral,
                value: None,
                resolved_value: None,
                handler_param_types: None,
            },
            Expr::Tpl(tpl) => CallArgument {
                arg_type: ArgumentType::TemplateLiteral,
                value: Some(self.extract_template_literal(tpl)),
                resolved_value: None,
                handler_param_types: None,
            },
            _ => CallArgument {
                arg_type: ArgumentType::Other,
                value: None,
                resolved_value: None,
                handler_param_types: None,
            },
        }
    }
}

impl Visit for CallSiteExtractor {
    fn visit_module(&mut self, module: &Module) {
        self.definition_index = Some(build_definition_index(module));
        module.visit_children_with(self);
    }

    fn visit_var_decl(&mut self, var_decl: &VarDecl) {
        for decl in &var_decl.decls {
            if let Pat::Ident(ident) = &decl.name {
                let var_name = ident.id.sym.to_string();

                if let Some(init) = &decl.init {
                    match &**init {
                        Expr::Lit(Lit::Str(str_lit)) => {
                            self.argument_values
                                .insert(var_name.clone(), str_lit.value.to_string());
                        }
                        Expr::Tpl(tpl) => {
                            self.argument_values
                                .insert(var_name.clone(), self.extract_template_literal(tpl));
                        }
                        _ => {}
                    }

                    if let Some(call_expr) = Self::find_call_expr_in_expr(init)
                        && let Some(info) = self.build_correlated_call_info(call_expr)
                    {
                        self.call_result_vars.insert(var_name.clone(), info);
                    }

                    // Extract type annotation and link to call expression if present
                    // Pattern: const x: Type = await someCall() OR const x: Type = someCall()
                    if let Some(type_ann) = &ident.type_ann {
                        let type_string = self
                            .source_map
                            .span_to_snippet(type_ann.type_ann.span())
                            .unwrap_or_else(|_| "unknown".to_string());

                        // Calculate file-relative UTF-16 offset
                        let span = type_ann.type_ann.span();
                        let loc = self.source_map.lookup_char_pos(span.lo);
                        let file_start = loc.file.start_pos;
                        let file_relative_byte = (span.lo - file_start).0 as usize;

                        // Convert to UTF-16 using the source already held in the SourceMap
                        // (no per-param disk re-read).
                        let utf16_offset =
                            crate::utils::utf16_offset(&loc.file.src, file_relative_byte);

                        let result_type_info = ResultTypeInfo {
                            type_string: type_string.clone(),
                            utf16_offset,
                        };

                        // Find the call expression span to associate the type with
                        let call_span = Self::find_call_span_in_expr(init);
                        if let Some(span) = call_span {
                            self.call_span_to_result_type.insert(span, result_type_info);
                        }
                    }

                    let definition = match &**init {
                        Expr::Call(_) | Expr::New(_) => self.expr_to_string(init),
                        Expr::Ident(ident) => format!("= {}", ident.sym),
                        Expr::Lit(Lit::Str(str_lit)) => format!("= \"{}\"", str_lit.value),
                        Expr::Tpl(tpl) => format!("= `{}`", self.extract_template_literal(tpl)),
                        _ => "variable_assignment".to_string(),
                    };

                    self.variable_definitions.insert(var_name, definition);
                }
            }
        }

        var_decl.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        if let Callee::Expr(callee_expr) = &call.callee {
            let (object_name, property_name, callee_seed_expr) = match &**callee_expr {
                Expr::Member(member) => {
                    let object_name = self.expr_to_string(&member.obj);
                    let property_name = match &member.prop {
                        MemberProp::Ident(prop_ident) => prop_ident.sym.to_string(),
                        MemberProp::PrivateName(name) => name.name.to_string(),
                        MemberProp::Computed(computed) => match &*computed.expr {
                            Expr::Lit(Lit::Str(s)) => s.value.to_string(),
                            Expr::Ident(ident) => ident.sym.to_string(),
                            _ => self.expr_to_string(&computed.expr),
                        },
                    };
                    (object_name, property_name, Some(&*member.obj))
                }
                Expr::Ident(ident) => (
                    "global".to_string(),
                    ident.sym.to_string(),
                    Some(&**callee_expr),
                ),
                _ => return,
            };

            let args: Vec<CallArgument> = call
                .args
                .iter()
                .map(|arg| self.extract_argument(&arg.expr))
                .collect();

            let (line, column) = self.get_line_and_column(call.span);
            let location = format!("{}:{}:{}", self.current_file.display(), line, column);

            let definition = if object_name == "global" {
                self.variable_definitions.get(&property_name).cloned()
            } else {
                self.variable_definitions
                    .get(&object_name)
                    .cloned()
                    .or_else(|| {
                        let root = object_name.split(['.', '[', '(']).next().unwrap_or("");
                        if root.is_empty() {
                            None
                        } else {
                            self.variable_definitions.get(root).cloned()
                        }
                    })
            };

            let result_type = self.call_span_to_result_type.get(&call.span).cloned();

            let correlated_call = if call.args.is_empty() {
                self.call_result_vars
                    .get(&object_name)
                    .cloned()
                    .or_else(|| {
                        let Expr::Member(member) = &**callee_expr else {
                            return None;
                        };
                        let origin_call = Self::find_call_expr_in_expr(&member.obj)?;
                        self.build_correlated_call_info(origin_call)
                    })
            } else {
                None
            };

            let seed_expr = call.args.first().map(|arg| &*arg.expr).and_then(|expr| {
                if matches!(expr, Expr::Arrow(_) | Expr::Fn(_)) {
                    return None;
                }

                let mut ids = HashSet::new();
                collect_used_ids_in_expr_set(expr, &mut ids);
                if ids.is_empty() { None } else { Some(expr) }
            });

            let context_slice =
                self.compute_context_slice(call.span, seed_expr.or(callee_seed_expr));

            self.call_sites.push(CallSite {
                callee_object: object_name,
                callee_property: property_name,
                args,
                definition,
                location,
                result_type,
                correlated_call,
                context_slice,
            });
        }

        call.visit_children_with(self);
    }
}

/// Service for extracting call sites from multiple files
#[allow(dead_code)]
pub struct CallSiteExtractionService {
    call_sites: Vec<CallSite>,
}

impl Default for CallSiteExtractionService {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
impl CallSiteExtractionService {
    pub fn new() -> Self {
        Self {
            call_sites: Vec::new(),
        }
    }

    pub fn extract_from_visitors(&mut self, visitors: Vec<CallSiteExtractor>) {
        for visitor in visitors {
            self.call_sites.extend(visitor.call_sites);
        }
    }

    pub fn get_call_sites(&self) -> &[CallSite] {
        &self.call_sites
    }

    /// Prepare call sites for LLM classification with framework context
    #[allow(dead_code)]
    pub fn prepare_for_classification(&self) -> serde_json::Value {
        serde_json::json!({
            "call_sites": self.call_sites,
            "total_count": self.call_sites.len()
        })
    }
}

#[cfg(test)]
mod definition_index_tests {
    use super::{DefinitionSource, build_definition_index};
    use crate::parser::parse_file;
    use swc_common::{
        SourceMap, SourceMapper,
        errors::{ColorConfig, Handler},
        sync::Lrc,
    };
    use swc_ecma_ast::*;
    use swc_ecma_visit::{Visit, VisitWith};

    fn parse_ts(source: &str) -> (Lrc<SourceMap>, Module) {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = tmp_dir.path().join("input.ts");
        std::fs::write(&file_path, source).expect("write file");

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));
        let module = parse_file(&file_path, &cm, &handler).expect("parsed module");

        (cm, module)
    }

    #[derive(Default)]
    struct IdFinder {
        import_r: Option<Id>,
        base: Option<Id>,
        route: Option<Id>,
        routes: Option<Id>,
        arrow_param_r: Option<Id>,
    }

    impl Visit for IdFinder {
        fn visit_import_decl(&mut self, import: &ImportDecl) {
            for spec in &import.specifiers {
                if let ImportSpecifier::Named(named) = spec
                    && named.local.sym.as_ref() == "r"
                {
                    self.import_r = Some(named.local.to_id());
                }
            }

            import.visit_children_with(self);
        }

        fn visit_var_declarator(&mut self, decl: &VarDeclarator) {
            if let Pat::Ident(binding) = &decl.name {
                match binding.id.sym.as_ref() {
                    "base" => self.base = Some(binding.id.to_id()),
                    "route" => self.route = Some(binding.id.to_id()),
                    "routes" => self.routes = Some(binding.id.to_id()),
                    _ => {}
                }
            }

            decl.visit_children_with(self);
        }

        fn visit_arrow_expr(&mut self, arrow: &ArrowExpr) {
            if self.arrow_param_r.is_none() {
                for param in &arrow.params {
                    if let Pat::Ident(binding) = param
                        && binding.id.sym.as_ref() == "r"
                    {
                        self.arrow_param_r = Some(binding.id.to_id());
                        break;
                    }
                }
            }

            arrow.visit_children_with(self);
        }
    }

    #[test]
    fn test_definition_index_import_is_leaf() {
        let (cm, module) = parse_ts(
            r#"import { route as r } from "./x";
app.get(r, handler);
"#,
        );

        let mut finder = IdFinder::default();
        module.visit_with(&mut finder);
        let import_r = finder.import_r.expect("imported r id");

        let index = build_definition_index(&module);
        let info = index.defs.get(&import_r).expect("definition indexed");

        match &info.source {
            DefinitionSource::Import(span) => {
                let snippet = cm.span_to_snippet(*span).expect("snippet");
                assert!(snippet.contains("import"));
                assert!(snippet.contains("route"));
                assert!(snippet.contains("r"));
                assert!(snippet.contains("./x"));
            }
            _ => panic!("expected Import source"),
        }

        assert!(info.deps.is_empty());
    }

    #[test]
    fn test_definition_index_variable_decl_tracks_deps() {
        let (_cm, module) = parse_ts(
            r#"const base = "/api";
const route = base + "/users";
app.get(route, handler);
"#,
        );

        let mut finder = IdFinder::default();
        module.visit_with(&mut finder);
        let base = finder.base.expect("base id");
        let route = finder.route.expect("route id");

        let index = build_definition_index(&module);
        let info = index.defs.get(&route).expect("route indexed");

        match &info.source {
            DefinitionSource::VariableDecl(_, _) => {}
            _ => panic!("expected VariableDecl source"),
        }

        assert!(info.deps.contains(&base));
    }

    #[test]
    fn test_definition_index_callback_param_tracks_parent_context() {
        let (cm, module) = parse_ts(
            r#"const routes = ["/a"];
routes.forEach((r) => {
  app.get(r, handler);
});
"#,
        );

        let mut finder = IdFinder::default();
        module.visit_with(&mut finder);
        let routes = finder.routes.expect("routes id");
        let r = finder.arrow_param_r.expect("arrow param r id");

        let index = build_definition_index(&module);
        let info = index.defs.get(&r).expect("callback param indexed");

        match &info.source {
            DefinitionSource::CallbackParam {
                parent_call_span, ..
            } => {
                let snippet = cm.span_to_snippet(*parent_call_span).expect("snippet");
                assert!(snippet.contains("forEach"));
                assert!(snippet.contains("routes"));
            }
            _ => panic!("expected CallbackParam source"),
        }

        assert!(info.deps.contains(&routes));
    }
}

#[cfg(test)]
mod context_slice_tests {
    use super::{CallSite, CallSiteExtractor};
    use crate::parser::parse_file;
    use swc_common::{
        SourceMap,
        errors::{ColorConfig, Handler},
        sync::Lrc,
    };
    use swc_ecma_visit::VisitWith;

    fn extract_call_sites(source: &str) -> Vec<CallSite> {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = tmp_dir.path().join("input.ts");
        std::fs::write(&file_path, source).expect("write file");

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));

        let module = parse_file(&file_path, &cm, &handler).expect("parsed module");
        let mut extractor = CallSiteExtractor::new(file_path, cm);
        module.visit_with(&mut extractor);

        extractor.call_sites
    }

    #[test]
    fn test_context_slice_includes_variable_deps_and_anchor() {
        let call_sites = extract_call_sites(
            r#"const base = "/api";
const route = base + "/users";
app.get(route, handler);
"#,
        );

        let call_site = call_sites
            .iter()
            .find(|cs| cs.callee_object == "app" && cs.callee_property == "get")
            .expect("expected app.get call site");

        let slice = call_site.context_slice.as_deref().expect("context_slice");
        assert!(slice.contains("const base"));
        assert!(slice.contains("const route"));
        assert!(slice.contains("app.get"));

        let base_idx = slice.find("const base").expect("base idx");
        let route_idx = slice.find("const route").expect("route idx");
        let call_idx = slice.find("app.get").expect("call idx");
        assert!(base_idx < route_idx);
        assert!(route_idx < call_idx);
    }

    #[test]
    fn test_context_slice_import_is_leaf() {
        let call_sites = extract_call_sites(
            r#"import { route } from "./x";
app.get(route, handler);
"#,
        );

        let call_site = call_sites
            .iter()
            .find(|cs| cs.callee_object == "app" && cs.callee_property == "get")
            .expect("expected app.get call site");

        let slice = call_site.context_slice.as_deref().expect("context_slice");
        assert!(slice.contains("import"));
        assert!(slice.contains("./x"));
        assert!(slice.contains("app.get"));
    }

    #[test]
    fn test_context_slice_callback_param_includes_parent_call() {
        let call_sites = extract_call_sites(
            r#"const routes = ["/a"];
routes.forEach((r) => {
  app.get(r, handler);
});
"#,
        );

        let call_site = call_sites
            .iter()
            .find(|cs| cs.callee_object == "app" && cs.callee_property == "get")
            .expect("expected app.get call site");

        let slice = call_site.context_slice.as_deref().expect("context_slice");
        assert!(slice.contains("const routes"));
        assert!(slice.contains("forEach"));
        assert!(slice.contains("app.get"));
    }

    #[test]
    fn test_context_slice_avoids_cycles() {
        let call_sites = extract_call_sites(
            r#"const a = b;
const b = a;
app.get(a, handler);
"#,
        );

        let call_site = call_sites
            .iter()
            .find(|cs| cs.callee_object == "app" && cs.callee_property == "get")
            .expect("expected app.get call site");

        let slice = call_site.context_slice.as_deref().expect("context_slice");
        assert!(slice.contains("const a"));
        assert!(slice.contains("const b"));
        assert!(slice.contains("app.get"));
    }
}

#[cfg(test)]
mod call_site_extraction_tests {
    use super::{CallSite, CallSiteExtractor};
    use crate::parser::parse_file;
    use swc_common::{
        SourceMap,
        errors::{ColorConfig, Handler},
        sync::Lrc,
    };
    use swc_ecma_visit::VisitWith;

    fn extract_call_sites(source: &str) -> Vec<CallSite> {
        let tmp_dir = tempfile::tempdir().expect("tempdir");
        let file_path = tmp_dir.path().join("input.ts");
        std::fs::write(&file_path, source).expect("write file");

        let cm: Lrc<SourceMap> = Default::default();
        let handler = Handler::with_tty_emitter(ColorConfig::Never, true, false, Some(cm.clone()));

        let module = parse_file(&file_path, &cm, &handler).expect("parsed module");
        let mut extractor = CallSiteExtractor::new(file_path, cm);
        module.visit_with(&mut extractor);

        extractor.call_sites
    }

    #[test]
    fn test_extracts_namespaced_member_calls() {
        let call_sites = extract_call_sites(
            r#"api.client.get("/orders");
api.client.post("/orders");
"#,
        );

        let get_call = call_sites
            .iter()
            .find(|cs| cs.callee_object == "api.client" && cs.callee_property == "get")
            .expect("expected api.client.get call site");
        assert_eq!(get_call.args.len(), 1);

        let post_call = call_sites
            .iter()
            .find(|cs| cs.callee_object == "api.client" && cs.callee_property == "post")
            .expect("expected api.client.post call site");
        assert_eq!(post_call.args.len(), 1);
    }

    #[test]
    fn test_extracts_factory_member_calls() {
        let call_sites = extract_call_sites(
            r#"getClient().get("/orders");
axios.create({ baseURL }).post("/orders");
"#,
        );

        call_sites
            .iter()
            .find(|cs| cs.callee_object == "getClient()" && cs.callee_property == "get")
            .expect("expected getClient().get call site");

        call_sites
            .iter()
            .find(|cs| cs.callee_object == "axios.create()" && cs.callee_property == "post")
            .expect("expected axios.create().post call site");
    }

    #[test]
    fn test_extracts_chained_route_calls() {
        let call_sites = extract_call_sites(
            r#"router.route("/v1/users").get(handler);
"#,
        );

        call_sites
            .iter()
            .find(|cs| cs.callee_object == "router.route()" && cs.callee_property == "get")
            .expect("expected router.route().get call site");
    }

    #[test]
    fn test_correlates_inline_fetch_for_json_calls() {
        let call_sites = extract_call_sites(
            r#"async function f() {
  const data = await (await fetch("/orders")).json();
  return data;
}
"#,
        );

        let json_call = call_sites
            .iter()
            .find(|cs| cs.callee_property == "json")
            .expect("expected .json() call site");

        let correlated = json_call
            .correlated_call
            .as_ref()
            .expect("expected correlated call info");
        assert_eq!(correlated.url.as_deref(), Some("/orders"));
        assert_eq!(correlated.method.as_deref(), Some("GET"));
    }
}
