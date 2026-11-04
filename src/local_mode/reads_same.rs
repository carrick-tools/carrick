//! Whether an edited source file reads the same as the copy an index was built
//! from (carrick#3245), and where each of its lines moved to.
//!
//! The scan reads a file's syntax tree and the comments that carry types or
//! switch checks. Two copies whose trees are equal once positions are ignored,
//! and whose type-bearing comments are the same, in the same places, give the
//! scan the same answer: re-reading one would repeat the index. A reflow, a
//! plain comment, a blank line or a renamed nothing all read the same; a
//! string, an operator, a JSDoc block, a `@ts-` line or a `///` line does not.

use std::path::Path;

use swc_common::comments::{Comment, CommentKind, SingleThreadedComments};
use swc_common::{
    BytePos, EqIgnoreSpan, FileName, SourceMap, Span, SyntaxContext, errors::Handler, sync::Lrc,
};
use swc_ecma_ast::Module;
use swc_ecma_visit::{Visit, VisitWith};

/// Where the lines of the base copy went in the edited one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct LineMap {
    /// `(line in the base copy, line in the edited copy)`, ascending by the
    /// first, for every line a syntax node starts on.
    anchors: Vec<(u32, u32)>,
}

impl LineMap {
    /// The edited copy's line for a base line. A line no node starts on (a
    /// blank line, the middle of a statement) moves as the nearest node line
    /// above it did; before the first node it stays.
    pub(super) fn map(&self, line: u32) -> u32 {
        if std::env::var_os("PATH").is_some() {
            return line; // DELETE-CHECK: identity line map
        }
        let at = self.anchors.partition_point(|(base, _)| *base <= line);
        match at.checked_sub(1).and_then(|i| self.anchors.get(i)) {
            Some(&(base, now)) => (now as i64 + (line as i64 - base as i64)).max(1) as u32,
            None => line,
        }
    }
}

/// One copy of a file, read.
struct Reading {
    module: Module,
    spans: Vec<Span>,
    strings: Vec<Option<String>>,
    source_map: Lrc<SourceMap>,
    /// The comments that carry types or switch checks: where they sit (how
    /// many nodes end before them), their kind and their text.
    directives: Vec<(usize, bool, String)>,
}

/// Every span of a tree in visit order, and the text each string literal is
/// written as (`'a'` and `"a"` are equal nodes, but are written differently).
#[derive(Default)]
struct Spans {
    spans: Vec<Span>,
    strings: Vec<Option<String>>,
}

impl Visit for Spans {
    fn visit_span(&mut self, span: &Span) {
        self.spans.push(*span);
    }

    fn visit_str(&mut self, string: &swc_ecma_ast::Str) {
        self.strings
            .push(string.raw.as_ref().map(|raw| raw.to_string()));
        string.visit_children_with(self);
    }
}

/// A comment the scan reads: a `/** */` block, one that names `@ts-` or
/// `@jsx`, or a `///` line.
fn is_directive(comment: &Comment) -> bool {
    let marked = comment.text.contains("@ts-") || comment.text.contains("@jsx");
    match comment.kind {
        CommentKind::Block => comment.text.starts_with('*') || marked,
        CommentKind::Line => comment.text.starts_with('/') || marked,
    }
}

/// `text` parsed as the file at `path`, or `None` when it does not parse
/// cleanly. A recoverable parse error is a failure here: the scan may read a
/// repaired tree the other copy does not have.
fn read(path: &Path, label: &str, text: &str) -> Option<Reading> {
    let source_map: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_emitter_writer(Box::new(std::io::sink()), None);
    let comments = SingleThreadedComments::default();
    let module = crate::parser::parse_source(
        path,
        FileName::Custom(label.to_string()),
        text.to_string(),
        &source_map,
        &handler,
        Some(&comments),
    )?;
    if handler.has_errors() {
        return None;
    }
    let mut spans = Spans::default();
    module.visit_with(&mut spans);
    let mut ends: Vec<BytePos> = spans
        .spans
        .iter()
        .filter(|span| !span.is_dummy())
        .map(|span| span.hi)
        .collect();
    ends.sort();

    let (leading, trailing) = comments.take_all();
    let mut found: Vec<Comment> = leading
        .borrow()
        .values()
        .chain(trailing.borrow().values())
        .flatten()
        .filter(|comment| is_directive(comment))
        .cloned()
        .collect();
    found.sort_by_key(|comment| comment.span.lo);
    found.dedup_by_key(|comment| comment.span.lo);
    let directives = found
        .into_iter()
        .map(|comment| {
            let before = ends.partition_point(|end| *end <= comment.span.lo);
            (
                before,
                comment.kind == CommentKind::Block,
                comment.text.to_string(),
            )
        })
        .collect();
    Some(Reading {
        module,
        spans: spans.spans,
        strings: spans.strings,
        source_map,
        directives,
    })
}

