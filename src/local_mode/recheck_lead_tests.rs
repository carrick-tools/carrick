//! The edit-time lead through the probe's own path (carrick-cloud#3249).
//!
//! The held side is an indexed blob built by the extractor the index uses;
//! `calls`, which only the call graph fills, is set by name where a test needs
//! it. The edited side is `src/b.ts`, read by the probe's own parse
//! ([`additions`]) and scored by [`with_leads`].

use super::super::contract::{Lead, LeadBasis};
use super::super::lead::{BUILDS, LEAD_TOKEN_FLOOR};
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
}

fn here<'a>(source: &'a str, calls: Calls<'a>) -> HeldFile<'a> {
    HeldFile {
        repo: None,
        file: "src/a.ts",
        source,
        calls,
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
            file_path: held.file.to_string(),
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
fn leads(held: &[HeldFile<'_>], edited: &str) -> Vec<(String, Option<Lead>)> {
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
    with_leads(&held, &Tree::at(dir.path()), "src/b.ts", added)
        .into_iter()
        .map(|function| (function.name, function.lead))
        .collect()
}

fn only_lead(found: Vec<(String, Option<Lead>)>) -> Option<Lead> {
    assert_eq!(found.len(), 1, "{found:?}");
    found.into_iter().next().unwrap().1
}

#[test]
fn a_copy_under_another_name_leads_to_the_indexed_one() {
    let copy = JOIN.replace("joinRelative", "relativePath");
    let lead = only_lead(leads(&[here(JOIN, &[])], &copy)).expect("a lead");
    assert_eq!(lead.name, "joinRelative");
    assert_eq!(lead.file, "src/a.ts");
    assert_eq!(lead.line, 1);
    assert_eq!(lead.basis, LeadBasis::Tokens);
    assert!(lead.score >= LEAD_TOKEN_FLOOR, "{lead:?}");
}

/// A copy kept in another repo, with a local renamed: a lead that names
/// that repo.
#[test]
fn a_near_copy_in_another_repo_leads_and_names_its_repo() {
    let mirror = JOIN.replace("trimmed", "base");
    let other = HeldFile {
        repo: Some("web-app"),
        file: "lib/paths.ts",
        source: &mirror,
        calls: &[],
    };
    let copy = JOIN.replace("joinRelative", "relativePath");
    let lead = only_lead(leads(&[other], &copy)).expect("a lead");
    assert_eq!(lead.repo, "web-app");
    assert_eq!(lead.file, "lib/paths.ts");
    assert_eq!(lead.name, "joinRelative");
}

#[test]
fn an_ordinary_new_function_gets_no_lead() {
    let edited = "export function totalOf(rows: { amount: number }[]): number {\n  \
        return rows.reduce((sum, row) => sum + row.amount, 0);\n}\n";
    assert_eq!(only_lead(leads(&[here(JOIN, &[])], edited)), None);
}

#[test]
fn a_function_that_calls_the_hit_gets_no_lead() {
    let wrapper = "export function relativePath(root: string, file: string) {\n  \
        const trimmed = root.endsWith('/') ? root.slice(0, -1) : root;\n  \
        const rest = file.startsWith(trimmed) ? file.slice(trimmed.length + 1) : file;\n  \
        return joinRelative(trimmed, rest);\n}\n";
    // The wrapper shares the hit's words: without the edge it would lead.
    let lead = only_lead(leads(&[here(JOIN, &[])], wrapper));
    assert_eq!(lead, None);
}

/// The hit calls a function spelled like the new one: they are one chain,
/// not two copies.
#[test]
fn a_hit_that_calls_the_new_name_gets_no_lead() {
    let held = format!("{JOIN}function relativePath(a: string) {{ return a; }}\n");
    let calls: Calls<'_> = &[("joinRelative", "relativePath")];
    let copy = JOIN.replace("joinRelative", "relativePath");
    assert_eq!(only_lead(leads(&[here(&held, calls)], &copy)), None);
}

#[test]
fn a_same_name_twin_with_another_body_gets_no_lead() {
    let held = "export function main() {\n  const server = createServer(handler);\n  \
        server.listen(Number(process.env.PORT ?? 8080));\n}\n";
    let edited = "export function main() {\n  const rows = readRows('input.csv');\n  \
        for (const row of rows) console.log(row.total);\n}\n";
    assert_eq!(only_lead(leads(&[here(held, &[])], edited)), None);
}

#[test]
fn a_same_name_twin_with_the_same_body_leads_on_the_name() {
    let lead = only_lead(leads(&[here(JOIN, &[])], JOIN)).expect("a lead");
    assert_eq!(lead.name, "joinRelative");
    assert_eq!(lead.file, "src/a.ts");
    assert_eq!(lead.basis, LeadBasis::Name);
}

fn typed(name: &str, returns: &str) -> String {
    JOIN.replace("joinRelative", name)
        .replace("file: string)", &format!("file: string): {returns}"))
}

#[test]
fn a_different_written_return_type_demotes_the_better_hit() {
    // The second copy carries one literal more, so it scores lower.
    let held = format!(
        "{}{}",
        typed("joinNumbered", "number"),
        typed("joinTyped", "string").replace("-1)", "-1) + 'x'")
    );
    let edited = typed("relativePath", "string");
    let lead = only_lead(leads(&[here(&held, &[])], &edited)).expect("a lead");
    assert_eq!(lead.name, "joinTyped", "{lead:?}");
    assert_eq!(lead.return_types, None);
}

#[test]
fn a_demoted_hit_leads_alone_and_names_both_types() {
    let held = typed("joinNumbered", "number");
    let edited = typed("relativePath", "string");
    let lead = only_lead(leads(&[here(&held, &[])], &edited)).expect("a lead");
    assert_eq!(lead.name, "joinNumbered");
    assert_eq!(
        lead.return_types,
        Some(["string".to_string(), "number".to_string()])
    );
}

#[test]
fn the_lead_names_its_indexed_callers() {
    let held = format!(
        "{JOIN}export function first(p: string) {{ return joinRelative('/', p); }}\n\
         export function second(p: string) {{ return joinRelative('.', p); }}\n"
    );
    let calls: Calls<'_> = &[("first", "joinRelative"), ("second", "joinRelative")];
    let copy = JOIN.replace("joinRelative", "relativePath");
    let lead = only_lead(leads(&[here(&held, calls)], &copy)).expect("a lead");
    assert_eq!(lead.callers_total, 2, "{lead:?}");
    let callers: Vec<&str> = lead.callers.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(callers, vec!["first", "second"]);
}

/// The index is built once per probe call, however many functions the edit
/// added, and not at all when it added none.
#[test]
fn one_probe_call_builds_the_index_once() {
    let three = ["one", "two", "three"]
        .map(|name| JOIN.replace("joinRelative", name))
        .concat();
    BUILDS.with(|builds| builds.set(0));
    let found = leads(&[here(JOIN, &[])], &three);
    assert_eq!(found.len(), 3);
    assert!(found.iter().all(|(_, lead)| lead.is_some()), "{found:?}");
    assert_eq!(BUILDS.with(|builds| builds.get()), 1);

    BUILDS.with(|builds| builds.set(0));
    let nothing = with_leads(
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
