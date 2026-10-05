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
    InferKind, InferRequestItem, InferSlotTiming, InferTiming, SidecarError, SidecarResponse,
    TypeSidecar, ready_budget,
};
use crate::visitor::FunctionDefinition;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};
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
/// compiler prints a union's members in the order it first met them, and
/// sending the same slots in another order (by file, say) changed how 227 of
/// 6,653 signatures on one tree read without changing what they say. The
/// sidecar now puts the unions of the signature kinds in one stable order
/// (carrick#1993); whether anything else the compiler prints still moves with
/// the order is measured before the order is changed.
const BATCH_SLOTS: usize = 500;

/// A slot is named in the log when it took this many milliseconds or more
/// (carrick#1985).
///
/// The sidecar names the slowest slots of every batch, and in most batches
/// none of them is slow: a pass that is going well answers a slot in a
/// millisecond or two. A line for each would bury the ones the log is read
/// for, so a slot gets one when it alone took what a hundred such slots do.
/// Every batch still gets its own line, and that line gives its slowest slot
/// whatever it took.
const SLOW_SLOT_MS: f64 = 100.0;

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
    /// How the time of the batches that were answered divides.
    timing: PassTiming,
}

/// How the time of a pass divides among its slots (carrick#1985): what each
/// answered batch said of its own slots, added up.
///
/// It is what the log says of the pass and nothing else reads it. A batch
/// that got no answer said nothing, so its slots are not in here.
#[derive(Debug, Default, PartialEq)]
struct PassTiming {
    /// Slots the sidecar timed.
    slots: u64,
    /// Their times, added up, in milliseconds.
    slots_ms: f64,
    /// Of `slots_ms`, the time the compiler took to compute the types.
    type_ms: f64,
    /// Of `slots_ms`, the time printing the types to text.
    print_ms: f64,
    /// How long the scanner waited for the batches those slots were in.
    waited: Duration,
    /// Slots that were the first asked of their file.
    first_in_file_slots: u64,
    /// The times of those, added up, in milliseconds.
    first_in_file_ms: f64,
    /// The time of every slot a batch named as one of its slowest.
    named_ms: Vec<f64>,
    /// The most a slot that no batch named can have taken: a batch that
    /// names fewer slots than it timed left out only slots that took no
    /// longer than the last one it named.
    unnamed_at_most_ms: f64,
    /// The longest type a slot printed, in characters, and whose slot.
    longest_printed: Option<(u64, String)>,
}

/// What the slowest hundredth of a pass's slots took.
#[derive(Debug, PartialEq)]
struct SlowestHundredth {
    /// How many slots that is.
    slots: usize,
    /// Their times, added up, in milliseconds.
    ms: f64,
    /// Whether `ms` is the figure or only its floor. It is the figure when
    /// every slot of the hundredth was named by its batch. A batch names a
    /// fixed number of slots, so one that holds more of the pass's slowest
    /// than that leaves some unnamed, and `ms` then counts quicker named
    /// slots in their place: no more than the real figure, possibly less.
    exact: bool,
}

impl PassTiming {
    /// Add what one answered batch said of its slots. `waited` is how long
    /// the scanner waited for the batch, and `describe` words a slot for
    /// the log.
    fn add(
        &mut self,
        timing: &InferTiming,
        waited: Duration,
        describe: impl Fn(&InferSlotTiming) -> String,
    ) {
        self.slots += timing.slots;
        self.slots_ms += timing.slots_ms;
        self.type_ms += timing.type_ms;
        self.print_ms += timing.print_ms;
        self.waited += waited;
        self.first_in_file_slots += timing.first_in_file_slots;
        self.first_in_file_ms += timing.first_in_file_ms;
        self.named_ms
            .extend(timing.slowest.iter().map(|slot| slot.ms));
        if (timing.slowest.len() as u64) < timing.slots
            && let Some(last_named) = timing.slowest.last()
        {
            self.unnamed_at_most_ms = self.unnamed_at_most_ms.max(last_named.ms);
        }
        if let Some(longest) = &timing.longest_printed
            && self
                .longest_printed
                .as_ref()
                .is_none_or(|(length, _)| longest.printed_length > *length)
        {
            self.longest_printed = Some((longest.printed_length, describe(longest)));
        }
    }

