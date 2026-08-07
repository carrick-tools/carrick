/**
 * Wire contract for the v2 capture bundle ("tsc as the serializer").
 *
 * This file IS the seam (design doc: "seam, not split"): everything outside
 * src/sidecar/src/capture/ may import types from this file and the
 * `captureStub` entry point from ./index.js, and nothing else. Modules inside
 * capture/ import only node builtins, `typescript`, and each other. The
 * stdio `capture_v2` action is the only surface the Rust client sees.
 */

/**
 * How the anchor was produced upstream (design-doc amendment 2). Recorded so
 * the fidelity metric separates anchor-recall loss from serialization loss.
 * (Named anchor_origin because `provenance` is taken by the op-level
 * producer-provenance fields in src/eval_output.rs.)
 */
export type AnchorOrigin = 'llm-symbol' | 'deterministic-infer' | 'anchor-backfill';

/**
 * Serialization tier of a captured alias (design doc, Capture step 5):
 *  - emitted: compiler declaration emit of an addressable symbol (best)
 *  - node_builder: SymbolTracker-verified node-builder print of an anonymous
 *    inferred type
 *  - structural_fallback: the legacy hand-text tier. Two shapes share it:
 *    (a) literal anchors — the WP3 wiring of v1 inference/inline text into
 *    the surface (self-checked like any other alias, so decay is still
 *    caught), and (b) demotions from the capture-native paths (guard
 *    failure, locator failure, inaccessible symbols), which emit `unknown`
 *    with a recorded `capture_failure_reason`. Keeping both at one tier is
 *    what makes the remaining legacy-text dependence measurable and
 *    ratchetable.
 */
export type SerializationTier = 'emitted' | 'node_builder' | 'structural_fallback';

/** Explicit exported symbol: `export type A = import('./m').Sym;` */
export interface SymbolAnchorRequest {
  kind: 'symbol';
  /** Manifest alias, e.g. Endpoint_abc123_Response */
  alias: string;
  /** Exported symbol name in the producer repo */
  symbol_name: string;
  /** Declaring module, repo-root-relative, e.g. src/types/stock.ts */
  source_file: string;
  anchor_origin: AnchorOrigin;
  /**
   * Wrap the captured symbol in this many TS array levels (#248/#306): an
   * anchor is the ELEMENT symbol by contract (`User[]` -> `User`), so the
   * use-site's array-ness rides here and the surface alias becomes
   * `import('./m').Sym[]`. Omitted/0 captures the symbol as-is.
   */
  array_depth?: number;
}

/**
 * Inline literal type text with no addressable symbol (the v1 inline-alias
 * path): the surface entry gets `export type A = <type_text>;`. A bare
 * identifier that names a sibling symbol anchor's symbol resolves through
 * that anchor's module specifier so it does not dangle in the entry file;
 * any other text is emitted verbatim and the self-check owns the verdict.
 */
export interface LiteralAnchorRequest {
  kind: 'literal';
  alias: string;
  /** Verbatim TS type text (a bare symbol name or an inline object type). */
  type_text: string;
  anchor_origin: AnchorOrigin;
}

/**
 * Addressable handler: `export type A = Awaited<ReturnType<typeof
 * import('./m').fn>>;` -- guarded (design doc, Capture step 1): the symbol
 * must be exported, must not be an overload set (ReturnType silently resolves
 * the last overload), and must not be generic (type params erase). Guard
 * failures demote to structural_fallback with the reason recorded.
 */
export interface HandlerReturnAnchorRequest {
  kind: 'handler_return';
  alias: string;
  symbol_name: string;
  source_file: string;
  anchor_origin: AnchorOrigin;
}

/**
 * Anonymous inferred type at a source location (no addressable symbol). The
 * node is located by byte span when given, else by expression text on/after
 * a line, else by line. Its type is printed into the surface entry via the
 * compiler node builder with a real SymbolTracker (see node-builder.ts).
 */
export interface InferAnchorRequest {
  kind: 'infer';
  alias: string;
  source_file: string;
  anchor_origin: AnchorOrigin;
  /** Byte span of the target node (TS source positions). */
  span_start?: number;
  span_end?: number;
  /** 1-based line the target starts on (locator fallback + disambiguation). */
  line_number?: number;
  /** Exact source text of the target expression (locator fallback). */
  expression_text?: string;
  /**
   * carrick#498: the anchor targets a handler PARAMETER, not an expression.
   * Carries the upstream `function_param` locator (a parameter name, a whole
   * destructured binding pattern, or one binding element inside it), so the
   * capture resolves the payload the handler RECEIVES. Without it a line-only
   * subscriber anchor resolves the enclosing registration CALL and captures
   * that call's return type (`void`, a subscription handle) as the contract.
   * When present the parameter resolution is authoritative: a failure demotes
   * rather than falling back to the expression locator.
   */
  param_name?: string;
  /**
   * Transport unwrapping applied to the located type before printing
   * (design doc, Capture step 6: machinery unwrapping stays at capture time).
   * Default 'awaited': Promise / thenable layers are unwrapped.
   */
  unwrap?: 'awaited' | 'none';
}

