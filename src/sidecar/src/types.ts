/**
 * Type definitions for the sidecar message protocol
 * These types define the JSON messages exchanged between Rust and the Node.js sidecar
 */

import type { ResponseModes } from './response-modes.js';
import type { ResultLabel } from './capture/api.js';

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
  | 'receiver_type'     // Type of the RECEIVER of a member call (carrick#695)
  | 'request_config';   // A request call's options literal: does it write the `member` that carries the body? (carrick#2674)

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
  /**
   * carrick#2111: the type the model named for a producer's response, by the
   * alias of the request that reads the same response. The inferrer answers
   * how each relates to what it read (`InferredType.model_symbol_relation`).
   */
  model_symbols?: ModelSymbol[];
  /**
   * carrick#2674: the service's settled server-framework claims. Each is
   * verified again in the service's program, and the members the verified
   * ones name are what a route handler's own parameters are read through:
   * the reply a `send` op's payload slot states, and the request body a
   * receive op's `request.body` path names. Absent or empty, nothing is read.
   */
  server_claims?: ServerClaims;
}

/** The server-framework claims an `infer` request reads handler parameters through (carrick#2674). */
export interface ServerClaims {
  /** The service root module resolution starts from, as on `verify_library_claims`. */
  from_dir: string;
  scan_root?: string;
  /**
   * As the client sends them: each is read through the library-check schema
   * on its own, and a refused one is dropped (`settledServerChecks`).
   */
  checks: unknown[];
  /**
   * Per package specifier, the type-only packages the store says its surface
   * re-exports (the store's `type_packages`): a reply member declared in one
   * of them is the package's own.
   */
  type_packages?: Record<string, string[]>;
}

/** A type the model named for the row `alias` is for (carrick#2111). */
export interface ModelSymbol {
  alias: string;
  symbol_name: string;
  /** The file the model says declares or exports it. */
  source_file: string;
}

/**
 * carrick#2111: how the type the model named for a producer's response
 * relates to the compiler's type of what the handler sends. The compiler
 * decides, by assignability both ways (`symbol-relation.ts`).
 *
 * Reported only where both resolve and the read holds no `any` or `unknown`.
 * No entry means nothing was compared, and the model's symbol stands.
 */
