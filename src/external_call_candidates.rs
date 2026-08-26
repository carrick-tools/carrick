//! Outbound call candidates: the scanner's egress channel.
//!
//! Two sources feed one row shape.
//!
//! **SDK-mediated calls** ([`scan_workspace`]) are detected here,
//! deterministically and AST-only. The scanner's existing outbound-call
//! candidates are HTTP-shaped: `fetch` / axios-style calls, env-var-anchored
//! URLs, absolute URLs. A call made through a service client imported from an
//! npm package never looks like that, so it is invisible to candidate
//! generation and to every surface downstream of it.
//!
//! **HTTP-shaped calls** ([`from_data_calls`]) are already extracted and
//! already classified; they are projected into this channel rather than
//! detected again. A call to a host the repo declares in `externalDomains`, or
//! through a base declared in `externalEnvVars`, is excluded from endpoint
//! matching by design — which is correct, and which used to mean the fact of
//! the call was recorded nowhere an egress inventory could read. Projecting the
//! row changes no classification and no matching; it records what the
//! classification already decided.
//!
//! # How the SDK detection works
//!
//! In a monorepo the file that calls a vendor client is rarely the file that
//! imports it. A shared package wraps the client, exports a handle, and dozens
//! of modules elsewhere in the tree call through the handle. Resolving only a
//! file's own imports therefore sees the wrapper and misses every caller.
//!
//! So the scan runs over the whole workspace and answers one question per
//! binding: which external package, if any, does the value in it come from?
//!
//! 1. **Facts.** Every source file in the repo is parsed once and reduced to
//!    plain data — imports, re-exports, exports, aliases, function return
//!    shapes, class field maps, and call sites. Every value expression the scan
//!    cares about reduces to the same shape: a root (a binding, or `this`) plus
//!    a list of steps, each either a property access or a call.
//! 2. **Ownership.** A monotone fixpoint over those facts assigns each binding
//!    and each export an owner: an external package, a record of properties
//!    that are owned, a namespace standing for another file's exports, or a
//!    function whose calls yield an owner. Two different packages reaching the
//!    same slot is a conflict, and a conflict is terminal: the slot owns
//!    nothing rather than owning a guess.
//! 3. **Rows.** A call site becomes a row when its own root-and-steps chain
//!    evaluates to a package.
//! 4. **Attribution.** Each service takes the rows of the files reachable from
//!    its own file list by following internal import edges. A file shared by
//!    two services appears in both, which is the intended reading: this
//!    service's deployment contains this call site.
//!
//! The detection is intentionally narrow:
//!
//! - **Structural, not name-based.** The external universe is the union of the
//!   runtime dependencies every `package.json` in the tree declares, minus the
//!   workspace's own package names. There is no allowlist and no denylist of
//!   SDK or vendor names anywhere in this module.
//! - **Almost no type reading.** Resolution is limited to what the AST proves
//!   about values, with two exceptions, both of them a declaration the code
//!   makes about its own shape rather than an inference over the type graph: a
//!   function's declared return type, and a class property's declared type. A
//!   name written in either position, when the file imports that name from a
//!   dependency, owns that dependency. Every other type position stays out —
//!   a parameter typed by an imported class, a value read off a DI container,
//!   a variable's declared type — because none of those says what the value
//!   the code hands back or holds actually is.
//! - **Unknown never blocks, disagreement does.** An unresolved contributor to
//!   a slot leaves the slot's other contributors intact; two contributors that
//!   name different packages drop it. That is the recall-leaning choice, taken
//!   deliberately: a wrapper whose branches all reach one package is the common
//!   shape, and a wrapper that reaches two is a genuine ambiguity.
//! - **Ownership is anchored, not just packaged.** A package-owned value
//!   carries the export it came from (`default` for a default import, the
//!   exported name for a named one, nothing for a namespace) and the subpath
//!   the specifier named (`edge` for `pkg/edge`). Rows report both, and the
//!   rule above reaches them: two chains that name one package through
//!   different exports, or through different subpaths, disagree and drop the
//!   slot.
//! - **Never merged into HTTP matching.** These rows are their own mechanism
//!   and their own payload field. They are not endpoints, they are not consumer
//!   calls, and nothing here touches extraction, matching, or type
//!   compatibility. Folding SDK traffic into HTTP endpoint matching is the
//!   known false-positive class this design exists to avoid.
//!
//! These are candidates, not verified egress. Any declared dependency
//! qualifies, so a web framework, a local crypto library, and a validation
//! library all produce rows alongside genuine service clients, which is the
//! intended trade of recall in the scanner against classification downstream.
//!
//! # Known limitations
//!
//! All deliberate, and all silent rather than guessed:
//!
//! - **ESM only.** `import` declarations, re-exports, and `import()` with a
//!   string literal. `require()`, TypeScript `import =`, and a dynamic import
//!   whose specifier is computed produce no bindings and no reachability edge.
//! - **Type-only imports bind nothing.** `import type ...`, per-specifier
//!   `import { type Foo }`, and type-only re-exports bind no value and are not
//!   reachability edges, so no chain runs through one. They do name a package
//!   for the two annotation positions above: a factory declared to return a
//!   type-imported client owns that client's package, which is the shape the
//!   annotation rule exists for.
//! - **Destructuring** is followed through object patterns with identifier
//!   keys. Array patterns, rest elements, and computed keys are not.
//! - **Transitivity has a shape, not a depth limit.** A handle reached through
//!   wrapper modules, a factory's return value, a property of a returned object
//!   literal, a class field, or a barrel re-export chain resolves however many
//!   hops away it was written. A value that only exists at runtime — read from
//!   a container, selected by configuration, passed in as a parameter — does
//!   not.
//! - **Unknown destinations emit nothing.** An internal wrapper around the
//!   global `fetch` is real egress with no nameable target, so it produces no
//!   row here.
//! - **Constructor calls** (`new Client(...)`) resolve the receiver without
//!   becoming rows themselves, because constructing a client is not egress.
//! - **Class fields** resolve one level and without inheritance. A field
//!   assigned from a constructor parameter is filled in from the construction
//!   sites of that class in the same file, and every site contributes to one
//!   field map, so two instances built with differently-owned arguments are not
//!   told apart. A base class's fields do not reach a subclass, and a class
//!   constructed through an imported binding carries no constructor facts.
//!   `this` inside a class resolves against the innermost enclosing class
//!   whatever function nests it.
//!
//! What a value's configuration comes from is not a limit on any of this. A
//! factory reads its host and its credentials from the environment or from the
//! database and still constructs the same client, so the binding owns the
//! package the constructing expression names. Ownership is about which code
//! the call goes through, not about which account it reaches.

use crate::analyzer::Analyzer;
use crate::config::Config;
use crate::file_finder::find_files;
use crate::mount_graph::DataFetchingCall;
use crate::packages::MANIFEST_SKIP_DIRS;
use crate::type_manifest::parse_file_location;
use crate::url_normalizer::UrlNormalizer;
use crate::workspace_resolver::{Resolution, WorkspaceIndex};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use swc_common::{FileName, GLOBALS, Globals, Mark, SourceMap, sync::Lrc};
use swc_ecma_ast::*;
use swc_ecma_parser::{EsSyntax, Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
use swc_ecma_transforms_base::resolver;
use swc_ecma_visit::{Visit, VisitMutWith, VisitWith};
use tracing::warn;

/// Upper bound on the rows one service contributes to the upload payload.
/// Rows are sorted before truncation, so a repo over the cap yields a stable
/// prefix rather than an arbitrary sample.
///
/// Measured rather than guessed. Once the scan became workspace-transitive, a
/// service stopped contributing only its own directory and started contributing
/// every shared package it reaches, which is a different order of magnitude: an
/// offline probe over a large real-world monorepo produced roughly 15k, 7k, and
/// 3.5k rows for its three services. The 5000 the channel shipped with would
/// have silently truncated the largest of those, and truncation follows the sort
/// order — file first — so what it drops is the tail of the alphabet, which in a
/// monorepo is disproportionately the shared packages the widening was built to
/// reach. 20000 leaves headroom above the measured peak while staying small:
/// rows are around 100 bytes, so a service at the cap is about 2MB, far under
/// the request limit the cap exists to protect.
pub const MAX_CANDIDATES_PER_SERVICE: usize = 20000;

/// Safety net on the ownership fixpoint. Each round can only move a slot up a
/// finite lattice, so the loop terminates on its own; the bound turns a bug in
/// that argument into a logged, truncated result rather than a scan that never
/// returns on somebody's repo.
const MAX_FIXPOINT_ROUNDS: usize = 64;

/// How the scanner established that a call site leaves the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallMechanism {
    /// The callee resolves, through the workspace, to a declared runtime
    /// dependency.
    Sdk,
    /// An extracted HTTP call whose target names a host the repo declares in
    /// `externalDomains`.
    ExternalHttp,
    /// An extracted HTTP call whose base URL is an environment variable the
    /// repo declares in `externalEnvVars`.
    EnvVarUrl,
}

/// A value that came out of a dependency, with the import that anchored it.
///
/// The package is the destination a row reports. The two anchors say which
/// export of it, and which entry point of it, the receiver was reached
/// through. They are part of the value's identity rather than decoration on
/// it, so two chains reaching one slot through different exports of the same
/// package disagree, and disagreement drops the slot. That is the rule the
/// package name is already held to, applied one level finer. A handle built
/// from `pkg`'s default export and one built from its `edge` entry point are
/// not interchangeable, and a slot that is sometimes one and sometimes the
/// other says nothing a reader can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PkgRef {
    /// The dependency exactly as a `package.json` names it.
    package: String,
    /// The exported name the root binding came from: `default` for a default
    /// import, the exported name for a named one, `None` for a namespace
    /// import, which binds the module rather than one of its exports.
    import_symbol: Option<String>,
    /// What the specifier named under the package root — `edge` for
    /// `pkg/edge` — and `None` when it named the root.
    subpath: Option<String>,
}

/// One outbound call candidate.
///
/// Ordering is `(file, line, callee, package, mechanism)`, which is also the
/// sort key rows are emitted in. The two anchor fields sit after that key and
/// never reorder anything, because one call site yields one row and so no two
/// rows share the key. SDK rows are byte-identical across runs of the same
/// tree because they are pure AST; the HTTP-shaped rows are projected from LLM
/// extraction and inherit its stability, so the ORDER is fixed but the row set
/// is only as reproducible as the extraction behind it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExternalCallCandidate {
    /// Repo-relative path of the file containing the call.
    pub file: String,
    /// 1-based line number of the call expression.
    pub line: usize,
    /// What was called, in the terms the mechanism knows.
    ///
    /// For [`CallMechanism::Sdk`], the callee as written, dotted
    /// (`ledger.payments.create`), computed from the AST so it carries no
    /// whitespace or comments from the source. A call inside the chain prints
    /// as `()` (`getTransport().verify`), and a receiver held in a class field
    /// prints as written (`this.client.upload`). For the two HTTP-shaped
    /// mechanisms there is no callee to name — the client is `fetch` or an
    /// axios-alike in every case — so the field carries the HTTP method, which
    /// is the operation the row records.
    pub callee: String,
    /// Where the call goes, in the terms the mechanism knows: the declared
    /// dependency for [`CallMechanism::Sdk`], exactly as the `package.json`
    /// names it (a subpath import such as `pkg/edge` resolves to `pkg`); the
    /// hostname for [`CallMechanism::ExternalHttp`]; the environment variable
    /// name for [`CallMechanism::EnvVarUrl`].
    ///
    /// The field keeps the name it shipped with in carrick#511, when `sdk` was
    /// the only mechanism. cloud#350 calls the same column `target`.
    pub package: String,
    /// Which of the three detections produced this row.
    pub mechanism: CallMechanism,
    /// Which export of `package` the receiver's root binding came from:
    /// `default` for a default import, the exported name for a named one.
    /// Absent for a namespace import, which binds the module rather than one
    /// of its exports, and absent for the two HTTP-shaped mechanisms, which
    /// have no import behind them at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_symbol: Option<String>,
    /// The subpath the import specifier named under the package root — `edge`
    /// for `pkg/edge`. Absent when the import named the root, and absent on
    /// the HTTP-shaped mechanisms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
}

/// Scan the whole workspace and return the SDK-mediated call candidates that
/// belong to the service whose files are `service_files`.
///
/// The scan is workspace-wide because ownership is: the file that imports a
/// vendor client and the file that calls through it are routinely in different
/// packages. Attribution is per service: a row survives when its file is
/// reachable from `service_files` by following internal import edges, which is
/// the same question as "does this service's deployment contain that code?".
///
/// `service_files` is expected to be the service's already-filtered source list
/// from [`crate::file_finder::find_service_files`], and the workspace pass uses
/// the same walk over `repo_root`, so test trees, story files, and
/// vendored/build directories are excluded by the scanner's existing rules
/// rather than by anything invented here.
///
/// Computed fresh per service. One extra parse pass per service costs less than
/// threading a cache through the engine would, and keeps this entry point a
/// pure function of the tree on disk.
pub fn scan_workspace(service_files: &[PathBuf], repo_root: &Path) -> Vec<ExternalCallCandidate> {
    let index = WorkspaceIndex::build(repo_root);
    if !index.has_external_packages() {
        return Vec::new();
    }

    let workspace = Workspace::parse(repo_root, &index, service_files);
    let ownership = workspace.resolve_ownership();
    let rows_by_file = workspace.rows(&ownership);

    let mut rows: BTreeSet<ExternalCallCandidate> = BTreeSet::new();
    for file in workspace.reachable_from(service_files, repo_root) {
        if let Some(file_rows) = rows_by_file.get(&file) {
            rows.extend(file_rows.iter().cloned());
        }
    }
    cap(rows.into_iter().collect())
}

