/**
 * Zod validators for the sidecar message protocol
 * These validators ensure incoming JSON messages are correctly typed
 */

import { z } from 'zod';
import type {
  SidecarRequest,
  InferKind,
  SymbolRequest,
  InferRequestItem,
  LibraryCheck,
} from './types.js';

// ============================================================================
// Inference Kind Schema
// ============================================================================

export const InferKindSchema = z.enum([
  'function_return',
  'expression',
  'call_result',
  'variable',
  'response_body',
  'request_body',
  'signature_return',
  'function_param',
  'receiver_type',
  'request_config',
]);

// ============================================================================
// Extraction Config Schemas (New)
// ============================================================================

const ExtractionRuleSchema = z.object({
  wrapperSymbols: z.array(z.string()).optional(),
  machineryIndicators: z.array(z.string()).optional(),
  originModuleGlobs: z.array(z.string()).optional(),
  payloadGenericIndex: z.number().int().nonnegative().optional(),
  payloadPropertyPath: z.array(z.string()).optional(),
  unwrapRecursively: z.boolean().optional(),
  // Nonnegative, not positive: the Rust side accepts a model-emitted
  // maxDepth of 0 (a harmless no-op rule), and one rule rejected here would
  // fail the entire infer request.
  maxDepth: z.number().int().nonnegative().optional(),
});

const ExtractionConfigSchema = z.object({
  rules: z.array(ExtractionRuleSchema),
});

// ============================================================================
// Tsconfig Snapshot Schema
// ============================================================================

const TsconfigSnapshotSchema = z.object({
  compilerOptions: z.object({
    module: z.string().optional(),
    moduleResolution: z.string().optional(),
    target: z.string().optional(),
    lib: z.array(z.string()).optional(),
    types: z.array(z.string()).optional(),
    typeRoots: z.array(z.string()).optional(),
    jsx: z.string().optional(),
    strict: z.boolean().optional(),
    esModuleInterop: z.boolean().optional(),
    skipLibCheck: z.boolean().optional(),
    declaration: z.boolean().optional(),
    declarationMap: z.boolean().optional(),
    paths: z.record(z.array(z.string())).optional(),
    baseUrl: z.string().optional(),
  }).passthrough(),
});

// ============================================================================
// Pinned Dependency Snapshot Schema
// ============================================================================

const PinnedDependencySnapshotSchema = z.record(z.string());

// ============================================================================
// Symbol Request Schema
// ============================================================================

export const SymbolRequestSchema = z.object({
  symbol_name: z.string().min(1, 'Symbol name cannot be empty'),
  source_file: z.string().min(1, 'Source file cannot be empty'),
  alias: z.string().optional(),
  // #248: SDL list depth to wrap the bundled element type in (`Order` → `Order[]`).
  // No upper bound here deliberately: a batch invalid at this schema layer
  // fails the *whole* bundle request (see the `maxDepth` note above), which
  // is worse than one bad symbol. The bundler enforces MAX_ARRAY_DEPTH
  // per-symbol instead, so an over-bound depth becomes a normal
  // `symbol_failures` entry rather than sinking every other symbol in the
  // batch.
  array_depth: z.number().int().nonnegative().optional(),
  // carrick#1841: the symbol names a consumer's response (a status-keyed
  // response table bundles as its 2xx body).
  consumer_response: z.boolean().optional(),
});

// ============================================================================
// Infer Request Item Schema
// ============================================================================

export const InferRequestItemSchema = z.object({
  file_path: z.string().min(1, 'File path cannot be empty'),
  line_number: z.number().int().positive('Line number must be positive'),
  span_start: z.number().int().nonnegative('Span start must be non-negative').optional(),
  span_end: z.number().int().nonnegative('Span end must be non-negative').optional(),
  expression_text: z.string().optional(),
  expression_line: z.number().int().positive('Expression line must be positive').optional(),
  infer_kind: InferKindSchema,
  alias: z.string().optional(),
  param_name: z.string().optional(),
  // The body-carrying member a `request_config` item asks about.
  member: z.string().optional(),
});

