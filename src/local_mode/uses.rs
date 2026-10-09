//! What other files of a repo use from one file: the callers of its functions
//! and the operations that read its types (carrick#2067).
//!
//! Folded once, when the read model is built, from the blobs the indexer keeps
//! in `.carrick/repos/`, so `check` answers it without opening a blob. The
//! shape a reader sees is the `uses` section of `docs/local-mode-output.md`.
//!
//! Callers follow the hosted `get_callers` reading of the same `calls` edges:
//! a bare member name and its qualified spelling are one callee, and a call
//! written at the top of a file (`<module>`) is a caller like any other.
//! Callers and type uses inside the file itself are left out, because the
//! reader already has that file open.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::cloud_storage::{CloudRepoData, ManifestRole, ManifestTypeKind};

use super::index::service_id;
use super::read_model::{
    FileUses, IndexedCaller, IndexedFunctionUse, IndexedItem, IndexedTypeOperation, IndexedTypeUse,
    ItemKind,
};

/// How many callers one function stores. `callers_total` keeps the exact
/// count, so a capped list never reads as a complete one.
pub const MAX_CALLERS: usize = 20;

/// Fold one repo's local blobs into its `uses` map, keyed by the repo-relative
/// path of the file that declares what is used. A file nothing outside it uses
/// has no entry.
///
/// `blobs` must be the blobs of ONE repo: an edge never crosses repos, and
/// `files` is that repo's row map, which the type uses are matched against.
pub(super) fn fold(
    blobs: &[&CloudRepoData],
    files: &BTreeMap<String, Vec<IndexedItem>>,
) -> BTreeMap<String, FileUses> {
    let mut uses: BTreeMap<String, FileUses> = BTreeMap::new();
    for (file, functions) in callers(blobs) {
        uses.entry(file).or_default().functions = functions;
    }
    for (file, types) in type_uses(blobs, files) {
        uses.entry(file).or_default().types = types;
    }
    uses
}

fn normalize(path: &str) -> String {
    super::normalize_relative(Path::new(path))
}

/// How a callee is identified: by the line it starts on where the scan
/// recorded one, which is what lets a bare and a qualified spelling of one
/// function land together, and by its name where it did not.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum CalleeKey {
    Line(u32),
    Name(String),
}

#[derive(Default)]
struct Callee {
    names: BTreeSet<String>,
    line: u32,
    /// `(file, line, function)` -> the smallest service that recorded it. A
    /// file two services include is scanned twice, and is one place.
    callers: BTreeMap<(String, u32, String), String>,
}

fn callers(blobs: &[&CloudRepoData]) -> BTreeMap<String, Vec<IndexedFunctionUse>> {
    let mut callees: BTreeMap<(String, CalleeKey), Callee> = BTreeMap::new();
    for blob in blobs {
        let service = service_id(blob);
        for definition in blob.function_definitions.values() {
            let caller_file = normalize(&definition.file_path.to_string_lossy());
            for call in &definition.calls {
                let callee_file = normalize(&call.file_path);
                if callee_file.is_empty() || callee_file == caller_file {
                    continue;
                }
                let key = if call.line_number > 0 {
                    CalleeKey::Line(call.line_number)
                } else {
                    CalleeKey::Name(call.name.clone())
                };
                let callee = callees.entry((callee_file, key)).or_default();
                callee.names.insert(call.name.clone());
                callee.line = call.line_number;
                let line = if call.call_site_line > 0 {
                    call.call_site_line
                } else {
                    definition.line_number
                };
                callee
                    .callers
                    .entry((caller_file.clone(), line, definition.name.clone()))
                    .and_modify(|kept| {
                        if service < *kept {
                            kept.clone_from(&service);
                        }
                    })
                    .or_insert_with(|| service.clone());
            }
        }
    }

    let mut out: BTreeMap<String, Vec<IndexedFunctionUse>> = BTreeMap::new();
    for ((file, _), callee) in callees {
        // The qualified spelling wins: `readWidget` and `CatalogClient.readWidget`
        // at one line are one function, and the qualified one says which.
        let name = callee
            .names
            .iter()
            .find(|name| name.contains('.'))
            .or_else(|| callee.names.iter().next())
            .cloned()
            .unwrap_or_default();
        let callers_total = callee.callers.len();
        let callers = callee
            .callers
            .into_iter()
            .take(MAX_CALLERS)
            .map(|((file, line, function), service)| IndexedCaller {
                service,
                file,
                line,
                function,
            })
            .collect();
        out.entry(file).or_default().push(IndexedFunctionUse {
            name,
            line: callee.line,
            callers_total,
            callers,
        });
    }
    for functions in out.values_mut() {
        functions.sort_by(|a, b| (a.line, &a.name).cmp(&(b.line, &b.name)));
    }
    out
}

