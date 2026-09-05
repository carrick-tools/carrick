//! The request shape a wrapper module fixes for every call site that delegates
//! to it: the HTTP method, and whether the request carries a body.
//!
//! Cross-file wrapper-site resolution (#369/#370) resolves the TARGET of a call
//! that delegates to a same-repo request wrapper, because the wrapper's source
//! is injected into the analyzing prompt. The METHOD was never resolved the
//! same way: a wrapper that hardcodes `method: "POST"` inside its own `fetch`
//! tells the delegating site nothing, the model emits no method for the site,
//! and `normalize_consumer_method` falls back to `GET`. Every resolved site of
//! a POST-only client is then indexed as a GET (carrick-cloud#386).
//!
//! The method is a structural fact of the wrapper's own request call, so it is
//! read off the AST rather than asked for. What is read is deliberately narrow:
//!
//! - A call counts as a REQUEST only on a structural signature — an HTTP-verb
//!   callee property (`client.post(...)`), or an object-literal argument
//!   carrying at least one of `method` / `headers` / `body` / `data`, which is
//!   the shape of a request-options bag in any client library. Response
//!   handling on the same module (`response.json()`, `response.text()`) raises
//!   an HTTP candidate too, and must not be mistaken for a second request with
//!   an unreadable method.
//! - A request whose method is not a literal (`fetch(url, { method })` — the
//!   wrapper parameterizes it, so the SITE's argument is the real method) makes
//!   the whole module's shape unknown. The site keeps whatever extraction gave
//!   it.
//! - Requests that disagree on the method make the module's shape unknown: a
//!   delegating site could be reaching either.
//!
//! No library, framework or package name appears anywhere in here — the rules
//! are the shape of the syntax, not a list of clients.

use swc_ecma_ast::*;

use crate::type_manifest::{is_http_method, normalize_manifest_method};

/// The request a wrapper module issues, as far as it can be read off the AST.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapperRequestShape {
    /// Normalized (uppercase) HTTP method every request in the module uses.
    pub method: String,
    /// Whether the request carries a body: `Some(true)` / `Some(false)` when
    /// the argument list settles it, `None` when it does not. Only a definite
    /// `Some(false)` is acted on downstream, so an unreadable argument list
    /// never deletes a real payload anchor.
    pub has_body: Option<bool>,
}

/// What one candidate call site says about the module's request shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RequestShapeSignal {
    /// Not a request at all (no request-options bag, no HTTP-verb callee) —
    /// contributes nothing. Response handling lands here.
    #[default]
    NotARequest,
    /// A request whose method cannot be read as a literal. Poisons the module:
    /// the wrapper parameterizes its method, so only the call site knows it.
    Unreadable,
    /// A request with a literal method.
    Known(WrapperRequestShape),
}

/// The HTTP-verb callee properties a request may be spelled with
/// (`client.post(url, body)`). `is_http_method` is the single definition of
/// what an HTTP method is; `connect`/`trace` are excluded because a bare
/// `.connect(...)` is overwhelmingly a transport/database call, not a request.
pub(crate) fn verb_from_callee_property(prop: Option<&str>) -> Option<String> {
    let prop = prop?;
    let upper = prop.to_uppercase();
    if matches!(
        upper.as_str(),
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
    ) {
        Some(upper)
    } else {
        None
    }
}

/// The literal string an expression evaluates to, or `None` when it is not a
/// literal (an identifier, a member expression, an interpolated template).
pub(crate) fn literal_string(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(Lit::Str(s)) => Some(s.value.to_string()),
        Expr::Tpl(tpl) if tpl.exprs.is_empty() && tpl.quasis.len() == 1 => {
            Some(tpl.quasis[0].raw.to_string())
        }
        Expr::Paren(paren) => literal_string(&paren.expr),
        Expr::TsAs(as_expr) => literal_string(&as_expr.expr),
        Expr::TsConstAssertion(assertion) => literal_string(&assertion.expr),
        _ => None,
    }
}

