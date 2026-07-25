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

use crate::services::type_sidecar::{InferKind, InferRequestItem, TypeSidecar};
use crate::visitor::FunctionDefinition;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tracing::{debug, warn};

/// Shown in the signature hint when a return type is neither annotated nor
/// successfully inferred.
const RETURN_UNKNOWN: &str = "unknown";

/// How long to wait for the sidecar to become ready before giving up on
/// signature inference (matches the type-resolution path).
const SIDECAR_READY_TIMEOUT: Duration = Duration::from_secs(10);

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
pub fn populate_function_signatures(
    sidecar: Option<&TypeSidecar>,
    function_definitions: &mut HashMap<String, FunctionDefinition>,
    repo_path: &str,
) {
    if let Some(sidecar) = sidecar {
        match sidecar.wait_ready(SIDECAR_READY_TIMEOUT) {
            Ok(()) => infer_missing_types(sidecar, function_definitions, repo_path),
            Err(e) => debug!("Sidecar not ready for signature inference: {e}"),
        }
    }

    for def in function_definitions.values_mut() {
        def.signature = Some(compose_signature(def));
    }
}

/// Build infer requests for unannotated slots, call the sidecar, and merge the
/// results back onto the function definitions.
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

    debug!("Inferring {} unannotated signature slot(s)", requests.len());

    let response = match sidecar.infer_types(&requests, None) {
        Ok(response) => response,
        Err(e) => {
            warn!("Signature inference failed: {e}");
            return;
        }
    };

    let Some(inferred) = response.inferred_types else {
        return;
    };

    for ty in &inferred {
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
            }
            SigSlot::Param(index) => {
                if let Some(arg) = def.arguments.get_mut(index) {
                    arg.is_explicit = ty.is_explicit;
                    arg.type_string = Some(ty.type_string.clone());
                }
            }
        }
    }
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
fn compose_signature(def: &FunctionDefinition) -> String {
    let params = def
        .arguments
        .iter()
        .map(|arg| match &arg.type_string {
            Some(ty) => format!("{}: {}", arg.name, ty),
            None => arg.name.clone(),
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
            return_type: return_type.map(|t| t.to_string()),
            return_is_explicit: return_type.is_some(),
            signature: None,
            intent_input_hash: None,
        }
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
}