fn type_uses(
    blobs: &[&CloudRepoData],
    files: &BTreeMap<String, Vec<IndexedItem>>,
) -> BTreeMap<String, Vec<IndexedTypeUse>> {
    let rows: BTreeMap<String, &Vec<IndexedItem>> = files
        .iter()
        .map(|(file, items)| (normalize(file), items))
        .collect();

    // Declaring file -> (line, symbol) -> the operations that read it.
    let mut found: BTreeMap<String, BTreeMap<(u32, String), BTreeSet<IndexedTypeOperation>>> =
        BTreeMap::new();
    for blob in blobs {
        let service = service_id(blob);
        for entry in blob.type_manifest.iter().flatten() {
            let Some(home) = &entry.defined_in else {
                continue;
            };
            let site = normalize(&entry.file_path);
            let declared = normalize(&home.file_path);
            if site == declared {
                continue;
            }
            let kind = match entry.role {
                ManifestRole::Producer => ItemKind::Route,
                ManifestRole::Consumer => ItemKind::Call,
            };
            let key = entry.key.canonical();
            // The operation must be a row the reader can see: a manifest entry
            // with no row of its own (a dispatch case keyed differently, a row
            // the join dropped) names nothing to open.
            let Some(row) = rows.get(&site).and_then(|items| {
                items.iter().find(|item| {
                    item.service == service
                        && item.kind == kind
                        && item.key == key
                        && item.line == Some(entry.line_number)
                })
            }) else {
                continue;
            };
            found
                .entry(declared)
                .or_default()
                .entry((home.line_number, home.symbol.clone()))
                .or_default()
                .insert(IndexedTypeOperation {
                    kind,
                    direction: match entry.type_kind {
                        ManifestTypeKind::Request => "request",
                        ManifestTypeKind::Response => "response",
                    }
                    .to_string(),
                    service: service.clone(),
                    key,
                    method: row.method.clone(),
                    path: row.path.clone(),
                    file: site,
                    line: entry.line_number,
                });
        }
    }

    found
        .into_iter()
        .map(|(file, types)| {
            let types = types
                .into_iter()
                .map(|((line, symbol), operations)| IndexedTypeUse {
                    symbol,
                    line,
                    operations: operations.into_iter().collect(),
                })
                .collect();
            (file, types)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// A blob with only what the fold reads.
    fn blob(service: &str, functions: Value, manifest: Value) -> CloudRepoData {
        serde_json::from_value(json!({
            "repo_name": "repo",
            "service_name": service,
            "endpoints": [],
            "calls": [],
            "mounts": [],
            "apps": {},
            "imported_handlers": [],
            "function_definitions": functions,
            "config_json": null,
            "package_json": null,
            "packages": null,
            "last_updated": "2026-10-09T00:00:00Z",
            "commit_hash": "abc",
            "type_manifest": manifest,
        }))
        .expect("a blob")
    }

    fn function(name: &str, file: &str, line: u32, calls: Value) -> Value {
        json!({
            "name": name,
            "file_path": file,
            "node_type": "FunctionDeclaration",
            "arguments": [],
            "line_number": line,
            "calls": calls,
        })
    }

    fn call(name: &str, file: &str, line: u32, site: u32) -> Value {
        json!({ "name": name, "file_path": file, "line_number": line, "call_site_line": site })
    }

    fn caller(service: &str, file: &str, line: u32, function: &str) -> IndexedCaller {
        IndexedCaller {
            service: service.into(),
            file: file.into(),
            line,
            function: function.into(),
        }
    }

    fn fold_one(blobs: &[&CloudRepoData]) -> BTreeMap<String, FileUses> {
        fold(blobs, &BTreeMap::new())
    }

    #[test]
    fn callers_from_other_files_are_named_and_a_same_file_caller_is_not() {
        let b = blob(
            "svc",
            json!({
                "a": function("countActive", "src/inventory.ts", 5,
                    json!([call("readWidget", "src/client.ts", 12, 9)])),
                "b": function("nameOf", "src/inventory.ts", 13,
                    json!([call("readWidget", "src/client.ts", 12, 17)])),
                "c": function("helper", "src/client.ts", 20,
                    json!([call("readWidget", "src/client.ts", 12, 22)])),
            }),
            json!([]),
        );
        let uses = fold_one(&[&b]);
        let functions = &uses["src/client.ts"].functions;
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "readWidget");
        assert_eq!(functions[0].line, 12);
        assert_eq!(functions[0].callers_total, 2);
        assert_eq!(
            functions[0].callers,
            vec![
                caller("svc", "src/inventory.ts", 9, "countActive"),
                caller("svc", "src/inventory.ts", 17, "nameOf"),
            ]
        );
        // The caller file uses nothing of its own that anyone else calls.
        assert!(!uses.contains_key("src/inventory.ts"));
    }

    #[test]
    fn a_file_two_services_scan_is_one_caller() {
        let functions = json!({
            "a": function("run", "shared/run.ts", 3, json!([call("send", "src/send.ts", 9, 4)])),
        });
        let first = blob("svc-b", functions.clone(), json!([]));
        let second = blob("svc-a", functions, json!([]));
        let uses = fold_one(&[&first, &second]);
        let send = &uses["src/send.ts"].functions[0];
        assert_eq!(send.callers_total, 1);
        assert_eq!(
            send.callers,
            vec![caller("svc-a", "shared/run.ts", 4, "run")]
        );
    }

    #[test]
    fn a_top_level_call_counts_and_a_missing_site_falls_back_to_the_caller() {
        let b = blob(
            "svc",
            json!({
                "m": function("<module>", "src/boot.ts", 1, json!([call("send", "src/send.ts", 9, 3)])),
                "f": function("later", "src/late.ts", 40, json!([call("send", "src/send.ts", 9, 0)])),
            }),
            json!([]),
        );
        let uses = fold_one(&[&b]);
        assert_eq!(
            uses["src/send.ts"].functions[0].callers,
            vec![
                caller("svc", "src/boot.ts", 3, "<module>"),
                caller("svc", "src/late.ts", 40, "later"),
            ]
        );
    }

    #[test]
    fn a_long_caller_list_is_capped_and_counted() {
        let mut functions = serde_json::Map::new();
        for n in 0..25u32 {
            functions.insert(
                format!("f{n}"),
                function(
                    &format!("f{n}"),
                    &format!("src/caller{n:02}.ts"),
                    1,
                    json!([call("send", "src/send.ts", 9, 2)]),
                ),
            );
        }
        let b = blob("svc", Value::Object(functions), json!([]));
        let uses = fold_one(&[&b]);
        let send = &uses["src/send.ts"].functions[0];
        assert_eq!(send.callers_total, 25);
        assert_eq!(send.callers.len(), MAX_CALLERS);
        assert_eq!(send.callers[0].file, "src/caller00.ts");
    }

    #[test]
    fn a_bare_and_a_qualified_reference_are_one_function() {
        let b = blob(
            "svc",
            json!({
                "a": function("one", "src/a.ts", 1, json!([call("readWidget", "src/client.ts", 12, 2)])),
                "b": function("two", "src/b.ts", 1, json!([call("CatalogClient.readWidget", "src/client.ts", 12, 3)])),
            }),
            json!([]),
        );
        let uses = fold_one(&[&b]);
        let functions = &uses["src/client.ts"].functions;
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "CatalogClient.readWidget");
        assert_eq!(functions[0].callers_total, 2);
    }

    fn manifest_entry(file: &str, line: u32, defined_in: Option<Value>) -> Value {
        let mut entry = json!({
            "protocol": "http",
            "method": "GET",
            "path": "/api/widgets/:id",
            "role": "consumer",
            "type_kind": "response",
            "type_alias": "Endpoint_x_Response",
            "file_path": file,
            "line_number": line,
            "is_explicit": true,
            "type_state": "explicit",
            "evidence": {
                "file_path": file,
                "line_number": line,
                "infer_kind": "call_result",
                "is_explicit": true,
                "type_state": "explicit",
            },
        });
        if let Some(home) = defined_in {
            entry["defined_in"] = home;
        }
        entry
    }

    fn row_for(b: &CloudRepoData, file: &str, line: u32) -> BTreeMap<String, Vec<IndexedItem>> {
        let entry = &b.type_manifest.as_ref().unwrap()[0];
        let item: IndexedItem = serde_json::from_value(json!({
            "kind": "call",
            "service": "svc",
            "key": entry.key.canonical(),
            "method": "GET",
            "path": "/api/widgets/:id",
            "line": line,
            "col": null,
            "source": "fact",
            "resolution_source": null,
            "evidence": null,
            "counterparts": [],
            "verdict": null,
        }))
        .expect("a row");
        BTreeMap::from([(file.to_string(), vec![item])])
    }

    #[test]
    fn a_type_declared_elsewhere_names_the_operation_that_reads_it() {
        let home = json!({ "file_path": "src/types.ts", "line_number": 3, "symbol": "Widget" });
        let b = blob(
            "svc",
            json!({}),
            json!([manifest_entry("src/inventory.ts", 9, Some(home))]),
        );
        let files = row_for(&b, "src/inventory.ts", 9);
        let uses = fold(&[&b], &files);
        let types = &uses["src/types.ts"].types;
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].symbol, "Widget");
        assert_eq!(types[0].line, 3);
        let operation = &types[0].operations[0];
        assert_eq!(operation.kind, ItemKind::Call);
        assert_eq!(operation.direction, "response");
        assert_eq!(operation.service, "svc");
        assert_eq!(
            (operation.method.as_str(), operation.path.as_str()),
            ("GET", "/api/widgets/:id")
        );
        assert_eq!(
            (operation.file.as_str(), operation.line),
            ("src/inventory.ts", 9)
        );
    }

    #[test]
    fn a_type_with_no_home_a_home_in_its_own_file_or_no_row_is_not_named() {
        let elsewhere =
            json!({ "file_path": "src/types.ts", "line_number": 3, "symbol": "Widget" });
        let here = json!({ "file_path": "src/inventory.ts", "line_number": 3, "symbol": "Widget" });
        for (home, row_line) in [(None, 9), (Some(here), 9), (Some(elsewhere), 10)] {
            let b = blob(
                "svc",
                json!({}),
                json!([manifest_entry("src/inventory.ts", 9, home.clone())]),
            );
            let files = row_for(&b, "src/inventory.ts", row_line);
            assert!(
                fold(&[&b], &files).is_empty(),
                "{home:?} at row line {row_line}"
            );
        }
    }
}
