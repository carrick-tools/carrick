//! The functions an edit's new function may already exist as, through the
//! probe's own path (carrick-cloud#3490).
//!
//! The held side is an indexed blob built by the extractor the index uses;
//! `calls`, which only the call graph fills, is set by name where a test needs
//! it, and so are the intent and the signature the full scan writes. The edited
//! side is `src/b.ts`, read by the probe's own parse ([`additions`]) and ranked
//! by [`with_similar`].

use super::super::contract::SimilarFunction;
use super::super::similar::{BUILDS, Query};
use super::*;

const JOIN: &str = "export function joinRelative(root: string, file: string) {\n  \
    const trimmed = root.endsWith('/') ? root.slice(0, -1) : root;\n  \
    return file.startsWith(trimmed) ? file.slice(trimmed.length + 1) : file;\n}\n";

/// `(caller, callee)` pairs set on the held blob's `calls`.
type Calls<'a> = &'a [(&'a str, &'a str)];

/// One indexed file: the repo it is in (`None` for the edited one), its
/// path, its source and the calls the index records in it.
struct HeldFile<'a> {
    repo: Option<&'a str>,
    file: &'a str,
    source: &'a str,
    calls: Calls<'a>,
    /// Written on every function of the file, as the full scan would.
    intent: Option<&'a str>,
    /// The file the calls name as their callee's, when it is not this one.
    callee_file: Option<&'a str>,
}

fn here<'a>(source: &'a str, calls: Calls<'a>) -> HeldFile<'a> {
    HeldFile {
        repo: None,
        file: "src/a.ts",
        source,
        calls,
        intent: None,
        callee_file: None,
    }
}

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn blob(repo: &str, held: &HeldFile<'_>) -> crate::cloud_storage::CloudRepoData {
    let extracted = crate::pr_functions::extracted_in(
        Path::new(held.file),
        held.file.to_string(),
        held.source.to_string(),
    )
    .expect("held source parses");
    let mut definitions = extracted.extractor.function_definitions;
    for definition in definitions.values_mut() {
        definition.intent = held.intent.map(str::to_string);
    }
    for (caller, callee) in held.calls {
        let callee_line = definitions
            .values()
            .find(|d| d.name == *callee)
            .map_or(0, |d| d.line_number);
        let caller = definitions
            .values_mut()
            .find(|d| d.name == *caller)
            .expect("caller is held");
        caller.calls.push(crate::visitor::FunctionCallRef {
            name: callee.to_string(),
            file_path: held.callee_file.unwrap_or(held.file).to_string(),
            line_number: callee_line,
            call_site_line: 0,
            call_count: 1,
        });
    }
    serde_json::from_value(serde_json::json!({
        "repo_name": repo,
        "endpoints": [],
        "calls": [],
        "mounts": [],
        "apps": {},
        "imported_handlers": [],
        "function_definitions": serde_json::to_value(&definitions).unwrap(),
        "config_json": null,
        "package_json": null,
        "packages": null,
        "last_updated": "2026-11-09T00:00:00Z",
        "commit_hash": "abc",
    }))
    .expect("a blob")
}

