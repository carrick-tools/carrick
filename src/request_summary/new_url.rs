//! `new URL(input, base)`, read by the URL standard (WHATWG URL, the basic
//! URL parser given a base) over the text the source writes (carrick#4124).
//!
//! The one answer to "what URL does this `new URL` name": the value reader
//! ([`super::Reader`]'s `new_url`) and the guards' `new URL` arm
//! ([`super::guards`]) both call [`resolve_new_url`]. It reads only the
//! subset below and refuses everything else, never guessing:
//!
//! - An absolute input (`scheme://…`) is itself; the base plays no part.
//! - A path from the root (`/x`) keeps the base's origin and nothing else of
//!   it. Only the path is stated, as everywhere a `new URL` is read
//!   (carrick#610): a literal base there is often a placeholder
//!   (`new URL(req.url, "http://localhost")`).
//! - The function's own parameter (or a key of one) is the caller's: what
//!   it holds is read at each caller.
//! - A relative input resolves against a literal absolute base: the base's
//!   last path segment is replaced (`things` on `https://h/v2` is
//!   `https://h/things`, on `https://h/v2/` it is `https://h/v2/things`),
//!   with `.` and `..` segments removed. A relative input on any other base
//!   is refused: the base's path decides where it lands.
//! - A scheme-relative input (`//host/x`) is refused.

use super::{Piece, concat, is_absolute_url};

/// The URL `new URL(input, base)` names, as pieces, or `None` where the
/// source does not state it. `base` is `None` for `new URL(input)`, where
/// the input is the URL.
pub(super) fn resolve_new_url(input: &[Piece], base: Option<&[Piece]>) -> Option<Vec<Piece>> {
    let Some(base) = base else {
        return Some(input.to_vec());
    };
    let first = input.first()?;
    if first.is_parameter() {
        return Some(input.to_vec());
    }
    let Piece::Lit(head) = first else {
        return None;
    };
    if head.starts_with("//") {
        return None;
    }
    if head.starts_with('/') || is_absolute_url(head) {
        return Some(input.to_vec());
    }
    // `g:h`, `mailto:x`, `localhost:3000/x`: a scheme of its own makes the
    // input a URL the subset does not read. A `\` is a `/` to the standard
    // for some schemes only.
    if has_scheme(head) || head.contains('\\') {
        return None;
    }
    let [Piece::Lit(base)] = base else {
        return None;
    };
    let base = BaseUrl::parse(base)?;
    if let [Piece::Lit(input)] = input {
        return Some(vec![Piece::Lit(base.resolve(input))]);
    }
    // A template: its holes are read as written, so a `.` or `..` segment,
    // or a query or a fragment that leads it, is not resolved here.
    if head.is_empty() || head.starts_with(['?', '#']) || input.iter().any(has_dot_segment) {
        return None;
    }
    Some(concat([vec![Piece::Lit(base.directory())], input.to_vec()]))
}

/// Whether `head` opens with a scheme and its `:` (`[A-Za-z][A-Za-z0-9+.-]*:`)
/// before any `/`, `?` or `#`.
fn has_scheme(head: &str) -> bool {
    let Some(colon) = head.find(':') else {
        return false;
    };
    let scheme = &head[..colon];
    !scheme.contains(['/', '?', '#'])
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Whether a literal piece holds a `.` or `..` path segment.
fn has_dot_segment(piece: &Piece) -> bool {
    let Piece::Lit(text) = piece else {
        return false;
    };
    let path = text.split(['?', '#']).next().unwrap_or_default();
    path.split('/').any(|segment| dot(segment).is_some())
}

/// `Some(1)` for a `.` segment, `Some(2)` for `..`, by the standard's
/// spellings (`%2e` for a dot, in either case).
fn dot(segment: &str) -> Option<u8> {
    match segment.to_ascii_lowercase().as_str() {
        "." | "%2e" => Some(1),
        ".." | ".%2e" | "%2e." | "%2e%2e" => Some(2),
        _ => None,
    }
}

/// A literal absolute base, split as the standard splits a hierarchical
/// URL.
struct BaseUrl<'a> {
    /// `scheme://authority`.
    origin: &'a str,
    /// From the first `/` after the authority to the query; `/` when the
    /// base writes none.
    path: &'a str,
    /// From `?` to the fragment, or empty.
    query: &'a str,
}