/// The property name of an object-literal member, when it is a plain key.
fn prop_key_name(prop: &PropOrSpread) -> Option<String> {
    let PropOrSpread::Prop(prop) = prop else {
        return None;
    };
    let key = match &**prop {
        Prop::KeyValue(kv) => &kv.key,
        Prop::Shorthand(ident) => return Some(ident.sym.to_string()),
        Prop::Method(method) => &method.key,
        Prop::Getter(getter) => &getter.key,
        Prop::Setter(setter) => &setter.key,
        Prop::Assign(_) => return None,
    };
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(s) => Some(s.value.to_string()),
        _ => None,
    }
}

/// The value expression of an object-literal member named `name`, or `None`
/// when the member is absent or a shorthand (`{ method }` — present, but its
/// value is a binding, not a literal).
pub(crate) fn prop_value<'a>(obj: &'a ObjectLit, name: &str) -> Option<Option<&'a Expr>> {
    for prop in &obj.props {
        if prop_key_name(prop).as_deref() != Some(name) {
            continue;
        }
        let PropOrSpread::Prop(prop) = prop else {
            continue;
        };
        return Some(match &**prop {
            Prop::KeyValue(kv) => Some(&*kv.value),
            _ => None,
        });
    }
    None
}

/// Whether an object literal looks like a request-options bag: it carries at
/// least one of the four keys every HTTP client spells the same way.
pub(crate) fn is_request_options(obj: &ObjectLit) -> bool {
    obj.props.iter().any(|prop| {
        matches!(
            prop_key_name(prop).as_deref(),
            Some("method") | Some("headers") | Some("body") | Some("data")
        )
    })
}

/// The request-options bag among a call's arguments: the one object literal
/// that carries a request-options key, whatever position it sits at and
/// whatever else the call is passed.
///
/// This used to require the bag to be the call's ONLY object literal, which
/// read a paginating helper — `page(Schema, url, { page, limit }, { method,
/// headers }, …)` — as not a request at all, while the same client's plain
/// helper one line below was read (carrick#675). A literal carrying none of
/// the four keys states nothing about the request, so it cannot make the one
/// that does ambiguous.
///
/// Two literals that BOTH carry a request-options key is still nothing this
/// reads: a payload spelled `{ body: … }` beside a bag spelled `{ headers: … }`
/// leaves which one configures the request a guess, and the call is dropped.
pub(crate) fn request_options_argument(call: &CallExpr) -> Option<(usize, &ObjectLit)> {
    let mut found: Option<(usize, &ObjectLit)> = None;
    for (index, arg) in call.args.iter().enumerate() {
        if arg.spread.is_some() {
            continue;
        }
        if let Expr::Object(obj) = &*arg.expr
            && is_request_options(obj)
        {
            if found.is_some() {
                return None;
            }
            found = Some((index, obj));
        }
    }
    found
}

/// Read what one call site says about its module's request shape.
///
/// `callee_property` is the property the call was made through (`post` in
/// `client.post(...)`), as the candidate scanner already recorded it.
pub fn call_request_shape(call: &CallExpr, callee_property: Option<&str>) -> RequestShapeSignal {
    let verb = verb_from_callee_property(callee_property);
    let options = request_options_argument(call);

    // Not request-shaped: no verb, no options bag. Response handling
    // (`response.json()`), config builders and everything else land here and
    // contribute nothing.
    if verb.is_none() && options.is_none() {
        return RequestShapeSignal::NotARequest;
    }

    // The method: an explicit literal in the options bag wins over the verb the
    // call was spelled with, because a client that accepts both is configured
    // by the bag.
    let method = match options {
        Some((_, obj)) => match prop_value(obj, "method") {
            // A `method` key whose value is not a string literal is the
            // parameterized wrapper: only the delegating site knows the method.
            Some(value) => match value.and_then(literal_string) {
                Some(literal) => {
                    let normalized = normalize_manifest_method(&literal);
                    if !is_http_method(&normalized) {
                        return RequestShapeSignal::Unreadable;
                    }
                    normalized
                }
                None => return RequestShapeSignal::Unreadable,
            },
            // No `method` key at all. The verb the call was spelled with is the
            // method; without one this module's shape stays unknown, even
            // though a bag with no `method` is a GET everywhere it is written.
            //
            // The two readers of that fact want different things. Here the fold
            // propagates a method AND a body-presence to delegating sites that
            // state no URL of their own, so a wrong `GET` also strips their
            // payload anchor, and the sites are only reachable through a module
            // whose other requests may disagree. `imported_request_member`
            // reads one member's own call, which has already stated a URL and a
            // request-options object beside it, so there the same absence is
            // read as the GET it is.
            None => match verb {
                Some(verb) => verb,
                None => return RequestShapeSignal::Unreadable,
            },
        },
        None => match verb {
            Some(verb) => verb,
            None => return RequestShapeSignal::Unreadable,
        },
    };

    RequestShapeSignal::Known(WrapperRequestShape {
        method,
        has_body: body_presence(call, options),
    })
}