/// Project the already-classified HTTP-shaped calls of a service into channel
/// rows.
///
/// `data_calls` are the consumer calls of the built mount graph, so every row
/// here corresponds to a call the scanner already extracted, already keyed, and
/// already classified. Nothing is re-detected and nothing is re-classified:
/// this reads `host` (retained by the mount-graph builder from normalisation)
/// and the same `externalDomains` / `externalEnvVars` declarations the matcher
/// consults, and writes down what they already say.
///
/// Which calls become rows, and the reason in each case:
///
/// - **Declared-external host** → [`CallMechanism::ExternalHttp`]. The repo has
///   said this host is somebody else's.
/// - **Declared-external env-var base** → [`CallMechanism::EnvVarUrl`]. Same
///   statement, made about a base URL the source does not spell out.
/// - **Declared-internal, either shape** → no row. An in-org destination is
///   already carried by the endpoint index, and calling it is not egress.
/// - **Undeclared, either shape** → no row. Where the call goes is exactly what
///   is unknown; the scanner already reports the undeclared env-var bases as
///   `EnvVarCall` findings, and inventing an egress row from a host nobody
///   classified would be a guess. The retained `host` on the call itself is
///   what a later surface would widen from, without another scan.
///
/// A call whose extracted line is not a positive integer is skipped: a row
/// without a location cannot be audited.
pub fn from_data_calls(
    data_calls: &[DataFetchingCall],
    repo_root: &Path,
    config: &Config,
    normalizer: &UrlNormalizer,
) -> Vec<ExternalCallCandidate> {
    let mut rows: BTreeSet<ExternalCallCandidate> = BTreeSet::new();
    for call in data_calls {
        let Some((target, mechanism)) = classify_data_call(call, config, normalizer) else {
            continue;
        };
        let Some(line) = call.line else {
            continue;
        };
        // `file_location` packs the file and the line as `"{file}:{line}"`;
        // only the file half is wanted here, because the line is carried
        // typed. `parse_file_location` is the repo's existing unpacker, and is
        // used rather than a second string split so this file half is the same
        // one every other consumer of the location sees.
        let (file, _) = parse_file_location(&call.file_location);
        rows.insert(ExternalCallCandidate {
            file: relative_location(&file, repo_root),
            line: line as usize,
            // No callee to name on an HTTP call; the method is the operation.
            callee: call.method.to_uppercase(),
            package: target,
            mechanism,
            // An HTTP row has no import behind it, so neither anchor applies.
            import_symbol: None,
            subpath: None,
        });
    }
    cap(rows.into_iter().collect())
}

/// The mechanism and target for one already-classified consumer call, or `None`
/// when the call is not declared external.
fn classify_data_call(
    call: &DataFetchingCall,
    config: &Config,
    normalizer: &UrlNormalizer,
) -> Option<(String, CallMechanism)> {
    // Host first: a literal absolute origin is never an env-var base, and the
    // host is only retained for that shape.
    if let Some(host) = &call.host {
        return normalizer
            .is_external_host(host)
            .then(|| (host.clone(), CallMechanism::ExternalHttp));
    }
    // The env-var check runs on `canonical_path` — the key the matcher itself
    // classifies on — so this row names the variable the matcher excluded the
    // call for, not a differently-derived one.
    if !Analyzer::is_env_var_base_url(&call.canonical_path) {
        return None;
    }
    let env_var = Analyzer::extract_env_var_name(&call.canonical_path);
    config
        .is_external_env_var(&env_var)
        .then_some((env_var, CallMechanism::EnvVarUrl))
}

/// Sort order is already fixed by the `BTreeSet` the rows arrive in; this
/// applies the per-service cap, so a service over it yields a stable prefix
/// rather than an arbitrary sample. The cap spans mechanisms, because the cap
/// exists to bound the payload and the payload carries one list.
fn cap(mut rows: Vec<ExternalCallCandidate>) -> Vec<ExternalCallCandidate> {
    if rows.len() > MAX_CANDIDATES_PER_SERVICE {
        warn!(
            "External call candidates truncated to {} of {} rows for this service",
            MAX_CANDIDATES_PER_SERVICE,
            rows.len()
        );
        rows.truncate(MAX_CANDIDATES_PER_SERVICE);
    }
    rows
}

/// Merge the rows of both sources into the single list the payload carries.
///
/// Sorted and deduplicated as one set, then capped once, so the channel has one
/// order and one bound whatever produced a given row.
pub fn merge(
    sdk_rows: Vec<ExternalCallCandidate>,
    http_rows: Vec<ExternalCallCandidate>,
) -> Vec<ExternalCallCandidate> {
    let merged: BTreeSet<ExternalCallCandidate> = sdk_rows.into_iter().chain(http_rows).collect();
    cap(merged.into_iter().collect())
}

/// Repo-relative file path for a projected HTTP row.
///
/// The mount graph keys `file_location` as the analysis scanned it: already
/// repo-relative on the incremental path (the engine normalises the keys before
/// rebuilding the graph) and as-scanned on the full path. Strip the scan root
/// when it is present, and a leading `./` either way, so the two paths produce
/// the same row for the same file — and so an HTTP row is comparable with an
/// SDK row, which is relative by construction.
fn relative_location(file: &str, repo_root: &Path) -> String {
    let path = Path::new(file);
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .trim_start_matches("./")
        .to_string()
}

// ---------------------------------------------------------------------------
// Reduced expressions
// ---------------------------------------------------------------------------

/// A binding, identified by name and by the syntax context the resolver
/// stamped on it, so a local `const client = ...` inside a function does not
/// collide with a module-level import of the same name.
///
/// The context is kept as a plain integer rather than swc's `SyntaxContext` so
/// that map ordering is a property of the source rather than of interning
/// order, which matters because the row list must be byte-identical between
/// runs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct BindingId {
    symbol: String,
    context: u32,
}

impl BindingId {
    fn of(ident: &Ident) -> Self {
        BindingId {
            symbol: ident.sym.to_string(),
            context: ident.ctxt.as_u32(),
        }
    }

    fn of_binding(ident: &BindingIdent) -> Self {
        BindingId::of(&ident.id)
    }

    /// The slot an `export default <expression>` writes into. `*default*` is
    /// not a legal identifier, so it can never collide with a real binding.
    fn default_export() -> Self {
        BindingId {
            symbol: "*default*".to_string(),
            context: 0,
        }
    }
}

/// Where a chain starts.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Root {
    Binding(BindingId),
    /// `this` inside a class, carrying the index of the class whose field map
    /// resolves it.
    This(usize),
}

/// One hop along a chain.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Prop(String),
    Call,
}

/// A value expression reduced to the only thing ownership can use: where it
/// starts, and what was applied to it.
///
/// This one shape carries every resolvable form — a receiver, a factory result,
/// a destructured property, a call in the middle of a callee — so the rules do
/// not each need their own matcher.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Chain {
    root: Root,
    steps: Vec<Step>,
}

impl Chain {
    fn extended(&self, step: Step) -> Chain {
        let mut steps = self.steps.clone();
        steps.push(step);
        Chain {
            root: self.root.clone(),
            steps,
        }
    }

    /// The callee as written. A call inside the chain prints as `()`.
    fn text(&self) -> String {
        let mut text = match &self.root {
            Root::Binding(id) => id.symbol.clone(),
            Root::This(_) => "this".to_string(),
        };
        for step in &self.steps {
            match step {
                Step::Prop(name) => {
                    text.push('.');
                    text.push_str(name);
                }
                Step::Call => text.push_str("()"),
            }
        }
        text
    }
}

// ---------------------------------------------------------------------------
// Ownership lattice
// ---------------------------------------------------------------------------

/// What one property of a record is known to own. Conflict is representable so
/// that disagreement is sticky: a property two branches disagree about must
/// stay dropped once it has been dropped, or the fixpoint could oscillate.
///
/// A record property carries a package and nothing richer, deliberately. That is
/// what bounds the lattice: if a property could hold another record, a cyclic
/// data structure would let records grow without limit and the fixpoint would
/// never settle. Values that need to nest — a static field holding an instance,
/// whose own field holds the client — live in slots of their own rather than
/// inside a record.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PropValue {
    Pkg(PkgRef),
    Conflict,
}

/// What a resolved slot holds.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Owner {
    /// A value that came out of an external package. Any property of it, and
    /// any call on it, is still that package's, and carries the same anchors.
    Pkg(PkgRef),
    /// An object whose named properties are owned — a function's returned
    /// object literal, or a class instance's field map.
    Record(BTreeMap<String, PropValue>),
    /// A namespace standing for another file's exports; a property lookup goes
    /// through that file's export table.
    NamespaceOf(usize),
    /// The class declared at `(file, class index)`, as a value. A property of it
    /// is a static slot; constructing it yields the class's instance field map.
    /// A marker rather than a record, so a static slot can hold anything a slot
    /// can hold — an instance, a factory — which is what the singleton idiom
    /// needs and what a record's package-only properties cannot express.
    ClassOf(usize, usize),
    /// A function whose calls yield the boxed owner. The binding itself carries
    /// through assignment and export; only a call position unwraps it.
    FnReturning(Box<Owner>),
}

/// A slot's value. Absent means unknown, which never blocks: it is the other
/// contributors to the slot that decide. `Conflict` is the top of the lattice
/// and is terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Owned(Owner),
    Conflict,
}

/// Two owners joined.
///
/// Equality is structural, so two [`Owner::Pkg`] values that name one package
/// through different exports or different subpaths are already unequal here
/// and already fall through to `Conflict`. The symbol and subpath needed no
/// arm of their own: they are part of what a package-owned value *is*.
fn join_owners(left: &Owner, right: &Owner) -> Value {
    if left == right {
        return Value::Owned(left.clone());
    }
    match (left, right) {
        (Owner::Record(a), Owner::Record(b)) => {
            let mut merged = a.clone();
            for (key, value) in b {
                match merged.get(key) {
                    Some(existing) if existing == value => {}
                    Some(_) => {
                        merged.insert(key.clone(), PropValue::Conflict);
                    }
                    None => {
                        merged.insert(key.clone(), value.clone());
                    }
                }
            }
            Value::Owned(Owner::Record(merged))
        }
        (Owner::FnReturning(a), Owner::FnReturning(b)) => match join_owners(a, b) {
            Value::Owned(owner) => Value::Owned(Owner::FnReturning(Box::new(owner))),
            Value::Conflict => Value::Conflict,
        },
        _ => Value::Conflict,
    }
}

/// The whole workspace's ownership state. Every map is a `BTreeMap` because
/// the star-export rule iterates one of them, and iteration order there would
/// otherwise reach the row list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Ownership {
    bindings: BTreeMap<(usize, BindingId), Value>,
    exports: BTreeMap<(usize, String), Value>,
    /// A class's instance field map, keyed by file and class index. Separate
    /// from the class's static slots: conflating the two would let a static read
    /// resolve through an instance field.
    class_instances: BTreeMap<(usize, usize), Value>,
    /// One slot per static member, keyed by file, class index, and name. A slot
    /// rather than a property of a record on the class binding, because the
    /// singleton idiom stores an INSTANCE in a static field and reads a field of
    /// it back out — two levels, which a record's package-only properties cannot
    /// carry.
    static_slots: BTreeMap<(usize, usize, String), Value>,
}

impl Ownership {
    fn join_binding(&mut self, key: (usize, BindingId), value: Value) {
        join_into(&mut self.bindings, key, value);
    }

    fn join_export(&mut self, key: (usize, String), value: Value) {
        join_into(&mut self.exports, key, value);
    }

    fn join_class_instance(&mut self, key: (usize, usize), value: Value) {
        join_into(&mut self.class_instances, key, value);
    }

    fn join_static_slot(&mut self, key: (usize, usize, String), value: Value) {
        join_into(&mut self.static_slots, key, value);
    }

    fn binding(&self, file: usize, id: &BindingId) -> Option<&Owner> {
        owner_of(self.bindings.get(&(file, id.clone())))
    }

    fn export(&self, file: usize, name: &str) -> Option<&Owner> {
        owner_of(self.exports.get(&(file, name.to_string())))
    }

    fn class_instance(&self, file: usize, class: usize) -> Option<&Owner> {
        owner_of(self.class_instances.get(&(file, class)))
    }

    fn static_slot(&self, file: usize, class: usize, name: &str) -> Option<&Owner> {
        owner_of(self.static_slots.get(&(file, class, name.to_string())))
    }

    /// Evaluate a chain against this state: the root's owner, then one step at
    /// a time. `None` is "not established", which covers both unknown and
    /// conflicted.
    fn eval(&self, file: usize, chain: &Chain) -> Option<Owner> {
        let mut owner = match &chain.root {
            Root::Binding(id) => self.binding(file, id)?.clone(),
            Root::This(class) => self.class_instance(file, *class)?.clone(),
        };
        for step in &chain.steps {
            owner = match (step, owner) {
                // Anything reached off a package's value is still that
                // package's: a sub-client, a namespace, a builder.
                (_, Owner::Pkg(package)) => Owner::Pkg(package),
                (Step::Prop(name), Owner::Record(fields)) => match fields.get(name) {
                    Some(PropValue::Pkg(package)) => Owner::Pkg(package.clone()),
                    _ => return None,
                },
                (Step::Prop(name), Owner::NamespaceOf(target)) => {
                    self.export(target, name)?.clone()
                }
                (Step::Prop(name), Owner::ClassOf(target, class)) => {
                    self.static_slot(target, class, name)?.clone()
                }
                // A class in call position is `new C(...)`: the chain reducer
                // spells construction the same way it spells a call, and calling
                // a class without `new` throws, so the two cannot be confused in
                // code that runs.
                (Step::Call, Owner::ClassOf(target, class)) => {
                    self.class_instance(target, class)?.clone()
                }
                (Step::Call, Owner::FnReturning(returned)) => *returned,
                _ => return None,
            };
        }
        Some(owner)
    }

    /// The package a chain resolves to, if it resolves to one at all. Records,
    /// namespaces, and uncalled functions are resolved values but not
    /// destinations, so they yield no row.
    fn resolve_package(&self, file: usize, chain: &Chain) -> Option<PkgRef> {
        match self.eval(file, chain)? {
            Owner::Pkg(package) => Some(package),
            _ => None,
        }
    }
}

fn owner_of(value: Option<&Value>) -> Option<&Owner> {
    match value {
        Some(Value::Owned(owner)) => Some(owner),
        _ => None,
    }
}

fn join_into<K: Ord>(map: &mut BTreeMap<K, Value>, key: K, value: Value) {
    let joined = match (map.get(&key), &value) {
        (None, _) => value.clone(),
        (Some(Value::Conflict), _) | (_, Value::Conflict) => Value::Conflict,
        (Some(Value::Owned(existing)), Value::Owned(incoming)) => join_owners(existing, incoming),
    };
    map.insert(key, joined);
}

// ---------------------------------------------------------------------------
// Per-file facts
// ---------------------------------------------------------------------------

/// Where a name imported or re-exported by a file comes from.
#[derive(Debug, Clone)]
enum Source {
    ExternalPackage(PkgRef),
    /// A named export of another workspace file.
    InternalNamed(usize, String),
    /// Another workspace file taken whole.
    InternalNamespace(usize),
}

#[derive(Debug, Clone)]
struct ImportFact {
    local: BindingId,
    source: Source,
}

#[derive(Debug, Clone)]
struct ReExportFact {
    name: String,
    source: Source,
}

#[derive(Debug, Clone)]
struct ExportFact {
    name: String,
    local: BindingId,
}

#[derive(Debug, Clone)]
struct AliasFact {
    local: BindingId,
    value: Chain,
}