impl<'a> BaseUrl<'a> {
    /// `None` unless `text` is `scheme://…`.
    fn parse(text: &'a str) -> Option<Self> {
        if text.starts_with("//") || !is_absolute_url(text) {
            return None;
        }
        let without_fragment = text.split('#').next().unwrap_or_default();
        let after_scheme = without_fragment.find("://")? + 3;
        let authority_end = without_fragment[after_scheme..]
            .find(['/', '?'])
            .map_or(without_fragment.len(), |at| after_scheme + at);
        let origin = &without_fragment[..authority_end];
        let rest = &without_fragment[authority_end..];
        let (path, query) = match rest.find('?') {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, ""),
        };
        Some(Self {
            origin,
            path: if path.is_empty() { "/" } else { path },
            query,
        })
    }

    /// The origin and the base's path up to its last `/`: where a relative
    /// path lands.
    fn directory(&self) -> String {
        let end = self.path.rfind('/').map_or(0, |at| at + 1);
        format!("{}{}", self.origin, &self.path[..end])
    }

    /// `input`, a relative reference written whole, resolved against this
    /// base.
    fn resolve(&self, input: &str) -> String {
        if input.is_empty() {
            return format!("{}{}{}", self.origin, self.path, self.query);
        }
        if input.starts_with('#') {
            return format!("{}{}{}{input}", self.origin, self.path, self.query);
        }
        if input.starts_with('?') {
            return format!("{}{}{input}", self.origin, self.path);
        }
        let split = input.find(['?', '#']).unwrap_or(input.len());
        let (path, rest) = input.split_at(split);
        let end = self.path.rfind('/').map_or(0, |at| at + 1);
        let merged = format!("{}{path}", &self.path[..end]);
        format!("{}{}{rest}", self.origin, remove_dot_segments(&merged))
    }
}