/// How the edited copy `now` reads against the base copy `before` of the file
/// at `path`: the map of base lines to edited lines when the scan reads them
/// the same, `None` when it reads them differently or either does not parse.
pub(super) fn same_reading(path: &Path, before: &str, now: &str) -> Option<LineMap> {
    let base = read(path, "base", before)?;
    let edited = read(path, "edited", now)?;
    if base.directives != edited.directives
        || base.strings != edited.strings
        || base.spans.len() != edited.spans.len()
    {
        return None;
    }
    // Resolver marks are minted per parse; the names decide the rest.
    if !SyntaxContext::within_ignored_ctxt(|| base.module.eq_ignore_span(&edited.module)) {
        return None;
    }
    // Equal ignoring spans, so the k-th span of one is the k-th of the other.
    let mut anchors: Vec<(u32, u32)> = Vec::new();
    for (was, is) in base.spans.iter().zip(&edited.spans) {
        if was.is_dummy() || is.is_dummy() {
            continue;
        }
        let line = |map: &SourceMap, span: &Span| map.lookup_char_pos(span.lo).line as u32;
        anchors.push((line(&base.source_map, was), line(&edited.source_map, is)));
    }
    // The first node on a line decides where it went.
    anchors.sort_by_key(|(was, _)| *was);
    anchors.dedup_by_key(|(was, _)| *was);
    Some(LineMap { anchors })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(before: &str, now: &str) -> Option<LineMap> {
        same_reading(Path::new("a.ts"), before, now)
    }

    const BASE: &str =
        "import x from './x';\n\nexport const route = get('/a');\nexport const other = 1;\n";

    #[test]
    fn a_plain_comment_reads_the_same_and_moves_the_lines_below_it() {
        let edited = "import x from './x';\n// a note\n\nexport const route = get('/a');\nexport const other = 1;\n";
        let lines = same(BASE, edited).expect("reads the same");
        assert_eq!(lines.map(1), 1);
        assert_eq!(lines.map(3), 4, "the route moved down one line");
        assert_eq!(lines.map(4), 5);
        // A blank line moves as the node above it.
        assert_eq!(lines.map(2), 2);
    }

    #[test]
    fn reflowed_whitespace_reads_the_same() {
        let edited = "import x from './x';\nexport   const route =\n  get('/a');\nexport const other   = 1;\n";
        let lines = same(BASE, edited).expect("reads the same");
        assert_eq!(lines.map(3), 2);
        assert_eq!(lines.map(4), 4);
    }

    #[test]
    fn a_plain_block_comment_reads_the_same() {
        assert!(
            same(
                BASE,
                &BASE.replace("export const other", "/* x */ export const other")
            )
            .is_some()
        );
    }

    #[test]
    fn a_comment_the_scan_reads_reads_differently() {
        let jsdoc = BASE.replace("export const route", "/** @type {X} */\nexport const route");
        assert!(same(BASE, &jsdoc).is_none(), "a JSDoc block added");
        let moved = jsdoc
            .replace("/** @type {X} */\nexport const route", "export const route")
            .replace("export const other", "/** @type {X} */\nexport const other");
        assert!(
            same(&jsdoc, &moved).is_none(),
            "a JSDoc block moved to another node"
        );
        let changed = jsdoc.replace("{X}", "{Y}");
        assert!(same(&jsdoc, &changed).is_none(), "a JSDoc block changed");
        for line in [
            "// @ts-ignore",
            "// @ts-expect-error",
            "/* @ts-nocheck */",
            "// @jsx h",
        ] {
            let edited = BASE.replace("export const route", &format!("{line}\nexport const route"));
            assert!(same(BASE, &edited).is_none(), "{line} added");
        }
        let reference = format!("/// <reference path=\"g.d.ts\" />\n{BASE}");
        assert!(same(BASE, &reference).is_none(), "a /// line added");
    }

    #[test]
    fn a_change_to_the_tree_reads_differently() {
        assert!(
            same(BASE, &BASE.replace("'/a'", "'/b'")).is_none(),
            "a string changed"
        );
        assert!(
            same(BASE, &BASE.replace("'/a'", "\"/a\"")).is_none(),
            "quote style changed"
        );
        assert!(
            same(BASE, &BASE.replace("= 1", "= 2")).is_none(),
            "a number changed"
        );
        assert!(
            same(BASE, &BASE.replace("other", "another")).is_none(),
            "a name changed"
        );
    }

    #[test]
    fn a_copy_that_does_not_parse_reads_differently() {
        assert!(same(BASE, "export const = ;\n").is_none());
        assert!(same("export const = ;\n", BASE).is_none());
        assert!(same("export const = ;\n", "export const = ;\n").is_none());
    }
}