/// One argument at a construction site, reduced to what the constructor's field
/// assignments can read out of it.
#[derive(Debug, Clone)]
enum Argument {
    /// A chain: a binding, or something reached off one.
    Value(Chain),
    /// An object literal, which is how a constructor taking an options bag is
    /// almost always called. Shorthand properties included.
    Record(BTreeMap<String, Chain>),
    /// A spread, or anything the chain reducer cannot name. Occupies the
    /// position so later arguments keep their index.
    Opaque,
}

/// `new LocalClass(args)`, wherever it appears. Collected from every `new`
/// expression rather than only from variable declarators: the singleton idiom
/// constructs into a static field, and a dependency-injected client reaches its
/// class through exactly that assignment.
#[derive(Debug, Clone)]
struct ConstructionFact {
    class_binding: BindingId,
    arguments: Vec<Argument>,
}

/// The returns of one function, in the two shapes ownership can read.
#[derive(Debug, Clone, Default)]
struct ReturnShape {
    /// Every `return <expr>` that reduces to a chain, and an arrow's expression
    /// body.
    returns: Vec<Chain>,
    /// Properties of returned object literals: the property name and the chain
    /// its value reduces to. Every branch of a ternary or of `??`/`||` in the
    /// property's position contributes separately.
    record_returns: Vec<(String, Chain)>,
}

impl ReturnShape {
    fn is_empty(&self) -> bool {
        self.returns.is_empty() && self.record_returns.is_empty()
    }
}

#[derive(Debug, Clone)]
struct FunctionFact {
    local: BindingId,
    shape: ReturnShape,
    /// The dependency the declared return type names, when it names one. The
    /// function's own statement about what it hands back, which is what a
    /// factory has instead of a returned expression the reducer can follow.
    annotation: Option<PkgRef>,
}

/// A static member that is a function: a factory, or a getter standing in for a
/// field.
#[derive(Debug, Clone)]
struct StaticFunction {
    name: String,
    shape: ReturnShape,
    /// A getter is read as a property, so its slot holds what it returns; a
    /// method is called, so its slot holds a function.
    is_getter: bool,
}

#[derive(Debug, Clone)]
struct ClassFact {
    /// The binding the class is declared under, when it has one. That binding
    /// resolves to the class itself, through which the static slots are reached.
    binding: Option<BindingId>,
    instance_fields: Vec<(String, Chain)>,
    /// Instance properties whose declared type names a dependency. The class's
    /// own statement about what the field holds, which is what a field nothing
    /// visibly assigns has instead of an assignment.
    annotation_fields: Vec<(String, PkgRef)>,
    /// Static property initializers.
    static_fields: Vec<(String, Chain)>,
    static_functions: Vec<StaticFunction>,
    /// `this.<field> = <constructor parameter>[.<property>...]`: the field, the
    /// parameter's position, and the path read off it. The bare-parameter form
    /// has an empty path. Filled in from the construction sites, one level, no
    /// inheritance.
    ctor_param_fields: Vec<(String, usize, Vec<String>)>,
}

#[derive(Debug, Clone)]
struct CallSite {
    line: usize,
    callee: Chain,
}

#[derive(Debug, Default)]
struct FileFacts {
    imports: Vec<ImportFact>,
    /// Every name this file imports from a dependency, whether the import
    /// binds a value or only a type, keyed by the local name a type annotation
    /// would write. Keyed by name rather than by binding because a type
    /// reference is erased before it reaches a runtime binding, so there is no
    /// syntax context to compare it on.
    type_names: BTreeMap<String, PkgRef>,
    reexports: Vec<ReExportFact>,
    /// `export * from <internal file>`.
    star_exports: BTreeSet<usize>,
    exports: Vec<ExportFact>,
    aliases: Vec<AliasFact>,
    constructions: Vec<ConstructionFact>,
    /// `<identifier>.<field> = <expression>` at any depth. Folded against the
    /// file's classes: the ones whose identifier names a class are static
    /// assignments, the rest are dropped.
    member_assignments: Vec<(BindingId, String, Chain)>,
    functions: Vec<FunctionFact>,
    classes: Vec<ClassFact>,
    call_sites: Vec<CallSite>,
    /// Files this one pulls in, however it pulls them in: static imports,
    /// re-exports, stars, and literal dynamic imports all count, because all
    /// four ship the target with the caller.
    edges: BTreeSet<usize>,
}

/// Every source file in the repo, reduced to facts.
struct Workspace {
    files: Vec<PathBuf>,
    facts: Vec<FileFacts>,
}

impl Workspace {
    fn parse(repo_root: &Path, index: &WorkspaceIndex, service_files: &[PathBuf]) -> Workspace {
        // The same walk the per-service scan uses, so the two see one file set
        // and one set of exclusions.
        let (mut absolute, _) = find_files(&repo_root.to_string_lossy(), &MANIFEST_SKIP_DIRS);

        // Those exclusions are relative to the walk root, so a service rooted
        // at a directory named after a build artifact (`packages/build`) is
        // scanned by its own walk and skipped by the repo-wide one. Its files
        // are added back rather than the exclusion being weakened: the
        // directory is genuinely a service when it is the configured root and
        // genuinely build output when it is not.
        absolute.extend(service_files.iter().cloned());
        absolute.sort();
        absolute.dedup();

        let files: Vec<PathBuf> = absolute
            .iter()
            .map(|path| path.strip_prefix(repo_root).unwrap_or(path).to_path_buf())
            .collect();
        let file_index: BTreeMap<PathBuf, usize> = files
            .iter()
            .enumerate()
            .map(|(idx, path)| (path.clone(), idx))
            .collect();

        let facts = files
            .iter()
            .zip(absolute.iter())
            .map(|(relative, path)| collect_facts(relative, path, index, &file_index))
            .collect();

        Workspace { files, facts }
    }

    /// Run the ownership rules to a fixpoint.
    ///
    /// Each round reads the previous round's state and writes a new one, so the
    /// result cannot depend on the order facts are visited in. Every write is a
    /// join, so a slot only ever moves up the lattice, and the lattice has
    /// finite height per slot — that is what makes the loop terminate.
    fn resolve_ownership(&self) -> Ownership {
        let mut state = Ownership::default();
        for round in 0..MAX_FIXPOINT_ROUNDS {
            let previous = state.clone();
            for (file, facts) in self.facts.iter().enumerate() {
                self.apply_facts(file, facts, &previous, &mut state);
            }
            if state == previous {
                return state;
            }
            if round + 1 == MAX_FIXPOINT_ROUNDS {
                warn!(
                    "External call candidate ownership did not settle in {} rounds; \
                     reporting what resolved so far",
                    MAX_FIXPOINT_ROUNDS
                );
            }
        }
        state
    }

    fn apply_facts(
        &self,
        file: usize,
        facts: &FileFacts,
        previous: &Ownership,
        next: &mut Ownership,
    ) {
        for import in &facts.imports {
            if let Some(value) = self.source_value(&import.source, previous) {
                next.join_binding((file, import.local.clone()), value);
            }
        }

        for (index, class) in facts.classes.iter().enumerate() {
            let mut instance = record_of(file, &class.instance_fields, previous);
            // A declared property type decides the field. It is the class's own
            // statement about what the field holds, it is a fact of the source
            // rather than of the round, and in the shape it exists for — a
            // field no construction site ever fills in — it is the only thing
            // there.
            for (name, package) in &class.annotation_fields {
                instance.insert(name.clone(), PropValue::Pkg(package.clone()));
            }
            if !instance.is_empty() {
                next.join_class_instance((file, index), record_value(instance));
            }
            if let Some(binding) = &class.binding {
                next.join_binding(
                    (file, binding.clone()),
                    Value::Owned(Owner::ClassOf(file, index)),
                );
            }
            for (name, chain) in &class.static_fields {
                if let Some(owner) = previous.eval(file, chain) {
                    next.join_static_slot((file, index, name.clone()), Value::Owned(owner));
                }
            }
            for function in &class.static_functions {
                let Some(owner) = return_owner(file, &function.shape, previous) else {
                    continue;
                };
                let value = if function.is_getter {
                    // A getter is read, never called, so its slot holds the
                    // returned value rather than a function.
                    Value::Owned(owner)
                } else {
                    Value::Owned(Owner::FnReturning(Box::new(owner)))
                };
                next.join_static_slot((file, index, function.name.clone()), value);
            }
        }

        for function in &facts.functions {
            // Same order as a class property: what the signature declares
            // decides, and the returned expressions answer only when the
            // signature names no dependency. A declared return type is the
            // function's own statement, and it is what a factory that hands
            // back a cached or injected handle has instead of a `return` the
            // reducer can follow.
            let owner = match &function.annotation {
                Some(package) => Some(Owner::Pkg(package.clone())),
                None => return_owner(file, &function.shape, previous),
            };
            if let Some(owner) = owner {
                next.join_binding(
                    (file, function.local.clone()),
                    Value::Owned(Owner::FnReturning(Box::new(owner))),
                );
            }
        }

        for alias in &facts.aliases {
            if let Some(owner) = previous.eval(file, &alias.value) {
                next.join_binding((file, alias.local.clone()), Value::Owned(owner));
            }
        }

        // The fields a constructor fills in from its arguments belong to the
        // class, not to one construction site. Every site contributes to the one
        // instance record, which over-approximates when a class is constructed
        // twice with differently-owned arguments — two instances share one field
        // map. For a candidates channel that is the right trade: the alternative
        // is losing the shape entirely, and a disagreement between two sites
        // still drops the field rather than picking one.
        for construction in &facts.constructions {
            let Some((index, fields)) =
                self.construction_fields(file, facts, construction, previous)
            else {
                continue;
            };
            if !fields.is_empty() {
                next.join_class_instance((file, index), record_value(fields));
            }
        }

        for (target, field, chain) in &facts.member_assignments {
            let Some(index) = facts
                .classes
                .iter()
                .position(|class| class.binding.as_ref() == Some(target))
            else {
                continue;
            };
            if let Some(owner) = previous.eval(file, chain) {
                next.join_static_slot((file, index, field.clone()), Value::Owned(owner));
            }
        }

        for export in &facts.exports {
            if let Some(value) = previous.bindings.get(&(file, export.local.clone())) {
                next.join_export((file, export.name.clone()), value.clone());
            }
        }

        for reexport in &facts.reexports {
            if let Some(value) = self.source_value(&reexport.source, previous) {
                next.join_export((file, reexport.name.clone()), value);
            }
        }

        for star in &facts.star_exports {
            for ((source_file, name), value) in &previous.exports {
                if source_file == star {
                    next.join_export((file, name.clone()), value.clone());
                }
            }
        }
    }

    fn source_value(&self, source: &Source, state: &Ownership) -> Option<Value> {
        match source {
            Source::ExternalPackage(package) => Some(Value::Owned(Owner::Pkg(package.clone()))),
            Source::InternalNamed(target, name) => {
                state.exports.get(&(*target, name.clone())).cloned()
            }
            Source::InternalNamespace(target) => Some(Value::Owned(Owner::NamespaceOf(*target))),
        }
    }

    /// What one construction site proves about the fields its constructor
    /// assigns from its parameters, and which class those fields belong to.
    ///
    /// `None` when the identifier being constructed is not a class declared in
    /// this file: an imported class carries no constructor facts here, one level
    /// and no inheritance.
    fn construction_fields(
        &self,
        file: usize,
        facts: &FileFacts,
        construction: &ConstructionFact,
        state: &Ownership,
    ) -> Option<(usize, BTreeMap<String, PropValue>)> {
        let index = facts
            .classes
            .iter()
            .position(|class| class.binding.as_ref() == Some(&construction.class_binding))?;
        let class = &facts.classes[index];

        let mut fields: BTreeMap<String, PropValue> = BTreeMap::new();
        for (field, position, path) in &class.ctor_param_fields {
            let Some(argument) = construction.arguments.get(*position) else {
                continue;
            };
            let chain = match argument {
                Argument::Value(chain) => {
                    let mut chain = chain.clone();
                    for property in path {
                        chain = chain.extended(Step::Prop(property.clone()));
                    }
                    chain
                }
                // `new C({ client })` reading `options.client`: the property is
                // in the literal, so the argument's own chain for it is what the
                // field owns, with any deeper path appended.
                Argument::Record(properties) => {
                    let Some((first, rest)) = path.split_first() else {
                        continue;
                    };
                    let Some(chain) = properties.get(first) else {
                        continue;
                    };
                    let mut chain = chain.clone();
                    for property in rest {
                        chain = chain.extended(Step::Prop(property.clone()));
                    }
                    chain
                }
                Argument::Opaque => continue,
            };
            if let Some(package) = state.resolve_package(file, &chain) {
                insert_prop(&mut fields, field.clone(), package);
            }
        }
        Some((index, fields))
    }

    /// Rows for every workspace file, before any service takes its share.
    fn rows(&self, state: &Ownership) -> BTreeMap<PathBuf, BTreeSet<ExternalCallCandidate>> {
        let mut by_file: BTreeMap<PathBuf, BTreeSet<ExternalCallCandidate>> = BTreeMap::new();
        for (file, facts) in self.facts.iter().enumerate() {
            for site in &facts.call_sites {
                let Some(package) = state.resolve_package(file, &site.callee) else {
                    continue;
                };
                by_file.entry(self.files[file].clone()).or_default().insert(
                    ExternalCallCandidate {
                        file: self.files[file].to_string_lossy().to_string(),
                        line: site.line,
                        callee: site.callee.text(),
                        package: package.package,
                        mechanism: CallMechanism::Sdk,
                        import_symbol: package.import_symbol,
                        subpath: package.subpath,
                    },
                );
            }
        }
        by_file
    }

    /// The files a service ships: its own, plus everything they pull in,
    /// transitively.
    ///
    /// A service file the workspace walk did not produce — an `include` root
    /// outside the scan root — seeds nothing, because a file outside the tree
    /// has no facts to contribute.
    fn reachable_from(&self, service_files: &[PathBuf], repo_root: &Path) -> BTreeSet<PathBuf> {
        let file_index: BTreeMap<&Path, usize> = self
            .files
            .iter()
            .enumerate()
            .map(|(idx, path)| (path.as_path(), idx))
            .collect();

        let mut queue: VecDeque<usize> = VecDeque::new();
        let mut seen: BTreeSet<usize> = BTreeSet::new();
        for file in service_files {
            let relative = file.strip_prefix(repo_root).unwrap_or(file);
            if let Some(idx) = file_index.get(relative)
                && seen.insert(*idx)
            {
                queue.push_back(*idx);
            }
        }
        while let Some(idx) = queue.pop_front() {
            for edge in &self.facts[idx].edges {
                if seen.insert(*edge) {
                    queue.push_back(*edge);
                }
            }
        }
        seen.into_iter()
            .map(|idx| self.files[idx].clone())
            .collect()
    }
}

