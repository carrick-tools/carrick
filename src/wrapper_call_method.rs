//! The verb at a call through a declaration is the declaration's, not the
//! model's (carrick#1384).
//!
//! A call site written on an imported binding states no method: the verb is in
//! the declaration it reaches. The model answers such a site with one anyway,
//! and nothing checked it — [`crate::wrapper_request_shape`] propagates a
//! module's verb only onto rows with no span of their own, and folds it over
//! every wrapper module a FILE imports rather than the one this call reaches.
//! So a guard that delegates to a GET fetcher was indexed as a POST.
//!
//! This pass reads the verb where it is written. For a model row at a call site
//! that states no request of its own, the declaration the call reaches is
//! resolved through the module graph
//! ([`crate::wrapper_call_join::declaration_reached`]), and the requests that
//! module issues are folded: when every one of them states the same literal
//! verb, that verb is the site's.
//!
//! Every part of that is read off the AST, and no library, framework or hook is
//! named. The guards are the same "say nothing" ones the fold uses:
//!
//! - **The site must state no verb.** A call the scanner reads as a request
//!   itself — an HTTP-verb callee, a request-options bag — states its own
//!   method, and this pass never touches it.
//! - **Only a model row is corrected.** A deterministic row read its method off
//!   the source it was resolved from.
//! - **The declaration's requests must agree** on one literal verb
//!   ([`fold_module`]). A module that parameterizes its method, or issues
//!   requests that disagree, states nothing, and the row keeps what it had —
//!   counted, so a scan can say how often it could not answer.
//! - **One hop for a module that issues no request of its own.** A guard
//!   sitting between the site and the fetcher delegates to a module it imports,
//!   so the modules IT imports are read as well, and they must all agree. A
//!   verb two modules further out is not in what this pass reads, and the row
//!   keeps the model's.
//!
//! The correction moves the row's operation, so it runs before the passes that
//! decide which rows are the same request
//! ([`crate::consumer_row_fold`], [`crate::wrapper_call_join`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tracing::debug;

use crate::agents::file_analyzer_agent::{DataCallResult, FileAnalysisResult, ResolutionSource};
use crate::import_bindings::BindingResolver;
use crate::type_manifest::normalize_manifest_method;
use crate::workspace_resolver::WorkspaceIndex;
use crate::wrapper_call_join::{declaration_reached, read_call_sites, resolve_module, site_of};
use crate::wrapper_request_shape::{RequestShapeSignal, WrapperRequestShape, fold_module};

/// What the pass decided, so a scan can state it rather than change the index
/// silently.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WrapperMethodCorrections {
    /// Rows whose verb the declaration they reach states, and disagreed with.
    pub corrected: usize,
    /// Rows left alone because the declaration issues requests whose verb it
    /// could not read, or whose verbs disagree. The row keeps the model's, and
    /// this is how often that happened.
    pub declaration_unreadable: usize,
}

