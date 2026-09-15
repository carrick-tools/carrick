//! Deterministic GraphQL contract extraction.
//!
//! Producers are SDL schemas: `.graphql`/`.gql` files and `gql`/`graphql`
//! tagged template literals containing type-system definitions. Consumers are
//! executable documents: tagged template literals and `.graphql` files
//! containing operations. Everything here is parse-based — no LLM. Sources
//! that fail to parse (e.g. documents with interpolations mid-token) are
//! skipped silently: per the brittleness guardrails, drift findings may only
//! come from deterministic evidence, so a miss is a coverage gap, never a
//! false positive.
//!
//! Out of scope by design: Relay compiled artifacts and persisted-query
//! manifests (no document in source). A code-first schema (root fields built
//! by calls, e.g. Pothos/TypeGraphQL/Nexus) states no SDL in source; its
//! printed schema is read when the service names it in `graphqlSchemas`
//! (carrick#1099, [`resolve_declared_schemas`]), and the scan report hints at
//! that setting when a GraphQL-using server indexes no schema fields
//! ([`service_notices`]).
//!
//! A document is a call only when the schema it is written against is served
//! here: [`SchemaCatalogue`] attributes each document to a schema file, and a
//! document against a committed schema no service serves is a call to someone
//! else's API (carrick#1134). User-facing description: README, "GraphQL
//! documents for another team's API".

use crate::operation::{GraphqlOperationKind, OperationKey};
use crate::parser::parse_file;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use swc_common::errors::{ColorConfig, Handler};
use swc_common::{GLOBALS, Globals, SourceMap, Spanned, sync::Lrc};
use swc_ecma_ast::{
    CallExpr, Callee, Expr, ImportDecl, ImportSpecifier, TaggedTpl, TsEntityName, TsType,
    TsTypeElement, VarDeclarator,
};
use swc_ecma_visit::{Visit, VisitWith};
use tracing::debug;
use walkdir::WalkDir;

/// A producer or consumer GraphQL operation with its source location.
#[derive(Debug, Clone)]
pub struct GraphqlOp {
    pub key: OperationKey,
    pub file_path: PathBuf,
    pub line: u32,
    /// 1-based line in `file_path` where the GraphQL text this operation was
    /// parsed from begins: `1` for a `.graphql`/`.gql` file, the template's
    /// line for a tagged template. `(file_path, document_line)` names one
    /// document, the unit a document is attributed to a schema by
    /// ([`SchemaCatalogue::attribute`], carrick#1134).
    pub document_line: u32,
    /// Deterministic type anchor (`primary_type_symbol`), mirroring the
    /// HTTP/socket anchors (#248). For SDL producers this is the root field's
    /// SDL type expression rendered to its canonical form (`Order`, `Order!`,
    /// `[Order!]!`) — the only anchor source available without a
    /// framework-specific SDL-field → TS-resolver mapping (that mapping is
    /// follow-up #268). Document consumers carry no SDL type, so this stays `None`.
    pub primary_type_symbol: Option<String>,
    /// Consumer's bound TS result type, captured deterministically at the
    /// `client.request<T>(DOC)` call site (mirrors `SocketOp::payload_type_symbol`,
    /// #245). For a consumer that binds the document to a named result type
    /// (`request<OrderView>`) or a single-property wrapper whose key is the
    /// operation field (`request<{ order: OrderView }>`), this is the inner
    /// symbol (`OrderView`); `None` for producers and for consumers with no
    /// typed call site. This is the consumer anchor the SDL path can't provide.
    pub payload_type_symbol: Option<String>,
    /// Module specifier the consumer's bound type is imported from, paired with
    /// `payload_type_symbol`. `None` when the symbol is declared in the same file
    /// or no call site was matched.
    pub payload_type_source: Option<String>,
    /// PRODUCER-only: the file whose resolver implements this schema field,
    /// joined in from the file-analyzer's `graphql_operations` (Stage B1). The
    /// producer's real response contract is the resolver function's RETURN type
    /// expanded (`Promise<ApiResponse<Order>>` → `{ data: …, errors }`), which
    /// the SDL alone can't give, so this points the `FunctionReturn` infer
    /// request at the resolver. `None` for SDL producers with no matched LLM op,
    /// and always `None` for consumers (they anchor on `payload_type_symbol`).
    pub resolver_file: Option<PathBuf>,
    /// PRODUCER-only: 1-based line where the resolver function is defined,
    /// paired with `resolver_file`. Anchors the `FunctionReturn` infer request.
    /// `None` whenever `resolver_file` is `None`.
    pub resolver_line: Option<u32>,
    /// PRODUCER-only fallback (#248): the co-located TS type that declares this
    /// field's response shape when NO resolver function exists (e.g. an SDL
    /// `orders: [Order!]!` field backed by `interface Order`, with no
    /// `resolveOrders`). Located by the file-analyzer (`graphql_operations`
    /// `primary_type_symbol`), it is bundled + structurally expanded + wrapped in
    /// the SDL list depth by the type sidecar — the deterministic half of the
    /// LLM-locate/scanner-expand split. `None` when a resolver was matched (the
    /// `FunctionReturn` path wins) or nothing was located. Paired with
    /// `resolver_file`, which is set to the analyzed file the entry came from.
    pub response_type_symbol: Option<String>,
    /// PRODUCER-only: import specifier the `response_type_symbol` is declared in
    /// (`./types/order`), resolved against `resolver_file`. `None` when the type
    /// is declared in `resolver_file` itself. Null whenever
    /// `response_type_symbol` is null.
    pub response_type_source: Option<String>,
    /// CONSUMER-only fallback (#268): the co-located TS type describing a
    /// document's RESULT shape when the deterministic pass found no explicit
    /// call-site generic (`TaggedTplVisitor::capture_request_call` never
    /// matched, so `payload_type_symbol` is `None`). Located by the
    /// file-analyzer (`graphql_consumer_locates`), it is bundled + structurally
    /// expanded by the type sidecar through the same path
    /// `payload_type_symbol` uses — the deterministic half of the
    /// LLM-locate/scanner-expand split (mirrors the producer `response_type_symbol`
    /// fallback). `None` when the deterministic anchor already exists (that
    /// signal always wins — see the engine merge's isolation guard) or nothing
    /// was located. Always `None` for producers.
    pub consumer_located_type_symbol: Option<String>,
    /// CONSUMER-only: import specifier the `consumer_located_type_symbol` type is
    /// declared in (`./types/order`), resolved against `file_path` (the
    /// consuming file itself — the join is scoped per-file, so there is no
    /// separate "located file" the way producers have a distinct
    /// `resolver_file`). `None` when the type is declared in `file_path` itself.
    /// Null whenever `consumer_located_type_symbol` is null.
    pub consumer_located_type_source: Option<String>,
    /// CONSUMER-only: the schema identity the document this operation was
    /// parsed from is bound to, set by [`ConsumerAttribution::apply`] on every
    /// consumer it keeps and carried onto the call row
    /// (`calls[].schema_binding`). `None` for producers and before
    /// attribution runs.
    pub schema_binding: Option<SchemaBinding>,
}

#[derive(Debug, Clone, Default)]
pub struct GraphqlExtraction {
    /// Schema root fields this service provides.
    pub producers: Vec<GraphqlOp>,
    /// Top-level fields of executable documents this service sends.
    pub consumers: Vec<GraphqlOp>,
}

impl GraphqlExtraction {
    pub fn is_empty(&self) -> bool {
        self.producers.is_empty() && self.consumers.is_empty()
    }

    fn merge(&mut self, other: GraphqlExtraction) {
        self.producers.extend(other.producers);
        self.consumers.extend(other.consumers);
    }
}

/// Repo-global GraphQL producer context for the file-analyzer (Stage B2).
///
/// The file-analyzer needs two things to link a resolver function to a schema
/// field and emit a `graphql_operations` entry: (1) the list of SDL producer
/// fields this service exposes (so it knows which functions are resolvers), and
/// (2) the SDL scan roots (so the orchestrator can route an otherwise-skipped,
/// candidate-less resolver file co-located with the schema into analysis). Both
/// are derived deterministically from the SDL — no LLM, no per-file cost.
///
/// `lines` is one formatted string per producer field (`"query order: Order"`),
/// stable across every file in a scan, so it lives in the cacheable front block
/// of the user message. Empty `lines` means the service has no SDL producers and
/// nothing changes.
#[derive(Debug, Clone, Default)]
pub struct GraphqlProducerHints {
    /// One `"{kind} {field}: {sdl_type}"` line per SDL root field.
    pub lines: Vec<String>,
    /// The service's SDL scan roots (its `directory` + `include` roots),
    /// used to gate the don't-skip routing to schema-co-located files.
    pub scan_roots: Vec<PathBuf>,
    /// One line per root field of the schemas the service names in
    /// `graphqlSchemas` that `lines` does not already hold, in the same
    /// format. Given only to the files that build a schema in code
    /// ([`Self::schema_builder_lines`], carrick#1157).
    pub declared_lines: Vec<String>,
}

impl GraphqlProducerHints {
    /// Build the producer hint context for a service: run the (cheap,
    /// deterministic) SDL scan over `scan_roots` + `service_files` and format
    /// each producer field as a hint line. `scan_roots` are retained for the
    /// co-location check in the don't-skip routing. `declared_schemas` are
    /// the files the service's `graphqlSchemas` resolved to.
    pub fn collect(
        scan_roots: Vec<PathBuf>,
        declared_schemas: &[PathBuf],
        service_files: &[PathBuf],
    ) -> Self {
        // Declared schemas (`graphqlSchemas`) are deliberately NOT in the hint
        // list: the hints are part of every analysed file's prompt, so adding
        // them would re-ask the model for the whole service the first time the
        // setting appears, and the setting is meant to take effect on a free
        // rescan. Their producer rows come from `scan_repo` in the engine, and
        // their lines reach only the files that build the schema in code.
        let extraction = scan_repo(&scan_roots, &[], service_files);
        let lines: Vec<String> = extraction
            .producers
            .iter()
            .filter_map(Self::format_producer)
            .collect();
        let mut seen: HashSet<String> = lines.iter().cloned().collect();
        let declared_lines = scan_repo(&[], declared_schemas, &[])
            .producers
            .iter()
            .filter_map(Self::format_producer)
            .filter(|line| seen.insert(line.clone()))
            .collect();
        Self {
            lines,
            scan_roots,
            declared_lines,
        }
    }

    /// Whether the service has any schema field a resolver could be linked
    /// to, walked or declared.
    pub fn has_schema_fields(&self) -> bool {
        !self.lines.is_empty() || !self.declared_lines.is_empty()
    }

    /// The field list for a file that builds the schema in code: the walked
    /// fields, then the declared ones. A code-first schema's fields are
    /// usually only in a printed schema the service declares.
    pub fn schema_builder_lines(&self) -> Vec<String> {
        self.lines
            .iter()
            .chain(&self.declared_lines)
            .cloned()
            .collect()
    }