fn record_value(fields: BTreeMap<String, PropValue>) -> Value {
    Value::Owned(Owner::Record(fields))
}

fn insert_prop(fields: &mut BTreeMap<String, PropValue>, name: String, package: PkgRef) {
    match fields.get(&name) {
        Some(PropValue::Pkg(existing)) if *existing == package => {}
        Some(_) => {
            fields.insert(name, PropValue::Conflict);
        }
        None => {
            fields.insert(name, PropValue::Pkg(package));
        }
    }
}

fn record_of(
    file: usize,
    entries: &[(String, Chain)],
    state: &Ownership,
) -> BTreeMap<String, PropValue> {
    let mut fields = BTreeMap::new();
    for (name, chain) in entries {
        if let Some(package) = state.resolve_package(file, chain) {
            insert_prop(&mut fields, name.clone(), package);
        }
    }
    fields
}

/// What calling this function yields.
///
/// Plain returns decide it when any of them resolve; otherwise the properties
/// of returned object literals do. A function that has both is read as
/// returning the value, not the record — a deliberate simplification, since a
/// function that sometimes hands back a client and sometimes hands back a bag
/// holding one is already telling the reader very little.
///
/// A function whose returns are themselves functions yields nothing: a second
/// call level is out of scope, and wrapping would give the lattice unbounded
/// height.
fn return_owner(file: usize, shape: &ReturnShape, state: &Ownership) -> Option<Owner> {
    let mut plain: Option<Value> = None;
    for chain in &shape.returns {
        if let Some(owner) = state.eval(file, chain) {
            plain = Some(match plain {
                None => Value::Owned(owner),
                Some(Value::Owned(existing)) => join_owners(&existing, &owner),
                Some(Value::Conflict) => Value::Conflict,
            });
        }
    }
    if let Some(Value::Owned(owner)) = plain {
        return (!matches!(owner, Owner::FnReturning(_))).then_some(owner);
    }
    let record = record_of(file, &shape.record_returns, state);
    (!record.is_empty()).then_some(Owner::Record(record))
}

// ---------------------------------------------------------------------------
// Parsing and fact collection
// ---------------------------------------------------------------------------

fn syntax_for(file_path: &Path) -> (Syntax, bool) {
    match file_path.extension().and_then(|e| e.to_str()) {
        Some("ts") => (
            Syntax::Typescript(TsSyntax {
                decorators: true,
                ..Default::default()
            }),
            true,
        ),
        Some("tsx") => (
            Syntax::Typescript(TsSyntax {
                tsx: true,
                decorators: true,
                ..Default::default()
            }),
            true,
        ),
        Some("jsx") => (
            Syntax::Es(EsSyntax {
                jsx: true,
                ..Default::default()
            }),
            false,
        ),
        _ => (Syntax::Es(Default::default()), false),
    }
}

fn collect_facts(
    relative: &Path,
    absolute: &Path,
    index: &WorkspaceIndex,
    file_index: &BTreeMap<PathBuf, usize>,
) -> FileFacts {
    let Ok(content) = std::fs::read_to_string(absolute) else {
        return FileFacts::default();
    };
    let (syntax, is_typescript) = syntax_for(absolute);

    // A fresh SourceMap per file keeps byte offsets — and therefore the line
    // lookups below — file-local.
    let source_map: Lrc<SourceMap> = Default::default();
    let source_file =
        source_map.new_source_file(Lrc::new(FileName::Real(absolute.to_path_buf())), content);

    let lexer = Lexer::new(
        syntax,
        Default::default(),
        StringInput::from(&*source_file),
        None,
    );
    let mut parser = Parser::new_from(lexer);
    let Ok(mut module) = parser.parse_module() else {
        // An unparseable file contributes nothing. The scanner already reports
        // parse failures on its own path; this channel stays quiet.
        return FileFacts::default();
    };

    // The resolver stamps syntax contexts, so bindings are compared by
    // (symbol, context) rather than by name. Marks are per-file, which is why
    // every slot is keyed by the file as well as the binding.
    GLOBALS.set(&Globals::new(), || {
        let unresolved_mark = Mark::new();
        let top_level_mark = Mark::new();
        let mut pass = resolver(unresolved_mark, top_level_mark, is_typescript);
        module.visit_mut_with(&mut pass);
    });

    let mut collector = FactCollector {
        resolver: Resolver {
            index,
            file_index,
            from: relative.to_path_buf(),
        },
        source_map,
        facts: FileFacts::default(),
        class_stack: Vec::new(),
        ctor_params: Vec::new(),
    };
    collector.collect_module_declarations(&module);
    module.visit_with(&mut collector);
    collector.facts
}

/// Specifier resolution bound to one file, folded down to the workspace's file
/// indices.
struct Resolver<'a> {
    index: &'a WorkspaceIndex,
    file_index: &'a BTreeMap<PathBuf, usize>,
    from: PathBuf,
}

impl Resolver<'_> {
    /// `None` when the specifier names nothing the scan can use: a builtin, an
    /// asset, or a file the walk excluded (a test tree, build output).
    fn resolve(&self, specifier: &str) -> Option<Target> {
        match self.index.resolve(&self.from, specifier) {
            Resolution::External { package, subpath } => Some(Target::External(package, subpath)),
            Resolution::Internal(path) => self.file_index.get(&path).copied().map(Target::Internal),
            Resolution::Unresolved => None,
        }
    }
}

enum Target {
    /// The declared package name, and the subpath the specifier named under
    /// it.
    External(String, Option<String>),
    Internal(usize),
}

struct FactCollector<'a> {
    resolver: Resolver<'a>,
    source_map: Lrc<SourceMap>,
    facts: FileFacts,
    /// Indices of the classes currently being visited, innermost last.
    class_stack: Vec<usize>,
    /// Constructor parameter bindings of each class on the stack, by position.
    ctor_params: Vec<Vec<Option<BindingId>>>,
}