/// What the probe gives each function an edit writes as `src/b.ts`.
fn similar(held: &[HeldFile<'_>], edited: &str) -> Vec<(String, Vec<SimilarFunction>)> {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "package.json", "{\"name\":\"svc\"}");
    write(dir.path(), "src/b.ts", edited);
    let mine = super::super::index::repo_label(dir.path());
    let held = Held {
        local: held
            .iter()
            .map(|file| blob(file.repo.unwrap_or(&mine), file))
            .collect(),
        hosted: Default::default(),
    };
    let services = vec![crate::config::Config::default()];
    let none = std::collections::BTreeSet::new();
    let added = additions(&services, dir.path(), "src/b.ts", &none);
    with_similar(&held, &Tree::at(dir.path()), "src/b.ts", added)
        .into_iter()
        .map(|function| (function.name, function.similar))
        .collect()
}

fn only_rows(found: Vec<(String, Vec<SimilarFunction>)>) -> Vec<SimilarFunction> {
    assert_eq!(found.len(), 1, "{found:?}");
    found.into_iter().next().unwrap().1
}

fn names(rows: &[SimilarFunction]) -> Vec<&str> {
    rows.iter().map(|row| row.name.as_str()).collect()
}

#[test]
fn a_copy_under_another_name_comes_back_with_where_it_is() {
    let copy = JOIN.replace("joinRelative", "relativePath");
    let rows = only_rows(similar(&[here(JOIN, &[])], &copy));
    assert_eq!(names(&rows), ["joinRelative"], "{rows:?}");
    assert_eq!(rows[0].file, "src/a.ts");
    assert_eq!(rows[0].line, 1);
    assert!(rows[0].exported);
}

/// A copy kept in another repo, with a local renamed: a row that names that repo.
#[test]
fn a_near_copy_in_another_repo_comes_back_and_names_its_repo() {
    let mirror = JOIN.replace("trimmed", "base");
    let other = HeldFile {
        repo: Some("web-app"),
        file: "lib/paths.ts",
        source: &mirror,
        calls: &[],
        intent: None,
        callee_file: None,
    };
    let copy = JOIN.replace("joinRelative", "relativePath");
    let rows = only_rows(similar(&[other], &copy));
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].repo, "web-app");
    assert_eq!(rows[0].file, "lib/paths.ts");
    assert_eq!(rows[0].name, "joinRelative");
}

/// The old rule read a word overlap and printed only a copy above 0.70 of it:
/// this renamed copy, with its own locals and an extra step, is under it and is
/// still named.
#[test]
fn a_near_copy_under_the_lead_floor_is_listed() {
    let held = JOIN.to_string();
    let renamed = "export function relativePath(dir: string, name: string) {\n  \
        const base = dir.endsWith('/') ? dir.slice(0, -1) : dir;\n  \
        const log = console.debug('relative', name);\n  \
        const cut = base.length + 1;\n  \
        return name.startsWith(base) ? name.slice(cut) : name;\n}\n";
    let rows = only_rows(similar(&[here(&held, &[])], renamed));
    assert_eq!(names(&rows), ["joinRelative"], "{rows:?}");
}

#[test]
fn a_new_function_that_shares_no_word_gets_no_row() {
    let edited = "export function zebra(quartz: Quartz): Zebra {\n  \
        return quartz.stripes.map((stripe) => stripe.width);\n}\n";
    assert_eq!(only_rows(similar(&[here(JOIN, &[])], edited)), []);
}

#[test]
fn a_function_that_calls_the_hit_gets_no_row() {
    let wrapper = "export function relativePath(root: string, file: string) {\n  \
        const trimmed = root.endsWith('/') ? root.slice(0, -1) : root;\n  \
        const rest = file.startsWith(trimmed) ? file.slice(trimmed.length + 1) : file;\n  \
        return joinRelative(trimmed, rest);\n}\n";
    // The wrapper shares the hit's words: without the edge it would be listed.
    assert_eq!(only_rows(similar(&[here(JOIN, &[])], wrapper)), []);
}

/// The hit calls a function spelled like the new one: they are one chain,
/// not two copies.
#[test]
fn a_hit_that_calls_the_new_name_gets_no_row() {
    let held = format!("{JOIN}function relativePath(a: string) {{ return a; }}\n");
    let calls: Calls<'_> = &[("joinRelative", "relativePath")];
    let copy = JOIN.replace("joinRelative", "relativePath");
    let rows = only_rows(similar(&[here(&held, calls)], &copy));
    // The held `relativePath` shares the new one's name; the hit that calls it is gone.
    assert_eq!(names(&rows), ["relativePath"], "{rows:?}");
}

