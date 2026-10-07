/**
 * Type definitions for the sidecar message protocol
 * These types define the JSON messages exchanged between Rust and the Node.js sidecar
 */

// ============================================================================
// Inference Kind Enum
// ============================================================================

/**
 * The kind of type inference to perform
 */
export type InferKind =
  | 'function_return'   // Get return type of a function
  | 'expression'        // Get type of an expression
  | 'call_result'       // Get return type of a call expression
  | 'variable'          // Get type of a variable declaration
  | 'response_body'     // Find response body (.json()/.send()/ctx.body)
  | 'request_body'      // Find request body (req.body/ctx.request.body or call payloads)
  | 'signature_return'  // Function return for the signature hint — NO Promise/wrapper unwrapping
  | 'function_param'    // Type of a single named parameter (explicit or contextually inferred)
  | 'receiver_type';    // Type of the RECEIVER of a member call (carrick#695)

// ============================================================================
// Extraction Config Types (Agent-Informed Payload Unwrapping)
// ============================================================================

/**
 * A rule for unwrapping machinery/wrapper types to extract payload types.
 *
 * The unwrapping logic follows these priorities:
 * 1. Exact wrapperSymbols match extracts (gated on originModuleGlobs when present)
 * 2. machineryIndicators only trigger unwrap if originModuleGlobs also match
 * 3. Payload extraction: prefer generic args, then property paths
 * 4. A rule that matches but extracts nothing never blocks later rules; an
 *    origin-verified match with no recoverable payload collapses to `unknown`
 *    only after every rule has run
 */
export interface ExtractionRule {
  /**
   * Exact wrapper type/symbol names to unwrap. When originModuleGlobs is
   * also set, the symbol must originate from a matching module.
   * Examples: ["Response", "AxiosResponse", "Promise", "Observable"]
   */
  wrapperSymbols?: string[];

  /**
   * Method/property indicators that suggest a wrapper type.
   * Examples: ["status", "json", "send", "header", "cookie"]
   * Note: Only used in conjunction with originModuleGlobs to avoid false positives.
   */
  machineryIndicators?: string[];

  /**
   * Glob patterns for module origins. Only unwrap if the symbol's declarations
   * come from modules matching these patterns.
   * Examples: ["express", "express/*", "@types/express/*", "axios", "axios/*"]
   */
  originModuleGlobs?: string[];

  /**
   * Index of the generic type argument containing the payload.
   * Defaults to 0 (first type arg). It counts the parameters of the name
   * the rule matched: a type alias named in `wrapperSymbols` is read by its
   * own parameters, which it may order differently from the type it stands
   * for (carrick#1843).
   * Examples:
   *   - Response<T> → index 0
   *   - Map<K, V> → index 1 for values
   */
  payloadGenericIndex?: number;

  /**
   * Property path to extract payload when generics aren't available.
   * Examples: ["data"] for AxiosResponse.data, ["body"] for Response.body
   */
  payloadPropertyPath?: string[];

  /**
   * Whether to recursively unwrap nested wrappers.
   * Example: Promise<Response<T>> → unwrap both layers to get T
   */
  unwrapRecursively?: boolean;

  /**
   * Maximum unwrap depth when unwrapRecursively is true.
   * Defaults to 4 to prevent infinite loops.
   */
  maxDepth?: number;
}

/**
 * Configuration for extracting payload types from machinery wrappers.
 * Provided by the main Carrick process based on agent analysis.
 */
export interface ExtractionConfig {
  rules: ExtractionRule[];
}

// ============================================================================
// Pinned Dependency Snapshot Types
// ============================================================================

/**
 * A map of package names to exact pinned versions.
 * Used to ensure deterministic typechecking across CI runs.
 */
export interface PinnedDependencySnapshot {
  [packageName: string]: string;
}

// ============================================================================
// Tsconfig Snapshot Types
// ============================================================================

/**
 * A normalized/closed tsconfig object where all `extends` chains have been resolved.
 * Contains only the compiler options needed for surface checking.
 */
export interface TsconfigSnapshot {
  compilerOptions: {
    module?: string;
    moduleResolution?: string;
    target?: string;
    lib?: string[];
    types?: string[];
    typeRoots?: string[];
    jsx?: string;
    strict?: boolean;
    esModuleInterop?: boolean;
    skipLibCheck?: boolean;
    declaration?: boolean;
    declarationMap?: boolean;
    paths?: Record<string, string[]>;
    baseUrl?: string;
    [key: string]: unknown;
  };
}

// ============================================================================
// Request Types
// ============================================================================

/**
 * Base fields present in all requests
 */
interface BaseRequest {
  request_id: string;
}

/**
 * Initialize the sidecar with a repository root
 */