impl FactCollector<'_> {
    /// Imports, re-exports, and export declarations, which only ever appear at
    /// the top level of a module.
    fn collect_module_declarations(&mut self, module: &Module) {
        for item in &module.body {
            let ModuleItem::ModuleDecl(decl) = item else {
                continue;
            };
            match decl {
                ModuleDecl::Import(import) => self.collect_import(import),
                ModuleDecl::ExportNamed(export) => self.collect_named_export(export),
                ModuleDecl::ExportAll(export) => {
                    if export.type_only {
                        continue;
                    }
                    if let Some(Target::Internal(target)) =
                        self.resolver.resolve(export.src.value.as_ref())
                    {
                        self.facts.edges.insert(target);
                        self.facts.star_exports.insert(target);
                    }
                }
                ModuleDecl::ExportDecl(export) => self.collect_exported_decl(&export.decl),
                ModuleDecl::ExportDefaultDecl(export) => match &export.decl {
                    DefaultDecl::Fn(function) => {
                        if let Some(ident) = &function.ident {
                            let local = BindingId::of(ident);
                            // A default-exported function is a `FnExpr`, not a
                            // `FnDecl`, so the declaration visitor never sees
                            // it and its returns have to be read here.
                            self.collect_function(local.clone(), &function.function);
                            self.export_binding("default", local);
                        }
                    }
                    DefaultDecl::Class(class) => {
                        if let Some(ident) = &class.ident {
                            self.export_binding("default", BindingId::of(ident));
                        }
                    }
                    DefaultDecl::TsInterfaceDecl(_) => {}
                },
                ModuleDecl::ExportDefaultExpr(export) => {
                    // A default export of an expression has no binding of its
                    // own, so it gets a synthetic one and the expression is
                    // recorded against it like any other alias.
                    let local = match unwrap_value(&export.expr) {
                        Expr::Ident(ident) => BindingId::of(ident),
                        other => {
                            let local = BindingId::default_export();
                            if let Some(chain) = self.chain_of(other) {
                                self.facts.aliases.push(AliasFact {
                                    local: local.clone(),
                                    value: chain,
                                });
                            }
                            local
                        }
                    };
                    self.export_binding("default", local);
                }
                _ => {}
            }
        }
    }

    fn collect_import(&mut self, import: &ImportDecl) {
        let Some(target) = self.resolver.resolve(import.src.value.as_ref()) else {
            return;
        };
        // `import type { X } from './x'` ships nothing, so it is not a
        // reachability edge either.
        if let Target::Internal(file) = &target
            && !import.type_only
        {
            self.facts.edges.insert(*file);
        }
        for specifier in &import.specifiers {
            let (local, imported) = match specifier {
                ImportSpecifier::Named(named) => {
                    let imported = match &named.imported {
                        Some(ModuleExportName::Ident(ident)) => ident.sym.to_string(),
                        Some(ModuleExportName::Str(name)) => name.value.to_string(),
                        None => named.local.sym.to_string(),
                    };
                    (&named.local, Some(imported))
                }
                ImportSpecifier::Default(default) => (&default.local, Some("default".to_string())),
                ImportSpecifier::Namespace(namespace) => (&namespace.local, None),
            };

            // A name imported from a dependency is what a type annotation
            // writing that name resolves to, and a type-only import is exactly
            // as good a witness of that as a value import. It still binds no
            // value: only the annotation positions below read this map.
            if let Target::External(package, subpath) = &target {
                self.facts.type_names.insert(
                    local.sym.to_string(),
                    PkgRef {
                        package: package.clone(),
                        import_symbol: imported.clone(),
                        subpath: subpath.clone(),
                    },
                );
            }

            // `import type ...` and the inline `import { type X }` are erased,
            // so neither binds a value.
            let type_only = import.type_only
                || matches!(specifier, ImportSpecifier::Named(named) if named.is_type_only);
            if type_only {
                continue;
            }

            let source = match (&target, imported) {
                (Target::External(package, subpath), imported) => Source::ExternalPackage(PkgRef {
                    package: package.clone(),
                    import_symbol: imported,
                    subpath: subpath.clone(),
                }),
                (Target::Internal(file), Some(name)) => Source::InternalNamed(*file, name),
                (Target::Internal(file), None) => Source::InternalNamespace(*file),
            };
            self.facts.imports.push(ImportFact {
                local: BindingId::of(local),
                source,
            });
        }
    }

    /// The dependency a type annotation names, when the file's own imports say
    /// which package the name came from.
    ///
    /// Only a bare type reference is read — `Forge`, or `ns.Forge` through a
    /// namespace import. A generic wrapper (`Promise<Forge>`), a union, and an
    /// inline object type all name nothing this can attribute, so they resolve
    /// to nothing.
    fn annotation_package(&self, annotation: Option<&TsTypeAnn>) -> Option<PkgRef> {
        let TsType::TsTypeRef(reference) = &*annotation?.type_ann else {
            return None;
        };
        let root = leftmost_type_ident(&reference.type_name);
        self.facts.type_names.get(root.sym.as_ref()).cloned()
    }

    fn collect_named_export(&mut self, export: &NamedExport) {
        if export.type_only {
            return;
        }
        let target = export
            .src
            .as_ref()
            .and_then(|src| self.resolver.resolve(src.value.as_ref()));
        if let Some(Target::Internal(file)) = &target {
            self.facts.edges.insert(*file);
        }
        for specifier in &export.specifiers {
            match specifier {
                ExportSpecifier::Named(named) if named.is_type_only => {}
                ExportSpecifier::Named(named) => {
                    let ModuleExportName::Ident(local) = &named.orig else {
                        continue;
                    };
                    let exported = match &named.exported {
                        Some(ModuleExportName::Ident(ident)) => ident.sym.to_string(),
                        Some(ModuleExportName::Str(name)) => name.value.to_string(),
                        None => local.sym.to_string(),
                    };
                    match &target {
                        Some(Target::External(package, subpath)) => {
                            self.facts.reexports.push(ReExportFact {
                                name: exported,
                                source: Source::ExternalPackage(PkgRef {
                                    package: package.clone(),
                                    import_symbol: Some(local.sym.to_string()),
                                    subpath: subpath.clone(),
                                }),
                            });
                        }
                        Some(Target::Internal(file)) => {
                            self.facts.reexports.push(ReExportFact {
                                name: exported,
                                source: Source::InternalNamed(*file, local.sym.to_string()),
                            });
                        }
                        // `export { x }` with no source re-exports a local
                        // binding of this file.
                        None => self.export_binding(&exported, BindingId::of(local)),
                    }
                }
                ExportSpecifier::Namespace(namespace) => {
                    let ModuleExportName::Ident(ident) = &namespace.name else {
                        continue;
                    };
                    let source = match &target {
                        // `export * as ns from 'pkg'` re-exports the module
                        // itself, so it names no one export of it.
                        Some(Target::External(package, subpath)) => {
                            Source::ExternalPackage(PkgRef {
                                package: package.clone(),
                                import_symbol: None,
                                subpath: subpath.clone(),
                            })
                        }
                        Some(Target::Internal(file)) => Source::InternalNamespace(*file),
                        None => continue,
                    };
                    self.facts.reexports.push(ReExportFact {
                        name: ident.sym.to_string(),
                        source,
                    });
                }
                ExportSpecifier::Default(_) => {}
            }
        }
    }

    fn collect_exported_decl(&mut self, decl: &Decl) {
        match decl {
            Decl::Var(var) => {
                for declarator in &var.decls {
                    if let Pat::Ident(name) = &declarator.name {
                        self.export_binding(name.id.sym.as_ref(), BindingId::of_binding(name));
                    }
                }
            }
            Decl::Fn(function) => {
                self.export_binding(function.ident.sym.as_ref(), BindingId::of(&function.ident));
            }
            Decl::Class(class) => {
                self.export_binding(class.ident.sym.as_ref(), BindingId::of(&class.ident));
            }
            _ => {}
        }
    }

    fn export_binding(&mut self, name: &str, local: BindingId) {
        self.facts.exports.push(ExportFact {
            name: name.to_string(),
            local,
        });
    }

    /// Reduce an expression to a chain, or `None` when some part of it is not
    /// statically nameable (computed access, a literal receiver, a spread).
    ///
    /// `await`, parentheses, and TypeScript's value-preserving casts are
    /// transparent: they change the type of the expression, never where the
    /// value came from.
    fn chain_of(&self, expr: &Expr) -> Option<Chain> {
        match unwrap_value(expr) {
            Expr::Ident(ident) => Some(Chain {
                root: Root::Binding(BindingId::of(ident)),
                steps: Vec::new(),
            }),
            Expr::This(_) => self.class_stack.last().map(|class| Chain {
                root: Root::This(*class),
                steps: Vec::new(),
            }),
            Expr::Member(member) => {
                let MemberProp::Ident(prop) = &member.prop else {
                    return None;
                };
                Some(
                    self.chain_of(&member.obj)?
                        .extended(Step::Prop(prop.sym.to_string())),
                )
            }
            Expr::Call(call) => match &call.callee {
                Callee::Expr(callee) => Some(self.chain_of(callee)?.extended(Step::Call)),
                _ => None,
            },
            Expr::New(new_expr) => Some(self.chain_of(&new_expr.callee)?.extended(Step::Call)),
            Expr::OptChain(opt_chain) => match &*opt_chain.base {
                OptChainBase::Member(member) => {
                    let MemberProp::Ident(prop) = &member.prop else {
                        return None;
                    };
                    Some(
                        self.chain_of(&member.obj)?
                            .extended(Step::Prop(prop.sym.to_string())),
                    )
                }
                OptChainBase::Call(call) => Some(self.chain_of(&call.callee)?.extended(Step::Call)),
            },
            _ => None,
        }
    }

    fn record_call_site(&mut self, callee: &Expr, span: swc_common::Span) {
        if let Some(chain) = self.chain_of(callee) {
            self.facts.call_sites.push(CallSite {
                line: self.source_map.lookup_char_pos(span.lo).line,
                callee: chain,
            });
        }
    }

    /// `import('<literal>')`, whatever position it appears in. Returns the
    /// resolved target and records the reachability edge, because a dynamic
    /// import ships its target with the importer exactly as a static one does.
    fn dynamic_import_target(&mut self, expr: &Expr) -> Option<Target> {
        let Expr::Call(call) = unwrap_value(expr) else {
            return None;
        };
        self.dynamic_import_of(call)
    }

    fn dynamic_import_of(&mut self, call: &CallExpr) -> Option<Target> {
        if !matches!(call.callee, Callee::Import(_)) {
            return None;
        }
        let specifier = call.args.first().and_then(|arg| match &*arg.expr {
            Expr::Lit(Lit::Str(literal)) => Some(literal.value.to_string()),
            _ => None,
        })?;
        let target = self.resolver.resolve(&specifier)?;
        if let Target::Internal(file) = &target {
            self.facts.edges.insert(*file);
        }
        Some(target)
    }

    /// The bindings a variable declarator introduces, given the value it is
    /// initialized from.
    fn collect_declarator(&mut self, declarator: &VarDeclarator) {
        let Some(init) = &declarator.init else {
            return;
        };

        // `const { x } = await import('spec')` / `const m = await import('spec')`
        // bind names of another module, so they are import facts rather than
        // aliases.
        if let Some(target) = self.dynamic_import_target(init) {
            self.collect_dynamic_import_binding(&declarator.name, &target);
            return;
        }

        if let Pat::Ident(name) = &declarator.name {
            let local = BindingId::of_binding(name);

            // `const C = class { ... }` — the class's field maps belong to this
            // binding.
            if let Expr::Class(class) = unwrap_value(init) {
                self.collect_class(&class.class, Some(local));
                return;
            }

            // A function bound to a name: its returns decide what calling it
            // yields.
            match unwrap_value(init) {
                Expr::Arrow(arrow) => self.collect_arrow(local.clone(), arrow),
                Expr::Fn(function) => self.collect_function(local.clone(), &function.function),
                _ => {}
            }

            // `const c = new LocalClass(...)` needs no fact of its own: the
            // alias below reduces it to the class binding plus a call, and a
            // call on a class evaluates to that class's instance field map.

            if let Some(chain) = self.chain_of(init) {
                self.facts.aliases.push(AliasFact {
                    local,
                    value: chain,
                });
            }
            return;
        }

        // `const { transport } = await getContext()` is the same chain as
        // `getContext().transport`, one step longer.
        if let Pat::Object(pattern) = &declarator.name
            && let Some(chain) = self.chain_of(init)
        {
            for (property, local) in destructured_properties(pattern) {
                self.facts.aliases.push(AliasFact {
                    local,
                    value: chain.extended(Step::Prop(property)),
                });
            }
        }
    }

    fn collect_dynamic_import_binding(&mut self, pattern: &Pat, target: &Target) {
        match pattern {
            // `const mod = await import('pkg')` binds the module, not one of
            // its exports, exactly as a namespace import does.
            Pat::Ident(name) => {
                let source = match target {
                    Target::External(package, subpath) => Source::ExternalPackage(PkgRef {
                        package: package.clone(),
                        import_symbol: None,
                        subpath: subpath.clone(),
                    }),
                    Target::Internal(file) => Source::InternalNamespace(*file),
                };
                self.facts.imports.push(ImportFact {
                    local: BindingId::of_binding(name),
                    source,
                });
            }
            // `const { default: C } = await import('pkg')` names an export,
            // and the key it is read under is that export's name.
            Pat::Object(object) => {
                for (property, local) in destructured_properties(object) {
                    let source = match target {
                        Target::External(package, subpath) => Source::ExternalPackage(PkgRef {
                            package: package.clone(),
                            import_symbol: Some(property),
                            subpath: subpath.clone(),
                        }),
                        Target::Internal(file) => Source::InternalNamed(*file, property),
                    };
                    self.facts.imports.push(ImportFact { local, source });
                }
            }
            _ => {}
        }
    }

    fn collect_arrow(&mut self, local: BindingId, arrow: &ArrowExpr) {
        let shape = self.arrow_shape(arrow);
        let annotation = self.annotation_package(arrow.return_type.as_deref());
        self.push_function(local, shape, annotation);
    }

    fn arrow_shape(&mut self, arrow: &ArrowExpr) -> ReturnShape {
        let mut shape = ReturnShape::default();
        match &*arrow.body {
            BlockStmtOrExpr::BlockStmt(body) => self.collect_returns(Some(body), &mut shape),
            BlockStmtOrExpr::Expr(expr) => self.collect_return_value(expr, &mut shape),
        }
        shape
    }

    fn collect_function(&mut self, local: BindingId, function: &Function) {
        let shape = self.body_shape(function.body.as_ref());
        let annotation = self.annotation_package(function.return_type.as_deref());
        self.push_function(local, shape, annotation);
    }

    fn body_shape(&mut self, body: Option<&BlockStmt>) -> ReturnShape {
        let mut shape = ReturnShape::default();
        self.collect_returns(body, &mut shape);
        shape
    }

    fn push_function(&mut self, local: BindingId, shape: ReturnShape, annotation: Option<PkgRef>) {
        if shape.is_empty() && annotation.is_none() {
            return;
        }
        self.facts.functions.push(FunctionFact {
            local,
            shape,
            annotation,
        });
    }

    fn collect_returns(&mut self, body: Option<&BlockStmt>, shape: &mut ReturnShape) {
        let Some(body) = body else {
            return;
        };
        let mut collector = ReturnCollector {
            returned: Vec::new(),
        };
        body.visit_with(&mut collector);
        for expr in collector.returned {
            self.collect_return_value(&expr, shape);
        }
    }

    fn collect_return_value(&mut self, expr: &Expr, shape: &mut ReturnShape) {
        // A function that picks between two object literals contributes both,
        // so the branches are taken apart before the shape of any one of them
        // is looked at.
        for branch in value_branches(expr) {
            let Expr::Object(object) = branch else {
                if let Some(chain) = self.chain_of(branch) {
                    shape.returns.push(chain);
                }
                continue;
            };
            for (name, chain) in self.object_properties(object) {
                shape.record_returns.push((name, chain));
            }
        }
    }

    /// `{ a, b: c.d }` reduced to the property names and the chains behind them.
    fn object_properties(&self, object: &ObjectLit) -> Vec<(String, Chain)> {
        let mut properties = Vec::new();
        for property in &object.props {
            let PropOrSpread::Prop(prop) = property else {
                continue;
            };
            match &**prop {
                // `{ transport }` is `{ transport: transport }`, so the property
                // is owned by whatever the binding is.
                Prop::Shorthand(ident) => properties.push((
                    ident.sym.to_string(),
                    Chain {
                        root: Root::Binding(BindingId::of(ident)),
                        steps: Vec::new(),
                    },
                )),
                Prop::KeyValue(entry) => {
                    let Some(name) = property_name(&entry.key) else {
                        continue;
                    };
                    for value in value_branches(&entry.value) {
                        if let Some(chain) = self.chain_of(value) {
                            properties.push((name.clone(), chain));
                        }
                    }
                }
                _ => continue,
            }
        }
        properties
    }

    /// A class's field maps, its static members, and the constructor parameters
    /// its body may assign from. Visits the body itself so nested classes and
    /// `this` receivers see the right enclosing class.
    fn collect_class(&mut self, class: &Class, binding: Option<BindingId>) {
        let index = self.facts.classes.len();
        self.facts.classes.push(ClassFact {
            binding,
            instance_fields: Vec::new(),
            annotation_fields: Vec::new(),
            static_fields: Vec::new(),
            static_functions: Vec::new(),
            ctor_param_fields: Vec::new(),
        });

        let mut parameters: Vec<Option<BindingId>> = Vec::new();
        for member in &class.body {
            if let ClassMember::Constructor(constructor) = member {
                parameters = constructor
                    .params
                    .iter()
                    .map(|param| match param {
                        ParamOrTsParamProp::Param(Param {
                            pat: Pat::Ident(name),
                            ..
                        }) => Some(BindingId::of_binding(name)),
                        _ => None,
                    })
                    .collect();
            }
        }

        self.class_stack.push(index);
        self.ctor_params.push(parameters);

        for member in &class.body {
            match member {
                ClassMember::ClassProp(property) => {
                    let Some(name) = property_name(&property.key) else {
                        continue;
                    };
                    // Read before the initializer, because the shape this
                    // exists for has a declared type and no initializer at
                    // all. Something outside the class body fills it in.
                    if !property.is_static
                        && let Some(package) = self.annotation_package(property.type_ann.as_deref())
                    {
                        self.facts.classes[index]
                            .annotation_fields
                            .push((name.clone(), package));
                    }
                    let Some(value) = &property.value else {
                        continue;
                    };
                    let Some(chain) = self.chain_of(value) else {
                        continue;
                    };
                    if property.is_static {
                        self.facts.classes[index].static_fields.push((name, chain));
                    } else {
                        self.facts.classes[index]
                            .instance_fields
                            .push((name, chain));
                    }
                }
                ClassMember::Method(method) if method.is_static => {
                    let Some(name) = property_name(&method.key) else {
                        continue;
                    };
                    let is_getter = match method.kind {
                        MethodKind::Method => false,
                        MethodKind::Getter => true,
                        MethodKind::Setter => continue,
                    };
                    let shape = self.body_shape(method.function.body.as_ref());
                    if shape.is_empty() {
                        continue;
                    }
                    self.facts.classes[index]
                        .static_functions
                        .push(StaticFunction {
                            name,
                            shape,
                            is_getter,
                        });
                }
                _ => {}
            }
        }
        class.visit_children_with(self);

        self.class_stack.pop();
        self.ctor_params.pop();
    }

    /// `<receiver>.<field> = <expression>`, in the two forms ownership reads:
    /// `this.<field>` inside a class body, and `<identifier>.<field>` anywhere,
    /// which is how a class's static field gets written.
    fn collect_assignment(&mut self, assign: &AssignExpr) {
        let AssignTarget::Simple(SimpleAssignTarget::Member(member)) = &assign.left else {
            return;
        };
        let MemberProp::Ident(field) = &member.prop else {
            return;
        };
        let field = field.sym.to_string();

        if let Expr::Ident(receiver) = unwrap_value(&member.obj) {
            // Kept whatever the receiver turns out to be; the fold drops every
            // one whose identifier is not a class declared in this file.
            if let Some(chain) = self.chain_of(&assign.right) {
                self.facts
                    .member_assignments
                    .push((BindingId::of(receiver), field, chain));
            }
            return;
        }

        if !matches!(unwrap_value(&member.obj), Expr::This(_)) {
            return;
        }
        let Some(class) = self.class_stack.last().copied() else {
            return;
        };

        // A field assigned from a constructor parameter — the parameter itself,
        // or a property read off it, which is how an options bag is unpacked —
        // owns whatever the construction sites passed in.
        if let Some(chain) = self.chain_of(&assign.right)
            && let Root::Binding(assigned) = &chain.root
            && let Some(position) = self
                .ctor_params
                .last()
                .and_then(|params| params.iter().position(|p| p.as_ref() == Some(assigned)))
        {
            let mut path = Vec::new();
            for step in &chain.steps {
                match step {
                    Step::Prop(name) => path.push(name.clone()),
                    // A call on the parameter is a value the site cannot supply.
                    Step::Call => return,
                }
            }
            self.facts.classes[class]
                .ctor_param_fields
                .push((field, position, path));
            return;
        }

        if let Some(chain) = self.chain_of(&assign.right) {
            self.facts.classes[class]
                .instance_fields
                .push((field, chain));
        }
    }

    /// Every `new <identifier>(...)`, wherever it sits. A construction inside an
    /// assignment or an argument list is as good a witness of what the
    /// constructor received as one in a declarator.
    fn collect_construction(&mut self, new_expr: &NewExpr) {
        let Expr::Ident(class) = unwrap_value(&new_expr.callee) else {
            return;
        };
        let arguments = new_expr
            .args
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|argument| {
                if argument.spread.is_some() {
                    // A spread shifts every later position, so nothing after it
                    // can be matched to a parameter by index either.
                    return Argument::Opaque;
                }
                if let Expr::Object(object) = unwrap_value(&argument.expr) {
                    return Argument::Record(self.object_properties(object).into_iter().collect());
                }
                match self.chain_of(&argument.expr) {
                    Some(chain) => Argument::Value(chain),
                    None => Argument::Opaque,
                }
            })
            .collect();
        self.facts.constructions.push(ConstructionFact {
            class_binding: BindingId::of(class),
            arguments,
        });
    }
}

impl Visit for FactCollector<'_> {
    fn visit_var_declarator(&mut self, declarator: &VarDeclarator) {
        self.collect_declarator(declarator);
        // A class initializer has already been visited by `collect_class`.
        if let Some(init) = &declarator.init
            && matches!(unwrap_value(init), Expr::Class(_))
        {
            return;
        }
        declarator.visit_children_with(self);
    }

    fn visit_fn_decl(&mut self, function: &FnDecl) {
        self.collect_function(BindingId::of(&function.ident), &function.function);
        function.visit_children_with(self);
    }

    fn visit_class_decl(&mut self, class: &ClassDecl) {
        self.collect_class(&class.class, Some(BindingId::of(&class.ident)));
    }

    fn visit_class_expr(&mut self, class: &ClassExpr) {
        self.collect_class(&class.class, class.ident.as_ref().map(BindingId::of));
    }

    fn visit_assign_expr(&mut self, assign: &AssignExpr) {
        self.collect_assignment(assign);
        assign.visit_children_with(self);
    }

    fn visit_new_expr(&mut self, new_expr: &NewExpr) {
        self.collect_construction(new_expr);
        new_expr.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        if let Callee::Expr(callee) = &call.callee {
            self.record_call_site(callee, call.span);
        } else {
            // A dynamic import anywhere in the file is still an edge, whether
            // or not anything is bound from it.
            self.dynamic_import_of(call);
        }
        call.visit_children_with(self);
    }

    fn visit_opt_chain_expr(&mut self, opt_chain: &OptChainExpr) {
        // `client?.send(...)` is an optional call, not a `CallExpr`, so it
        // needs its own arm or the row is silently lost.
        if let OptChainBase::Call(call) = &*opt_chain.base {
            let callee = call.callee.clone();
            self.record_call_site(&callee, opt_chain.span);
        }
        opt_chain.visit_children_with(self);
    }
}