/// Whether the call carries a request body.
///
/// `Some(true)` when an options bag names one, or when a verb-spelled call
/// passes a positional payload after the URL. `Some(false)` when the argument
/// list is exhausted by the URL and an options bag that names no body.
/// `None` when neither is settled.
fn body_presence(call: &CallExpr, options: Option<(usize, &ObjectLit)>) -> Option<bool> {
    let positional = call.args.len();
    if let Some((index, obj)) = options {
        if prop_value(obj, "body").is_some() || prop_value(obj, "data").is_some() {
            return Some(true);
        }
        // A third argument beside the URL and the options bag is a payload this
        // does not model; only the plain `(url, options)` shape settles it.
        if positional <= index + 1 {
            return Some(false);
        }
        return None;
    }
    // Verb-spelled with no options bag: `client.get(url)` sends no body,
    // `client.post(url, payload)` does.
    match positional {
        0 | 1 => Some(false),
        _ => Some(true),
    }
}

/// The shape a whole module fixes, folded over its call sites.
///
/// `None` — no propagation — whenever the module issues no readable request,
/// issues one whose method cannot be read, or issues requests that disagree.
/// Body presence collapses to `None` on disagreement, which downstream reads as
/// "do not touch the payload anchor".
pub fn fold_module<'a>(
    signals: impl IntoIterator<Item = &'a RequestShapeSignal>,
) -> Option<WrapperRequestShape> {
    let mut folded: Option<WrapperRequestShape> = None;
    for signal in signals {
        match signal {
            RequestShapeSignal::NotARequest => continue,
            RequestShapeSignal::Unreadable => return None,
            RequestShapeSignal::Known(shape) => match &mut folded {
                None => folded = Some(shape.clone()),
                Some(acc) => {
                    if acc.method != shape.method {
                        return None;
                    }
                    if acc.has_body != shape.has_body {
                        acc.has_body = None;
                    }
                }
            },
        }
    }
    folded
}