export interface InitRequest extends BaseRequest {
  action: 'init';
  repo_root: string;
  tsconfig_path?: string;
  /**
   * The scanned repo's root: with no tsconfig named or in `repo_root`, the
   * nearest `tsconfig.json` above `repo_root` up to this root types the
   * service (carrick#1776). Without it only `repo_root` is searched.
   */
  scan_root?: string;
  /** Optional tsconfig snapshot (closed/merged) - preferred over tsconfig_path */
  tsconfig_snapshot?: TsconfigSnapshot;
  /** Optional pinned dependencies for this repo */
  pinned_dependencies?: PinnedDependencySnapshot;
}

/**
 * Request to bundle explicit types from source files
 */
export interface BundleRequest extends BaseRequest {
  action: 'bundle';
  symbols: SymbolRequest[];
}

/**
 * Request to run the v2 "tsc as serializer" capture for one service.
 * Produces a types-only stub package (compiler-emitted declaration tree +
 * pinned deps) instead of a flattened structural string. The full contract
 * lives in ./capture/api.ts -- the seam between the sidecar and the v2
 * capture bundle.
 */
export interface CaptureV2Request extends BaseRequest {
  action: 'capture_v2';
  /** Absolute path to the producer repo root */
  repo_root: string;
  /** Service name used for the @carrick/<service> stub package */
  service_name: string;
  /** Anchors to alias in the surface entry (symbol / handler_return / infer) */
  anchors: import('./capture/api.js').CaptureAnchorRequest[];
  /** Directory to write the stub package into */
  out_dir: string;
  /** Optional explicit tsconfig path (defaults to <repo_root>/tsconfig.json) */
  tsconfig_path?: string;
  /** The scanned repo's root, as on `init` (carrick#1776). */
  scan_root?: string;
}

/**
 * Response for the capture_v2 action
 */
export interface CaptureV2Response extends BaseResponse {
  result?: import('./capture/api.js').CaptureStubResult;
  errors?: string[];
}

/**
 * Request to run the v2 "tsc as judge" check for a set of matched pairs.
 * Assembles the given capture stubs into a scratch synthetic monorepo and
 * returns one verdict per pair. The full contract lives in ./capture/api.ts --
 * the seam between the sidecar and the v2 capture/check bundle.
 */
export interface CheckV2Request extends BaseRequest {
  action: 'check_v2';
  /** Capture stub packages to assemble (one per participating service). */
  stubs: import('./capture/api.js').CheckStubInput[];
  /** Matched pairs to verify. */
  pairs: import('./capture/api.js').CheckPairSpec[];
  /** Parent dir for the scratch workspace (default: OS temp dir). */
  workspace_root?: string;
  /** Keep the assembled workspace on disk (default false; tests set true). */
  keep_workspace?: boolean;
}

/**
 * Response for the check_v2 action. Emitted as the terminal frame; the async
 * install protocol emits `status: 'progress'` keepalive frames before it.
 */
export interface CheckV2Response extends BaseResponse {
  result?: import('./capture/api.js').CheckResult;
  errors?: string[];
}

/**
 * Request to infer implicit types at specific locations
 */
export interface InferRequest extends BaseRequest {
  action: 'infer';
  requests: InferRequestItem[];
  /** Agent-generated extraction config for machinery unwrapping */
  extraction_config?: ExtractionConfig;
}

/**
 * Health check request
 */
export interface HealthRequest extends BaseRequest {
  action: 'health';
}

/**
 * Shutdown the sidecar process
 */
export interface ShutdownRequest extends BaseRequest {
  action: 'shutdown';
}

/**
 * List the root files the init'd project's program was built from, in order
 * (carrick#2027).
 */
export interface ListProgramFilesRequest extends BaseRequest {
  action: 'list_program_files';
}

/**
 * Add files to the programs that type them, in the given order
 * (carrick#2027).
 */
export interface AddProgramFilesRequest extends BaseRequest {
  action: 'add_program_files';
  /** Absolute paths, or relative to the init'd root. */
  files: string[];
}

/**
 * Request to resolve type definitions from a v2 capture stub package.
 * Returns both the original declaration and the compiler-expanded form for
 * each surface alias (design doc: `resolve_per_endpoint_definitions` is
 * re-pointed at the surface tree, a strictly richer source than the old
 * flattened bundle string).
 */
export interface ResolveDefinitionsRequest extends BaseRequest {
  action: 'resolve_definitions';
  /** Absolute path to the capture stub dir (package.json + types/ tree). */
  stub_dir: string;
  /** Surface type alias names to resolve */
  aliases: string[];
}

/**
 * One consumer call to retype with the producer's response type (carrick#1491).
 * The locator is the one the consumer's `call_result` inference used: a span,
 * or expression text near a line.
 */