    /// What the slowest hundredth of the timed slots took (one slot at
    /// least), or `None` when no slot was timed.
    fn slowest_hundredth(&self) -> Option<SlowestHundredth> {
        if self.slots == 0 {
            return None;
        }
        let wanted = (self.slots as usize).div_ceil(100);
        let mut named = self.named_ms.clone();
        named.sort_by(|a, b| b.total_cmp(a));
        named.truncate(wanted);
        // Every slot of the hundredth was named when there are that many
        // named slots and the quickest of them took at least what any
        // unnamed slot can have taken.
        let exact = named.len() == wanted
            && named
                .last()
                .is_some_and(|quickest| *quickest >= self.unnamed_at_most_ms);
        Some(SlowestHundredth {
            slots: wanted,
            ms: named.iter().sum(),
            exact,
        })
    }

    /// The one line the log holds for the pass, or `None` when no batch
    /// said how long its slots took.
    fn summary(&self) -> Option<String> {
        let hundredth = self.slowest_hundredth()?;
        let share = |ms: f64| {
            if self.slots_ms > 0.0 {
                100.0 * ms / self.slots_ms
            } else {
                0.0
            }
        };
        let longest = match &self.longest_printed {
            Some((length, whose)) => {
                format!("the longest type printed is {length} character(s), by {whose}")
            }
            None => "no slot printed a type".to_string(),
        };
        Some(format!(
            "Signature inference timing: {} slot(s) took {:.1}s of the {:.1}s waited for their \
             batches ({:.1}s computing types, {:.1}s printing them); the slowest {} (1%) took \
             {}{:.1}s ({:.0}%); the {} first asked of their file took {:.1}s ({:.0}%); {longest}",
            self.slots,
            self.slots_ms / 1000.0,
            self.waited.as_secs_f64(),
            self.type_ms / 1000.0,
            self.print_ms / 1000.0,
            hundredth.slots,
            if hundredth.exact { "" } else { "at least " },
            hundredth.ms / 1000.0,
            share(hundredth.ms),
            self.first_in_file_slots,
            self.first_in_file_ms / 1000.0,
            share(self.first_in_file_ms),
        ))
    }
}

/// A slot as the log names it: the function, which of its slots, and where
/// the function is. Names and a position: never the function's text, and
/// never the type the slot printed.
///
/// A parameter is named by its place in the list, counted from 1. Its own
/// name can be a destructuring pattern, which is source text.
fn describe_slot(
    slot: &InferSlotTiming,
    targets: &HashMap<String, SigTarget>,
    function_definitions: &HashMap<String, FunctionDefinition>,
) -> String {
    let target = slot.alias.as_ref().and_then(|alias| targets.get(alias));
    let which = match target.map(|target| &target.slot) {
        Some(SigSlot::Return) => "return".to_string(),
        Some(SigSlot::Param(index)) => format!("parameter {}", index + 1),
        None => match slot.infer_kind {
            InferKind::SignatureParam | InferKind::FunctionParam => "parameter".to_string(),
            _ => "return".to_string(),
        },
    };
    match target.and_then(|target| function_definitions.get(&target.fn_name)) {
        Some(def) => format!(
            "{} {which} at {}:{}",
            def.name,
            def.file_path.display(),
            def.line_number
        ),
        // A slot the pass did not send; say where the sidecar says it is.
        None => format!(
            "an unknown function's {which} at {}:{}",
            slot.file_path, slot.line_number
        ),
    }
}

