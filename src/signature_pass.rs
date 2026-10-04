//! Function-signature pass.
//!
//! Composes a one-line signature hint for every function definition and, where
//! a sidecar is available, fills in param/return types that lack source
//! annotations via compiler inference.
//!
//! Provenance is metadata, not a routing decision: each slot carries
//! `is_explicit` (annotated vs inferred). The hint is composed for every
//! function regardless of whether inference ran, so explicit-typed code is
//! fully served even without a sidecar. Deep type resolution is intentionally
//! out of scope here — the named types in a signature become drill-downable via
//! the bundle pipeline in follow-up work (issues #116/#117).

use crate::services::type_sidecar::{
    InferKind, InferRequestItem, SidecarError, SidecarResponse, TypeSidecar, ready_budget,
};
use crate::visitor::FunctionDefinition;
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, warn};

/// Shown in the signature hint when a return type is neither annotated nor
/// successfully inferred.
const RETURN_UNKNOWN: &str = "unknown";

/// The most slots one inference request carries (carrick#1915).
///
/// The pass used to be one request, so a request that got no answer kept
/// nothing: a first index sent 15,786 slots, the sidecar needed 38 minutes,
/// and the scan had stopped waiting at 15. A request that fails now costs its
/// own slots and no others.
///
/// The size does not bound how long a batch may run: the sidecar reports
/// progress per slot, and the deadline measures silence (carrick#1914). It
/// bounds what one stalled slot takes with it.
///
/// The slots are cut into batches in the order they were always sent. The
/// compiler prints a union's members in the order it first met them, so
/// sending the same slots in another order (by file, say) changes how some
/// signatures read without changing what they say: 227 of 6,653 on one tree.
const BATCH_SLOTS: usize = 500;

/// Which slot of a function signature an inference request targets.
#[derive(Debug, Clone, PartialEq)]
enum SigSlot {
    Return,
    Param(usize),
}

/// Maps a generated inference alias back to the function + slot it fills.
#[derive(Debug, Clone)]
struct SigTarget {
    fn_name: String,
    slot: SigSlot,
}

/// Populate `signature` on every function definition, filling unannotated
/// param/return types via the sidecar when one is available and ready.
///
/// The file-level owner of module-scope calls is not a function (carrick#965):
/// it is skipped here as well as in the inference requests, or its row would
/// claim `() => unknown` — a signature for something that has none.
pub fn populate_function_signatures(
    sidecar: Option<&TypeSidecar>,
    function_definitions: &mut HashMap<String, FunctionDefinition>,
    repo_path: &str,
) {
    if let Some(sidecar) = sidecar {
        match sidecar.wait_ready(ready_budget()) {
            Ok(()) => infer_missing_types(sidecar, function_definitions, repo_path),
            Err(e) => debug!("Sidecar not ready for signature inference: {e}"),
        }
    }

    for def in function_definitions.values_mut() {
        if def.name == crate::visitor::MODULE_SCOPE_KEY {
            continue;
        }
        def.signature = Some(compose_signature(def));
    }
}

/// What the pass over the batches came to.
#[derive(Debug, Default, PartialEq)]
struct PassOutcome {
    /// Slots the sidecar typed, now on their function definitions.
    inferred: usize,
    /// Slots with no answer: those of every batch that failed, and `unsent`.
    lost: usize,
    /// Batches that were sent and got no answer.
    failed_batches: usize,
    /// Slots never sent, because a batch failed in a way that left no
    /// sidecar to ask.
    unsent: usize,
}