export interface RetypeItem {
  /** Caller key, echoed on the outcome. */
  item_id: string;
  /** Absolute path, or relative to the init'd root. */
  file_path: string;
  line_number: number;
  span_start?: number;
  span_end?: number;
  expression_text?: string;
  expression_line?: number;
  /** The producer's response type as TypeScript text, fully inlined. */
  producer_type: string;
  /**
   * The producer's response as its handler returns it, literals read before
   * TypeScript widens them (carrick#1516), when that differs from
   * `producer_type`. Asked only when `producer_type` raised diagnostics.
   */
  producer_unwidened_type?: string;
  /** Judge the form JSON puts on the wire (an `http` response). */
  wire: boolean;
}

/**
 * Retype consumer calls with the producer's response type and report what the
 * consumer file's own type-check says about it. Needs `init`: it runs in the
 * consumer's real program.
 */
export interface RetypeCheckRequest extends BaseRequest {
  action: 'retype_check';
  items: RetypeItem[];
  /** Time the request may spend; items it does not reach abstain. */
  budget_ms?: number;
}

/**
 * One library-semantics claim to check against the package's own type
 * declarations, on one receiver (carrick#1564). The unit of verification is
 * the pair `(claim_id, receiver)`.
 */
export interface SemanticsCheck {
  claim_id: string;
  /** Module specifier, resolved from the request's `from_dir`. */
  package: string;
  /** `"default"` or a named export. */
  export: string;
  /** `"export"` or `"instance:<factory member>"`. */
  receiver: string;
  claim:
    | { kind: 'factory'; member: string; base_url_key: string }
    | { kind: 'verb'; member: string; method: string }
    | { kind: 'verb_body'; member: string; args: 'path_body' | 'path_options'; body_key?: string }
    | { kind: 'request'; member: string | null; args: 'config' | 'path_options'; url_key?: string; method_key: string }
    | { kind: 'request_body'; member: string | null; args: 'config' | 'path_options'; url_key?: string; method_key: string; body_key: string };
}

/**
 * Check library-semantics claims against each package's type declarations.
 * Needs `init`: module resolution runs under the service's compiler options,
 * in the service's program.
 */
export interface VerifyClientSemanticsRequest extends BaseRequest {
  action: 'verify_client_semantics';
  /** Absolute service root; module resolution starts here. */
  from_dir: string;
  checks: SemanticsCheck[];
  /** Checks not reached in time come back `unchecked` with reason `budget`. */
  budget_ms?: number;
}

/**
 * Where one part of a library call sits: argument `arg` (0-based), or, with
 * `key`, the property `key` of the object passed there (carrick#1616).
 */
export interface ClaimSlot {
  arg: number;
  key?: string;
}

/**
 * A name the call does not carry: the one the maker's `name` slot bound to
 * the instance (`maker`), or the one the scope member bound (`scope`).
 */
export interface BoundName {
  bound: 'maker' | 'scope';
}

/** The closed role list; the role alone picks the checks a claim needs. */
export type LibraryRole =
  | 'http_client'
  | 'graphql_client'
  | 'broker'
  | 'in_process_bus'
  | 'socket'
  | 'server_framework'
  | 'none';

/** What an `op` claim does on the wire. */
export type LibraryOp = 'request' | 'send' | 'receive' | 'execute';

/** Which receivers an op, scope or reserved name is claimed on. */
export type ClaimOn = 'export' | 'instance' | 'both';

/**
 * Whether a string-accepting key of an options type is the name (design D2).
 * Which key is the name is the picker's answer; the verifier checks only that
 * every string-accepting key at the call is accounted for.
 */
export type KeyLabel = 'name' | 'not_name';

/** Where a name means something. Carried for the scanner; the verifier never reads it. */
export interface NameScope {
  scope: 'global' | 'service';
  namespace: string | null;
}

/**
 * One claim element, in the shape the store answers (contract on
 * carrick#1564, comment 5937606126, sections 2 and 3, as amendment 2,
 * comment 5939543981, changes it), tagged by `kind`. `picker` and
 * `name_scope` are carried and never read.
 *
 * `on` and `of` say which receivers an op, scope or reserved name applies
 * to, and an element carries at most one of them: `of` is exactly one
 * receiver, by its receiver id (`instance:new`, `instance:connect>scope:channel`);
 * `on` is the export, every instance of every maker, or both.
 */