    /// Format a single producer op as `"{kind} {field}: {sdl_type}"`
    /// (e.g. `"query order: Order"`). `None` if the op is not a GraphQL
    /// producer key (should never happen for `.producers`) or has no SDL type.
    fn format_producer(op: &GraphqlOp) -> Option<String> {
        let OperationKey::Graphql { kind, field } = &op.key else {
            return None;
        };
        let sdl_type = op.primary_type_symbol.as_deref()?;
        Some(format!("{} {}: {}", kind.as_str(), field, sdl_type))
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Whether `file` lives under one of the SDL scan roots — i.e. it is
    /// co-located with this service's schema. Used to scope the don't-skip
    /// routing tightly: only schema-co-located resolver files are rescued from
    /// the zero-candidate skip, never every exported-function file in the repo.
    pub fn file_within_scan_roots(&self, file: &Path) -> bool {
        self.scan_roots.iter().any(|root| file.starts_with(root))
    }
}

/// Repo-global GraphQL consumer context for the file-analyzer (#268 — the
/// consumer mirror of `GraphqlProducerHints`).
///
/// The deterministic pass (`TaggedTplVisitor::capture_request_call`) anchors a
/// document consumer only when it finds an explicit call-site generic
/// (`client.request<T>(DOC)`). A document consumed with no such generic (e.g. a
/// subscription handed straight to a callback, `graphql-ws`-style) stays
/// unanchored even though its result type is often co-located in the same
/// file. This hint tells the file-analyzer exactly which (kind, field, file)
/// triples are still unanchored, so it only ever tries to locate a type for an
/// operation the deterministic pass genuinely missed.
///
/// `lines` is one formatted string per unanchored consumer op, stable across
/// every file in a scan, so it lives in the cacheable front block of the user
/// message alongside the producer hints. Empty `lines` means every consumer
/// op in this service already has a deterministic anchor (or there are no
/// GraphQL consumers at all) and nothing changes.
#[derive(Debug, Clone, Default)]
pub struct GraphqlConsumerHints {
    /// One `"{kind}|{field} @ {file}"` line per consumer op with no
    /// `payload_type_symbol` anchor.
    pub lines: Vec<String>,
    /// Exact files containing at least one unanchored consumer op, so the
    /// don't-skip routing can rescue a candidate-less file that co-locates
    /// one (#268). Exact path membership only — unlike the producer's
    /// scan-root containment check, a consumer's located type has no fixed
    /// directory to scope to (it can live in any TS/JS file the deterministic
    /// pass already walked).
    pub files: std::collections::HashSet<PathBuf>,
}

impl GraphqlConsumerHints {
    /// Build the consumer hint context for a service: run the same
    /// deterministic scan `GraphqlProducerHints::collect` uses (accepted as a
    /// second walk over the same roots/files — cheap and parse-only, no LLM
    /// cost) and keep every consumer op with no `payload_type_symbol`. An op
    /// the deterministic pass already anchored (`TaggedTplVisitor::capture_request_call`
    /// matched an explicit generic) needs no hint — there is nothing left to
    /// locate.
    pub fn collect(scan_roots: Vec<PathBuf>, service_files: &[PathBuf]) -> Self {
        let extraction = scan_repo(&scan_roots, &[], service_files);
        let mut lines = Vec::new();
        let mut files = std::collections::HashSet::new();
        for op in &extraction.consumers {
            if op.payload_type_symbol.is_some() {
                continue;
            }
            let Some(line) = Self::format_consumer(op) else {
                continue;
            };
            lines.push(line);
            files.insert(op.file_path.clone());
        }
        Self { lines, files }
    }

    /// Format a single unanchored consumer op as `"{kind}|{field} @ {file}"`
    /// (e.g. `"subscription|orderUpdated @ lib/graphql.ts"`). `None` if the op
    /// is not a GraphQL consumer key (should never happen for `.consumers`).
    fn format_consumer(op: &GraphqlOp) -> Option<String> {
        let OperationKey::Graphql { kind, field } = &op.key else {
            return None;
        };
        Some(format!(
            "{}|{} @ {}",
            kind.as_str(),
            field,
            op.file_path.display()
        ))
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Whether `file` co-locates at least one unanchored consumer op — used to
    /// rescue an otherwise-skipped, candidate-less file into analysis (#268).
    pub fn file_has_hint(&self, file: &Path) -> bool {
        self.files.contains(file)
    }
}

/// Additional directories excluded from GraphQL discovery, alongside the shared
/// dependency and build-artifact exclusions.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "out",
    "coverage",
    "__generated__", // Relay artifacts — out of scope
];

/// Whether `path` names a GraphQL file by its extension.
fn is_graphql_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext == "graphql" || ext == "gql")
}

/// Every `.graphql`/`.gql` file a service's own walk reads under `roots`, in
/// walk order (roots in order, names sorted), skipping dependency, build and
/// generated-artifact folders. A path under two overlapping roots is listed
/// twice; callers dedup.
fn graphql_files_under(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in roots {
        for entry in WalkDir::new(root)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                !e.file_name()
                    .to_str()
                    .map(|name| {
                        SKIP_DIRS.contains(&name)
                            || crate::packages::MANIFEST_SKIP_DIRS.contains(&name)
                    })
                    .unwrap_or(false)
            })
            .filter_map(Result::ok)
        {
            if is_graphql_file(entry.path()) {
                files.push(entry.path().to_path_buf());
            }
        }
    }
    files
}

/// Extract GraphQL operations for a single service: `.graphql`/`.gql` SDL files
/// under the service's own `scan_roots` plus tagged template literals in the
/// given service files (the same TS/JS set the rest of the pipeline analyzes).
///
/// `scan_roots` are the service's own directories (its `directory` plus any
/// `include` roots), NOT the whole monorepo. Walking the repo root here would
/// attribute a sibling package's schema to every service in the monorepo (#242):
/// `orders-pkg` would be credited with `gateway`'s `query order` producer.
///
/// `declared_schemas` are the files the service names in `graphqlSchemas`
/// (resolved by [`resolve_declared_schemas`]). They are read wherever they
/// sit, including build folders the walk skips and another service's
/// directory, and only their PRODUCERS are kept: the setting declares what the
/// service serves, so an executable document in one is not a call this service
/// makes. They go first, so a declared file that also sits under a scan root
/// is read once, as a declaration.
pub fn scan_repo(
    scan_roots: &[PathBuf],
    declared_schemas: &[PathBuf],
    service_files: &[PathBuf],
) -> GraphqlExtraction {
    let mut extraction = GraphqlExtraction::default();

    // Overlapping roots (a service `include` that overlaps its `directory`) must
    // not extract the same schema twice, so dedup SDL paths across roots.
    let mut seen_sdl: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for path in declared_schemas {
        if !seen_sdl.insert(path.clone()) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        extraction
            .producers
            .extend(extract_from_document_text(&content, path, 1).producers);
    }
    for path in graphql_files_under(scan_roots) {
        if !seen_sdl.insert(path.clone()) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        extraction.merge(extract_from_document_text(&content, &path, 1));
    }

    for file in service_files {
        let is_script = file
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| matches!(ext, "ts" | "tsx" | "js" | "jsx"));
        if !is_script {
            continue;
        }
        extraction.merge(extract_from_ts_file(file));
    }

    debug!(
        producers = extraction.producers.len(),
        consumers = extraction.consumers.len(),
        "GraphQL extraction complete"
    );
    extraction
}

/// What a service's `graphqlSchemas` entries resolved to (carrick#1099).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeclaredSchemas {
    /// Every file an entry matched, sorted and deduplicated, joined onto the
    /// repository root.
    pub files: Vec<PathBuf>,
    /// One sentence per entry or file that declares nothing: an entry that is
    /// not a repository path or not a valid glob, an entry that matches no
    /// file, and a matched file that defines no root operation field. The scan
    /// report prints each one, so a declaration that does nothing is never
    /// silent.
    pub problems: Vec<String>,
}

/// Resolve a service's `graphqlSchemas` entries against the repository root.
///
/// Each entry is a path relative to the root (where `carrick.json` sits) or a
/// glob (`apps/api/dist/**/*.graphql`). Nothing is skipped: the setting exists
/// for the printed schema in a build folder or another app's directory, which
/// the service's own SDL walk does not read. A matched file is parsed here as
/// well, so a file that is not a schema, or defines no Query, Mutation or
/// Subscription field, is reported rather than quietly adding nothing.
pub fn resolve_declared_schemas(repo_root: &Path, patterns: &[String]) -> DeclaredSchemas {
    let mut declared = DeclaredSchemas::default();
    let mut files = std::collections::BTreeSet::new();
    let options = glob::MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    for raw in patterns {
        let entry = raw.trim().trim_start_matches("./");
        let relative = Path::new(entry);
        if entry.is_empty()
            || relative.is_absolute()
            || relative
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            declared.problems.push(format!(
                "`graphqlSchemas` entry '{raw}' is not a path inside the repository, so nothing \
                 was read for it"
            ));
            continue;
        }
        // Checked on its own first, so an error position counts from the
        // entry the user wrote rather than from the checkout path.
        if let Err(error) = glob::Pattern::new(entry) {
            declared.problems.push(format!(
                "`graphqlSchemas` entry '{raw}' is not a valid glob ({error}), so nothing was \
                 read for it"
            ));
            continue;
        }
        // The root is escaped so a checkout path containing glob syntax (`[`,
        // `*`) cannot become a pattern that silently matches nothing.
        let pattern = format!(
            "{}/{}",
            glob::Pattern::escape(&repo_root.to_string_lossy()),
            entry
        );
        // An escaped root joined to an entry that compiled above is a valid
        // pattern, so the error arm cannot be reached.
        let matches: Vec<PathBuf> = glob::glob_with(&pattern, options)
            .map(|paths| {
                paths
                    .filter_map(Result::ok)
                    .filter(|path| path.is_file())
                    .collect()
            })
            .unwrap_or_default();
        if matches.is_empty() {
            declared.problems.push(format!(
                "`graphqlSchemas` entry '{raw}' matches no file in this repository, so the \
                 operations it declares are not indexed"
            ));
            continue;
        }
        files.extend(matches);
    }
    for file in &files {
        let shown = file.strip_prefix(repo_root).unwrap_or(file).display();
        match std::fs::read_to_string(file) {
            Ok(content) => {
                if extract_from_document_text(&content, file, 1)
                    .producers
                    .is_empty()
                {
                    declared.problems.push(format!(
                        "`graphqlSchemas` file '{shown}' defines no Query, Mutation or \
                         Subscription field, so it adds no operations"
                    ));
                }
            }
            Err(error) => declared.problems.push(format!(
                "`graphqlSchemas` file '{shown}' could not be read ({error}), so it adds no \
                 operations"
            )),
        }
    }
    declared.files = files.into_iter().collect();
    declared
}

/// The schema identity a GraphQL call row is bound to, as the index blob
/// carries it (`calls[].schema_binding`, carrick#1134). Only documents that
/// stay calls have one: an external or unresolved document is not a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaBinding {
    /// A schema a service in this repository serves holds the document's
    /// fields. An operation no producer has is a real missing operation.
    Served,
    /// No schema this repository holds has any of the document's fields. Its
    /// server may be a repository the project does not index.
    NoLocalSchema,
}

/// Whether a schema file is one a service in this scan serves (carrick#1134).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaOrigin {
    /// A service's own SDL walk reads the file, or a service names it in
    /// `graphqlSchemas`.
    Served,
    /// The repository tracks the file but no service serves it: a copy of a
    /// schema someone else serves, kept so documents can be checked against
    /// it.
    External,
}

/// One schema file the repository holds, reduced to what attribution reads.
#[derive(Debug, Clone)]
pub struct KnownSchema {
    /// Repository-relative path.
    pub file: PathBuf,
    pub origin: SchemaOrigin,
    /// Canonical keys (`graphql|query|order`) of its root operation fields.
    root_keys: HashSet<String>,
}

/// Where one service reads schemas from, for [`SchemaCatalogue::build`].
#[derive(Debug, Clone, Default)]
pub struct ServedSchemaSources {
    /// The service's SDL walk roots (its `directory` and `include` roots).
    pub roots: Vec<PathBuf>,
    /// The files its `graphqlSchemas` entries resolved to.
    pub declared: Vec<PathBuf>,
}

/// Every schema file in the repository, each marked served or external: the
/// identities a GraphQL document is attributed to (carrick#1134).
///
/// A document's operations are keyed by field name alone, so a client that
/// talks to someone else's GraphQL API and a client of the project's own
/// server produce the same kind of row, and the only thing that tells them
/// apart without reading a codegen config is the schema the document is
/// written against. SDL is the contract stated outright, so the check is set
/// membership on parsed root fields: no library, file-name or URL convention.
///
/// "Tracked" is git's index, so a printed schema a developer generated locally
/// and never committed is not an identity. When git cannot answer (the tree is
/// not a repository), every schema file on disk outside dependency folders
/// counts instead.
///
/// User-facing description: README, "GraphQL documents for another team's
/// API".
///
/// Built once per scan. The tally records what each service's attribution
/// removed, for the report.
#[derive(Debug, Default)]
pub struct SchemaCatalogue {
    schemas: Vec<KnownSchema>,
    tally: std::sync::Mutex<BTreeMap<String, AttributionSummary>>,
}

/// What a document's file says about where it is sent: its environment reads
/// ([`file_env_reads`]) classified by `internalEnvVars` / `externalEnvVars`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportOrigin {
    Internal,
    External,
    /// No classified read, or reads that disagree.
    Unknown,
}

/// Every environment variable `file_path` reads, in any spelling
/// [`crate::env_alias::env_read_name`] recognizes (`process.env.NAME`,
/// `import.meta.env.NAME`, `Deno.env.get("NAME")`). Empty for a file that does
/// not parse or is not a script, such as a `.graphql` file.
///
/// Used only as the transport tie-break of [`SchemaCatalogue::attribute`], so
/// the file is parsed on demand rather than on every scan of it.
pub fn file_env_reads(file_path: &Path) -> BTreeSet<String> {
    let is_script = file_path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            matches!(
                ext,
                "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "mjs" | "cjs"
            )
        });
    if !is_script {
        return BTreeSet::new();
    }
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));
    let globals = Globals::new();
    GLOBALS.set(&globals, || {
        let Some(module) = parse_file(file_path, &cm, &handler) else {
            return BTreeSet::new();
        };
        let mut reads = EnvReads::default();
        module.visit_with(&mut reads);
        reads.names
    })
}