/// Correct the method of every model row at a call through a declaration whose
/// requests state one.
///
/// `workspace` resolves the specifiers the repo declares (aliases, workspace
/// packages) as well as relative ones; `None` follows relative specifiers only.
pub fn correct_wrapper_call_methods(
    file_results: &mut HashMap<String, FileAnalysisResult>,
    workspace: Option<&WorkspaceIndex>,
) -> WrapperMethodCorrections {
    let mut corrections = WrapperMethodCorrections::default();

    // Only a file with a model row has a site to read; nothing else is parsed.
    let mut keys: Vec<String> = file_results
        .iter()
        .filter(|(_, result)| result.data_calls.iter().any(is_correctable_row))
        .map(|(key, _)| key.clone())
        .collect();
    keys.sort();
    if keys.is_empty() {
        return corrections;
    }

    let mut resolver = match workspace {
        Some(workspace) => BindingResolver::with_workspace(workspace.clone()),
        None => BindingResolver::new(),
    };
    // One module is read once however many sites reach it.
    let mut declarations: HashMap<PathBuf, DeclarationRequests> = HashMap::new();

    for key in keys {
        let file = Path::new(&key);
        let Ok(canonical) = file.canonicalize() else {
            continue;
        };
        let Some(calls) = read_call_sites(file) else {
            continue;
        };
        let Some(result) = file_results.get(&key) else {
            continue;
        };

        let mut stated: Vec<(usize, String)> = Vec::new();
        for (index, call) in result.data_calls.iter().enumerate() {
            if !is_correctable_row(call) {
                continue;
            }
            let Some(site) = site_of(&calls.sites, call) else {
                continue;
            };
            // The site's own source states the request: its method is what the
            // scanner read there, not something to look up elsewhere.
            if site.request != RequestShapeSignal::NotARequest {
                continue;
            }
            let Some(reached) = declaration_reached(
                &mut resolver,
                workspace,
                file,
                &canonical,
                &calls.imports,
                &site.root,
            ) else {
                continue;
            };
            let requests = declarations
                .entry(reached.file.clone())
                .or_insert_with(|| declaration_requests(workspace, &reached.file))
                .clone();
            match requests {
                DeclarationRequests::None => {}
                DeclarationRequests::Unreadable => {
                    corrections.declaration_unreadable += 1;
                    debug!(
                        "  - {key}: line {} reaches {} in {}, whose requests state no single \
                         verb; keeping the extracted method",
                        call.line_number,
                        reached.published,
                        reached.file.display()
                    );
                }
                DeclarationRequests::Stated(shape) => {
                    let extracted = call
                        .method
                        .as_deref()
                        .map(normalize_manifest_method)
                        .unwrap_or_default();
                    if extracted != shape.method {
                        debug!(
                            "  - {key}: line {} states no verb and reaches {} in {}, which \
                             requests {}; correcting {extracted}",
                            call.line_number,
                            reached.published,
                            reached.file.display(),
                            shape.method
                        );
                        stated.push((index, shape.method));
                    }
                }
            }
        }

        if stated.is_empty() {
            continue;
        }
        let Some(result) = file_results.get_mut(&key) else {
            continue;
        };
        for (index, method) in stated {
            result.data_calls[index].method = Some(method);
            corrections.corrected += 1;
        }
    }

    corrections
}

/// What the requests of one declaring module state about their verb.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DeclarationRequests {
    /// The module issues no request this pass can reach, one hop included.
    None,
    /// It issues requests, and they state no single literal verb.
    Unreadable,
    /// Every request it issues states the same literal verb.
    Stated(WrapperRequestShape),
}

/// Read what one module's requests state, following one hop for a module that
/// issues none of its own.
fn declaration_requests(workspace: Option<&WorkspaceIndex>, module: &Path) -> DeclarationRequests {
    let Some((own, specifiers)) = module_requests(module) else {
        return DeclarationRequests::None;
    };
    if own != DeclarationRequests::None {
        // The module performs HTTP itself, so its own requests are what its
        // members issue. Its imports are not read then: its own requests
        // already answer, and a client module that also imports another client
        // says nothing about which one a member uses.
        return own;
    }

    // A module that issues no request delegates to one it imports — the guard
    // between the site and the fetcher. Exactly one hop: every module it
    // imports that issues a request of its own has to agree, and a verb one
    // module further out is not read at all.
    let mut folded: Option<WrapperRequestShape> = None;
    let mut seen: Vec<PathBuf> = Vec::new();
    for specifier in specifiers {
        let Some(next) = resolve_module(workspace, module, &specifier) else {
            continue;
        };
        if seen.contains(&next) {
            continue;
        }
        seen.push(next.clone());
        let Some((delegated, _)) = module_requests(&next) else {
            continue;
        };
        match delegated {
            DeclarationRequests::None => continue,
            DeclarationRequests::Unreadable => return DeclarationRequests::Unreadable,
            DeclarationRequests::Stated(shape) => match &mut folded {
                None => folded = Some(shape),
                Some(accumulated) => {
                    if accumulated.method != shape.method {
                        return DeclarationRequests::Unreadable;
                    }
                    if accumulated.has_body != shape.has_body {
                        accumulated.has_body = None;
                    }
                }
            },
        }
    }
    match folded {
        Some(shape) => DeclarationRequests::Stated(shape),
        None => DeclarationRequests::None,
    }
}

/// What one module's OWN requests state, with the specifiers it imports.
/// `None` when the module cannot be read at all.
fn module_requests(module: &Path) -> Option<(DeclarationRequests, Vec<String>)> {
    let calls = read_call_sites(module)?;
    let signals: Vec<&RequestShapeSignal> = calls.sites.iter().map(|site| &site.request).collect();
    let issues_request = signals
        .iter()
        .any(|signal| **signal != RequestShapeSignal::NotARequest);
    let requests = if !issues_request {
        DeclarationRequests::None
    } else {
        match fold_module(signals) {
            Some(shape) => DeclarationRequests::Stated(shape),
            None => DeclarationRequests::Unreadable,
        }
    };
    Some((requests, calls.specifiers))
}