export type LibraryClaim =
  | {
      /** How an instance is made: `member(...)` (`call`) or `new member(...)`; `member` null is the export itself. */
      kind: 'make';
      form: 'call' | 'new';
      member: string | null;
      /** An HTTP base URL, or a broker's address. */
      base?: ClaimSlot;
      /** A prefix every name the instance sends gets. */
      prefix?: ClaimSlot;
      /** A definition's id or a queue's name: bound to every op that names `{ bound: 'maker' }`. */
      name?: ClaimSlot;
      handler?: ClaimSlot;
      key_labels?: Record<string, KeyLabel>;
      name_scope?: NameScope;
      picker?: string;
    }
  | {
      /** A member that returns a receiver bound to a name (a channel, room or queue). */
      kind: 'scope';
      member: string;
      name: ClaimSlot;
      path?: string[];
      on?: ClaimOn;
      of?: string;
      key_labels?: Record<string, KeyLabel>;
      name_scope?: NameScope;
      picker?: string;
    }
  | {
      kind: 'op';
      op: LibraryOp;
      /** null: the receiver itself (after `path`) is called. */
      member: string | null;
      /** The members walked from the receiver to the object `member` sits on (`client.tasks.trigger`). */
      path?: string[];
      on?: ClaimOn;
      /** The one receiver the op acts on, by its receiver id. */
      of?: string;
      name?: ClaimSlot | BoundName;
      payload?: ClaimSlot;
      handler?: ClaimSlot;
      ack?: ClaimSlot;
      key_labels?: Record<string, KeyLabel>;
      name_scope?: NameScope;
      picker?: string;
      /** HTTP: an options object sits at `arg` (no `key`). */
      options?: ClaimSlot;
      /** HTTP: the method the member always sends. */
      method?: string;
      /** HTTP: where the method sits. */
      method_key?: ClaimSlot;
    }
  | {
      /** A name the library emits itself, spelled in one of `member`'s parameters. */
      kind: 'reserved';
      member: string;
      name: string;
      path?: string[];
      on?: ClaimOn;
      of?: string;
      picker?: string;
    };

/**
 * One claim to check against the package's own declarations, on one receiver.
 * The receiver is `export`, `instance:<member>` (what `export.member(...)`
 * returns), `instance:()` (what calling the export returns), `instance:new`
 * (what `new export(...)` builds) or `instance:new:<member>` (what
 * `new export.member(...)` builds), optionally followed by
 * `>scope:<path.member>` (what that scope member returns). The unit is
 * `(claim_id, receiver)`.
 */
export interface LibraryCheck {
  claim_id: string;
  /** The module specifier as the service imports it (`pkg` or `pkg/sub`). */
  package: string;
  export: string;
  role: LibraryRole;
  receiver: string;
  claim: LibraryClaim;
}

/**
 * Check library claims of every role against each package's own
 * declarations (carrick#1616). `verify_client_semantics` answers HTTP claims
 * through the same verifier.
 */
export interface VerifyLibraryClaimsRequest extends BaseRequest {
  action: 'verify_library_claims';
  from_dir: string;
  checks: LibraryCheck[];
  /** Checks not reached in time come back `unchecked` with reason `budget`; null or absent is the default. */
  budget_ms?: number | null;
}

/**
 * List each package's declared surface the way the verifier reads it
 * (carrick#1660): the input a model picks claims from, and the full-surface
 * hash the shared library store keys its answers on.
 */
export interface ListLibrarySurfaceRequest extends BaseRequest {
  action: 'list_library_surface';
  from_dir: string;
  /** Module specifiers: a package, its subpaths, or a runtime module (`node:events`). */
  packages: string[];
  /** Entries kept per package (exports, receivers, members, signatures, parameters and keys each count one); default 1000. */
  max_entries?: number;
  /** Per package, the only exports to list (the ones the service imports); absent lists every value export. */
  exports?: Record<string, string[]>;
}

/**
 * Union type for all possible sidecar requests
 */
export type SidecarRequest =
  | RetypeCheckRequest
  | VerifyClientSemanticsRequest
  | VerifyLibraryClaimsRequest
  | ListLibrarySurfaceRequest
  | InitRequest
  | BundleRequest
  | CaptureV2Request
  | CheckV2Request
  | InferRequest
  | ResolveDefinitionsRequest
  | HealthRequest
  | ShutdownRequest
  | ListProgramFilesRequest
  | AddProgramFilesRequest;

/**
 * Request for a specific symbol to be bundled
 */
export interface SymbolRequest {
  /** The name of the symbol (type, interface, class, etc.) */
  symbol_name: string;
  /** The source file path (relative to repo root) */
  source_file: string;
  /** Optional alias for the exported type */
  alias?: string;
  /**
   * Wrap the bundled symbol in this many TS array levels (#248). A GraphQL SDL
   * producer field `[Order!]!` backed by `interface Order` bundles `Order` with
   * `array_depth: 1`, so the sidecar emits `Order[]`: the element type carries
   * the shape, the SDL list marker carries the depth. Omitted/`0` bundles the
   * symbol as-is (the HTTP/socket/consumer default).
   */
  array_depth?: number;
}

/**
 * Request for type inference at a specific location
 */