/// Collects the names [`file_env_reads`] returns.
#[derive(Default)]
struct EnvReads {
    names: BTreeSet<String>,
}

impl Visit for EnvReads {
    fn visit_expr(&mut self, node: &Expr) {
        if let Some(name) = crate::env_alias::env_read_name(node) {
            self.names.insert(name);
        }
        node.visit_children_with(self);
    }
}

/// The schema identity one document was attributed to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentIdentity {
    /// A served schema covers it: its operations are calls to this project.
    Served,
    /// None of its fields is a root field of any schema this repository
    /// holds. The server may be another repository in the project, so its
    /// operations stay calls and matching decides.
    NoLocalSchema,
    /// Only schemas no service serves cover it (the files, sorted).
    External(Vec<PathBuf>),
    /// No single schema covers the fields it shares with known schemas, or
    /// both a served and an external schema cover them and the transport does
    /// not settle which.
    Unresolved,
}

/// Per-document attribution for one service's consumers, from
/// [`SchemaCatalogue::attribute`].
#[derive(Debug, Clone, Default)]
pub struct ConsumerAttribution {
    documents: HashMap<(PathBuf, u32), DocumentIdentity>,
}

/// What [`ConsumerAttribution::apply`] removed from a service's consumers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttributionSummary {
    /// Operations attributed to external schemas, per sorted schema file set.
    pub external: BTreeMap<Vec<PathBuf>, usize>,
    /// Operations whose document is [`DocumentIdentity::Unresolved`].
    pub unresolved: usize,
}

impl AttributionSummary {
    pub fn is_empty(&self) -> bool {
        self.external.is_empty() && self.unresolved == 0
    }
}

/// Repository-relative form of `path`, with any leading `./` dropped.
fn repo_relative(repo_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .components()
        .skip_while(|c| matches!(c, std::path::Component::CurDir))
        .collect()
}

/// Every GraphQL file on disk under `repo_root`, skipping `.git` and
/// dependency folders only: the fallback when git cannot say what is tracked.
/// Build folders are walked, because a committed printed schema usually sits
/// in one.
fn graphql_files_on_disk(repo_root: &Path) -> Vec<PathBuf> {
    WalkDir::new(repo_root)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || !e
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name == ".git" || name == "node_modules")
        })
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file() && is_graphql_file(entry.path()))
        .map(|entry| entry.path().to_path_buf())
        .collect()
}

impl SchemaCatalogue {
    /// Read every schema file the repository holds and mark each one served
    /// or external. `services` are this scan's services.
    pub fn build(repo_root: &Path, services: &[ServedSchemaSources]) -> Self {
        let mut served: BTreeSet<PathBuf> = BTreeSet::new();
        for service in services {
            for path in graphql_files_under(&service.roots)
                .iter()
                .chain(&service.declared)
            {
                served.insert(repo_relative(repo_root, path));
            }
        }
        let tracked: BTreeSet<PathBuf> = match crate::git_state::tracked_paths(
            repo_root,
            &["*.graphql", "*.gql"],
        ) {
            Ok(paths) => paths
                .iter()
                .map(|path| repo_relative(repo_root, Path::new(path)))
                .collect(),
            Err(reason) => {
                debug!(
                    "GraphQL schema catalogue: git cannot list tracked files ({reason}); reading every schema file on disk"
                );
                graphql_files_on_disk(repo_root)
                    .iter()
                    .map(|path| repo_relative(repo_root, path))
                    .collect()
            }
        };
        let schemas: Vec<KnownSchema> = served
            .iter()
            .chain(tracked.difference(&served))
            .filter_map(|file| {
                let content = std::fs::read_to_string(repo_root.join(file)).ok()?;
                let root_keys: HashSet<String> = extract_from_document_text(&content, file, 1)
                    .producers
                    .iter()
                    .map(|op| op.key.canonical())
                    .collect();
                if root_keys.is_empty() {
                    return None;
                }
                let origin = if served.contains(file) {
                    SchemaOrigin::Served
                } else {
                    SchemaOrigin::External
                };
                Some(KnownSchema {
                    file: file.clone(),
                    origin,
                    root_keys,
                })
            })
            .collect();
        debug!(
            served = schemas
                .iter()
                .filter(|s| s.origin == SchemaOrigin::Served)
                .count(),
            external = schemas
                .iter()
                .filter(|s| s.origin == SchemaOrigin::External)
                .count(),
            "GraphQL schema catalogue built"
        );
        Self {
            schemas,
            tally: Default::default(),
        }
    }

    /// Attribute each of `extraction`'s documents to a schema identity.
    ///
    /// A document's attribution basis is the set of its root fields that are a
    /// root field of SOME known schema. A field no schema has is left out of
    /// the basis rather than failing the document, so a document against the
    /// project's own schema that uses a field the server has since removed is
    /// still attributed to that schema and the removed field still reads as a
    /// missing operation. The candidates are the schemas holding the whole
    /// basis; the service's own producers (including schema text in its source
    /// files) are one more served candidate. Then:
    ///
    /// - empty basis: [`DocumentIdentity::NoLocalSchema`];
    /// - served candidates only: [`DocumentIdentity::Served`];
    /// - external candidates only: [`DocumentIdentity::External`];
    /// - both: `transport(file)` decides, and without an answer the document
    ///   is [`DocumentIdentity::Unresolved`];
    /// - no candidate (the basis spans schemas): [`DocumentIdentity::Unresolved`].
    ///
    /// The transport is a same-file HTTP call, so a `.graphql` file, which has
    /// none, is unresolved whenever the tie-break is needed.
    pub fn attribute(
        &self,
        extraction: &GraphqlExtraction,
        transport: impl Fn(&Path) -> TransportOrigin,
    ) -> ConsumerAttribution {
        let own: HashSet<String> = extraction
            .producers
            .iter()
            .map(|op| op.key.canonical())
            .collect();
        let mut fields_by_document: BTreeMap<(PathBuf, u32), BTreeSet<String>> = BTreeMap::new();
        for op in &extraction.consumers {
            fields_by_document
                .entry((op.file_path.clone(), op.document_line))
                .or_default()
                .insert(op.key.canonical());
        }
        let mut documents = HashMap::new();
        for (document, fields) in fields_by_document {
            let basis: Vec<&String> = fields
                .iter()
                .filter(|key| {
                    own.contains(*key) || self.schemas.iter().any(|s| s.root_keys.contains(*key))
                })
                .collect();
            let identity = if basis.is_empty() {
                DocumentIdentity::NoLocalSchema
            } else {
                let covers = |keys: &HashSet<String>| basis.iter().all(|key| keys.contains(*key));
                let served = covers(&own)
                    || self
                        .schemas
                        .iter()
                        .any(|s| s.origin == SchemaOrigin::Served && covers(&s.root_keys));
                let external: Vec<PathBuf> = self
                    .schemas
                    .iter()
                    .filter(|s| s.origin == SchemaOrigin::External && covers(&s.root_keys))
                    .map(|s| s.file.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                match (served, external.is_empty()) {
                    (true, true) => DocumentIdentity::Served,
                    (false, false) => DocumentIdentity::External(external),
                    (false, true) => DocumentIdentity::Unresolved,
                    (true, false) => match transport(&document.0) {
                        TransportOrigin::Internal => DocumentIdentity::Served,
                        TransportOrigin::External => DocumentIdentity::External(external),
                        TransportOrigin::Unknown => DocumentIdentity::Unresolved,
                    },
                }
            };
            documents.insert(document, identity);
        }
        ConsumerAttribution { documents }
    }

    /// Record what `service`'s attribution removed, replacing an earlier
    /// record for the same service (a retried service is analysed twice).
    pub fn record(&self, service: &str, summary: AttributionSummary) {
        let mut tally = self.tally.lock().unwrap_or_else(|e| e.into_inner());
        if summary.is_empty() {
            tally.remove(service);
        } else {
            tally.insert(service.to_string(), summary);
        }
    }

    /// The report lines for every service whose attribution removed an
    /// operation, in service order.
    pub fn notices(&self) -> Vec<String> {
        let tally = self.tally.lock().unwrap_or_else(|e| e.into_inner());
        let mut lines = Vec::new();
        for (service, summary) in tally.iter() {
            for (files, count) in &summary.external {
                let files = files
                    .iter()
                    .map(|file| format!("'{}'", file.display()))
                    .collect::<Vec<_>>()
                    .join(" or ");
                lines.push(format!(
                    "Service '{service}': {count} GraphQL document operation(s) are written \
                     against {files}, which no service in this repository serves, so they are \
                     read as calls to an external API and not indexed. If a service here serves \
                     that schema, name the file in its `graphqlSchemas`."
                ));
            }
            if summary.unresolved > 0 {
                lines.push(format!(
                    "Service '{service}': {} GraphQL document operation(s) are not indexed \
                     because no single schema holds all their fields, or both a served and an \
                     external schema do and the call's base URL does not say which.",
                    summary.unresolved
                ));
            }
        }
        lines
    }
}

impl ConsumerAttribution {
    /// The identity of the document `op` was parsed from.
    pub fn identity(&self, op: &GraphqlOp) -> Option<&DocumentIdentity> {
        self.documents
            .get(&(op.file_path.clone(), op.document_line))
    }

    /// Remove every consumer whose document is external or unresolved, bind
    /// every kept consumer to its identity, and say how many were removed.
    pub fn apply(&self, extraction: &mut GraphqlExtraction) -> AttributionSummary {
        let mut summary = AttributionSummary::default();
        extraction.consumers.retain_mut(|op| {
            let binding = match self.identity(op) {
                Some(DocumentIdentity::External(files)) => {
                    *summary.external.entry(files.clone()).or_default() += 1;
                    return false;
                }
                Some(DocumentIdentity::Unresolved) => {
                    summary.unresolved += 1;
                    return false;
                }
                Some(DocumentIdentity::Served) => Some(SchemaBinding::Served),
                Some(DocumentIdentity::NoLocalSchema) => Some(SchemaBinding::NoLocalSchema),
                None => None,
            };
            op.schema_binding = binding;
            true
        });
        summary
    }
}

/// What the scan report says about one service's GraphQL schema coverage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphqlNotices {
    /// A `graphqlSchemas` declaration that did nothing. Printed as a warning.
    pub warnings: Vec<String>,
    /// The code-first hint: a service that uses GraphQL and serves routes but
    /// indexes no schema field and declares no schema file.
    pub hints: Vec<String>,
}

/// The facts [`service_notices`] decides from, gathered by the engine once a
/// service's rows are built.
#[derive(Debug, Clone, Copy)]
pub struct GraphqlServiceFacts<'a> {
    /// The service name as the report prints it.
    pub service: &'a str,
    /// Whether the service's config has any `graphqlSchemas` entry.
    pub declares_schemas: bool,
    /// What those entries resolved to.
    pub declared: &'a DeclaredSchemas,
    /// GraphQL producer rows the service indexed, from any source.
    pub producers: usize,
    /// Whether the service indexed at least one HTTP route. A GraphQL server is
    /// served on one, and a client-only app (whose GraphQL library is just as
    /// present) usually serves none, so this keeps the hint off the clients.
    pub serves_http: bool,
    /// GraphQL libraries the service depends on or was detected using.
    pub graphql_libraries: &'a [String],
}

/// Decide the per-service GraphQL lines for the scan report (carrick#1099).
///
/// The hint is gated on THIS service having no producer row, not on the scan
/// having no GraphQL row at all: a code-first server that also sends documents
/// to someone else's GraphQL API has consumer rows and still indexes none of
/// the operations it serves.
pub fn service_notices(facts: GraphqlServiceFacts<'_>) -> GraphqlNotices {
    let mut notices = GraphqlNotices::default();
    for problem in &facts.declared.problems {
        notices
            .warnings
            .push(format!("Service '{}': {problem}", facts.service));
    }
    if !facts.declares_schemas
        && facts.producers == 0
        && facts.serves_http
        && !facts.graphql_libraries.is_empty()
    {
        let mut libraries: Vec<&str> = facts.graphql_libraries.iter().map(String::as_str).collect();
        libraries.sort_unstable();
        libraries.dedup();
        let libraries = libraries
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        notices.hints.push(format!(
            "Service '{}' uses GraphQL ({libraries}) and serves HTTP routes, but indexes no \
             GraphQL schema fields. If its schema is built in code, name the printed SDL file \
             in `graphqlSchemas` for this service in carrick.json.",
            facts.service
        ));
    }
    notices
}