/**
 * Per-item locator validation for infer requests.
 *
 * This is deliberately NOT part of the envelope schema: real runs batch
 * every alias of a repo into one infer request, and a single bad item
 * must produce a per-item error (the alias then pads to `unknown`
 * downstream), not reject the whole batch.
 *
 * Returns an error message, or null when the item is valid.
 */
export function validateInferRequestItem(item: InferRequestItem): string | null {
  if (
    item.span_start !== undefined &&
    item.span_end !== undefined &&
    item.span_end < item.span_start
  ) {
    return 'span_end must be greater than or equal to span_start';
  }
  const hasSpan = item.span_start !== undefined && item.span_end !== undefined;
  const hasText = item.expression_text !== undefined;
  // Function-anchored inference (function_return for file-based routes,
  // signature_return / function_param for the signature pass) locates the
  // function by line_number alone, so it does not require a span or text.
  // response_body / request_body also accept a line-only anchor: for a
  // named-handler route registration the scanner falls back to the registration
  // line, and the inferrer follows that line to the handler (`handlerAtLine`).
  // Worst case the line resolves nothing and the alias pads to `unknown` — the
  // same outcome rejection produced, but now the resolvable cases get through.
  const lineOnlyOk =
    item.infer_kind === 'function_return' ||
    item.infer_kind === 'signature_return' ||
    item.infer_kind === 'function_param' ||
    item.infer_kind === 'response_body' ||
    item.infer_kind === 'request_body';
  if (!hasSpan && !hasText && !lineOnlyOk) {
    return 'at least one of (span_start + span_end) or expression_text is required';
  }
  return null;
}

// ============================================================================
// Action-specific Request Schemas
// ============================================================================

const BaseRequestSchema = z.object({
  request_id: z.string().min(1, 'Request ID cannot be empty'),
});

export const InitRequestSchema = BaseRequestSchema.extend({
  action: z.literal('init'),
  repo_root: z.string().min(1, 'Repo root cannot be empty'),
  tsconfig_path: z.string().optional(),
  scan_root: z.string().min(1).optional(),
  tsconfig_snapshot: TsconfigSnapshotSchema.optional(),
  pinned_dependencies: PinnedDependencySnapshotSchema.optional(),
});

export const BundleRequestSchema = BaseRequestSchema.extend({
  action: z.literal('bundle'),
  symbols: z.array(SymbolRequestSchema).min(1, 'At least one symbol is required'),
});

/** v2 "tsc as serializer" capture (contract in ./capture/api.ts). */
const AnchorOriginSchema = z.enum([
  'llm-symbol',
  'deterministic-infer',
  'anchor-backfill',
  'manifest-placeholder',
]);

const CaptureAnchorRequestSchema = z.discriminatedUnion('kind', [
  z.object({
    kind: z.literal('symbol'),
    alias: z.string().min(1),
    symbol_name: z.string().min(1),
    source_file: z.string().min(1),
    anchor_origin: AnchorOriginSchema,
    array_depth: z.number().int().nonnegative().optional(),
    consumer_response: z.boolean().optional(),
  }),
  z.object({
    kind: z.literal('handler_return'),
    alias: z.string().min(1),
    symbol_name: z.string().min(1),
    source_file: z.string().min(1),
    anchor_origin: AnchorOriginSchema,
  }),
  z.object({
    kind: z.literal('infer'),
    alias: z.string().min(1),
    source_file: z.string().min(1),
    anchor_origin: AnchorOriginSchema,
    span_start: z.number().int().nonnegative().optional(),
    span_end: z.number().int().nonnegative().optional(),
    line_number: z.number().int().positive().optional(),
    expression_text: z.string().optional(),
    // Any string, the empty one included: a name no parameter has is the
    // capture's to demote, and refusing it here would fail every anchor of
    // the request.
    param_name: z.string().optional(),
    unwrap: z.enum(['awaited', 'none']).optional(),
  }),
  z.object({
    kind: z.literal('literal'),
    alias: z.string().min(1),
    type_text: z.string().min(1),
    anchor_origin: AnchorOriginSchema,
    source_file: z.string().min(1).optional(),
    printed_names: z
      .array(
        z.object({
          name: z.string().min(1),
          file: z.string().min(1),
          export_path: z.array(z.string().min(1)).min(1),
        })
      )
      .optional(),
    raw_text_read: z.literal(true).optional(),
    absent_body_tolerated: z.literal(true).optional(),
  }),
]);