/// The shape one importing file may propagate, folded over every wrapper module
/// its imports resolved to. An unknown shape on any of them makes the whole
/// thing unknown: the file could be delegating to either.
pub fn fold_wrappers<'a>(
    shapes: impl IntoIterator<Item = Option<&'a WrapperRequestShape>>,
) -> Option<WrapperRequestShape> {
    let mut folded: Option<WrapperRequestShape> = None;
    let mut any = false;
    for shape in shapes {
        any = true;
        let shape = shape?;
        match &mut folded {
            None => folded = Some(shape.clone()),
            Some(acc) => {
                if acc.method != shape.method {
                    return None;
                }
                if acc.has_body != shape.has_body {
                    acc.has_body = None;
                }
            }
        }
    }
    if any { folded } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swc_scanner::SwcScanner;
    use std::path::PathBuf;

    /// Fold the request shape of a module the way the orchestrator does: over
    /// the HTTP candidates the gatekeeper raised for it.
    fn module_shape(content: &str) -> Option<WrapperRequestShape> {
        let scanner = SwcScanner::new();
        let result = scanner.scan_content(&PathBuf::from("client.ts"), content, &[], &[]);
        assert!(!result.parse_failed, "fixture must parse");
        fold_module(result.candidates.iter().map(|c| &c.request_shape))
    }

    #[test]
    fn a_hardcoded_method_is_read_off_the_options_bag() {
        // The carrick-cloud#386 shape: every request the wrapper issues is a
        // POST, and no delegating site says so.
        let shape = module_shape(
            r#"
export class Client {
  async load(id: string) {
    const response = await fetch(this.url, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ id }),
    });
    return response.json();
  }
}
"#,
        );
        assert_eq!(
            shape,
            Some(WrapperRequestShape {
                method: "POST".to_string(),
                has_body: Some(true),
            })
        );
    }

    #[test]
    fn response_handling_does_not_poison_the_module() {
        // `response.json()` / `response.text()` raise HTTP candidates of their
        // own. Reading them as requests with an unreadable method would make
        // every real wrapper unknown — which is the whole population #386 is
        // about.
        let shape = module_shape(
            r#"
export async function load(id: string) {
  const response = await fetch(`${BASE}/things/${id}`, { method: "DELETE" });
  if (!response.ok) {
    const text = await response.text();
    throw new Error(text);
  }
  return response.json();
}
"#,
        );
        assert_eq!(
            shape,
            Some(WrapperRequestShape {
                method: "DELETE".to_string(),
                has_body: Some(false),
            })
        );
    }

    #[test]
    fn a_parameterized_method_leaves_the_module_unknown() {
        // The wrapper takes the method from its caller, so only the delegating
        // site knows it — exactly the case the resolution prompt already
        // handles, and the one this must not overwrite.
        assert_eq!(
            module_shape(
                r#"
export async function request(method: string, path: string) {
  return fetch(`${BASE}${path}`, { method, headers: {} });
}
"#,
            ),
            None
        );
    }

    #[test]
    fn requests_that_disagree_leave_the_module_unknown() {
        assert_eq!(
            module_shape(
                r#"
export async function read(id: string) {
  return fetch(`${BASE}/things/${id}`, { method: "GET" });
}
export async function write(body: unknown) {
  return fetch(`${BASE}/things`, { method: "POST", body: JSON.stringify(body) });
}
"#,
            ),
            None
        );
    }

    #[test]
    fn a_module_with_no_request_is_unknown() {
        assert_eq!(
            module_shape("export function label(v: string) { return v.trim(); }"),
            None
        );
    }

    #[test]
    fn a_lowercase_literal_method_is_normalized() {
        let shape = module_shape(
            r#"
export async function send(payload: unknown) {
  return client.request({ method: "put", url: `${BASE}/things`, data: payload });
}
"#,
        );
        assert_eq!(
            shape,
            Some(WrapperRequestShape {
                method: "PUT".to_string(),
                has_body: Some(true),
            })
        );
    }

    #[test]
    fn a_verb_spelled_call_carries_its_method_and_payload() {
        let shape = module_shape(
            r#"
export async function create(payload: unknown) {
  return client.post(`${BASE}/things`, payload);
}
"#,
        );
        assert_eq!(
            shape,
            Some(WrapperRequestShape {
                method: "POST".to_string(),
                has_body: Some(true),
            })
        );
    }

    #[test]
    fn a_verb_spelled_call_with_only_a_url_sends_no_body() {
        let shape = module_shape(
            r#"
export async function list() {
  return client.get(`${BASE}/things`);
}
"#,
        );
        assert_eq!(
            shape,
            Some(WrapperRequestShape {
                method: "GET".to_string(),
                has_body: Some(false),
            })
        );
    }

    #[test]
    fn an_off_enum_literal_method_leaves_the_module_unknown() {
        assert_eq!(
            module_shape(
                r#"export async function go() { return fetch(URL, { method: "SUBSCRIBE" }); }"#,
            ),
            None
        );
    }

    #[test]
    fn one_unknown_wrapper_makes_the_importer_unknown() {
        let known = WrapperRequestShape {
            method: "POST".to_string(),
            has_body: Some(true),
        };
        assert_eq!(fold_wrappers([Some(&known)]), Some(known.clone()));
        assert_eq!(fold_wrappers([Some(&known), None]), None);
        // No wrapper at all is not "agreed": there is nothing to propagate.
        assert_eq!(fold_wrappers([]), None);
    }

    #[test]
    fn wrappers_that_agree_on_the_method_but_not_the_body_keep_the_method() {
        let with_body = WrapperRequestShape {
            method: "POST".to_string(),
            has_body: Some(true),
        };
        let without_body = WrapperRequestShape {
            method: "POST".to_string(),
            has_body: Some(false),
        };
        assert_eq!(
            fold_wrappers([Some(&with_body), Some(&without_body)]),
            Some(WrapperRequestShape {
                method: "POST".to_string(),
                has_body: None,
            })
        );
    }
}