/// Extract operations from raw GraphQL text. Tries SDL first (producers),
/// then executable-document parsing (consumers). `base_line` is the 1-based
/// line of the text's first line in its host file, so tagged-template
/// contents report host-file line numbers.
pub fn extract_from_document_text(
    text: &str,
    file_path: &Path,
    base_line: u32,
) -> GraphqlExtraction {
    let mut extraction = GraphqlExtraction::default();
    let to_line = |pos_line: usize| base_line.saturating_add(pos_line.saturating_sub(1) as u32);

    if let Ok(schema) = graphql_parser::parse_schema::<String>(text) {
        use graphql_parser::schema::{Definition, TypeDefinition, TypeExtension};

        // Root operation type names default to Query/Mutation/Subscription
        // but can be remapped by an explicit `schema { ... }` definition.
        let mut roots: Vec<(String, GraphqlOperationKind)> = vec![
            ("Query".to_string(), GraphqlOperationKind::Query),
            ("Mutation".to_string(), GraphqlOperationKind::Mutation),
            (
                "Subscription".to_string(),
                GraphqlOperationKind::Subscription,
            ),
        ];
        let mut has_type_system_definitions = false;

        for definition in &schema.definitions {
            if let Definition::SchemaDefinition(schema_def) = definition {
                has_type_system_definitions = true;
                roots.clear();
                if let Some(name) = &schema_def.query {
                    roots.push((name.clone(), GraphqlOperationKind::Query));
                }
                if let Some(name) = &schema_def.mutation {
                    roots.push((name.clone(), GraphqlOperationKind::Mutation));
                }
                if let Some(name) = &schema_def.subscription {
                    roots.push((name.clone(), GraphqlOperationKind::Subscription));
                }
            }
        }

        for definition in &schema.definitions {
            let (name, fields) = match definition {
                Definition::TypeDefinition(TypeDefinition::Object(obj)) => {
                    has_type_system_definitions = true;
                    (&obj.name, &obj.fields)
                }
                Definition::TypeExtension(TypeExtension::Object(ext)) => {
                    has_type_system_definitions = true;
                    (&ext.name, &ext.fields)
                }
                Definition::TypeDefinition(_)
                | Definition::TypeExtension(_)
                | Definition::DirectiveDefinition(_) => {
                    has_type_system_definitions = true;
                    continue;
                }
                Definition::SchemaDefinition(_) => continue,
            };
            let Some((_, kind)) = roots.iter().find(|(root, _)| root == name) else {
                continue;
            };
            for field in fields {
                extraction.producers.push(GraphqlOp {
                    key: OperationKey::graphql(*kind, field.name.clone()),
                    file_path: file_path.to_path_buf(),
                    line: to_line(field.position.line),
                    document_line: base_line,
                    // Deterministic anchor: the root field's SDL type
                    // expression (e.g. `Order`, `Order!`, `[Order!]!`).
                    primary_type_symbol: Some(render_sdl_type(&field.field_type)),
                    // Producers carry no consumer-side bound type.
                    payload_type_symbol: None,
                    payload_type_source: None,
                    // SDL alone has no resolver location; the file-analyzer's
                    // graphql_operations fill these in the engine merge (Stage B1).
                    resolver_file: None,
                    resolver_line: None,
                    // Populated in the engine merge only when the LLM located a
                    // co-located backing type for a resolver-less field (#248).
                    response_type_symbol: None,
                    response_type_source: None,
                    // Producer-only fallback's consumer counterpart; never set
                    // on producers.
                    consumer_located_type_symbol: None,
                    consumer_located_type_source: None,
                    schema_binding: None,
                });
            }
        }

        // SDL parsed and contained type-system definitions: this text is a
        // schema, not an executable document — done, even if no root fields
        // were found (e.g. a file defining only `type User`).
        if has_type_system_definitions {
            return extraction;
        }
    }

    if let Ok(document) = graphql_parser::parse_query::<String>(text) {
        use graphql_parser::query::{Definition, OperationDefinition, Selection};

        for definition in &document.definitions {
            let Definition::Operation(operation) = definition else {
                continue; // standalone fragments carry no operation identity
            };
            let (kind, selection_set) = match operation {
                // `{ user }` shorthand is an anonymous query
                OperationDefinition::SelectionSet(set) => (GraphqlOperationKind::Query, set),
                OperationDefinition::Query(q) => (GraphqlOperationKind::Query, &q.selection_set),
                OperationDefinition::Mutation(m) => {
                    (GraphqlOperationKind::Mutation, &m.selection_set)
                }
                OperationDefinition::Subscription(s) => {
                    (GraphqlOperationKind::Subscription, &s.selection_set)
                }
            };
            for selection in &selection_set.items {
                // Top-level fragment spreads can't be resolved without the
                // fragment source (often interpolated) — skip, never guess.
                let Selection::Field(field) = selection else {
                    continue;
                };
                if field.name.starts_with("__") {
                    continue; // introspection
                }
                extraction.consumers.push(GraphqlOp {
                    // alias-aware: match on the real field name, not the alias
                    key: OperationKey::graphql(kind, field.name.clone()),
                    file_path: file_path.to_path_buf(),
                    line: to_line(field.position.line),
                    document_line: base_line,
                    // Executable documents carry no SDL type — the SDL-derived
                    // anchor stays unset. The consumer's real TS result type is
                    // captured separately at the `client.request<T>(DOC)` call
                    // site (see `payload_type_symbol`), populated by the TS-file
                    // pass below; SDL-text parsing has no call site, so it stays
                    // `None` here.
                    primary_type_symbol: None,
                    payload_type_symbol: None,
                    payload_type_source: None,
                    // Consumers never carry a resolver location.
                    resolver_file: None,
                    resolver_line: None,
                    // Producer-only fallback; never set on consumers.
                    response_type_symbol: None,
                    response_type_source: None,
                    // Populated in the engine merge only when the LLM located a
                    // co-located result type for a document with no explicit
                    // call-site generic (#268). The TS-file pass below still
                    // gets first shot via `payload_type_symbol`.
                    consumer_located_type_symbol: None,
                    consumer_located_type_source: None,
                    schema_binding: None,
                });
            }
        }
    }

    extraction
}

/// Render an SDL field type to its canonical GraphQL type expression
/// (`Order`, `Order!`, `[Order!]!`). This is the deterministic producer anchor
/// (#248): it travels straight from the parsed schema with no resolver mapping,
/// so it works for any schema-first SDL regardless of the server framework.
fn render_sdl_type(ty: &graphql_parser::schema::Type<'_, String>) -> String {
    use graphql_parser::schema::Type;
    match ty {
        Type::NamedType(name) => name.clone(),
        Type::ListType(inner) => format!("[{}]", render_sdl_type(inner)),
        Type::NonNullType(inner) => format!("{}!", render_sdl_type(inner)),
    }
}

/// List-nesting depth of a rendered SDL type expression (#248): `[Order!]!` → 1,
/// `[[Order!]!]!` → 2, `Order`/`Order!` → 0. Non-null (`!`) markers do not add
/// depth. Used to wrap a resolver-less field's bundled element type in the right
/// number of TS array levels (`Order` → `Order[]` for `[Order!]!`) so the
/// producer's response contract matches the SDL list shape.
pub fn graphql_list_depth(rendered_sdl_type: &str) -> u32 {
    rendered_sdl_type.chars().take_while(|c| *c == '[').count() as u32
}

/// Join a `gql`/`graphql` tagged template's literal parts, dropping
/// interpolations (interpolated fragments leave unresolved spreads that still
/// parse; an interpolation mid-token breaks the parse and the document is
/// skipped silently downstream).
fn tagged_tpl_text(node: &TaggedTpl) -> String {
    node.tpl
        .quasis
        .iter()
        .map(|quasi| {
            quasi
                .cooked
                .as_ref()
                .map(|c| c.to_string())
                .unwrap_or_else(|| quasi.raw.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The single operation key of an already-extracted `gql` document, when it has
/// exactly one operation-field consumer (the `const NAME = gql\`...\`` shape the
/// request call site binds by ident). Returns `None` for SDL, multi-field, or
/// empty extractions — those don't map cleanly to one request binding. Operates
/// on the merged extraction the tagged-template handler already produced, so the
/// document text is parsed only once.
fn single_operation_key(extraction: &GraphqlExtraction) -> Option<&OperationKey> {
    if !extraction.producers.is_empty() || extraction.consumers.len() != 1 {
        return None;
    }
    extraction.consumers.first().map(|op| &op.key)
}

/// Extract operations from `gql`/`graphql` tagged template literals in a
/// TypeScript/JavaScript file, and recover the consumer's bound TS result type
/// from `client.request<T>(DOC)` call sites (the consumer anchor the SDL path
/// can't provide).
fn extract_from_ts_file(file_path: &Path) -> GraphqlExtraction {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));

    let globals = Globals::new();
    GLOBALS.set(&globals, || {
        let Some(module) = parse_file(file_path, &cm, &handler) else {
            return GraphqlExtraction::default();
        };

        let mut visitor = TaggedTplVisitor {
            cm: cm.clone(),
            file_path,
            extraction: GraphqlExtraction::default(),
            type_imports: HashMap::new(),
            gql_const_key: HashMap::new(),
            request_key_types: HashMap::new(),
            pending_gql_binding: None,
        };
        module.visit_with(&mut visitor);

        // Backfill consumer anchors: for each consumer op whose operation key had
        // a typed `request<T>(DOC)` call site, set the captured symbol. Matching on
        // the full canonical key (kind + field) keeps a query and a mutation that
        // share a field name from cross-anchoring.
        let TaggedTplVisitor {
            mut extraction,
            request_key_types,
            ..
        } = visitor;
        for op in &mut extraction.consumers {
            if op.key.graphql_field().is_some()
                && let Some((symbol, source)) = request_key_types.get(&op.key.canonical())
            {
                op.payload_type_symbol = Some(symbol.clone());
                op.payload_type_source = source.clone();
            }
        }
        extraction
    })
}

/// Map every tracked `const NAME = gql\`...\`` document binding in `file_path`
/// to its single operation's canonical key (`TICKET_QUERY → graphql|query|ticket`).
///
/// This is the deterministic document-identity index the file-analyzer
/// post-processor uses to repair a `client.request(DOC)` data call whose target
/// the model reported as the shared transport URL instead of the operation: the
/// call text names the document binding, and this map turns that binding into
/// the exact canonical key the operation matcher joins on. Reuses the same
/// single-parse `gql_const_key` the anchor path already builds, so there is no
/// second document parse and no separate identity notion to drift.
///
/// Returns an empty map when the file has no tracked single-operation documents
/// (a multi-field document or SDL never enters `gql_const_key`, so it is never a
/// rewrite target — we only ever rewrite to an unambiguous operation key).
pub fn document_operation_keys(file_path: &Path) -> HashMap<String, String> {
    let cm: Lrc<SourceMap> = Default::default();
    let handler = Handler::with_tty_emitter(ColorConfig::Never, false, false, Some(cm.clone()));

    let globals = Globals::new();
    GLOBALS.set(&globals, || {
        let Some(module) = parse_file(file_path, &cm, &handler) else {
            return HashMap::new();
        };

        let mut visitor = TaggedTplVisitor {
            cm: cm.clone(),
            file_path,
            extraction: GraphqlExtraction::default(),
            type_imports: HashMap::new(),
            gql_const_key: HashMap::new(),
            request_key_types: HashMap::new(),
            pending_gql_binding: None,
        };
        module.visit_with(&mut visitor);
        visitor.gql_const_key
    })
}

struct TaggedTplVisitor<'a> {
    cm: Lrc<SourceMap>,
    file_path: &'a Path,
    extraction: GraphqlExtraction,
    /// Named-import local name → module specifier, so a consumer's bound result
    /// type imported as a named symbol can be anchored (copy of the socket
    /// `type_imports` pattern).
    type_imports: HashMap<String, String>,
    /// `gql`/`graphql` const binding ident → the document's single operation
    /// key in canonical form (`GET_ORDER` → `graphql|query|order`). Keying by the
    /// canonical key (kind + field), not the bare field, keeps a `query` and a
    /// `mutation` that share one field name in the same file from colliding. The
    /// request call site binds the document by this ident, so this joins the call
    /// site's type to the consumer op.
    gql_const_key: HashMap<String, String>,
    /// Canonical operation key → `(bound type symbol, import source)` recovered
    /// from a `request<T>(DOC)` call site. Filled in `visit_call_expr` once both
    /// the gql-const binding and the call site are known. Keyed by canonical key
    /// (not bare field) so it matches the consumer op's `key.canonical()` exactly.
    request_key_types: HashMap<String, (String, Option<String>)>,
    /// Binding ident of the `const NAME = gql\`...\`` declarator currently being
    /// visited, so the `visit_tagged_tpl` handler — which already parses the
    /// document to build the consumer op — can record `NAME → operation key` from
    /// that single parse instead of parsing the text a second time.
    pending_gql_binding: Option<String>,
}

impl TaggedTplVisitor<'_> {
    /// Capture a `receiver.method<T>(DOC)`-style GraphQL execution call. The
    /// capture is keyed entirely on a structural triad, with NO method-name
    /// allowlist and NO client-identifier allowlist, so it is framework-agnostic:
    ///   (a) it is a member call (`obj.method(...)`);
    ///   (b) it carries an explicit TS type generic (`method<OrderView>(...)`);
    ///   (c) its first positional argument is an ident bound to a tracked `gql`
    ///       document const (looked up in `gql_const_key`).
    /// Those three together identify a GraphQL execution by construction — the
    /// method name (`request`/`query`/`exec`/…) adds nothing over "first arg is a
    /// known gql document", so it is not inspected. The generic + known-gql-const
    /// requirements keep precision: a plain `foo.map<T>(x)` is never captured
    /// because `x` is not a tracked gql document.
    fn capture_request_call(&mut self, node: &CallExpr) {
        let Callee::Expr(callee) = &node.callee else {
            return;
        };
        let Expr::Member(member) = &**callee else {
            return;
        };
        // The call must be a member call (`obj.method(...)`) — but the method
        // name itself is intentionally NOT inspected (no allowlist).
        if member.prop.as_ident().is_none() {
            return;
        }
        // Must carry an explicit TS type argument (`request<OrderView>(...)`).
        let Some(type_args) = node.type_args.as_ref() else {
            return;
        };
        let Some(type_arg) = type_args.params.first() else {
            return;
        };
        // First argument must be a bare ident bound to a recorded gql document.
        let Some(first) = node.args.first() else {
            return;
        };
        let Expr::Ident(doc_ident) = &*first.expr else {
            return;
        };
        let Some(canonical_key) = self.gql_const_key.get(doc_ident.sym.as_ref()) else {
            return;
        };
        let canonical_key = canonical_key.clone();
        // The canonical key is `graphql|<kind>|<field>`; the wrapper-unwrap rule
        // matches the single property name against the operation field, so pull
        // the field back out of the key (last `|`-segment).
        let field = canonical_key.rsplit('|').next().unwrap_or("").to_string();

        // Unwrap `{ <field>: T }` to T when the single property name matches the
        // operation field (`{ order: OrderView }` for the `order` op); otherwise
        // the result is `None` (see `resolve_request_type_arg`). Keyed on the
        // parsed gql field name, never a hardcoded list.
        let resolved = resolve_request_type_arg(type_arg, &field);
        if let Some(symbol) = resolved {
            let source = self.type_imports.get(&symbol).cloned();
            self.request_key_types
                .insert(canonical_key, (symbol, source));
        }
    }
}

