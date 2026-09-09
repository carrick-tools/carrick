//! Which request a named member's body reaches, so the value the WRAPPER
//! writes for a dispatching route reaches the sites that call it
//! (carrick#872).
//!
//! A body-dispatching route is one `(METHOD, path)` serving many operations,
//! told apart by a field of the request (carrick#831). The value is written
//! where the request is written — inside the client member — and the row that
//! records the call is emitted at the SITE that calls that member:
//!
//! ```ignore
//! // api-client.ts
//! class ApiClient {
//!   async searchByIntent(query: string) {
//!     return fetch(this.lambdaUrl, {
//!       method: "POST",
//!       body: JSON.stringify({ action: "search-by-intent", query }),
//!     });
//!   }
//! }
//! // tools/search-by-intent.ts
//! const result = await client.searchByIntent(query);
//! ```
//!
//! The site states no literal, so nothing at the site can say WHICH of the
//! route's operations it asks for, and every such call reads
//! `dispatch_value_unknown` and matches nothing. The value is not missing,
//! though: extraction states it on the wrapper's own request, one file away.
//! What is missing is the join, and this module supplies its consumer half —
//! the one thing the site's own file cannot say, which member's request the
//! site reaches.
//!
//! WHAT IS READ HERE, AND WHAT IS NOT. The literal itself is never read here.
//! Which field of a request is the discriminator is a fact about the HANDLER
//! that serves it, not a shape a caller can be recognised by (carrick#831
//! ruling, and the 2026-09-05 ruling on bare literals): the model states it,
//! at the wrapper's request and at a call site that writes one. This module
//! reads only the structure the model cannot see across a file boundary — the
//! line the member's single request is written on — and the carry that uses it
//! copies the model's own answer from that line onto the site's row. A wrapper
//! that takes its action as a PARAMETER states no literal, so the model states
//! no dispatch for it and nothing is carried: that is not a gap this pass can
//! close, because the field name is unknowable without the producer.
//!
//! WHAT COUNTS AS A REQUEST. The same structural test the rest of the scanner
//! uses, with no client library, framework or helper name anywhere: a call
//! carrying a request-options bag (one object-literal argument with at least
//! one of `method` / `headers` / `body` / `data`), or an HTTP-verb callee
//! property with a string or template argument. The second half needs that
//! argument, because a verb property alone is also how a cache, a map and a
//! store spell their reads — `this.cache.get(() => this.fetch())` is a
//! delegation, not a request, and counting it would drop the very member this
//! pass exists for.
//!
//! WHAT COUNTS AS REACHING IT. A member reaches the requests written in its
//! own body, including inside the callbacks it builds there, plus everything
//! the SIBLING members it names reach — `this.<name>(…)` or a bare `<name>(…)`
//! where the module declares `<name>`. Following siblings is the whole point:
//! the common client shape puts the cache in one member, the request in
//! another, and a two-hop delegation (`findService` → `getAllRepoData` →
//! `fetchCrossRepoData`) is what a consumer actually calls.
//!
//! The safety is the count, and only the count: a member is indexed when
//! everything it reaches is ONE request line. Two, and a site calling it could
//! be reaching either, so it states nothing. Zero, likewise. A name the module
//! declares twice is dropped, and so is any member that delegates to it — a
//! delegation to a name with two meanings has no single answer either.
//!
//! This index is deliberately NOT [`crate::imported_request_member`]'s. That
//! one asserts a site's method and PATH, so it requires the member's request to
//! state a route-shaped URL of its own and drops everything else — including
//! every client whose URL is an opaque field (`fetch(this.lambdaUrl, …)`),
//! which is exactly the shape a body-dispatching route is called with. Widening
//! it would change which rows the scanner emits and what they claim. This
//! index claims nothing about a target: it says only which line a member's sole
//! request sits on, and it is read for nothing else.

use std::collections::{HashMap, HashSet};

use swc_common::{SourceMap, Spanned, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_visit::{Visit, VisitWith};

use crate::wrapper_request_shape::{request_options_argument, verb_from_callee_property};

/// The single request a named member reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchMember {
    /// 1-based line, in the member's OWN module, that the request is written
    /// on. The join key into that module's extracted rows: the model's answer
    /// for that line carries the dispatch value, or carries none.
    pub request_line: u32,
}

/// The members of one module that reach exactly one request, keyed by name.
pub type DispatchMemberIndex = HashMap<String, DispatchMember>;