/// Whether this row is one the pass may correct: the model's own reading.
///
/// Deliberately NOT restricted to a row with a span. A call written on an
/// imported binding raises no HTTP candidate of its own — that is what makes
/// its verb unreadable at the site in the first place — so the rows this pass
/// exists for are routinely the ones with no span, placed by their line.
fn is_correctable_row(call: &DataCallResult) -> bool {
    call.resolution_source == Some(ResolutionSource::Model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swc_scanner::SWC_SPAN_BASE;

    /// The fetcher: every request it issues is a GET, read off an options bag
    /// that states no method.
    const FETCHER: &str = r#"export async function sendJson(path: string) {
  const response = await fetch(path, { headers: { accept: "application/json" } });
  return response.json();
}
"#;

    /// The guard between the site and the fetcher. It issues no request of its
    /// own: the verb is one module further out.
    const GUARD: &str = r#"import { sendJson } from "./fetcher";

export async function requireEntitlement(scope: string) {
  return sendJson(`/v1/shelves?scope=${scope}`);
}
"#;

    /// The site: a call on an imported binding, stating no verb at all.
    const SITE: &str = r#"import { requireEntitlement } from "../lib/guard";

export function PanelRoute() {
  return requireEntitlement("panel");
}
"#;

    fn write(root: &Path, rel: &str, content: &str) -> String {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// A model row at the call that starts at `needle`, placed by its LINE and
    /// carrying no span — the shape a call through an imported binding has,
    /// because no HTTP candidate is raised at such a site.
    fn row(content: &str, needle: &str, method: &str, target: &str) -> DataCallResult {
        DataCallResult {
            candidate_id: "model".to_string(),
            call_expression_span_start: None,
            call_expression_span_end: None,
            ..spanned_row(content, needle, method, target)
        }
    }

    /// The same row as the candidate scanner joined it: with its span.
    fn spanned_row(content: &str, needle: &str, method: &str, target: &str) -> DataCallResult {
        let offset = content.find(needle).expect("needle is in the source") as u32;
        let span = offset + SWC_SPAN_BASE;
        let line = content[..offset as usize].lines().count() as i32;
        DataCallResult {
            candidate_id: format!("span:{span}"),
            line_number: line,
            target: target.to_string(),
            method: Some(method.to_string()),
            call_kind: None,
            pattern_matched: "requireEntitlement(".to_string(),
            call_expression_span_start: Some(span),
            call_expression_span_end: Some(span + needle.len() as u32),
            call_expression_text: None,
            call_expression_line: Some(line),
            payload_expression_text: None,
            payload_expression_line: None,
            primary_type_symbol: None,
            type_import_source: None,
            loopback_default_url: None,
            base: None,
            consumers_not_resolved: None,
            dispatch: None,
            resolution_source: Some(ResolutionSource::Model),
            reaches_request: None,
        }
    }

    fn correct(
        files: Vec<(String, Vec<DataCallResult>)>,
    ) -> (
        HashMap<String, FileAnalysisResult>,
        WrapperMethodCorrections,
    ) {
        correct_with(files, None)
    }

    /// The same, with the module index a scan hands the pass. `None` follows
    /// relative specifiers only, which is what a repo's declared aliases are
    /// tested against.
    fn correct_with(
        files: Vec<(String, Vec<DataCallResult>)>,
        workspace: Option<&WorkspaceIndex>,
    ) -> (
        HashMap<String, FileAnalysisResult>,
        WrapperMethodCorrections,
    ) {
        let mut results: HashMap<String, FileAnalysisResult> = files
            .into_iter()
            .map(|(path, data_calls)| {
                (
                    path,
                    FileAnalysisResult {
                        data_calls,
                        ..Default::default()
                    },
                )
            })
            .collect();
        let corrections = correct_wrapper_call_methods(&mut results, workspace);
        (results, corrections)
    }

    fn method(results: &HashMap<String, FileAnalysisResult>, file: &str) -> Option<String> {
        results[file].data_calls[0].method.clone()
    }

    /// The acceptance shape again, with every import written through an alias
    /// the repo declares — which is how a repo that declares one writes them
    /// all, including the guard's own import of the fetcher, so the hop needs
    /// the index too. Without the index the specifiers reach nothing and the
    /// row keeps the verb the model gave it.
    #[test]
    fn a_guard_reached_through_a_declared_alias_still_states_its_fetcher_s_verb() {
        for (config, contents) in [
            (
                "tsconfig.json",
                "{\n  \"compilerOptions\": {\n    \"baseUrl\": \".\",\n    \"paths\": { \"@/*\": [\"src/*\"] }\n  }\n}\n",
            ),
            (
                "deno.jsonc",
                "{\n  // the import map a Deno repo declares\n  \"imports\": { \"@/\": \"./src/\" }\n}\n",
            ),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path();
            write(root, config, contents);
            write(root, "src/lib/fetcher.ts", FETCHER);
            write(
                root,
                "src/lib/guard.ts",
                &GUARD.replace("./fetcher", "@/lib/fetcher"),
            );
            let source = SITE.replace("../lib/guard", "@/lib/guard");
            let site = write(root, "src/routes/panel.tsx", &source);

            let rows = || {
                vec![(
                    site.clone(),
                    vec![row(
                        &source,
                        "requireEntitlement(\"panel\")",
                        "POST",
                        "/v1/shelves",
                    )],
                )]
            };

            let workspace = WorkspaceIndex::build_with_aliases(root, None);
            let (corrected, corrections) = correct_with(rows(), Some(&workspace));
            assert_eq!(
                method(&corrected, &site).as_deref(),
                Some("GET"),
                "{config}: the aliases the repo declares reach the guard and its fetcher"
            );
            assert_eq!(corrections.corrected, 1, "{config}");

            let (untouched, corrections) = correct_with(rows(), None);
            assert_eq!(
                method(&untouched, &site).as_deref(),
                Some("POST"),
                "{config}: without the repo's own resolver nothing states a verb, and the \
                 row keeps what it had"
            );
            assert_eq!(corrections, WrapperMethodCorrections::default(), "{config}");
        }
    }

    /// carrick#1384's acceptance: the site calls an imported guard, the guard
    /// delegates to a GET fetcher, and the model answered the site with POST.
    #[test]
    fn a_guard_delegating_to_a_get_fetcher_states_get_at_its_call_site() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/lib/fetcher.ts", FETCHER);
        write(root, "src/lib/guard.ts", GUARD);
        let site = write(root, "src/routes/panel.tsx", SITE);

        let (results, corrections) = correct(vec![(
            site.clone(),
            vec![row(
                SITE,
                "requireEntitlement(\"panel\")",
                "POST",
                "/v1/shelves",
            )],
        )]);

        assert_eq!(
            method(&results, &site).as_deref(),
            Some("GET"),
            "the site states no verb; the requests it reaches are GETs"
        );
        assert_eq!(
            corrections,
            WrapperMethodCorrections {
                corrected: 1,
                declaration_unreadable: 0
            }
        );
    }

    #[test]
    fn a_site_that_states_its_own_verb_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/lib/fetcher.ts", FETCHER);
        // The site calls the fetcher's module directly, with a verb of its own.
        let source = "import { client } from \"../lib/fetcher\";\n\nexport function save() {\n  return client.post(\"/v1/shelves\", {});\n}\n";
        let site = write(root, "src/routes/panel.tsx", source);

        let (results, corrections) = correct(vec![(
            site.clone(),
            vec![spanned_row(
                source,
                "client.post(\"/v1/shelves\", {})",
                "POST",
                "/v1/shelves",
            )],
        )]);

        assert_eq!(
            method(&results, &site).as_deref(),
            Some("POST"),
            "the verb is written at this line; nothing elsewhere overrules it"
        );
        assert_eq!(corrections, WrapperMethodCorrections::default());
    }

    #[test]
    fn a_declaration_whose_requests_disagree_keeps_the_extracted_verb() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "src/lib/guard.ts",
            "export async function requireEntitlement(scope: string) {\n  if (scope === \"panel\") {\n    return fetch(\"/v1/shelves\", { method: \"POST\", body: \"{}\" });\n  }\n  return fetch(\"/v1/shelves\", { headers: {} });\n}\n",
        );
        let site = write(root, "src/routes/panel.tsx", SITE);

        let (results, corrections) = correct(vec![(
            site.clone(),
            vec![row(
                SITE,
                "requireEntitlement(\"panel\")",
                "PUT",
                "/v1/shelves",
            )],
        )]);

        assert_eq!(
            method(&results, &site).as_deref(),
            Some("PUT"),
            "the declaration issues a POST and a GET: nothing to correct from"
        );
        assert_eq!(
            corrections,
            WrapperMethodCorrections {
                corrected: 0,
                declaration_unreadable: 1
            },
            "and the scan says how often it could not answer"
        );
    }

    /// The hop budget, stated. The guard delegates to a helper that delegates
    /// to the fetcher, so the verb is two modules out and this pass does not
    /// read it.
    #[test]
    fn a_verb_two_modules_out_is_not_read() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/lib/fetcher.ts", FETCHER);
        write(
            root,
            "src/lib/helper.ts",
            "import { sendJson } from \"./fetcher\";\n\nexport const send = (path: string) => sendJson(path);\n",
        );
        write(
            root,
            "src/lib/guard.ts",
            "import { send } from \"./helper\";\n\nexport async function requireEntitlement(scope: string) {\n  return send(`/v1/shelves?scope=${scope}`);\n}\n",
        );
        let site = write(root, "src/routes/panel.tsx", SITE);

        let (results, corrections) = correct(vec![(
            site.clone(),
            vec![row(
                SITE,
                "requireEntitlement(\"panel\")",
                "POST",
                "/v1/shelves",
            )],
        )]);

        assert_eq!(
            method(&results, &site).as_deref(),
            Some("POST"),
            "no request this pass reads states a verb, so the row keeps what it had"
        );
        assert_eq!(corrections, WrapperMethodCorrections::default());
    }

    #[test]
    fn a_deterministic_row_keeps_the_method_its_pass_read() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/lib/fetcher.ts", FETCHER);
        write(root, "src/lib/guard.ts", GUARD);
        let site = write(root, "src/routes/panel.tsx", SITE);

        let (results, corrections) = correct(vec![(
            site.clone(),
            vec![DataCallResult {
                // The imported-member pass read this row out of the declaring
                // module: its method came from the same source this pass reads.
                resolution_source: Some(ResolutionSource::ImportedMember),
                ..row(SITE, "requireEntitlement(\"panel\")", "POST", "/v1/shelves")
            }],
        )]);

        assert_eq!(
            method(&results, &site).as_deref(),
            Some("POST"),
            "a deterministic row's method is not this pass's to change"
        );
        assert_eq!(corrections, WrapperMethodCorrections::default());
    }

    /// A row with no span is placed by its line, and a line carrying more than
    /// one call does not say which of them the row is. The imported binding is
    /// FIRST on the line here, so a pass that took the first call would
    /// correct this row.
    #[test]
    fn a_line_carrying_more_than_one_call_states_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/lib/fetcher.ts", FETCHER);
        write(root, "src/lib/guard.ts", GUARD);
        let source = "import { requireEntitlement } from \"../lib/guard\";\n\nexport function PanelRoute() {\n  return requireEntitlement(\"panel\") && save(\"panel\");\n}\n";
        let site = write(root, "src/routes/panel.tsx", source);

        let (results, corrections) = correct(vec![(
            site.clone(),
            vec![row(
                source,
                "requireEntitlement(\"panel\")",
                "POST",
                "/v1/shelves",
            )],
        )]);

        assert_eq!(
            method(&results, &site).as_deref(),
            Some("POST"),
            "which call on the line this row is, is not something the line says"
        );
        assert_eq!(corrections, WrapperMethodCorrections::default());
    }

    #[test]
    fn a_call_on_a_binding_this_file_declares_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/lib/fetcher.ts", FETCHER);
        write(root, "src/lib/guard.ts", GUARD);
        // Imports the guard, and the row is at a call on a binding of its own.
        let source = "import { requireEntitlement } from \"../lib/guard\";\n\nconst save = makeSaver();\n\nexport function PanelRoute() {\n  requireEntitlement(\"panel\");\n  return save(\"panel\");\n}\n";
        let site = write(root, "src/routes/panel.tsx", source);

        let (results, corrections) = correct(vec![(
            site.clone(),
            vec![row(source, "save(\"panel\")", "POST", "/v1/shelves")],
        )]);

        assert_eq!(
            method(&results, &site).as_deref(),
            Some("POST"),
            "the call is written on a binding this file declares: the import \
             beside it states nothing about it"
        );
        assert_eq!(corrections, WrapperMethodCorrections::default());
    }
}