impl Visit for TaggedTplVisitor<'_> {
    fn visit_import_decl(&mut self, node: &ImportDecl) {
        // Record every named import's local name → module specifier (copy of the
        // socket pattern) so an imported result type can carry its source.
        let source = node.src.value.as_ref();
        for specifier in &node.specifiers {
            if let ImportSpecifier::Named(named) = specifier {
                self.type_imports
                    .insert(named.local.sym.to_string(), source.to_string());
            }
        }
    }

    fn visit_var_declarator(&mut self, node: &VarDeclarator) {
        // `const NAME = gql\`...\`` — stash the binding ident so the child
        // `TaggedTpl` (which parses the document anyway) records `NAME → key`
        // from that one parse, rather than parsing the text a second time here.
        let mut stashed = false;
        if let (swc_ecma_ast::Pat::Ident(binding), Some(init)) = (&node.name, node.init.as_deref())
            && let Expr::TaggedTpl(tpl) = init
            && let Expr::Ident(tag) = &*tpl.tag
            && matches!(tag.sym.as_ref(), "gql" | "graphql")
        {
            self.pending_gql_binding = Some(binding.id.sym.to_string());
            stashed = true;
        }
        node.visit_children_with(self);
        if stashed {
            // Clear in case the document had no single operation key (multi-field
            // or SDL): the binding must not leak onto a later tagged template.
            self.pending_gql_binding = None;
        }
    }

    fn visit_tagged_tpl(&mut self, node: &TaggedTpl) {
        if let Expr::Ident(tag) = &*node.tag
            && matches!(tag.sym.as_ref(), "gql" | "graphql")
        {
            let text = tagged_tpl_text(node);
            let base_line = self.cm.lookup_char_pos(node.span().lo).line as u32;
            let parsed = extract_from_document_text(&text, self.file_path, base_line);
            // Reuse this single parse to record the const→key association for the
            // enclosing `const NAME = gql\`...\`` declarator (set in
            // `visit_var_declarator`), so the document is parsed only once.
            if let Some(binding) = self.pending_gql_binding.take()
                && let Some(key) = single_operation_key(&parsed)
            {
                self.gql_const_key.insert(binding, key.canonical());
            }
            self.extraction.merge(parsed);
        }
        node.visit_children_with(self);
    }

    fn visit_call_expr(&mut self, node: &CallExpr) {
        self.capture_request_call(node);
        node.visit_children_with(self);
    }
}

/// Resolve the TS type argument of a `request<T>(DOC)` call to a single anchor
/// symbol. `request<OrderView>` → `OrderView`; `request<{ order: OrderView }>`
/// with `field == "order"` unwraps to `OrderView`. Anything we can't anchor to a
/// single named symbol returns `None` and the consumer stays unanchored — we do
/// not guess. That includes: a single-property wrapper whose key does NOT match
/// the operation field (the whole literal would be the bound type, but a literal
/// has no single symbol name); type args that aren't a bare named reference or a
/// matching single-field envelope; and built-in/primitive references. Precision
/// over recall.
fn resolve_request_type_arg(type_arg: &TsType, field: &str) -> Option<String> {
    match type_arg {
        // `request<OrderView>` — a bare named reference.
        TsType::TsTypeRef(_) => named_type_symbol_of(type_arg),
        // `request<{ order: OrderView }>` — single-property object literal.
        TsType::TsTypeLit(lit) => {
            let mut props = lit.members.iter().filter_map(|member| match member {
                TsTypeElement::TsPropertySignature(prop) => Some(prop),
                _ => None,
            });
            let prop = props.next()?;
            // Exactly one property, or we can't tell which is the envelope.
            if props.next().is_some() {
                return None;
            }
            let prop_name = match &*prop.key {
                Expr::Ident(ident) => ident.sym.to_string(),
                Expr::Lit(swc_ecma_ast::Lit::Str(s)) => s.value.to_string(),
                _ => return None,
            };
            let inner = prop.type_ann.as_ref()?;
            if prop_name == field {
                // The single property IS the operation envelope — unwrap to T.
                named_type_symbol_of(&inner.type_ann)
            } else {
                // The single property is NOT the operation field — the whole
                // literal is the bound type, but a literal has no single symbol
                // name to anchor on, so leave it unanchored (precision).
                None
            }
        }
        _ => None,
    }
}

/// Bare symbol name of a simple named type reference (`OrderView` from a
/// `TsTypeRef`), reusing the socket precision rules: only an unqualified,
/// non-generic, non-builtin reference yields a symbol.
fn named_type_symbol_of(ty: &TsType) -> Option<String> {
    match ty {
        TsType::TsTypeRef(type_ref) if type_ref.type_params.is_none() => {
            match &type_ref.type_name {
                TsEntityName::Ident(ident) => {
                    let name = ident.sym.to_string();
                    if is_builtin_type(&name) {
                        None
                    } else {
                        Some(name)
                    }
                }
                TsEntityName::TsQualifiedName(_) => None,
            }
        }
        _ => None,
    }
}