/// A path from the root with its `.` and `..` segments applied, as the
/// standard's path state does: `..` drops the segment before it (never the
/// root), and a trailing `.` or `..` leaves the path ending in `/`.
fn remove_dot_segments(path: &str) -> String {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let last = segments.len() - 1;
    let mut out: Vec<&str> = Vec::new();
    for (index, segment) in segments.into_iter().enumerate() {
        match dot(segment) {
            Some(2) => {
                out.pop();
                if index == last {
                    out.push("");
                }
            }
            Some(_) => {
                if index == last {
                    out.push("");
                }
            }
            None => out.push(segment),
        }
    }
    format!("/{}", out.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(text: &str) -> Piece {
        Piece::Lit(text.to_string())
    }

    fn resolved(input: &str, base: &str) -> Option<String> {
        match resolve_new_url(&[lit(input)], Some(&[lit(base)]))?.as_slice() {
            [Piece::Lit(text)] => Some(text.clone()),
            other => panic!("not one literal: {other:?}"),
        }
    }

    /// The reference-resolution examples the URL standard's test data shares
    /// with RFC 3986 section 5.4, on the base `http://a/b/c/d;p?q`.
    #[test]
    fn relative_references_resolve_as_the_standard_resolves_them() {
        let base = "http://a/b/c/d;p?q";
        for (input, want) in [
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("g#s", "http://a/b/c/g#s"),
            ("g?y#s", "http://a/b/c/g?y#s"),
            (";x", "http://a/b/c/;x"),
            ("g;x", "http://a/b/c/g;x"),
            ("", "http://a/b/c/d;p?q"),
            (".", "http://a/b/c/"),
            ("./", "http://a/b/c/"),
            ("..", "http://a/b/"),
            ("../", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../", "http://a/"),
            ("../../g", "http://a/g"),
            ("../../../g", "http://a/g"),
            ("../../../../g", "http://a/g"),
            ("g.", "http://a/b/c/g."),
            (".g", "http://a/b/c/.g"),
            ("g..", "http://a/b/c/g.."),
            ("..g", "http://a/b/c/..g"),
            ("./../g", "http://a/b/g"),
            ("./g/.", "http://a/b/c/g/"),
            ("g/./h", "http://a/b/c/g/h"),
            ("g/../h", "http://a/b/c/h"),
            ("%2e%2E/g", "http://a/b/g"),
        ] {
            assert_eq!(resolved(input, base).as_deref(), Some(want), "{input}");
        }
    }

    /// carrick#4124: the trailing `/` of the base decides whether its last
    /// segment is kept.
    #[test]
    fn the_base_keeps_its_last_segment_only_behind_a_slash() {
        assert_eq!(
            resolved("things/list", "https://api.example.test/v2/").as_deref(),
            Some("https://api.example.test/v2/things/list")
        );
        assert_eq!(
            resolved("things/list", "https://api.example.test/v2").as_deref(),
            Some("https://api.example.test/things/list")
        );
        assert_eq!(
            resolved("things", "https://api.example.test").as_deref(),
            Some("https://api.example.test/things")
        );
        assert_eq!(
            resolved("things", "https://api.example.test?x=1#top").as_deref(),
            Some("https://api.example.test/things")
        );
    }

    #[test]
    fn an_absolute_input_or_a_root_path_is_the_input_itself() {
        let opaque = Piece::Unknown;
        for input in ["https://h.test/v1/x", "/v1/x"] {
            assert_eq!(
                resolve_new_url(&[lit(input)], Some(std::slice::from_ref(&opaque))),
                Some(vec![lit(input)]),
                "{input}"
            );
        }
        assert_eq!(
            resolve_new_url(&[lit("things")], None),
            Some(vec![lit("things")])
        );
    }

    /// A relative input on a base the source does not write whole, a
    /// scheme-relative input, and a base that is no absolute URL state
    /// nothing.
    #[test]
    fn what_the_subset_does_not_read_is_refused() {
        assert_eq!(
            resolve_new_url(&[lit("things")], Some(&[Piece::Unknown])),
            None
        );
        assert_eq!(
            resolve_new_url(&[lit("things")], Some(&[Piece::Unknown, lit("/v2/")])),
            None
        );
        assert_eq!(
            resolve_new_url(&[lit("//cdn.test/x")], Some(&[lit("https://h.test/")])),
            None
        );
        assert_eq!(
            resolve_new_url(&[lit("things")], Some(&[lit("/v2/")])),
            None
        );
        assert_eq!(
            resolve_new_url(&[lit("things")], Some(&[lit("//h.test/v2/")])),
            None
        );
        // A scheme of the input's own (RFC 3986 5.4.1's `g:h`), and a `\`.
        for input in ["g:h", "things:batchGet", "localhost:3000/x", "a\\b"] {
            assert_eq!(
                resolve_new_url(&[lit(input)], Some(&[lit("https://h.test/v2/")])),
                None,
                "{input}"
            );
        }
        // A `:` past the first segment is no scheme.
        assert_eq!(
            resolved("things/a:b", "https://h.test/v2/").as_deref(),
            Some("https://h.test/v2/things/a:b")
        );
    }

    /// A template's holes stay where they are written, under the base's
    /// directory; a dot segment in it is not resolved.
    #[test]
    fn a_template_lands_under_the_base_directory() {
        let id = Piece::Unknown;
        assert_eq!(
            resolve_new_url(
                &[lit("things/"), id.clone(), lit("/archive")],
                Some(&[lit("https://h.test/v2/list")])
            ),
            Some(vec![
                lit("https://h.test/v2/things/"),
                id.clone(),
                lit("/archive")
            ])
        );
        assert_eq!(
            resolve_new_url(
                &[lit("../things/"), id.clone()],
                Some(&[lit("https://h.test/v2/")])
            ),
            None
        );
        assert_eq!(
            resolve_new_url(&[lit("?q="), id], Some(&[lit("https://h.test/v2/")])),
            None
        );
    }
}