export const CaptureV2RequestSchema = BaseRequestSchema.extend({
  action: z.literal('capture_v2'),
  repo_root: z.string().min(1, 'Repo root cannot be empty'),
  service_name: z.string().min(1, 'Service name cannot be empty'),
  anchors: z.array(CaptureAnchorRequestSchema).min(1, 'At least one anchor is required'),
  out_dir: z.string().min(1, 'Output dir cannot be empty'),
  tsconfig_path: z.string().optional(),
  scan_root: z.string().min(1).optional(),
});

const CheckStubInputSchema = z.object({
  service_name: z.string().min(1),
  stub_dir: z.string().min(1),
});

const CheckPairEndpointSchema = z.object({
  service_name: z.string().min(1),
  alias: z.string().min(1),
});

const CheckPairSpecSchema = z
  .object({
    pair_key: z.string().min(1),
    protocol: z.enum(['http', 'graphql', 'socket', 'pubsub']),
    type_kind: z.enum(['request', 'response', 'both']),
    producer: CheckPairEndpointSchema,
    consumer: CheckPairEndpointSchema,
    topic: z.string().min(1).optional(),
  })
  .superRefine((spec, ctx) => {
    // carrick#1979: the topic is the pub/sub operation's own key. It is
    // required there and refused elsewhere, so zod can never strip a stray one.
    if (spec.protocol === 'pubsub' && spec.topic === undefined) {
      ctx.addIssue({ code: 'custom', path: ['topic'], message: 'A pubsub pair requires a topic' });
    }
    if (spec.protocol !== 'pubsub' && spec.topic !== undefined) {
      ctx.addIssue({ code: 'custom', path: ['topic'], message: 'Only a pubsub pair carries a topic' });
    }
  });

export const CheckV2RequestSchema = BaseRequestSchema.extend({
  action: z.literal('check_v2'),
  stubs: z.array(CheckStubInputSchema).min(1, 'At least one stub is required'),
  pairs: z.array(CheckPairSpecSchema).min(1, 'At least one pair is required'),
  workspace_root: z.string().optional(),
  keep_workspace: z.boolean().optional(),
});

export const InferRequestSchema = BaseRequestSchema.extend({
  action: z.literal('infer'),
  requests: z.array(InferRequestItemSchema).min(1, 'At least one infer request is required'),
  extraction_config: ExtractionConfigSchema.optional(),
  model_symbols: z
    .array(
      z.object({
        alias: z.string().min(1),
        symbol_name: z.string().min(1),
        source_file: z.string().min(1),
      })
    )
    .optional(),
  // Declared below with the library claims; read lazily (carrick#2674).
  server_claims: z.lazy(() => ServerClaimsSchema).optional(),
});

export const HealthRequestSchema = BaseRequestSchema.extend({
  action: z.literal('health'),
});

export const ShutdownRequestSchema = BaseRequestSchema.extend({
  action: z.literal('shutdown'),
});

export const ListProgramFilesRequestSchema = BaseRequestSchema.extend({
  action: z.literal('list_program_files'),
});

export const AddProgramFilesRequestSchema = BaseRequestSchema.extend({
  action: z.literal('add_program_files'),
  files: z.array(z.string().min(1, 'File path cannot be empty')),
});

export const PositionRowsRequestSchema = BaseRequestSchema.extend({
  action: z.literal('position_rows'),
  files: z.array(z.string().min(1, 'File path cannot be empty')),
  scan_root: z.string().min(1, 'Scan root cannot be empty'),
  out_path: z.string().min(1, 'Out path cannot be empty'),
  service: z.string(),
});

export const ImplementationsRequestSchema = BaseRequestSchema.extend({
  action: z.literal('implementations'),
  queries: z.array(
    z.object({
      file: z.string().min(1, 'File path cannot be empty'),
      start: z.number().int().nonnegative(),
      end: z.number().int().nonnegative(),
    })
  ),
  scan_root: z.string().min(1, 'Scan root cannot be empty'),
});