/// Build infer requests for unannotated slots, send them to the sidecar in
/// batches, and merge each batch's results onto the function definitions as
/// it returns.
fn infer_missing_types(
    sidecar: &TypeSidecar,
    function_definitions: &mut HashMap<String, FunctionDefinition>,
    repo_path: &str,
) {
    let repo_root_absolute = absolute_repo_root(repo_path);
    let (requests, targets) = build_infer_requests(function_definitions, &repo_root_absolute);
    if requests.is_empty() {
        return;
    }
    let slots = requests.len();

    debug!(
        "Inferring {slots} unannotated signature slot(s) in {} batch(es)",
        slots.div_ceil(BATCH_SLOTS)
    );

    // Timed on its own: the phase line's `signatures` covers these round trips
    // AND the scanner-side request build, and only the split says which grew
    // (carrick#767).
    let round_trips = std::time::Instant::now();
    let outcome = infer_in_batches(
        &requests,
        BATCH_SLOTS,
        &targets,
        function_definitions,
        |batch| sidecar.infer_types(batch, None),
    );
    let seconds = round_trips.elapsed().as_secs_f64();

    if outcome.lost > 0 {
        warn!(
            "Signature inference: {} of {slots} slot(s) inferred, {} lost ({} batch(es) got no \
             answer, {} slot(s) were never sent), in {seconds:.1}s of sidecar time",
            outcome.inferred, outcome.lost, outcome.failed_batches, outcome.unsent,
        );
    } else {
        debug!(
            "Signature inference: {} of {slots} slot(s) inferred, none lost, in {seconds:.1}s of \
             sidecar time",
            outcome.inferred
        );
    }
}

/// Ask for the requests `batch_slots` at a time, in their order, and merge
/// what comes back before asking for the next batch, so a batch that fails
/// takes nothing a previous one brought.
///
/// `infer` is the sidecar call. A batch it fails on is lost and the next one
/// is asked, unless the failure says there is no sidecar left
/// ([`sidecar_still_answers`]): then the batches not yet sent are lost with
/// it, without being sent.
fn infer_in_batches(
    requests: &[InferRequestItem],
    batch_slots: usize,
    targets: &HashMap<String, SigTarget>,
    function_definitions: &mut HashMap<String, FunctionDefinition>,
    mut infer: impl FnMut(&[InferRequestItem]) -> Result<SidecarResponse, SidecarError>,
) -> PassOutcome {
    let mut outcome = PassOutcome::default();
    let mut batches = requests.chunks(batch_slots);
    while let Some(batch) = batches.next() {
        let error = match infer(batch) {
            Ok(response) => {
                outcome.inferred += merge_inferred(&response, targets, function_definitions);
                continue;
            }
            Err(error) => error,
        };
        outcome.lost += batch.len();
        outcome.failed_batches += 1;
        if sidecar_still_answers(&error) {
            warn!(
                "Signature inference failed for a batch of {} slot(s): {error}",
                batch.len()
            );
            continue;
        }
        let unsent: usize = batches.by_ref().map(<[InferRequestItem]>::len).sum();
        warn!(
            "Signature inference stopped at a batch of {} slot(s), with {unsent} slot(s) not yet \
             sent: {error}",
            batch.len()
        );
        outcome.lost += unsent;
        outcome.unsent = unsent;
    }
    outcome
}

/// Whether the sidecar can be asked the next batch after failing this way.
///
/// A timed-out operation has already had its sidecar replaced by a fresh one
/// (carrick#1914), and a frame that could not be written or read says nothing
/// about the process. The rest say there is no process to ask: sending the
/// remaining batches would fail each the same way.
fn sidecar_still_answers(error: &SidecarError) -> bool {
    match error {
        SidecarError::Timeout
        | SidecarError::SerializationError(_)
        | SidecarError::DeserializationError(_)
        // Failures of other operations; an inference does not return them.
        | SidecarError::ResolutionFailed(_)
        | SidecarError::CaptureFailed(_)
        | SidecarError::CheckFailed(_) => true,
        SidecarError::SpawnFailed(_)
        | SidecarError::InitFailed(_)
        | SidecarError::NotReady(_)
        | SidecarError::ProcessDied
        | SidecarError::IoError(_)
        | SidecarError::Interrupted(_) => false,
    }
}

/// Put one batch's inferred types on the function definitions they belong
/// to. Returns how many slots were filled.
fn merge_inferred(
    response: &SidecarResponse,
    targets: &HashMap<String, SigTarget>,
    function_definitions: &mut HashMap<String, FunctionDefinition>,
) -> usize {
    let mut filled = 0;
    for ty in response.inferred_types.iter().flatten() {
        let Some(target) = targets.get(&ty.alias) else {
            continue;
        };
        let Some(def) = function_definitions.get_mut(&target.fn_name) else {
            continue;
        };
        match target.slot {
            SigSlot::Return => {
                def.return_type = Some(ty.type_string.clone());
                def.return_is_explicit = ty.is_explicit;
                filled += 1;
            }
            SigSlot::Param(index) => {
                if let Some(arg) = def.arguments.get_mut(index) {
                    arg.is_explicit = ty.is_explicit;
                    arg.type_string = Some(ty.type_string.clone());
                    filled += 1;
                }
            }
        }
    }
    filled
}