export interface InferRequestItem {
  /** Path to the file (relative to repo root) */
  file_path: string;
  /** Line number (1-based) for context and alias generation */
  line_number: number;
  /** Start byte offset of the target expression (from SWC spans) */
  span_start?: number;
  /** End byte offset of the target expression (from SWC spans) */
  span_end?: number;
  /** Verbatim expression text to locate in source (from Gemini) */
  expression_text?: string;
  /** Line number where the expression starts (from Gemini) */
  expression_line?: number;
  /** The kind of inference to perform */
  infer_kind: InferKind;
  /** Optional alias for the inferred type */
  alias?: string;
  /** Target parameter name for `function_param` inference. */
  param_name?: string;
}

// ============================================================================
// Response Types
// ============================================================================

/**
 * Response status
 */
export type ResponseStatus = 'success' | 'error' | 'ready' | 'not_ready';

/**
 * Base response fields
 */
interface BaseResponse {
  request_id: string;
  status: ResponseStatus;
}

/**
 * Response for init action
 */
export interface InitResponse extends BaseResponse {
  status: 'ready' | 'error';
  init_time_ms?: number;
  errors?: string[];
}

/**
 * Response for bundle action
 */
export interface BundleResponse extends BaseResponse {
  /** The bundled .d.ts content */
  dts_content?: string;
  /** Manifest mapping aliases to their type strings */
  manifest?: ManifestEntry[];
  /** Individual symbol failures */
  symbol_failures?: SymbolFailure[];
  /** General errors */
  errors?: string[];
}

/**
 * Response for infer action
 */
export interface InferResponse extends BaseResponse {
  /** Successfully inferred types */
  inferred_types?: InferredType[];
  /** How long the requests took, and which were slowest (carrick#1985) */
  infer_timing?: InferTiming;
  /** General errors */
  errors?: string[];
}

/**
 * How long one request of an `infer` batch took (carrick#1985).
 *
 * It names the request and measures its answer. It never holds the answer:
 * the type a request printed is here as a length only.
 */
export interface InferSlotTiming {
  /** The alias the request carried, when it carried one */
  alias?: string;
  /** The file the request named, as it named it */
  file_path: string;
  /** The line the request named */
  line_number: number;
  /** The kind of inference the request asked for */
  infer_kind: InferKind;
  /**
   * Wall time, in milliseconds, from the batch taking the request up to it
   * keeping the request's answer or its error.
   */
  ms: number;
  /**
   * Of `ms`, the time in the compiler call that computes the type: a
   * function's `getReturnType()`, a parameter's `getType()`. It is told
   * apart for `signature_return` and `function_param`, and is 0 for the
   * other kinds, whose time in the compiler stays in `ms` alone.
   */
  type_ms: number;
  /**
   * Of `ms`, the time in the inferrer's print of a type to text. Truncation
   * is off, so a type is printed at its whole length. For
   * `signature_return` and `function_param` this is every print the request
   * makes; the other kinds also print through the structural expander, and
   * that time stays in `ms` alone.
   */
  print_ms: number;
  /** Characters in the `type_string` the request printed; 0 when it printed none */
  printed_length: number;
  /**
   * No earlier request named this file to the project that answered. Such a
   * request does the work the file's later ones reuse: the index of the
   * file's functions, and every type the checker resolves on the way that it
   * had not resolved before.
   */
  first_in_file: boolean;
}

/**
 * The timing of the requests of one `infer` batch (carrick#1985).
 */
export interface InferTiming {
  /** Requests the batch was done with: answered, refused, skipped or failed */
  slots: number;
  /** Their wall times, added up, in milliseconds */
  slots_ms: number;
  /** Of `slots_ms`, the time computing types (each request's `type_ms`) */
  type_ms: number;
  /** Of `slots_ms`, the time printing types (each request's `print_ms`) */
  print_ms: number;
  /** How many of them were the first asked of their file */
  first_in_file_slots: number;
  /** The wall times of those, added up, in milliseconds */
  first_in_file_ms: number;
  /** The slowest requests, slowest first: `SLOWEST_SLOTS` of them at most */
  slowest: InferSlotTiming[];
  /** The request that printed the longest type; absent when none printed one */
  longest_printed?: InferSlotTiming;
}

/**
 * Response for resolve_definitions action
 */
export interface ResolveDefinitionsResponse extends BaseResponse {
  /** Successfully resolved definitions */
  definitions?: ResolvedDefinitionResult[];
  /** Errors during resolution */
  errors?: string[];
}

/**
 * A single resolved type definition
 */
export interface ResolvedDefinitionResult {
  type_alias: string;
  /** Original declaration text as written */
  definition: string;
  /** Compiler-expanded form with all types fully inlined */
  expanded: string;
}

/**
 * Response for health action
 */
export interface HealthResponse extends BaseResponse {
  status: 'ready' | 'not_ready';
  init_time_ms?: number;
}

/**
 * Response for shutdown action
 */
export interface ShutdownResponse extends BaseResponse {
  status: 'success';
}

/**
 * Response for list_program_files action
 */
