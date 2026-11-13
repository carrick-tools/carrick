//! Large source files, generated (carrick#2308).
//!
//! Each is a shape the language defines for holding many requests or routes in
//! one file, written from that definition and copied from nowhere. The right
//! answer about one is a function of the generator's arguments, so nothing
//! large is committed: `n` is how many requests or routes the file holds, and
//! `tag` names its paths after the test that generated it.
//!
//! Read by the unit tests of `src/agents/source_parts.rs` and by
//! `tests/ask_in_parts_test.rs`. Receivers come from the invented installed
//! packages `fixture-table` and `fixture-http` (`tests/support/fact_files.rs`),
//! so the fact layer lists their calls (carrick#3711 R2).

#![allow(dead_code)]

use std::fmt::Write as _;

/// One generated file: its text, and the byte offset each of its `n` requests
/// or routes starts at, in source order.
pub struct Shape {
    pub source: String,
    pub starts: Vec<usize>,
}

/// How many names the flat module's one import line brings in.
pub const IMPORTED_NAMES: usize = 600;

/// The name `i` of the flat module's import line. Long, as generated names
/// are, so the one line is most of what a reader would call the file's head.
pub fn imported_name(i: usize) -> String {
    format!("GeneratedRequestAndResponseShapeNumber{i:04}")
}

/// A flat module of `n` exported functions, each making one verb-named call
/// with a `url` literal on an object argument, under one import line of
/// [`IMPORTED_NAMES`] names.
pub fn flat_module(tag: &str, n: usize) -> Shape {
    let mut source = String::from("import { ");
    for i in 0..IMPORTED_NAMES {
        write!(source, "{}, ", imported_name(i)).unwrap();
    }
    source.push_str("} from \"./shapes\";\n");
    source.push_str(
        "import { makeConfigClient } from \"fixture-http\"; const client = makeConfigClient();\n\n",
    );
    let mut starts = Vec::with_capacity(n);
    for i in 0..n {
        writeln!(source, "/** Reads item {i}. */").unwrap();
        writeln!(
            source,
            "export function readItem{i}(id: string): Promise<{}> {{",
            imported_name(i % IMPORTED_NAMES)
        )
        .unwrap();
        source.push_str("  return ");
        starts.push(source.len());
        writeln!(
            source,
            "client.get({{ url: \"/{tag}/items/{i}\", params: {{ id }} }});"
        )
        .unwrap();
        source.push_str("}\n\n");
    }
    Shape { source, starts }
}

/// The handler the route array's route `i` names.
pub fn route_array_handler(i: usize) -> String {
    format!("handleRoute{i}")
}

/// A module whose one exported array literal holds `n` route objects and is
/// most of the file. Route `i` answers `GET /{tag}/routes/{i}` with the
/// function [`route_array_handler`] names, written in the route object.
pub fn route_array(tag: &str, n: usize) -> Shape {
    let mut source = String::from("const SERVICE = \"generated\";\n\n");
    source.push_str("export const routes = [\n");
    let mut starts = Vec::with_capacity(n);
    for i in 0..n {
        source.push_str("  {\n");
        writeln!(source, "    method: \"GET\",").unwrap();
        writeln!(source, "    path: \"/{tag}/routes/{i}\",").unwrap();
        writeln!(
            source,
            "    handler: function {}(req: any, res: any) {{",
            route_array_handler(i)
        )
        .unwrap();
        source.push_str("      ");
        starts.push(source.len());
        writeln!(source, "res.json({{ service: SERVICE, route: {i} }});").unwrap();
        source.push_str("    },\n");
        source.push_str("  },\n");
    }
    source.push_str("];\n");
    Shape { source, starts }
}

/// A module whose one exported function registers `n` routes in its body, each
/// with an inline handler.
pub fn registering_function(tag: &str, n: usize) -> Shape {
    let mut source = String::from("const SERVICE = \"generated\";\n\n");
    source.push_str("export function register(app: import(\"fixture-table\").Table) {\n");
    let mut starts = Vec::with_capacity(n);
    for i in 0..n {
        source.push_str("  ");
        starts.push(source.len());
        writeln!(
            source,
            "app.get(\"/{tag}/registered/{i}\", async (req: any, res: any) => {{"
        )
        .unwrap();
        writeln!(source, "    res.json({{ service: SERVICE, route: {i} }});").unwrap();
        source.push_str("  });\n");
    }
    source.push_str("}\n");
    Shape { source, starts }
}

/// A class with `n` methods that each make one request.
pub fn request_class(tag: &str, n: usize) -> Shape {
    let mut source =
        String::from("import makeClient from \"fixture-http\"; const http = makeClient();\n\n");
    source.push_str("export class GeneratedClient {\n");
    source.push_str("  constructor(private readonly base: string) {}\n\n");
    let mut starts = Vec::with_capacity(n);
    for i in 0..n {
        writeln!(source, "  async loadThing{i}(id: string) {{").unwrap();
        source.push_str("    return ");
        starts.push(source.len());
        writeln!(
            source,
            "http.get(`${{this.base}}/{tag}/things/{i}/${{id}}`);"
        )
        .unwrap();
        source.push_str("  }\n\n");
    }
    source.push_str("}\n");
    Shape { source, starts }
}

/// A module whose `n` route registrations sit between `opening` and `closing`,
/// each line of which is written as given: the statements of whatever
/// construct those lines open and close, under one declaration above it.
/// Each registration ends on the line `  });`.
pub fn statements_inside(tag: &str, n: usize, opening: &str, closing: &str) -> Shape {
    let mut source =
        String::from("import makeTable from \"fixture-table\"; const app = makeTable();\n\n");
    writeln!(source, "{opening}").unwrap();
    let mut starts = Vec::with_capacity(n);
    for i in 0..n {
        source.push_str("  ");
        starts.push(source.len());
        writeln!(
            source,
            "app.get(\"/{tag}/inside/{i}\", async (req: any, res: any) => {{"
        )
        .unwrap();
        writeln!(source, "    res.json({{ route: {i} }});").unwrap();
        source.push_str("  });\n");
    }
    writeln!(source, "{closing}").unwrap();
    Shape { source, starts }
}

/// A module that is one function expression called where it is written, its
/// body the `n` registrations.
pub fn called_function(tag: &str, n: usize) -> Shape {
    statements_inside(tag, n, "(function () {\n  \"use strict\";", "})();")
}

/// A module whose `n` registrations are the body of a top-level `try`.
pub fn try_block(tag: &str, n: usize) -> Shape {
    statements_inside(
        tag,
        n,
        "try {",
        "} catch (error) {\n  console.error(error);\n}",
    )
}

/// Every shape at `n`, by name.
pub fn every_shape(tag: &str, n: usize) -> Vec<(&'static str, Shape)> {
    vec![
        ("flat_module", flat_module(tag, n)),
        ("route_array", route_array(tag, n)),
        ("registering_function", registering_function(tag, n)),
        ("request_class", request_class(tag, n)),
        ("called_function", called_function(tag, n)),
        ("try_block", try_block(tag, n)),
    ]
}