/// Read every named member of `module` that reaches exactly one request.
///
/// Two passes: collect what each member writes and whom it calls, then close
/// the delegation graph over it. Cycle-safe — a pair of members that call each
/// other contributes each other's requests and nothing more.
pub fn collect_dispatch_members(
    module: &Module,
    source_map: &Lrc<SourceMap>,
) -> DispatchMemberIndex {
    let mut collector = MemberWalk {
        source_map,
        stack: Vec::new(),
        bodies: HashMap::new(),
        duplicated: HashSet::new(),
    };
    module.visit_with(&mut collector);
    for name in &collector.duplicated {
        collector.bodies.remove(name);
    }
    let bodies = collector.bodies;
    let duplicated = collector.duplicated;

    let mut index = DispatchMemberIndex::new();
    for name in bodies.keys() {
        let mut lines: HashSet<u32> = HashSet::new();
        if reach(name, &bodies, &duplicated, &mut HashSet::new(), &mut lines)
            && let Some(line) = sole(&lines)
        {
            index.insert(name.clone(), DispatchMember { request_line: line });
        }
    }
    index
}

/// The one line in `lines`, or `None` for none and for more than one.
fn sole(lines: &HashSet<u32>) -> Option<u32> {
    let mut iter = lines.iter();
    match (iter.next(), iter.next()) {
        (Some(line), None) => Some(*line),
        _ => None,
    }
}

/// Accumulate every request line `name` reaches into `lines`.
///
/// `false` means the answer is unknowable — the member delegates to a name the
/// module declares twice — which is a stronger statement than "nothing found"
/// and stops the member being indexed at all. A member already on the stack
/// contributes nothing: its own lines are being accumulated by the frame that
/// is walking it.
fn reach(
    name: &str,
    bodies: &HashMap<String, MemberBody>,
    duplicated: &HashSet<String>,
    visiting: &mut HashSet<String>,
    lines: &mut HashSet<u32>,
) -> bool {
    if !visiting.insert(name.to_string()) {
        return true;
    }
    let Some(body) = bodies.get(name) else {
        return true;
    };
    lines.extend(body.request_lines.iter().copied());
    // More than one already: nothing further can make it fewer, and a client
    // module's whole delegation graph is not worth walking to learn that.
    if lines.len() > 1 {
        return true;
    }
    let mut called: Vec<&String> = body.calls.iter().collect();
    called.sort();
    for callee in called {
        if duplicated.contains(callee) {
            return false;
        }
        if !reach(callee, bodies, duplicated, visiting, lines) {
            return false;
        }
        if lines.len() > 1 {
            return true;
        }
    }
    true
}

/// What one named member's body holds: the requests written in it, and the
/// names it calls that might be siblings.
#[derive(Debug, Default)]
struct MemberBody {
    request_lines: Vec<u32>,
    /// Every name called as `this.<name>()` or as a bare `<name>()`. Filtered
    /// against the module's own members when the graph is closed, so a call to
    /// an import or to a local helper simply reaches nothing.
    calls: HashSet<String>,
}

/// One named member being walked. Anonymous functions open no frame: a request
/// or a delegation written inside a callback is written by the member that
/// builds the callback, which is the member a site calls.
struct MemberWalk<'a> {
    source_map: &'a Lrc<SourceMap>,
    stack: Vec<String>,
    bodies: HashMap<String, MemberBody>,
    /// Names the module declares more than once. Dropped, and poisonous to
    /// anything that delegates to them.
    duplicated: HashSet<String>,
}

impl MemberWalk<'_> {
    fn walk_named<N: VisitWith<Self>>(&mut self, name: Option<String>, body: &N) {
        let Some(name) = name else {
            body.visit_with(self);
            return;
        };
        if self.bodies.contains_key(&name) {
            self.duplicated.insert(name.clone());
        }
        self.bodies.entry(name.clone()).or_default();
        self.stack.push(name);
        body.visit_with(self);
        self.stack.pop();
    }

    fn current(&mut self) -> Option<&mut MemberBody> {
        let name = self.stack.last()?.clone();
        self.bodies.get_mut(&name)
    }

    fn line_of(&self, call: &CallExpr) -> u32 {
        u32::try_from(self.source_map.lookup_char_pos(call.span().lo).line).unwrap_or(0)
    }
}