/// Build one infer request per unannotated slot, with a generated alias keyed
/// back to its (function, slot) target. Iterates in name order so request
/// generation is deterministic.
fn build_infer_requests(
    function_definitions: &HashMap<String, FunctionDefinition>,
    repo_root_absolute: &Path,
) -> (Vec<InferRequestItem>, HashMap<String, SigTarget>) {
    let mut requests = Vec::new();
    let mut targets = HashMap::new();
    let mut counter = 0usize;

    let mut names: Vec<&String> = function_definitions.keys().collect();
    names.sort();

    for name in names {
        let def = &function_definitions[name];
        // The file-level owner of module-scope calls (carrick#965) is not a
        // function: it has no return to infer and no parameter to type, and
        // asking the sidecar about its first line would stamp whatever lives
        // there onto the row.
        if def.name == crate::visitor::MODULE_SCOPE_KEY {
            continue;
        }
        let file_path = to_absolute_path(&def.file_path.to_string_lossy(), repo_root_absolute);

        if def.return_type.is_none() {
            let alias = format!("__sig{counter}");
            counter += 1;
            requests.push(InferRequestItem {
                file_path: file_path.clone(),
                line_number: def.line_number,
                span_start: None,
                span_end: None,
                expression_text: None,
                expression_line: None,
                infer_kind: InferKind::SignatureReturn,
                alias: Some(alias.clone()),
                param_name: None,
            });
            targets.insert(
                alias,
                SigTarget {
                    fn_name: name.clone(),
                    slot: SigSlot::Return,
                },
            );
        }

        for (index, arg) in def.arguments.iter().enumerate() {
            if arg.type_string.is_some() {
                continue;
            }
            let alias = format!("__sig{counter}");
            counter += 1;
            requests.push(InferRequestItem {
                file_path: file_path.clone(),
                line_number: def.line_number,
                span_start: None,
                span_end: None,
                expression_text: None,
                expression_line: None,
                infer_kind: InferKind::FunctionParam,
                alias: Some(alias.clone()),
                // ts-morph matches by getName(), which drops the rest `...`.
                param_name: Some(arg.name.trim_start_matches("...").to_string()),
            });
            targets.insert(
                alias,
                SigTarget {
                    fn_name: name.clone(),
                    slot: SigSlot::Param(index),
                },
            );
        }
    }

    (requests, targets)
}

/// Compose the one-line signature hint, e.g.
/// `(token: string, opts?: VerifyOpts) => Promise<AuthResult>`. Params without a
/// known type render as the bare name; an unknown return renders as `unknown`.
/// Defaulted trailing parameters are optional at the call site. Before a
/// required parameter they still occupy a position, but accept `undefined`.
/// Initializer source remains in the argument record rather than this type.
fn compose_signature(def: &FunctionDefinition) -> String {
    let last_required = def
        .arguments
        .iter()
        .rposition(|arg| !arg.is_optional && !arg.has_default && !arg.is_rest);
    let params = def
        .arguments
        .iter()
        .enumerate()
        .map(|(index, arg)| {
            let default_before_required =
                arg.has_default && last_required.is_some_and(|required| index < required);
            let optional = arg.is_optional || (arg.has_default && !default_before_required);
            let mut param = arg.name.clone();
            if optional {
                param.push('?');
            }
            if default_before_required {
                // Parentheses preserve function/intersection type precedence.
                let ty = arg.type_string.as_deref().unwrap_or("unknown");
                param.push_str(&format!(": ({ty}) | undefined"));
            } else if let Some(ty) = &arg.type_string {
                param.push_str(&format!(": {ty}"));
            }
            param
        })
        .collect::<Vec<_>>()
        .join(", ");
    let ret = def.return_type.as_deref().unwrap_or(RETURN_UNKNOWN);
    format!("({params}) => {ret}")
}