export interface ListProgramFilesResponse extends BaseResponse {
  /** The program's root files, as absolute paths, in order. */
  files?: string[];
  errors?: string[];
}

/**
 * Response for add_program_files action
 */
export interface AddProgramFilesResponse extends BaseResponse {
  /** How many of the given files were added. */
  added?: number;
  errors?: string[];
}

/**
 * Error response
 */
export interface ErrorResponse extends BaseResponse {
  status: 'error';
  errors: string[];
}

/** A diagnostic the retype ADDED to the consumer file, at its original line. */
export interface RetypeDiagnostic {
  line: number;
  code: number;
  message: string;
}

/**
 * What the consumer file's type-check said once the call stated the
 * producer's response type.
 *
 * - `mismatch`: the rewrite added diagnostics; each is a place the consumer
 *   uses something the producer's response does not provide.
 * - `agrees`: it added none.
 * - `wider`: the published type added diagnostics and the handler's
 *   unwidened return added none (carrick#1516): the producer's type is wider
 *   than what it sends. `diagnostics` are the published type's.
 * - `abstain`: the check could not be made; `reason` says why.
 */
export interface RetypeOutcome {
  item_id: string;
  outcome: 'mismatch' | 'agrees' | 'wider' | 'abstain';
  diagnostics: RetypeDiagnostic[];
  reason?: string;
}

export interface RetypeCheckResponse extends BaseResponse {
  outcomes?: RetypeOutcome[];
  errors?: string[];
}

/**
 * The verdict on one `(claim_id, receiver)` pair.
 *
 * - `verified`: the declarations satisfy the claim.
 * - `failed`: the declarations resolved and contradict it.
 * - `unchecked`: the declarations could not be read.
 */
export interface SemanticsResult {
  claim_id: string;
  receiver: string;
  verdict: 'verified' | 'failed' | 'unchecked';
  /** Absent exactly when verified. */
  reason?: string;
}

/** How one package of the request resolved, for logs. */
export interface SemanticsModule {
  package: string;
  resolved_file?: string;
  installed_version?: string;
  reason?: string;
}

export interface VerifyClientSemanticsResponse extends BaseResponse {
  /** Exactly one per check, in request order. */
  semantics?: SemanticsResult[];
  semantics_modules?: SemanticsModule[];
  errors?: string[];
}

/** Library-claim verdicts (carrick#1616): the same objects, under the contract's names. */
export interface VerifyLibraryClaimsResponse extends BaseResponse {
  /** Exactly one per check, in request order. */
  verdicts?: SemanticsResult[];
  modules?: SemanticsModule[];
  /** Wall time of the verification, program build included. */
  duration_ms?: number;
  errors?: string[];
}

/** What one parameter position takes, read with the verifier's predicates. */
export interface SurfaceParam {
  name: string;
  optional: boolean;
  rest: boolean;
  /** The parameter's type as the declarations print it, truncated. */
  type: string;
  accepts_string: boolean;
  /** A function type with a declared signature. */
  function: boolean;
  /** Declared keys, when it is an object type. */
  keys?: SurfaceKey[];
  /** String literals the slot spells (an overload's literal, a `keyof` map's keys). */
  literals?: string[];
}

export interface SurfaceKey {
  name: string;
  /** Declared optional (`key?:`). */
  optional: boolean;
  accepts_string: boolean;
  function: boolean;
}

export interface SurfaceSignature {
  params: SurfaceParam[];
  returns: string;
}

export interface SurfaceMember {
  name: string;
  /** Declared by the receiver's home packages (not only inherited from another package). */
  own: boolean;
  signatures: SurfaceSignature[];
}

/** One receiver the verifier can read claims on, in its receiver grammar, with what it declares. */
export interface SurfaceReceiver {
  receiver: string;
  call?: SurfaceSignature[];
  construct?: SurfaceSignature[];
  members: SurfaceMember[];
}

export interface SurfaceExport {
  export: string;
  receivers: SurfaceReceiver[];
}

/** One specifier's surface. */
export interface LibrarySurface {
  package: string;
  resolved_file?: string;
  installed_version?: string;
  /** Why nothing was listed (the verifier's module reasons). */
  reason?: string;
  /** Entries dropped by `max_entries`. */
  truncated: number;
  exports: SurfaceExport[];
}

export interface ListLibrarySurfaceResponse extends BaseResponse {
  /** One per requested specifier, in request order. */
  surfaces?: LibrarySurface[];
  /**
   * The full-surface hash: sha256 of the JSON array of `[package, exports]`
   * for every surface that listed at least one export, sorted by `package`.
   * Type text names the request's directory as `<root>`, so the same
   * packages hash the same wherever they are installed.
   */
  surface_sha256?: string;
  errors?: string[];
}

/**
 * Union type for all possible sidecar responses
 */