/// Two other files call the hit, and so does a function in its own file, which
/// is not counted. The intent the index holds comes back as it is written.
#[test]
fn a_row_counts_the_other_files_that_call_it_and_carries_its_intent() {
    let hit = "export function joinRelative(root: string, file: string) {\n  \
        return file.startsWith(root) ? file.slice(root.length + 1) : file;\n}\n\
        export function inside(p: string) { return joinRelative('/', p); }\n";
    let first = "export function first(p: string) { return joinRelative('/', p); }\n";
    let second = "export function second(p: string) { return joinRelative('.', p); }\n";
    let held = vec![
        HeldFile {
            file: "src/hit.ts",
            intent: Some("Cuts a root prefix off a file path."),
            ..here(hit, &[("inside", "joinRelative")])
        },
        HeldFile {
            file: "src/first.ts",
            callee_file: Some("src/hit.ts"),
            ..here(first, &[("first", "joinRelative")])
        },
        HeldFile {
            file: "src/second.ts",
            callee_file: Some("src/hit.ts"),
            ..here(second, &[("second", "joinRelative")])
        },
    ];
    let copy = "export function relativePath(root: string, file: string) {\n  \
        return file.startsWith(root) ? file.slice(root.length + 1) : file;\n}\n";
    let rows = only_rows(similar(&held, copy));
    let row = rows
        .iter()
        .find(|r| r.name == "joinRelative")
        .expect("the hit");
    assert_eq!(row.called_from, 2, "{row:?}");
    assert_eq!(
        row.intent.as_deref(),
        Some("Cuts a root prefix off a file path.")
    );
}

/// The index is built once per probe call, however many functions the edit
/// added, and not at all when it added none.
#[test]
fn one_probe_call_builds_the_index_once() {
    let three = ["one", "two", "three"]
        .map(|name| JOIN.replace("joinRelative", name))
        .concat();
    BUILDS.with(|builds| builds.set(0));
    let found = similar(&[here(JOIN, &[])], &three);
    assert_eq!(found.len(), 3);
    assert!(found.iter().all(|(_, rows)| !rows.is_empty()), "{found:?}");
    assert_eq!(BUILDS.with(|builds| builds.get()), 1);

    BUILDS.with(|builds| builds.set(0));
    let nothing = with_similar(
        &Held {
            local: Vec::new(),
            hosted: Default::default(),
        },
        &Tree::at(Path::new("/nowhere")),
        "src/b.ts",
        Vec::new(),
    );
    assert!(nothing.is_empty());
    assert_eq!(BUILDS.with(|builds| builds.get()), 0);
}

/// A ranker written to disk and read back ranks as the one that was built.
#[test]
fn a_ranker_read_back_answers_as_built() {
    let hit = "export function joinRelative(root: string, file: string) {\n  \
        return file.startsWith(root) ? file.slice(root.length + 1) : file;\n}\n\
        export function inside(p: string) { return joinRelative('/', p); }\n";
    let first = "export function first(p: string) { return joinRelative('/', p); }\n";
    let held = vec![
        HeldFile {
            file: "src/hit.ts",
            intent: Some("Cuts a root prefix off a file path."),
            ..here(hit, &[("inside", "joinRelative")])
        },
        HeldFile {
            file: "src/first.ts",
            callee_file: Some("src/hit.ts"),
            ..here(first, &[("first", "joinRelative")])
        },
    ];
    let blobs: Vec<_> = held.iter().map(|file| blob("shop", file)).collect();
    let built = super::super::similar::Similar::build(
        blobs.iter().map(|blob| (blob.repo_name.clone(), blob)),
    );
    let bytes = serde_json::to_vec(&built).expect("a ranker is written");
    let back: super::super::similar::Similar =
        serde_json::from_slice(&bytes).expect("a ranker is read back");

    let edited = "export function relativePath(root: string, file: string) {\n  \
        return file.startsWith(root) ? file.slice(root.length + 1) : file;\n}\n";
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "package.json", "{\"name\":\"svc\"}");
    write(dir.path(), "src/b.ts", edited);
    let none = std::collections::BTreeSet::new();
    let added = additions(
        &[crate::config::Config::default()],
        dir.path(),
        "src/b.ts",
        &none,
    );
    let mut queries: Vec<Query> = added.into_iter().map(|(_, query)| query).collect();
    queries.push(Query::named("joinRelative"));
    queries.push(Query::named("first"));
    assert_eq!(queries.len(), 3);
    let mut some_called = false;
    for query in &queries {
        let rows = built.similar(query, "shop", "src/b.ts");
        some_called |= rows.iter().any(|row| row.called_from > 0);
        assert_eq!(back.similar(query, "shop", "src/b.ts"), rows, "{query:?}");
    }
    assert!(some_called, "the fixture has a row another file calls");
    assert_eq!(back.size(), built.size());
}
