//! Requests a function builds and returns for a caller to send
//! (carrick#3470).
//!
//! ```ts
//! export function buildOrderLookup(baseUrl: string, orderId: string) {
//!   return { url: `${baseUrl}/api/v2/orders/${orderId}`, method: "GET" };
//! }
//! ```
//!
//! No call is made here, so no site reaches a labelled export and nothing
//! else states a row. Read off the syntax alone, with no name of a library:
//!
//! - **What is one.** An object literal a function returns
//!   ([`crate::return_flow::returned_objects`]: a `return` argument, an
//!   arrow's expression body, or a local given that one literal) with a
//!   `url` key and at least one key a request's options carry
//!   ([`crate::wrapper_request_shape::is_request_options`]). A literal handed
//!   to a call is that call's, and is never read here.
//! - **Its target.** The `url` value as the request summaries read it,
//!   rendered as they render a target ([`super::render_target`]). A
//!   parameter at the head, followed by a `/`-led path, reads as an opaque
//!   base (R1, David, 2026-11-07), as an environment base does: here only,
//!   never for a call, where a wrapper prefix over an unread argument stays
//!   a lead.
//! - **Its method.** The literal at `method`, GET where no key can write one
//!   ([`crate::wrapper_request_shape::built_request_method`]); a method
//!   nothing here reads states nothing.
//! - **Its body.** The `body` or `data` value, typed where it is written as
//!   a request body is.
//! - **Only where the function has no caller in its service.** A builder a
//!   caller takes is that caller's request to send (a follow-up reads the
//!   transport's URL through the builder's return).
//!
//! Each becomes a candidate row ([`crate::site_rows::descriptor_rows`],
//! source `request_descriptor`): the code states the target, not that
//! anything sends it.

use std::path::Path;

use swc_common::Spanned;
use swc_ecma_ast::{ObjectLit, Prop, PropOrSpread};

use super::decided_sites::relative;
use super::site_record::AnchorAt;
use super::{FileIr, Piece, Reader, Root, Scope, Site, Value, render_target};
use crate::wrapper_request_shape::{built_request_method, is_request_options, prop_key_name};

/// A returned object literal shaped like a request, as the body reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BuiltRequest {
    /// The literal.
    site: Site,
    /// The `url` value.
    url: Vec<Piece>,
    /// Upper case.
    method: String,
    /// The `body` (or `data`) value's expression.
    body: Option<Site>,
}

impl BuiltRequest {
    /// `object` read in `scope`, when it is shaped like a request.
    pub(super) fn read(reader: &Reader<'_>, object: &ObjectLit, scope: &Scope<'_>) -> Option<Self> {
        let keys: Vec<String> = object.props.iter().filter_map(prop_key_name).collect();
        if !keys.iter().any(|key| key == "url") || !is_request_options(object) {
            return None;
        }
        let method = built_request_method(object)?;
        let Some(Value::Str(url)) = reader.object(object, scope).fields.get("url").cloned() else {
            return None;
        };
        let body = object.props.iter().find_map(|prop| {
            let name = prop_key_name(prop)?;
            if name != "body" && name != "data" {
                return None;
            }
            let PropOrSpread::Prop(prop) = prop else {
                return None;
            };
            match &**prop {
                Prop::KeyValue(kv) => Some(reader.site(kv.value.span())),
                Prop::Shorthand(ident) => Some(reader.site(ident.span)),
                _ => None,
            }
        });
        Some(BuiltRequest {
            site: reader.site(object.span),
            url,
            method,
            body,
        })
    }
}

/// One request a function builds and returns, with nothing in its service
/// calling the function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    /// The object literal.
    pub at: AnchorAt,
    /// Upper case.
    pub method: String,
    /// The target as the request summaries write one: an optional `${base}`
    /// and a `/`-led path.
    pub target: String,
    /// Where the base ends, when the target opens with one.
    pub base_end: Option<Root>,
    /// The body's expression, typed as a request body.
    pub body: Option<AnchorAt>,
}

/// The URL with a leading parameter read as an opaque base, where a `/`-led
/// literal follows it (R1): the base a caller would hand in.
fn with_parameter_base(url: &[Piece]) -> Vec<Piece> {
    let mut url = url.to_vec();
    let base = match url.as_slice() {
        [Piece::Param(index, name), Piece::Lit(next), ..] if next.starts_with('/') => {
            Some((*index, name.clone()))
        }
        [Piece::ParamKey(index, _, name), Piece::Lit(next), ..] if next.starts_with('/') => {
            Some((*index, name.clone()))
        }
        _ => None,
    };
    if let Some((index, name)) = base {
        url[0] = Piece::Opaque {
            text: name.clone(),
            end: Root::Param(index, name),
            link: None,
        };
    }
    url
}