export type CaptureAnchorRequest =
  | SymbolAnchorRequest
  | HandlerReturnAnchorRequest
  | InferAnchorRequest
  | LiteralAnchorRequest;

export type SelfCheckOutcome = 'ok' | 'allowlisted_external' | 'decayed_internal';

export interface CaptureAliasRecord {
  alias: string;
  anchor_kind: CaptureAnchorRequest['kind'];
  symbol_name?: string;
  /** Repo-root-relative declaring module; `<inline>` for literal anchors. */
  source_file: string;
  anchor_origin: AnchorOrigin;
  serialization: SerializationTier;
  self_check: SelfCheckOutcome;
  /** Human-readable reason when self_check is not 'ok'. */
  self_check_detail?: string;
  /**
   * Recorded when the alias never reached a usable tier (guard failure,
   * locator failure, inaccessible symbols during node-builder printing).
   * Present exactly for demoted anchors; a successful literal anchor sits
   * at the structural_fallback tier WITHOUT a failure reason.
   */
  capture_failure_reason?: string;
  /** True when the alias resolved to any/unknown/never during self-check.
   * With self_check === 'allowlisted_external' this is expected on a bare
   * checkout and is NOT a decay; the probe gates own the final verdict. */
  top_type_at_self_check: boolean;
  /**
   * Set when the self-check found a disqualifier at DEPTH (member / element /
   * index signature / type argument / callable return) with no failing
   * pinned-external explanation: an author-baked `any`/`unknown`, or
   * `budget_exhausted` — a subtree too deep/wide to finish within the walk's
   * budget (failed closed, not silently clean). The check phase pre-gates
   * exactly these aliases: its whole-type probe gates cannot see member-level
   * decay, and `any` at any depth lets an arbitrary counterparty read
   * compatible. `any` routes to `gate_caught_baked_any`; `unknown` and
   * `budget_exhausted` route to `unverifiable`.
   */
  deep_top_type_kind?: 'any' | 'unknown' | 'budget_exhausted';
  /** Member path of the deep find, e.g. `metadata` or `items<0>.meta`. */
  deep_top_type_path?: string;
}

/** Aggregate fidelity metric, emitted per capture (one service). */
export interface CaptureFidelity {
  total_aliases: number;
  by_serialization: Record<SerializationTier, number>;
  by_self_check: Record<SelfCheckOutcome, number>;
  by_anchor_origin: Record<AnchorOrigin, number>;
  /** Aliases whose capture is usable at check time (self_check ok or
   * allowlisted_external) over total. */
  usable_rate: number;
}

export interface CaptureStubResult {
  success: boolean;
  stub_dir: string;
  package_name: string;
  /** Stub-relative paths of the emitted declaration tree. */
  emitted_files: string[];
  /** Exact-version pins for external packages referenced by the tree. */
  pinned_dependencies: Record<string, string>;
  /** External specifiers referenced by the tree but absent from the lockfile. */
  unpinned_externals: string[];
  aliases: CaptureAliasRecord[];
  fidelity: CaptureFidelity;
  /** Tree-relative paths of files included because they declare global or
   * module augmentations (design doc, Capture step 4). */
  augmentation_files: string[];
  /** Number of emitted specifiers rewritten by the post-emit pass
   * (tsconfig-paths mappings and absolute internal import types). */
  specifier_rewrites: number;
  /** True when the source repo had no node_modules at capture time. */
  bare_checkout: boolean;
  ts_version: string;
  errors: string[];
}

export interface CaptureStubOptions {
  repoRoot: string;
  serviceName: string;
  anchors: CaptureAnchorRequest[];
  /** Directory the stub package is written into (created if missing). */
  outDir: string;
  tsconfigPath?: string;
}

// ===========================================================================
// Check phase ("tsc as the judge") — WP2
//
// The check phase assembles two or more capture stub packages into a scratch
// synthetic monorepo (pnpm, node-linker=isolated), installs their pinned deps,
// generates one probe file per matched pair, runs the vendored `tsc` CLI over
// the probes, and classifies the diagnostics into four buckets. It imports
// only node builtins + `typescript` + itself — same seam as capture.
// ===========================================================================

/** Wire protocol of a matched pair (drives the direction table). */
export type ProbeProtocol = 'http' | 'graphql' | 'socket' | 'pubsub';