impl Visit for MemberWalk<'_> {
    fn visit_class_method(&mut self, node: &ClassMethod) {
        // Getters and setters are not how a client spells a request, the same
        // exclusion the request-member index applies.
        let name = match node.kind {
            MethodKind::Method => prop_name_text(&node.key),
            _ => None,
        };
        self.walk_named(name, &node.function.body);
    }

    fn visit_private_method(&mut self, node: &PrivateMethod) {
        // A `#private` member cannot be a delegation target by name from
        // anywhere this pass can read, so it opens no frame and its contents
        // belong to whatever encloses it.
        node.function.body.visit_with(self);
    }

    fn visit_fn_decl(&mut self, node: &FnDecl) {
        let name = node.ident.sym.to_string();
        self.walk_named(Some(name), &node.function.body);
    }

    fn visit_class_prop(&mut self, node: &ClassProp) {
        // `handler = async () => { … }` is a member with a name, spelled as a
        // property. Its arrow opens no frame of its own, so the body lands on
        // the property's name.
        let name = prop_name_text(&node.key);
        match (&name, node.value.as_deref()) {
            (Some(_), Some(Expr::Arrow(arrow))) => self.walk_named(name, &arrow.body),
            (Some(_), Some(Expr::Fn(fn_expr))) => self.walk_named(name, &fn_expr.function.body),
            _ => node.visit_children_with(self),
        }
    }

    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        let (Pat::Ident(binding), Some(init)) = (&node.name, &node.init) else {
            node.visit_children_with(self);
            return;
        };
        let name = binding.id.sym.to_string();
        match &**init {
            Expr::Arrow(arrow) => self.walk_named(Some(name), &arrow.body),
            Expr::Fn(fn_expr) => self.walk_named(Some(name), &fn_expr.function.body),
            _ => node.visit_children_with(self),
        }
    }

    fn visit_call_expr(&mut self, node: &CallExpr) {
        if self.stack.last().is_some() {
            if issues_request(node) {
                let line = self.line_of(node);
                if let Some(body) = self.current() {
                    body.request_lines.push(line);
                }
            }
            if let Some(name) = sibling_call_name(node)
                && let Some(body) = self.current()
            {
                body.calls.insert(name);
            }
        }
        node.visit_children_with(self);
    }
}

/// Does this call issue an HTTP request?
///
/// A request-options bag settles it. Otherwise an HTTP-verb callee property
/// counts only beside a string or template argument: a bare verb property is
/// equally how a cache, a store or a map spells a read, and `this.cache.get(()
/// => this.fetchThings())` is a delegation.
fn issues_request(call: &CallExpr) -> bool {
    if request_options_argument(call).is_some() {
        return true;
    }
    verb_from_callee_property(callee_property(call).as_deref()).is_some() && has_stringish_arg(call)
}

fn has_stringish_arg(call: &CallExpr) -> bool {
    call.args.iter().any(|arg| {
        arg.spread.is_none() && matches!(&*arg.expr, Expr::Lit(Lit::Str(_)) | Expr::Tpl(_))
    })
}

/// The name a call might be reaching a sibling member by: `this.<name>(…)` or
/// a bare `<name>(…)`. A deeper receiver (`this.cache.get()`, `other.load()`)
/// is a call on something the module holds, not a call on the module's own
/// member, and names nothing here.
fn sibling_call_name(call: &CallExpr) -> Option<String> {
    let Callee::Expr(callee) = &call.callee else {
        return None;
    };
    match &**callee {
        Expr::Ident(ident) => Some(ident.sym.to_string()),
        Expr::Member(member) if matches!(&*member.obj, Expr::This(_)) => match &member.prop {
            MemberProp::Ident(ident) => Some(ident.sym.to_string()),
            MemberProp::Computed(computed) => match &*computed.expr {
                Expr::Lit(Lit::Str(literal)) => Some(literal.value.to_string()),
                _ => None,
            },
            MemberProp::PrivateName(_) => None,
        },
        _ => None,
    }
}

fn callee_property(call: &CallExpr) -> Option<String> {
    let Callee::Expr(callee) = &call.callee else {
        return None;
    };
    let Expr::Member(member) = &**callee else {
        return None;
    };
    match &member.prop {
        MemberProp::Ident(ident) => Some(ident.sym.to_string()),
        MemberProp::Computed(computed) => match &*computed.expr {
            Expr::Lit(Lit::Str(literal)) => Some(literal.value.to_string()),
            _ => None,
        },
        MemberProp::PrivateName(_) => None,
    }
}