/// The descriptor `built` states, in `file` under `root`.
fn descriptor_of(root: &Path, file: &Path, built: &BuiltRequest) -> Option<Descriptor> {
    let url = with_parameter_base(&built.url);
    let target = render_target(&url)?;
    let base_end = match url.first() {
        Some(Piece::Opaque { end, .. }) => Some(end.clone()),
        _ => None,
    };
    let file = relative(root, file);
    let anchor = |site: &Site| AnchorAt {
        file: file.clone(),
        line: site.line,
        span_start: site.span_start,
        span_end: site.span_end,
    };
    Some(Descriptor {
        at: anchor(&built.site),
        method: built.method.clone(),
        target,
        base_end,
        body: built.body.as_ref().map(anchor),
    })
}

/// Every descriptor of `files`: the requests each function the extractor
/// keys builds and returns, where `has_caller` says nothing in the service
/// calls it. In file, then line order, one per literal.
pub(super) fn of<'f>(
    root: &Path,
    files: impl IntoIterator<Item = (&'f std::path::PathBuf, &'f FileIr)>,
    has_caller: impl Fn(&Path, &str) -> bool,
) -> Vec<Descriptor> {
    let mut out: Vec<Descriptor> = Vec::new();
    for (path, file) in files {
        for (key, function) in &file.functions {
            if function.built_requests.is_empty() || has_caller(path, key) {
                continue;
            }
            out.extend(
                function
                    .built_requests
                    .iter()
                    .filter_map(|built| descriptor_of(root, path, built)),
            );
        }
    }
    out.sort_by(|a, b| {
        (&a.at.file, a.at.line, a.at.span_start).cmp(&(&b.at.file, b.at.line, b.at.span_start))
    });
    out.dedup_by(|a, b| a.at == b.at);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The descriptors of one file `source`, with `called` the functions
    /// something calls.
    fn read(source: &str, called: &[&str]) -> Vec<(String, String, Option<String>)> {
        let path = PathBuf::from("/repo/src/requests.ts");
        let file = super::super::file_ir_of(&path, source).expect("parses");
        of(Path::new("/repo"), [(&path, &file)], |_, key| {
            called.contains(&key)
        })
        .into_iter()
        .map(|found| {
            (
                found.method,
                found.target,
                found.body.map(|body| body.line.to_string()),
            )
        })
        .collect()
    }

    fn row(method: &str, target: &str, body: Option<u32>) -> (String, String, Option<String>) {
        (
            method.to_string(),
            target.to_string(),
            body.map(|line| line.to_string()),
        )
    }

    #[test]
    fn a_returned_request_behind_a_parameter_base_is_one_descriptor() {
        let source = r#"
export function buildOrderLookup(baseUrl: string, orderId: string, token: string) {
  return {
    url: `${baseUrl}/api/v2/orders/${encodeURIComponent(orderId)}`,
    method: "GET",
    headers: { Authorization: `Bearer ${token}` },
  };
}
export const buildOrderCreate = (baseUrl: string, draft: object) => ({
  url: `${baseUrl}/api/v2/orders`,
  method: "POST",
  body: JSON.stringify(draft),
});
"#;
        assert_eq!(
            read(source, &[]),
            [
                row(
                    "GET",
                    "${baseUrl}/api/v2/orders/${encodeURIComponent(orderId)}",
                    None
                ),
                row("POST", "${baseUrl}/api/v2/orders", Some(12)),
            ]
        );
    }

    #[test]
    fn a_request_returned_through_a_local_and_with_no_method_is_a_get() {
        let source = r#"
export function buildPing(token: string) {
  const request = { url: "/api/v2/ping", headers: { token } };
  return request;
}
"#;
        assert_eq!(read(source, &[]), [row("GET", "/api/v2/ping", None)]);
    }

    #[test]
    fn shapes_that_are_no_request_state_nothing() {
        let source = r#"
export function noticeInit(location: string) {
  return { headers: { Location: location } };
}
export function routeTable(handler: () => void) {
  return [{ method: "GET", path: "/health", handler }];
}
export function routeObject(handler: () => void) {
  return { method: "GET", path: "/health", handler };
}
export function buildWholePath(path: string) {
  return { url: path, method: "GET" };
}
export function buildGlued(baseUrl: string) {
  return { url: `${baseUrl}api/v2/orders`, method: "GET" };
}
export function buildUnreadMethod(baseUrl: string, verb: string) {
  return { url: `${baseUrl}/api/v2/orders`, method: verb };
}
export function bareUrl(baseUrl: string) {
  return { url: `${baseUrl}/api/v2/orders` };
}
"#;
        assert_eq!(read(source, &[]), []);
    }

    #[test]
    fn a_literal_handed_to_a_call_is_the_calls_not_a_descriptor() {
        let source = r#"
declare const client: { post(options: object): Promise<unknown> };
export function create() {
  return client.post({ url: "/v1/things", method: "POST", body: "{}" });
}
"#;
        assert_eq!(read(source, &[]), []);
    }

    #[test]
    fn a_builder_something_calls_is_its_callers_request() {
        let source = r#"
export function buildInvoiceLookup(baseUrl: string, id: string) {
  return { url: `${baseUrl}/api/v2/invoices/${id}`, method: "GET" };
}
"#;
        assert_eq!(read(source, &["buildInvoiceLookup"]), []);
        assert_eq!(read(source, &[]).len(), 1);
    }
}