/**
 * Type kind of a matched pair. `request`/`response` disambiguate HTTP body
 * direction (the confirmed inversion the direction table fixes); socket/pubsub
 * pairs are `both`.
 */
export type ProbeTypeKind = 'request' | 'response' | 'both';

/** One capture stub package to assemble into the check workspace. */
export interface CheckStubInput {
  /** Service name (used for scrub labels + pair endpoints). */
  service_name: string;
  /** Absolute path to the capture stub dir (package.json + types/ tree). */
  stub_dir: string;
}

/** One side of a matched pair: a service + the surface alias to probe. */
export interface CheckPairEndpoint {
  service_name: string;
  alias: string;
}

/**
 * One matched pair to verify. The direction table maps (protocol, type_kind)
 * to which endpoint is the `sent` value and which is the `expected` binding,
 * so callers pass semantic producer/consumer roles and never a raw direction.
 * (WP3 in Rust feeds protocol + type_kind; the table stays here, one place.)
 */
export interface CheckPairSpec {
  /** Stable caller key echoed back on the verdict; the pair_id is derived from it. */
  pair_key: string;
  protocol: ProbeProtocol;
  type_kind: ProbeTypeKind;
  producer: CheckPairEndpoint;
  consumer: CheckPairEndpoint;
}

/**
 * Four-bucket classifier output (pinned decision 7):
 *  - compatible: no diagnostics; the value-level assignment holds.
 *  - incompatible: an assignment-class diagnostic (TS2322/2741/...) — real
 *    compiler text is the report.
 *  - unverifiable: a side decayed to unknown/never, a surface export is
 *    missing/renamed, or a stub tree carries its own diagnostics (poison).
 *  - gate_caught_baked_any: a side resolved to `any` (the IsAny probe gate
 *    fired) — the backstop that stops a baked-any reading as compatible.
 */
export type VerdictBucket =
  | 'compatible'
  | 'incompatible'
  | 'unverifiable'
  | 'gate_caught_baked_any';

export interface CheckVerdict {
  /** Deterministic FNV-1a hash of the pair (never a temp path). */
  pair_id: string;
  /** Caller key, echoed for the WP3 verdict join. */
  pair_key: string;
  bucket: VerdictBucket;
  /**
   * For gate/import buckets: which side and which gate fired, e.g.
   * `producer:any`, `consumer:unknown`, `import:producer`. Absent for
   * compatible.
   */
  gate?: string;
  /** User-facing message: scrubbed real TS text, or a synthesized reason.
   * Never contains absolute paths or scan internals. Absent for compatible. */
  diagnostic?: string;
  /** TS diagnostic codes attributed to this pair's probe, sorted. */
  codes: number[];
}

/**
 * A service degraded SERVICE-WIDE: install failure, or a stub-tree diagnostic
 * that could not be attributed to any alias's import closure (#438). Poison
 * contained to specific aliases does NOT appear here — those pairs carry their
 * own `poison:*` verdicts while the service's clean pairs verify normally. So
 * absence from this list is not a "fully verified" signal; read per-pair
 * verdicts for that.
 */
export interface DegradedService {
  service_name: string;
  reason: string;
}

export interface CheckResult {
  success: boolean;
  /** Scratch workspace directory (kept unless caller cleans it). */
  workspace_dir: string;
  /** `pnpm` when isolation held; `unavailable` when the vendored pnpm is
   * missing (soundness over availability — pinned design, Check step 2). */
  isolation: 'pnpm' | 'unavailable';
  install_ok: boolean;
  /** Scrubbed install-failure summary when install_ok is false. */
  install_error?: string;
  ts_version: string;
  /** Verdicts, sorted by pair_id for byte-stable output. */
  verdicts: CheckVerdict[];
  degraded_services: DegradedService[];
  errors: string[];
}

export interface CheckOptions {
  stubs: CheckStubInput[];
  pairs: CheckPairSpec[];
  /** Parent dir for the scratch workspace (default: os.tmpdir()). */
  workspaceRoot?: string;
  /** Absolute path to the vendored pnpm binary. Defaults to the sidecar's
   * own node_modules/.bin/pnpm resolved from this bundle's location. */
  pnpmPath?: string;
  /** Absolute path to the tsc CLI. Defaults to the sidecar's own
   * node_modules/.bin/tsc. Tests inject a stand-in to pin the
   * abnormal-termination path. */
  tscPath?: string;
  /** Delete the scratch workspace before returning (default true). Tests that
   * inspect the assembled tree pass false. */
  cleanup?: boolean;
}

/** Progress phases emitted over the async install protocol. */
export type CheckProgressPhase = 'assembling' | 'installing' | 'checking';