fn prop_name_text(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(literal) => Some(literal.value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use swc_common::{FileName, GLOBALS, Globals, Mark};
    use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
    use swc_ecma_transforms_base::resolver;
    use swc_ecma_visit::VisitMutWith;

    fn members(source: &str) -> DispatchMemberIndex {
        let source_map: Lrc<SourceMap> = Default::default();
        let source_file = source_map.new_source_file(
            Lrc::new(FileName::Real(PathBuf::from("client.ts"))),
            source.to_string(),
        );
        let lexer = Lexer::new(
            Syntax::Typescript(TsSyntax {
                decorators: true,
                ..Default::default()
            }),
            Default::default(),
            StringInput::from(&*source_file),
            None,
        );
        let mut module = Parser::new_from(lexer)
            .parse_module()
            .expect("fixture parses");
        GLOBALS.set(&Globals::new(), || {
            let unresolved = Mark::new();
            let top_level = Mark::new();
            module.visit_mut_with(&mut resolver(unresolved, top_level, true));
        });
        collect_dispatch_members(&module, &source_map)
    }

    fn line_of(source: &str, member: &str) -> Option<u32> {
        members(source)
            .get(member)
            .map(|member| member.request_line)
    }

    #[test]
    fn a_member_whose_own_body_issues_one_request_states_its_line() {
        let source = r#"
class ApiClient {
  async searchByIntent(query: string) {
    return fetch(this.lambdaUrl, {
      method: "POST",
      body: JSON.stringify({ action: "search-by-intent", query }),
    });
  }
}
"#;
        assert_eq!(line_of(source, "searchByIntent"), Some(4));
    }

    #[test]
    fn a_member_reaches_the_request_of_the_sibling_it_delegates_to() {
        let source = r#"
class ApiClient {
  async getAllRepoData() {
    return this.cache.get(() => this.fetchCrossRepoData());
  }

  private async fetchCrossRepoData() {
    const response = await fetch(this.lambdaUrl, {
      method: "POST",
      body: JSON.stringify({ action: "get-cross-repo-data" }),
    });
    return response.json();
  }
}
"#;
        // Both members answer the same request, and the cache read is not one:
        // it is a verb property with no string argument.
        assert_eq!(line_of(source, "getAllRepoData"), Some(8));
        assert_eq!(line_of(source, "fetchCrossRepoData"), Some(8));
    }

    #[test]
    fn a_member_reaches_a_request_two_delegations_away() {
        let source = r#"
class ApiClient {
  async findService(name: string) {
    const repos = await this.getAllRepoData();
    return matchService(repos, name);
  }

  async getAllRepoData() {
    return this.cache.get(() => this.fetchCrossRepoData());
  }

  private async fetchCrossRepoData() {
    return fetch(this.lambdaUrl, { method: "POST", body: "{}" });
  }
}
"#;
        assert_eq!(line_of(source, "findService"), Some(13));
    }

    #[test]
    fn a_second_fetch_with_no_options_bag_is_not_a_second_request() {
        // The staged read: a bare `fetch(url)` states no options and no verb
        // property, so the member still reaches exactly one request.
        let source = r#"
class ApiClient {
  private async fetchCrossRepoData() {
    const response = await fetch(this.lambdaUrl, {
      method: "POST",
      body: JSON.stringify({ action: "get-cross-repo-data" }),
    });
    const data = await response.json();
    if (data.staged_url) {
      const staged = await fetch(data.staged_url);
      return staged.json();
    }
    return data;
  }
}
"#;
        assert_eq!(line_of(source, "fetchCrossRepoData"), Some(4));
    }

    #[test]
    fn a_member_reaching_two_requests_states_nothing() {
        let source = r#"
class ApiClient {
  async sync() {
    await fetch(this.url, { method: "POST", body: "a" });
    await fetch(this.url, { method: "PUT", body: "b" });
  }
}
"#;
        assert!(!members(source).contains_key("sync"));
    }

    #[test]
    fn a_member_reaching_no_request_states_nothing() {
        let source = r#"
class ApiClient {
  displayName(repo: Repo) {
    return repo.service ?? repo.name;
  }
}
"#;
        assert!(!members(source).contains_key("displayName"));
    }

    #[test]
    fn a_name_the_module_declares_twice_is_dropped() {
        let source = r#"
class One {
  async load() {
    return fetch(this.url, { method: "GET" });
  }
}
class Two {
  async load() {
    return fetch(this.other, { method: "POST", body: "x" });
  }
}
"#;
        assert!(!members(source).contains_key("load"));
    }

    #[test]
    fn a_member_delegating_to_a_duplicated_name_is_dropped() {
        let source = r#"
class One {
  async outer() {
    return this.load();
  }
  async load() {
    return fetch(this.url, { method: "GET" });
  }
}
class Two {
  async load() {
    return fetch(this.other, { method: "POST", body: "x" });
  }
}
"#;
        assert!(!members(source).contains_key("outer"));
    }

    #[test]
    fn a_verb_property_with_a_path_literal_is_a_request() {
        let source = r#"
export function listThings(client: Client) {
  return client.get("/api/things");
}
"#;
        assert_eq!(line_of(source, "listThings"), Some(3));
    }

    #[test]
    fn a_module_function_wrapper_is_indexed_by_its_binding_name() {
        let source = r#"
const postAction = async (path: string) =>
  fetch(`${base}${path}`, {
    method: "POST",
    body: JSON.stringify({ action: "store-metadata" }),
  });
"#;
        assert_eq!(line_of(source, "postAction"), Some(3));
    }

    #[test]
    fn two_members_calling_each_other_do_not_recurse_forever() {
        let source = r#"
class Client {
  async a() {
    return this.b();
  }
  async b() {
    await this.a();
    return fetch(this.url, { method: "POST", body: "x" });
  }
}
"#;
        assert_eq!(line_of(source, "a"), Some(8));
        assert_eq!(line_of(source, "b"), Some(8));
    }
}