/// The lines the log holds for one answered batch (carrick#1985): one for
/// the batch, then one for each slot it named that took [`SLOW_SLOT_MS`] or
/// more, slowest first.
///
/// A slot's line splits its time into the compiler computing the type and
/// the print of it. What is left over is finding the function and reading
/// back the names the print wrote, which is small beside either.
fn batch_timing_lines(
    batch_number: usize,
    batches: usize,
    waited: Duration,
    timing: &InferTiming,
    describe: impl Fn(&InferSlotTiming) -> String,
) -> Vec<String> {
    let slowest_ms = timing.slowest.first().map_or(0.0, |slot| slot.ms);
    let slow: Vec<&InferSlotTiming> = timing
        .slowest
        .iter()
        .filter(|slot| slot.ms >= SLOW_SLOT_MS)
        .collect();
    // The sidecar names a fixed number: when every one it named is slow,
    // there may be slow ones it did not name.
    let maybe_more = !slow.is_empty()
        && slow.len() == timing.slowest.len()
        && (slow.len() as u64) < timing.slots;
    let mut lines = vec![format!(
        "Signature inference batch {batch_number} of {batches}: {} slot(s) took {:.1}s of the \
         {:.1}s waited ({:.1}s computing types, {:.1}s printing them); the slowest took \
         {slowest_ms:.0}ms; {}{} took {SLOW_SLOT_MS:.0}ms or more; the {} first asked of their \
         file took {:.1}s",
        timing.slots,
        timing.slots_ms / 1000.0,
        waited.as_secs_f64(),
        timing.type_ms / 1000.0,
        timing.print_ms / 1000.0,
        if maybe_more { "at least " } else { "" },
        slow.len(),
        timing.first_in_file_slots,
        timing.first_in_file_ms / 1000.0,
    )];
    lines.extend(slow.into_iter().map(|slot| {
        format!(
            "Signature inference slow slot: {} took {:.0}ms ({:.0}ms computing its type, \
             {:.0}ms printing {} character(s)){}",
            describe(slot),
            slot.ms,
            slot.type_ms,
            slot.print_ms,
            slot.printed_length,
            if slot.first_in_file {
                ", first asked of its file"
            } else {
                ""
            },
        )
    }));
    lines
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
    if let Some(line) = outcome.timing.summary() {
        debug!("{line}");
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
    let batch_count = requests.len().div_ceil(batch_slots);
    let mut batch_number = 0;
    let mut batches = requests.chunks(batch_slots);
    while let Some(batch) = batches.next() {
        batch_number += 1;
        let sent = Instant::now();
        let error = match infer(batch) {
            Ok(response) => {
                let waited = sent.elapsed();
                outcome.inferred += merge_inferred(&response, targets, function_definitions);
                // For the log only (carrick#1985): which slots the batch's
                // time went to. Nothing above or below reads it.
                if let Some(timing) = &response.infer_timing {
                    let describe =
                        |slot: &InferSlotTiming| describe_slot(slot, targets, function_definitions);
                    for line in
                        batch_timing_lines(batch_number, batch_count, waited, timing, describe)
                    {
                        debug!("{line}");
                    }
                    outcome.timing.add(timing, waited, describe);
                }
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
                // Not `FunctionParam`: that is the kind a contract's parameter
                // is asked by, and its text keeps the compiler's own union
                // order. This one's text is the function index's, printed in a
                // stable order (carrick#1993).
                infer_kind: InferKind::SignatureParam,
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
            .find(|r| r.infer_kind == InferKind::SignatureParam)
            .expect("param request");
        assert_eq!(param_req.param_name.as_deref(), Some("opts"));
        // The contract kind is never the pass's: its text is not put in the
        // stable order (carrick#1993).
        assert!(
            requests
                .iter()
                .all(|r| r.infer_kind != InferKind::FunctionParam)
        );

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
            .find(|r| r.infer_kind == InferKind::SignatureParam)
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
                timing: PassTiming::default(),
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
                timing: PassTiming::default(),
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
                timing: PassTiming::default(),
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

    // ---- carrick#1985: the log names the slots a pass's time went to ----

    /// A writer a test reads back: what the pass logged, as the log holds it.
    #[derive(Clone, Default)]
    struct Logged(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Logged {
        fn lines(&self) -> Vec<String> {
            String::from_utf8_lossy(&self.0.lock().unwrap())
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    impl std::io::Write for Logged {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logged {
        type Writer = Logged;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Run `pass` and return every line it logged at debug or above.
    fn logged_by(pass: impl FnOnce()) -> Vec<String> {
        let logged = Logged::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logged.clone())
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, pass);
        logged.lines()
    }

    /// One slot's timing, as the sidecar writes it. `parts` is how much of
    /// `ms` went on computing the type and on printing it.
    fn slot_timing(
        request: &InferRequestItem,
        ms: f64,
        parts: (f64, f64),
        printed_length: u64,
        first_in_file: bool,
    ) -> serde_json::Value {
        serde_json::json!({
            "alias": request.alias,
            "file_path": request.file_path,
            "line_number": request.line_number,
            "infer_kind": request.infer_kind,
            "ms": ms,
            "type_ms": parts.0,
            "print_ms": parts.1,
            "printed_length": printed_length,
            "first_in_file": first_in_file,
        })
    }

    /// `answer(batch)` with the timing the sidecar puts beside the types:
    /// `slowest` as given, and the totals a batch of these slots adds up to
    /// (`parts`: computing types, printing them).
    fn timed_answer(
        batch: &[InferRequestItem],
        slots_ms: f64,
        parts: (f64, f64),
        first_in_file: (u64, f64),
        slowest: Vec<serde_json::Value>,
        longest_printed: Option<serde_json::Value>,
    ) -> SidecarResponse {
        let inferred: Vec<serde_json::Value> = batch
            .iter()
            .map(|request| {
                serde_json::json!({
                    "alias": request.alias,
                    "type_string": "{ secret: TypeText }",
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
        let mut timing = serde_json::json!({
            "slots": batch.len(),
            "slots_ms": slots_ms,
            "type_ms": parts.0,
            "print_ms": parts.1,
            "first_in_file_slots": first_in_file.0,
            "first_in_file_ms": first_in_file.1,
            "slowest": slowest,
        });
        if let Some(longest) = longest_printed {
            timing["longest_printed"] = longest;
        }
        serde_json::from_value(serde_json::json!({
            "request_id": "req",
            "status": "success",
            "inferred_types": inferred,
            "infer_timing": timing,
        }))
        .expect("a sidecar answer")
    }

    /// The request for `function`'s return, or for its one parameter.
    fn slot_of<'a>(
        requests: &'a [InferRequestItem],
        targets: &HashMap<String, SigTarget>,
        function: &str,
        slot: SigSlot,
    ) -> &'a InferRequestItem {
        requests
            .iter()
            .find(|request| {
                let target = &targets[request.alias.as_ref().unwrap()];
                target.fn_name == function && target.slot == slot
            })
            .expect("the slot was requested")
    }

    #[test]
    fn the_slow_slots_of_a_batch_are_logged_by_name_time_and_printed_length() {
        let (mut definitions, targets, requests) = three_functions();
        definitions.get_mut("inB").unwrap().line_number = 42;
        let slow_return = slot_of(&requests, &targets, "inB", SigSlot::Return).clone();
        let slow_param = slot_of(&requests, &targets, "inC", SigSlot::Param(0)).clone();
        let quick = slot_of(&requests, &targets, "inA", SigSlot::Return).clone();

        let lines = logged_by(|| {
            infer_in_batches(&requests, 6, &targets, &mut definitions, |batch| {
                Ok(timed_answer(
                    batch,
                    31_250.0,
                    (29_420.0, 1_790.0),
                    (3, 30_100.0),
                    vec![
                        slot_timing(&slow_return, 30_000.4, (28_300.2, 1_690.0), 1_204_551, true),
                        slot_timing(&slow_param, 1_100.0, (1_098.6, 0.4), 18, false),
                        slot_timing(&quick, 99.9, (20.0, 79.0), 7, true),
                    ],
                    Some(slot_timing(
                        &slow_return,
                        30_000.4,
                        (28_300.2, 1_690.0),
                        1_204_551,
                        true,
                    )),
                ))
            });
        });
        let timing: Vec<&str> = lines
            .iter()
            .filter_map(|line| {
                line.split_once("carrick::signature_pass: ")
                    .map(|(_, said)| said)
            })
            .collect();

        assert_eq!(
            timing,
            vec![
                "Signature inference batch 1 of 1: 6 slot(s) took 31.2s of the 0.0s waited \
                 (29.4s computing types, 1.8s printing them); the slowest took 30000ms; 2 took \
                 100ms or more; the 3 first asked of their file took 30.1s",
                "Signature inference slow slot: inB return at /r/b.ts:42 took 30000ms (28300ms \
                 computing its type, 1690ms printing 1204551 character(s)), first asked of its \
                 file",
                "Signature inference slow slot: inC parameter 1 at /r/c.ts:10 took 1100ms \
                 (1099ms computing its type, 0ms printing 18 character(s))",
            ],
            "one line for the batch, then one for each slot that took {SLOW_SLOT_MS}ms or more"
        );
        assert!(
            lines.iter().all(|line| line.contains("DEBUG")),
            "the lines are for the run log, not the terminal: {lines:?}"
        );
        // A length is not the type: nothing a slot answered is in the log.
        assert!(lines.iter().all(|line| !line.contains("TypeText")));
    }

    #[test]
    fn a_pass_s_timing_adds_up_its_batches_and_names_the_longest_type() {
        let (mut definitions, targets, requests) = three_functions();
        let a_return = slot_of(&requests, &targets, "inA", SigSlot::Return).clone();
        let b_return = slot_of(&requests, &targets, "inB", SigSlot::Return).clone();
        let c_return = slot_of(&requests, &targets, "inC", SigSlot::Return).clone();

        let outcome = infer_in_batches(&requests, 2, &targets, &mut definitions, |batch| {
            let first = &batch[0];
            Ok(if first.file_path == "/r/a.ts" {
                timed_answer(
                    batch,
                    10.5,
                    (6.0, 3.5),
                    (1, 10.0),
                    vec![slot_timing(&a_return, 10.0, (6.0, 3.0), 40, true)],
                    Some(slot_timing(&a_return, 10.0, (6.0, 3.0), 40, true)),
                )
            } else if first.file_path == "/r/b.ts" {
                timed_answer(
                    batch,
                    9_000.0,
                    (1_200.0, 7_750.5),
                    (1, 8_990.0),
                    vec![slot_timing(
                        &b_return,
                        8_990.0,
                        (1_195.0, 7_750.0),
                        950_000,
                        true,
                    )],
                    Some(slot_timing(
                        &b_return,
                        8_990.0,
                        (1_195.0, 7_750.0),
                        950_000,
                        true,
                    )),
                )
            } else {
                // A batch whose sidecar says nothing of its time still merges.
                let _ = &c_return;
                answer(batch)
            })
        });

        assert_eq!(
            outcome.inferred, 6,
            "a batch with no timing is answered as before"
        );
        assert_eq!(
            outcome.timing.slots, 4,
            "only the batches that said are counted"
        );
        assert_eq!(outcome.timing.slots_ms, 9_010.5);
        assert_eq!(outcome.timing.type_ms, 1_206.0);
        assert_eq!(outcome.timing.print_ms, 7_754.0);
        assert_eq!(outcome.timing.first_in_file_slots, 2);
        assert_eq!(outcome.timing.first_in_file_ms, 9_000.0);
        assert_eq!(
            outcome.timing.longest_printed,
            Some((950_000, "inB return at /r/b.ts:10".to_string()))
        );
        let summary = PassTiming {
            waited: Duration::from_millis(9_400),
            ..outcome.timing
        }
        .summary()
        .expect("two batches said how long their slots took");
        assert_eq!(
            summary,
            "Signature inference timing: 4 slot(s) took 9.0s of the 9.4s waited for their \
             batches (1.2s computing types, 7.8s printing them); the slowest 1 (1%) took 9.0s \
             (100%); the 2 first asked of their file took 9.0s (100%); the longest type printed \
             is 950000 character(s), by inB return at /r/b.ts:10"
        );
    }

    #[test]
    fn a_pass_no_batch_timed_has_no_timing_line() {
        let (mut definitions, targets, requests) = three_functions();
        let lines = logged_by(|| {
            let outcome = infer_in_batches(&requests, 2, &targets, &mut definitions, |batch| {
                Ok(answer(batch))
            });
            assert_eq!(outcome.timing, PassTiming::default());
            assert_eq!(outcome.timing.summary(), None);
        });
        assert!(
            lines
                .iter()
                .all(|line| !line.contains("Signature inference")),
            "{lines:?}"
        );
    }

    /// A batch of `slots` slots that named `named` of them, by their times.
    fn batch_naming(slots: u64, named: &[f64]) -> InferTiming {
        let slowest: Vec<serde_json::Value> = named
            .iter()
            .map(|ms| {
                serde_json::json!({
                    "file_path": "/r/a.ts",
                    "line_number": 1,
                    "infer_kind": "signature_return",
                    "ms": ms,
                    "type_ms": 0.0,
                    "print_ms": 0.0,
                    "printed_length": 0,
                    "first_in_file": false,
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "slots": slots,
            "slots_ms": 1_000_000.0,
            "type_ms": 600_000.0,
            "print_ms": 390_000.0,
            "first_in_file_slots": 0,
            "first_in_file_ms": 0.0,
            "slowest": slowest,
        }))
        .expect("a batch's timing")
    }

    fn pass_of(batches: &[InferTiming]) -> PassTiming {
        let mut pass = PassTiming::default();
        for batch in batches {
            pass.add(batch, Duration::ZERO, |_| "a slot".to_string());
        }
        pass
    }

    #[test]
    fn the_slowest_hundredth_is_taken_across_batches_not_from_each() {
        // 300 slots: the slowest three are all in the second batch.
        let pass = pass_of(&[
            batch_naming(100, &[9.0, 8.0, 7.0, 6.0]),
            batch_naming(100, &[500.0, 400.0, 300.0, 5.0]),
            batch_naming(100, &[4.0, 3.0, 2.0, 1.0]),
        ]);
        assert_eq!(
            pass.slowest_hundredth(),
            Some(SlowestHundredth {
                slots: 3,
                ms: 1_200.0,
                exact: true,
            })
        );
    }

    #[test]
    fn a_batch_that_named_every_slow_slot_it_could_makes_the_figure_a_floor() {
        // 600 slots, so six are wanted. The first batch named four, all slow,
        // and timed more than it named: a fifth slow slot may be unnamed.
        let pass = pass_of(&[
            batch_naming(300, &[500.0, 400.0, 300.0, 200.0]),
            batch_naming(300, &[50.0, 40.0, 3.0, 2.0]),
        ]);
        assert_eq!(
            pass.slowest_hundredth(),
            Some(SlowestHundredth {
                slots: 6,
                ms: 1_490.0,
                exact: false,
            })
        );
        assert!(
            pass.summary()
                .unwrap()
                .contains("the slowest 6 (1%) took at least 1.5s")
        );

        // The same first batch naming every slot it timed left none out.
        let pass = pass_of(&[
            batch_naming(4, &[500.0, 400.0, 300.0, 200.0]),
            batch_naming(596, &[50.0, 40.0, 3.0, 2.0]),
        ]);
        assert_eq!(
            pass.slowest_hundredth(),
            Some(SlowestHundredth {
                slots: 6,
                ms: 1_490.0,
                exact: true,
            })
        );
    }

    #[test]
    fn a_pass_with_fewer_named_slots_than_a_hundredth_says_at_least() {
        // 1,000 slots want ten; the one batch named three.
        let pass = pass_of(&[batch_naming(1_000, &[30.0, 20.0, 10.0])]);
        assert_eq!(
            pass.slowest_hundredth(),
            Some(SlowestHundredth {
                slots: 10,
                ms: 60.0,
                exact: false,
            })
        );
    }

    #[test]
    fn a_batch_whose_named_slots_are_all_slow_says_there_may_be_more() {
        let describe = |_: &InferSlotTiming| "a slot".to_string();
        let all_slow = batch_naming(500, &[900.0, 800.0, 700.0]);
        let lines = batch_timing_lines(2, 5, Duration::from_secs(4), &all_slow, describe);
        assert_eq!(lines.len(), 4);
        assert!(
            lines[0].contains("; at least 3 took 100ms or more;"),
            "{}",
            lines[0]
        );

        let one_quick = batch_naming(500, &[900.0, 800.0, 99.0]);
        let lines = batch_timing_lines(2, 5, Duration::from_secs(4), &one_quick, describe);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("; 2 took 100ms or more;"), "{}", lines[0]);

        let none_slow = batch_naming(500, &[9.0, 8.0]);
        let lines = batch_timing_lines(2, 5, Duration::from_secs(4), &none_slow, describe);
        assert_eq!(
            lines,
            vec![
                "Signature inference batch 2 of 5: 500 slot(s) took 1000.0s of the 4.0s waited \
                 (600.0s computing types, 390.0s printing them); the slowest took 9ms; 0 took \
                 100ms or more; the 0 first asked of their file took 0.0s"
            ]
        );
    }

    #[test]
    fn a_slot_the_pass_did_not_send_is_named_by_where_the_sidecar_says_it_is() {
        let (definitions, targets, _) = three_functions();
        let stray: InferSlotTiming = serde_json::from_value(serde_json::json!({
            "alias": "not_ours",
            "file_path": "/r/elsewhere.ts",
            "line_number": 7,
            "infer_kind": "signature_param",
            "ms": 1.0,
            "type_ms": 0.2,
            "print_ms": 0.1,
            "printed_length": 3,
            "first_in_file": false,
        }))
        .unwrap();
        assert_eq!(
            describe_slot(&stray, &targets, &definitions),
            "an unknown function's parameter at /r/elsewhere.ts:7"
        );
    }
}