export type SidecarResponse =
  | RetypeCheckResponse
  | VerifyClientSemanticsResponse
  | VerifyLibraryClaimsResponse
  | ListLibrarySurfaceResponse
  | InitResponse
  | BundleResponse
  | CaptureV2Response
  | CheckV2Response
  | InferResponse
  | ResolveDefinitionsResponse
  | HealthResponse
  | ShutdownResponse
  | ListProgramFilesResponse
  | AddProgramFilesResponse
  | ErrorResponse;

/**
 * An entry in the type manifest
 */
export interface ManifestEntry {
  /** The alias or original name of the type */
  alias: string;
  /** The original symbol name */
  original_name: string;
  /** The source file where the type was found */
  source_file: string;
  /** The full type definition string */
  type_string: string;
  /** Whether this was an explicit annotation or inferred */
  is_explicit: boolean;
}

/**
 * An inferred type result
 */
export interface InferredType {
  /** The alias for this type (generated if not provided) */
  alias: string;
  /** The full TypeScript type string */
  type_string: string;
  /** Whether the type was explicitly annotated in source */
  is_explicit: boolean;
  /** Source location information */
  source_location: SourceLocation;
  /** The kind of inference that was performed */
  infer_kind: InferKind;
  /** The unwrapped/extracted payload type (if different from type_string) */
  payload_type_string?: string;
  /**
   * carrick#695: for `receiver_type`, the package that DECLARES the resolved
   * type, when its declaration file sits under a `node_modules` tree. Absent
   * for a type the workspace itself declares, and absent when the type did not
   * resolve — the caller must not read absence as "local".
   */
  declaring_package?: string;
  /**
   * carrick#695: for `receiver_type`, the awaited return type of the member
   * invoked on that receiver. A fact returned alongside the receiver, never a
   * classification on its own.
   */
  member_return_type?: string;
  /**
   * The deterministic source symbol of the resolved type (`Payment`), derived
   * from the ts-morph `Type`'s `getSymbol() || getAliasSymbol()` name with
   * TS/lib globals filtered out. Lets the manifest anchor (`primary_type_symbol`)
   * be filled without depending on the LLM. `undefined` when the resolved type
   * has no single user-defined symbol to anchor on. For array types the anchor
   * is the ELEMENT's symbol (`TimelineEvent` for `TimelineEvent[]`) — the array
   * type's own symbol is the builtin `Array`, never an anchor.
   */
  primary_type_symbol?: string;
  /**
   * Array levels the resolved type wraps around the anchor symbol (#306):
   * `TimelineEvent[]` reports `primary_type_symbol: 'TimelineEvent'` +
   * `array_depth: 1`. Lets `resolve_all_types` copy the use-site's array-ness
   * onto an explicit `SymbolRequest` for the same alias, which would otherwise
   * bundle the bare element and erase the array (array-vs-scalar scored
   * compatible, #306). Omitted when 0 or when there is no anchor symbol.
   */
  array_depth?: number;
  /**
   * Declaration file of `primary_type_symbol` (absolute path), when the
   * anchor symbol has a resolvable source declaration. Reported by every
   * infer kind that reports the symbol (carrick#1819): it is where a reader
   * finds the type an anchor names, and the scanner writes an anchor's home
   * from it. The file is the one that declares the symbol the type resolves
   * to, never a barrel that re-exports it.
   *
   * The scanner's pub/sub two-anchor arbitration (carrick#413) also reads it,
   * for a borrowed-anchor request only, to re-aim a demoted explicit
   * `SymbolRequest` at the tsc-witnessed payload type: the bundler requires
   * the symbol to be declared in, or re-exported by, the request's
   * `source_file`, and the inference is the only party that knows where that
   * is.
   */
  primary_type_symbol_source?: string;
  /**
   * Why this inference carries `any`/`unknown` (carrick#376), recorded at the
   * decision point that produced it rather than reconstructed downstream.
   *
   * The inferrer is the only layer that knows the difference between "the
   * compiler resolved this to `any`" and "the recovery declined to read an
   * unresolvable callee's argument because nothing said it was a serialiser".
   * That difference is the whole answer to "why is this endpoint `any`", so it
   * is recorded here and joined onto the manifest entry beside the capture
   * surface's own findings.
   *
   * Sorted by `path`; absent (not empty) when the type carries no top type.
   */
  any_provenance?: TypeProvenance[];
  /**
   * carrick#1516, response inferences only: the same inference re-read with
   * every literal on the handler's path kept at its literal type
   * (`unwidened.ts`). `type_string` is what the compiler infers and what the
   * index publishes; this is what the handler actually sends when TypeScript
   * widened a literal in it (`scope: string` published, `scope: 'all' |
   * 'specific'` sent). Absent when the two are the same, or when the reading
   * was dropped as unsound.
   */
  unwidened_type_string?: string;
  /**
   * carrick#1749, `call_result` only: present when the source itself states
   * the type of the body it reads, AT the read: a cast of the read
   * (`res.json() as Promise<T>`, a callback's `value as T`) or the annotated
   * declaration it initializes (`const data: T = await res.json()`). The
   * text in `type_string` is then that statement. Absent when no type is
   * stated, or when the only annotation is further away than the read.
   */
  stated_body?: StatedBody;
  /**
   * carrick#1836: the declarations behind the names `type_string` prints that
   * the request's file does not resolve. The printer writes names bare where
   * no scope reads them, so this is the only record of what they meant. A
   * name printed for two declarations is listed twice. Absent when there is
   * nothing to list.
   */
  printed_names?: PrintedName[];
  /**
   * carrick#1842, `call_result` only: the published `string` is the body read
   * as raw text, by a `.text()` read of the call's response or by a call whose
   * signature takes the literal `'text'` the source passes as its body format.
   * Raw text states no structural contract, so the check phase reads the pair
   * unverifiable rather than comparing `string` with the other side's body.
   * Absent when the body is not a raw-text read.
   */
  raw_text_read?: true;
  /**
   * carrick#1961: the node this answer was read from, for an answer whose
   * text holds `any` or `unknown` and whose node carries that same type. The
   * scanner does not carry such a text; it has the capture read a node
   * itself, so that the capture's record says which positions are open and
   * why. Left to the request's own locator, that node is the handler or the
   * registration the request was located at, and the capture prints the
   * function. With this, it reads the body. `file_path` is absolute and the
   * span is in this sidecar's numbering, as `span_start`/`span_end` are on a
   * request. Absent on every other answer.
   */
  reread_at?: {
    file_path: string;
    span_start: number;
    span_end: number;
    line_number: number;
  };
}