export const ResolveDefinitionsRequestSchema = BaseRequestSchema.extend({
  action: z.literal('resolve_definitions'),
  stub_dir: z.string().min(1, 'Stub dir cannot be empty'),
  aliases: z.array(z.string().min(1)).min(1, 'At least one alias is required'),
});

const RetypeItemSchema = z.object({
  item_id: z.string().min(1),
  file_path: z.string().min(1),
  line_number: z.number().int().positive(),
  span_start: z.number().int().nonnegative().optional(),
  span_end: z.number().int().nonnegative().optional(),
  expression_text: z.string().optional(),
  expression_line: z.number().int().positive().optional(),
  producer_type: z.string().min(1),
  producer_unwidened_type: z.string().min(1).optional(),
  wire: z.boolean(),
});

export const RetypeCheckRequestSchema = BaseRequestSchema.extend({
  action: z.literal('retype_check'),
  items: z.array(RetypeItemSchema).min(1, 'At least one item is required'),
  budget_ms: z.number().int().nonnegative().optional(),
});

/** One library-semantics claim, as the scanner derives it (carrick#1564). */
const SemanticsClaimSchema = z.discriminatedUnion('kind', [
  z.object({
    kind: z.literal('factory'),
    member: z.string().min(1),
    base_url_key: z.string().min(1),
  }),
  z.object({
    kind: z.literal('verb'),
    member: z.string().min(1),
    method: z.string().min(1),
  }),
  z.object({
    kind: z.literal('verb_body'),
    member: z.string().min(1),
    args: z.enum(['path_body', 'path_options']),
    body_key: z.string().min(1).optional(),
  }),
  z.object({
    kind: z.literal('request'),
    member: z.string().min(1).nullable(),
    args: z.enum(['config', 'path_options']),
    url_key: z.string().min(1).optional(),
    method_key: z.string().min(1),
  }),
  z.object({
    kind: z.literal('request_body'),
    member: z.string().min(1).nullable(),
    args: z.enum(['config', 'path_options']),
    url_key: z.string().min(1).optional(),
    method_key: z.string().min(1),
    body_key: z.string().min(1),
  }),
]);

const SemanticsCheckSchema = z.object({
  claim_id: z.string().min(1),
  package: z.string().min(1),
  export: z.string().min(1),
  // Any other receiver answers `unchecked` (`receiver_invalid`) on its own
  // rather than failing the batch.
  receiver: z.string().min(1),
  claim: SemanticsClaimSchema,
});

export const VerifyClientSemanticsRequestSchema = BaseRequestSchema.extend({
  action: z.literal('verify_client_semantics'),
  from_dir: z.string().min(1),
  checks: z.array(SemanticsCheckSchema),
  budget_ms: z.number().int().nonnegative().optional(),
});

/** Where one part of a library call sits (carrick#1616). */
const ClaimSlotSchema = z
  .object({
    arg: z.number().int().nonnegative(),
    key: z.string().min(1).optional(),
  })
  .strict();

const BoundNameSchema = z.object({ bound: z.enum(['maker', 'scope', 'key']) }).strict();

/** Procedure routers (#2474 R6a): where a router member takes its procedures, and how nested keys join. */
const CollectsSchema = z
  .object({
    arg: z.number().int().nonnegative(),
    key: z.string().min(1).optional(),
    join: z.string().min(1),
  })
  .strict();

/**
 * Label structure v2 (carrick#2434). The schema takes the shape only: which
 * method word, side or export a field names is the verifier's per-check
 * verdict, since one element this schema refuses fails the whole request.
 */
const RunsSlotSchema = z
  .object({
    arg: z.number().int().nonnegative(),
    key: z.string().min(1).optional(),
    bound: z.literal('maker').optional(),
  })
  .strict();
const YieldsSchema = z
  .object({
    param: z.number().int().nonnegative(),
    export: z.string().min(1),
    side: z.string().min(1),
  })
  .strict();

/** Route fields (#2474 R0): member paths read off a handler's request parameter. */
const RequestAtSchema = z
  .object({
    param: z.number().int().nonnegative(),
    path: z.array(z.string().min(1)).optional(),
    url: z.array(z.string().min(1)).optional(),
    method: z.array(z.string().min(1)).optional(),
    body: z.array(z.string().min(1)).optional(),
  })
  .strict();