/// Lowercase/well-known TS types that must never be treated as a resolvable
/// payload anchor (mirror of `socket_io::is_builtin_type`).
fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "any"
            | "unknown"
            | "never"
            | "void"
            | "object"
            | "string"
            | "number"
            | "boolean"
            | "bigint"
            | "symbol"
            | "undefined"
            | "null"
            | "Array"
            | "Promise"
            | "Record"
            | "Map"
            | "Set"
            | "Date"
            | "Object"
            | "String"
            | "Number"
            | "Boolean"
            | "Symbol"
            | "BigInt"
            | "Function"
            | "RegExp"
            | "Error"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(ops: &[GraphqlOp]) -> Vec<String> {
        let mut keys: Vec<String> = ops.iter().map(|op| op.key.canonical()).collect();
        keys.sort();
        keys
    }

    #[test]
    fn vite_artifacts_are_excluded_from_schema_discovery() {
        let repo = tempfile::tempdir().unwrap();
        for directory in ["src", ".vite/deps", "src/.vite/deps"] {
            let dir = repo.path().join(directory);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("schema.graphql"), "type Query { item: String }").unwrap();
        }
        let extraction = scan_repo(&[repo.path().to_path_buf()], &[], &[]);
        assert_eq!(extraction.producers.len(), 1);
        assert_eq!(
            extraction.producers[0].file_path,
            repo.path().join("src/schema.graphql")
        );
    }

    /// `(canonical_key, primary_type_symbol)` pairs, sorted, for asserting the
    /// deterministic anchor derived for each producer.
    fn anchors(ops: &[GraphqlOp]) -> Vec<(String, Option<String>)> {
        let mut pairs: Vec<(String, Option<String>)> = ops
            .iter()
            .map(|op| (op.key.canonical(), op.primary_type_symbol.clone()))
            .collect();
        pairs.sort();
        pairs
    }

    /// #248: an SDL producer's deterministic anchor is the root field's SDL type
    /// expression — bare (`Order`), non-null (`Order!`), and list
    /// (`[Order!]!`) forms all render canonically, with no resolver mapping.
    #[test]
    fn sdl_producers_anchor_on_their_field_type_expression() {
        let sdl = r#"
            type Order { id: ID! }
            type Query {
                order(id: ID!): Order
                orders: [Order!]!
            }
            type Mutation {
                refundOrder(id: ID!): Order!
            }
            type Subscription {
                orderUpdated: Order!
            }
        "#;
        let result = extract_from_document_text(sdl, Path::new("schema.graphql"), 1);
        assert_eq!(
            anchors(&result.producers),
            vec![
                (
                    "graphql|mutation|refundOrder".to_string(),
                    Some("Order!".to_string())
                ),
                ("graphql|query|order".to_string(), Some("Order".to_string())),
                (
                    "graphql|query|orders".to_string(),
                    Some("[Order!]!".to_string())
                ),
                (
                    "graphql|subscription|orderUpdated".to_string(),
                    Some("Order!".to_string())
                ),
            ]
        );
    }

    /// Stage B2: a producer op formats as `"{kind} {field}: {sdl_type}"`, the
    /// exact line shape injected into the file-analyzer's GRAPHQL SCHEMA
    /// PRODUCERS context block.
    #[test]
    fn producer_hint_lines_format_kind_field_and_sdl_type() {
        let sdl = r#"
            type Order { id: ID! }
            type Query {
                order(id: ID!): Order
                orders: [Order!]!
            }
            type Mutation { refundOrder(id: ID!): Order! }
        "#;
        let extraction = extract_from_document_text(sdl, Path::new("schema.graphql"), 1);
        let mut lines: Vec<String> = extraction
            .producers
            .iter()
            .filter_map(GraphqlProducerHints::format_producer)
            .collect();
        lines.sort();
        assert_eq!(
            lines,
            vec![
                "mutation refundOrder: Order!".to_string(),
                "query order: Order".to_string(),
                "query orders: [Order!]!".to_string(),
            ]
        );
    }

    /// `file_within_scan_roots` gates the don't-skip routing: only files under an
    /// SDL scan root (schema-co-located) are eligible, so a resolver in the
    /// schema package is routed while an unrelated exported-function file is not.
    #[test]
    fn file_within_scan_roots_matches_only_co_located_files() {
        let hints = GraphqlProducerHints {
            lines: vec!["query order: Order".to_string()],
            scan_roots: vec![PathBuf::from("/repo/services/orders")],
            declared_lines: vec![],
        };
        assert!(hints.file_within_scan_roots(Path::new("/repo/services/orders/src/resolvers.ts")));
        assert!(!hints.file_within_scan_roots(Path::new("/repo/services/billing/src/handlers.ts")));
        assert!(
            !GraphqlProducerHints::default()
                .file_within_scan_roots(Path::new("/repo/services/orders/src/resolvers.ts"))
        );
    }

    /// End-to-end of the deterministic hint builder: a real SDL file under the
    /// scan root yields formatted producer lines, and a co-located file is
    /// recognised by the routing gate. A root with no SDL yields empty hints.
    #[test]
    fn collect_builds_hints_from_sdl_under_scan_root() {
        let dir = std::env::temp_dir().join(format!(
            "carrick-gql-hints-{}-{:016x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("schema.graphql"),
            "type Order { id: ID! }\ntype Query { order(id: ID!): Order }\n",
        )
        .unwrap();

        let hints = GraphqlProducerHints::collect(vec![dir.clone()], &[], &[]);
        assert_eq!(hints.lines, vec!["query order: Order".to_string()]);
        assert!(!hints.is_empty());
        assert!(hints.file_within_scan_roots(&dir.join("resolvers.ts")));

        // A scan root with no SDL produces no hints (the no-op path).
        let empty_root = dir.join("nested-empty");
        std::fs::create_dir_all(&empty_root).unwrap();
        let empty = GraphqlProducerHints::collect(vec![empty_root], &[], &[]);
        assert!(empty.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// carrick#1157: a declared schema's fields stay out of the lines every
    /// file gets, and reach the schema-builder list once each, after the
    /// walked fields.
    #[test]
    fn declared_schema_fields_reach_only_the_schema_builder_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("api");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("schema.graphql"),
            "type Order { id: ID! }\ntype Query { order(id: ID!): Order }\n",
        )
        .unwrap();
        let printed = tmp.path().join("printed.graphql");
        std::fs::write(
            &printed,
            "type Order { id: ID! }\ntype Query {\n  order(id: ID!): Order\n  orders: [Order!]!\n}\ntype Mutation { cancelOrder(id: ID!): Boolean! }\n",
        )
        .unwrap();

        let hints = GraphqlProducerHints::collect(vec![root], &[printed], &[]);

        assert_eq!(hints.lines, vec!["query order: Order".to_string()]);
        assert_eq!(
            hints.schema_builder_lines(),
            vec![
                "query order: Order".to_string(),
                "query orders: [Order!]!".to_string(),
                "mutation cancelOrder: Boolean!".to_string(),
            ]
        );
        assert!(hints.has_schema_fields());

        let declared_only = GraphqlProducerHints::collect(
            vec![tmp.path().join("missing")],
            &[tmp.path().join("printed.graphql")],
            &[],
        );
        assert!(
            declared_only.is_empty(),
            "no walked field reaches every file"
        );
        assert!(declared_only.has_schema_fields());
    }

    /// Document consumers carry no SDL type, so their anchor is left unset (the
    /// TS result-type anchor is the follow-up #268).
    #[test]
    fn document_consumers_have_no_sdl_anchor() {
        let doc = r#"
            query GetOrder($id: ID!) { order(id: $id) { id } }
        "#;
        let result = extract_from_document_text(doc, Path::new("queries.graphql"), 1);
        assert_eq!(
            anchors(&result.consumers),
            vec![("graphql|query|order".to_string(), None)]
        );
    }

    /// #268: `GraphqlConsumerHints::collect` keeps only consumer ops the
    /// deterministic pass could NOT anchor (`payload_type_symbol.is_none()`) —
    /// an op the `request<T>(DOC)` call-site capture already anchored needs no
    /// hint, so it must be filtered out of both `lines` and `files`.
    #[test]
    fn consumer_hints_filter_out_anchored_consumers() {
        let dir = std::env::temp_dir().join(format!(
            "carrick-gql-consumer-hints-{}-{:016x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("client.ts");
        std::fs::write(
            &file,
            r#"
import { gql } from "graphql-tag";
import { OrderView } from "./types";
const client = makeClient();
const GET_ORDER = gql`
  query GetOrder($id: ID!) { order(id: $id) { id } }
`;
const ON_ORDER_UPDATED = gql`
  subscription OnOrderUpdated { orderUpdated { id } }
`;
async function fetchOrder(id) {
  // Anchored: explicit call-site generic.
  return client.request<OrderView>(GET_ORDER, { id });
}
function subscribe(cb) {
  // Unanchored: no call-site generic at all — nothing consumes ON_ORDER_UPDATED
  // through a typed call.
  return cb;
}
"#,
        )
        .unwrap();

        let hints = GraphqlConsumerHints::collect(vec![], std::slice::from_ref(&file));
        std::fs::remove_dir_all(&dir).ok();

        // Only the unanchored subscription produces a hint line.
        assert_eq!(hints.lines.len(), 1, "got lines: {:?}", hints.lines);
        assert!(
            hints.lines[0].starts_with("subscription|orderUpdated @ "),
            "unexpected hint line: {}",
            hints.lines[0]
        );
        assert!(!hints.is_empty());
        assert!(hints.file_has_hint(&file));

        // A file with no unanchored consumers yields no hints at all.
        let empty = GraphqlConsumerHints::collect(vec![], &[]);
        assert!(empty.is_empty());
        assert!(!empty.file_has_hint(&file));
    }

    /// Write `source` to a tempfile and run the TS-file extractor over it,
    /// exercising the gql-const → request-call-site anchor join.
    fn extract_ts(source: &str) -> GraphqlExtraction {
        let dir = std::env::temp_dir().join(format!(
            "carrick-gql-consumer-{}-{:016x}",
            std::process::id(),
            {
                let mut hash: u64 = 0xcbf29ce484222325;
                for byte in source.as_bytes() {
                    hash ^= u64::from(*byte);
                    hash = hash.wrapping_mul(0x100000001b3);
                }
                hash
            }
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("client.ts");
        std::fs::write(&file, source).unwrap();
        let result = extract_from_ts_file(&file);
        std::fs::remove_dir_all(&dir).ok();
        result
    }

    /// `(canonical_key, payload_type_symbol)` pairs for the consumer anchor
    /// captured at the `request<T>(DOC)` call site.
    fn payload_anchors(ops: &[GraphqlOp]) -> Vec<(String, Option<String>)> {
        let mut pairs: Vec<(String, Option<String>)> = ops
            .iter()
            .map(|op| (op.key.canonical(), op.payload_type_symbol.clone()))
            .collect();
        pairs.sort();
        pairs
    }

    /// A `request<OrderView>(GET_ORDER)` call site anchors the `query order`
    /// consumer on the bound named result type. The SDL-derived
    /// `primary_type_symbol` stays `None`; the new `payload_type_symbol` carries
    /// the real anchor.
    #[test]
    fn consumer_anchors_on_named_request_type_arg() {
        let result = extract_ts(
            r#"
import { gql } from "graphql-tag";
import { OrderView } from "./types";
const client = makeClient();
const GET_ORDER = gql`
  query GetOrder($id: ID!) { order(id: $id) { id } }
`;
async function fetchOrder(id) {
  return client.request<OrderView>(GET_ORDER, { id });
}
"#,
        );
        assert_eq!(
            payload_anchors(&result.consumers),
            vec![(
                "graphql|query|order".to_string(),
                Some("OrderView".to_string())
            )]
        );
        // SDL anchor stays unset — the new info lives in payload_type_symbol.
        assert_eq!(
            anchors(&result.consumers),
            vec![("graphql|query|order".to_string(), None)]
        );
        // Imported symbol carries its source.
        let order = result
            .consumers
            .iter()
            .find(|op| op.key.canonical() == "graphql|query|order")
            .unwrap();
        assert_eq!(order.payload_type_source.as_deref(), Some("./types"));
    }

    /// Generalization: the capture is NOT gated on a method-name allowlist, so a
    /// non-`request` execution method (`gqlClient.exec<T>(DOC)`, Apollo-style
    /// `useQuery<T>(DOC)`) anchors exactly like `request<T>(DOC)`. Under the old
    /// `matches!(method.sym, "request" | "query" | "mutate" | "subscribe")` gate
    /// these returned early and were never captured. The structural triad
    /// (member call + TS generic + tracked gql-const first arg) is all that's
    /// required.
    #[test]
    fn consumer_anchors_on_non_request_method_name() {
        // `exec` — not in the old allowlist.
        let exec_result = extract_ts(
            r#"
import { gql } from "graphql-tag";
import { OrderView } from "./types";
const gqlClient = makeClient();
const GET_ORDER = gql`
  query GetOrder($id: ID!) { order(id: $id) { id } }
`;
async function fetchOrder(id) {
  return gqlClient.exec<OrderView>(GET_ORDER, { id });
}
"#,
        );
        assert_eq!(
            payload_anchors(&exec_result.consumers),
            vec![(
                "graphql|query|order".to_string(),
                Some("OrderView".to_string())
            )]
        );

        // Apollo-style `useQuery<T>(DOC)` as a member call, single-property
        // wrapper matching the field — also not in the old allowlist.
        let use_query_result = extract_ts(
            r#"
import { gql } from "graphql-tag";
import { OrderView } from "./types";
const apollo = makeClient();
const GET_ORDER = gql`
  query GetOrder($id: ID!) { order(id: $id) { id } }
`;
function OrderComponent(id) {
  return apollo.useQuery<{ order: OrderView }>(GET_ORDER, { id });
}
"#,
        );
        assert_eq!(
            payload_anchors(&use_query_result.consumers),
            vec![(
                "graphql|query|order".to_string(),
                Some("OrderView".to_string())
            )]
        );
    }

    /// Precision guard survives the method-allowlist removal: a generic member
    /// call whose first arg is NOT a tracked gql document (`foo.map<T>(x)`) is
    /// never captured, even though it is a member call with a TS generic.
    #[test]
    fn non_gql_generic_member_call_is_not_captured() {
        let result = extract_ts(
            r#"
import { gql } from "graphql-tag";
import { OrderView } from "./types";
const GET_ORDER = gql`
  query GetOrder($id: ID!) { order(id: $id) { id } }
`;
function compute(items, x) {
  return items.map<OrderView>(x);
}
"#,
        );
        // The gql document is still parsed into a consumer op, but it carries no
        // payload anchor because no qualifying execution call referenced it.
        assert_eq!(
            payload_anchors(&result.consumers),
            vec![("graphql|query|order".to_string(), None)]
        );
    }

    /// A `request<{ order: OrderView }>(GET_ORDER)` call site — the single
    /// property name (`order`) matches the operation field, so the wrapper is
    /// unwrapped to the inner symbol `OrderView`. This is the exact corpus shape
    /// (`web-frontend/lib/graphql.ts`).
    #[test]
    fn consumer_unwraps_single_property_wrapper_matching_field() {
        let result = extract_ts(
            r#"
import { gql } from "graphql-tag";
import { OrderView } from "./types";
const client = makeClient();
const GET_ORDER = gql`
  query GetOrder($id: ID!) { order(id: $id) { id } }
`;
async function fetchOrder(id) {
  const res = await client.request<{ order: OrderView }>(GET_ORDER, { id });
  return res.order;
}
"#,
        );
        assert_eq!(
            payload_anchors(&result.consumers),
            vec![(
                "graphql|query|order".to_string(),
                Some("OrderView".to_string())
            )]
        );
        assert_eq!(
            anchors(&result.consumers),
            vec![("graphql|query|order".to_string(), None)]
        );
    }

    /// A single-property wrapper whose key does NOT match the operation field is
    /// NOT unwrapped (the property isn't the operation envelope) — precision over
    /// recall leaves it unanchored rather than guessing.
    #[test]
    fn consumer_does_not_unwrap_when_property_name_mismatches_field() {
        let result = extract_ts(
            r#"
import { gql } from "graphql-tag";
import { OrderView } from "./types";
const client = makeClient();
const GET_ORDER = gql`
  query GetOrder($id: ID!) { order(id: $id) { id } }
`;
async function fetchOrder(id) {
  return client.request<{ wrongField: OrderView }>(GET_ORDER, { id });
}
"#,
        );
        assert_eq!(
            payload_anchors(&result.consumers),
            vec![("graphql|query|order".to_string(), None)]
        );
    }

    /// Two documents in one file sharing a field name but differing in operation
    /// kind (`query order` vs `mutation order`) must anchor independently to their
    /// own bound type. Keying the gql-const/request joins by the canonical key
    /// (kind + field), not the bare field, prevents the mutation's type from
    /// clobbering the query's (or vice versa).
    #[test]
    fn same_field_name_different_kinds_anchor_independently() {
        let result = extract_ts(
            r#"
import { gql } from "graphql-tag";
import { OrderView, RefundReceipt } from "./types";
const client = makeClient();
const GET_ORDER = gql`
  query GetOrder($id: ID!) { order(id: $id) { id } }
`;
const REFUND_ORDER = gql`
  mutation RefundOrder($id: ID!) { order(id: $id) { id } }
`;
async function run(id) {
  const a = await client.request<OrderView>(GET_ORDER, { id });
  const b = await client.mutate<RefundReceipt>(REFUND_ORDER, { id });
  return [a, b];
}
"#,
        );
        // Each canonical key carries its OWN bound type — no cross-anchoring.
        assert_eq!(
            payload_anchors(&result.consumers),
            vec![
                (
                    "graphql|mutation|order".to_string(),
                    Some("RefundReceipt".to_string())
                ),
                (
                    "graphql|query|order".to_string(),
                    Some("OrderView".to_string())
                ),
            ]
        );
        // Each anchored symbol carries the correct import source.
        let query_op = result
            .consumers
            .iter()
            .find(|op| op.key.canonical() == "graphql|query|order")
            .unwrap();
        assert_eq!(query_op.payload_type_source.as_deref(), Some("./types"));
        let mutation_op = result
            .consumers
            .iter()
            .find(|op| op.key.canonical() == "graphql|mutation|order")
            .unwrap();
        assert_eq!(mutation_op.payload_type_source.as_deref(), Some("./types"));
    }

    /// No type argument on the request call → no anchor (the document still
    /// extracts as a consumer).
    #[test]
    fn consumer_without_type_arg_stays_unanchored() {
        let result = extract_ts(
            r#"
import { gql } from "graphql-tag";
const client = makeClient();
const GET_ORDER = gql`
  query GetOrder($id: ID!) { order(id: $id) { id } }
`;
async function fetchOrder(id) {
  return client.request(GET_ORDER, { id });
}
"#,
        );
        assert_eq!(
            payload_anchors(&result.consumers),
            vec![("graphql|query|order".to_string(), None)]
        );
    }

    /// #248 corpus binding: the anchors derived from the REAL corpus-1 gateway
    /// schema must equal the `primary_type_symbol` values committed in that
    /// repo's `expected.json` (the cross-repo eval's anchor ground truth). This
    /// fails if the extractor drifts OR the ground truth is edited away from the
    /// deterministic SDL form, keeping the live anchor metric honest without
    /// Vertex credentials.
    #[test]
    fn corpus_gateway_producer_anchors_match_ground_truth() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/xrepo-corpus-1/orders-monorepo/packages/gateway/src");
        let schema = root.join("schema.graphql");
        let sdl = std::fs::read_to_string(&schema)
            .unwrap_or_else(|e| panic!("read corpus schema {}: {e}", schema.display()));
        let result = extract_from_document_text(&sdl, &schema, 1);

        // The exact ground-truth anchors from
        // orders-monorepo/expected.json::graphql_operations (producers).
        assert_eq!(
            anchors(&result.producers),
            vec![
                (
                    "graphql|mutation|refundOrder".to_string(),
                    Some("Order!".to_string())
                ),
                ("graphql|query|order".to_string(), Some("Order".to_string())),
                (
                    "graphql|query|orders".to_string(),
                    Some("[Order!]!".to_string())
                ),
                (
                    "graphql|subscription|orderUpdated".to_string(),
                    Some("Order!".to_string())
                ),
            ]
        );
    }

    #[test]
    fn sdl_root_fields_become_producers() {
        let sdl = r#"
            type User { id: ID!, name: String }
            type Query {
                user(id: ID!): User
                users: [User!]!
            }
            type Mutation {
                createUser(name: String!): User
            }
        "#;
        let result = extract_from_document_text(sdl, Path::new("schema.graphql"), 1);
        assert_eq!(
            keys(&result.producers),
            vec![
                "graphql|mutation|createUser",
                "graphql|query|user",
                "graphql|query|users",
            ]
        );
        assert!(result.consumers.is_empty());
    }

    #[test]
    fn schema_definition_remaps_root_types() {
        let sdl = r#"
            schema { query: RootQuery }
            type RootQuery { health: String }
            type Query { ignored: String }
        "#;
        let result = extract_from_document_text(sdl, Path::new("schema.graphql"), 1);
        assert_eq!(keys(&result.producers), vec!["graphql|query|health"]);
    }

    #[test]
    fn extend_type_query_adds_producers() {
        let sdl = r#"
            extend type Query { extra: String }
        "#;
        let result = extract_from_document_text(sdl, Path::new("schema.graphql"), 1);
        assert_eq!(keys(&result.producers), vec!["graphql|query|extra"]);
    }

    #[test]
    fn non_root_only_sdl_is_schema_not_document() {
        let sdl = "type User { id: ID! }";
        let result = extract_from_document_text(sdl, Path::new("types.graphql"), 1);
        assert!(result.producers.is_empty());
        assert!(result.consumers.is_empty());
    }

    #[test]
    fn operations_become_consumers_with_real_field_names() {
        let doc = r#"
            query GetUser($id: ID!) {
                currentUser: user(id: $id) { id name }
                __typename
            }
            mutation { createUser(name: "x") { id } }
        "#;
        let result = extract_from_document_text(doc, Path::new("queries.graphql"), 1);
        assert_eq!(
            keys(&result.consumers),
            vec!["graphql|mutation|createUser", "graphql|query|user"]
        );
        assert!(result.producers.is_empty());
    }

    #[test]
    fn anonymous_shorthand_is_a_query() {
        let result = extract_from_document_text("{ health }", Path::new("q.graphql"), 1);
        assert_eq!(keys(&result.consumers), vec!["graphql|query|health"]);
    }

    #[test]
    fn unparseable_text_is_skipped_silently() {
        let result = extract_from_document_text("query { broken", Path::new("q.graphql"), 1);
        assert!(result.is_empty());
    }

    #[test]
    fn tagged_templates_are_extracted_from_ts() {
        let dir = std::env::temp_dir().join(format!("carrick-gql-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("client.ts");
        std::fs::write(
            &file,
            r#"
import { gql } from "graphql-tag";
const FRAGMENT = gql`fragment UserFields on User { id name }`;
const GET_USER = gql`
  query GetUser($id: ID!) {
    user(id: $id) { ...UserFields }
  }
  ${FRAGMENT}
`;
const notGraphql = sql`SELECT 1`;
"#,
        )
        .unwrap();

        let result = extract_from_ts_file(&file);
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(keys(&result.consumers), vec!["graphql|query|user"]);
        assert!(result.producers.is_empty());
    }

    #[test]
    fn typedefs_template_yields_producers() {
        let dir = std::env::temp_dir().join(format!("carrick-gql-sdl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("server.ts");
        std::fs::write(
            &file,
            r#"
import gql from "graphql-tag";
export const typeDefs = gql`
  type Query { orders: [Order!]! }
  type Order { id: ID! }
`;
"#,
        )
        .unwrap();

        let result = extract_from_ts_file(&file);
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(keys(&result.producers), vec!["graphql|query|orders"]);
    }

    #[test]
    fn sdl_walk_is_scoped_to_service_roots() {
        // #242: a monorepo package's SDL must be attributed only to that
        // package's own roots, never to a sibling. Walking the repo root (as the
        // old signature did) would credit package `b` with package `a`'s schema.
        let base = std::env::temp_dir().join(format!("carrick-gql-scope-{}", std::process::id()));
        let pkg_a = base.join("packages/a/src");
        let pkg_b = base.join("packages/b/src");
        std::fs::create_dir_all(&pkg_a).unwrap();
        std::fs::create_dir_all(&pkg_b).unwrap();
        std::fs::write(
            pkg_a.join("schema.graphql"),
            "type Query { order(id: ID!): String }",
        )
        .unwrap();

        let a_root = base.join("packages/a");
        let b_root = base.join("packages/b");
        let no_files: &[PathBuf] = &[];
        let scoped_a = scan_repo(std::slice::from_ref(&a_root), &[], no_files);
        let scoped_b = scan_repo(std::slice::from_ref(&b_root), &[], no_files);
        std::fs::remove_dir_all(&base).ok();

        assert_eq!(
            keys(&scoped_a.producers),
            vec!["graphql|query|order"],
            "package a's own root must find its schema"
        );
        assert!(
            scoped_b.producers.is_empty(),
            "package b must NOT be credited with sibling a's schema, got {:?}",
            keys(&scoped_b.producers)
        );
    }

    /// carrick#1099: a printed schema in a skipped build folder is invisible to
    /// the service walk and read when declared; only its producers count, and a
    /// declared file under a scan root is read once.
    #[test]
    fn declared_schema_is_read_under_a_skipped_folder_and_keeps_producers_only() {
        let repo = tempfile::tempdir().unwrap();
        let service = repo.path().join("api");
        let printed = repo.path().join("web/dist");
        std::fs::create_dir_all(&service).unwrap();
        std::fs::create_dir_all(&printed).unwrap();
        std::fs::write(
            printed.join("schema.graphql"),
            "type Query { widgets: [String!]! }\ntype Mutation { dispose(id: ID!): Boolean! }",
        )
        .unwrap();
        std::fs::write(service.join("ops.graphql"), "type Query { local: String }").unwrap();
        std::fs::write(service.join("doc.graphql"), "query Remote { remote }").unwrap();
        let roots = [service.clone()];

        let undeclared = scan_repo(&roots, &[], &[]);
        assert_eq!(keys(&undeclared.producers), vec!["graphql|query|local"]);
        assert_eq!(keys(&undeclared.consumers), vec!["graphql|query|remote"]);

        let declared = resolve_declared_schemas(
            repo.path(),
            &[
                "web/dist/*.graphql".to_string(),
                "api/ops.graphql".to_string(),
                "api/doc.graphql".to_string(),
            ],
        );
        // A declared executable document states no served field, and says so.
        assert_eq!(
            declared.problems,
            vec![
                "`graphqlSchemas` file 'api/doc.graphql' defines no Query, Mutation or \
                 Subscription field, so it adds no operations"
                    .to_string()
            ]
        );
        let extraction = scan_repo(&roots, &declared.files, &[]);
        // Declared files are read first and once: the root walk neither
        // duplicates `local` nor turns the declared document into a call.
        assert!(extraction.consumers.is_empty());
        assert_eq!(
            keys(&extraction.producers),
            vec![
                "graphql|mutation|dispose",
                "graphql|query|local",
                "graphql|query|widgets"
            ]
        );
        assert_eq!(
            extraction
                .producers
                .iter()
                .find(|op| op.key.canonical() == "graphql|query|widgets")
                .unwrap()
                .file_path,
            printed.join("schema.graphql")
        );
    }

    #[test]
    fn declared_schemas_report_every_entry_that_declares_nothing() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("dist")).unwrap();
        std::fs::write(
            repo.path().join("dist/types.graphql"),
            "type Widget { id: ID! }",
        )
        .unwrap();

        let declared = resolve_declared_schemas(
            repo.path(),
            &[
                "missing/schema.graphql".to_string(),
                "../outside.graphql".to_string(),
                "/abs/schema.graphql".to_string(),
                "dist/[.graphql".to_string(),
                "dist/*.graphql".to_string(),
            ],
        );
        assert_eq!(declared.files, vec![repo.path().join("dist/types.graphql")]);
        assert_eq!(
            declared.problems,
            vec![
                "`graphqlSchemas` entry 'missing/schema.graphql' matches no file in this \
                 repository, so the operations it declares are not indexed"
                    .to_string(),
                "`graphqlSchemas` entry '../outside.graphql' is not a path inside the repository, \
                 so nothing was read for it"
                    .to_string(),
                "`graphqlSchemas` entry '/abs/schema.graphql' is not a path inside the \
                 repository, so nothing was read for it"
                    .to_string(),
                "`graphqlSchemas` entry 'dist/[.graphql' is not a valid glob (Pattern syntax \
                 error near position 5: invalid range pattern), so nothing was read for it"
                    .to_string(),
                "`graphqlSchemas` file 'dist/types.graphql' defines no Query, Mutation or \
                 Subscription field, so it adds no operations"
                    .to_string(),
            ]
        );
    }

    fn facts<'a>(
        declared: &'a DeclaredSchemas,
        libraries: &'a [String],
    ) -> GraphqlServiceFacts<'a> {
        GraphqlServiceFacts {
            service: "api",
            declares_schemas: false,
            declared,
            producers: 0,
            serves_http: true,
            graphql_libraries: libraries,
        }
    }

    #[test]
    fn the_code_first_hint_needs_a_graphql_server_with_no_producer_and_no_setting() {
        let none = DeclaredSchemas::default();
        let libraries = vec!["graphql".to_string()];

        let hinted = service_notices(facts(&none, &libraries));
        assert!(hinted.warnings.is_empty());
        assert_eq!(
            hinted.hints,
            vec![
                "Service 'api' uses GraphQL (`graphql`) and serves HTTP routes, but indexes no \
                 GraphQL schema fields. If its schema is built in code, name the printed SDL \
                 file in `graphqlSchemas` for this service in carrick.json."
                    .to_string()
            ]
        );

        let with_producer = GraphqlServiceFacts {
            producers: 1,
            ..facts(&none, &libraries)
        };
        let client_only = GraphqlServiceFacts {
            serves_http: false,
            ..facts(&none, &libraries)
        };
        let declaring = GraphqlServiceFacts {
            declares_schemas: true,
            ..facts(&none, &libraries)
        };
        for quiet in [with_producer, client_only, declaring, facts(&none, &[])] {
            assert_eq!(service_notices(quiet), GraphqlNotices::default());
        }

        let broken = DeclaredSchemas {
            files: vec![],
            problems: vec!["`graphqlSchemas` entry 'x' matches no file".to_string()],
        };
        let warned = service_notices(GraphqlServiceFacts {
            declares_schemas: true,
            ..facts(&broken, &libraries)
        });
        assert_eq!(
            warned.warnings,
            vec!["Service 'api': `graphqlSchemas` entry 'x' matches no file".to_string()]
        );
        assert!(warned.hints.is_empty());
    }

    fn known(file: &str, origin: SchemaOrigin, sdl: &str) -> KnownSchema {
        KnownSchema {
            file: PathBuf::from(file),
            origin,
            root_keys: extract_from_document_text(sdl, Path::new(file), 1)
                .producers
                .iter()
                .map(|op| op.key.canonical())
                .collect(),
        }
    }

    /// Two schemas that share one root field, `viewer`: the shape carrick#1134
    /// has to tell apart without a codegen config.
    fn two_schema_catalogue() -> SchemaCatalogue {
        SchemaCatalogue {
            schemas: vec![
                known(
                    "tools/dist/catalog.graphql",
                    SchemaOrigin::Served,
                    "type Query { products: [String] viewer: String } \
                     type Mutation { addProduct(name: String): String }",
                ),
                known(
                    "tools/dist/ledger.graphql",
                    SchemaOrigin::External,
                    "type Query { balance: Int statements: [String] viewer: String }",
                ),
            ],
            tally: Default::default(),
        }
    }

    /// One document per file, parsed as the scan parses a `.graphql` file.
    fn documents(docs: &[(&str, &str)]) -> GraphqlExtraction {
        let mut extraction = GraphqlExtraction::default();
        for (file, text) in docs {
            extraction.merge(extract_from_document_text(text, Path::new(file), 1));
        }
        extraction
    }

    fn identity_of<'a>(
        attribution: &'a ConsumerAttribution,
        extraction: &GraphqlExtraction,
        file: &str,
    ) -> &'a DocumentIdentity {
        let op = extraction
            .consumers
            .iter()
            .find(|op| op.file_path == Path::new(file))
            .unwrap_or_else(|| panic!("no consumer in {file}"));
        attribution
            .identity(op)
            .expect("every document is attributed")
    }

    #[test]
    fn a_document_is_attributed_to_the_schema_that_holds_its_fields() {
        let catalogue = two_schema_catalogue();
        let extraction = documents(&[
            (
                "served.gql",
                "query A { products } mutation B { addProduct(name: \"x\") }",
            ),
            // `retiredListing` is in no schema: left out of the basis, so the
            // document is still the served schema's and the field still reads
            // as a missing operation.
            (
                "drift.gql",
                "query A { products } query B { retiredListing }",
            ),
            ("external.gql", "query A { balance } query B { statements }"),
            ("spans.gql", "query A { products balance }"),
            ("nowhere.gql", "query A { somethingElse }"),
        ]);
        let attribution = catalogue.attribute(&extraction, |_| TransportOrigin::Unknown);

        assert_eq!(
            identity_of(&attribution, &extraction, "served.gql"),
            &DocumentIdentity::Served
        );
        assert_eq!(
            identity_of(&attribution, &extraction, "drift.gql"),
            &DocumentIdentity::Served
        );
        assert_eq!(
            identity_of(&attribution, &extraction, "external.gql"),
            &DocumentIdentity::External(vec![PathBuf::from("tools/dist/ledger.graphql")])
        );
        assert_eq!(
            identity_of(&attribution, &extraction, "spans.gql"),
            &DocumentIdentity::Unresolved
        );
        assert_eq!(
            identity_of(&attribution, &extraction, "nowhere.gql"),
            &DocumentIdentity::NoLocalSchema
        );
    }

    /// A document that selects a nested field the served schema removed still
    /// holds that schema's root field, so it stays the served schema's and its
    /// operation stays a call. Attribution reads root fields only.
    #[test]
    fn a_removed_nested_field_keeps_the_document_with_its_schema() {
        let catalogue = two_schema_catalogue();
        let mut extraction = documents(&[("nested.gql", "query A { products { id legacySku } }")]);
        let attribution = catalogue.attribute(&extraction, |_| TransportOrigin::Unknown);

        assert_eq!(
            identity_of(&attribution, &extraction, "nested.gql"),
            &DocumentIdentity::Served
        );
        let summary = attribution.apply(&mut extraction);
        assert!(summary.is_empty());
        assert_eq!(keys(&extraction.consumers), vec!["graphql|query|products"]);
        assert_eq!(
            extraction.consumers[0].schema_binding,
            Some(SchemaBinding::Served)
        );
    }

    /// A document whose only root field no schema holds (the served schema
    /// removed it, and no external schema declares it) is never dropped: it
    /// has no local identity, stays a call, and reads as a missing operation.
    /// The same holds whether or not the repository also holds an external
    /// schema.
    #[test]
    fn a_removed_root_field_stays_a_call() {
        let served_only = SchemaCatalogue {
            schemas: vec![known(
                "tools/dist/catalog.graphql",
                SchemaOrigin::Served,
                "type Query { products: [String] }",
            )],
            tally: Default::default(),
        };
        for catalogue in [served_only, two_schema_catalogue()] {
            let mut extraction = documents(&[("retired.gql", "query A { retiredListing }")]);
            let attribution = catalogue.attribute(&extraction, |_| TransportOrigin::Unknown);

            assert_eq!(
                identity_of(&attribution, &extraction, "retired.gql"),
                &DocumentIdentity::NoLocalSchema
            );
            let summary = attribution.apply(&mut extraction);
            assert!(summary.is_empty(), "nothing is dropped or counted");
            assert_eq!(
                keys(&extraction.consumers),
                vec!["graphql|query|retiredListing"]
            );
            assert_eq!(
                extraction.consumers[0].schema_binding,
                Some(SchemaBinding::NoLocalSchema)
            );
        }
    }

    #[test]
    fn a_shared_field_is_settled_by_the_transport_or_left_unresolved() {
        let catalogue = two_schema_catalogue();
        let extraction = documents(&[
            ("internal.ts", "query A { viewer }"),
            ("external.ts", "query A { viewer }"),
            ("unknown.gql", "query A { viewer }"),
        ]);
        let attribution = catalogue.attribute(&extraction, |file| {
            match file.to_str().unwrap_or_default() {
                "internal.ts" => TransportOrigin::Internal,
                "external.ts" => TransportOrigin::External,
                _ => TransportOrigin::Unknown,
            }
        });

        assert_eq!(
            identity_of(&attribution, &extraction, "internal.ts"),
            &DocumentIdentity::Served
        );
        assert_eq!(
            identity_of(&attribution, &extraction, "external.ts"),
            &DocumentIdentity::External(vec![PathBuf::from("tools/dist/ledger.graphql")])
        );
        assert_eq!(
            identity_of(&attribution, &extraction, "unknown.gql"),
            &DocumentIdentity::Unresolved
        );
    }

    /// Schema text in the service's own source (a `typeDefs` template) is not
    /// a file in the catalogue, but the service serves it all the same.
    #[test]
    fn the_services_own_producers_are_a_served_schema() {
        let catalogue = two_schema_catalogue();
        let mut extraction = documents(&[("doc.gql", "query A { balance }")]);
        extraction.merge(extract_from_document_text(
            "type Query { balance: Int }",
            Path::new("src/typedefs.ts"),
            4,
        ));
        let attribution = catalogue.attribute(&extraction, |_| TransportOrigin::Unknown);

        assert_eq!(
            identity_of(&attribution, &extraction, "doc.gql"),
            &DocumentIdentity::Unresolved,
            "own producers and the external schema both hold `balance`, with no transport"
        );
    }

    #[test]
    fn apply_removes_external_and_unresolved_documents_and_counts_them() {
        let catalogue = two_schema_catalogue();
        let mut extraction = documents(&[
            ("served.gql", "query A { products }"),
            ("external.gql", "query A { balance } query B { statements }"),
            ("spans.gql", "query A { products balance }"),
            ("nowhere.gql", "query A { somethingElse }"),
        ]);
        let attribution = catalogue.attribute(&extraction, |_| TransportOrigin::Unknown);
        let summary = attribution.apply(&mut extraction);

        assert_eq!(
            keys(&extraction.consumers),
            vec!["graphql|query|products", "graphql|query|somethingElse"]
        );
        assert_eq!(
            extraction
                .consumers
                .iter()
                .map(|op| op.schema_binding)
                .collect::<Vec<_>>(),
            vec![
                Some(SchemaBinding::Served),
                Some(SchemaBinding::NoLocalSchema)
            ]
        );
        assert_eq!(
            summary,
            AttributionSummary {
                external: [(vec![PathBuf::from("tools/dist/ledger.graphql")], 2)]
                    .into_iter()
                    .collect(),
                unresolved: 2,
            }
        );

        catalogue.record("web", summary);
        let notices = catalogue.notices();
        assert_eq!(notices.len(), 2, "{notices:?}");
        assert!(
            notices[0].starts_with(
                "Service 'web': 2 GraphQL document operation(s) are written against \
                 'tools/dist/ledger.graphql', which no service in this repository serves"
            ),
            "{}",
            notices[0]
        );
        // A retry that removes nothing replaces the earlier record.
        catalogue.record("web", AttributionSummary::default());
        assert!(catalogue.notices().is_empty());
    }

    /// Outside a git repository every schema file on disk counts, build
    /// folders included; dependency folders and documents never do.
    #[test]
    fn catalogue_marks_walked_and_declared_schemas_served_and_the_rest_external() {
        let repo = tempfile::tempdir().unwrap();
        let write = |relative: &str, text: &str| {
            let path = repo.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            "apps/api/src/schema.graphql",
            "type Query { orders: [String] }",
        );
        write(
            "tools/dist/printed.graphql",
            "type Query { invoices: [String] }",
        );
        write("tools/dist/vendor.graphql", "type Query { balance: Int }");
        write(
            "node_modules/pkg/schema.graphql",
            "type Query { dependency: Int }",
        );
        write("apps/web/src/doc.gql", "query A { balance }");
        write("apps/web/src/types.graphql", "type Widget { id: ID! }");

        let catalogue = SchemaCatalogue::build(
            repo.path(),
            &[
                ServedSchemaSources {
                    roots: vec![repo.path().join("apps/api")],
                    declared: vec![repo.path().join("tools/dist/printed.graphql")],
                },
                ServedSchemaSources {
                    roots: vec![repo.path().join("apps/web")],
                    declared: vec![],
                },
            ],
        );
        let mut seen: Vec<(String, SchemaOrigin)> = catalogue
            .schemas
            .iter()
            .map(|s| (s.file.display().to_string(), s.origin))
            .collect();
        seen.sort_by(|a, b| a.0.cmp(&b.0));

        assert_eq!(
            seen,
            vec![
                (
                    "apps/api/src/schema.graphql".to_string(),
                    SchemaOrigin::Served
                ),
                (
                    "tools/dist/printed.graphql".to_string(),
                    SchemaOrigin::Served
                ),
                (
                    "tools/dist/vendor.graphql".to_string(),
                    SchemaOrigin::External
                ),
            ]
        );
    }

    #[test]
    fn env_reads_cover_every_runtime_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("client.ts");
        std::fs::write(
            &file,
            "const a = process.env.CATALOG_URL ?? 'http://localhost:4000';\n\
             const b = process.env['LEDGER_URL'];\n\
             const c = import.meta.env.VITE_API_URL;\n\
             const d = import.meta.url;\n\
             const e = Deno.env.get('LEDGER_TOKEN');\n\
             export { a, b, c, d, e };\n",
        )
        .unwrap();

        assert_eq!(
            file_env_reads(&file).into_iter().collect::<Vec<_>>(),
            vec!["CATALOG_URL", "LEDGER_TOKEN", "LEDGER_URL", "VITE_API_URL"]
        );
        assert!(file_env_reads(&dir.path().join("schema.graphql")).is_empty());
    }
}