/**
 * What the source states a body read to be (carrick#1749). The scanner
 * compares `root` with the type symbol the model named for the same call: a
 * model symbol that is not the stated root names something other than the
 * body (an element of it, or a value the caller later computed from it), and
 * the source's own statement outranks it.
 */
export interface StatedBody {
  /**
   * The name the stated type is rooted at, once `Promise<...>`, array levels,
   * parentheses and `| null`/`| undefined` are peeled: `Order` for
   * `Promise<Order[] | null>`, `Page` for `Page<Order>`. Absent when the root
   * is not a named type (an object literal type, a union of several types).
   */
  root?: string;
  /** Declaration file (absolute path) of `root`, when it resolves to one. */
  root_source?: string;
  /** Array levels peeled on the way to `root`. Omitted when 0. */
  array_depth?: number;
  /**
   * Type arguments written at `root` (`Page<Order>` → 1). Omitted when 0.
   * `root` alone does not name the body such a statement states.
   */
  root_type_arguments?: number;
}

/**
 * Why a type carries `any`/`unknown` at a position, and where.
 *
 * Declared in the capture bundle (`./capture/api.ts`) because the capture
 * self-check is one of the two producers; re-exported here so the rest of the
 * sidecar reads it through the single sanctioned door rather than reaching
 * across the bundle seam.
 */
export type TypeProvenance = import('./capture/api.js').TypeProvenance;
/** carrick#1836: declared in the capture bundle, which reads it on a literal anchor. */
export type PrintedName = import('./capture/api.js').PrintedName;
export type TypeProvenanceReason = import('./capture/api.js').TypeProvenanceReason;

/**
 * Source location information for a type
 */
export interface SourceLocation {
  /** File path relative to repo root */
  file_path: string;
  /** Start line (1-based) */
  start_line: number;
  /** End line (1-based) */
  end_line: number;
  /** Start column (0-based) */
  start_column?: number;
  /** End column (0-based) */
  end_column?: number;
}

/**
 * Information about a symbol that failed to resolve
 */
export interface SymbolFailure {
  /** The symbol that failed */
  symbol_name: string;
  /** The source file where it was supposed to be */
  source_file: string;
  /** Reason for the failure */
  reason: string;
}

// ============================================================================
// Bundle Result (internal)
// ============================================================================

/**
 * Internal result from the bundler
 */
export interface BundleResult {
  /** Whether bundling was successful */
  success: boolean;
  /** The bundled .d.ts content */
  dts_content?: string;
  /** Manifest entries for successfully bundled types */
  manifest?: ManifestEntry[];
  /** Failures for individual symbols */
  symbol_failures?: SymbolFailure[];
  /** General error messages */
  errors?: string[];
}

/**
 * Internal result from the type inferrer
 */
export interface InferResult {
  /** Whether inference was successful */
  success: boolean;
  /** Successfully inferred types */
  inferred_types?: InferredType[];
  /** How long each request took, summed up (carrick#1985) */
  timing: InferTiming;
  /** General error messages */
  errors?: string[];
}