export interface ModelSymbolRelation {
  /**
   * `same`: one type. `list`: the read is a list of the symbol, `depth`
   * levels. `member`: the symbol is one member of the read's union.
   * `nullable`: the read is the symbol or `null`/`undefined`. `different`:
   * anything else.
   */
  relation: 'same' | 'list' | 'member' | 'nullable' | 'different';
  /** List levels of the read around the symbol; present for `list`. */
  depth?: number;
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
 * Server mode only (carrick#2879): the first frame on every connection. A
 * daemon whose build id differs refuses it and closes the connection, so a
 * client never reads an answer from another build.
 */
export interface HelloRequest extends BaseRequest {
  action: 'hello';
  /** The build id of the sidecar the client would have spawned (`--build-id`). */
  build_id: string;
}

/** Server mode only: what the daemon holds. */
export interface StatusRequest extends BaseRequest {
  action: 'status';
}

/** Server mode only: stop the daemon, removing its socket and record. */
export interface StopRequest extends BaseRequest {
  action: 'stop';
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
 * Write one row for each call, member access, comparison and identifier in
 * each of a service's source files to a gzip JSON-lines file (carrick#2405).
 * Record-only: nothing in a scan reads the file yet.
 */
export interface PositionRowsRequest extends BaseRequest {
  action: 'position_rows';
  /** Absolute paths of the service's source files, in the order written. */
  files: string[];
  /** The scanned repo's root: every path in the file is relative to it. */
  scan_root: string;
  /** Where the rows file goes. Written beside it under a temporary name, then renamed. */
  out_path: string;
  /** The service the rows are for, written in the header. */
  service: string;
}

/**
 * The implementations of the bodyless declarations some calls go through
 * (carrick#2432 slice B), asked of the analysis process: one batch per
 * service.
 */
export interface ImplementationsRequest extends BaseRequest {
  action: 'implementations';
  /** One question per call, each the span of its `call` (or `member`) position row. */
  queries: ImplementationsQuery[];
  /** The scanned repo's root: every name in an answer is relative to it. */
  scan_root: string;
}

export interface ImplementationsQuery {
  /** Absolute path of the file holding the call. */
  file: string;
  /** The row's span: UTF-16 code units from the start of the file. */
  start: number;
  end: number;
}

/** A declaration as a position row names one. */
export interface NamedDeclaration {
  symbol: string | null;
  decl_file: string | null;
  decl_line: number | null;
  decl_package: string | null;
}

/** Why an answer names no implementation, where it is not because there is none. */
export type ImplementationsReason =
  | 'not_in_program'
  | 'no_call'
  | 'no_symbol'
  | 'no_declaration'
  | 'has_body'
  | 'checker_threw';

export interface ImplementationsAnswer {
  /** The declaration the call goes through, as its row names it. */
  declaration: NamedDeclaration | null;
  /** By file, then position; empty when `too_many`. */
  implementations: NamedDeclaration[];
  /** More than the cap (16) were found, and none is listed. */
  too_many: boolean;
  reason?: ImplementationsReason;
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
 * the instance (`maker`), the one the scope member bound (`scope`), or the
 * key path the op's value sits under in a `collects` object (`key`, #2474
 * R6a).
 */
export interface BoundName {
  bound: 'maker' | 'scope' | 'key';
}

/**
 * Where a router member takes its procedures (#2474 R6a, contract amendment
 * 3 on carrick#2434): an object at argument `arg` (or its property `key`)
 * whose property values are procedures or nested collections, each named by
 * its key; nested keys join with `join`.
 */
export interface Collects extends ClaimSlot {
  join: string;
}

/**
 * The function or client a call runs (label structure v2, carrick#2434): a
 * slot of the call itself, or, with `bound: 'maker'`, a slot of the call that
 * made the receiver (a mutation object whose `mutate` runs the function its
 * maker was given).
 */
export interface RunsSlot extends ClaimSlot {
  bound?: 'maker';
}

/**
 * What a receive op's handler is handed (label structure v2, carrick#2434):
 * its parameter `param` is an instance of the package's export `export`, on
 * `side` of the connection (a server's connection handler is given the
 * server end of a socket).
 */
export interface Yields {
  param: number;
  export: string;
  side: string;
}

/**
 * Where a route handler's request holds its path and method (#2474 R0): the
 * handler's parameter `param` is the request, and each of the others is a
 * member path read off it, relative to that parameter (`['path']`,
 * `['raw', 'url']`). At least one of `path` or `url`.
 */
export interface RequestAt {
  param: number;
  path?: string[];
  url?: string[];
  method?: string[];
  /** The member that holds the request's body (carrick#2674). */
  body?: string[];
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
      /** The function or client the maker runs (label v2). */
      runs?: ClaimSlot;
      key_labels?: Record<string, KeyLabel>;
      name_scope?: NameScope;
      picker?: string;
      /** Which part of what the maker returns holds the value its function resolves to; read by a `result` claim, never by this one. */
      result?: ResultShape;
    }
  | {
      /** A member that returns a receiver bound to a name (a channel, room or queue). */
      kind: 'scope';
      member: string;
      /** Exactly one of `name` and `collects` (#2474 R6a). */
      name?: ClaimSlot;
      path?: string[];
      on?: ClaimOn;
      of?: string;
      key_labels?: Record<string, KeyLabel>;
      name_scope?: NameScope;
      picker?: string;
      /**
       * Route fields (#2474 R0): the member runs the function here and hands
       * it, as `yields` says, an instance bound under `name`. Comes with
       * `yields`.
       */
      runs?: ClaimSlot;
      yields?: Yields;
      /** Route fields (#2474 R0): the instance handed here is served under `name`. */
      mounts?: ClaimSlot;
      /** Procedure routers (#2474 R6a): the member collects procedures by key and returns the collection. */
      collects?: Collects;
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
      /**
       * HTTP (label v2): the method sent when the call names none. Beside
       * `method_key`, the default when that option is absent; alone, the one
       * method the call sends whatever its member is called.
       */
      method_default?: string;
      /** HTTP (label v2): where the base URL the path joins sits. */
      base?: ClaimSlot;
      /** Label v2: the function or client the op runs. */
      runs?: RunsSlot;
      /** Label v2, `receive` only: what the handler is handed. */
      yields?: Yields;
      /** Route fields (#2474 R0), `receive` only: where the handler's request holds its path and method. */
      request?: RequestAt;
      /** Which part of what the call returns holds the response; read by a `result` claim, never by this one. */
      result?: ResultShape;
    }
  | {
      /**
       * Which part of what a call returns holds the response body, judged over
       * every overload of `member` (after `path`) on the receiver; `member`
       * null is the receiver itself called (carrick#2817). `form` says whether
       * the call is a maker's or an op's.
       */
      kind: 'result';
      form: 'make' | 'op';
      member: string | null;
      path?: string[];
      on?: ClaimOn;
      of?: string;
      picker?: string;
      result: ResultShape;
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
  /** The scanned repo's root, as on `init`: a verdict's `declarations` are named from it, as position rows are. */
  scan_root?: string;
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
  | HelloRequest
  | StatusRequest
  | StopRequest
  | ListProgramFilesRequest
  | AddProgramFilesRequest
  | PositionRowsRequest
  | ImplementationsRequest;

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
  /**
   * carrick#1841: the symbol names what a consumer receives. A symbol that is
   * a response table keyed by status code (`{ 200: Item }`) then bundles as
   * its 2xx body, never as the table.
   */
  consumer_response?: boolean;
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
  /** The member of a request options literal that carries the body (`request_config`). */
  member?: string;
  /**
   * carrick#2818: the library store's label for the call's result, naming the
   * part that holds the response body. Read for `call_result`.
   */
  result?: ResultLabel;
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

/** Response for hello: the daemon's build id and process. */
export interface HelloResponse extends BaseResponse {
  status: 'success';
  build_id: string;
  pid: number;
}

/** One service a daemon holds: the root and scan root its `init` named. */
export interface HeldService {
  repo_root: string;
  scan_root: string | null;
  /** Whether a request has built its program yet (`init` builds nothing). */
  built: boolean;
  /**
   * How often the files its programs were read from had changed before a
   * request (carrick#2880): read again into the held programs (`refreshes`),
   * or the service built again from its `init` (`drops`).
   */
  freshness: { refreshes: number; drops: number };
  /**
   * Its kept `infer` items (carrick#2881): how many are held, how many
   * requests' items were answered from them (`replayed`) and read again
   * (`recomputed`).
   */
  items: { held: number; replayed: number; recomputed: number };
  /** How often it was released to keep the heap under its cap; its next request builds it again. */
  evictions: number;
}

/**
 * One service whose capture a daemon holds (carrick#2882), with what its
 * captures reused, counted over the hold's life.
 */
export interface HeldCapture {
  repo_root: string;
  service_name: string;
  captures: number;
  /** Source files whose declarations were written again, and kept. */
  emitted: number;
  emit_kept: number;
  /** Stub files checked again, and whose diagnostics were kept. */
  checked: number;
  check_kept: number;
  /** Source files parsed again, and handed back unparsed. */
  parsed: number;
  parse_kept: number;
  /** How often it was released to keep the heap under its cap. */
  evictions: number;
}

/** Response for status. */
export interface StatusResponse extends BaseResponse {
  status: 'success';
  daemon: {
    pid: number;
    build_id: string;
    cli_version: string | null;
    socket: string;
    started_at: string;
    uptime_ms: number;
    idle_ms: number;
    services: HeldService[];
    captures: HeldCapture[];
    heap_used_mb: number;
    rss_mb: number;
    /** The heap held services are kept under (`CARRICK_TYPED_HEAP_CAP_MB`, default 3072). */
    heap_cap_mb: number;
    /** Only under `CARRICK_TYPED_VERIFY=1` (carrick#2886): requests answered warm and cold, and how they compared. */
    verify?: { checked: number; mismatches: number; order_only: number };
  };
}

/** Response for stop, written before the daemon exits. */
export interface StopResponse extends BaseResponse {
  status: 'success';
}

/** Response for implementations: one answer per question, in request order. */
export interface ImplementationsResponse extends BaseResponse {
  result?: { answers: ImplementationsAnswer[] };
  errors?: string[];
}

/** Response for position_rows: what was written, counted. */
export interface PositionRowsResponse extends BaseResponse {
  result?: PositionRowsCounts;
  errors?: string[];
}

export interface PositionRowsCounts {
  /** Source files in the request, each one line of the file. */
  files: number;
  rows: number;
  /** Rows whose position resolved to a symbol. */
  resolved: number;
  /** Rows whose type is `any` (flag 1). */
  any: number;
  /** Rows whose type is the compiler's error type (flag 2). */
  error_type: number;
  /** `jsx` rows: one per JSX opening or self-closing element. */
  jsx: number;
  /** The rows file's size on disk, compressed. */
  bytes: number;
  /** Time spent building programs and reading files into them. */
  load_ms: number;
  /** Time spent walking the files and writing the rows. */
  walk_ms: number;
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

/** Where a verified op's member is declared on its receiver, as a position row's `decl_file` and `decl_line`. */
export interface SemanticsDeclaration {
  decl_file: string;
  decl_line: number;
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
  /** Verified op verdicts only: the declarations its member resolves to on the receiver. */
  declarations?: SemanticsDeclaration[];
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
  /**
   * Which parts of the awaited return could hold the response body
   * (carrick#2817). Left out of the surface hash. Absent when the signature
   * returns nothing.
   */
  result_candidates?: SurfaceResultCandidates;
}

/** What a signature's return offers as its result, by rule R (`result-candidates.ts`). */
export interface SurfaceResultCandidates {
  /** The caller supplies the whole return as a type argument: `whole` is the only candidate. */
  whole_decided: boolean;
  /** Properties that carry a type parameter of the signature, sorted. */
  members: string[];
  /** Methods with no required argument that return a bare type parameter of their own, sorted. */
  parse: string[];
}

/**
 * Which part of what a call returns holds the response the server sent:
 * exactly one of the whole awaited return, one member of it, or the awaited
 * return of one of its parse methods (carrick#2817).
 */
export type ResultShape = { whole: true } | { member: [string] } | { parse: string };

export interface SurfaceMember {
  name: string;
  /** Declared by the receiver's home packages (not only inherited from another package). */
  own: boolean;
  /**
   * The members the parameters of a function this member takes declare
   * (carrick#2674): what a receive op's `request` field names its member
   * paths in. Under a cap of their own, apart from the main one.
   */
  handlers?: SurfaceHandler[];
  signatures: SurfaceSignature[];
}

/**
 * A function a member takes at argument `arg`, or at `key` of the options
 * object there, and the parameters of it that declare members. A handler the
 * member's overloads state alike is listed once, from the first overload.
 */
export interface SurfaceHandler {
  arg: number;
  key?: string;
  params: SurfaceHandlerParam[];
}

/** One parameter of a handler, with the members its type declares (none: not listed). */
export interface SurfaceHandlerParam {
  param: number;
  name: string;
  /** The parameter's type as the declarations print it, truncated. */
  type: string;
  members: SurfaceHandlerMember[];
}

/** A member of a handler's parameter, by path from the parameter, to two levels. */
export interface SurfaceHandlerMember {
  path: string[];
  type: string;
  optional: boolean;
  /** Callable (a method or a function-typed property). */
  function: boolean;
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
  | HelloResponse
  | StatusResponse
  | StopResponse
  | ListProgramFilesResponse
  | AddProgramFilesResponse
  | PositionRowsResponse
  | ImplementationsResponse
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
   * compatible, #306). Omitted when 0. Reported without an anchor symbol
   * too (carrick#1967): a list whose element prints structurally is still a
   * list, and at a handler's send the join copies its depth onto the model's
   * symbol.
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
   * carrick#2100, response inferences only: `true` when this inference was
   * owed an unwidened reading (its handler's path has literals to mark) and
   * the batch's time budget ran out before it was read. The reading's absence
   * then says nothing about the handler, so the check must not judge the
   * widened `type_string` as what the handler sends. Absent otherwise.
   */
  unwidened_out_of_budget?: true;
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
   * carrick#2111, a producer's response read only: how the type the model
   * named for this row relates to the type read here. Absent where the model
   * named none, or nothing could be compared.
   */
  model_symbol_relation?: ModelSymbolRelation;
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
   * carrick#2814, a route's request body or a subscriber's payload: the
   * handler is in reach and works when nothing is sent, because it defaults
   * the body or reads it only in ways `undefined` survives (`?.`, `?? {}`).
   * Absent when the handler dereferences it, reads it through something not
   * seen, or was not found.
   */
  absent_body_tolerated?: true;
  /**
   * carrick#2054, a response joined from two or more success bodies: the
   * request fields whose values pick among them (`reads`), and, for one
   * query parameter or body field whose every test the reading follows, the
   * union each stated value receives (`cases`). `type_string` is unchanged.
   * Absent when no request read separates the bodies, or when every value
   * receives the same union.
   */
  response_modes?: ResponseModes;
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
  /**
   * What each request came to, in request order: its inferred type or the
   * error it gave, and the files its unwidened reading rewrote and checked
   * (carrick#2881). The daemon keeps each one to answer it again.
   */
  items: InferItemOutcome[];
}

/** What one `infer` request item came to (carrick#2881). */
export interface InferItemOutcome {
  inferred?: InferredType;
  errors: string[];
  /**
   * The files holding a function on the item's unwidened path: the reading
   * rewrote and type-checked them beside every other item's, so another
   * item reaching them can change it. Absent when it had no reading to make.
   */
  pathFiles?: string[];
}