/// Every `return <expr>` of one function body, stopping at nested function
/// boundaries so an inner function's returns are not attributed to the outer
/// one.
struct ReturnCollector {
    returned: Vec<Expr>,
}

impl Visit for ReturnCollector {
    fn visit_return_stmt(&mut self, statement: &ReturnStmt) {
        if let Some(argument) = &statement.arg {
            self.returned.push((**argument).clone());
        }
        statement.visit_children_with(self);
    }

    fn visit_function(&mut self, _: &Function) {}
    fn visit_arrow_expr(&mut self, _: &ArrowExpr) {}
    fn visit_class(&mut self, _: &Class) {}
}

/// Strip the wrappers that change an expression's type without changing where
/// its value came from.
fn unwrap_value(expr: &Expr) -> &Expr {
    match expr {
        Expr::Await(inner) => unwrap_value(&inner.arg),
        Expr::Paren(inner) => unwrap_value(&inner.expr),
        Expr::TsNonNull(inner) => unwrap_value(&inner.expr),
        Expr::TsAs(inner) => unwrap_value(&inner.expr),
        Expr::TsSatisfies(inner) => unwrap_value(&inner.expr),
        other => other,
    }
}

/// The alternatives a value expression can take at runtime. A ternary and the
/// short-circuiting operators each pick one branch, and any of them may be the
/// one that carries the client.
fn value_branches(expr: &Expr) -> Vec<&Expr> {
    match unwrap_value(expr) {
        Expr::Cond(cond) => {
            let mut branches = value_branches(&cond.cons);
            branches.extend(value_branches(&cond.alt));
            branches
        }
        Expr::Bin(binary)
            if matches!(binary.op, BinaryOp::NullishCoalescing | BinaryOp::LogicalOr) =>
        {
            let mut branches = value_branches(&binary.left);
            branches.extend(value_branches(&binary.right));
            branches
        }
        other => vec![other],
    }
}

/// `{ a, b: c }` — the property read and the binding it lands in. Rest elements
/// and computed keys carry nothing nameable and are skipped.
fn destructured_properties(pattern: &ObjectPat) -> Vec<(String, BindingId)> {
    let mut bound = Vec::new();
    for property in &pattern.props {
        match property {
            ObjectPatProp::Assign(entry) => {
                bound.push((
                    entry.key.id.sym.to_string(),
                    BindingId::of_binding(&entry.key),
                ));
            }
            ObjectPatProp::KeyValue(entry) => {
                if let (Some(name), Pat::Ident(local)) = (property_name(&entry.key), &*entry.value)
                {
                    bound.push((name, BindingId::of_binding(local)));
                }
            }
            ObjectPatProp::Rest(_) => {}
        }
    }
    bound
}

/// The name a type reference starts from: `Forge` in `Forge`, and `ns` in
/// `ns.Forge`, which is the name a namespace import binds.
fn leftmost_type_ident(name: &TsEntityName) -> &Ident {
    match name {
        TsEntityName::Ident(ident) => ident,
        TsEntityName::TsQualifiedName(qualified) => leftmost_type_ident(&qualified.left),
    }
}