const ClaimOnSchema = z.enum(['export', 'instance', 'both']);
const MemberPathSchema = z.array(z.string().min(1));
const KeyLabelsSchema = z.record(z.string().min(1), z.enum(['name', 'not_name']));
/** Carried for the scanner, never read. */
const NameScopeSchema = z
  .object({ scope: z.enum(['global', 'service']), namespace: z.string().min(1).nullable() })
  .strict();

/**
 * One claim element in the shape the store answers, tagged by `kind`
 * (contract on carrick#1564, sections 2 and 3, as amendment 2 changes them).
 * A maker's parts are slots; `on` and `of` are checked against each other by
 * the verifier (`claim_invalid`), not here.
 */
const LibraryClaimSchema = z.discriminatedUnion('kind', [
  z
    .object({
      kind: z.literal('make'),
      form: z.enum(['call', 'new']),
      member: z.string().min(1).nullable(),
      base: ClaimSlotSchema.optional(),
      prefix: ClaimSlotSchema.optional(),
      name: ClaimSlotSchema.optional(),
      handler: ClaimSlotSchema.optional(),
      runs: ClaimSlotSchema.optional(),
      key_labels: KeyLabelsSchema.optional(),
      name_scope: NameScopeSchema.optional(),
      picker: z.string().optional(),
    })
    .strict(),
  z
    .object({
      kind: z.literal('scope'),
      member: z.string().min(1),
      // Exactly one of `name` and `collects`: the verifier's verdict (#2474 R6a).
      name: ClaimSlotSchema.optional(),
      path: MemberPathSchema.optional(),
      on: ClaimOnSchema.optional(),
      of: z.string().min(1).optional(),
      key_labels: KeyLabelsSchema.optional(),
      name_scope: NameScopeSchema.optional(),
      picker: z.string().optional(),
      runs: ClaimSlotSchema.optional(),
      yields: YieldsSchema.optional(),
      mounts: ClaimSlotSchema.optional(),
      collects: CollectsSchema.optional(),
    })
    .strict(),
  z
    .object({
      kind: z.literal('op'),
      op: z.enum(['request', 'send', 'receive', 'execute']),
      member: z.string().min(1).nullable(),
      path: MemberPathSchema.optional(),
      on: ClaimOnSchema.optional(),
      of: z.string().min(1).optional(),
      name: z.union([BoundNameSchema, ClaimSlotSchema]).optional(),
      payload: ClaimSlotSchema.optional(),
      handler: ClaimSlotSchema.optional(),
      ack: ClaimSlotSchema.optional(),
      key_labels: KeyLabelsSchema.optional(),
      name_scope: NameScopeSchema.optional(),
      picker: z.string().optional(),
      options: ClaimSlotSchema.optional(),
      method: z.string().min(1).optional(),
      method_key: ClaimSlotSchema.optional(),
      method_default: z.string().min(1).optional(),
      base: ClaimSlotSchema.optional(),
      runs: RunsSlotSchema.optional(),
      yields: YieldsSchema.optional(),
      request: RequestAtSchema.optional(),
    })
    .strict(),
  z
    .object({
      kind: z.literal('reserved'),
      member: z.string().min(1),
      name: z.string().min(1),
      path: MemberPathSchema.optional(),
      on: ClaimOnSchema.optional(),
      of: z.string().min(1).optional(),
      picker: z.string().optional(),
    })
    .strict(),
]);

const LibraryCheckSchema = z
  .object({
    claim_id: z.string().min(1),
    package: z.string().min(1),
    export: z.string().min(1),
    role: z.enum([
      'http_client',
      'graphql_client',
      'broker',
      'in_process_bus',
      'socket',
      'server_framework',
      'none',
    ]),
    // Any other receiver answers `unchecked` (`receiver_invalid`) on its own.
    receiver: z.string().min(1),
    claim: LibraryClaimSchema,
  })
  .strict();

/**
 * The server-framework claims an `infer` request reads handler parameters
 * through (carrick#2674). Each check is read on its own by
 * `settledServerChecks`: a refused one is dropped, never the request.
 */
const ServerClaimsSchema = z
  .object({
    from_dir: z.string().min(1),
    scan_root: z.string().min(1).optional(),
    checks: z.array(z.unknown()),
    type_packages: z.record(z.string().min(1), z.array(z.string().min(1))).optional(),
  })
  .strict();