/// Resolve the repo root to an absolute, canonicalized path (mirrors
/// FileOrchestrator's resolution so the sidecar sees consistent paths).
fn absolute_repo_root(repo_path: &str) -> std::path::PathBuf {
    let repo_root = Path::new(repo_path);
    if repo_root.is_absolute() {
        return repo_root.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(repo_root))
        .unwrap_or_else(|_| repo_root.to_path_buf())
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf())
}

/// Convert a (possibly relative) file path to an absolute path the sidecar can
/// open. Mirrors `FileOrchestrator::to_absolute_path`.
fn to_absolute_path(file_path: &str, repo_root_absolute: &Path) -> String {
    let path = Path::new(file_path);
    if path.is_absolute() {
        return file_path.to_string();
    }
    let resolved = std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf());
    resolved
        .canonicalize()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| repo_root_absolute.join(path).to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visitor::{FunctionArgument, FunctionDefinition, FunctionNodeType};

    fn arg(name: &str, ty: Option<&str>) -> FunctionArgument {
        FunctionArgument {
            name: name.to_string(),
            type_ann: None,
            is_explicit: ty.is_some(),
            type_string: ty.map(|t| t.to_string()),
            is_optional: false,
            has_default: false,
            default_value: None,
            is_rest: name.starts_with("..."),
        }
    }

    fn def(args: Vec<FunctionArgument>, return_type: Option<&str>) -> FunctionDefinition {
        FunctionDefinition {
            name: "fn".to_string(),
            file_path: "src/auth.ts".into(),
            node_type: FunctionNodeType::Placeholder,
            arguments: args,
            body_source: None,
            is_exported: true,
            line_number: 10,
            end_line: 0,
            intent: None,
            calls: vec![],
            tokens: vec![],
            return_type: return_type.map(|t| t.to_string()),
            return_is_explicit: return_type.is_some(),
            signature: None,
            intent_input_hash: None,
            dispatch_table: None,
        }
    }

    /// The file-level owner of module-scope calls (carrick#965) is not a
    /// function, so it is left without a signature rather than given
    /// `() => unknown` — a row that claims a shape it does not have.
    #[test]
    fn the_module_scope_owner_is_left_without_a_signature() {
        let mut module_owner = def(vec![], None);
        module_owner.name = crate::visitor::MODULE_SCOPE_KEY.to_string();
        let mut definitions = HashMap::from([
            (crate::visitor::MODULE_SCOPE_KEY.to_string(), module_owner),
            ("readRun".to_string(), def(vec![], Some("void"))),
        ]);

        populate_function_signatures(None, &mut definitions, ".");

        assert_eq!(
            definitions[crate::visitor::MODULE_SCOPE_KEY].signature,
            None,
            "the file owns calls, not a callable shape"
        );
        assert_eq!(
            definitions["readRun"].signature.as_deref(),
            Some("() => void")
        );
    }

    #[test]
    fn default_before_required_accepts_undefined_even_without_initializer_source() {
        let mut defaulted = arg("value", Some("number"));
        defaulted.has_default = true;
        let d = def(
            vec![defaulted, arg("required", Some("string"))],
            Some("void"),
        );
        assert_eq!(
            compose_signature(&d),
            "(value: (number) | undefined, required: string) => void"
        );
    }

    #[test]
    fn composes_fully_typed_signature() {
        let d = def(
            vec![
                arg("token", Some("string")),
                arg("opts", Some("VerifyOpts")),
            ],
            Some("Promise<AuthResult>"),
        );
        assert_eq!(
            compose_signature(&d),
            "(token: string, opts: VerifyOpts) => Promise<AuthResult>"
        );
    }

    #[test]
    fn composes_untyped_signature_with_unknown_return() {
        let d = def(vec![arg("x", None)], None);
        assert_eq!(compose_signature(&d), "(x) => unknown");
    }

    #[test]
    fn composes_mixed_signature() {
        let d = def(
            vec![arg("id", Some("string")), arg("flag", None)],
            Some("void"),
        );
        assert_eq!(compose_signature(&d), "(id: string, flag) => void");
    }

    #[test]
    fn composes_zero_arg_signature() {
        let d = def(vec![], Some("number"));
        assert_eq!(compose_signature(&d), "() => number");
    }

    #[test]
    fn build_requests_targets_only_unannotated_slots() {
        let mut defs = HashMap::new();
        // one annotated param, one unannotated param, no return annotation
        defs.insert(
            "verify".to_string(),
            def(vec![arg("token", Some("string")), arg("opts", None)], None),
        );
        let repo_root = Path::new("/tmp/repo");
        let (requests, targets) = build_infer_requests(&defs, repo_root);

        // 1 return gap + 1 param gap = 2 requests (the annotated param is skipped)
        assert_eq!(requests.len(), 2);
        assert_eq!(targets.len(), 2);

        let return_req = requests
            .iter()
            .find(|r| r.infer_kind == InferKind::SignatureReturn)
            .expect("return request");
        assert_eq!(return_req.line_number, 10);
        assert_eq!(return_req.param_name, None);

        let param_req = requests
            .iter()
            .find(|r| r.infer_kind == InferKind::FunctionParam)
            .expect("param request");
        assert_eq!(param_req.param_name.as_deref(), Some("opts"));

        // every request alias maps back to a target
        for req in &requests {
            let alias = req.alias.as_ref().expect("alias");
            assert!(targets.contains_key(alias), "alias {alias} should map");
        }
    }

    #[test]
    fn build_requests_skips_fully_annotated_functions() {
        let mut defs = HashMap::new();
        defs.insert(
            "greet".to_string(),
            def(vec![arg("name", Some("string"))], Some("string")),
        );
        let (requests, targets) = build_infer_requests(&defs, Path::new("/tmp/repo"));
        assert!(requests.is_empty());
        assert!(targets.is_empty());
    }

    #[test]
    fn build_requests_strips_rest_param_dots() {
        let mut defs = HashMap::new();
        defs.insert(
            "variadic".to_string(),
            def(vec![arg("...args", None)], Some("void")),
        );
        let (requests, _) = build_infer_requests(&defs, Path::new("/tmp/repo"));
        let param_req = requests
            .iter()
            .find(|r| r.infer_kind == InferKind::FunctionParam)
            .expect("param request");
        assert_eq!(param_req.param_name.as_deref(), Some("args"));
    }

    // ---- carrick#1915: the pass is sent in batches ----

    /// A function in `file` with one unannotated parameter and no return
    /// annotation: two slots.
    fn two_slot_def(name: &str, file: &str) -> FunctionDefinition {
        let mut d = def(vec![arg("input", None)], None);
        d.name = name.to_string();
        d.file_path = file.into();
        d
    }

    /// A sidecar answer that types every slot of the batch as `typed`.
    fn answer(batch: &[InferRequestItem]) -> SidecarResponse {
        let inferred: Vec<serde_json::Value> = batch
            .iter()
            .map(|request| {
                serde_json::json!({
                    "alias": request.alias,
                    "type_string": "typed",
                    "is_explicit": false,
                    "source_location": {
                        "file_path": request.file_path,
                        "start_line": 1,
                        "end_line": 1
                    },
                    "infer_kind": request.infer_kind,
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "request_id": "req",
            "status": "success",
            "inferred_types": inferred,
        }))
        .expect("a sidecar answer")
    }

    #[test]
    fn the_slots_go_in_the_order_they_were_built_a_batch_at_a_time() {
        // Names sort one way and their files the other.
        let mut definitions = HashMap::new();
        for (name, file) in [
            ("a", "/r/z.ts"),
            ("b", "/r/y.ts"),
            ("c", "/r/z.ts"),
            ("d", "/r/x.ts"),
            ("e", "/r/y.ts"),
        ] {
            definitions.insert(name.to_string(), two_slot_def(name, file));
        }
        let (requests, targets) = build_infer_requests(&definitions, Path::new("/r"));
        let built: Vec<String> = requests.iter().map(|r| r.alias.clone().unwrap()).collect();
        assert_eq!(built.len(), 10);

        let mut sent: Vec<Vec<String>> = Vec::new();
        let outcome = infer_in_batches(&requests, 4, &targets, &mut definitions, |batch| {
            sent.push(batch.iter().map(|r| r.alias.clone().unwrap()).collect());
            Ok(answer(batch))
        });

        let sizes: Vec<usize> = sent.iter().map(Vec::len).collect();
        assert_eq!(sizes, vec![4, 4, 2]);
        assert_eq!(
            sent.concat(),
            built,
            "batching changes how many slots a request carries, not their order"
        );
        assert_eq!(outcome.inferred, 10);
        assert!(
            definitions
                .values()
                .all(|d| d.return_type.as_deref() == Some("typed")
                    && d.arguments[0].type_string.as_deref() == Some("typed"))
        );
    }

    /// Three functions of two slots each, a file each: at two slots a batch,
    /// one batch per function. Returns the definitions, their targets and the
    /// requests.
    fn three_functions() -> (
        HashMap<String, FunctionDefinition>,
        HashMap<String, SigTarget>,
        Vec<InferRequestItem>,
    ) {
        let definitions = HashMap::from([
            ("inA".to_string(), two_slot_def("inA", "/r/a.ts")),
            ("inB".to_string(), two_slot_def("inB", "/r/b.ts")),
            ("inC".to_string(), two_slot_def("inC", "/r/c.ts")),
        ]);
        let (requests, targets) = build_infer_requests(&definitions, Path::new("/r"));
        assert_eq!(requests.len(), 6);
        (definitions, targets, requests)
    }

    #[test]
    fn a_batch_that_times_out_costs_that_batch_only() {
        let (mut definitions, targets, requests) = three_functions();
        let mut asked = Vec::new();
        let outcome = infer_in_batches(&requests, 2, &targets, &mut definitions, |batch| {
            asked.push(batch[0].file_path.clone());
            if batch[0].file_path == "/r/b.ts" {
                // The sidecar is replaced by a fresh one (carrick#1914).
                Err(SidecarError::Timeout)
            } else {
                Ok(answer(batch))
            }
        });

        assert_eq!(asked, vec!["/r/a.ts", "/r/b.ts", "/r/c.ts"]);
        assert_eq!(
            outcome,
            PassOutcome {
                inferred: 4,
                lost: 2,
                failed_batches: 1,
                unsent: 0,
            }
        );
        for kept in ["inA", "inC"] {
            assert_eq!(definitions[kept].return_type.as_deref(), Some("typed"));
            assert_eq!(
                definitions[kept].arguments[0].type_string.as_deref(),
                Some("typed")
            );
        }
        assert_eq!(definitions["inB"].return_type, None);
        assert_eq!(definitions["inB"].arguments[0].type_string, None);
    }

    #[test]
    fn a_sidecar_that_is_gone_ends_the_pass_and_keeps_what_was_merged() {
        let (mut definitions, targets, requests) = three_functions();
        let mut asked = 0;
        let outcome = infer_in_batches(&requests, 2, &targets, &mut definitions, |batch| {
            asked += 1;
            if batch[0].file_path == "/r/a.ts" {
                Ok(answer(batch))
            } else {
                Err(SidecarError::ProcessDied)
            }
        });

        assert_eq!(asked, 2, "the third batch is not sent to a dead process");
        assert_eq!(
            outcome,
            PassOutcome {
                inferred: 2,
                lost: 4,
                failed_batches: 1,
                unsent: 2,
            }
        );
        assert_eq!(definitions["inA"].return_type.as_deref(), Some("typed"));
        assert_eq!(definitions["inC"].return_type, None);
    }

    #[test]
    fn a_slot_the_sidecar_could_not_type_is_neither_inferred_nor_lost() {
        let (mut definitions, targets, requests) = three_functions();
        let outcome = infer_in_batches(&requests, 2, &targets, &mut definitions, |batch| {
            // Only the first slot of each batch comes back.
            Ok(answer(&batch[..1]))
        });
        assert_eq!(
            outcome,
            PassOutcome {
                inferred: 3,
                lost: 0,
                failed_batches: 0,
                unsent: 0,
            }
        );
    }

    #[test]
    fn only_a_sidecar_that_can_still_answer_is_asked_again() {
        // Replaced by a fresh process, or an answer that could not be read.
        assert!(sidecar_still_answers(&SidecarError::Timeout));
        assert!(sidecar_still_answers(&SidecarError::DeserializationError(
            "bad frame".into()
        )));
        // No process to ask.
        assert!(!sidecar_still_answers(&SidecarError::ProcessDied));
        assert!(!sidecar_still_answers(&SidecarError::IoError(
            "broken pipe".into()
        )));
        assert!(!sidecar_still_answers(&SidecarError::NotReady(
            "its replacement was not ready".into()
        )));
    }
}