fn property_name(key: &PropName) -> Option<String> {
    match key {
        PropName::Ident(ident) => Some(ident.sym.to_string()),
        PropName::Str(name) => Some(name.value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::file_finder::find_service_files;

    const IGNORE_PATTERNS: &[&str] = &["node_modules", "dist", "build", ".next"];

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/external-call-candidates")
    }

    /// Run the scan exactly as the engine does: the service's file list comes
    /// from `find_service_files`, so the scanner's own test-file and
    /// artifact-directory exclusions are what filter the seeds, and the
    /// external universe comes from the repo's own manifests.
    fn scan(directory: &str) -> Vec<ExternalCallCandidate> {
        let root = fixture_root();
        let service = Config {
            directory: Some(directory.to_string()),
            ..Default::default()
        };
        let (files, _) = find_service_files(&root.to_string_lossy(), &service, IGNORE_PATTERNS);
        scan_workspace(&files, &root)
    }

    fn rows_for(rows: &[ExternalCallCandidate], file: &str) -> Vec<(usize, String, String)> {
        rows.iter()
            .filter(|row| row.file == file)
            .map(|row| (row.line, row.callee.clone(), row.package.clone()))
            .collect()
    }

    fn triples(rows: &[ExternalCallCandidate]) -> Vec<(String, usize, String, String)> {
        rows.iter()
            .map(|row| {
                (
                    row.file.clone(),
                    row.line,
                    row.callee.clone(),
                    row.package.clone(),
                )
            })
            .collect()
    }

    /// One row reduced to what the anchor tests read: the sort key, then the
    /// export and the subpath the receiver came through.
    type AnchoredRow = (
        String,
        usize,
        String,
        String,
        Option<String>,
        Option<String>,
    );

    /// The same without the file, for the assertions scoped to one file.
    type FileAnchor = (usize, String, String, Option<String>, Option<String>);

    /// The row plus the two anchors, for the tests that are about which import
    /// a receiver came through rather than only which package.
    fn anchored(rows: &[ExternalCallCandidate]) -> Vec<AnchoredRow> {
        rows.iter()
            .map(|row| {
                (
                    row.file.clone(),
                    row.line,
                    row.callee.clone(),
                    row.package.clone(),
                    row.import_symbol.clone(),
                    row.subpath.clone(),
                )
            })
            .collect()
    }

    /// The literal form an expected row is written in.
    type AnchorLiteral<'a> = (
        &'a str,
        usize,
        &'a str,
        &'a str,
        Option<&'a str>,
        Option<&'a str>,
    );

    fn expected_anchored(rows: &[AnchorLiteral<'_>]) -> Vec<AnchoredRow> {
        rows.iter()
            .map(|(file, line, callee, package, symbol, subpath)| {
                (
                    file.to_string(),
                    *line,
                    callee.to_string(),
                    package.to_string(),
                    symbol.map(str::to_string),
                    subpath.map(str::to_string),
                )
            })
            .collect()
    }

    fn expected(rows: &[(&str, usize, &str, &str)]) -> Vec<(String, usize, String, String)> {
        rows.iter()
            .map(|(file, line, callee, package)| {
                (
                    file.to_string(),
                    *line,
                    callee.to_string(),
                    package.to_string(),
                )
            })
            .collect()
    }

    /// The single-file service from carrick#511, unchanged. Widening the
    /// dependency universe and the file set must not move a row that the
    /// direct-import base case already produced.
    mod direct_imports {
        use super::*;

        #[test]
        fn fixture_scan_matches_expected_rows() {
            let rows = scan("apps/api");
            assert_eq!(
                triples(&rows),
                expected(&[
                    (
                        "apps/api/src/direct-call.ts",
                        4,
                        "sendNotice",
                        "courier-sdk"
                    ),
                    (
                        "apps/api/src/member-call.ts",
                        6,
                        "ledger.payments.create",
                        "ledger-client"
                    ),
                    (
                        "apps/api/src/member-call.ts",
                        10,
                        "createInvoice",
                        "ledger-client"
                    ),
                    (
                        "apps/api/src/namespace-call.ts",
                        3,
                        "telemetry.createSink",
                        "telemetry-sink"
                    ),
                    (
                        "apps/api/src/namespace-call.ts",
                        6,
                        "telemetry.emit",
                        "telemetry-sink"
                    ),
                    (
                        "apps/api/src/namespace-call.ts",
                        7,
                        "sink.flush",
                        "telemetry-sink"
                    ),
                    (
                        "apps/api/src/optional-dep.ts",
                        6,
                        "uplink.put",
                        "storage-uplink"
                    ),
                    (
                        "apps/api/src/subpath-import.ts",
                        4,
                        "publishEdge",
                        "courier-sdk"
                    ),
                ])
            );
            assert!(rows.iter().all(|row| row.mechanism == CallMechanism::Sdk));
        }

        /// `peerDependencies` and `optionalDependencies` count as runtime
        /// dependencies; `telemetry-sink` is a peer, `storage-uplink` optional.
        #[test]
        fn peer_and_optional_dependencies_resolve() {
            let rows = scan("apps/api");
            assert!(rows.iter().any(|row| row.package == "telemetry-sink"));
            assert_eq!(
                rows_for(&rows, "apps/api/src/optional-dep.ts"),
                vec![(6, "uplink.put".to_string(), "storage-uplink".to_string())]
            );
        }

        /// Relative imports, Node builtins (bare and `node:`-prefixed), and a
        /// workspace-internal package all fail the structural rule.
        #[test]
        fn relative_builtin_and_workspace_imports_emit_nothing() {
            assert_eq!(
                rows_for(&scan("apps/api"), "apps/api/src/no-rows.ts"),
                Vec::new()
            );
        }

        /// A type-only declaration binds nothing, while the value specifier in
        /// a mixed declaration still does.
        /// A subpath import reports the package it belongs to, and now also
        /// which entry point of it the value came through.
        #[test]
        fn a_subpath_import_records_its_subpath() {
            let rows = scan("apps/api");
            assert_eq!(
                anchored(&rows)
                    .into_iter()
                    .filter(|(file, ..)| file == "apps/api/src/subpath-import.ts")
                    .collect::<Vec<_>>(),
                expected_anchored(&[(
                    "apps/api/src/subpath-import.ts",
                    4,
                    "publishEdge",
                    "courier-sdk",
                    Some("publishEdge"),
                    Some("edge"),
                )])
            );
        }

        #[test]
        fn type_only_imports_emit_nothing() {
            let rows = scan("apps/api");
            assert_eq!(rows_for(&rows, "apps/api/src/type-only.ts"), Vec::new());
            assert!(
                rows_for(&rows, "apps/api/src/member-call.ts").contains(&(
                    10,
                    "createInvoice".to_string(),
                    "ledger-client".to_string()
                )),
                "the value half of a mixed type/value import still resolves"
            );
        }

        /// Test trees and build-artifact directories are excluded by the
        /// scanner's existing walk rules, not by anything this module defines.
        #[test]
        fn test_and_artifact_files_are_never_scanned() {
            let rows = scan("apps/api");
            assert!(
                rows.iter().all(|row| !row.file.contains("__tests__")
                    && !row.file.contains("/dist/")
                    && !row.file.ends_with(".test.ts")),
                "excluded trees leaked into the rows: {:?}",
                rows
            );
        }
    }

    /// The workspace service: every call site it ships is written in a package
    /// that is not the service, and every wrapper shape in between is one the
    /// ownership pass has to walk.
    mod workspace_transitive {
        use super::*;

        fn worker() -> Vec<ExternalCallCandidate> {
            scan("apps/worker")
        }

        #[test]
        fn workspace_scan_matches_expected_rows() {
            assert_eq!(
                triples(&worker()),
                expected(&[
                    (
                        "apps/worker/src/barrel-consumer.ts",
                        3,
                        "ledger.invoices.list",
                        "ledger-client"
                    ),
                    (
                        "apps/worker/src/billing.ts",
                        4,
                        "ledger.payments.create",
                        "ledger-client"
                    ),
                    (
                        "apps/worker/src/conflict.ts",
                        5,
                        "soloClient.upload",
                        "vault-blob"
                    ),
                    (
                        "apps/worker/src/destructured.ts",
                        6,
                        "transport.sendMail",
                        "postbox-mailer"
                    ),
                    (
                        "apps/worker/src/digest.ts",
                        6,
                        "transport.sendMail",
                        "postbox-mailer"
                    ),
                    (
                        "apps/worker/src/member-form.ts",
                        6,
                        "context.transport.sendMail",
                        "postbox-mailer"
                    ),
                    (
                        "apps/worker/src/namespace-internal.ts",
                        4,
                        "ledgerKit.ledger.payments.settle",
                        "ledger-client"
                    ),
                    (
                        "apps/worker/src/notify.ts",
                        4,
                        "mailer.sendMail",
                        "postbox-mailer"
                    ),
                    (
                        "apps/worker/src/shadow.ts",
                        9,
                        "mailer.sendMail",
                        "postbox-mailer"
                    ),
                    (
                        "apps/worker/src/split-context.ts",
                        6,
                        "transport.sendMail",
                        "postbox-mailer"
                    ),
                    (
                        "packages/crawl-kit/render.ts",
                        3,
                        "headless.launch",
                        "crawler-kit"
                    ),
                    (
                        "packages/crawl-kit/render.ts",
                        6,
                        "runtime.shutdown",
                        "crawler-kit"
                    ),
                    ("packages/doc-kit/sign.ts", 4, "Pdf.load", "pdf-toolkit"),
                    ("packages/doc-kit/sign.ts", 6, "doc.sign", "pdf-toolkit"),
                    (
                        "packages/job-kit/handler.ts",
                        4,
                        "mailer.sendMail",
                        "postbox-mailer"
                    ),
                    (
                        "packages/jobs-kit/provider.ts",
                        19,
                        "this._client.send",
                        "relay-queue"
                    ),
                    (
                        "packages/mail-kit/digest.ts",
                        4,
                        "createTransport",
                        "postbox-mailer"
                    ),
                    (
                        "packages/mail-kit/index.ts",
                        9,
                        "getTransport().verify",
                        "postbox-mailer"
                    ),
                    (
                        "packages/mail-kit/transport.ts",
                        5,
                        "createTransport",
                        "postbox-mailer"
                    ),
                    (
                        "packages/mail-kit/transport.ts",
                        8,
                        "createTransport",
                        "postbox-mailer"
                    ),
                    (
                        "packages/singleton-kit/index.ts",
                        22,
                        "reporter.client.shutdown",
                        "pulse-analytics"
                    ),
                    (
                        "packages/singleton-kit/index.ts",
                        26,
                        "PulseReporter.current.client.capture",
                        "pulse-analytics"
                    ),
                    (
                        "packages/storage-kit/beacon.ts",
                        8,
                        "Beacon.client.capture",
                        "beacon-metrics"
                    ),
                    (
                        "packages/storage-kit/blob-store.ts",
                        11,
                        "this.client.upload",
                        "vault-blob"
                    ),
                    (
                        "packages/storage-kit/queue-store.ts",
                        13,
                        "store.client.flush",
                        "vault-blob"
                    ),
                ])
            );
        }

        /// A client constructed once in a shared package and consumed through a
        /// subpath import of that package.
        #[test]
        fn exported_client_resolves_across_a_subpath_import() {
            assert_eq!(
                rows_for(&worker(), "apps/worker/src/billing.ts"),
                vec![(
                    4,
                    "ledger.payments.create".to_string(),
                    "ledger-client".to_string()
                )]
            );
        }

        /// A local factory whose every return constructs the client, consumed
        /// through the module-level handle it produced.
        #[test]
        fn local_factory_result_is_owned_by_what_its_returns_construct() {
            assert_eq!(
                rows_for(&worker(), "apps/worker/src/notify.ts"),
                vec![(
                    4,
                    "mailer.sendMail".to_string(),
                    "postbox-mailer".to_string()
                )]
            );
        }

        /// A call in the middle of the callee chain resolves through the
        /// function it calls, and the row carries the outer call's line.
        #[test]
        fn a_call_inside_the_chain_resolves_and_prints_as_written() {
            assert_eq!(
                rows_for(&worker(), "packages/mail-kit/index.ts"),
                vec![(
                    9,
                    "getTransport().verify".to_string(),
                    "postbox-mailer".to_string()
                )]
            );
        }

        /// A property of a returned object literal, reached both by
        /// destructuring the call and by a member access on its result. One
        /// branch is plain, the other a ternary.
        #[test]
        fn returned_object_properties_resolve_both_ways() {
            let rows = worker();
            assert_eq!(
                rows_for(&rows, "apps/worker/src/destructured.ts"),
                vec![(
                    6,
                    "transport.sendMail".to_string(),
                    "postbox-mailer".to_string()
                )]
            );
            assert_eq!(
                rows_for(&rows, "apps/worker/src/member-form.ts"),
                vec![(
                    6,
                    "context.transport.sendMail".to_string(),
                    "postbox-mailer".to_string()
                )]
            );
        }

        /// A default-exported function declaration is a function expression in
        /// the AST, so its returns need reading where the export is read or the
        /// factory shape is silently uncovered.
        #[test]
        fn a_default_exported_factory_resolves() {
            assert_eq!(
                rows_for(&worker(), "apps/worker/src/digest.ts"),
                vec![(
                    6,
                    "transport.sendMail".to_string(),
                    "postbox-mailer".to_string()
                )]
            );
        }

        /// A function that picks between two object literals owns the property
        /// through both of them, so a return written as a ternary of records is
        /// not a blind spot.
        #[test]
        fn a_ternary_of_returned_records_resolves() {
            assert_eq!(
                rows_for(&worker(), "apps/worker/src/split-context.ts"),
                vec![(
                    6,
                    "transport.sendMail".to_string(),
                    "postbox-mailer".to_string()
                )]
            );
        }

        /// A field assigned in the constructor makes `this.<field>` a nameable
        /// receiver.
        #[test]
        fn class_field_assigned_in_the_constructor_resolves() {
            assert_eq!(
                rows_for(&worker(), "packages/storage-kit/blob-store.ts"),
                vec![(
                    11,
                    "this.client.upload".to_string(),
                    "vault-blob".to_string()
                )]
            );
        }

        /// A field assigned straight from a constructor parameter owns whatever
        /// the construction site passed in, so the instance the site produced
        /// carries it.
        #[test]
        fn constructor_parameter_field_resolves_at_the_construction_site() {
            assert_eq!(
                rows_for(&worker(), "packages/storage-kit/queue-store.ts"),
                vec![(
                    13,
                    "store.client.flush".to_string(),
                    "vault-blob".to_string()
                )]
            );
        }

        /// A static field is the class's, not an instance's.
        #[test]
        fn static_field_resolves_through_the_class_binding() {
            assert_eq!(
                rows_for(&worker(), "packages/storage-kit/beacon.ts"),
                vec![(
                    8,
                    "Beacon.client.capture".to_string(),
                    "beacon-metrics".to_string()
                )]
            );
        }

        /// The singleton idiom: a static field is assigned an instance of the
        /// class through the class name, and a field of that instance is read
        /// back out. Two levels of value, which is why static members are slots
        /// of their own rather than properties of a record on the class.
        ///
        /// Both ways in are covered — the field read directly, and the static
        /// getter that usually wraps it.
        #[test]
        fn a_static_field_holding_an_instance_resolves_through_it() {
            assert_eq!(
                rows_for(&worker(), "packages/singleton-kit/index.ts"),
                vec![
                    (
                        22,
                        "reporter.client.shutdown".to_string(),
                        "pulse-analytics".to_string()
                    ),
                    (
                        26,
                        "PulseReporter.current.client.capture".to_string(),
                        "pulse-analytics".to_string()
                    ),
                ]
            );
        }

        /// A constructor that unpacks an options object, called with an object
        /// literal, at a construction site that is not a variable declarator.
        /// The site is what proves the field's owner, and the row is inside the
        /// class, on `this`.
        #[test]
        fn a_constructor_options_object_reaches_this_inside_the_class() {
            assert_eq!(
                rows_for(&worker(), "packages/jobs-kit/provider.ts"),
                vec![(
                    19,
                    "this._client.send".to_string(),
                    "relay-queue".to_string()
                )]
            );
        }

        /// A single `await` between the declaration and the factory call used
        /// to defeat resolution outright.
        #[test]
        fn awaited_factory_result_resolves() {
            assert_eq!(
                rows_for(&worker(), "packages/doc-kit/sign.ts"),
                vec![
                    (4, "Pdf.load".to_string(), "pdf-toolkit".to_string()),
                    (6, "doc.sign".to_string(), "pdf-toolkit".to_string()),
                ]
            );
        }

        /// Both binding forms of a literal dynamic import: destructured, and
        /// taken whole.
        #[test]
        fn dynamic_import_bindings_resolve() {
            assert_eq!(
                rows_for(&worker(), "packages/crawl-kit/render.ts"),
                vec![
                    (3, "headless.launch".to_string(), "crawler-kit".to_string()),
                    (6, "runtime.shutdown".to_string(), "crawler-kit".to_string()),
                ]
            );
        }

        /// Two re-export hops between the file that constructs the client and
        /// the file that calls through it.
        #[test]
        fn a_barrel_re_export_chain_carries_ownership() {
            assert_eq!(
                rows_for(&worker(), "apps/worker/src/barrel-consumer.ts"),
                vec![(
                    3,
                    "ledger.invoices.list".to_string(),
                    "ledger-client".to_string()
                )]
            );
        }

        /// `export *` carries a name only one source owns. A name two sources
        /// own differently is a genuine ambiguity and is dropped rather than
        /// resolved to whichever star was written first.
        #[test]
        fn star_exports_carry_agreement_and_drop_disagreement() {
            assert_eq!(
                rows_for(&worker(), "apps/worker/src/conflict.ts"),
                vec![(5, "soloClient.upload".to_string(), "vault-blob".to_string())]
            );
        }

        /// A namespace import of an internal module resolves properties through
        /// that module's exports.
        #[test]
        fn namespace_import_of_an_internal_module_resolves() {
            assert_eq!(
                rows_for(&worker(), "apps/worker/src/namespace-internal.ts"),
                vec![(
                    4,
                    "ledgerKit.ledger.payments.settle".to_string(),
                    "ledger-client".to_string()
                )]
            );
        }

        /// A handler no file imports statically still ships with the service
        /// that loads it, so its call sites are the service's.
        #[test]
        fn a_file_reached_only_by_a_dynamic_import_is_in_scope() {
            assert_eq!(
                rows_for(&worker(), "packages/job-kit/handler.ts"),
                vec![(
                    4,
                    "mailer.sendMail".to_string(),
                    "postbox-mailer".to_string()
                )]
            );
        }

        /// A workspace file with resolvable call sites that no service reaches
        /// is not that service's egress.
        #[test]
        fn an_unreachable_file_stays_out_of_the_service_rows() {
            assert_eq!(
                rows_for(&worker(), "packages/orphan-kit/orphan.ts"),
                Vec::new()
            );
            assert_eq!(
                rows_for(&scan("apps/api"), "packages/orphan-kit/orphan.ts"),
                Vec::new()
            );
        }

        /// Hoisting makes a root-declared dependency importable anywhere, and
        /// the package that holds the wrapper declares nothing.
        #[test]
        fn a_dependency_only_the_root_manifest_declares_still_resolves() {
            assert!(
                worker().iter().any(|row| row.package == "pdf-toolkit"),
                "the root-only dependency produced no rows"
            );
        }

        /// carrick#510 is silent on dev dependencies, so they are excluded: a
        /// call into a build or test tool is not service egress. The wrapper is
        /// inside the service, so only the dependency map keeps it out.
        #[test]
        fn a_dev_dependency_wrapper_emits_nothing() {
            assert_eq!(
                rows_for(&worker(), "apps/worker/src/dev-wrapper.ts"),
                Vec::new()
            );
            assert_eq!(
                rows_for(&scan("apps/api"), "apps/api/src/dev-only.ts"),
                Vec::new()
            );
        }

        /// A type-only re-export binds nothing at runtime, so a name that
        /// reaches the consumer only through one owns nothing.
        #[test]
        fn a_type_only_re_export_carries_nothing() {
            assert_eq!(
                rows_for(&worker(), "apps/worker/src/type-reexport.ts"),
                Vec::new()
            );
        }

        /// A local declaration shadowing an import must not inherit its
        /// package.
        #[test]
        fn shadowed_binding_does_not_resolve() {
            assert_eq!(
                rows_for(&worker(), "apps/worker/src/shadow.ts"),
                vec![(
                    9,
                    "mailer.sendMail".to_string(),
                    "postbox-mailer".to_string()
                )]
            );
        }

        /// Calling an internal function is not egress, however much the
        /// function itself owns.
        #[test]
        fn calling_an_internal_wrapper_is_not_a_row() {
            let rows = worker();
            for file in [
                "apps/worker/src/documents.ts",
                "apps/worker/src/crawl.ts",
                "apps/worker/src/storage.ts",
                "apps/worker/src/entry.ts",
            ] {
                assert_eq!(rows_for(&rows, file), Vec::new(), "{} emitted rows", file);
            }
        }

        /// A file two services both reach belongs to both deployments, so its
        /// rows appear in both payloads — and a service reaches nothing it does
        /// not import.
        #[test]
        fn a_shared_file_contributes_to_every_service_that_reaches_it() {
            let shared = (
                8,
                "createTransport".to_string(),
                "postbox-mailer".to_string(),
            );
            assert!(
                rows_for(&worker(), "packages/mail-kit/transport.ts").contains(&shared),
                "the wrapper is missing from the worker payload"
            );
            assert!(
                rows_for(&scan("apps/relay"), "packages/mail-kit/transport.ts").contains(&shared),
                "the same wrapper is missing from the second service's payload"
            );
            let api_files: BTreeSet<String> =
                scan("apps/api").into_iter().map(|row| row.file).collect();
            assert!(
                api_files.iter().all(|file| file.starts_with("apps/api/")),
                "a service that imports no shared package reached one: {:?}",
                api_files
            );
        }

        /// `.catch()` on an owned call resolves through the inner call's root,
        /// so the site yields a second row on the same line. Both rows are
        /// true, and neither is special-cased away.
        #[test]
        fn a_catch_on_an_owned_call_yields_its_own_row() {
            assert_eq!(
                rows_for(&scan("apps/relay"), "apps/relay/src/index.ts"),
                vec![
                    (
                        4,
                        "mailer.sendMail".to_string(),
                        "postbox-mailer".to_string()
                    ),
                    (
                        4,
                        "mailer.sendMail().catch".to_string(),
                        "postbox-mailer".to_string()
                    ),
                ]
            );
        }

        /// The repo-wide walk excludes directories named after build
        /// artifacts, but a service configured to live in one is a service.
        /// Its own files seed the scan whatever that walk thought of them.
        #[test]
        fn a_service_rooted_in_an_excluded_directory_still_scans() {
            assert_eq!(
                rows_for(&scan("apps/build"), "apps/build/src/index.ts"),
                vec![(3, "sendNotice".to_string(), "courier-sdk".to_string())]
            );
        }
    }

    /// Two scans of the same tree produce byte-identical rows.
    /// The receiver shapes carrick#511 could not reach, and the two anchors a
    /// row now carries. Its own service, because the shapes are about which
    /// import a value came through and the fixture's other services are about
    /// how far a value travels.
    mod receiver_shapes {
        use super::*;

        fn forge() -> Vec<ExternalCallCandidate> {
            scan("apps/forge")
        }

        fn anchors_for(rows: &[ExternalCallCandidate], file: &str) -> Vec<FileAnchor> {
            anchored(rows)
                .into_iter()
                .filter(|(row_file, ..)| row_file == file)
                .map(|(_, line, callee, package, symbol, subpath)| {
                    (line, callee, package, symbol, subpath)
                })
                .collect()
        }

        fn anchor(
            line: usize,
            callee: &str,
            symbol: Option<&str>,
            subpath: Option<&str>,
        ) -> Vec<FileAnchor> {
            vec![(
                line,
                callee.to_string(),
                "forge-sdk".to_string(),
                symbol.map(str::to_string),
                subpath.map(str::to_string),
            )]
        }

        #[test]
        fn forge_service_scan_matches_expected_rows() {
            assert_eq!(
                anchored(&forge()),
                expected_anchored(&[
                    (
                        "apps/forge/src/callback.ts",
                        25,
                        "forge.sessions.open",
                        "forge-sdk",
                        Some("default"),
                        None,
                    ),
                    (
                        "apps/forge/src/callback.ts",
                        25,
                        "forge.sessions.open().then",
                        "forge-sdk",
                        Some("default"),
                        None,
                    ),
                    (
                        "apps/forge/src/dynamic.ts",
                        5,
                        "client.sessions.create",
                        "forge-sdk",
                        Some("default"),
                        None,
                    ),
                    (
                        "apps/forge/src/dynamic.ts",
                        11,
                        "sdk.close",
                        "forge-sdk",
                        None,
                        None,
                    ),
                    (
                        "apps/forge/src/injected-runner.ts",
                        7,
                        "this.client.sessions.release",
                        "forge-sdk",
                        Some("default"),
                        None,
                    ),
                    (
                        "apps/forge/src/release.ts",
                        6,
                        "client.sessions.release",
                        "forge-sdk",
                        Some("Forge"),
                        Some("edge"),
                    ),
                    (
                        "apps/forge/src/runner.ts",
                        11,
                        "this.client.sessions.release",
                        "forge-sdk",
                        Some("default"),
                        None,
                    ),
                    (
                        "apps/forge/src/sessions.ts",
                        9,
                        "forge.sessions.create",
                        "forge-sdk",
                        Some("default"),
                        None,
                    ),
                ])
            );
        }

        /// A handle built by a factory in a sibling file: the factory's own
        /// `return` names the client, so the call through the handle is a row
        /// even though the calling file never mentions the package.
        #[test]
        fn a_relative_factory_resolves_through_what_it_returns() {
            assert_eq!(
                anchors_for(&forge(), "apps/forge/src/sessions.ts"),
                anchor(9, "forge.sessions.create", Some("default"), None)
            );
        }

        /// The same hop when the factory hands back something the AST cannot
        /// follow — a pooled handle read out of a map. Its declared return type
        /// is the statement that resolves it, and the type is imported from a
        /// subpath, so the row records the subpath too.
        #[test]
        fn a_relative_factory_resolves_through_its_declared_return_type() {
            assert_eq!(
                anchors_for(&forge(), "apps/forge/src/release.ts"),
                anchor(6, "client.sessions.release", Some("Forge"), Some("edge"))
            );
        }

        /// A client held in a class property, constructed in the constructor.
        /// The callee prints as written.
        #[test]
        fn a_class_property_constructed_in_the_constructor_resolves() {
            assert_eq!(
                anchors_for(&forge(), "apps/forge/src/runner.ts"),
                anchor(11, "this.client.sessions.release", Some("default"), None)
            );
        }

        /// The same property when nothing in the class assigns it — the shape a
        /// framework or an injector fills in. The declared type is all there
        /// is, and it is enough.
        #[test]
        fn a_class_property_typed_by_a_dependency_resolves() {
            assert_eq!(
                anchors_for(&forge(), "apps/forge/src/injected-runner.ts"),
                anchor(7, "this.client.sessions.release", Some("default"), None)
            );
        }

        /// `const { default: C } = await import('pkg')` names an export and
        /// `const m = await import('pkg')` names the module, so the two rows
        /// carry different anchors. Constructing through the first is still not
        /// a row of its own; it only resolves the receiver.
        #[test]
        fn dynamic_import_bindings_carry_the_export_they_name() {
            assert_eq!(
                anchors_for(&forge(), "apps/forge/src/dynamic.ts"),
                vec![
                    (
                        5,
                        "client.sessions.create".to_string(),
                        "forge-sdk".to_string(),
                        Some("default".to_string()),
                        None,
                    ),
                    (
                        11,
                        "sdk.close".to_string(),
                        "forge-sdk".to_string(),
                        None,
                        None,
                    ),
                ]
            );
        }

        /// Two branches reaching one binding through different exports of one
        /// package, and two more through different subpaths of it. The package
        /// agrees in both cases and the anchor does not, which is a
        /// disagreement like any other: the binding owns nothing.
        #[test]
        fn one_package_reached_through_two_anchors_emits_nothing() {
            assert_eq!(
                anchors_for(&forge(), "apps/forge/src/split-consumer.ts"),
                Vec::new()
            );
        }

        /// The line the annotation rule does not cross. A parameter's declared
        /// type says what a caller is expected to pass, not what it passed, so
        /// neither a locally declared interface nor an imported one resolves a
        /// receiver.
        ///
        /// Nor does a callback's parameter inherit the receiver the promise
        /// came off. The rows here are the owned call and the `.then` applied
        /// to what it returned, both of them chains off a value the package
        /// produced; `session.scrape` inside the callback is not one of them.
        #[test]
        fn a_callback_parameter_does_not_inherit_its_receiver() {
            assert_eq!(
                anchors_for(&forge(), "apps/forge/src/callback.ts"),
                vec![
                    (
                        25,
                        "forge.sessions.open".to_string(),
                        "forge-sdk".to_string(),
                        Some("default".to_string()),
                        None,
                    ),
                    (
                        25,
                        "forge.sessions.open().then".to_string(),
                        "forge-sdk".to_string(),
                        Some("default".to_string()),
                        None,
                    ),
                ]
            );
        }
    }

    #[test]
    fn scan_is_deterministic() {
        assert_eq!(scan("apps/worker"), scan("apps/worker"));
        assert_eq!(scan("apps/api"), scan("apps/api"));
    }

    #[test]
    fn a_repo_declaring_no_dependencies_emits_nothing() {
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path();
        std::fs::write(root.join("package.json"), r#"{"name":"bare"}"#).unwrap();
        std::fs::write(
            root.join("app.ts"),
            "import { send } from \"courier-sdk\";\nsend();\n",
        )
        .unwrap();
        assert_eq!(
            scan_workspace(&[root.join("app.ts")], root),
            Vec::new(),
            "no declared dependencies means no candidates"
        );
    }

    #[test]
    fn mechanism_serializes_as_snake_case() {
        let row = ExternalCallCandidate {
            file: "src/a.ts".to_string(),
            line: 3,
            callee: "client.send".to_string(),
            package: "courier-sdk".to_string(),
            mechanism: CallMechanism::Sdk,
            import_symbol: Some("default".to_string()),
            subpath: Some("edge".to_string()),
        };
        assert_eq!(
            serde_json::to_value(&row).unwrap(),
            serde_json::json!({
                "file": "src/a.ts",
                "line": 3,
                "callee": "client.send",
                "package": "courier-sdk",
                "mechanism": "sdk",
                "import_symbol": "default",
                "subpath": "edge"
            })
        );
    }

    /// The anchors are additive: a row that has neither serializes to exactly
    /// the five keys the channel shipped with, so a reader written against
    /// carrick#511 still reads every row.
    #[test]
    fn absent_anchors_are_omitted_from_the_row() {
        let row = ExternalCallCandidate {
            file: "src/a.ts".to_string(),
            line: 3,
            callee: "sdk.send".to_string(),
            package: "courier-sdk".to_string(),
            mechanism: CallMechanism::Sdk,
            import_symbol: None,
            subpath: None,
        };
        assert_eq!(
            serde_json::to_value(&row).unwrap(),
            serde_json::json!({
                "file": "src/a.ts",
                "line": 3,
                "callee": "sdk.send",
                "package": "courier-sdk",
                "mechanism": "sdk"
            })
        );
    }

    /// And they are optional on the way back in, so a row serialized before
    /// they existed still deserializes.
    #[test]
    fn a_row_without_anchors_deserializes() {
        let row: ExternalCallCandidate = serde_json::from_value(serde_json::json!({
            "file": "src/a.ts",
            "line": 3,
            "callee": "sdk.send",
            "package": "courier-sdk",
            "mechanism": "sdk"
        }))
        .expect("row");
        assert_eq!(row.import_symbol, None);
        assert_eq!(row.subpath, None);
    }

    /// The HTTP-shaped mechanisms ride the row shape `sdk` shipped with — same
    /// five keys, same casing — so one reader handles all three.
    #[test]
    fn http_mechanisms_serialize_in_the_same_row_shape() {
        let row = ExternalCallCandidate {
            file: "src/pay.ts".to_string(),
            line: 12,
            callee: "POST".to_string(),
            package: "api.vendor.test".to_string(),
            mechanism: CallMechanism::ExternalHttp,
            import_symbol: None,
            subpath: None,
        };
        assert_eq!(
            serde_json::to_value(&row).unwrap(),
            serde_json::json!({
                "file": "src/pay.ts",
                "line": 12,
                "callee": "POST",
                "package": "api.vendor.test",
                "mechanism": "external_http"
            })
        );
        assert_eq!(
            serde_json::to_value(ExternalCallCandidate {
                mechanism: CallMechanism::EnvVarUrl,
                package: "BILLING_API".to_string(),
                ..row
            })
            .unwrap()["mechanism"],
            serde_json::json!("env_var_url")
        );
    }

    mod http_rows {
        use super::*;
        use crate::mount_graph::DataFetchingCall;

        fn config() -> Config {
            Config {
                internal_domains: ["orders.internal.test".to_string()].into_iter().collect(),
                external_domains: ["api.vendor.test".to_string()].into_iter().collect(),
                internal_env_vars: ["ORDERS_URL".to_string()].into_iter().collect(),
                external_env_vars: ["BILLING_API".to_string()].into_iter().collect(),
                ..Config::default()
            }
        }

        /// A consumer call as the mount-graph builder records one: the host and
        /// the typed line are the fields it now retains from normalisation and
        /// extraction.
        fn call(target: &str, host: Option<&str>, line: Option<u32>) -> DataFetchingCall {
            DataFetchingCall {
                method: "get".to_string(),
                target_url: target.to_string(),
                canonical_path: target.to_string(),
                client: "fetch(".to_string(),
                file_location: format!("src/client.ts:{}", line.unwrap_or(1)),
                call_kind: None,
                repo_name: None,
                service_name: None,
                host: host.map(str::to_string),
                line,
            }
        }

        fn rows(calls: &[DataFetchingCall]) -> Vec<ExternalCallCandidate> {
            let config = config();
            from_data_calls(
                calls,
                Path::new("/repo"),
                &config,
                &crate::url_normalizer::UrlNormalizer::new(&config),
            )
        }

        /// The domain and the line the scanner used to discard both reach the
        /// row, and the HTTP method stands in for the callee.
        #[test]
        fn declared_external_host_becomes_a_row() {
            assert_eq!(
                rows(&[call(
                    "https://api.vendor.test/v1/charges",
                    Some("api.vendor.test"),
                    Some(12)
                )]),
                vec![ExternalCallCandidate {
                    file: "src/client.ts".to_string(),
                    line: 12,
                    callee: "GET".to_string(),
                    package: "api.vendor.test".to_string(),
                    mechanism: CallMechanism::ExternalHttp,
                    import_symbol: None,
                    subpath: None,
                }]
            );
        }

        /// A subdomain of a declared external domain is classified external by
        /// the normalizer, and the row names the host as written, not the
        /// declaration that matched it.
        #[test]
        fn subdomain_of_a_declared_host_keeps_its_own_name() {
            let rows = rows(&[call(
                "https://eu.api.vendor.test/v1/charges",
                Some("eu.api.vendor.test"),
                Some(3),
            )]);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].package, "eu.api.vendor.test");
        }

        /// The env-var name reaches the row for a variable the repo declares
        /// external — the pair the matcher drops on its way past the call.
        #[test]
        fn declared_external_env_var_becomes_a_row() {
            assert_eq!(
                rows(&[call("${process.env.BILLING_API}/invoices", None, Some(7))]),
                vec![ExternalCallCandidate {
                    file: "src/client.ts".to_string(),
                    line: 7,
                    callee: "GET".to_string(),
                    package: "BILLING_API".to_string(),
                    mechanism: CallMechanism::EnvVarUrl,
                    import_symbol: None,
                    subpath: None,
                }]
            );
        }

        /// The canonical `ENV_VAR:NAME:/path` shape resolves to the same row as
        /// the raw interpolation.
        #[test]
        fn canonical_env_var_route_becomes_a_row() {
            let rows = rows(&[call("ENV_VAR:BILLING_API:/invoices", None, Some(4))]);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].package, "BILLING_API");
            assert_eq!(rows[0].mechanism, CallMechanism::EnvVarUrl);
        }

        /// An undeclared destination is the one thing the scanner does not
        /// know. It stays out of the inventory rather than being guessed into
        /// it; the unclassified env-var case is already reported as an
        /// `EnvVarCall` finding, which this channel does not touch.
        #[test]
        fn undeclared_destinations_emit_nothing() {
            assert_eq!(
                rows(&[
                    call(
                        "https://api.unlisted.test/v1/x",
                        Some("api.unlisted.test"),
                        Some(2)
                    ),
                    call("${process.env.UNKNOWN_API}/x", None, Some(3)),
                ]),
                Vec::new()
            );
        }

        /// An in-org destination is not egress: it is already in the endpoint
        /// index as a call, and the inventory would double-count it.
        #[test]
        fn declared_internal_destinations_emit_nothing() {
            assert_eq!(
                rows(&[
                    call(
                        "https://orders.internal.test/orders",
                        Some("orders.internal.test"),
                        Some(2)
                    ),
                    call("${process.env.ORDERS_URL}/orders", None, Some(3)),
                ]),
                Vec::new()
            );
        }

        /// A relative call has no destination to name at all.
        #[test]
        fn relative_call_emits_nothing() {
            assert_eq!(rows(&[call("/api/orders", None, Some(9))]), Vec::new());
        }

        /// A row with no line cannot be audited, so it is dropped rather than
        /// carried with a guessed location.
        #[test]
        fn call_without_a_line_emits_nothing() {
            assert_eq!(
                rows(&[call(
                    "https://api.vendor.test/v1/charges",
                    Some("api.vendor.test"),
                    None
                )]),
                Vec::new()
            );
        }

        /// The file half of `file_location` is repo-relative in the row
        /// whichever analysis path recorded it — the incremental path
        /// normalises its keys, the full path does not.
        #[test]
        fn absolute_scan_paths_are_relativized() {
            let mut absolute = call(
                "https://api.vendor.test/v1/charges",
                Some("api.vendor.test"),
                Some(5),
            );
            absolute.file_location = "/repo/src/client.ts:5".to_string();
            assert_eq!(rows(&[absolute])[0].file, "src/client.ts");
        }

        /// Two calls to the same host on the same line collapse; the same host
        /// on different lines does not.
        #[test]
        fn rows_are_deduplicated_and_sorted() {
            let hit = call(
                "https://api.vendor.test/v1/charges",
                Some("api.vendor.test"),
                Some(12),
            );
            let mut earlier = hit.clone();
            earlier.line = Some(4);
            earlier.file_location = "src/client.ts:4".to_string();
            let rows = rows(&[hit.clone(), earlier, hit]);
            assert_eq!(
                rows.iter().map(|row| row.line).collect::<Vec<_>>(),
                vec![4, 12]
            );
        }

        /// The merged list is one sorted, deduplicated set, so the payload has
        /// a single order whatever produced a given row.
        #[test]
        fn merge_sorts_both_sources_as_one_set() {
            let sdk = ExternalCallCandidate {
                file: "src/client.ts".to_string(),
                line: 20,
                callee: "ledger.charge".to_string(),
                package: "ledger-client".to_string(),
                mechanism: CallMechanism::Sdk,
                import_symbol: Some("default".to_string()),
                subpath: None,
            };
            let http = ExternalCallCandidate {
                file: "src/client.ts".to_string(),
                line: 12,
                callee: "GET".to_string(),
                package: "api.vendor.test".to_string(),
                mechanism: CallMechanism::ExternalHttp,
                import_symbol: None,
                subpath: None,
            };
            let merged = merge(vec![sdk.clone(), sdk.clone()], vec![http.clone()]);
            assert_eq!(merged, vec![http, sdk]);
        }
    }
}