/**
 * The checks of an `infer` request's server claims the library-check schema
 * takes, and how many it refused (carrick#2674). A refused claim reads
 * nothing; the others still do.
 */
export function settledServerChecks(checks: readonly unknown[]): { checks: LibraryCheck[]; refused: number } {
  const settled: LibraryCheck[] = [];
  let refused = 0;
  for (const check of checks) {
    const parsed = LibraryCheckSchema.safeParse(check);
    if (parsed.success) settled.push(parsed.data as LibraryCheck);
    else refused += 1;
  }
  return { checks: settled, refused };
}

export const VerifyLibraryClaimsRequestSchema = BaseRequestSchema.extend({
  action: z.literal('verify_library_claims'),
  from_dir: z.string().min(1),
  scan_root: z.string().min(1).optional(),
  checks: z.array(LibraryCheckSchema),
  // The contract's sample sends `null` for "no budget of the request's own".
  budget_ms: z.number().int().nonnegative().nullable().optional(),
});

export const ListLibrarySurfaceRequestSchema = BaseRequestSchema.extend({
  action: z.literal('list_library_surface'),
  from_dir: z.string().min(1),
  packages: z.array(z.string().min(1)),
  max_entries: z.number().int().positive().optional(),
  exports: z.record(z.string(), z.array(z.string().min(1))).optional(),
});

// ============================================================================
// Discriminated Union Schema
// ============================================================================

/**
 * The main sidecar request schema - a discriminated union on the 'action' field
 */
export const SidecarRequestSchema = z.discriminatedUnion('action', [
  InitRequestSchema,
  BundleRequestSchema,
  CaptureV2RequestSchema,
  CheckV2RequestSchema,
  InferRequestSchema,
  ResolveDefinitionsRequestSchema,
  RetypeCheckRequestSchema,
  VerifyClientSemanticsRequestSchema,
  VerifyLibraryClaimsRequestSchema,
  ListLibrarySurfaceRequestSchema,
  HealthRequestSchema,
  ShutdownRequestSchema,
  ListProgramFilesRequestSchema,
  AddProgramFilesRequestSchema,
  PositionRowsRequestSchema,
  ImplementationsRequestSchema,
]);

// ============================================================================
// Parse Function
// ============================================================================

/**
 * Result type for parseRequest
 */
export type ParseResult =
  | { success: true; request: SidecarRequest }
  | { success: false; error: string };

/**
 * Validates and parses incoming JSON into a typed SidecarRequest
 *
 * @param json - The raw JSON value to parse
 * @returns ParseResult with either the typed request or an error message
 */
export function parseRequest(json: unknown): ParseResult {
  const result = SidecarRequestSchema.safeParse(json);

  if (result.success) {
    return {
      success: true,
      request: result.data as SidecarRequest,
    };
  }

  // Format Zod errors nicely
  const errorMessages = result.error.issues.map((issue) => {
    const path = issue.path.join('.');
    return path ? `${path}: ${issue.message}` : issue.message;
  });

  return {
    success: false,
    error: `Invalid request: ${errorMessages.join('; ')}`,
  };
}

/**
 * Validates and parses incoming JSON, throwing on error
 *
 * @param json - The raw JSON value to parse
 * @returns The typed SidecarRequest
 * @throws Error if validation fails
 */
export function parseRequestOrThrow(json: unknown): SidecarRequest {
  const result = parseRequest(json);
  if (!result.success) {
    throw new Error(result.error);
  }
  return result.request;
}

// ============================================================================
// Type exports (inferred from schemas)
// ============================================================================

export type ValidatedInitRequest = z.infer<typeof InitRequestSchema>;
export type ValidatedBundleRequest = z.infer<typeof BundleRequestSchema>;
export type ValidatedInferRequest = z.infer<typeof InferRequestSchema>;
export type ValidatedHealthRequest = z.infer<typeof HealthRequestSchema>;
export type ValidatedShutdownRequest = z.infer<typeof ShutdownRequestSchema>;
export type ValidatedSidecarRequest = z.infer<typeof SidecarRequestSchema>;
