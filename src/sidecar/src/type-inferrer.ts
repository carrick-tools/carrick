/**
 * Type Inferrer - Scope-based type inference with payload unwrapping
 *
 * This module extracts types even when developers don't write explicit
 * annotations. It uses span-based node lookup (no line windows) to target
 * precise expressions provided by the Rust/LLM pipeline.
 *
 * Key feature: Agent-informed payload unwrapping
 * - Extracts payload types from machinery wrappers (Response<T>, AxiosResponse<T>, etc.)
 * - Supports union/intersection composition
 * - Recursive unwrapping with depth limits
 *
 * Framework agnosticism: The LLM emits the payload subexpression directly (e.g.,
 * the `users` in `res.json(users)`, `ctx.body = users`, `h.response(users)`, or
 * `return users`). The sidecar resolves that node and reads its type — no
 * framework-specific method-name lists live here. For payload-less handlers
 * (redirects, 204s), the LLM emits null and we fall back to the containing
 * function's return type.
 */

import * as path from 'node:path';
import {
  Project,
  SourceFile,
  Node,
  SyntaxKind,
  VariableDeclarationKind,
  type FunctionDeclaration,
  type ArrowFunction,
  type BinaryExpression,
  type FunctionExpression,
  type MethodDeclaration,
  type CallExpression,
  type ObjectLiteralExpression,
  type ParameterDeclaration,
  type ConditionalExpression,
  type PropertyAssignment,
  type StringLiteral,
  type Type,
  type TypeReferenceNode,
  type VariableDeclaration,
  type Symbol as TsSymbol,
  ts,
} from 'ts-morph';
import { typeToStringAsDeclared } from './print-type.js';
import type {
  InferRequestItem,
  InferResult,
  InferSlotTiming,
  InferredType,
  InferKind,
  ModelSymbol,
  SourceLocation,
  StatedBody,
  ExtractionConfig,
  ExtractionRule,
  TypeProvenance,
} from './types.js';
import { validateInferRequestItem } from './validators.js';
import { declarationSite } from './bundler.js';
import { printedTypeHoldsTopType, relateSymbolToRead } from './symbol-relation.js';
import { notePrintedType, PrintedTypes } from './printed-names.js';
import { externalImportsOf, isExternalOrigin } from './origin.js';
import { reachedOnlyOnFailure } from './failure-path.js';
import { readResponseModes, type JoinedBody, type ResponseModes } from './response-modes.js';
import { functionAtLine } from './function-line-index.js';
import { attributedDeclaration, installedPackageOf } from './declaration-attribution.js';
import { endLineOf, startLineOf } from './line-index.js';
import {
  callsOf,
  descendantsOf,
  descendantsOnLine,
  descendantsOnLines,
  wrap,
  type IndexedNode,
} from './node-index.js';
import { elapsedMs, inferTiming, phaseClock, timedPhase } from './infer-timing.js';
import type { LabelledMembers } from './library-claims.js';
import { StatedParams } from './stated-params.js';
import { bindingToleratesAbsence, handlerToleratesAbsentBody } from './absent-body.js';
import {
  classifyStatusCodes,
  constituents,
  httpStatus,
  statesNoBody,
  statesNoShape,
  statusKeyCode,
  statusTableOf,
  successEntries,
} from './capture/index.js';
import {
  addedDiagnostics,
  applyInsertions,
  fileDiagnostics,
  literalInsertions,
  mapBack,
  mapForward,
  normalise,
  pathOf,
  type Insertion,
  type PathFunction,
} from './unwidened.js';
import {
  expandTypeStructural,
  jsonWireType,
  type ExpandOrigin,
  type MemberOverrides,
  type WireFormat,
} from './type-structural-expander.js';

/**
 * TS/lib globals and primitives that must never be emitted as a deterministic
 * type anchor (`primary_type_symbol`). A payload whose resolved symbol is one of
 * these is library machinery, not a user contract — mirror of the Rust-side
 * `is_builtin_type` filter in `socket_io.rs` so the HTTP-inference anchor and the
 * socket anchor reject the same set.
 */
/**
 * Async-iterator / generator transport wrappers. A resolver written as an async
 * generator (`async function* x(): AsyncGenerator<Order>`) carries its contract
 * in the yield position; these symbol names gate `unwrapAsyncIterableType`, which
 * peels them down to the yield type before structural expansion. They are also
 * folded into `BUILTIN_ANCHOR_SYMBOLS` so the wrapper itself can never become a
 * `primary_type_symbol` anchor.
 */
const ASYNC_ITERABLE_SYMBOLS = new Set<string>([
  'AsyncGenerator',
  'AsyncIterableIterator',
  'AsyncIterator',
  'Generator',
  'IterableIterator',
]);

const BUILTIN_ANCHOR_SYMBOLS = new Set<string>([
  ...ASYNC_ITERABLE_SYMBOLS,
  'any',
  'unknown',
  'never',
  'void',
  'object',
  'string',
  'number',
  'boolean',
  'bigint',
  'symbol',
  'undefined',
  'null',
  'Array',
  'Promise',
  'Record',
  'Map',
  'Set',
  'Date',
  'Object',
  'String',
  'Number',
  'Boolean',
  'Symbol',
  'BigInt',
  'Function',
  'RegExp',
  'Error',
]);

/**
 * Strongly-discriminating member names of HTTP transport machinery — the
 * fetch/DOM `Response` & `Request`, a Node `http.ServerResponse`, a framework
 * reply object. A type declared in a lib or installed-dependency origin that carries
 * a subset of these is framework machinery, never a user contract: the
 * PRODUCER-side structural mirror of the consumer `machineryIndicators`
 * (ExtractionConfig), used to reject a wrapper envelope whose response field is
 * a raw `Response` (carrick#371) instead of emitting it as a comparable type.
 *
 * Framework-agnostic by construction: no framework NAME appears here, only the
 * shared HTTP-message surface. The names are deliberately the ones no
 * JSON payload ever carries (`ok`, `redirected`, `bodyUsed`, `arrayBuffer`,
 * `writeHead`, ...), so the origin gate + `MACHINERY_INDICATOR_THRESHOLD` never
 * fire on real data. Kept in lockstep with the capture-seam mirror
 * `capture/machinery.ts` (the seam forbids sharing a module across it, same as
 * `BUILTIN_ANCHOR_SYMBOLS` mirrors `socket_io.rs`). Exported so a drift-guard
 * test (`machinery-indicator-mirror.test.ts`) asserts the two sets stay equal.
 */
export const MACHINERY_MEMBER_INDICATORS = new Set<string>([
  // fetch / DOM Response & Request body-consumer surface
  'ok',
  'redirected',
  'bodyUsed',
  'arrayBuffer',
  'blob',
  'formData',
  'clone',
  'json',
  'statusText',
  // FROZEN (old path, 2026-10-21): the reply-object half below only (the
  // fetch half above and the threshold stay). Deletions and safety fixes
  // only. Fixes for wrong or missing types go in labelled reply objects
  // (carrick#2472 C4) and the language library rows (carrick#2445). Delete
  // when a delete-and-replay on the four repos moves no row wrongly, after
  // carrick-cloud#2379 and carrick#2445 (carrick#2472, C6).
  // Node http ServerResponse / reply-object surface
  'statusCode',
  'statusMessage',
  'setHeader',
  'getHeader',
  'removeHeader',
  'writeHead',
  'flushHeaders',
]);

/** Machinery needs at least this many indicator members to be recognized. */
const MACHINERY_INDICATOR_THRESHOLD = 3;

/**
 * True when a binary expression COMPARES rather than assigns or computes: its
 * value is a `boolean` the operator manufactured, never a payload
 * (carrick#1017).
 */
function isComparisonExpression(expr: BinaryExpression): boolean {
  switch (expr.getOperatorToken().getKind()) {
    case SyntaxKind.EqualsEqualsToken:
    case SyntaxKind.EqualsEqualsEqualsToken:
    case SyntaxKind.ExclamationEqualsToken:
    case SyntaxKind.ExclamationEqualsEqualsToken:
    case SyntaxKind.LessThanToken:
    case SyntaxKind.LessThanEqualsToken:
    case SyntaxKind.GreaterThanToken:
    case SyntaxKind.GreaterThanEqualsToken:
    case SyntaxKind.InstanceOfKeyword:
    case SyntaxKind.InKeyword:
      return true;
    default:
      return false;
  }
}

/**
 * How deep the response-branch split (carrick#1017) follows nested
 * conditionals. Two levels covers `a ? x : b ? y : z`; deeper nesting is a
 * dispatch table, not a response contract.
 */
const RESPONSE_BRANCH_MAX_DEPTH = 2;

/**
 * The members the standard response-init object declares. An argument beside
 * the body whose properties are all from this set is telling the platform how
 * to send the response, not what to send (carrick#1017).
 */
const RESPONSE_INIT_MEMBER_NAMES = new Set<string>([
  'status',
  'statusText',
  'headers',
]);

/**
 * How far the response-helper recovery (carrick#631) descends through nested
 * calls looking for the payload argument. `cors(request, json(payload))` needs
 * two; the bound is a backstop against a pathologically nested return.
 */
const RESPONSE_HELPER_MAX_DEPTH = 4;

/**
 * How many wrapper calls a handler bound to one is followed through
 * (carrick#2472 P3-2b): `wrap(trace(async (req, res) => …))` needs two. A
 * backstop against a pathologically nested initializer.
 */
const WRAPPER_CALL_MAX_DEPTH = 3;

/**
 * Members an options object uses to state an HTTP status. A >= 400 status marks
 * the branch an error path, whose shape is not the endpoint's contract.
 */
const STATUS_MEMBER_NAMES = ['status', 'statusCode'] as const;

/**
 * What a response send's status says about its body (carrick#1161).
 * `success` (below 300) and `undecided` (no status stated anywhere) send the
 * route's contract; `error` (400 and up) and `redirect` (3xx) do not; and
 * `variable` is a status the source computes, which fails open into the
 * union so no success body is ever guessed away.
 */
type ResponseSiteStatus = 'success' | 'error' | 'redirect' | 'variable' | 'undecided';

/**
 * The reasons an `unknown` answer is a decision, not a failure to read: the
 * list `DECIDED_ABSTAIN_REASONS` in `src/engine/type_compat_v2.rs` holds
 * final. A stated reply never replaces one (carrick#2674).
 */
const DECIDED_ABSTAIN_REASONS: ReadonlySet<string> = new Set([
  'no_success_payload',
  'no_request_body',
  'projected_value_only',
  'serialised_body_unread',
  'body_built_by_callee',
  'reshaped_by_callback',
]);

/** One response branch's payload, before branches are joined. */
interface PayloadCandidate {
  typeString: string;
  isExplicit: boolean;
  node: Node;
  anchorType: Type;
  statedTypeNode?: Node;
}

/** A function that can build a response: one with a body to read. */
type BuilderFunction = FunctionDeclaration | ArrowFunction | FunctionExpression;

/**
 * How a repository's own response builder fills an envelope (carrick#2200):
 * the parameter it serialises into `body`, and the parameter its status
 * member takes or the status its literal states.
 */
interface EnvelopeBuilder {
  builder: BuilderFunction;
  payloadParameter: number;
  statusParameter?: number;
  fixedStatus?: ResponseSiteStatus;
}

/**
 * Why one returned branch of a serialised-body envelope handler was not read
 * (carrick#2200). Each has its own sentence in `SERIALISED_BODY_UNREAD_DETAIL`.
 */
type EnvelopeUnreadCause = 'prebuilt' | 'not_serialised' | 'builder' | 'top_type';

/** What one returned branch of an envelope handler sends. */
type EnvelopeBranchRead = PayloadCandidate | 'dropped' | { unread: EnvelopeUnreadCause };

/** The provenance sentence for each reason a branch was not read. */
const SERIALISED_BODY_UNREAD_DETAIL: Record<EnvelopeUnreadCause, string> = {
  prebuilt:
    'A return hands back a status and body built somewhere else, such as a variable or a field of another result, so the value in its body was not read.',
  not_serialised:
    "A return's body string is not made by JSON.stringify, so the value it holds was not read.",
  builder:
    'A return calls a response helper that could not be followed to the one value it serialises, so what it sends was not read.',
  top_type:
    'A return serialises a value typed any or unknown, so the shape of its body is not known.',
};

/** `any` or `unknown` as a type in printed type text, not as a member name or a string. */
const TOP_TYPE_TOKEN = /(?<![\w$'"`])(?:any|unknown)(?![\w$'"`?:])/;

/** A response payload read out of one or more response sends. */
interface RecoveredPayload {
  typeString: string;
  isExplicit: boolean;
  /** The first surviving payload node, where the inferred type is located. */
  node: Node;
  /** Every payload node that fed `typeString`. */
  nodes: Node[];
  anchorType?: Type;
  /** The type of each distinct payload that fed `typeString` (carrick#2111). */
  readTypes: Type[];
  statedTypeNode?: Node;
  /** carrick#2054: see `InferredType.response_modes`. */
  responseModes?: ResponseModes;
}

/**
 * The type an assertion states, or undefined when it states none. A const
 * assertion (`x as const`, `<const>x`) narrows its operand's own type and
 * names no type: its type node prints as `const`, a name nothing declares
 * (carrick#2590). The walk stops there rather than reaching for an assertion
 * further out, which describes a larger value than the read.
 */
function statedAssertionType(typeNode: Node | undefined): Node | undefined {
  if (!typeNode || ts.isConstTypeReference(typeNode.compilerNode)) return undefined;
  return typeNode;
}

/**
 * The items whose whitespace-collapsed texts differ, first of each kept, in
 * order: how a joined union drops a member it already has.
 */
function distinctByText<T>(items: T[], text: (item: T) => string): T[] {
  const seen = new Set<string>();
  return items.filter((item) => {
    const key = text(item).replace(/\s+/g, ' ').trim();
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

/**
 * The status codes a TYPE fixes: a numeric literal, or a union made only of
 * them, every one in the HTTP range. `undefined` for anything else, including
 * a plain `number`.
 */
function statusCodesOfType(type: Type): number[] | undefined {
  const members = type.isUnion() ? type.getUnionTypes() : [type];
  const codes: number[] = [];
  for (const member of members) {
    if (!member.isNumberLiteral()) return undefined;
    const code = httpStatus(member.getLiteralValue());
    if (code === undefined) return undefined;
    codes.push(code);
  }
  return codes.length > 0 ? codes : undefined;
}

/**
 * The parts of a request that ARE the body, as validator middleware names them
 * (`validate('json', Schema)`). HTTP vocabulary for how a body is carried — the
 * same class of generic route vocabulary as the `schema` / `body` / `response` /
 * `handler` keys the schema anchors already read, and deliberately not a list of
 * libraries. Every other part a validator can bind (`query`, `param`, `header`,
 * `cookie`) is NOT the body, so a schema bound to one of those declares no
 * request contract.
 */
const REQUEST_BODY_PARTS = new Set<string>(['json', 'form', 'body']);

/**
 * Which of a validation schema's two types a contract reads (carrick#1101).
 *
 * A schema's INPUT is what a caller may send; its OUTPUT is what parsing hands
 * the handler. They differ wherever parsing changes the value: a defaulted key
 * is optional to send and present after parsing, a transform changes a
 * member's type, a coercion accepts any value. A request contract is what a
 * caller sends, so it reads the input; a response contract reads the output.
 */
type SchemaDirection = 'input' | 'output';

/**
 * A contract a route declares, as printed text, plus the provenance the reading
 * recorded (a coerced request member, carrick#1101). Provenance is absent, not
 * empty, when there is nothing to say.
 */
/**
 * carrick-cloud#1366: the parameter and config-member names a client takes a
 * request body under. A generic slot is read as the body only under one of
 * these names; a type parameter alone also types response modes and fallbacks.
 */
const BODY_MEMBER_NAMES = new Set(['data', 'body', 'json']);

/**
 * The zero-argument reads that take the WHOLE body out of a transport response
 * (the platform `Response` and the clients that copy its shape): parsed as
 * JSON, or as raw text (carrick#1842).
 */
const WHOLE_BODY_READS = new Set(['json', 'text']);

/**
 * The other zero-argument reads the platform `Response` takes its whole body
 * out with, in a form that is neither JSON nor text. The expression that
 * holds one holds the body the call answered with (carrick#2112).
 */
const BINARY_BODY_READS = new Set(['arrayBuffer', 'blob', 'bytes', 'formData']);

/** The body format a caller names to read a response as raw text (carrick#1842). */
const RAW_TEXT_FORMAT = 'text';

interface DeclaredContract {
  text: string;
  provenance?: TypeProvenance[];
  /**
   * carrick#2674: the route states it takes no request body, and why. The
   * contract is then the decided `no_request_body` abstain, not `text`.
   */
  noRequestBody?: string;
}

/** A type the function a value is made in states for it (carrick#2594). */
interface StatedInFunction extends DeclaredContract {
  /** Where the statement is: the annotation's owner, the cast or the call. */
  at: Node;
  /** Which statement it is, for the log. */
  how: string;
  /** The type node the source wrote, where the statement is one (carrick#2714). */
  typeNode?: Node;
}

/**
 * Print a `Type` to its string form WITHOUT the compiler's default truncation.
 *
 * `Type.getText()` truncates large/anonymous object types to ~160 chars and inserts
 * `...`, which yields a syntactically-invalid or structurally-wrong surface that then
 * produces false type-drift verdicts downstream. `NoTruncation` disables that; this is
 * the same flag set `definition-resolver.ts` already uses for expanded definitions.
 */
const TYPE_TEXT_FLAGS =
  ts.TypeFormatFlags.NoTruncation | ts.TypeFormatFlags.InTypeAlias;

function typeText(type: Type, enclosingNode?: Node): string {
  // Timed as the print (carrick#1985): the type is already computed when it
  // gets here, so this is what its length costs. A module the print names is
  // written as its declaring file, whichever file it is printed from
  // (carrick#2019).
  const text = timedPhase('print', () =>
    enclosingNode
      ? typeToStringAsDeclared(
          enclosingNode.getProject().getTypeChecker().compilerObject,
          type.compilerType,
          enclosingNode.compilerNode,
          TYPE_TEXT_FLAGS
        )
      : type.getText(undefined, TYPE_TEXT_FLAGS)
  );
  notePrintedType(type, enclosingNode, text);
  return text;
}

/**
 * `text` is `string`, alone or beside `null` and `undefined` in either order:
 * the only payload a raw-text read publishes (carrick#1842).
 */
/**
 * Source text with every plain string literal written in double quotes:
 * `'/a'`, `"/a"` and an interpolation-free `` `/a` `` are one value, so text
 * that differs only in its quotes compares equal (carrick#2056).
 *
 * The text is read left to right, pairing quotes the way the scanner does,
 * so a template's closing backtick never pairs with the next template's
 * opening one. A literal holding a quote, a backslash or an interpolation is
 * copied as it is, and so is an unterminated one with everything after it.
 */
export function canonicalStringQuotes(text: string): string {
  const isQuote = (ch: string): boolean => ch === "'" || ch === '"' || ch === '`';
  // The index just past the literal opening at `start`, or -1.
  const literalEnd = (start: number): number => {
    const quote = text[start];
    let j = start + 1;
    while (j < text.length) {
      const ch = text[j];
      if (ch === '\\') {
        j += 2;
      } else if (ch === quote) {
        return j + 1;
      } else if (quote === '`' && ch === '$' && text[j + 1] === '{') {
        const close = codeEnd(j + 2);
        if (close < 0) return -1;
        j = close + 1;
      } else {
        j++;
      }
    }
    return -1;
  };
  // The index of the `}` that closes an interpolation whose code starts at `start`, or -1.
  const codeEnd = (start: number): number => {
    let depth = 0;
    let j = start;
    while (j < text.length) {
      const ch = text[j];
      if (isQuote(ch)) {
        const end = literalEnd(j);
        if (end < 0) return -1;
        j = end;
        continue;
      }
      if (ch === '{') depth++;
      if (ch === '}') {
        if (depth === 0) return j;
        depth--;
      }
      j++;
    }
    return -1;
  };

  let out = '';
  let i = 0;
  while (i < text.length) {
    if (!isQuote(text[i])) {
      out += text[i];
      i++;
      continue;
    }
    const end = literalEnd(i);
    if (end < 0) return out + text.slice(i);
    const body = text.slice(i + 1, end - 1);
    out += /['"`\\]|\$\{/.test(body) ? text.slice(i, end) : `"${body}"`;
    i = end;
  }
  return out;
}

function isBareStringText(text: string): boolean {
  const members = text.split('|').map((member) => member.trim());
  return (
    members.includes('string') &&
    members.every((member) => member === 'string' || member === 'null' || member === 'undefined')
  );
}

/**
 * Union of all function-like nodes in ts-morph
 */
type FunctionLike =
  | FunctionDeclaration
  | ArrowFunction
  | FunctionExpression
  | MethodDeclaration;

/**
 * What the def-use walk found downstream of a call whose result was bound:
 * the node whose type is the call's payload, and whether the source reached
 * the payload only by reading MEMBERS out of it (carrick#1375).
 */
interface CallResultUse {
  terminal: Node;
  /**
   * Every read of the value took a member out of it and none read it whole,
   * so this site states a part of a payload rather than a payload. The type
   * a projection prints is not a wire contract and must not be judged as one.
   */
  projectionOnly: boolean;
  /** The member reads themselves, so a caller can ask what they resolve to. */
  projections: Node[];
  /**
   * The call starts a chain, a callback in it is handed the body unread, and
   * nothing in the chain states the body (carrick#3004). The terminal is the
   * call itself: what the chain ends in is the value a callback built from
   * the body, never the body, and where the call's own value is untyped the
   * site states no response contract.
   */
  handedUnreadBody?: boolean;
}

/**
 * Options for TypeInferrer construction
 */
export interface TypeInferrerOptions {
  /** The ts-morph Project instance */
  project: Project;
  /**
   * The registry package a resolved file belongs to, when the module graph
   * that built the project names one (carrick#1260). Supplied for a project
   * whose resolution went through a graph rather than through `node_modules`;
   * absent otherwise, and the path itself is then the only thing to read.
   */
  packageOf?: (filePath: string) => string | undefined;
  /**
   * The service root the project was built for. The machinery origin gate
   * reads it to tell a workspace package the compiler reached through a
   * `node_modules` symlink from an installed dependency (carrick#1264).
   */
  repoRoot: string;
  /**
   * How much work the unwidened reading of one `infer` batch may do
   * (carrick#2629). Each field defaults to its `UNWIDENED_*` constant.
   */
  unwidenedLimits?: Partial<UnwidenedLimits>;
  /**
   * The clock the unwidened reading's last-resort time limit reads, in
   * milliseconds. Defaults to `performance.now`; a test passes a slow one.
   */
  now?: () => number;
}

/**
 * The bounds on one batch's unwidened reading (carrick#2629). The first two
 * count work, so a batch reads the same on a quiet machine and a loaded one;
 * the third is a wall-clock backstop for a reading that runs away.
 */
export interface UnwidenedLimits {
  /** Readings (requests re-read in the rewrite) one batch may make. */
  readings: number;
  /** Files one batch may type-check before and after its rewrite. */
  files: number;
  /** The longest the reading may run, from its start, before it stops. */
  backstopMs: number;
}

/**
 * How many readings one batch may make (carrick#2629). A count, not a time,
 * so whether a reading is made never depends on the machine's load: a reading
 * a batch cannot make marks its response `unwidened_out_of_budget`, and the
 * check then abstains on every mismatch against it (carrick#2100). Sized well
 * above the largest batch measured; see the PR that set it.
 */
const UNWIDENED_MAX_READINGS = 5_000;
/** How many files one batch's reading may type-check (carrick#2629). */
const UNWIDENED_MAX_FILES = 5_000;
/**
 * The last resort: a reading still running this long after it started stops
 * and says so in the log. The reading reports progress after every file it
 * checks and every request it re-reads, so the scanner's limit on silence
 * never needs it; it is here for a reading that runs away.
 */
const UNWIDENED_BACKSTOP_MS = 30 * 60_000;

/**
 * Called each time an `infer` batch is done with one of its requests,
 * whatever that request answered: skipped, refused, failed or inferred.
 */
export type InferRequestDone = () => void;

/**
 * Called each time the unwidened reading finishes a unit of its work: a file
 * type-checked or a request re-read (carrick#2629). The reading runs after
 * every request is done, so this is what keeps a long one from going silent.
 */
export type UnwidenedStepDone = () => void;

/**
 * Result of unwrapping a type
 */
interface UnwrapResult {
  /** The unwrapped type string */
  typeString: string;
  /** Whether the original type had explicit annotation */
  isExplicit: boolean;
  /** Whether unwrapping was actually performed */
  wasUnwrapped: boolean;
  /**
   * The single recovered payload `Type`, when a rule extracted one (#336).
   * The anchor symbol + array depth must be derived from the PAYLOAD, not the
   * wrapper: `axios.get<Order[]>` resolves to `AxiosResponse<Order[]>`, whose
   * own symbol is the wrapper and whose array-ness lives one level down.
   * Absent when nothing was unwrapped, when a union joined several payloads,
   * or when verified machinery collapsed to `unknown`.
   */
  payloadType?: Type;
  /**
   * The unwrap ended in a VERIFIED-MACHINERY collapse: a rule matched the
   * wrapper's symbol and origin, so the type is known transport, and no rule
   * could recover a payload from inside it. `typeString` is then the honest
   * `unknown`, not a contract — callers that have a second way to reach the
   * payload (the carrick#631 return-statement recovery) must treat this the
   * same as the structural machinery verdict rather than as a resolved type.
   */
  verifiedMachinery?: boolean;
  /**
   * A union resolved SOME of its branches and not others (carrick#166). The
   * names here are the branches that read as `unknown`; `typeString` is
   * `unknown` rather than a join of the branches that did read, because a
   * contract known to be partial is a false match and worse than the top type
   * it replaced. Distinct from `verifiedMachinery`, which means NOTHING
   * resolved and the return-statement recovery should take over.
   *
   * Type NAMES only, never printed text: a printed type whose module the use
   * site does not import carries an absolute path, and this reaches a reader
   * as a provenance `detail`.
   */
  unreadUnionBranches?: string[];
}

/**
 * Outcome of trying one ExtractionRule against one type. A rule that verifies
 * wrapper identity but recovers no payload must neither win (stomping the
 * type) nor be indistinguishable from a non-match (the wrapper is verified
 * machinery, never the contract) — so the three cases are explicit.
 */
type RuleAttempt =
  | { kind: 'extracted'; result: UnwrapResult }
  | { kind: 'verified-no-payload' }
  | { kind: 'no-match' };

/**
 * One of the names a type goes by, with the arguments the compiler states for
 * that name (carrick#1843): the symbol of the declaration its structure comes
 * from with the type's own arguments, or the alias it was written through
 * with the alias's arguments.
 */
interface WrapperReading {
  symbol: TsSymbol;
  typeArguments: Type[];
  /** The name is the alias the type was written through, not its own symbol. */
  viaAlias: boolean;
}

/**
 * TypeInferrer - Extracts types from source code, both explicit and inferred
 *
 * Usage:
 *   const inferrer = new TypeInferrer({ project });
 *   const result = inferrer.infer(requests, extractionConfig);
 */
export class TypeInferrer {
  private readonly project: Project;
  private readonly packageOf: ((filePath: string) => string | undefined) | undefined;
  private readonly repoRoot: string;
  /**
   * The node each inferred type was read from, keyed by the location object
   * `getNodeLocation` built for it (the one an `InferredType` carries). The
   * unwidened reading re-reads from here (carrick#1516).
   */
  private readonly readNodes = new WeakMap<SourceLocation, Node>();
  private readonly unwidenedLimits: UnwidenedLimits;
  private readonly now: () => number;
  /**
   * The files a request has named to this inferrer, as the requests named
   * them (carrick#1985). It lives as long as the project does, so the first
   * request of a file is the first of the process, not of a batch.
   */
  private readonly filesAsked = new Set<string>();

  constructor(options: TypeInferrerOptions) {
    this.project = options.project;
    this.packageOf = options.packageOf;
    this.repoRoot = options.repoRoot;
    this.unwidenedLimits = {
      readings: options.unwidenedLimits?.readings ?? UNWIDENED_MAX_READINGS,
      files: options.unwidenedLimits?.files ?? UNWIDENED_MAX_FILES,
      backstopMs: options.unwidenedLimits?.backstopMs ?? UNWIDENED_BACKSTOP_MS,
    };
    this.now = options.now ?? (() => performance.now());
  }

  /**
   * What the structural printer needs to tell the user's own declarations from
   * the runtime's and its packages' — the same program and service root this
   * class asks `isExternalOrigin` about machinery, so the two layers cannot
   * classify one declaration two ways.
   */
  private expandOrigin(): ExpandOrigin {
    return {
      program: this.project.getProgram().compilerObject,
      repoRoot: this.repoRoot,
      imports: externalImportsOf(this.project),
    };
  }

  /**
   * Infer types for the given requests
   *
   * @param requests - Array of inference requests
   * @param extractionConfig - Agent-generated extraction config for payload unwrapping
   * @param onRequestDone - Called once per request, as the batch is done with it
   * @param onUnwidenedStep - Called once per unit of the unwidened reading's work
   * @returns InferResult with inferred types or errors
   */
  infer(
    requests: InferRequestItem[],
    extractionConfig?: ExtractionConfig,
    onRequestDone?: InferRequestDone,
    modelSymbols?: ModelSymbol[],
    labels?: LabelledMembers,
    onUnwidenedStep?: UnwidenedStepDone
  ): InferResult {
    this.modelSymbols = new Map((modelSymbols ?? []).map((symbol) => [symbol.alias, symbol]));
    this.statedParams = this.statedParamsFor(labels ?? { sends: new Map(), sets: new Map(), bodies: new Set() });
    const inferredTypes: InferredType[] = [];
    const errors: string[] = [];
    /** Response inferences the unwidened reading re-reads (carrick#1516). */
    const responses: Array<{ request: InferRequestItem; result: InferredType }> = [];
    /** Request-body inferences it re-reads after them (carrick#2011). */
    const bodies: Array<{ request: InferRequestItem; result: InferredType }> = [];
    /** How long each request took, in the order they were done (carrick#1985). */
    const timings: InferSlotTiming[] = [];

    for (const request of requests) {
      const slotStarted = performance.now();
      const phasesStarted = phaseClock();
      const firstInFile = !this.filesAsked.has(request.file_path);
      this.filesAsked.add(request.file_path);
      let printedLength = 0;
      try {
        // Plain JavaScript has no type annotations to extract, and `checkJs` is
        // off, so inferring against a `.js` file yields nothing useful — it only
        // crashes deep in the compiler API on undefined symbols (`escapedName`,
        // `flags`) and floods the log with the resulting error strings. Skip it.
        // `allowJs` stays on so `.ts` files can still resolve `.js` imports.
        if (/\.(js|jsx|mjs|cjs)$/i.test(request.file_path)) {
          continue;
        }
        const loc = this.formatRequestLocation(request);
        const itemError = validateInferRequestItem(request);
        if (itemError) {
          errors.push(
            `Invalid infer item at ${request.file_path}:${loc} (${request.infer_kind}): ${itemError}`
          );
          continue;
        }
        // carrick#1836: what the names the published text prints meant, read
        // while the program the prints were made in is still the project's.
        // The unwidened re-read below records nothing: it prints the same
        // node, and its reading has a budget.
        const prints = new PrintedTypes();
        const result = prints.during(() => this.inferSingle(request, extractionConfig));
        if (result) {
          printedLength = result.type_string.length;
          this.recordPrintedNames(result, request, prints);
          inferredTypes.push(result);
          if (request.infer_kind === 'response_body' || request.infer_kind === 'function_return') {
            responses.push({ request, result });
          } else if (request.infer_kind === 'request_body') {
            bodies.push({ request, result });
          }
        } else {
          errors.push(
            `Could not infer type at ${request.file_path}:${loc} (${request.infer_kind})`
          );
        }
      } catch (err) {
        const error = err instanceof Error ? err.message : String(err);
        const loc = this.formatRequestLocation(request);
        errors.push(
          `Error inferring type at ${request.file_path}:${loc}: ${error}`
        );
      } finally {
        // Before the caller is told: what it does with the news (a progress
        // frame) is not this request's time.
        const phases = phaseClock();
        timings.push({
          ...(request.alias === undefined ? {} : { alias: request.alias }),
          file_path: request.file_path,
          line_number: request.line_number,
          infer_kind: request.infer_kind,
          ms: elapsedMs(slotStarted, performance.now()),
          type_ms: elapsedMs(phasesStarted.type, phases.type),
          print_ms: elapsedMs(phasesStarted.print, phases.print),
          printed_length: printedLength,
          first_in_file: firstInFile,
        });
        onRequestDone?.();
      }
    }

    try {
      // One allowance for the batch: the responses draw on it first, the
      // bodies on what they leave.
      const work: UnwidenedWork = {
        readings: this.unwidenedLimits.readings,
        files: this.unwidenedLimits.files,
        backstopAt: this.now() + this.unwidenedLimits.backstopMs,
        step: onUnwidenedStep ?? (() => {}),
      };
      // The responses' rewrite forgets every node of the files it touches, so
      // each body's read node is kept as its span, taken now and found again
      // in the restored file.
      const bodySpans = bodies.map((body) => {
        const node = this.readNodes.get(body.result.source_location);
        return {
          ...body,
          readSpan: node && {
            file: node.getSourceFile(),
            start: node.getStart(),
            end: node.getEnd(),
            kind: node.getKind(),
          },
        };
      });
      try {
        this.addUnwidenedReadings(responses, extractionConfig, work, 'response');
      } catch (err) {
        // The reading only ever adds a field; it never costs an answer.
        this.logError(
          `Unwidened reading failed: ${err instanceof Error ? err.message : String(err)}`
        );
      }
      try {
        // Second, and in a rewrite of its own: the bodies never take a
        // response's share of the work, whose cap a spent allowance turns into
        // an abstention (carrick#2100).
        this.addUnwidenedReadings(bodySpans, extractionConfig, work, 'request_body');
      } catch (err) {
        this.logError(
          `Unwidened reading of request bodies failed: ${err instanceof Error ? err.message : String(err)}`
        );
        // carrick#2011: a body whose reading was not made is judged on its
        // widened type alone, so it is marked the way a spent work cap marks it
        // and its pair abstains on a mismatch rather than report one.
        for (const body of bodies) {
          if (body.result.unwidened_type_string === undefined) {
            body.result.unwidened_out_of_budget = true;
          }
        }
      }
    } catch (err) {
      this.logError(
        `Unwidened reading failed: ${err instanceof Error ? err.message : String(err)}`
      );
    }

    return {
      success: errors.length === 0 || inferredTypes.length > 0,
      inferred_types: inferredTypes.length > 0 ? inferredTypes : undefined,
      timing: inferTiming(timings),
      errors: errors.length > 0 ? errors : undefined,
    };
  }

  /** The types the model named, by the alias of the row each is for (carrick#2111). */
  private modelSymbols = new Map<string, ModelSymbol>();
  /**
   * carrick#2674: what route handlers' parameters state, through the
   * members the batch's verified server-framework labels name. Empty when
   * the batch carries no labels, and then it reads nothing.
   */
  private statedParams = this.statedParamsFor({ sends: new Map(), sets: new Map(), bodies: new Set() });

  /**
   * carrick#2111: where the model named a type for the response this request
   * reads, record on `inferred` how that type relates to the compiler's type
   * of what the handler sends (`read`, one type per surviving branch). The
   * driver keeps, re-depths or drops the model's symbol on the answer.
   *
   * Nothing is recorded, and the symbol stands, where there is nothing to
   * compare: the symbol does not resolve in this program, or the published
   * read holds `any` or `unknown` somewhere.
   */
  private relateModelSymbol(
    request: InferRequestItem,
    inferred: InferredType,
    read: ReadonlyArray<Type | undefined>
  ): void {
    if (request.infer_kind !== 'response_body' && request.infer_kind !== 'function_return') return;
    const named = this.modelSymbols.get(inferred.alias);
    if (!named) return;
    const types = read.filter((type): type is Type => type !== undefined);
    if (types.length !== read.length || types.length === 0) return;
    if (printedTypeHoldsTopType(inferred.type_string)) return;
    try {
      const symbolType = this.modelSymbolType(named);
      if (!symbolType) return;
      const answer = relateSymbolToRead(
        this.project.getTypeChecker().compilerObject,
        symbolType.compilerType,
        types.map((type) => type.compilerType)
      );
      if (answer) inferred.model_symbol_relation = answer;
    } catch (err) {
      // The relation only ever adds an answer; it never costs one.
      this.logError(
        `Model symbol relation failed for ${inferred.alias}: ${err instanceof Error ? err.message : String(err)}`
      );
    }
  }

  /**
   * `func` is the function the row's line names: it opens on that line (a
   * method under its decorators, an exported handler), or it is the handler
   * of the registration that opens there (`app.post<…>(\n '/path',\n async
   * (request) => { … })`, or a named handler declared elsewhere).
   */
  private isTheRowsOwnFunction(request: InferRequestItem, func: FunctionLike): boolean {
    if (startLineOf(func) === request.line_number) return true;
    return this.registrationAtLine(func.getSourceFile(), request.line_number)?.handler === func;
  }

  /**
   * The declared type of a symbol the model named, found as the bundle finds
   * it (through a barrel's re-exports), in a file this program already
   * holds. A name that declares a value and no type resolves to nothing.
   */
  private modelSymbolType(named: ModelSymbol): Type | undefined {
    const absolutePath = path.isAbsolute(named.source_file)
      ? named.source_file
      : path.resolve(this.repoRoot, named.source_file);
    const sourceFile = this.project.getSourceFile(absolutePath);
    if (!sourceFile) return undefined;
    const site = declarationSite(sourceFile, named.symbol_name, named.source_file, this.expandOrigin());
    if ('reason' in site) return undefined;
    const declaration =
      site.sourceFile.getInterface(site.name) ??
      site.sourceFile.getTypeAlias(site.name) ??
      site.sourceFile.getClass(site.name) ??
      site.sourceFile.getEnum(site.name);
    return declaration?.getType();
  }

  /**
   * carrick#1836: list on `result` the declarations behind the names its text
   * prints that the request's file cannot resolve (`PrintedTypes.namesIn`).
   */
  private recordPrintedNames(
    result: InferredType,
    request: InferRequestItem,
    prints: PrintedTypes
  ): void {
    const sourceFile = this.getSourceFile(request.file_path);
    if (!sourceFile) return;
    const printedNames = prints.namesIn(
      [result.type_string],
      sourceFile.compilerNode,
      this.project.getTypeChecker().compilerObject
    );
    if (printedNames.length > 0) result.printed_names = printedNames;
  }

  /**
   * carrick#1516: read each response inference again with the literals on its
   * handler's path marked `as const`, and record the narrower type the handler
   * really returns beside the published one (`unwidened_type_string`). See
   * `unwidened.ts` for what is marked and why the reading is sound.
   *
   * carrick#2011: a request body (`read` is `'request_body'`) is read the same
   * way, the call that sends it in place of the handler, except that a literal
   * handed to any call other than the send is never marked.
   *
   * One rewrite for the whole batch: every file on any request's path is
   * rewritten once, type-checked before and after, the requests re-inferred,
   * and every file restored before returning, so the project other requests
   * read is the project the scan loaded.
   *
   * The reading is bounded by work, not time (carrick#2629): a batch makes at
   * most `UnwidenedLimits.readings` readings over at most `.files` files,
   * taken in request order, the responses first. The first request past
   * either cap and every request after it are marked
   * `unwidened_out_of_budget` (carrick#2100) and publish no reading, on any
   * machine. A reading still running at the wall-clock backstop stops the
   * same way and says so in the log. A request that never needed a reading
   * (a declared return, nothing to mark) or lost it for a reason of its own
   * (a marking that added a diagnostic) is never marked.
   */
  private addUnwidenedReadings(
    responses: Array<{ request: InferRequestItem; result: InferredType; readSpan?: SavedSpan }>,
    extractionConfig: ExtractionConfig | undefined,
    work: UnwidenedWork,
    read: 'response' | 'request_body'
  ): void {
    const started = this.now();
    const outOfTime = (stage: string, unread: ReadonlyArray<{ result: InferredType }>): boolean => {
      if (this.now() <= work.backstopAt) return false;
      this.log(
        `Unwidened reading stopped ${stage}: it ran past its last-resort time limit ` +
          `(${this.unwidenedLimits.backstopMs} ms), ${unread.length} reading(s) not made`
      );
      for (const plan of unread) plan.result.unwidened_out_of_budget = true;
      return true;
    };
    interface Plan {
      request: InferRequestItem;
      result: InferredType;
      read: { file: SourceFile; start: number; end: number };
      path: PathFunction[];
      /** The markings on this request's path, by file. */
      insertions: Array<[SourceFile, Insertion[]]>;
      /** Original span of the node an expression-text locator names. */
      locatedByText?: { start: number; end: number };
    }
    const planned: Plan[] = [];

    // Every position is read before the first rewrite: a rewrite forgets
    // every node of its file.
    for (const { request, result, readSpan } of responses) {
      // A declared return type is the contract; nothing was widened.
      if (result.is_explicit) continue;
      const readNode = readSpan
        ? nodeAtSpan(readSpan)
        : this.readNodes.get(result.source_location);
      if (!readNode) continue;
      const functions = pathOf(readNode);
      const planInsertions: Plan['insertions'] = [];
      for (const fn of functions) {
        const insertions = literalInsertions(fn, read === 'request_body' ? readNode : undefined);
        if (insertions.length === 0) continue;
        planInsertions.push([fn.getSourceFile(), insertions]);
      }
      // Nothing on the path to mark: the reading would equal the published type.
      if (planInsertions.length === 0) continue;
      const path = functions.map((fn) => ({
        file: fn.getSourceFile(),
        start: fn.getStart(),
        end: fn.getEnd(),
      }));
      let locatedByText: Plan['locatedByText'];
      if (request.expression_text && request.span_start === undefined) {
        const sourceFile = this.getSourceFile(request.file_path);
        const located = sourceFile
          ? this.findNodeByText(sourceFile, request.expression_text, request.expression_line)
          : undefined;
        if (located) locatedByText = { start: located.getStart(), end: located.getEnd() };
      }
      planned.push({
        request,
        result,
        read: { file: readNode.getSourceFile(), start: readNode.getStart(), end: readNode.getEnd() },
        path,
        insertions: planInsertions,
        locatedByText,
      });
    }
    if (planned.length === 0) return;

    // The work cap: requests are taken in order while both allowances last.
    const checkedFiles = new Set<SourceFile>();
    let taken = 0;
    for (const plan of planned) {
      const added = new Set(plan.path.map((fn) => fn.file).filter((f) => !checkedFiles.has(f)));
      if (work.readings < 1 || added.size > work.files) break;
      work.readings -= 1;
      work.files -= added.size;
      for (const file of added) checkedFiles.add(file);
      taken += 1;
    }
    const plans = planned.slice(0, taken);
    const capped = planned.slice(taken);
    if (capped.length > 0) {
      this.log(
        `Unwidened reading stopped at its work cap (${this.unwidenedLimits.readings} readings, ` +
          `${this.unwidenedLimits.files} files per batch): ${capped.length} reading(s) not made`
      );
      for (const plan of capped) plan.result.unwidened_out_of_budget = true;
    }
    /** Readings that came back narrower than the published type, and so were published. */
    let narrower = 0;
    const report = () =>
      this.log(
        `Unwidened reading of ${read === 'response' ? 'responses' : 'request bodies'}: ` +
          `${planned.length} planned, ${plans.length} taken over ${checkedFiles.size} file(s), ` +
          `${narrower} narrower than published, ${Math.round(this.now() - started)} ms`
      );
    if (plans.length === 0) {
      report();
      return;
    }

    const insertionsByFile = new Map<SourceFile, Insertion[]>();
    for (const plan of plans) {
      for (const [file, list] of plan.insertions) {
        insertionsByFile.set(file, [...(insertionsByFile.get(file) ?? []), ...list]);
      }
    }
    const insertions = new Map<SourceFile, Insertion[]>();
    for (const [file, list] of insertionsByFile) insertions.set(file, normalise(list));
    const before = new Map<SourceFile, ReturnType<typeof fileDiagnostics>>();
    for (const file of checkedFiles) {
      if (outOfTime('before the rewrite', plans)) return report();
      before.set(file, fileDiagnostics(file));
      work.step();
    }
    const originals = new Map<SourceFile, string>();
    for (const file of insertions.keys()) originals.set(file, file.getFullText());

    try {
      for (const [file, list] of insertions) {
        file.replaceWithText(applyInsertions(originals.get(file)!, list));
      }

      // A diagnostic the marking added sits in a function on some request's
      // path (or elsewhere in that function's file): every request whose path
      // holds it loses its reading.
      const tainted = new Set<Plan>();
      for (const file of checkedFiles) {
        if (outOfTime('while checking the rewrite', plans)) return report();
        const list = insertions.get(file) ?? [];
        const after = fileDiagnostics(file);
        work.step();
        for (const at of addedDiagnostics(before.get(file)!, after, list)) {
          const holders = plans.flatMap((plan) => plan.path).filter(
            (fn) => fn.file === file && fn.start <= at && at < fn.end
          );
          for (const plan of plans) {
            const onPath = plan.path.some((fn) =>
              holders.length > 0 ? holders.includes(fn) : fn.file === file
            );
            if (onPath) tainted.add(plan);
          }
        }
      }

      for (const [at, plan] of plans.entries()) {
        if (outOfTime('while re-reading', plans.slice(at).filter((rest) => !tainted.has(rest)))) {
          return report();
        }
        if (tainted.has(plan)) {
          this.log(
            `Unwidened reading for ${plan.request.file_path}:${plan.request.line_number} ` +
              'dropped: marking its literals added a diagnostic on its path'
          );
          continue;
        }
        const request = this.mappedRequest(plan.request, plan.locatedByText, insertions);
        let again: InferredType | null = null;
        try {
          again = this.inferSingle(request, extractionConfig);
        } catch {
          again = null;
        }
        work.step();
        if (!again || again.type_string === plan.result.type_string) continue;
        // An invariant, not a case: `mappedRequest` is what keeps the re-read
        // on the node the inference read, so no fixture reaches this. It is
        // what turns a locator the mapping got wrong into no reading instead
        // of another node's type published as this handler's.
        const readAgain = this.readNodes.get(again.source_location);
        const list = insertions.get(plan.read.file) ?? [];
        const sameNode =
          readAgain !== undefined &&
          readAgain.getSourceFile() === plan.read.file &&
          mapBack(readAgain.getStart(), list) === plan.read.start &&
          mapBack(readAgain.getEnd(), list) === plan.read.end;
        if (!sameNode) {
          this.log(
            `Unwidened reading for ${plan.request.file_path}:${plan.request.line_number} ` +
              'dropped: the re-read did not read the node the inference read'
          );
          continue;
        }
        plan.result.unwidened_type_string = again.type_string;
        narrower += 1;
      }
      report();
    } finally {
      for (const [file, text] of originals) file.replaceWithText(text);
    }
  }

  /**
   * The request as it locates in the rewritten files: a span moved past the
   * insertions before it, an expression text replaced by the rewritten text of
   * the node it named. Lines do not move (an insertion holds no newline).
   */
  private mappedRequest(
    request: InferRequestItem,
    locatedByText: { start: number; end: number } | undefined,
    insertions: Map<SourceFile, Insertion[]>
  ): InferRequestItem {
    const sourceFile = this.getSourceFile(request.file_path);
    const list = sourceFile ? insertions.get(sourceFile) ?? [] : [];
    if (list.length === 0) return request;
    if (request.span_start !== undefined && request.span_end !== undefined) {
      return {
        ...request,
        span_start: mapForward(request.span_start, list, 'start'),
        span_end: mapForward(request.span_end, list, 'end'),
      };
    }
    if (locatedByText && sourceFile) {
      return {
        ...request,
        expression_text: sourceFile
          .getFullText()
          .slice(mapForward(locatedByText.start, list, 'start'), mapForward(locatedByText.end, list, 'end')),
      };
    }
    return request;
  }

  /**
   * Infer a single type from a request
   */
  private inferSingle(
    request: InferRequestItem,
    extractionConfig?: ExtractionConfig
  ): InferredType | null {
    const sourceFile = this.getSourceFile(request.file_path);
    if (!sourceFile) {
      this.logError(`Source file not found: ${request.file_path}`);
      return null;
    }

    switch (request.infer_kind) {
      case 'function_return':
        return this.inferFunctionReturn(sourceFile, request, extractionConfig);
      case 'response_body':
        return this.withStatedReply(
          sourceFile,
          request,
          this.inferResponseBody(sourceFile, request, extractionConfig)
        );
      case 'call_result':
        return this.inferCallResult(sourceFile, request, extractionConfig);
      case 'variable':
        return this.inferVariable(sourceFile, request, extractionConfig);
      case 'expression':
        return this.inferExpression(sourceFile, request, extractionConfig);
      case 'request_body':
        return this.withAbsentBodyMark(
          sourceFile,
          request,
          this.inferRequestBody(sourceFile, request, extractionConfig)
        );
      case 'signature_return':
        return this.inferSignatureReturn(sourceFile, request);
      case 'function_param':
        return this.inferFunctionParam(sourceFile, request);
      case 'receiver_type':
        return this.inferReceiverType(sourceFile, request);
      case 'request_config':
        return this.inferRequestConfig(sourceFile, request);
      default:
        this.logError(`Unknown infer kind: ${request.infer_kind}`);
        return null;
    }
  }

  /**
   * The call a `call_result` locator names, located exactly as `infer` locates
   * it (span, else expression text near a line). The retype check
   * (carrick#1491) rewrites this node, so it must be the node the consumer's
   * published type came from, not a guess at the line.
   */
  locateCall(request: InferRequestItem): { sourceFile: SourceFile; call: CallExpression } | undefined {
    const filePath = path.isAbsolute(request.file_path)
      ? request.file_path
      : path.join(this.repoRoot, request.file_path);
    const sourceFile = this.getSourceFile(filePath);
    if (!sourceFile) return undefined;
    const call = this.resolveTargetCallExpression(sourceFile, request);
    return call ? { sourceFile, call } : undefined;
  }

  /**
   * Get or add a source file to the project
   */
  private getSourceFile(filePath: string): SourceFile | undefined {
    let sourceFile = this.project.getSourceFile(filePath);
    if (!sourceFile) {
      try {
        sourceFile = this.project.addSourceFileAtPath(filePath);
      } catch (err) {
        this.logError(
          `Failed to add source file: ${err instanceof Error ? err.message : String(err)}`
        );
        return undefined;
      }
    }
    return sourceFile;
  }

  // ===========================================================================
  // Inference Methods by Kind
  // ===========================================================================

  /**
   * carrick#2824: a located response expression that sits in ANOTHER route's
   * handler is not this row's. A model can locate `response` at a line that
   * belongs to the next registration in the same module; reading that node
   * publishes the other route's reply. Returns the request with its locator
   * dropped, so it is answered by the registration on its own line, when:
   *  1. a registration R sits on the request's line;
   *  2. the located node is outside R and outside R's handler;
   *  3. some ancestor of the node is a registration whose handler contains it.
   * Anything else is returned unchanged. Matches on node shape, no names.
   */
  private withoutForeignLocator(
    sourceFile: SourceFile,
    request: InferRequestItem
  ): InferRequestItem {
    const node = this.resolveTargetNode(sourceFile, request);
    if (!node) return request;
    const atLine = this.registrationAtLine(sourceFile, request.line_number);
    if (!atLine) return request;
    const within = (inner: Node, outer: Node): boolean =>
      inner.getPos() >= outer.getPos() && inner.getEnd() <= outer.getEnd();
    if (within(node, atLine.registration) || within(node, atLine.handler)) {
      return request;
    }
    const inOtherHandler = node.getAncestors().some((ancestor) => {
      if (!Node.isCallExpression(ancestor) && !Node.isObjectLiteralExpression(ancestor)) {
        return false;
      }
      const handler = this.resolveRegisteredHandler(ancestor);
      return handler !== undefined && within(node, handler);
    });
    if (!inOtherHandler) return request;
    this.log(
      `Located expression at ${request.file_path}:${request.expression_line ?? request.line_number} ` +
        `sits in another registration's handler; answering by the registration on line ${request.line_number}`
    );
    return {
      ...request,
      span_start: undefined,
      span_end: undefined,
      expression_text: undefined,
      expression_line: undefined,
    };
  }

  private inferFunctionReturn(
    sourceFile: SourceFile,
    request: InferRequestItem,
    extractionConfig?: ExtractionConfig
  ): InferredType | null {
    request = this.withoutForeignLocator(sourceFile, request);
    // A `function_return` request can point at a route REGISTRATION: that is
    // the shape the scanner sends for a route whose handler RETURNS its
    // payload. The containing-function walk below then resolves the function
    // that registers the route, not the handler — so a registration declaring
    // its response contract is read from that declaration first.
    const registration = this.routeRegistrationForRequest(sourceFile, request);
    if (registration) {
      const declared = this.declaredResponseInferredType(request, registration);
      if (declared) {
        return declared;
      }
    }

    const func = this.resolveContainingFunction(sourceFile, request);

    if (!func) {
      // carrick#807: a file route exported as a call to a route builder
      // (`export const x = builder(options, async () => ...)`) starts no
      // function at its line; the handler follows the options object. Where a
      // registration sits on the line, read the handler it hands in, as the
      // line-only `response_body` path does.
      const atLine = this.registrationAtLine(sourceFile, request.line_number);
      if (atLine) {
        this.log(
          `Line ${request.line_number} is a route registration; following handler return`
        );
        return this.buildFunctionReturnInferredType(
          request,
          atLine.handler,
          extractionConfig,
          true
        );
      }
      this.log(`No function found for request at ${request.file_path}:${request.line_number}`);
      return null;
    }

    return this.buildFunctionReturnInferredType(request, func, extractionConfig);
  }

  /**
   * Build a `function_return` inferred type from an already-resolved function
   * node: unwrap `Promise<…>` / async-iterator transport, apply the extraction
   * config to the awaited type, and expand a bare named return structurally.
   * Extracted from `inferFunctionReturn` so the route-registration path
   * (`inferResponseBody` following a handler) can reuse the exact same logic
   * against the handler function instead of the containing function.
   */
  private buildFunctionReturnInferredType(
    request: InferRequestItem,
    func: FunctionLike,
    extractionConfig?: ExtractionConfig,
    treatVoidAsUnresolved = false
  ): InferredType | null {
    const returnTypeNode = func.getReturnTypeNode();
    const isExplicit = returnTypeNode !== undefined;
    let returnType = func.getReturnType();
    let typeString = typeText(returnType, func);
    /** Reasons for any `any`/`unknown` this inference ends up publishing. */
    let provenance: TypeProvenance[] | undefined;

    // Apply the agent-generated extraction config to the AWAITED type: an
    // async handler's return is Promise<Wrapper<T>>, whose symbol is
    // `Promise` — a rule naming the wrapper could never match it, and the
    // textual unwrapPromise below runs too late for the rules to see T.
    //
    // Then peel any async-iterator transport: an async generator resolver
    // resolves to `AsyncGenerator<Order>` (or `Promise<AsyncGenerator<Order>>`),
    // whose contract lives in the yield position. Unwrap it to the yield type so
    // both forms reduce to `Order` before structural expansion / rule matching.
    const awaitedType = this.unwrapAsyncIterableType(
      this.unwrapPromiseType(returnType)
    );
    // carrick#2200: a `{ statusCode, body: string }` return is transport the
    // handler built itself; its contract is the value it serialised into
    // `body`. Decided before the extraction config, whose rule for the
    // envelope would read `body` and publish `string`.
    if (this.serialisedBodyEnvelopeEngages(awaitedType, func)) {
      return this.inferSerialisedBodyEnvelope(request, func);
    }
    // A route handler that returns the reply it was handed (`return reply`)
    // states no body by that return; the reply's type is built from the
    // route's request type argument (carrick-cloud#2576).
    const returned = this.returnedExpressions(func);
    if (
      returned.length > 0 &&
      returned.every((expression) =>
        this.isRouteHandlerParameter(this.unwrapExpressionNode(expression), request)
      )
    ) {
      this.log(
        `Return at ${request.file_path}:${request.line_number} is an object the ` +
          'framework handed the handler; it states no body'
      );
      return null;
    }
    const unwrapResult = this.unwrapTypeWithConfig(awaitedType, func, extractionConfig);
    if (unwrapResult.wasUnwrapped && !unwrapResult.verifiedMachinery) {
      typeString = unwrapResult.typeString;
      // carrick#166: the union read some branches and not others, so the
      // `unknown` it answered with is a decision, not an absence. Say which
      // branches were unread — a reader can act on "this route also returns a
      // wrapper nothing could see inside" in a way they cannot act on a shrug.
      if (unwrapResult.unreadUnionBranches) {
        const unread = unwrapResult.unreadUnionBranches;
        // The COUNT is of branches; the names are deduped, because two
        // branches of the same wrapper carry the same name and listing it
        // twice says nothing.
        const names = [...new Set(unread)].sort().join(', ');
        provenance = [
          {
            path: '',
            kind: 'unknown',
            reason: 'machinery_envelope',
            detail:
              `${unread.length} of this handler's return branches resolved to transport ` +
              `no rule could read a payload out of (${names}), so publishing the branches ` +
              'that did read would state part of the contract as the whole of it',
          },
        ];
      }
    } else {
      // Either no wrapper rule fired, or one verified the wrapper's identity
      // and recovered no payload from it. Either way the resolved return
      // carries no contract of its own — it is transport machinery, or
      // `any`/`unknown` because the callee that produced it has no installed
      // declaration on a bare checkout. The contract may still be one level
      // in, in the ARGUMENT the handler handed the callee (carrick#631). Read
      // the handler's own `return` statements before abstaining.
      //
      // How much of the argument is trusted depends on WHY the return carries
      // nothing. Machinery means the callee is a known transport helper, so
      // its argument is the payload and any resolved type is read — whether
      // the structural check recognised it or a wrapper rule verified the
      // wrapper's symbol and origin and found no payload inside it. `any` /
      // `unknown` means the callee is simply unresolvable, and on a bare
      // checkout that describes most imported callees — `db.findMany({ where
      // })`, `client.send(params)` — whose argument is a query, not a
      // response. There the recovery is restricted to an argument the SOURCE
      // annotates (`satisfies` / `as` / `<T>`): the developer stated the
      // contract, so it is a claim rather than a guess.
      const machinery =
        unwrapResult.verifiedMachinery === true ||
        this.typeIsOrContainsResponseMachinery(awaitedType);
      const unresolvable = awaitedType.isAny() || awaitedType.isUnknown();
      if (machinery || unresolvable) {
        const recovered = this.recoverPayloadFromReturnStatements(
          func,
          !machinery,
          this.wireFormatFor(request)
        );
        if (recovered) {
          this.log(
            `Return type at ${request.file_path}:${request.line_number} carries no ` +
              "contract; recovered the payload from the response helper's argument"
          );
          return this.inferredFromRecoveredPayload(request, recovered);
        }
        // Nothing recoverable. Reject a wrapper envelope that IS or CONTAINS
        // framework machinery (carrick#371): `withApiWrapper({ handler: () =>
        // ({ response: Response, error })})` resolves the handler's return to
        // `{ response: Response; ... }`, which is transport machinery, not a
        // payload. Emitting it manufactures a false compat mismatch against
        // the consumer's real payload — so abstain (honest `unknown`).
        if (machinery) {
          this.log(
            `Return type at ${request.file_path}:${request.line_number} is or contains ` +
              'framework machinery (Response/Request-shaped); abstaining rather than ' +
              'capturing the wrapper envelope as a response contract'
          );
          return null;
        }
        this.log(
          `Return type at ${request.file_path}:${request.line_number} resolved to ` +
            `${awaitedType.isAny() ? 'any' : 'unknown'} and no returned call stated ` +
            'its payload type in source; leaving it unresolved'
        );
        // The honest `any` still reaches the manifest, so it carries the
        // reason with it (carrick#376): the callee has no resolvable
        // declaration and nothing in the handler says what it is, which is a
        // different answer from "the type really is `any`" and points at a
        // different fix (install the dependency, or annotate the payload).
        provenance = [
          {
            path: '',
            kind: awaitedType.isAny() ? 'any' : 'unknown',
            reason: 'no_payload_evidence',
            detail:
              "the handler returns a call whose callee has no resolvable declaration, and nothing states what it is: no returned sibling call hands it a body plus a status, and no returned argument carries a 'satisfies'/'as' annotation",
          },
        ];
      }
      // No wrapper rule fired: the (awaited) return resolved to its own type.
      // A named object return (`async (): Promise<Payment> => …`) renders as the
      // bare name `Payment`, which dangles in the source-less cross-repo bundle.
      // Expand the resolved object structurally so the real members reach the
      // bundle. `unwrapPromise` below is then a no-op on the structural form.
      typeString = this.expandResolvedTypeStructural(
        awaitedType,
        typeString,
        this.wireFormatFor(request)
      );
    }

    typeString = this.unwrapPromise(typeString, returnType);

    // Only the route-registration handler-following RESPONSE path treats a
    // void/empty return as payload-less (null) — a redirect / 204 / streaming
    // handler must stay unresolved, not become a spurious `void` type. The
    // regular `function_return` infer keeps `void` (a function that genuinely
    // returns void should report it, e.g. the signature-hint pass).
    const trimmed = typeString.trim();
    if (
      treatVoidAsUnresolved &&
      (trimmed === 'void' ||
        trimmed === 'undefined' ||
        trimmed === 'never' ||
        trimmed === '')
    ) {
      return null;
    }

    const anchor = this.unwrapArrayLevels(awaitedType);
    const inferred = this.createInferredType(
      request,
      typeString,
      isExplicit,
      this.getNodeLocation(func),
      unwrapResult.wasUnwrapped ? unwrapResult.typeString : undefined,
      this.primaryTypeSymbol(anchor.element),
      anchor.depth,
      this.primaryTypeSymbolSource(anchor.element)
    );
    if (provenance && provenance.length > 0) {
      inferred.any_provenance = provenance;
    }
    // The relation is asked only of the function the row's own line names.
    // A locator that binds inside a callback of the handler (`user` in
    // `this.users.find((user) => user.id === id)`) reaches this with that
    // callback, whose return is not the route's, and a read of the wrong
    // function must never displace the symbol the model named.
    if (this.modelSymbols.has(inferred.alias) && this.isTheRowsOwnFunction(request, func)) {
      this.relateModelSymbol(request, inferred, [
        unwrapResult.wasUnwrapped ? unwrapResult.payloadType : awaitedType,
      ]);
    }
    return inferred;
  }

  /**
   * Infer a function's return type for the signature hint. Unlike
   * `inferFunctionReturn`, this does NOT unwrap Promise or apply wrapper
   * rules — a function that returns `Promise<AuthResult>` should show exactly
   * that in its signature. Used by the function-signature collection pass.
   */
  private inferSignatureReturn(
    sourceFile: SourceFile,
    request: InferRequestItem
  ): InferredType | null {
    const func = this.resolveContainingFunction(sourceFile, request);

    if (!func) {
      this.log(`No function found for request at ${request.file_path}:${request.line_number}`);
      return null;
    }

    const isExplicit = func.getReturnTypeNode() !== undefined;
    // The compiler call is timed apart from the print of what it returns
    // (carrick#1985).
    const returnType = timedPhase('type', () => func.getReturnType());
    const typeString = typeText(returnType, func);

    return this.createInferredType(
      request,
      typeString,
      isExplicit,
      this.getNodeLocation(func)
    );
  }

  /**
   * Infer the type of a single named parameter. `is_explicit` reflects whether
   * the parameter carries a source annotation; the type string is the
   * compiler's view either way (so contextually-typed callback params resolve
   * even without an annotation). Uses ts-morph's default `getText()` form,
   * which keeps named types as names and bounds depth via the compiler's own
   * truncation.
   */
  private inferFunctionParam(
    sourceFile: SourceFile,
    request: InferRequestItem
  ): InferredType | null {
    const func = this.resolveContainingFunction(sourceFile, request);

    if (!func) {
      this.log(`No function found for request at ${request.file_path}:${request.line_number}`);
      return null;
    }

    if (!request.param_name) {
      this.logError(`function_param request missing param_name at ${request.file_path}:${request.line_number}`);
      return null;
    }

    const target = this.resolveParamTarget(func, request.param_name);

    if (!target) {
      this.log(
        `Parameter "${request.param_name}" not found at ${request.file_path}:${request.line_number}`
      );
      return null;
    }

    const isExplicit = target.param.getTypeNode() !== undefined;
    const paramType = timedPhase('type', () => target.node.getType());
    const typeString = typeText(paramType, target.node);

    // Deterministic anchor for the pub/sub two-anchor arbitration
    // (carrick#413): report the payload type's root symbol, its declaration
    // file, and the peeled array depth. The type STRING deliberately keeps
    // ts-morph's default named form (see the doc comment above) — the
    // scanner re-aims the explicit bundle at the root symbol instead of
    // pasting this string, so the alias's definition stays self-contained.
    const anchor = this.unwrapArrayLevels(this.unwrapPromiseType(paramType));
    const inferred = this.createInferredType(
      request,
      typeString,
      isExplicit,
      this.getNodeLocation(target.node),
      undefined,
      this.primaryTypeSymbol(anchor.element),
      anchor.depth,
      this.primaryTypeSymbolSource(anchor.element)
    );
    // carrick#2814: a subscriber that defaults its payload, or reads it only
    // in ways `undefined` survives, still works when none is sent.
    const binding = Node.isBindingElement(target.node) ? target.node : target.param;
    if (bindingToleratesAbsence(binding)) {
      inferred.absent_body_tolerated = true;
    }
    return inferred;
  }

  /**
   * carrick#2814: mark a route's request body as one its handler tolerates
   * going unsent, when the handler is in reach and every read of the body
   * survives `undefined` (`absent-body.ts`). The check then lets a caller that
   * may send nothing call a route whose body type has no required member.
   *
   * No mark where the handler cannot be found, where a validator the
   * registration declares reads the body first (a schema, or another function
   * handed in beside the handler), or where the handler reads no body it can
   * see: the check keeps the reading it had.
   */
  private withAbsentBodyMark(
    sourceFile: SourceFile,
    request: InferRequestItem,
    inferred: InferredType | null
  ): InferredType | null {
    if (!inferred) return inferred;
    const atLine = this.registrationAtLine(sourceFile, request.line_number);
    let registration: Node | undefined = atLine?.registration;
    let handler: FunctionLike | undefined = atLine?.handler;
    if (!handler) {
      const located = this.resolveTargetNode(sourceFile, request);
      handler = located ? this.registrationHandlerAt(located) : undefined;
      registration = handler && located ? this.unwrapExpressionNode(located) : undefined;
    }
    handler ??= this.exportedFunctionOpeningOn(sourceFile, request.line_number);
    if (!handler) return inferred;
    if (registration) {
      if (this.routeSchemaContract(registration, 'body') ?? this.validatedBodyContract(registration)) {
        return inferred;
      }
      if (this.statedParams.handsInAnotherFunction(registration, handler)) return inferred;
    }
    if (handlerToleratesAbsentBody(handler)) {
      inferred.absent_body_tolerated = true;
    }
    return inferred;
  }

  /**
   * Resolve a `function_param` locator against a function's parameter list.
   * Three shapes, tried in order:
   *
   * 1. A parameter whose name matches exactly (`(payload) => …` ← "payload").
   * 2. A parameter whose DESTRUCTURED BINDING PATTERN matches the locator
   *    text under whitespace normalization (`({ time, run }) => …` ←
   *    "{ time, run }") — the handler destructures the payload itself, so the
   *    pattern's own type IS the payload type.
   * 3. A named BINDING ELEMENT inside a destructured parameter
   *    (`({ payload }) => …` ← "payload") — the payload is one property of an
   *    envelope param, and the checker projects the element's type
   *    (catalog-worker handlers: `params: { id, payload: Infer<…> }`).
   *
   * All three read the type off a node the checker has already instantiated,
   * so generic wrappers (topic-map emitters, schema catalogs, channel handles)
   * resolve without any named payload symbol existing anywhere.
   */
  private resolveParamTarget(
    func: FunctionLike,
    paramName: string
  ): { param: ParameterDeclaration; node: Node } | undefined {
    const params = func.getParameters();

    // 1. Exact parameter name.
    const byName = params.find((p) => p.getName() === paramName);
    if (byName) {
      return { param: byName, node: byName };
    }

    // 2. Whole binding pattern, whitespace-normalized.
    const normalizedTarget = this.normalizeWhitespace(paramName);
    for (const param of params) {
      const nameNode = param.getNameNode();
      if (
        (Node.isObjectBindingPattern(nameNode) || Node.isArrayBindingPattern(nameNode)) &&
        this.normalizeWhitespace(nameNode.getText()) === normalizedTarget
      ) {
        return { param, node: param };
      }
    }

    // 3. Binding element inside a destructured parameter.
    for (const param of params) {
      const nameNode = param.getNameNode();
      if (!Node.isObjectBindingPattern(nameNode)) continue;
      for (const element of nameNode.getElements()) {
        if (element.getName() === paramName) {
          return { param, node: element };
        }
      }
    }

    return undefined;
  }

  private inferResponseBody(
    sourceFile: SourceFile,
    request: InferRequestItem,
    extractionConfig?: ExtractionConfig
  ): InferredType | null {
    request = this.withoutForeignLocator(sourceFile, request);
    const node = this.resolveTargetNode(sourceFile, request);

    if (!node) {
      // Line-only anchor (no span, no expression text) — the shape the scanner
      // sends for a named-handler route registration whose handler is declared
      // away from the registration line. Follow the handler at that line before
      // the generic function-return fallback, which can't reach a handler more
      // than a couple of lines from the registration.
      const atLine = this.registrationAtLine(sourceFile, request.line_number);
      if (atLine) {
        const declared = this.declaredResponseInferredType(
          request,
          atLine.registration
        );
        if (declared) {
          return declared;
        }
        this.log(
          `Line ${request.line_number} is a route registration; following handler return`
        );
        return this.buildFunctionReturnInferredType(
          request,
          atLine.handler,
          extractionConfig,
          true
        );
      }
      // carrick#807: the request named an expression and the file holds none
      // that matches. Where its line declares the handler, that handler is
      // still the row's, and what it returns is still the question. Without
      // this the unmatched text also fails the function lookup below, and a
      // row a line-only request answers is lost to a locator that added
      // nothing.
      const declaredHandler = request.expression_text
        ? this.handlerDeclaredAtLine(sourceFile, request.line_number)
        : undefined;
      if (declaredHandler) {
        this.log(
          `No node matches the located expression at ${request.file_path}:${request.line_number}; ` +
            'reading what the handler this line declares returns'
        );
        return this.buildFunctionReturnInferredType(
          request,
          declaredHandler,
          extractionConfig,
          true
        );
      }
      // No locator, or locator didn't resolve — likely a payload-less handler
      // (redirect, 204, streaming). Infer the containing function's return type.
      this.log(
        `No payload node found for request at ${request.file_path}:${request.line_number}; falling back to function return`
      );
      return this.inferFunctionReturn(sourceFile, request, extractionConfig);
    }

    // carrick#807: the row's line declares the handler itself, as it does for
    // a route its module's place in the project states, and the located
    // expression sits inside that handler. What the handler returns is the
    // answer a line-only request gets, and a locator must not change it.
    // The ancestor test comes first because it is free: only a node inside a
    // function that opens on the row's line can be inside the one the line
    // declares, and most rows are not.
    const insideFunctionOnLine = node
      .getAncestors()
      .some(
        (ancestor) =>
          (Node.isFunctionDeclaration(ancestor) ||
            Node.isArrowFunction(ancestor) ||
            Node.isFunctionExpression(ancestor) ||
            Node.isMethodDeclaration(ancestor)) &&
          startLineOf(ancestor) === request.line_number
      );
    const declaredHandler = insideFunctionOnLine
      ? this.handlerDeclaredAtLine(sourceFile, request.line_number)
      : undefined;
    if (
      declaredHandler &&
      declaredHandler.getStart() <= node.getStart() &&
      node.getEnd() <= declaredHandler.getEnd()
    ) {
      const returned = this.responseOfDeclaredHandler(
        request,
        declaredHandler,
        node,
        extractionConfig
      );
      if (returned !== undefined) {
        return returned;
      }
    }

    // Route-registry object literal: the locator lands on the registry entry
    // `{ method, path, handler: healthCheckHandler }`, whose response contract
    // is the handler's RETURN type — one indirection away, NOT the object's own
    // `{ method; path; handler }` shape. Follow the handler before treating the
    // object as a payload. (A call-shaped registration is handled below.)
    if (!Node.isCallExpression(node)) {
      const registryHandler = this.resolveRegisteredHandler(node);
      if (registryHandler) {
        const declared = this.declaredResponseInferredType(request, node);
        if (declared) {
          return declared;
        }
        this.log(
          `Span resolves to a route-registry object literal at ${request.file_path}:${request.line_number}; following handler return`
        );
        return this.buildFunctionReturnInferredType(
          request,
          registryHandler,
          extractionConfig,
          true
        );
      }
    }

    // carrick#1161: the locator anchors the HANDLER, not the response. When it
    // names one send among several, the route's contract is every success
    // body the handler sends, whichever of them the model happened to report.
    const fromSites = this.inferFromResponseSites(
      sourceFile,
      request,
      node,
      extractionConfig
    );
    if (fromSites !== undefined) {
      return fromSites;
    }

    // carrick#1017: the located expression evaluates to the transport wrapper
    // itself — `Response.json(entry)`, `new Response(JSON.stringify(entry))`,
    // `ctx.json(entry)` all have the platform `Response` as their type, and a
    // conditional over two of them has it on both sides. Publishing that type
    // reports the fetch API (`headers`, `ok`, `status`, `json(): Promise<any>`)
    // as the endpoint's response contract, and every consumer reading the real
    // body is then told its type is missing those members. The body is in the
    // ARGUMENT, so read it there — through the same payload rule the
    // function-return path uses, which splits the conditional, drops the
    // branch whose sibling options object states a >= 400 status, and joins
    // what survives. When it recovers nothing the row stays unresolved below:
    // no contract is published off the wrapper either way.
    const locatedType = node.getType();
    const locatedUnwrap = this.unwrapTypeWithConfig(
      locatedType,
      node,
      extractionConfig
    );
    const machineryAnchor =
      locatedUnwrap.verifiedMachinery === true ||
      (!locatedUnwrap.wasUnwrapped &&
        this.typeIsOrContainsResponseMachinery(
          this.unwrapPromiseType(locatedType)
        ));
    if (machineryAnchor) {
      const recovered = this.recoverPayloadFromResponseExpressions(
        [node],
        false,
        this.wireFormatFor(request)
      );
      if (recovered) {
        this.log(
          `Response payload at ${request.file_path}:${request.line_number} is the ` +
            "transport wrapper; recovered the body from the response call's argument"
        );
        return this.inferredFromRecoveredPayload(request, recovered);
      }
      this.log(
        `Response payload at ${request.file_path}:${request.line_number} is the ` +
          'transport wrapper and no branch states a body; leaving it unresolved'
      );
      return null;
    }

    // The resolved node IS the payload subexpression in the MVP schema.
    // Transitional fallback: if a caller still supplies a bare call expression
    // (e.g., `res.json(users)`), drill to its first argument. No method-name list.
    let payloadNode: Node = node;
    if (Node.isCallExpression(node)) {
      const args = node.getArguments();
      // A call that receives a function is a callback registration (e.g. an
      // endpoint registration like `app.get('/path', handler)`) — its first
      // argument is the route path, not a payload. The span locator falls
      // back to exactly this shape when no payload expression was reported,
      // so drilling here would put the path literal's type in the manifest.
      // A call handed a function whose result is plain data is not a
      // registration but the payload: `rows.map((r) => ({ ... }))` sends the
      // list, not the callback's return (carrick#2055).
      const registersCallback =
        args.some((arg) => Node.isArrowFunction(arg) || Node.isFunctionExpression(arg)) &&
        !this.callResultIsPlainData(node);
      if (registersCallback) {
        // A registration that DECLARES its response contract in a schema needs
        // no indirection: the declaration is the contract the framework
        // serializes against, so it wins over whatever the handler happens to
        // return.
        const declared = this.declaredResponseInferredType(request, node);
        if (declared) {
          return declared;
        }
        // The locator lands on a route registration whose handler carries the
        // response contract in its RETURN type — one indirection away. Follow
        // the handler and infer its return instead of dropping the payload.
        const handler = this.resolveRegisteredHandler(node);
        if (handler) {
          this.log(
            `Span resolves to a callback-registration call at ${request.file_path}:${request.line_number}; following handler return`
          );
          return this.buildFunctionReturnInferredType(
            request,
            handler,
            extractionConfig,
            true
          );
        }
        this.log(
          `Span resolves to a callback-registration call at ${request.file_path}:${request.line_number}; no payload to infer`
        );
        return null;
      }
      // A call whose result is a value the repo shapes is not a send: it is
      // the payload (carrick#1732), and drilling would publish its input.
      if (args.length > 0 && !this.callResultIsPayload(node)) {
        payloadNode = args[0];
      }
    }
    // A parameter the route registration hands its handler is never the
    // route's body: not the reply a handler returns (`return reply`), and not
    // the request the drill reaches in a call that forwards it
    // (`return finish(request, reply)`), whose type the framework built from
    // the route's request type argument (carrick-cloud#2576).
    if (this.isRouteHandlerParameter(payloadNode, request)) {
      this.log(
        `Response payload at ${request.file_path}:${request.line_number} is an object ` +
          'the framework handed the handler; it states no body'
      );
      return null;
    }

    const payloadType = payloadNode.getType();
    let typeString = typeText(payloadType, payloadNode);

    const unwrapResult = this.unwrapTypeWithConfig(
      payloadType,
      payloadNode,
      extractionConfig
    );

    if (unwrapResult.wasUnwrapped) {
      typeString = unwrapResult.typeString;
    } else {
      const resolved = this.unwrapPromiseType(payloadType);
      // The carrick#371 fail-closed guard. The LOCATED expression is handled
      // above, where a wrapper yields its body; this catches the argument the
      // drill reached being machinery itself (`hand(request, upstream)`),
      // which no response call carries a body for. Abstain rather than emit
      // the machinery.
      if (this.typeIsOrContainsResponseMachinery(resolved)) {
        this.log(
          `Response payload at ${request.file_path}:${request.line_number} is or contains ` +
            'framework machinery (Response/Request-shaped); abstaining rather than ' +
            'capturing it as a response contract'
        );
        return null;
      }
      // No wrapper rule fired: the payload resolved straight to its own type.
      // If that's a named object (`res.json(payment)` with `payment: Payment`),
      // `typeText` keeps the bare name `Payment`, which dangles in the
      // source-less cross-repo bundle → `any` → unverifiable. Expand the
      // resolved object structurally so the real members land in the bundle.
      // The name it falls back to is the awaited one: a located call under
      // `await` sends what it resolves to, never the promise (carrick#2055).
      typeString = this.expandResolvedTypeStructural(
        resolved,
        typeText(resolved, payloadNode),
        this.wireFormatFor(request)
      );
    }

    const anchor = this.unwrapArrayLevels(this.unwrapPromiseType(payloadType));
    const inferred = this.createInferredType(
      request,
      typeString,
      false,
      this.getNodeLocation(payloadNode),
      unwrapResult.wasUnwrapped ? unwrapResult.typeString : undefined,
      this.primaryTypeSymbol(anchor.element),
      anchor.depth,
      this.primaryTypeSymbolSource(anchor.element)
    );
    this.relateModelSymbol(request, inferred, [
      unwrapResult.wasUnwrapped ? unwrapResult.payloadType : this.unwrapPromiseType(payloadType),
    ]);
    return inferred;
  }

  /**
   * The handler a module exports on `line`: a function that opens on exactly
   * that line, exported at the top of its module, where no call on the line
   * registers a route (carrick#807).
   *
   * That is the anchor of a route its module's place in the project states:
   * the row's line is the handler's own export, and the module's exports are
   * how the framework finds it. Each condition keeps another kind of row out:
   *
   *  - the line has to be the function's first, with no tolerance. A model
   *    row that names no site of its own can sit on any line of a handler,
   *    and the nearest function to such a line is whatever is declared
   *    beside it;
   *  - a method of a class is a controller's handler, whose rows are
   *    anchored on what a model located before the method's own return;
   *  - a line that registers a route has a registration to read.
   */
  private handlerDeclaredAtLine(
    sourceFile: SourceFile,
    line: number
  ): FunctionLike | undefined {
    const declared = this.exportedFunctionOpeningOn(sourceFile, line);
    return declared && !this.registrationAtLine(sourceFile, line) ? declared : undefined;
  }

  /**
   * `handlerDeclaredAtLine` without its registration test, for a caller that
   * has already looked for a registration on the line.
   */
  private exportedFunctionOpeningOn(
    sourceFile: SourceFile,
    line: number
  ): FunctionLike | undefined {
    const declared = this.findFunctionByLine(sourceFile, line);
    if (!declared || startLineOf(declared) !== line) return undefined;

    if (Node.isFunctionDeclaration(declared)) {
      return Node.isSourceFile(declared.getParent()) && declared.isExported()
        ? declared
        : undefined;
    }
    if (!Node.isArrowFunction(declared) && !Node.isFunctionExpression(declared)) {
      return undefined;
    }
    const binding = declared.getParent();
    if (!Node.isVariableDeclaration(binding) || binding.getInitializer() !== declared) {
      return undefined;
    }
    const statement = binding.getVariableStatement();
    return statement && Node.isSourceFile(statement.getParent()) && statement.isExported()
      ? declared
      : undefined;
  }

  /**
   * The response of a handler the request's line declares, for a request
   * that also located an expression inside it (carrick#807).
   *
   * The handler's own return is read first, exactly as a `function_return`
   * request at the line reads it, and that reading stands: a body it
   * recovered, and a decision it made not to publish one. The located
   * expression is consulted in one case only. Where the handler returns a
   * call nothing resolves (its library is not installed), the return walk
   * reads no argument the source does not annotate, because it cannot tell a
   * response sender from a query. A located payload that is an ARGUMENT of
   * such a returned call says which it is, so the same walk runs again with
   * that callee taken as a sender: every success body the handler returns
   * through it, error branches dropped as before. The located expression's
   * own type is never what is published here.
   *
   * Returns `undefined` when the located expression is something the return
   * walk never looked at, so it is left to the readings below:
   *  - the handler returns nothing (`void`): it sends through a parameter;
   *  - the handler returns transport the walk read no body out of, and the
   *    located expression is not part of what it returns. A body built before
   *    the response is returned (`const res = send(body); ...; return res`)
   *    is one. Named on a send that states an error or redirect status, it is
   *    no success body and the row abstains.
   */
  private responseOfDeclaredHandler(
    request: InferRequestItem,
    handler: FunctionLike,
    located: Node,
    extractionConfig?: ExtractionConfig
  ): InferredType | null | undefined {
    const walked = this.buildFunctionReturnInferredType(request, handler, extractionConfig);
    const returned = this.responseReturnedExpressions(handler);
    if (walked === null) {
      // The walk read everything the handler returns and published nothing.
      // A located expression inside one of those returns was part of that.
      if (this.returnedSendHolding(returned, located)) {
        this.log(
          `Response locator at ${request.file_path}:${request.line_number} names a send ` +
            'its handler returns, which carries no body a contract can describe; abstaining'
        );
        return null;
      }
      // Outside the return, it is either a send itself or what one was handed.
      const sends = [
        this.peelTransparentExpression(located),
        this.receivingCallOf(located, () => true),
      ];
      const statesNoBody = sends.some((send) => {
        const status = send ? this.responseSiteStatus(send) : 'undecided';
        return status === 'error' || status === 'redirect';
      });
      if (statesNoBody) {
        this.log(
          `Response locator at ${request.file_path}:${request.line_number} names an error ` +
            'or redirect send and its handler returns no success body; abstaining'
        );
        return null;
      }
      return undefined;
    }
    const text = walked.type_string.trim();
    if (text === 'void' || text === 'undefined' || text === 'never' || text === '') {
      return undefined;
    }
    const unresolvedCallee = (walked.any_provenance ?? []).some(
      (entry) => entry.reason === 'no_payload_evidence'
    );
    if (!unresolvedCallee) {
      return walked;
    }

    const sender = this.returnedSendHolding(returned, located);
    const identity = sender ? this.calleeIdentity(sender) : undefined;
    if (identity) {
      const recovered = this.recoverPayloadFromResponseExpressions(
        returned,
        true,
        this.wireFormatFor(request),
        new Set([identity])
      );
      if (recovered) {
        this.log(
          `Return type at ${request.file_path}:${request.line_number} resolves to nothing; ` +
            'the located payload is an argument of a call the handler returns, so that ' +
            "call is a response sender and its success bodies are the route's"
        );
        return this.inferredFromRecoveredPayload(request, recovered);
      }
    }
    return walked;
  }

  /**
   * The call inside one of `returned` that the located expression is, or is
   * an argument of: the send a located payload was handed to. `undefined`
   * when the located expression is not part of what the handler returns.
   */
  private returnedSendHolding(returned: Node[], located: Node): Node | undefined {
    const branches = returned.flatMap((expression) =>
      this.expandResponseBranches(expression, 0)
    );
    const isReturned = (candidate: Node): boolean =>
      branches.some(
        (branch) =>
          branch.getStart() <= candidate.getStart() && candidate.getEnd() <= branch.getEnd()
      );
    const peeled = this.peelTransparentExpression(located);
    if (
      (Node.isCallExpression(peeled) || Node.isNewExpression(peeled)) &&
      branches.includes(peeled)
    ) {
      return peeled;
    }
    return this.receivingCallOf(located, isReturned);
  }

  /**
   * True when `node` is a bare reference to a parameter of the handler the
   * route registration on the request's line hands in (found as
   * `registrationAtLine` finds it, wrappers included): the request, the reply
   * or the context the framework passes, never a body the route sends
   * (carrick-cloud#2576). Where the parameter's type is declared is not read:
   * a parameter of any other function is data, whether the repo or an
   * installed package types it (`.then((account) => reply.send(account))`, a
   * helper's `account: Account`).
   */
  private isRouteHandlerParameter(node: Node, request: InferRequestItem): boolean {
    if (!Node.isIdentifier(node)) return false;
    const owners = (node.getSymbol()?.getDeclarations() ?? [])
      .filter((decl) => Node.isParameterDeclaration(decl))
      .map((decl) => decl.getParent().compilerNode);
    if (owners.length === 0) return false;
    const sourceFile = this.project.getSourceFile(request.file_path);
    if (!sourceFile) return false;
    const handler = this.registrationAtLine(sourceFile, request.line_number)?.handler;
    return handler !== undefined && owners.includes(handler.compilerNode);
  }

  /**
   * True when a located call's own result is the route's payload, so the
   * transitional drill into its first argument must not run (carrick#1732).
   *
   * `res.json(users)` reached that drill because nothing above it recognised
   * the send: its result reads `void`, `any` or `unknown`, and the payload is
   * the argument. `toPublicView(row)` is the opposite case: a mapper building
   * the object the route sends. Its first argument is the row it was built
   * FROM, which carries columns the route never sends.
   *
   * The call's result decides, not where its callee is declared. It is the
   * payload when it reads as one by the rule a response helper's argument is
   * read with (`nodeCarriesPayloadContract`: object-shaped, not machinery,
   * not `void`/`any`/`unknown`) and the object is not a library's own: a
   * codec's writer from `encode(message)` or a reply builder from a send is
   * the library describing itself, and keeps the drill. A library call that
   * returns the repo's own type (`toInstance(View, plain)`) is the payload.
   */
  private callResultIsPayload(call: CallExpression): boolean {
    if (this.callResultIsPlainData(call)) return true;
    const { element } = this.unwrapArrayLevels(this.unwrapPromiseType(call.getType()));
    if (this.symbolIsLibOrExternalOrigin(element.getSymbol() ?? element.getAliasSymbol())) {
      return false;
    }
    // Asked of the result as DECLARED. A value of the repo's own that
    // serialises to a string is still what the route sends, and drilling into
    // the call that built it would publish what it was built from.
    return this.nodeCarriesPayloadContract(call, false, false);
  }

  /**
   * True when a located call evaluates to plain data, so it is the payload
   * whatever its arguments and wherever its type is declared (carrick#2055).
   *
   * `rows.map(toDto)`, `rows.filter(cb)` and `listItems(ownerId)` produce the
   * list a route sends. Read as a send, the first produced the mapper's
   * function type, the second a callback's return, and the third `string`.
   * What a send or a registration hands back is never plain data: a reply
   * builder or a router carries methods, a bare send returns `void`, and a
   * serialiser returns a primitive. So the result alone tells them apart.
   *
   * Plain data, once promise levels are taken off: an object with at least
   * one member, no call or construct signature, and no member that is itself
   * callable; an array of one; or a union whose members other than `null` and
   * `undefined` all are. Tuples and empty objects are left to the callers'
   * own reading.
   */
  private callResultIsPlainData(call: CallExpression): boolean {
    const isPlainData = (type: Type, depth: number): boolean => {
      if (depth > 3) return false;
      if (type.isAny() || type.isUnknown() || type.isNever()) return false;
      if (type.isUnion()) {
        const members = type
          .getUnionTypes()
          .filter((member) => !member.isNull() && !member.isUndefined());
        return members.length > 0 && members.every((member) => isPlainData(member, depth + 1));
      }
      if (type.isIntersection()) {
        return type.getIntersectionTypes().every((member) => isPlainData(member, depth + 1));
      }
      const element = type.getArrayElementType();
      if (element) return isPlainData(element, depth + 1);
      if (!type.isObject() || type.isTuple() || this.isCallableType(type)) return false;
      if (this.typeIsOrContainsResponseMachinery(type)) return false;
      const members = type.getProperties();
      const indexed = type.getStringIndexType() ?? type.getNumberIndexType();
      if (members.length === 0 && !indexed) return false;
      const callable = (held: Type): boolean =>
        held.isUnion()
          ? held.getUnionTypes().some((member) => this.isCallableType(member))
          : this.isCallableType(held);
      if (indexed && callable(indexed)) return false;
      return members.every((member) => !callable(member.getTypeAtLocation(call)));
    };
    return isPlainData(this.unwrapPromiseType(call.getType()), 0);
  }

  /**
   * The wire representation a request's printed type takes. A route response
   * is serialised as JSON by every sender this layer reads a payload out of,
   * so it prints `toJSON()` results rather than the objects that declare them
   * (carrick#1163). Everything else prints the declared type.
   */
  private wireFormatFor(request: InferRequestItem): WireFormat {
    return request.infer_kind === 'response_body' ||
      request.infer_kind === 'function_return'
      ? 'json'
      : 'declared';
  }

  /** An inferred type for a payload read out of response sends. */
  private inferredFromRecoveredPayload(
    request: InferRequestItem,
    recovered: RecoveredPayload
  ): InferredType {
    const recoveredAnchor = recovered.anchorType
      ? this.unwrapArrayLevels(recovered.anchorType)
      : undefined;
    const resolvedSymbol = recoveredAnchor
      ? this.primaryTypeSymbol(recoveredAnchor.element)
      : undefined;
    // carrick#768: the resolved type of a stated annotation carries no symbol
    // when the alias resolves to an INSTANTIATED type (the schema-first
    // `type Body = Infer<typeof Schema>` shape): the compiler answers the
    // synthetic `__type` and the route loses the one name a reader could
    // import. The annotation as WRITTEN still names it, so read the anchor off
    // the type node when the resolved type had none. Fallback only: a
    // resolved symbol is the better answer and keeps its precedence.
    const stated = recovered.statedTypeNode;
    const writtenAnchor =
      resolvedSymbol === undefined && stated ? this.writtenAnchorOf(stated) : undefined;
    // carrick#1819: a symbol the resolved type carries has a declaration file
    // as much as one read off the annotation does.
    const resolvedSource =
      resolvedSymbol !== undefined && recoveredAnchor
        ? this.primaryTypeSymbolSource(recoveredAnchor.element)
        : undefined;
    const inferred = this.createInferredType(
      request,
      recovered.typeString,
      recovered.isExplicit,
      this.getNodeLocation(recovered.node),
      undefined,
      resolvedSymbol ?? writtenAnchor?.symbol,
      writtenAnchor ? writtenAnchor.depth : recoveredAnchor?.depth,
      writtenAnchor ? writtenAnchor.source : resolvedSource
    );
    if (recovered.responseModes) inferred.response_modes = recovered.responseModes;
    this.relateModelSymbol(request, inferred, recovered.readTypes);
    return inferred;
  }

  /**
   * carrick#1161: type a route response from every send in the handler the
   * locator anchors, rather than from the one send it names.
   *
   * The analyzer reports one response expression per row, and on a handler
   * that guards before it succeeds that is the guard's body. So the located
   * expression selects the HANDLER: the function that contains it, on a row
   * whose line is a route registration. Its returned sends are enumerated
   * (conditional branches split), each is classified by the status it states
   * (`responseSiteStatus`), the error and redirect branches are dropped, and
   * the success bodies are joined exactly as the payload walk joins return
   * statements.
   *
   * Returns `undefined` when this does not apply, which leaves the caller's
   * own reading of the located expression untouched:
   *  - the row's line is neither a route registration nor a line inside the
   *    handler the located send sits in, unless the located send states an
   *    error or redirect status and the handler keeps another send (carrick#2463);
   *  - the located expression is neither a send nor the argument of one;
   *  - that send is neither one the handler returns nor a statement of it;
   *  - it is a success send the handler does not return (a statement send
   *    joins only when the located send is dropped, and only when it ends
   *    the handler's path);
   *  - it is a success send and the only one that survives, so the join would
   *    be the located body anyway;
   *  - no joined body is the located one (a text body beside JSON ones).
   *
   * When the located send is an error or redirect branch and no success body
   * survives, the route sends no body a contract can describe: an explicit
   * `unknown` with its reason, so no later locator re-run reads the error body
   * or the redirect location back in.
   */
  private inferFromResponseSites(
    sourceFile: SourceFile,
    request: InferRequestItem,
    located: Node,
    extractionConfig?: ExtractionConfig
  ): InferredType | null | undefined {
    const func = this.findContainingFunctionForNode(located);
    if (!func) return undefined;
    // The row's line is a registration, or a line inside the handler the
    // located send sits in (carrick#2005: a file-based route's row names the
    // send's own line, and a send's line is no registration). A row whose
    // line is outside that function says nothing about the function, so it
    // is "untied" (carrick#2463): only a located error or redirect send is
    // walked then, and only when a send survives the drop.
    const tied =
      !!this.registrationAtLine(sourceFile, request.line_number) ||
      (func.getStartLineNumber() <= request.line_number &&
        request.line_number <= func.getEndLineNumber());

    const returnedBranches = this.responseReturnedExpressions(func).flatMap((expression) =>
      this.expandResponseBranches(expression, 0)
    );
    // carrick#2472 C2: the service's labels answer first. A labelled send is
    // a send, an unlabelled member of a labelled reply object is a setter;
    // where no label speaks, the structural reading decides.
    const labelledSend = (candidate: Node) =>
      this.statedParams.sendAt(this.peelTransparentExpression(candidate));
    const isLabelledSend = (candidate: Node): boolean => {
      const labelled = labelledSend(candidate);
      return labelled !== undefined && labelled !== 'setter';
    };
    const isSend = (candidate: Node): boolean => {
      const labelled = labelledSend(candidate);
      if (labelled !== undefined) return labelled !== 'setter';
      return this.isResponseSend(candidate, extractionConfig);
    };
    // carrick#2427: a handler that sends through a parameter returns nothing,
    // its sends are statements, and the located one can be an error send.
    // Statement sends find and drop that branch. A call whose result is
    // response machinery is not always a body send, though (a header or
    // cookie setter returns the response too), so a statement send is a
    // success body only when the located send is dropped and the statement
    // ends the handler's path. A located success send joins the returned
    // sends alone.
    const statementSends = this.responseSendStatements(func, isSend);
    const locatingBranches = [...returnedBranches, ...statementSends.map((s) => s.send)];
    if (locatingBranches.length === 0) return undefined;

    const peeled = this.peelTransparentExpression(located);
    const site = isSend(peeled) ? peeled : this.receivingCallOf(located, isSend);
    if (!site) return undefined;
    const contains = (outer: Node, inner: Node): boolean =>
      outer.getStart() <= inner.getStart() && inner.getEnd() <= outer.getEnd();
    const locatedBranch = locatingBranches.find((branch) => contains(branch, site));
    if (!locatedBranch) return undefined;

    const dropped = (status: ResponseSiteStatus): boolean =>
      status === 'error' || status === 'redirect';
    const siteStatus = this.responseSiteStatus(site, true);
    if (!tied && !dropped(siteStatus)) return undefined;
    let branches: Node[];
    if (dropped(siteStatus)) {
      branches = [
        ...returnedBranches,
        ...statementSends
          // A labelled send is a body wherever it sits (carrick#2472 C2).
          // FROZEN (old path, 2026-10-21): the `statementEndsFunctionPath` test
          // here; see that method.
          .filter(
            (s) =>
              s.send === locatedBranch ||
              isLabelledSend(s.send) ||
              this.statementEndsFunctionPath(s.statement, func)
          )
          .map((s) => s.send),
      ];
    } else {
      if (!returnedBranches.includes(locatedBranch)) return undefined;
      branches = returnedBranches;
    }
    const survivors: Node[] = [];
    for (const branch of branches) {
      const status = branch === locatedBranch ? siteStatus : this.responseSiteStatus(branch, true);
      if (status === 'variable') {
        this.log(
          `Response status at ${request.file_path}:${this.getNodeLocation(branch).start_line} ` +
            'is not a literal; keeping that send in the route contract'
        );
      }
      if (dropped(status)) continue;
      if (branch !== locatedBranch && !isSend(branch)) continue;
      survivors.push(branch);
    }

    // carrick#2463 (D2): an untied row claims "no success payload" only when
    // the function it reads keeps a send after the error sends are dropped.
    if (!tied && survivors.length === 0) return undefined;
    const locatedDropped = dropped(siteStatus);
    if (!locatedDropped && survivors.length <= 1) return undefined;

    const recovered =
      survivors.length > 0
        ? this.recoverPayloadFromResponseExpressions(
            survivors,
            false,
            this.wireFormatFor(request)
          )
        : null;

    if (locatedDropped) {
      if (recovered) {
        this.log(
          `Response locator at ${request.file_path}:${request.line_number} names an error ` +
            'or redirect send; publishing the success sends of its handler instead'
        );
        return this.inferredFromRecoveredPayload(request, recovered);
      }
      this.log(
        `Response locator at ${request.file_path}:${request.line_number} names an error ` +
          'or redirect send and its handler sends no success body; abstaining'
      );
      return this.decidedAbstain(request, this.getNodeLocation(site), {
        path: '',
        kind: 'unknown',
        reason: 'no_success_payload',
        detail:
          "every response this route's handler sends states an error or redirect status, " +
          'or carries no body a JSON contract can describe, so it publishes no success body',
      });
    }

    if (recovered && recovered.nodes.some((node) => contains(site, node))) {
      return this.inferredFromRecoveredPayload(request, recovered);
    }
    return undefined;
  }

  /**
   * The response sends `func` makes as statements (`res.status(409).json(e);`),
   * for a handler that sends through a parameter and returns nothing. Only the
   * outermost call of a statement is a send, only in `func`'s own scope, and
   * only where `isSend` accepts it, so a logger call or an `await save()`
   * statement is never one.
   */
  private responseSendStatements(
    func: FunctionLike,
    isSend: (candidate: Node) => boolean
  ): Array<{ statement: Node; send: Node }> {
    const body = func.getBody();
    if (!body || !Node.isBlock(body)) return [];
    const sends: Array<{ statement: Node; send: Node }> = [];
    for (const statement of body.getDescendantsOfKind(SyntaxKind.ExpressionStatement)) {
      if (this.findContainingFunctionForNode(statement) !== func) continue;
      const expression = this.peelTransparentExpression(statement.getExpression());
      if (isSend(expression)) sends.push({ statement, send: expression });
    }
    return sends;
  }

  // FROZEN (old path, 2026-10-21): this method and its one use. Deletions and
  // safety fixes only. Fixes for wrong or missing types go in send versus
  // setter, read from the framework label (carrick#2472 C4). Delete when C4 is
  // released (carrick#2472, C6).
  /**
   * True when nothing in `func` runs after `statement`: a bare `return;`
   * follows it, or it is the last statement of its block and the statement
   * that block belongs to (an `if` branch, a `try` or `catch` block) is itself
   * last, up to `func`'s body. A statement inside a loop, a `switch` case or
   * any other construct does not end the path; a send there is not read as a
   * success body.
   */
  private statementEndsFunctionPath(statement: Node, func: FunctionLike): boolean {
    const body = func.getBody();
    const next = statement.getNextSibling();
    if (next && Node.isReturnStatement(next) && !next.getExpression()) return true;
    let current: Node = statement;
    for (;;) {
      const parent = current.getParent();
      if (!parent) return false;
      if (Node.isIfStatement(parent)) {
        current = parent;
        continue;
      }
      if (!Node.isBlock(parent)) return false;
      const statements = parent.getStatements();
      if (statements[statements.length - 1] !== current) return false;
      if (parent === body) return true;
      const owner = parent.getParent();
      if (!owner) return false;
      if (Node.isIfStatement(owner) || Node.isTryStatement(owner) || Node.isBlock(owner)) {
        current = Node.isBlock(owner) ? parent : owner;
        continue;
      }
      if (Node.isCatchClause(owner)) {
        const tryStatement = owner.getParent();
        if (!tryStatement || !Node.isTryStatement(tryStatement)) return false;
        current = tryStatement;
        continue;
      }
      return false;
    }
  }

  // FROZEN (old path, 2026-10-21): this method's structural test (the whole
  // method is the upper bound). Deletions and safety fixes only. Fixes for
  // wrong or missing types go in labelled sends (carrick#2472 C4). Delete when
  // a delete-and-replay on the four repos moves no row wrongly, after
  // carrick-cloud#2379 and carrick#2445 (carrick#2472, C6).
  /**
   * True when `candidate` is a response send: a call or `new` whose result is
   * transport machinery (by the extraction config's verified rule or the
   * structural check).
   */
  private isResponseSend(
    candidate: Node,
    extractionConfig: ExtractionConfig | undefined
  ): boolean {
    const call = this.peelTransparentExpression(candidate);
    if (!Node.isCallExpression(call) && !Node.isNewExpression(call)) return false;
    const result = this.unwrapPromiseType(call.getType());
    if (result.isAny() || result.isUnknown()) return false;
    return (
      this.unwrapTypeWithConfig(result, call, extractionConfig).verifiedMachinery === true ||
      this.typeIsOrContainsResponseMachinery(result)
    );
  }

  /**
   * The call `node` is an ARGUMENT of, looking through the wrappers that do
   * not change a payload (parentheses, `as`, `satisfies`, `!`, `await`) and a
   * `JSON.stringify` around the body. `undefined` when `accept` rejects that
   * call or `node` is its callee.
   *
   * Two readings use it: a value handed to a response send is the payload that
   * send transmits, and a body read handed to a call that states what it
   * returns is a better statement of that body than the read (carrick#1382).
   */
  private receivingCallOf(
    node: Node,
    accept: (candidate: Node) => boolean
  ): Node | undefined {
    let current = node;
    let parent = current.getParent();
    while (
      parent &&
      (Node.isParenthesizedExpression(parent) ||
        Node.isAsExpression(parent) ||
        Node.isSatisfiesExpression(parent) ||
        Node.isNonNullExpression(parent) ||
        Node.isAwaitExpression(parent) ||
        (Node.isCallExpression(parent) && this.unwrapJsonStringifyArg(parent) === current))
    ) {
      current = parent;
      parent = current.getParent();
    }
    if (!parent || (!Node.isCallExpression(parent) && !Node.isNewExpression(parent))) {
      return undefined;
    }
    if (!parent.getArguments().includes(current)) return undefined;
    return accept(parent) ? parent : undefined;
  }

  /**
   * A call's result read through the library store's label (carrick#2818):
   * `whole` is the awaited type, `member` is that member of every non-nullish
   * constituent of it, and `parse` is the awaited return of that method as the
   * call returns it (a thenable's own `json()` is not on what it resolves to).
   * A part the type lacks is a decided abstain, so the capture's own re-read
   * of the raw call does not publish the envelope in its place.
   */
  private labelledCallResult(
    request: InferRequestItem,
    callExpr: CallExpression,
    label: NonNullable<InferRequestItem['result']>
  ): InferredType {
    const location = this.getNodeLocation(callExpr);
    const raw = callExpr.getType();
    const awaited = this.awaitedBeyondPromise(raw) ?? this.unwrapPromiseType(raw);
    const missing = (what: string): InferredType =>
      this.decidedAbstain(request, location, {
        path: '',
        kind: 'unknown',
        reason: 'result_member_missing',
        detail: `the library's label names ${what}, which this call's type does not have`,
      });

    let read: Type[];
    if ('whole' in label) {
      read = [awaited];
    } else if ('member' in label) {
      const name = label.member[0];
      read = [];
      const constituents = awaited.isUnion() ? awaited.getUnionTypes() : [awaited];
      for (const constituent of constituents) {
        if (isNullishType(constituent)) continue;
        const held = this.readMemberType(constituent, name, callExpr);
        if (!held) return missing(`the member '${name}'`);
        for (const part of held.isUnion() ? held.getUnionTypes() : [held]) {
          if (!isNullishType(part)) read.push(part);
        }
      }
      if (read.length === 0) return missing(`the member '${name}'`);
    } else {
      const method = (this.readMemberType(raw, label.parse, callExpr) ??
        this.readMemberType(awaited, label.parse, callExpr)) as Type | undefined;
      const signature = method
        ?.getCallSignatures()
        .find((candidate) =>
          candidate.getParameters().every((parameter) => {
            const declaration = parameter.getDeclarations()[0];
            return (
              !!declaration &&
              Node.isParameterDeclaration(declaration) &&
              (declaration.hasQuestionToken() ||
                declaration.hasInitializer() ||
                declaration.isRestParameter())
            );
          })
        );
      if (!signature) return missing(`the method '${label.parse}'`);
      const returned = signature.getReturnType();
      const parsed = this.awaitedBeyondPromise(returned) ?? this.unwrapPromiseType(returned);
      // A type parameter of the method that nothing at the call instantiates
      // (`json<J>()`) states no type: the caller supplies it.
      if (parsed.isTypeParameter()) return missing(`a concrete type from '${label.parse}'`);
      read = [parsed];
    }

    const texts: string[] = [];
    for (const part of read) {
      const text = this.structuralTextFromType(part, callExpr) ?? typeText(part, callExpr);
      if (!texts.includes(text)) texts.push(text);
    }
    const anchor =
      read.length === 1 ? this.unwrapArrayLevels(this.unwrapPromiseType(read[0])) : undefined;
    return this.createInferredType(
      request,
      texts.join(' | '),
      false,
      location,
      undefined,
      anchor ? this.primaryTypeSymbol(anchor.element) : undefined,
      anchor?.depth,
      anchor ? this.primaryTypeSymbolSource(anchor.element) : undefined
    );
  }

  private inferCallResult(
    sourceFile: SourceFile,
    request: InferRequestItem,
    extractionConfig?: ExtractionConfig
  ): InferredType | null {
    const callExpr = this.resolveTargetCallExpression(sourceFile, request);

    // A call's result is read off the call. A locator that finds no call names
    // nothing this can read, and whatever other node its text overlaps is not
    // the call's result (carrick#2056); the retype abstains on the same miss.
    if (!callExpr) {
      this.log(
        `Call-result locator at ${request.file_path}:${request.line_number} finds no call; leaving unresolved`
      );
      return null;
    }

    // carrick#2818: the library store labels which part of this call's result
    // holds the response body. The label is the library's own statement, so it
    // is read before any wrapper rule of the service's extraction config.
    if (request.result) {
      return this.labelledCallResult(request, callExpr, request.result);
    }

    // Walk up from the already-found call expression instead of re-searching
    const func = this.findContainingFunctionForNode(callExpr);
    const use = this.resolveCallResultTerminalNode(callExpr, func);
    const terminalNode = use.terminal;
    const returnType = terminalNode.getType();
    let typeString = typeText(returnType, terminalNode);
    let isExplicit = false;

    const unwrapResult = this.unwrapTypeWithConfig(
      returnType,
      terminalNode,
      extractionConfig
    );

    if (unwrapResult.wasUnwrapped) {
      typeString = unwrapResult.typeString;
      isExplicit = unwrapResult.isExplicit;
    }

    // carrick#3004: around a call whose body a chain callback reshapes, an
    // annotation or a cast further out states the reshaped value.
    const explicitType = use.handedUnreadBody
      ? null
      : this.extractExplicitTypeFromAncestor(terminalNode);
    if (explicitType) {
      typeString = explicitType;
      isExplicit = true;
    }
    // carrick#2714: the body read is cast to a placeholder
    // (`(await res.json()) as { data: Item[] } | { error: unknown }`) and the
    // source narrows the value later (`return json as { data: Item[] }`).
    // The narrowed statement is the body, and it is reported as stated, so a
    // model symbol naming an element inside it is not kept as the body.
    const narrowed =
      func && !use.handedUnreadBody ? this.narrowedBodyStatement(terminalNode, func) : undefined;
    if (narrowed) {
      typeString = narrowed.text;
      isExplicit = true;
    }

    // carrick#1843: a rule can now match a thenable or a carrier by its alias,
    // so on a result that is a carrier (`Task<Outcome<Reply<T>, E>>`) the
    // rules reach what the carrier holds before the carrier read below does.
    // That changes who finds the payload, never what is published: where the
    // rules read through the carrier, their answer is given the way the
    // carrier read gives its own. A result that is no carrier keeps the
    // answer it had.
    const where = `${request.file_path}:${request.line_number}`;
    const callArgs = this.callResultArguments(callExpr);
    const resultIsCarrier =
      !explicitType &&
      this.resultCarrierArguments(returnType, terminalNode, callArgs) !== undefined;

    // carrick#2714, carrick#1841: the call returns a request library's
    // response object, the body in a member around a status, headers and the
    // request's config. The body is what the source reads that member as, and
    // never the object: judged against a producer's body, every transport
    // member reads as one the producer does not send.
    if (!explicitType && !resultIsCarrier && func) {
      const transport = this.transportRuleOfCall(callExpr, extractionConfig);
      if (transport && !this.callStatesTypedPayload(callExpr, extractionConfig)) {
        return this.transportCallBody(request, callExpr, func, transport.rule, where);
      }
    }

    // carrick#1877: the terminal holds a thenable the names do not peel, a
    // subclass of `Promise` or a class with a `then` of its own. What a
    // caller receives is what `await` yields for it, as it is for the
    // `Promise<T>` the text unwrap at the end of this function peels: the
    // class is transport. A rule that matched the type as written, a type the
    // source states and a carrier behind the thenable have each answered
    // already and keep their answers.
    if (!unwrapResult.wasUnwrapped && !explicitType && !resultIsCarrier) {
      const yielded = this.awaitedBeyondPromise(returnType);
      if (yielded) {
        typeString = typeText(yielded, terminalNode);
      }
    }

    // The rules verified what the carrier holds as transport and read no
    // payload out of it (`Reply<unknown>`). That is the decision carrick#1841
    // makes below, and it is decided here for the same reason: left
    // undecided, the capture's own locator re-reads the raw call and
    // publishes the library's objects.
    if (unwrapResult.verifiedMachinery && resultIsCarrier) {
      this.log(
        `Call result at ${where} is a carrier of transport the service's wrapper rules ` +
          'verify and read no payload out of; this site states no response contract'
      );
      return this.transportAbstain(request, callExpr);
    }

    // carrick#1376: the call answers a RESULT CARRIER — a generic union whose
    // branches say whether the call worked and carry, on the success side, the
    // value it produced. The carrier is the transport's own bookkeeping; the
    // payload a caller receives is the success type argument, and publishing
    // the carrier instead reports an envelope as a wire contract, which is the
    // same class of answer the machinery guard refuses on the producer side.
    //
    // A type the source itself states outranks this: it is what the author
    // said. A wrapper rule that unwrapped is read first, and on a carrier it
    // no longer ends the matter (carrick#1843):
    //
    //  - a service with a rule for `Task` and none for `Outcome` leaves
    //    `Outcome<Reply<T>, E>`, which is as much a carrier as it was before
    //    the rule could match. The carrier read takes what the rule left.
    //  - a service with a rule for `Outcome` too reads the payload out of the
    //    carrier itself. That payload is what the carrier holds, and is
    //    published as the carrier read publishes it, so a site answers the
    //    same whichever of the two found its payload.
    //
    // A rule that left no single payload (a union join) leaves nothing to
    // read.
    const afterRules = unwrapResult.wasUnwrapped ? unwrapResult.payloadType : returnType;
    const readThroughCarrier =
      unwrapResult.wasUnwrapped &&
      resultIsCarrier &&
      !!afterRules &&
      this.resultCarrierArguments(afterRules, terminalNode, callArgs) === undefined;
    const carrierRead =
      explicitType || !afterRules
        ? undefined
        : readThroughCarrier
          ? { payload: afterRules }
          : this.resultCarrierPayload(afterRules, terminalNode, use.projections, where, callArgs);
    // carrick#1841: a carrier built from a response table whose success side
    // states no body (a `204` row), or whose side cannot be told, states no
    // response contract. The abstain is decided for the same reason as the
    // transport abstain above: left undecided, the capture's own locator
    // re-reads the raw call and publishes the carrier.
    if (carrierRead && 'abstain' in carrierRead) {
      this.log(`Call result at ${where}: ${carrierRead.abstain}`);
      return this.transportAbstain(request, callExpr, carrierRead.abstain);
    }
    const carrierCandidate = carrierRead?.payload;
    // carrick#1841: what the carrier holds goes through the service's wrapper
    // rules before it is published, as the call's own result did. The carrier
    // is found by its shape, so what it holds can still be a library's
    // envelope: a request library answers `Task<Outcome<Reply<T>, E>>`, and
    // `Reply<T>` is that library's response object, a status, a url and
    // headers around the body. Where a rule reads a payload out of it, that
    // payload is the body. Where a rule verifies it as the library's transport
    // and reads nothing out of it, this site states no contract, and the
    // transport object must not stand in for one: judged against a
    // producer's body, every member it adds reads as a field the producer
    // does not send.
    //
    // The abstain is decided, so it rides the row (`machinery_envelope` at the
    // root, no anchor). A plain `null` is re-read by the capture's own
    // locator, which would resolve the raw call and publish the carrier.
    const carrierUnwrap = carrierCandidate
      ? this.unwrapTypeWithConfig(carrierCandidate, terminalNode, extractionConfig)
      : undefined;
    if (
      carrierCandidate &&
      carrierUnwrap?.wasUnwrapped &&
      carrierUnwrap.typeString.trim() === 'unknown'
    ) {
      const carried = typeText(carrierCandidate, terminalNode);
      this.log(
        `Call result at ${where} carries ${carried}, which the service's wrapper rules ` +
          'verify as transport and read no payload out of; this site states no response contract'
      );
      return this.transportAbstain(request, callExpr);
    }
    const carried =
      carrierUnwrap?.wasUnwrapped && carrierUnwrap.payloadType
        ? carrierUnwrap.payloadType
        : carrierCandidate;
    // The payload rides the row as its own MEMBERS, never as its bare name
    // (#257): `derive_capture_anchors` turns a usable inference into a literal
    // capture anchor, and a bare name is out of scope where the surface
    // declares the alias, so it would decay to a top type and publish nothing
    // — trading a wrong answer for no answer. Where the payload carries no
    // member shape to print, the carrier keeps its own answer.
    const carrierText = carried ? this.structuralTextFromType(carried, terminalNode) : null;
    const carrierPayload = carrierText ? carried : undefined;
    if (carrierPayload && carrierText) {
      typeString = carrierText;
    } else if (carrierCandidate) {
      this.log(
        `Call result at ${request.file_path}:${request.line_number} carries a payload with no ` +
          'printable member shape; publishing the carrier as written'
      );
    }

    // carrick#1375: the source never read this call's result whole — every
    // read took a member out of it, and one of those members is the generic
    // this result was instantiated with. That is a source unwrapping an
    // envelope by hand, so both answers available here are wrong: the
    // projection is a fragment of the payload, and the envelope around it is
    // machinery. A wrapper rule that DID unwrap the envelope, or a type the
    // source itself states, is a contract and keeps its answer.
    //
    // A single explicit call generic (`client.get<Order[]>(url)`) is the
    // caller's own payload claim and states the contract as plainly as an
    // `as` does — it is what the anchor below falls back to on a checkout
    // with no node_modules. Several generics claim nothing unambiguously.
    //
    // The decision has to ride the row: a plain `null` is re-read by the
    // capture's own locator, which is kind-blind and would publish the
    // projection this just declined to publish (`inference_decided_no_contract`,
    // engine/type_compat_v2.rs). No anchor and no depth go with it, so the
    // row also reads as blind rather than as a sighted answer.
    if (
      use.projectionOnly &&
      this.projectionReadsGenericPayload(callExpr, use.projections) &&
      !unwrapResult.wasUnwrapped &&
      !explicitType &&
      !carrierPayload &&
      callExpr.getTypeArguments().length !== 1
    ) {
      this.log(
        `Call result at ${request.file_path}:${request.line_number} is only ever read as ` +
          `members of ${typeText(callExpr.getType(), callExpr)}; a part of a payload is not ` +
          'a payload, so this site states no response contract'
      );
      return this.decidedAbstain(request, this.getNodeLocation(callExpr), {
        path: '',
        kind: 'unknown',
        reason: 'projected_value_only',
        detail:
          "every read of this call's result takes a member out of it and none reads the " +
          'value itself, so what this site states is a part of a payload rather than the ' +
          'payload a caller receives',
      });
    }

    typeString = this.unwrapPromise(typeString, returnType);

    // carrick#2594: the call's value is untyped (`Promise<any>`,
    // `Promise<unknown>`), and the function it is made in states its type:
    // it returns the value from a function with a declared return type, holds
    // it in an annotated name, validates it with a schema, narrows it with a
    // function that returns it unchanged, casts it whole, or hands it to a
    // typed parameter. A function that converts the value into something new
    // states nothing about it, and neither does a use on a failure path.
    const untypedAnswer = typeString.trim() === 'any' || typeString.trim() === 'unknown';
    // carrick#3004: the call's own value is untyped and a callback in its
    // chain is handed it unread and builds a new value from it. The function
    // around the call states that new value, not the body, so nothing here
    // is a response contract. The decision rides the row: a plain answer is
    // re-read by the capture's own locator, which publishes the chain end.
    // A carrier's payload or a rule's that is itself a carrier of an untyped
    // value (a transport response around `unknown`) is no read either.
    const callPayload =
      carrierPayload ??
      (unwrapResult.wasUnwrapped && !untypedAnswer ? unwrapResult.payloadType : undefined) ??
      returnType;
    if (use.handedUnreadBody && (untypedAnswer || this.holdsTopTypeArgument(callPayload))) {
      this.log(
        `Call result at ${where} is untyped and handed unread to a callback that builds a ` +
          'new value from it; this site states no response contract'
      );
      return this.decidedAbstain(request, this.getNodeLocation(callExpr), {
        path: '',
        kind: 'unknown',
        reason: 'reshaped_by_callback',
        detail:
          "this call's result is handed unread to a callback in its chain that builds a new " +
          'value from it, and nothing in the chain states the body, so the value the chain ' +
          'ends in is not the response this call receives',
      });
    }
    if (untypedAnswer && func && !explicitType && !unwrapResult.wasUnwrapped && !carrierPayload) {
      const stated = this.statedInFunction(callExpr, func, {
        returned: true,
        parameters: true,
        skipFailurePaths: true,
      });
      if (stated) {
        this.log(
          `Call result at ${where} is untyped, and its function states its type ` +
            `(${stated.how}); publishing that type`
        );
        typeString = stated.text;
        isExplicit = true;
      }
    }

    // #336: consumer analogue of the #306 producer anchor. Anchor on the
    // CALL's own payload (the rule-extracted Type, else the awaited call
    // type), peeling array levels to the element symbol + depth. Without the
    // depth, `apply_inferred_array_depth` (Rust) has nothing to copy onto the
    // explicit SymbolRequest that pre-claims this alias, and the bundle for
    // `axios.get<Order[]>` renders the bare `Order` interface with the `[]`
    // gone. The anchor must come from the call expression, NOT the terminal
    // node: the def-use walk follows the binding to its last use, and in the
    // live repro that is `ordersResponse.data.length` — a `number` with no
    // symbol and no depth — while the response contract the manifest alias
    // describes is the call's payload (`Order[]`). A wrapper that unwrapped
    // to no single payload (union join, verified machinery) carries no
    // payloadType and anchors nothing; the wrapper's own symbol must never
    // anchor via that path.
    //
    // carrick#1877: the call's payload is what `await` yields for the call's
    // type. Where no rule matched that type as the names peel it, the same
    // protocol reading the text takes is read here, so a subclass of
    // `Promise` anchors the row on what it resolves to and never on itself.
    let callPayloadType = this.unwrapPromiseType(callExpr.getType());
    let callUnwrap = this.unwrapTypeWithConfig(callPayloadType, callExpr, extractionConfig);
    let readByProtocol = false;
    if (!callUnwrap.wasUnwrapped) {
      const yielded = this.awaitedBeyondPromise(callPayloadType);
      if (yielded) {
        callPayloadType = yielded;
        callUnwrap = this.unwrapTypeWithConfig(yielded, callExpr, extractionConfig);
        readByProtocol = true;
      }
    }
    // carrick#1376: the carrier's own symbol must never anchor either. The
    // surface pre-claims the alias from the anchor, so anchoring on the
    // carrier makes the capture emit the transport's bookkeeping as the
    // operation's declaration — and where the carrier is a local interface the
    // service does not export, nothing is emitted at all and the row reads
    // null. The payload the carrier was found to hold is the anchor.
    //
    // carrick#1843: that holds for a carrier a rule left of the call's own
    // result too. A rule for `Task` alone turns `Task<Outcome<T, E>>` into
    // `Outcome<T, E>` here, and where the terminal's read took no payload out
    // of it (an open payload, or a terminal further down a chain) the
    // carrier's alias would otherwise be the anchor. The same holds for a
    // carrier the protocol reading left (carrick#1877).
    const callAfterRules = callUnwrap.wasUnwrapped ? callUnwrap.payloadType : callPayloadType;
    const leftCarrier =
      (callUnwrap.wasUnwrapped || readByProtocol) &&
      !!callAfterRules &&
      this.resultCarrierArguments(callAfterRules, callExpr, callArgs) !== undefined;
    const anchorSource = narrowed?.typeNode
      ? narrowed.typeNode.getType()
      : carrierPayload ?? (leftCarrier ? undefined : callAfterRules);
    let anchor = anchorSource
      ? this.unwrapArrayLevels(this.unwrapPromiseType(anchorSource))
      : undefined;

    // #336 CI shape: the scanned checkout often has NO node_modules (the
    // GitHub Action scans a bare checkout), so the client library resolves to
    // `any`, the call's semantic type carries no symbol, and the anchor above
    // is empty — erasing the depth even though the caller wrote it out. The
    // payload claim is still in the AST: a SINGLE explicit call generic
    // (`axios.get<Order[]>`), whose type resolves against the repo's own
    // sources regardless of the untyped client. This is the same generic the
    // LLM's `primary_type_symbol` schema contract extracts, and the Rust
    // depth-copy's symbol-agreement guard makes a mismatched fallback inert.
    // Multi-generic calls are ambiguous and anchor nothing here.
    //
    // carrick#1491: why a typed call like `client.post<{ x: number }>(url)`
    // publishes no comparable type. Two facts, both pinned by
    // test/infer-inline-call-generic.test.ts:
    //  - when the client resolves and no wrapper rule unwrapped its envelope,
    //    the anchor above is the ENVELOPE's own symbol, so this fallback never
    //    runs and the text stays the whole envelope
    //    (`ClientResponse<{ x: number; }, any>`), whose defaulted request-data
    //    parameter is `any`;
    //  - when the client does not resolve, this fallback runs, and it anchors
    //    a NAMED generic only: an inline literal has no symbol, so
    //    `primaryTypeSymbol` returns nothing and no anchor is taken.
    // Either way the text carries `any`, cannot anchor a literal capture, and
    // the pair is left unverified; the check phase's retype of the consumer
    // call is what judges such a call today.
    if (!anchor || !this.primaryTypeSymbol(anchor.element)) {
      const typeArgs = callExpr.getTypeArguments();
      if (typeArgs.length === 1) {
        const argType = this.unwrapPromiseType(typeArgs[0].getType());
        const argUnwrap = this.unwrapTypeWithConfig(
          argType,
          typeArgs[0],
          extractionConfig
        );
        const argSource = argUnwrap.wasUnwrapped
          ? argUnwrap.payloadType
          : argType;
        if (argSource) {
          const argAnchor = this.unwrapArrayLevels(
            this.unwrapPromiseType(argSource)
          );
          if (this.primaryTypeSymbol(argAnchor.element)) {
            anchor = argAnchor;
          }
        }
      }
    }

    const inferred = this.createInferredType(
      request,
      typeString,
      isExplicit,
      this.getNodeLocation(terminalNode),
      unwrapResult.wasUnwrapped ? unwrapResult.typeString : undefined,
      anchor ? this.primaryTypeSymbol(anchor.element) : undefined,
      anchor?.depth,
      anchor ? this.primaryTypeSymbolSource(anchor.element) : undefined
    );
    // carrick#1749: say when the text is the source's own statement of the
    // body, and what that statement is rooted at, so the scanner can tell a
    // model symbol that names the body from one that names a part of it.
    const statedBody = narrowed?.typeNode
      ? this.statedRoot(narrowed.typeNode)
      : explicitType
        ? this.statedBodyAtRead(terminalNode)
        : undefined;
    if (statedBody) {
      inferred.stated_body = statedBody;
    }
    // carrick#1842: `string` cannot tell a body read as raw text from a JSON
    // body that is a string, and raw text states no structural contract. The
    // row says which it is, so the check phase can read the pair unverifiable
    // instead of comparing `string` with the other side's body.
    if (
      isBareStringText(typeString) &&
      (this.textReadAtTerminal(callExpr, terminalNode) || this.callChoosesTextBody(callExpr))
    ) {
      inferred.raw_text_read = true;
    }
    return inferred;
  }

  /**
   * The terminal of a call's def-use walk is a zero-argument `.text()` read of
   * the call's own result (carrick#1842): on its binding (`res.text()`,
   * through a declaration, `await` or a cast), or in the callback a `then` on
   * the call hands the response to (`fetch(u).then((res) => res.text())`).
   */
  private textReadAtTerminal(callExpr: CallExpression, terminal: Node): boolean {
    const value = Node.isVariableDeclaration(terminal) ? terminal.getInitializer() : terminal;
    const read = value ? this.unwrapExpressionNode(value) : undefined;
    if (!read || !Node.isCallExpression(read)) return false;
    const callResult = this.unwrapPromiseType(callExpr.getType()).compilerType;
    if (
      this.isTextReadOf(
        read,
        (receiver) =>
          receiver === callExpr ||
          this.unwrapPromiseType(receiver.getType()).compilerType === callResult
      )
    ) {
      return true;
    }
    const then = read.getExpression();
    if (
      !Node.isPropertyAccessExpression(then) ||
      then.getName() !== 'then' ||
      this.unwrapExpressionNode(then.getExpression()) !== callExpr
    ) {
      return false;
    }
    const callback = read.getArguments()[0];
    if (!callback || (!Node.isArrowFunction(callback) && !Node.isFunctionExpression(callback))) {
      return false;
    }
    const param = callback.getParameters()[0]?.getNameNode();
    if (!param || !Node.isIdentifier(param)) return false;
    const returned = this.returnedExpressions(callback);
    const inner = returned.length === 1 ? this.unwrapExpressionNode(returned[0]) : undefined;
    return (
      !!inner &&
      Node.isCallExpression(inner) &&
      this.isTextReadOf(
        inner,
        (receiver) => Node.isIdentifier(receiver) && receiver.getSymbol() === param.getSymbol()
      )
    );
  }

  /** `call` is `<receiver>.text()` with no arguments, and `accept` takes its receiver. */
  private isTextReadOf(call: CallExpression, accept: (receiver: Node) => boolean): boolean {
    const access = call.getExpression();
    return (
      Node.isPropertyAccessExpression(access) &&
      access.getName() === RAW_TEXT_FORMAT &&
      call.getArguments().length === 0 &&
      accept(this.unwrapExpressionNode(access.getExpression()))
    );
  }

  /**
   * The call's resolved signature, at this site, types a member of an object
   * argument as exactly the literal `'text'`, and the source passes that
   * literal there (carrick#1842). That is how a request library lets a caller
   * choose a text body from the formats it offers (`{ type: 'text' }`): a
   * generic config instantiated by the literal, or an overload taken by it.
   * A member typed as a wider union (`kind: 'text' | 'image'`) chooses no
   * format, and no member name is read.
   */
  private callChoosesTextBody(callExpr: CallExpression): boolean {
    const checker = this.project.getTypeChecker().compilerObject;
    let signature: ts.Signature | undefined;
    try {
      signature = checker.getResolvedSignature(callExpr.compilerNode);
    } catch {
      return false;
    }
    if (!signature || signature.parameters.length === 0) return false;
    return callExpr.getArguments().some((argument, index) => {
      if (!Node.isObjectLiteralExpression(argument)) return false;
      const parameter = signature!.parameters[Math.min(index, signature!.parameters.length - 1)];
      const parameterType = checker.getTypeOfSymbolAtLocation(parameter, callExpr.compilerNode);
      return argument.getProperties().some((property) => {
        if (!Node.isPropertyAssignment(property)) return false;
        const value = property.getInitializer();
        if (
          !value ||
          !(Node.isStringLiteral(value) || Node.isNoSubstitutionTemplateLiteral(value)) ||
          value.getLiteralValue() !== RAW_TEXT_FORMAT
        ) {
          return false;
        }
        const member = checker.getPropertyOfType(parameterType, property.getName());
        if (!member) return false;
        const declared = checker.getNonNullableType(
          checker.getTypeOfSymbolAtLocation(member, callExpr.compilerNode)
        );
        return declared.isStringLiteral() && declared.value === RAW_TEXT_FORMAT;
      });
    });
  }

  /**
   * What the source states the body read at `terminal` to be, when the type
   * `extractExplicitTypeFromAncestor` printed for it is stated AT the read
   * (carrick#1749): the read is the operand of that cast, or the initializer
   * of that annotated declaration, through wrappers that leave a value as it
   * is (parentheses, `await`, `!`, another cast). An annotation further out —
   * the declared type of the function the read sits in — describes something
   * else, and is not reported.
   */
  private statedBodyAtRead(terminal: Node): StatedBody | undefined {
    const typeNode = this.typeNodeStatedAtRead(terminal);
    return typeNode ? this.statedRoot(typeNode) : undefined;
  }

  /**
   * The type node `statedBodyAtRead` reads, for a caller that wants the
   * annotation itself: a consumer's response read reports its root, and a
   * handler's request read prints it (carrick#807). One reading of "stated at
   * the read" for both, so the two cannot disagree about which annotation
   * belongs to a read.
   */
  private typeNodeStatedAtRead(terminal: Node): Node | undefined {
    const typeNode = this.explicitTypeNodeFromAncestor(terminal);
    const owner = typeNode?.getParent();
    if (!typeNode || !owner || this.leavesAPositionOpen(typeNode)) return undefined;
    let current: Node = terminal;
    for (;;) {
      if (current === owner) break;
      const parent = current.getParent();
      if (!parent) return undefined;
      if (
        parent === owner &&
        Node.isVariableDeclaration(parent) &&
        parent.getInitializer() === current
      ) {
        break;
      }
      if (!this.wrapsReadTransparently(parent)) {
        return undefined;
      }
      current = parent;
    }
    return typeNode;
  }

  /**
   * A wrapper between a body read and the binding it initialises that leaves
   * the read's value as it is: `await`, parentheses, `!`, a cast, `satisfies`.
   */
  private wrapsReadTransparently(node: Node): boolean {
    return (
      Node.isParenthesizedExpression(node) ||
      Node.isAwaitExpression(node) ||
      Node.isNonNullExpression(node) ||
      Node.isAsExpression(node) ||
      Node.isTypeAssertion(node) ||
      Node.isSatisfiesExpression(node)
    );
  }

  /**
   * A stated type with `any` or `unknown` written anywhere in it (`unknown`,
   * `Record<string, unknown>`, `{ items: any[] }`) leaves a position open: it
   * is a placeholder the source narrows later (`const data: unknown = await
   * res.json()`, then `data as Entry[]`), not its statement of the body.
   */
  private leavesAPositionOpen(typeNode: Node): boolean {
    const open = (node: Node): boolean =>
      node.getKind() === SyntaxKind.AnyKeyword || node.getKind() === SyntaxKind.UnknownKeyword;
    return open(typeNode) || typeNode.getDescendants().some(open);
  }

  /**
   * The named root of a stated body type: `Promise<...>` and `PromiseLike`
   * (the language's await protocol), array levels, parentheses, `readonly`
   * and `| null`/`| undefined` are peeled, and what is left is either a type
   * reference, whose name is the root, or anything else, which has none.
   */
  private statedRoot(typeNode: Node): StatedBody {
    let node: Node = typeNode;
    let depth = 0;
    for (let step = 0; step < 16; step++) {
      if (Node.isParenthesizedTypeNode(node)) {
        node = node.getTypeNode();
        continue;
      }
      if (Node.isArrayTypeNode(node)) {
        node = node.getElementTypeNode();
        depth++;
        continue;
      }
      if (
        Node.isTypeOperatorTypeNode(node) &&
        node.getOperator() === SyntaxKind.ReadonlyKeyword
      ) {
        node = node.getTypeNode();
        continue;
      }
      if (Node.isUnionTypeNode(node)) {
        const present = node
          .getTypeNodes()
          .filter(
            (member) =>
              !(
                (Node.isLiteralTypeNode(member) &&
                  member.getLiteral().getKind() === SyntaxKind.NullKeyword) ||
                member.getKind() === SyntaxKind.UndefinedKeyword ||
                member.getKind() === SyntaxKind.NullKeyword
              )
          );
        if (present.length === 1) {
          node = present[0];
          continue;
        }
        break;
      }
      if (Node.isTypeReference(node)) {
        const name = node.getTypeName().getText();
        const args = node.getTypeArguments();
        if (args.length === 1 && (name === 'Promise' || name === 'PromiseLike')) {
          node = args[0];
          continue;
        }
        if (args.length === 1 && (name === 'Array' || name === 'ReadonlyArray')) {
          node = args[0];
          depth++;
          continue;
        }
      }
      break;
    }
    const depthField = depth > 0 ? { array_depth: depth } : {};
    if (!Node.isTypeReference(node)) {
      return depthField;
    }
    const typeName = node.getTypeName();
    const nameNode = Node.isQualifiedName(typeName) ? typeName.getRight() : typeName;
    let symbol = nameNode.getSymbol();
    if (symbol?.isAlias()) {
      symbol = symbol.getAliasedSymbol() ?? symbol;
    }
    const source = symbol?.getDeclarations()[0]?.getSourceFile().getFilePath();
    const typeArguments = node.getTypeArguments().length;
    return {
      root: nameNode.getText(),
      ...(source ? { root_source: source } : {}),
      ...depthField,
      ...(typeArguments > 0 ? { root_type_arguments: typeArguments } : {}),
    };
  }

  private inferVariable(
    sourceFile: SourceFile,
    request: InferRequestItem,
    extractionConfig?: ExtractionConfig
  ): InferredType | null {
    const node = this.resolveTargetNode(sourceFile, request);

    if (!node) {
      return null;
    }

    const varDecl = Node.isVariableDeclaration(node)
      ? node
      : node.getFirstAncestorByKind(SyntaxKind.VariableDeclaration);

    if (!varDecl) {
      return this.inferExpression(sourceFile, request, extractionConfig);
    }

    const typeNode = varDecl.getTypeNode();
    const isExplicit = typeNode !== undefined;
    let varType = varDecl.getType();
    let typeString = typeText(varType, varDecl);

    // Apply extraction config
    const unwrapResult = this.unwrapTypeWithConfig(
      varType,
      varDecl,
      extractionConfig
    );
    if (unwrapResult.wasUnwrapped) {
      typeString = unwrapResult.typeString;
    }

    typeString = this.unwrapPromise(typeString, varType);

    return this.createInferredType(
      request,
      typeString,
      isExplicit,
      this.getNodeLocation(varDecl),
      unwrapResult.wasUnwrapped ? unwrapResult.typeString : undefined
    );
  }

  private inferExpression(
    sourceFile: SourceFile,
    request: InferRequestItem,
    extractionConfig?: ExtractionConfig
  ): InferredType | null {
    const node = this.resolveTargetNode(sourceFile, request);

    if (!node) {
      return null;
    }

    const type = node.getType();
    let typeString = typeText(type, node);

    // Apply extraction config
    const unwrapResult = this.unwrapTypeWithConfig(type, node, extractionConfig);
    if (unwrapResult.wasUnwrapped) {
      typeString = unwrapResult.typeString;
    }

    typeString = this.unwrapPromise(typeString, type);

    // Deterministic anchor for the pub/sub two-anchor arbitration
    // (carrick#413). When an unwrap rule fired (envelope publishers), the
    // anchor follows the extracted PAYLOAD type, so a correct explicit
    // anchor naming the inner contract type reads as agreement, never as a
    // disagreement to arbitrate.
    const anchorSource =
      unwrapResult.wasUnwrapped && unwrapResult.payloadType
        ? unwrapResult.payloadType
        : type;
    const anchor = this.unwrapArrayLevels(this.unwrapPromiseType(anchorSource));
    return this.createInferredType(
      request,
      typeString,
      false,
      this.getNodeLocation(node),
      unwrapResult.wasUnwrapped ? unwrapResult.typeString : undefined,
      this.primaryTypeSymbol(anchor.element),
      anchor.depth,
      this.primaryTypeSymbolSource(anchor.element)
    );
  }

  private inferRequestBody(
    sourceFile: SourceFile,
    request: InferRequestItem,
    extractionConfig?: ExtractionConfig
  ): InferredType | null {
    let located = this.resolveTargetNode(sourceFile, request);

    if (!located) {
      // Line-only anchor: the scanner points a request infer at the route
      // registration line with no span/text. Follow the handler at that line and
      // read the route's declared request contract — the handler's parameter
      // annotation, the registration's schema, or the first typed request
      // expression in the body (`c.req.json<T>()`, `req.body as T`).
      const atLine = this.registrationAtLine(sourceFile, request.line_number);
      if (atLine) {
        const declared = this.declaredRequestContract(
          atLine.registration,
          atLine.handler
        );
        if (declared) {
          return this.declaredRequestInferredType(
            request,
            declared,
            this.getNodeLocation(atLine.handler)
          );
        }
      }
      // With no call registering a route on this line, the line is the
      // handler's own export: all that anchors a route its module's place in
      // the project states (carrick#807). Either way the body is what the
      // handler reads off the platform request it was handed, where the
      // source says what it read.
      const handler =
        atLine?.handler ?? this.exportedFunctionOpeningOn(sourceFile, request.line_number);
      if (!handler) {
        return null;
      }
      const read = this.platformBodyReadIn(handler);
      const stated = read ? this.requestStatedAtBodyRead(request, read) : null;
      if (stated) {
        return stated;
      }
      // The first typed read anywhere in the handler is a looser reading, and
      // is kept only where a registration already had it.
      const typedRead = atLine ? this.inferRequestReadFromHandler(atLine.handler) : null;
      return typedRead
        ? this.declaredRequestInferredType(
            request,
            { text: typedRead },
            this.getNodeLocation(handler)
          )
        : null;
    }

    // Inline-handler registration (`app.post('/x', async (c) => { … })`) or a
    // route-registry object literal: the locator lands on the registration, not
    // on a request expression, so the request contract lives ONE indirection
    // away — the first typed request-body read inside the handler body
    // (`c.req.json<T>()`, `req.body as T`, `const b: T = …`). Follow the handler
    // and scan its body before falling through to the direct-expression path
    // (which would otherwise read the registration call's useless return type).
    // Gated on the located node structurally BEING a registration, so the
    // consumer `JSON.stringify(payload)` / `req.body as T` paths are untouched.
    const registrationHandler = this.registrationHandlerAt(located);
    if (registrationHandler) {
      const requestType = this.requestContractFromRegistration(
        this.unwrapExpressionNode(located),
        registrationHandler
      );
      if (requestType) {
        return this.declaredRequestInferredType(
          request,
          requestType,
          this.getNodeLocation(registrationHandler)
        );
      }
      // carrick#827: the handler reads nothing from its parameters, so the
      // route takes no request body, and that is a decision. Left undecided,
      // the capture re-reads this same span and publishes the registration
      // call's own type (`void` wherever the server library resolves) as the
      // route's request contract.
      if (this.handlerReadsNothingFromItsParameters(registrationHandler)) {
        this.log(
          `Route registration at ${request.file_path}:${request.line_number}: its handler reads ` +
            'nothing from its parameters; the route takes no request body'
        );
        return this.decidedAbstain(request, this.getNodeLocation(registrationHandler), {
          path: '',
          kind: 'unknown',
          reason: 'no_request_body',
          detail:
            "the route's handler reads nothing from its parameters (each use of one is a call " +
            'whose value it discards or returns), so the route takes no request body',
        });
      }
      // A handler that reads from a parameter and states no type at the read:
      // do NOT fall through to read the registration call's return type, which
      // would emit a spurious framework type. Report unresolved, as before.
      this.log(
        `Route registration at ${request.file_path}:${request.line_number} has no typed request read; leaving unresolved`
      );
      return null;
    }

    // The locator resolved to an expression that is NOT the registration, but
    // the request's line still is one: this is the live producer shape, where
    // the analyzer reported a payload expression and the scanner sends it as a
    // text locator in preference to the registration span
    // (`file_orchestrator.rs`, request-body block). For a forwarding
    // schema-first route the only request-shaped text in the file is the
    // handler's request OBJECT (`request`), whose own type is the framework's
    // request machinery, or the forwarded controller call, whose type is the
    // RESPONSE. Either resolves to something, so the registration branch above
    // never runs and the route's declared contract is bypassed — the request
    // ships as machinery and decays to `any` in the cross-repo surface.
    //
    // A DECLARED contract outranks whatever expression the locator picked,
    // exactly as it does on the response side, so read the DECLARED anchors
    // (a), (b) and (b2) first and fall through to the expression only when the
    // route declares its request nowhere. The remaining registration anchor —
    // the typed request READ inside the handler body — is deliberately not
    // consulted here: that one is itself an expression, so the locator's own
    // expression stays authoritative when nothing is declared.
    const declaredAt = this.registrationAtLine(sourceFile, request.line_number);
    if (declaredAt) {
      const declared = this.declaredRequestContract(
        declaredAt.registration,
        declaredAt.handler
      );
      if (declared) {
        this.log(
          `Route registration at ${request.file_path}:${request.line_number} declares its ` +
            'request contract; using the declaration over the located expression'
        );
        return this.declaredRequestInferredType(
          request,
          declared,
          this.getNodeLocation(declaredAt.handler)
        );
      }
    }

    // A text locator (Gemini `expression_text` + `expression_line`, as opposed
    // to a byte span) can land on the property itself in `{ body:
    // JSON.stringify(body) }` — either the whole PropertyAssignment (locator
    // text is the full `key: value` source) or the property NAME identifier
    // (locator text is a bare word that exact-matches the name over the
    // value). Both type as the assigned value's own type — here the useless
    // `string` result of `JSON.stringify` — not the payload. A property name
    // is a label, not the contract expression: redirect to the value so the
    // unwraps below read the real request body. Shorthand (`{ body }`) is
    // left alone — its identifier already IS the value.
    if (Node.isPropertyAssignment(located)) {
      located = located.getInitializer() ?? located;
    } else if (
      Node.isIdentifier(located) &&
      Node.isPropertyAssignment(located.getParent()) &&
      (located.getParent() as PropertyAssignment).getNameNode() === located
    ) {
      const parent = located.getParent() as PropertyAssignment;
      located = parent.getInitializer() ?? located;
    }

    // carrick-cloud#1366: the locator named a handler parameter that a keyed
    // parameter decorator binds to ONE field of the body (`@Body('key') x: T`).
    // The parameter's type is that field's, so publishing it states the whole
    // body is a `T`. The body the handler reads is an object with one member
    // per keyed parameter the same decorator binds.
    const keyedBody = this.keyedBodyParamContract(located);
    if (keyedBody) {
      this.log(
        `Request locator at ${request.file_path}:${request.line_number} names a parameter bound ` +
          'to one body field by a keyed decorator; publishing the body those fields make up'
      );
      return this.declaredRequestInferredType(
        request,
        keyedBody,
        this.getNodeLocation(located)
      );
    }

    // carrick#2057: the locator named a member of the body the handler parsed
    // (`body.items` after `const body = (await request.json()) as ImportBody`,
    // or a binding destructured from that read). A member is one field of the
    // body, so publishing its type states the whole body is that field. The
    // body is what the source states at the read; where it states nothing,
    // nothing is published.
    const bodyRead = this.parsedBodyReadHolding(located);
    if (bodyRead) {
      const stated = this.requestStatedAtBodyRead(request, bodyRead);
      if (stated) {
        this.log(
          `Request locator at ${request.file_path}:${request.line_number} names a member of the ` +
            'body its handler parses; publishing the body the source states at that read'
        );
        return stated;
      }
      this.log(
        `Request locator at ${request.file_path}:${request.line_number} names a member of the ` +
          'body its handler parses, and the source states no type at that read; leaving unresolved'
      );
      return null;
    }

    // Mirror inferCallResult: strip `await`/`as`/parens/`!` so the inner
    // expression's type (not the surrounding `Promise<any>`) is read.
    let unwrapped = this.unwrapExpressionNode(located);

    // carrick-cloud#1366: the locator landed on the request CALL, or on the
    // request CONFIG the call takes, rather than on the body. The call's type
    // is its response, and a config's type is the config, so either one read
    // as the body publishes the wrong contract. Follow the call's signature to
    // the argument it sends as the body.
    let inCall = this.requestBodyInCall(unwrapped);
    if (inCall.kind === 'none') {
      // carrick#2628: the argument goes into a function that builds the body
      // itself. An abstain here is a decision: capture's own re-read of the
      // locator would publish the argument as the body.
      const built = this.bodyBuiltByCallee(unwrapped);
      if (built.kind === 'abstain') {
        this.log(
          `Request locator at ${request.file_path}:${request.line_number}: ${built.why}; ` +
            'the argument is not the body'
        );
        return this.decidedAbstain(request, this.getNodeLocation(unwrapped), {
          path: '',
          kind: 'unknown',
          reason: 'body_built_by_callee',
          detail: `the located argument is not the request body: ${built.why}`,
        });
      }
      inCall = built;
    }
    if (inCall.kind === 'body') {
      this.log(
        `Request locator at ${request.file_path}:${request.line_number} names ${inCall.from}; ` +
          'reading the body argument its signature declares'
      );
      unwrapped = this.unwrapExpressionNode(inCall.node);
    } else if (inCall.kind === 'body_type') {
      this.log(
        `Request locator at ${request.file_path}:${request.line_number} names ${inCall.from}; ` +
          "reading the body off the config variable's type"
      );
      return this.createInferredType(
        request,
        this.expandResolvedTypeStructural(inCall.type, typeText(inCall.type, inCall.at)),
        false,
        this.getNodeLocation(inCall.at)
      );
    } else if (inCall.kind === 'abstain') {
      this.log(
        `Request locator at ${request.file_path}:${request.line_number}: ${inCall.why}; leaving unresolved`
      );
      return null;
    } else if (inCall.kind === 'no_body') {
      this.log(
        `Request locator at ${request.file_path}:${request.line_number} is a request config ` +
          'that carries no body member; the call sends no request body'
      );
      return this.decidedAbstain(request, this.getNodeLocation(unwrapped), {
        path: '',
        kind: 'unknown',
        reason: 'no_request_body',
        detail:
          `the call's request config sets no '${inCall.member}' member, which is where the call ` +
          'takes its body, so the call sends no request body',
      });
    }

    // A `fetch` body is almost always `JSON.stringify(payload)`, whose own type
    // is the useless `string`. Drill to the serialized argument so the consumer
    // request shape is the payload's type, not `string`. General: any
    // `JSON.stringify(x)` resolves to the type of `x`.
    let node = this.unwrapJsonStringifyArg(unwrapped);

    // A locator can also land on a serialized IDENTIFIER one value-hop from
    // the payload — `const body = JSON.stringify(payload); sendBeacon(url,
    // body)` — whose own declared type is `string`, the same useless result
    // the direct-call unwrap above already handles. Follow the identifier to
    // its declaration and, only when that declaration's initializer is itself
    // a `JSON.stringify(...)` call, resolve through to the serialized
    // argument. One hop only: an identifier declared from anything else
    // already carries its own correct type and must not be rewritten.
    if (Node.isIdentifier(node)) {
      for (const def of node.getDefinitionNodes()) {
        const varDecl = Node.isVariableDeclaration(def)
          ? def
          : def.getFirstAncestorByKind(SyntaxKind.VariableDeclaration);
        const initializer = varDecl?.getInitializer();
        if (!initializer) continue;
        const unwrappedInit = this.unwrapExpressionNode(initializer);
        const stringifyArg = this.unwrapJsonStringifyArg(unwrappedInit);
        if (stringifyArg !== unwrappedInit) {
          node = stringifyArg;
          break;
        }
      }
    }

    // carrick#1101: the locator landed on a validated read — a call inside a
    // registered handler whose type IS the parsed output of a body schema that
    // the enclosing registration declares. That value is what the handler
    // receives, not what a caller sends, so publish the schema's input. Only a
    // direct call is read this way: a value that went through a serialiser or
    // a variable hop is left to the path below, so a consumer body forwarded
    // from inside a handler keeps its own type.
    if (node === unwrapped && Node.isCallExpression(node)) {
      const validatedRead = this.validatedReadRequestContract(node);
      if (validatedRead) {
        this.log(
          `Request locator at ${request.file_path}:${request.line_number} is a validated read ` +
            "of its registration's body schema; publishing the schema's input"
        );
        return this.declaredRequestInferredType(
          request,
          validatedRead,
          this.getNodeLocation(node)
        );
      }

      // carrick#1166: a validated read of a part the registration's validator
      // binds and which is not a body (`valid('param')`) is not what a caller
      // sends as the body. The route declares no body here, and saying so
      // keeps a later locator re-run from publishing the parameters instead.
      const nonBodyPart = declaredAt
        ? this.nonBodyValidatedPart(node, declaredAt.registration)
        : undefined;
      if (nonBodyPart !== undefined) {
        this.log(
          `Request locator at ${request.file_path}:${request.line_number} reads the validated ` +
            `'${nonBodyPart}' part, which is not a body; the route declares no request body here`
        );
        return this.decidedAbstain(request, this.getNodeLocation(node), {
          path: '',
          kind: 'unknown',
          reason: 'no_request_body',
          detail:
            `the located read is the '${nonBodyPart}' part a validator binds, not a request body, ` +
            'so the route states no body contract here',
        });
      }
    }

    const payloadType = node.getType();
    let typeString = typeText(payloadType, node);
    let isExplicit = false;

    // Apply extraction config
    const unwrapResult = this.unwrapTypeWithConfig(
      payloadType,
      node,
      extractionConfig
    );
    if (unwrapResult.wasUnwrapped) {
      typeString = unwrapResult.typeString;
      isExplicit = unwrapResult.isExplicit;
    } else {
      // No wrapper rule fired: the payload resolved straight to its own type.
      // For a consumer `fetch(url, { body: JSON.stringify(payload) })` with
      // `payload: CreatePaymentRequest`, `node` is the unwrapped `payload`
      // identifier and `payloadType` is the named object — `typeText` keeps the
      // bare name `CreatePaymentRequest`, which dangles in the source-less
      // cross-repo `.d.ts` bundle → resolves to `any` → unverifiable. Expand the
      // resolved object structurally so the real members reach the bundle,
      // mirroring `inferResponseBody`/`inferFunctionReturn`. A declared cast or
      // typed binding (handled below) still wins and renders its own structural
      // form via `extractExplicitTypeFromAncestor`.
      const resolved = this.unwrapPromiseType(payloadType);
      typeString = this.expandResolvedTypeStructural(resolved, typeString);
    }

    // A declared type — an `as T` cast or a typed variable binding/annotation
    // on an ancestor — wins over the call's raw `Promise<any>` / `any`. Only
    // recover when one is genuinely present; untyped `request.formData()`
    // stays `FormData` / `any`.
    //
    // Use the unwrapped `node`, not `located`: when the locator lands ON the
    // `(...) as T` cast itself, the cast is the located node (not an ancestor),
    // so an ancestor walk from `located` would miss it. `node` is the inner
    // expression whose ancestors include the `as T`, so the cast is recovered;
    // the typed-binding and untyped-control cases are unaffected.
    const explicitType = this.extractExplicitTypeFromAncestor(node);
    if (explicitType) {
      typeString = explicitType;
      isExplicit = true;
    }

    // carrick#1166: the read itself is untyped (`await c.req.json()` is `any`
    // or `unknown`), and the handler states its type further on: it validates
    // it with a schema (`Schema.safeParse(body ?? {})`, whose INPUT is the
    // request contract, carrick#1101), assigns it to an annotated name
    // (`let body: T; body = await c.req.json()`), narrows it with a function
    // that returns it unchanged, or casts the whole value (carrick#2594).
    const readType = this.unwrapPromiseType(payloadType);
    if (
      !explicitType &&
      !unwrapResult.wasUnwrapped &&
      (readType.isAny() || readType.isUnknown())
    ) {
      const func = this.findContainingFunctionForNode(node);
      const parsedWith = func
        ? this.statedInFunction(node, func, { returned: false, parameters: false, skipFailurePaths: false })
        : null;
      if (parsedWith) {
        this.log(
          `Request locator at ${request.file_path}:${request.line_number} is an untyped read ` +
            `its handler states a type for (${parsedWith.how}); publishing that type`
        );
        return this.declaredRequestInferredType(
          request,
          parsedWith,
          this.getNodeLocation(node)
        );
      }
    }

    // Publication guard (carrick#964): the locator landed on machinery — a
    // callable member of the handler's context, a method reference, a handler
    // binding — and neither a wrapper rule nor a declared type recovered a
    // payload from it. Publishing that would assert an explicit contract for a
    // type nothing can send over the wire, so abstain and let the row say
    // unknown.
    if (
      this.isCallableType(payloadType) &&
      !unwrapResult.wasUnwrapped &&
      !explicitType
    ) {
      this.log(
        `Request locator at ${request.file_path}:${request.line_number} resolved a callable ` +
          `(${typeString}); a function is not a payload, leaving unresolved`
      );
      return null;
    }

    typeString = this.unwrapPromise(typeString, payloadType);

    return this.createInferredType(
      request,
      typeString,
      isExplicit,
      this.getNodeLocation(node),
      unwrapResult.wasUnwrapped ? unwrapResult.typeString : undefined
    );
  }

  // ===========================================================================
  // Call Result Resolution
  // ===========================================================================

  private resolveCallResultTerminalNode(
    callExpr: CallExpression,
    func: FunctionLike | undefined
  ): CallResultUse {
    // carrick#1749: the call starts a chain, and a callback in it is handed
    // the body unread and casts it. That cast is the body read, and it comes
    // before the return-statement answer below, which for a chain is the
    // value the caller computed from the body, not the body.
    const chain = this.bodyReadInCallbackChain(callExpr);
    if (chain.read) {
      return { terminal: chain.read, projectionOnly: false, projections: [] };
    }
    // carrick#3004: a callback is handed the body unread and nothing states
    // it. The return below would answer the value that callback built.
    if (chain.handedUnread) {
      return { terminal: callExpr, projectionOnly: false, projections: [], handedUnreadBody: true };
    }

    // A return statement answers only for a call of the function it returns
    // from (carrick#2282). The nearest `return` above a call in a callback is
    // an OUTER function's, and its expression is what that function hands
    // back with the callback inside it: a command table, a client object, a
    // UI element. The one callback it does answer for is the executor of a
    // promise it returns (`return new Promise<T>((resolve) => { request(…,
    // (res) => resolve(…)) })`): what the caller receives is what that
    // promise settles with, and the call made in the executor produces it.
    const nearestReturn = callExpr.getFirstAncestorByKind(SyntaxKind.ReturnStatement);
    // A return whose value is an object or array literal built around the
    // call (`return { ok: true, count: countOf(await client.add(…)) }`) hands
    // back the caller's own value, which holds the result as one part: it
    // answers for the call no more than a command table does (carrick#2628).
    const returnStmt =
      nearestReturn &&
      (this.findContainingFunctionForNode(nearestReturn) === func ||
        this.callIsInReturnedPromiseExecutor(nearestReturn, func)) &&
      !this.callIsPartOfReturnedLiteral(callExpr, nearestReturn)
        ? nearestReturn
        : undefined;
    if (returnStmt) {
      const returnExpr = returnStmt.getExpression();
      if (returnExpr) {
        return { terminal: returnExpr, projectionOnly: false, projections: [] };
      }
    }

    // carrick#1851: the source reads the body in place on the call's own
    // value, `const body = await (await fetch(url)).text()`. No binding holds
    // the response, so the walk below has nothing to follow, and the call
    // itself would stand as the terminal and publish the transport object.
    // The read is the terminal, as it is when the walk finds it on a binding,
    // and a call that takes the read and states it (a schema, or a function
    // that returns it unchanged) says more than the read does (carrick#1382,
    // carrick#2595).
    const inPlaceRead = this.bodyReadOnCallValue(callExpr);
    if (inPlaceRead) {
      return {
        terminal: this.statedPayloadAroundBodyRead(inPlaceRead) ?? inPlaceRead,
        projectionOnly: false,
        projections: [],
      };
    }

    const binding = this.extractBindingFromCall(callExpr);
    if (binding && func) {
      let currentNames = binding.names;
      let lastNode: Node = binding.node;
      /** The member reads taken OUT of the tracked value (carrick#1375). */
      const projections: Node[] = [];
      /** The value itself was read: returned, passed, aliased, or body-read. */
      let sawWholeRead = false;
      // Where the call's own value states no type, a call that takes it whole
      // and states it (a schema, or a function that returns it unchanged) is
      // the first place the source states one, and the walk follows it
      // (`parse(raw)` after `const raw = await readRaw()`, carrick#1375). A
      // converter, or a declaration whose body cannot be read, states nothing
      // about the value (carrick#2595).
      const callValue = this.unwrapPromiseType(callExpr.getType());
      const callValueIsUntyped = callValue.isAny() || callValue.isUnknown();
      // A transport response handed whole to a helper is a body read through
      // that helper (`const data: T = await parseJson(response)`), as
      // `response.json()` is one in place.
      const callValueIsTransport = this.typeIsOrContainsResponseMachinery(callValue);
      const startPos = callExpr.getStart();
      const candidates = this.catchReadsDeferred(
        this.collectDefUseNodes(func),
        func,
        startPos,
        currentNames
      );

      for (const expr of candidates) {
        if (expr.getStart() <= startPos) continue;

        // carrick#1796: what the source does with the response once it has
        // FAILED describes the failure, not the payload. `if (res.ok) {
        // return ... } const errorText = await res.text()` published the error
        // text as the response contract because the walk keeps the LAST whole
        // read, and the failure path's read comes after the success path's
        // return. A read there is neither the payload nor a part of it.
        if (
          this.usesTrackedNames(expr, currentNames) &&
          reachedOnlyOnFailure(expr, func, (node) => this.isIdentifierUsage(node, currentNames))
        ) {
          continue;
        }

        // carrick#1017: a comparison is `boolean` by construction, so it is
        // never the payload a call yields. `const res = await fetch(url); if
        // (res.status === 404) return null; return await res.json() as Entry`
        // otherwise ends on the status check — the expected response type of
        // that call was published as `boolean`, and the producer's real body
        // was reported as not assignable to it.
        if (Node.isBinaryExpression(expr) && isComparisonExpression(expr)) {
          continue;
        }

        // The body a caller reads OUT of a transport response IS the payload
        // of the call that produced it: `res.json()` or `res.text()` on the
        // tracked binding, with whatever `as T` the source states around it
        // read by `extractExplicitTypeFromAncestor` below. A bare identifier never
        // matches `expressionUsesNames` (it tests DESCENDANTS), so without
        // this the body read is invisible to the def-use walk.
        if (Node.isIdentifier(expr) && this.isIdentifierUsage(expr, currentNames)) {
          const bodyRead = this.bodyReadOnReceiver(expr);
          if (bodyRead) {
            sawWholeRead = true;
            // carrick#1382: the read is a FLOOR, not a ceiling. Where the
            // source hands the body it read to a call that states what it
            // returns, that call says strictly more about the payload than
            // the untyped read does, and it must not lose to the read just
            // because the walk reaches the read second (candidates come in
            // pre-order, so the declaration of the parsed value is visited
            // before the identifier inside its own initializer).
            lastNode = this.statedPayloadAroundBodyRead(bodyRead) ?? bodyRead;
            continue;
          }
          // carrick#1375: how the source reads the value decides whether this
          // site states a payload at all. A member read takes a PART of it; a
          // return, an argument, an alias or a parse reads the value itself.
          const projection = this.projectionOnReceiver(expr);
          if (projection) {
            projections.push(projection);
          } else {
            sawWholeRead = true;
          }
        }

        if (Node.isVariableDeclaration(expr)) {
          const initializer = expr.getInitializer();
          if (
            initializer &&
            Node.isIdentifier(initializer) &&
            this.expressionUsesNames(initializer, currentNames)
          ) {
            const names = this.extractBindingNames(expr.getNameNode());
            currentNames = names;
            lastNode = expr;
          }
        }

        if (Node.isBinaryExpression(expr)) {
          const left = expr.getLeft();
          const right = expr.getRight();
          if (
            Node.isIdentifier(right) &&
            this.expressionUsesNames(right, currentNames)
          ) {
            if (Node.isIdentifier(left)) {
              const names = [left.getText()];
              currentNames = names;
              lastNode = expr;
            }
          }
        }

        // A later expression answers for the call only where it RESTATES the
        // value: an alias, a cast, a typed binding. An expression that only
        // reads MEMBERS out of it is a projection and states a part of the
        // payload, never the payload: the hook that derives
        // `query.data?.flags` off a query result published a boolean map as
        // the expected response of a call whose envelope the client declares
        // in full (carrick#1375). Any other use computes something else from
        // the value (`describe(page)`, `Math.max(...items.map(…))`,
        // `rows.map(…)`, `flag && render(row)`): it reads the value whole, and
        // the answer stays the call's own (carrick#2112). An expression that
        // takes the body out of a transport response in a form that is not
        // JSON or text (`new Uint8Array(await res.arrayBuffer())`) holds the
        // body, and answers as it did.
        if (
          this.expressionUsesNames(expr, currentNames) &&
          !this.usesNamesOnlyByProjection(expr, currentNames)
        ) {
          if (
            (callValueIsUntyped && this.statesTrackedValue(expr, currentNames)) ||
            this.restatesTrackedValue(expr, currentNames) ||
            this.holdsBinaryBodyRead(expr, currentNames) ||
            (callValueIsTransport && this.handsTrackedValueToACall(expr, currentNames))
          ) {
            lastNode = expr;
          } else {
            sawWholeRead = true;
          }
        }
      }

      return {
        terminal: this.destructuredValueTerminal(lastNode, binding.node, callExpr, callValueIsTransport),
        projectionOnly: projections.length > 0 && !sawWholeRead,
        projections,
      };
    }

    return { terminal: callExpr, projectionOnly: false, projections: [] };
  }

  /**
   * carrick#2756: what a `catch` clause does with the response describes a
   * body that failed to read (`catch (e) { log(await res.text()); throw e }`),
   * not the payload, yet it comes after the success read and would win as the
   * last whole read. Where the source uses the tracked value anywhere outside
   * a `catch` clause that does not hold the call, the clause's uses are left
   * out of the walk. Where every use is in such a clause the walk is as it was.
   */
  private catchReadsDeferred(
    candidates: Node[],
    func: Node,
    startPos: number,
    names: string[]
  ): Node[] {
    const inLaterCatch = (node: Node): boolean =>
      node
        .getAncestors()
        .some(
          (a) =>
            Node.isCatchClause(a) &&
            a.getStart() > startPos &&
            a.getAncestors().includes(func)
        );
    const usesOutside = candidates.some(
      (c) =>
        c.getStart() > startPos &&
        Node.isIdentifier(c) &&
        this.isIdentifierUsage(c, names) &&
        !inLaterCatch(c)
    );
    return usesOutside ? candidates.filter((c) => !inLaterCatch(c)) : candidates;
  }

  /**
   * Where the walk ends on the call's own destructuring binding (`const {
   * tasks } = await api.list()`), the declaration has no type of its own: the
   * checker answers `any` for it. The value it takes apart is the call's, so
   * the call answers (carrick#2112). Not for a transport value: a helper's
   * `{ body, response }` read as text would bypass the capture's machinery
   * guard, so that binding stays unread and the capture answers the row.
   */
  private destructuredValueTerminal(
    lastNode: Node,
    bindingNode: Node,
    callExpr: CallExpression,
    callValueIsTransport: boolean
  ): Node {
    if (
      lastNode !== bindingNode ||
      callValueIsTransport ||
      !Node.isVariableDeclaration(lastNode) ||
      Node.isIdentifier(lastNode.getNameNode())
    ) {
      return lastNode;
    }
    return callExpr;
  }

  /**
   * Between `callExpr` and the return statement around it there is an object
   * or array literal: the call's result is one part of a value the function
   * builds and returns.
   */
  private callIsPartOfReturnedLiteral(callExpr: CallExpression, returnStmt: Node): boolean {
    const ancestors = callExpr.getAncestors();
    return ancestors
      .slice(0, ancestors.indexOf(returnStmt))
      .some((a) => Node.isObjectLiteralExpression(a) || Node.isArrayLiteralExpression(a));
  }

  /**
   * `expr` declares a name for what a call returns when it is handed a
   * tracked name whole (`const data: T = await parseJson(response, 'op')`).
   */
  private handsTrackedValueToACall(expr: Node, names: string[]): boolean {
    return Node.isVariableDeclaration(expr) && this.callTakingTrackedValue(expr, names) !== undefined;
  }

  /**
   * The call whose result `expr` declares or assigns a name for, and the
   * index of its argument that is a tracked name whole, or undefined.
   */
  private callTakingTrackedValue(
    expr: Node,
    names: string[]
  ): { call: CallExpression; index: number } | undefined {
    const value = Node.isVariableDeclaration(expr)
      ? expr.getInitializer()
      : Node.isBinaryExpression(expr) && expr.getOperatorToken().getKind() === SyntaxKind.EqualsToken
        ? expr.getRight()
        : undefined;
    const init = this.unwrapExpressionNode(value);
    if (!Node.isCallExpression(init)) return undefined;
    const index = init
      .getArguments()
      .findIndex((argument) => this.isIdentifierUsage(this.unwrapExpressionNode(argument), names));
    return index >= 0 ? { call: init, index } : undefined;
  }

  /**
   * `expr` names the result of a call that states the tracked value it takes
   * (`callStatesItsArgument`): a schema validates it, or the callee returns it
   * unchanged. A converter's result describes what it built from the value,
   * not the value (carrick#2595).
   */
  private statesTrackedValue(expr: Node, names: string[]): boolean {
    const taking = this.callTakingTrackedValue(expr, names);
    return !!taking && this.callStatesItsArgument(taking.call, taking.index);
  }

  /** `expr` holds a binary whole-body read taken on a tracked name. */
  private holdsBinaryBodyRead(expr: Node, names: string[]): boolean {
    return expr
      .getDescendantsOfKind(SyntaxKind.Identifier)
      .some(
        (id) =>
          this.isIdentifierUsage(id, names) &&
          this.bodyReadOnReceiver(id, BINARY_BODY_READS) !== undefined
      );
  }

  /**
   * `func`, the function a call is made in, is the executor of the promise
   * `returnStmt` returns: `return new Promise<T>((resolve, reject) => { … })`
   * (carrick#2282). The promise is recognised by the language's await
   * protocol on the constructed value, never by a name.
   */
  private callIsInReturnedPromiseExecutor(
    returnStmt: Node,
    func: FunctionLike | undefined
  ): boolean {
    if (!func || !Node.isReturnStatement(returnStmt)) return false;
    const returned = this.unwrapExpressionNode(returnStmt.getExpression());
    if (!Node.isNewExpression(returned)) return false;
    const [executor] = returned.getArguments();
    if (!executor || this.unwrapExpressionNode(executor) !== func) return false;
    const constructed = returned.getType();
    return this.unwrapPromiseType(constructed) !== constructed;
  }

  /**
   * `expr` declares a name for the tracked value as it is: its initializer,
   * with the wrappers that leave a value what it was peeled (parentheses,
   * `await`, `as`, `satisfies`, `!`), is a tracked name (carrick#2112). The
   * declaration's type is then the value's, as the source restates it.
   */
  private restatesTrackedValue(expr: Node, names: string[]): boolean {
    if (!Node.isVariableDeclaration(expr)) return false;
    let value: Node | undefined = expr.getInitializer();
    for (;;) {
      const peeled: Node | undefined = value && this.unwrapExpressionNode(value);
      value = peeled && Node.isSatisfiesExpression(peeled) ? peeled.getExpression() : peeled;
      if (value === peeled) break;
    }
    return !!value && this.isIdentifierUsage(value, names);
  }

  /**
   * True when a member read on the call's result resolves to one of that
   * result's own TYPE ARGUMENTS — the source is unwrapping a generic envelope
   * by hand (`state.data` off a `ResourceState<Envelope>`), and the payload it
   * carries is the instantiation, not the envelope (carrick#1375).
   *
   * The generic is what tells the two apart. A call that answers its payload
   * directly is read member by member too, and its declared result IS the
   * contract; abstaining there would throw away the type the request boundary
   * states, which a replay over a real repo's consumer rows showed on a
   * `{ ok: true } | { ok: false; reason: string }` result read as `sent.ok`.
   */
  private projectionReadsGenericPayload(
    callExpr: CallExpression,
    projections: Node[]
  ): boolean {
    if (projections.length === 0) return false;
    const result = this.unwrapPromiseType(callExpr.getType());
    const args = [...result.getTypeArguments(), ...result.getAliasTypeArguments()];
    if (args.length === 0) return false;
    const argTexts = new Set(args.map((arg) => arg.getText()));
    return projections.some((projection) => {
      const read = this.unwrapPromiseType(projection.getType());
      const parts = [read, read.getNonNullableType()];
      for (const part of [...parts]) {
        if (part.isUnion()) parts.push(...part.getUnionTypes());
      }
      return parts.some((part) => argTexts.has(part.getText()));
    });
  }

  /**
   * The payload a RESULT CARRIER carries, or `undefined` when `type` is not
   * one or its success side cannot be told from its failure side
   * (carrick#1376).
   *
   * A carrier is recognised by its shape, never by a name: a union of object
   * branches, instantiated with two or more type arguments, at least one of
   * which a branch holds as a member. `Result<T, E>`, `Either<L, R>` and a
   * hand-rolled `{ ok: true; value: T } | { ok: false; error: E }` are all the
   * same shape, and a promise-like around one is peeled first through the
   * language's own await protocol. A single generic object — a resource state,
   * a query result — is NOT a union and is left to carrick#1375, which
   * abstains on it so a sibling site can answer.
   *
   * Which argument is the payload is decided twice over, and never guessed:
   *
   *  1. the platform's error shape. Exactly one argument that is not
   *     error-shaped, beside at least one that is, is the success side.
   *  2. what the source reads. Where every argument looks alike — `Pair<A,
   *     string>` — a member read of the carrier that resolves to exactly one
   *     of the arguments names the side this call site takes.
   *
   * Where neither decides, the carrier keeps its own answer and the limit is
   * logged: a coin flip published as a contract is worse than an envelope a
   * reader can see is an envelope.
   *
   * carrick#1841: an argument that is a RESPONSE TABLE (`{ 200: Item }`, every
   * key a status code) says which side it is by its keys, and that decides
   * first: the one table whose keys are all 2xx is the success side, and what
   * a caller receives is the table's body, the member the branch holds, never
   * the table. A success table whose rows state no body (`{ 204: void }`)
   * states no response contract. A carrier built from tables, or found
   * through the call's arguments, that nothing decides states none either:
   * the union is the client's bookkeeping, not a body, so it is not published
   * as written.
   */
  private resultCarrierPayload(
    type: Type,
    at: Node,
    projections: Node[],
    where: string,
    callArgs: Type[]
  ): { payload: Type } | { abstain: string } | undefined {
    const deferred = this.deferredConditionalPayload(type, at, projections, where, callArgs);
    if (deferred !== undefined) {
      return deferred;
    }
    const shape = this.resultCarrierArguments(type, at, callArgs);
    if (!shape) {
      return undefined;
    }
    const { carrier, carried, bodies, readOffCall } = shape;

    const checker = this.project.getTypeChecker().compilerObject;
    const tables = carried.map((arg) => {
      const entries = statusTableOf(checker, arg.compilerType);
      return {
        arg,
        entries,
        side: entries ? classifyStatusCodes(entries.map((entry) => entry.code)) : undefined,
      };
    });
    const successTables = tables.filter((table) => table.side === 'success');
    if (successTables.length === 1) {
      const { arg, entries } = successTables[0];
      const body = bodies.get(arg) ?? this.singleSuccessValue(arg, entries ?? [], at);
      if (body && statesNoBody(body.compilerType)) {
        return {
          abstain:
            `the call answers a carrier whose success side is the response table ` +
            `'${typeText(arg, at)}', and its success rows state no body, so this site ` +
            'states no response contract',
        };
      }
      if (body) {
        return { payload: body };
      }
    }
    const builtFromTables = tables.some((table) => table.side !== undefined);
    const undecided = (detail: string): { abstain: string } | undefined => {
      this.log(`Call result at ${where} answers a carrier (${typeText(carrier, at)}): ${detail}`);
      return builtFromTables || readOffCall
        ? {
            abstain:
              `the call answers a carrier (${typeText(carrier, at)}) whose success side ` +
              'cannot be told from its failure side, so this site states no response contract',
          }
        : undefined;
    };

    const argTexts = new Map(carried.map((arg) => [arg.getText(), arg]));
    const read = new Set<string>();
    for (const projection of projections) {
      const projected = this.unwrapPromiseType(projection.getType()).getNonNullableType();
      if (argTexts.has(projected.getText())) {
        read.add(projected.getText());
      }
    }
    const byRead = read.size === 1 ? argTexts.get([...read][0]) : undefined;

    const succeeded = carried.filter((arg) => !this.isErrorShaped(arg, at));
    const byShape =
      succeeded.length === 1 && succeeded.length < carried.length ? succeeded[0] : undefined;

    // The two tests must agree where both answer. An error shape is a shape,
    // and a payload is free to have one — a contact form declares a `name` and
    // a `message` too, and a service whose failure side is `{ code: number }`
    // would then read as the success side. Where the source reads a DIFFERENT
    // argument out than the shape test picked, the two disagree about which
    // side is which and nothing here knows better than the source does.
    if (byShape && byRead && byShape !== byRead) {
      return undecided(
        `its error shape names '${typeText(byShape, at)}' as the payload while the source ` +
          `reads '${typeText(byRead, at)}' out of it. Not picking one`
      );
    }
    const decided = byShape ?? byRead;
    if (decided) {
      return { payload: bodies.get(decided) ?? decided };
    }

    return undecided(
      'its success side cannot be told from its failure side: no single argument is the ' +
        'only non-error one and the source reads none of them out. Not guessing which ' +
        'argument is the payload'
    );
  }

  /**
   * carrick#1841: the branches of a conditional type the compiler left
   * deferred on a type parameter, each with its promise-like peeled, or
   * `undefined` for any other type.
   *
   * Inside a generic function whose result depends on a type parameter it has
   * not fixed (`<Throw extends boolean>(o) => client.get<Table, Errors,
   * Throw>(…)`), the call's result is such a conditional. The compiler states
   * what its branches allow itself: the constraint of the conditional, which
   * is its apparent type. No branch is read off the alias's declaration.
   */
  private deferredConditionalBranches(type: Type): Type[] | undefined {
    const compilerType = type.compilerType;
    if (!(compilerType.flags & ts.TypeFlags.Conditional)) return undefined;
    const checked = (compilerType as ts.ConditionalType).checkType;
    if (!(checked.flags & ts.TypeFlags.TypeParameter)) return undefined;
    let apparent: Type;
    try {
      apparent = type.getApparentType();
    } catch {
      return undefined;
    }
    if (apparent === type || apparent.isUnknown() || apparent.isAny()) return undefined;
    const members = apparent.isUnion() ? apparent.getUnionTypes() : [apparent];
    return members.map((member) => this.unwrapThenableType(this.unwrapPromiseType(member)));
  }

  /**
   * carrick#1841: what a call result deferred on a type parameter carries,
   * read through its branches. `undefined` where `type` is no such
   * conditional, or none of its branches is a result carrier, and the type
   * keeps the answer it had.
   *
   * Each branch that is a carrier is read by the carrier read, with the
   * call's own type arguments. Every one of them must name the same payload,
   * by the compiler's type identity, and every other branch must hold that
   * payload as the type of one of its members (the branch a client answers
   * with when it throws on failure holds the body and no error side). Then
   * the payload is the body whichever branch the caller fixes. A carrier
   * branch that states no contract decides for the whole. Branches that
   * disagree state none: the conditional is the client's bookkeeping and is
   * never published as written.
   */
  private deferredConditionalPayload(
    type: Type,
    at: Node,
    projections: Node[],
    where: string,
    callArgs: Type[]
  ): { payload: Type } | { abstain: string } | undefined {
    const branches = this.deferredConditionalBranches(type);
    if (!branches) return undefined;
    const reads = branches.map((branch) => ({
      branch,
      read: this.resultCarrierPayload(branch, at, projections, where, callArgs),
    }));
    const carriers = reads.flatMap((entry) => (entry.read ? [entry.read] : []));
    if (carriers.length === 0) return undefined;
    const abstained = carriers.find((read): read is { abstain: string } => 'abstain' in read);
    if (abstained) return abstained;
    const [{ payload }] = carriers as Array<{ payload: Type }>;
    const isPayload = (candidate: Type): boolean => candidate.compilerType === payload.compilerType;
    if (!(carriers as Array<{ payload: Type }>).every((read) => isPayload(read.payload))) {
      return {
        abstain:
          "the call's result depends on a type parameter and its branches carry different " +
          'bodies, so this site states no response contract',
      };
    }
    const holdsPayload = (branch: Type): boolean =>
      branch.getProperties().some((property) => {
        try {
          return isPayload(property.getTypeAtLocation(at));
        } catch {
          return false;
        }
      });
    if (!reads.every((entry) => entry.read !== undefined || holdsPayload(entry.branch))) {
      return {
        abstain:
          "the call's result depends on a type parameter and one of its branches does not " +
          'hold the body the others carry, so this site states no response contract',
      };
    }
    return { payload };
  }

  /**
   * The value of a response table's one success row, for a table a carrier
   * holds as itself rather than through its body. `undefined` where the table
   * has more than one success row: their union has no single type here.
   */
  private singleSuccessValue(
    table: Type,
    entries: ReturnType<typeof statusTableOf> & object,
    at: Node
  ): Type | undefined {
    const success = successEntries(entries);
    if (success.length !== 1) return undefined;
    return table.getProperty(success[0].key)?.getTypeAtLocation(at);
  }

  /**
   * carrick#1841: the type arguments a call's result was built from, for a
   * result that no longer carries them. A client whose result type is a
   * conditional alias (`Result<TData, TError, ThrowOnError>`) resolves to its
   * branch at the call, and the branch is an anonymous union with no
   * arguments of its own. The arguments are still stated in two places the
   * compiler keeps:
   *
   *  - the call's own explicit type arguments (`client.get<Table, Errors>(…)`);
   *  - the alias the callee's declared return type was written with, where
   *    the callee is a function wrapping such a call
   *    (`getItem<ThrowOnError>(…)` returning `Result<Table, Errors, ThrowOnError>`).
   *
   * An argument read here names a payload only where a branch of the result
   * holds it (`resultCarrierArguments`), so a type parameter of the callee or
   * a style flag carries nothing.
   */
  private callResultArguments(callExpr: CallExpression): Type[] {
    const explicit = callExpr.getTypeArguments().map((node) => node.getType());
    if (explicit.length > 0) return explicit;
    try {
      const declaration = this.project
        .getTypeChecker()
        .getResolvedSignature(callExpr)
        ?.getDeclaration();
      if (declaration && Node.isReturnTyped(declaration)) {
        return declaration.getReturnType().getAliasTypeArguments();
      }
    } catch {
      // A signature the checker cannot resolve states no arguments.
    }
    return [];
  }

  /**
   * The shape test of `resultCarrierPayload`: the carrier `type` is, once a
   * promise-like around it is peeled, and the type arguments a branch of it
   * holds as a member. `undefined` when `type` is not a carrier.
   */
  private resultCarrierArguments(
    type: Type,
    at: Node,
    callArgs: Type[] = []
  ):
    | { carrier: Type; carried: Type[]; bodies: Map<Type, Type>; readOffCall: boolean }
    | undefined {
    // carrick#1841: a conditional deferred on a type parameter is a carrier
    // where one of its branches is.
    const deferredBranches = this.deferredConditionalBranches(type);
    if (deferredBranches) {
      for (const branch of deferredBranches) {
        const shape = this.resultCarrierArguments(branch, at, callArgs);
        if (shape) return shape;
      }
      return undefined;
    }
    const carrier = this.unwrapThenableType(this.unwrapPromiseType(type));
    if (!carrier.isUnion()) {
      return undefined;
    }
    const branches = carrier.getUnionTypes();
    if (branches.length < 2 || !branches.every((branch) => this.isObjectShape(branch))) {
      return undefined;
    }
    const own = [...carrier.getAliasTypeArguments(), ...carrier.getTypeArguments()];
    // carrick#1841: a carrier with no arguments of its own is read through the
    // arguments its call was built from (`callResultArguments`).
    const readOffCall = own.length === 0;
    const args = readOffCall ? callArgs : own;
    if (args.length < 2) {
      return undefined;
    }
    // carrick#1841: on a carrier read off the call, a member every branch
    // holds with one type (the request and response objects a client hands
    // back beside the result) is the transport's, not a side of the result.
    const transport = readOffCall ? this.membersCommonToEveryBranch(branches, at) : new Set();
    const checker = this.project.getTypeChecker().compilerObject;
    // What a branch member holding a response table's body looks like: the
    // table indexed by its own keys (`Table[keyof Table]`), which the compiler
    // resolves to the union of the table's values.
    const tableValues = new Map<Type, Set<ts.Type>>();
    for (const arg of args) {
      const entries = statusTableOf(checker, arg.compilerType);
      if (entries) {
        tableValues.set(
          arg,
          new Set(
            entries.flatMap((entry) =>
              constituents(checker.getTypeOfSymbolAtLocation(entry.symbol, at.compilerNode))
            )
          )
        );
      }
    }
    const bodies = new Map<Type, Type>();
    // A type argument only names a payload when a branch actually holds it:
    // a generic that parameterises a status code or a key carries nothing.
    const carried = args.filter((arg) =>
      branches.some((branch) =>
        branch.getProperties().some((property) => {
          if (transport.has(property.getName())) return false;
          try {
            const held = property.getTypeAtLocation(at);
            if (held.getText() === arg.getText()) return true;
            const values = tableValues.get(arg);
            const heldTypes = constituents(held.compilerType);
            if (
              values &&
              heldTypes.length === values.size &&
              heldTypes.every((member) => values.has(member))
            ) {
              bodies.set(arg, held);
              return true;
            }
            return false;
          } catch {
            return false;
          }
        })
      )
    );
    return carried.length === 0 ? undefined : { carrier, carried, bodies, readOffCall };
  }

  /** The member names every branch holds, each with one and the same type. */
  private membersCommonToEveryBranch(branches: Type[], at: Node): Set<string> {
    const [first, ...rest] = branches;
    const common = new Set<string>();
    for (const property of first.getProperties()) {
      const name = property.getName();
      let held: Type;
      try {
        held = property.getTypeAtLocation(at);
      } catch {
        continue;
      }
      const same = rest.every((branch) => {
        try {
          return branch.getProperty(name)?.getTypeAtLocation(at).compilerType === held.compilerType;
        } catch {
          return false;
        }
      });
      if (same) common.add(name);
    }
    return common;
  }

  /**
   * What the source narrows a call's value to, where the type stated at the
   * walk's terminal is a placeholder that leaves a position open
   * (carrick#2714, `as { data: Item[] } | { error: unknown }`): the one type
   * stated for the value later in the function, on its success path
   * (`return json as { data: Item[] }`). A closed statement at the terminal
   * keeps the answer it had.
   */
  private narrowedBodyStatement(terminal: Node, func: FunctionLike): StatedInFunction | undefined {
    const placeholder = this.explicitTypeNodeFromAncestor(terminal);
    if (!placeholder || !this.leavesAPositionOpen(placeholder)) return undefined;
    const stated = this.statedInFunction(terminal, func, {
      returned: true,
      parameters: false,
      skipFailurePaths: true,
    });
    return stated?.typeNode ? stated : undefined;
  }

  /**
   * The rule that verifies a call's result as a request library's response
   * object, or undefined (carrick#2714). The result is that object when its
   * awaited type is one a rule recognises. It is that object too when the
   * call states no type arguments and its declared result, with its free
   * type parameters at their defaults (`freeResultAtDefaults`), is a type a
   * rule recognises (`get<T, R = Response<T>>(): Promise<R>`).
   * The compiler then takes the parameter from where the value goes
   * (`return client.post(…)` in a function declared `Promise<void>` types the
   * call `Promise<void>`), and that is the caller's claim about the object,
   * not what the call returns.
   */
  private transportRuleOfCall(
    callExpr: CallExpression,
    extractionConfig?: ExtractionConfig
  ): { rule: ExtractionRule } | undefined {
    if (!extractionConfig || extractionConfig.rules.length === 0) return undefined;
    const awaited = this.unwrapPromiseType(callExpr.getType());
    const direct = this.ruleRecognising(awaited, callExpr, extractionConfig);
    if (direct) return { rule: direct };
    if (callExpr.getTypeArguments().length > 0) return undefined;
    const declared = this.freeResultAtDefaults(callExpr);
    const rule = declared ? this.ruleRecognising(declared, callExpr, extractionConfig) : undefined;
    return rule ? { rule } : undefined;
  }

  /**
   * The first rule that matches `type` as a wrapper whose body is a member
   * (`payloadPropertyPath`) the type has, whether or not it reads a payload
   * out of it. A
   * wrapper with no such member (the platform's `Response`) is read by its
   * body reads (`res.json()`), which the def-use walk follows.
   */
  private ruleRecognising(
    type: Type,
    node: Node,
    config: ExtractionConfig
  ): ExtractionRule | undefined {
    return config.rules.find(
      (rule) =>
        !!rule.payloadPropertyPath?.length &&
        !!type.getProperty(rule.payloadPropertyPath[0]) &&
        this.tryUnwrapWithRule(type, node, rule, config, 0).kind !== 'no-match'
    );
  }

  /**
   * What a call's declared result is when its free type parameters take
   * their defaults, or undefined. A parameter is free when the declaration's
   * result mentions it and none of its parameters do, so only a type
   * argument, the default or the call's return position can fix it. The
   * result is read through `Promise`, a free parameter (its default), and a
   * conditional alias whose check type is a free parameter tested against
   * exactly that parameter's default (`R extends Default ? Response<T> : R`:
   * the true branch). Anything else is the type as written.
   */
  private freeResultAtDefaults(callExpr: CallExpression): Type | undefined {
    const declaration = this.project
      .getTypeChecker()
      .getResolvedSignature(callExpr)
      ?.getDeclaration();
    if (
      !declaration ||
      !(
        Node.isMethodSignature(declaration) ||
        Node.isMethodDeclaration(declaration) ||
        Node.isFunctionDeclaration(declaration) ||
        Node.isCallSignatureDeclaration(declaration)
      )
    ) {
      return undefined;
    }
    const returnNode = declaration.getReturnTypeNode();
    if (!returnNode) return undefined;
    const referenced = (node: Node | undefined): Set<string> =>
      new Set(
        (node ? [node, ...node.getDescendants()] : [])
          .filter((n) => Node.isTypeReference(n))
          .map((n) => (n as TypeReferenceNode).getTypeName().getText())
      );
    const inResult = referenced(returnNode);
    const inParameters = new Set(
      declaration.getParameters().flatMap((parameter) => [...referenced(parameter.getTypeNode())])
    );
    const free = new Map<string, Node | undefined>();
    for (const parameter of declaration.getTypeParameters()) {
      const name = parameter.getName();
      if (inResult.has(name) && !inParameters.has(name)) free.set(name, parameter.getDefault());
    }
    if (free.size === 0) return undefined;

    const atDefaults = (node: Node | undefined, depth: number): Type | undefined => {
      if (!node || depth > 4) return undefined;
      if (!Node.isTypeReference(node)) return node.getType();
      const name = node.getTypeName().getText();
      if (free.has(name)) return atDefaults(free.get(name), depth + 1);
      const args = node.getTypeArguments();
      if ((name === 'Promise' || name === 'PromiseLike') && args.length === 1) {
        return atDefaults(args[0], depth + 1);
      }
      const alias = node
        .getTypeName()
        .getSymbol()
        ?.getDeclarations()
        .find((d) => Node.isTypeAliasDeclaration(d));
      const conditional = alias && Node.isTypeAliasDeclaration(alias) ? alias.getTypeNode() : undefined;
      if (alias && Node.isTypeAliasDeclaration(alias) && conditional && Node.isConditionalTypeNode(conditional)) {
        const index = alias
          .getTypeParameters()
          .findIndex((parameter) => parameter.getName() === conditional.getCheckType().getText());
        const argument = index >= 0 ? args[index] : undefined;
        const tested = argument && Node.isTypeReference(argument) ? argument.getTypeName().getText() : undefined;
        const defaulted = tested !== undefined ? free.get(tested) : undefined;
        if (defaulted && conditional.getExtendsType().getText() === defaulted.getText()) {
          return atDefaults(conditional.getTrueType(), depth + 1);
        }
      }
      return node.getType();
    };
    return atDefaults(returnNode, 0);
  }

  /**
   * The call states the body itself: the rules read a payload out of its
   * result (`client.get<Widget>(url)`). An untyped payload is no payload to
   * the rules (they verify the object and read nothing), and a result its
   * return position chose is no response object to them.
   */
  private callStatesTypedPayload(
    callExpr: CallExpression,
    extractionConfig?: ExtractionConfig
  ): boolean {
    const unwrap = this.unwrapTypeWithConfig(
      this.unwrapPromiseType(callExpr.getType()),
      callExpr,
      extractionConfig
    );
    return unwrap.wasUnwrapped && !unwrap.verifiedMachinery && !!unwrap.payloadType;
  }

  /**
   * The body of a call that returns a response object (carrick#2714): the
   * one type the source states, in the function the call is made in, for the
   * reads of the object's payload member (`res.data as Widget`, `const w:
   * Widget = res.data`, `return res.data` from a function that declares its
   * return type), through a binding or in place. Where no read states one,
   * or two state different types, or the object is handed on whole, the
   * site states no response contract, and the abstain is decided so the
   * capture does not re-read the call and publish the object.
   */
  private transportCallBody(
    request: InferRequestItem,
    callExpr: CallExpression,
    func: FunctionLike,
    rule: ExtractionRule,
    where: string
  ): InferredType {
    const reads = this.payloadMemberReads(callExpr, func, rule.payloadPropertyPath ?? []);
    const statements = reads
      .map((read) =>
        this.statedInFunction(read, func, { returned: true, parameters: true, skipFailurePaths: true })
      )
      .filter((stated): stated is StatedInFunction => stated !== null);
    const texts = new Set(statements.map((stated) => stated.text));
    if (texts.size !== 1) {
      this.log(
        `Call result at ${where} is a response object around the body, and the source states ` +
          `${texts.size === 0 ? 'no type' : `${texts.size} different types`} for the body it reads; ` +
          'this site states no response contract'
      );
      return this.transportAbstain(
        request,
        callExpr,
        "this call returns the client's response object around the body, and the source " +
          'states no single type for the body it reads from it, so this site states no ' +
          'response contract'
      );
    }
    const [stated] = statements;
    this.log(`Call result at ${where} is a response object; the source states its body (${stated.how})`);
    const anchor = stated.typeNode
      ? this.unwrapArrayLevels(this.unwrapPromiseType(stated.typeNode.getType()))
      : undefined;
    const inferred = this.createInferredType(
      request,
      stated.text,
      true,
      this.getNodeLocation(stated.at),
      undefined,
      anchor ? this.primaryTypeSymbol(anchor.element) : undefined,
      anchor?.depth,
      anchor ? this.primaryTypeSymbolSource(anchor.element) : undefined
    );
    if (stated.typeNode) {
      inferred.stated_body = this.statedRoot(stated.typeNode);
    }
    return inferred;
  }

  /**
   * The reads of a response object's payload member (`path`, e.g. `data`)
   * that take it off the call's value: in place (`(await call).data`),
   * through the name the value is bound to (`res.data`), or as a name
   * destructured out of it (`const { data } = await call`).
   */
  private payloadMemberReads(callExpr: CallExpression, func: FunctionLike, path: string[]): Node[] {
    if (path.length === 0) return [];
    const follow = (start: Node): Node | undefined => {
      let node: Node = start;
      for (const segment of path) {
        const parent = node.getParent();
        if (
          !parent ||
          !Node.isPropertyAccessExpression(parent) ||
          parent.getExpression() !== node ||
          parent.getName() !== segment
        ) {
          return undefined;
        }
        node = parent;
      }
      return node;
    };

    let value: Node = callExpr;
    for (;;) {
      const parent = value.getParent();
      if (
        parent &&
        (Node.isParenthesizedExpression(parent) ||
          Node.isAwaitExpression(parent) ||
          Node.isNonNullExpression(parent)) &&
        parent.getExpression() === value
      ) {
        value = parent;
        continue;
      }
      break;
    }

    const inPlace = follow(value);
    if (inPlace) return [inPlace];

    const declaration = value.getParent();
    if (!declaration || !Node.isVariableDeclaration(declaration) || declaration.getInitializer() !== value) {
      return [];
    }
    const name = declaration.getNameNode();
    if (Node.isObjectBindingPattern(name)) {
      if (path.length !== 1) return [];
      return name
        .getElements()
        .filter(
          (element) =>
            !element.getDotDotDotToken() &&
            (element.getPropertyNameNode()?.getText() ?? element.getName()) === path[0] &&
            Node.isIdentifier(element.getNameNode())
        )
        .map((element) => element.getNameNode());
    }
    if (!Node.isIdentifier(name)) return [];
    const symbol = name.getSymbol()?.compilerSymbol;
    if (!symbol) return [];
    return func
      .getDescendantsOfKind(SyntaxKind.Identifier)
      .filter((id) => id !== name && id.getSymbol()?.compilerSymbol === symbol)
      .map(follow)
      .filter((read): read is Node => read !== undefined);
  }

  /**
   * The decided abstain of a call whose result carries transport the
   * service's wrapper rules verify and read no payload out of (carrick#1841,
   * carrick#1843): `unknown` with `machinery_envelope` at the root and no
   * anchor. The root reason is what keeps the capture's own locator from
   * re-reading the raw call (`inference_decided_no_contract`,
   * engine/type_compat_v2.rs).
   */
  private transportAbstain(
    request: InferRequestItem,
    callExpr: CallExpression,
    detail: string = "what this call's result carries is transport that the service's wrapper " +
      'rules verify and read no payload out of (a library response object around the body), ' +
      'so this site states no response contract'
  ): InferredType {
    return this.decidedAbstain(request, this.getNodeLocation(callExpr), {
      path: '',
      kind: 'unknown',
      reason: 'machinery_envelope',
      detail,
    });
  }

  /**
   * `Future<T>` -> `T` for a thenable, read off the await protocol rather
   * than a name: a `then` whose first parameter is a callback, whose own
   * first parameter is the value awaiting it yields. `Promise` and
   * `PromiseLike` are peeled by `unwrapPromiseType` before this.
   *
   * The callback is read through `null` and `undefined` (carrick#1877). A
   * `then` of the source's own making declares `(value: T) => void`; the
   * platform's declares `onfulfilled?: ((value: T) => ...) | null`, and that
   * is the `then` a subclass of `Promise` inherits. Read as written, an
   * optional, nullable callback has no call signature, and the subclass was
   * not seen as a thenable at all.
   *
   * A `then` that cannot be called, or whose first parameter is no callback,
   * is no protocol, and the type is returned as it is.
   *
   * The compiler's own awaited type arbitrates. This walk reads the first
   * signature of `then`; the language reads all of them. Where awaiting the
   * type and awaiting what this walk found are not the same thing to the
   * compiler (an overloaded `then` whose first signature is not the one
   * `await` takes), the walk did not read the protocol, and the type is
   * returned as it is rather than published as a guess.
   */
  private unwrapThenableType(type: Type): Type {
    const yielded = this.firstThenValue(type);
    if (yielded === type) return type;
    try {
      const checker = this.project.getTypeChecker().compilerObject;
      const awaited = checker.getAwaitedType(type.compilerType);
      return awaited !== undefined && awaited === checker.getAwaitedType(yielded.compilerType)
        ? yielded
        : type;
    } catch {
      return type;
    }
  }

  /** The value `then`'s first signature hands its callback, read until it stops changing. */
  private firstThenValue(type: Type): Type {
    let current = type;
    for (let depth = 0; depth < 8; depth++) {
      const then = current.getProperty('then');
      const declaration = then?.getDeclarations()[0];
      if (!then || !declaration) return current;
      let onValue: Type | undefined;
      try {
        onValue = then
          .getTypeAtLocation(declaration)
          .getCallSignatures()[0]
          ?.getParameters()[0]
          ?.getTypeAtLocation(declaration)
          .getNonNullableType();
      } catch {
        return current;
      }
      const value = onValue
        ?.getCallSignatures()[0]
        ?.getParameters()[0]
        ?.getTypeAtLocation(declaration);
      if (!value || value === current) return current;
      current = value;
    }
    return current;
  }

  /**
   * What `await` yields for `type` where the names do not say: `type` is,
   * once `Promise` and `PromiseLike` are peeled, a thenable by the protocol
   * (a subclass of `Promise`, a class with a `then` of its own). `undefined`
   * where the names say it all or `type` is no thenable, so a caller keeps
   * the reading it had (carrick#1877).
   */
  private awaitedBeyondPromise(type: Type): Type | undefined {
    const named = this.unwrapPromiseType(type);
    const yielded = this.unwrapThenableType(named);
    return yielded === named ? undefined : yielded;
  }

  /**
   * The platform's error shape, in full: `name` and `message` strings AND a
   * `stack`, which is what the `Error` interface declares and every subclass
   * of it inherits.
   *
   * `stack` is what makes the test a test. A name and a message alone are a
   * shape a PAYLOAD can have — a contact form declares both — and reading such
   * a payload as the failure side would publish the other argument, which is
   * the concrete-but-wrong answer this whole rule exists to avoid. A union is
   * error-shaped when every member of it is.
   */
  private isErrorShaped(type: Type, at: Node): boolean {
    if (type.isUnion()) {
      return type.getUnionTypes().every((part) => this.isErrorShaped(part, at));
    }
    const typeOf = (property: TsSymbol | undefined): Type | undefined => {
      if (!property) return undefined;
      try {
        return property.getTypeAtLocation(at);
      } catch {
        return undefined;
      }
    };
    const stringy = (property: TsSymbol | undefined): boolean =>
      typeOf(property)?.getNonNullableType().getText() === 'string';
    return (
      stringy(type.getProperty('name')) &&
      stringy(type.getProperty('message')) &&
      stringy(type.getProperty('stack'))
    );
  }

  /**
   * The member read that takes `identifier` as its RECEIVER — `query` in
   * `query.data`, `envelope` in `envelope.list[0]` — or `undefined` when the
   * identifier names the value itself.
   *
   * A member CALL is not a projection: `res.blob()` yields a body rather than
   * a part of one, and what it returns stays the walk's business. The
   * zero-argument json and text body reads have their own branch and are
   * taken before this is asked.
   */
  private projectionOnReceiver(identifier: Node): Node | undefined {
    const access = identifier.getParent();
    if (
      !access ||
      (!Node.isPropertyAccessExpression(access) &&
        !Node.isElementAccessExpression(access)) ||
      access.getExpression() !== identifier
    ) {
      return undefined;
    }
    const parent = access.getParent();
    if (
      parent &&
      Node.isCallExpression(parent) &&
      parent.getExpression() === access
    ) {
      return undefined;
    }
    return access;
  }

  /**
   * The call that CONSUMES this json body read and states what the body is —
   * `parseEnvelope(await response.json())` — or `undefined` when nothing
   * downstream of the read says more about it than the read itself does
   * (carrick#1382).
   *
   * Three conditions, all shapes of the language rather than names:
   *
   *  - the read reaches the call as an ARGUMENT, through the wrappers that do
   *    not change a value (`await`, parentheses, `as`, `satisfies`, `!`). A
   *    cast with no call around it therefore keeps the read as the terminal,
   *    so `(await res.json()) as Entry` is still read off the read itself;
   *  - the call's own result is BOUND — declared into a variable, returned, or
   *    assigned — so a call the source made for its side effect
   *    (`store(await res.json())`) states nothing about the payload;
   *  - that result is an OBJECT shape. A validator answering `boolean` or a
   *    serialiser answering `string` describes what the caller did with the
   *    body, not what the body is, and publishing it would be a
   *    concrete-but-wrong contract where the honest `any` of the read is
   *    merely unresolved;
   *  - the call states its argument (`callStatesItsArgument`): a schema
   *    validates it, or the callee returns it unchanged. A converter's
   *    object result describes what it built, not the body (carrick#2595).
   */
  private statedPayloadAroundBodyRead(bodyRead: Node): Node | undefined {
    return this.receivingCallOf(
      bodyRead,
      (candidate) =>
        Node.isCallExpression(candidate) &&
        this.callResultIsBound(candidate) &&
        this.isObjectShape(this.unwrapPromiseType(candidate.getType())) &&
        this.callStatesItsArgument(
          candidate,
          candidate
            .getArguments()
            .findIndex((argument) => argument.getStart() <= bodyRead.getStart() && bodyRead.getEnd() <= argument.getEnd())
        )
    );
  }

  /**
   * The source keeps this call's result: it initializes a declaration, is
   * returned, is assigned, or is an arrow's expression body. A result that is
   * kept is one the source has a use for; a discarded one is a side effect.
   */
  private callResultIsBound(call: CallExpression): boolean {
    let current: Node = call;
    let parent = current.getParent();
    while (
      parent &&
      (Node.isParenthesizedExpression(parent) ||
        Node.isAwaitExpression(parent) ||
        Node.isAsExpression(parent) ||
        Node.isSatisfiesExpression(parent) ||
        Node.isNonNullExpression(parent)) &&
      parent.getExpression() === current
    ) {
      current = parent;
      parent = current.getParent();
    }
    if (!parent) return false;
    if (Node.isVariableDeclaration(parent)) {
      return parent.getInitializer() === current;
    }
    if (Node.isReturnStatement(parent)) {
      return parent.getExpression() === current;
    }
    if (Node.isArrowFunction(parent)) {
      return parent.getBody() === current;
    }
    if (Node.isBinaryExpression(parent)) {
      return (
        parent.getOperatorToken().getKind() === SyntaxKind.EqualsToken &&
        parent.getRight() === current
      );
    }
    return false;
  }

  /**
   * A shape a JSON body can be: an object, an array, or a union of them.
   * Top types, primitives, `void` and callables are not.
   */
  private isObjectShape(type: Type): boolean {
    if (type.isAny() || type.isUnknown()) return false;
    if (type.isUnion()) {
      const parts = type.getUnionTypes().filter((part) => !part.isUndefined() && !part.isNull());
      return parts.length > 0 && parts.every((part) => this.isObjectShape(part));
    }
    if (type.isIntersection()) {
      return type.getIntersectionTypes().every((part) => this.isObjectShape(part));
    }
    return type.isObject() && !this.isCallableType(type);
  }

  /**
   * Every use of a tracked name inside `expr` reads a member out of the
   * tracked value, so the expression's type describes a PART of the payload.
   * False when the expression uses no tracked name at all, so a caller can
   * read it as "this is a projection" rather than "this is not a use".
   */
  private usesNamesOnlyByProjection(expr: Node, names: string[]): boolean {
    const uses = expr
      .getDescendantsOfKind(SyntaxKind.Identifier)
      .filter((id) => this.isIdentifierUsage(id, names));
    if (uses.length === 0) {
      return false;
    }
    return uses.every((id) => this.projectionOnReceiver(id) !== undefined);
  }

  private extractBindingFromCall(callExpr: CallExpression): { names: string[]; node: Node } | null {
    const varDecl = callExpr.getFirstAncestorByKind(SyntaxKind.VariableDeclaration);
    if (varDecl) {
      const initializer = varDecl.getInitializer();
      if (initializer === callExpr || this.unwrapExpressionNode(initializer ?? callExpr) === callExpr) {
        const names = this.extractBindingNames(varDecl.getNameNode());
        const node = varDecl;
        return { names, node };
      }
    }

    const assignment = callExpr.getFirstAncestorByKind(SyntaxKind.BinaryExpression);
    if (assignment) {
      const right = assignment.getRight();
      if (
        right === callExpr ||
        this.unwrapExpressionNode(right) === callExpr
      ) {
        const left = assignment.getLeft();
        const names = Node.isIdentifier(left) ? [left.getText()] : [];
        const node = assignment;
        return { names, node };
      }
    }

    return null;
  }

  private extractBindingNames(nameNode: Node): string[] {
    if (Node.isIdentifier(nameNode)) {
      return [nameNode.getText()];
    }

    if (Node.isObjectBindingPattern(nameNode) || Node.isArrayBindingPattern(nameNode)) {
      const names: string[] = [];
      for (const element of nameNode.getElements()) {
        if (Node.isBindingElement(element)) {
          const elementName = element.getNameNode();
          names.push(...this.extractBindingNames(elementName));
        }
      }
      return names;
    }

    return [];
  }

  private getPrimaryBindingNode(nameNode: Node): Node {
    if (Node.isIdentifier(nameNode)) {
      return nameNode;
    }

    if (Node.isObjectBindingPattern(nameNode) || Node.isArrayBindingPattern(nameNode)) {
      const elements = nameNode.getElements();
      if (elements.length > 0 && Node.isBindingElement(elements[0])) {
        const elementName = elements[0].getNameNode();
        const found = this.getPrimaryBindingNode(elementName);
        return found;
      }
    }

    return nameNode;
  }

  /**
   * The zero-argument `.json()` body read on this identifier — `res` in
   * `await res.json()` — or `undefined` (carrick#1017). The JSON body of an
   * HTTP response is read exactly this way whatever produced the response, so
   * the shape is structural, not a framework's name.
   */
  /**
   * The body read inside a chain that starts at `callExpr` (carrick#1749):
   * `request(url).check(ok).mapOk(response => { const data = response as
   * SearchResponse; ... })`.
   *
   * The chain is the run of member calls whose receiver is the call, then
   * that call's result, and so on. A callback passed to one of them whose
   * first parameter the compiler types `unknown` is handed a value nothing
   * has typed yet, which is what a parsed body is; where the source casts
   * that parameter, the cast is what the caller says the body is. The value
   * the chain ends in is what the caller computed from it, and publishing
   * that as the body is the false mismatch this rule exists for.
   *
   * A call that narrows that parameter (a schema validates it, or a function
   * returns it unchanged) states it the same way (carrick#3004).
   *
   * Exactly one such read across the whole chain answers. None, or more than
   * one (a callback on the failure side can be handed an `unknown` too), and
   * `handedUnread` says a callback was handed the body all the same: what it
   * returns is a value built from the body (`.mapOk`, `.then`, `.andThen`,
   * whatever the name), so the value the chain ends in is never the body
   * (carrick#3004). A parameter typed `any` is not read: an unresolved
   * library types every callback that way, success and failure alike, so it
   * says nothing about which one is the body.
   */
  private bodyReadInCallbackChain(callExpr: CallExpression): {
    read?: Node;
    handedUnread: boolean;
  } {
    const reads: Node[] = [];
    let handedUnread = false;
    let receiver: Node = callExpr;
    for (let link = 0; link < 64; link++) {
      const access = receiver.getParent();
      if (
        !access ||
        !Node.isPropertyAccessExpression(access) ||
        access.getExpression() !== receiver
      ) {
        break;
      }
      const call = access.getParent();
      if (!call || !Node.isCallExpression(call) || call.getExpression() !== access) {
        break;
      }
      for (const arg of call.getArguments()) {
        if (Node.isArrowFunction(arg) || Node.isFunctionExpression(arg)) {
          const statements = this.statementsOfUnreadParameter(arg);
          if (statements) {
            handedUnread = true;
            reads.push(...statements);
          }
        }
      }
      receiver = call;
    }
    return reads.length === 1 ? { read: reads[0], handedUnread } : { handedUnread };
  }

  /**
   * `type`, or a type argument it is instantiated with at any depth, is
   * `any` or `unknown`: `Chain<unknown>`, `Task<Outcome<unknown, E>>`. The
   * value such a carrier hands its callbacks is untyped (carrick#3004).
   */
  private holdsTopTypeArgument(type: Type, depth = 0): boolean {
    if (type.isAny() || type.isUnknown()) return true;
    if (depth >= 6) return false;
    const args = [...type.getTypeArguments(), ...type.getAliasTypeArguments()];
    return args.some((arg) => this.holdsTopTypeArgument(arg, depth + 1));
  }

  /**
   * What a callback states about its first parameter, when the compiler types
   * that parameter `unknown`, or `undefined` when it is not handed one. A
   * statement is a cast whose operand is the parameter itself (`response as
   * T`, `<T>response`), or a call that narrows it (`callStatesItsArgument`:
   * a schema validates it, or a function returns it unchanged). A cast that
   * leaves a position open states nothing about the body and is skipped.
   */
  private statementsOfUnreadParameter(
    callback: ArrowFunction | FunctionExpression
  ): Node[] | undefined {
    const param = callback.getParameters()[0];
    const name = param?.getNameNode();
    if (!param || !name || !Node.isIdentifier(name) || !param.getType().isUnknown()) {
      return undefined;
    }
    const symbol = name.getSymbol();
    if (!symbol) return [];
    const isParameter = (node: Node): boolean => {
      let operand: Node = node;
      while (Node.isParenthesizedExpression(operand)) {
        operand = operand.getExpression();
      }
      return Node.isIdentifier(operand) && operand.getSymbol() === symbol;
    };
    return callback.getDescendants().filter((node) => {
      if (Node.isCallExpression(node)) {
        const index = node.getArguments().findIndex(isParameter);
        return index >= 0 && this.callStatesItsArgument(node, index);
      }
      if (!Node.isAsExpression(node) && !Node.isTypeAssertion(node)) return false;
      if (!isParameter(node.getExpression())) return false;
      const stated = node.getType();
      const statedNode = node.getTypeNode();
      return (
        !!statedNode &&
        !stated.isAny() &&
        !stated.isUnknown() &&
        !this.leavesAPositionOpen(statedNode)
      );
    });
  }

  /**
   * The zero-argument whole-body read taken in place on the value `callExpr`
   * yields, or `undefined` (carrick#1851): `(await fetch(url)).text()`. The
   * receiver is the call itself, through the wrappers that leave a value as
   * it is (parentheses, `await`, `!`), so it is the same read
   * `bodyReadOnReceiver` finds on a binding of that value. A call that is not
   * awaited first is read the same way: a request that is a promise and reads
   * its own body (`send(url).json()`) yields the body from that read too.
   */
  private bodyReadOnCallValue(callExpr: CallExpression): Node | undefined {
    let value: Node = callExpr;
    for (;;) {
      const parent = value.getParent();
      if (
        !parent ||
        !(
          Node.isParenthesizedExpression(parent) ||
          Node.isAwaitExpression(parent) ||
          Node.isNonNullExpression(parent)
        ) ||
        parent.getExpression() !== value
      ) {
        break;
      }
      value = parent;
    }
    return this.bodyReadOnReceiver(value);
  }

  /**
   * The zero-argument whole-body read that takes `receiver` as its receiver,
   * `res.json()` or `res.text()`, or `undefined`. A text read is a body read
   * like a json one (carrick#1842): without it, `return res.text()` left the
   * walk on the response binding and published the transport object. `reads`
   * names the reads asked for.
   */
  private bodyReadOnReceiver(
    receiver: Node,
    reads: ReadonlySet<string> = WHOLE_BODY_READS
  ): Node | undefined {
    const access = receiver.getParent();
    if (
      !access ||
      !Node.isPropertyAccessExpression(access) ||
      access.getExpression() !== receiver ||
      !reads.has(access.getName())
    ) {
      return undefined;
    }
    const call = access.getParent();
    if (
      !call ||
      !Node.isCallExpression(call) ||
      call.getExpression() !== access ||
      call.getArguments().length > 0
    ) {
      return undefined;
    }
    return call;
  }

  private collectDefUseNodes(func: FunctionLike): Node[] {
    const candidates: Node[] = [];
    func.forEachDescendant((node) => {
      if (
        Node.isIdentifier(node) ||
        Node.isVariableDeclaration(node) ||
        Node.isBinaryExpression(node)
      ) {
        candidates.push(node);
      }
    });
    return candidates;
  }

  private expressionUsesNames(expr: Node, names: string[]): boolean {
    const identifiers = expr.getDescendantsOfKind(SyntaxKind.Identifier);
    return identifiers.some((id) => this.isIdentifierUsage(id, names));
  }

  /** `expr` is a use of a tracked name, or contains one. */
  private usesTrackedNames(expr: Node, names: string[]): boolean {
    return Node.isIdentifier(expr)
      ? this.isIdentifierUsage(expr, names)
      : this.expressionUsesNames(expr, names);
  }

  private isIdentifierUsage(id: Node, names: string[]): boolean {
    if (!Node.isIdentifier(id)) {
      return false;
    }

    const text = id.getText();
    if (!names.includes(text)) {
      return false;
    }

    const parent = id.getParent();
    if (
      parent &&
      Node.isVariableDeclaration(parent) &&
      parent.getNameNode() === id
    ) {
      return false;
    }
    if (
      parent &&
      Node.isBindingElement(parent) &&
      parent.getNameNode() === id
    ) {
      return false;
    }

    return true;
  }

  private isInFunctionScope(node: Node, func: FunctionLike): boolean {
    const ancestor = node.getFirstAncestor((candidate) =>
      Node.isFunctionDeclaration(candidate) ||
      Node.isArrowFunction(candidate) ||
      Node.isFunctionExpression(candidate) ||
      Node.isMethodDeclaration(candidate)
    );

    return ancestor === func;
  }

  // ===========================================================================
  // Extraction Config-based Payload Unwrapping (NEW)
  // ===========================================================================

  /**
   * Resolve the awaited type: `Promise<X>` / `PromiseLike<X>` / `Awaited<X>` →
   * `X` at the type level (recursively, so `Awaited<Promise<X>>` → `X`); any
   * non-thenable type passes through UNCHANGED.
   *
   * ts-morph 25.0.1 does not expose the compiler's `getAwaitedType` on its
   * `Type` wrapper (only `AwaitableNode.isAwaited()` on AST nodes, which is
   * unrelated), so we resolve structurally on the symbol/alias name instead of
   * gating on the literal `'Promise'` symbol. This generalizes past `Promise<T>`
   * to `PromiseLike<T>` and the `Awaited<T>` utility type without over-unwrapping:
   * a non-thenable like `AsyncGenerator<T>` is NOT awaitable and is returned
   * unchanged, so the `unwrapAsyncIterableType` step that runs right after still
   * sees (and peels) the iterator wrapper as before.
   */
  private unwrapPromiseType(type: Type): Type {
    // Bounded recursion guard: a pathological self-referential alias must not
    // loop. Real promise nesting is shallow; 16 is far more than enough.
    let current = type;
    for (let depth = 0; depth < 16; depth++) {
      const symbolName = current.getSymbol()?.getName();
      const aliasName = current.getAliasSymbol()?.getName();

      // `Promise<X>` / `PromiseLike<X>` — single type argument is the value.
      if (symbolName === 'Promise' || symbolName === 'PromiseLike') {
        const args = current.getTypeArguments();
        if (args.length === 1) {
          current = args[0];
          continue;
        }
        return current;
      }

      // `Awaited<X>` utility type — unwrap its alias argument and keep resolving
      // (TS usually eager-resolves this, but handle the surfaced-alias form too).
      if (aliasName === 'Awaited') {
        const aliasArgs = current.getAliasTypeArguments();
        if (aliasArgs.length === 1) {
          current = aliasArgs[0];
          continue;
        }
        return current;
      }

      return current;
    }
    return current;
  }

  /**
   * `AsyncGenerator<T, …>` / `AsyncIterableIterator<T>` / `AsyncIterator<T>` /
   * `Generator<T, …>` / `IterableIterator<T>` → `T` (the yield type) at the type
   * level; any other type passes through. A GraphQL subscription resolver written
   * as `async function* x(): AsyncGenerator<Order>` carries its contract in the
   * yield position, so the iterator wrapper must be peeled the same way Promise is
   * before structural expansion — otherwise the response contract resolves to the
   * library `AsyncGenerator<…>` machinery instead of the bare `Order`.
   */
  private unwrapAsyncIterableType(type: Type): Type {
    const symbolName = (type.getSymbol() || type.getAliasSymbol())?.getName();
    if (symbolName && ASYNC_ITERABLE_SYMBOLS.has(symbolName)) {
      const args = type.getTypeArguments();
      if (args.length >= 1) {
        return args[0];
      }
    }
    return type;
  }

  /** The "leave the type as it is" result every bail-out path shares. */
  private noUnwrap(type: Type, node: Node): UnwrapResult {
    return {
      typeString: typeText(type, node),
      isExplicit: false,
      wasUnwrapped: false,
    };
  }

  /**
   * Unwrap a type using the agent-generated ExtractionConfig.
   */
  private unwrapTypeWithConfig(
    type: Type,
    node: Node,
    extractionConfig?: ExtractionConfig
  ): UnwrapResult {
    if (!extractionConfig || extractionConfig.rules.length === 0) {
      return this.noUnwrap(type, node);
    }

    return this.unwrapType(type, node, extractionConfig, 0);
  }

  /**
   * Core unwrapping implementation with ExtractionConfig rules.
   *
   * Requirements:
   * 1. Exact wrapperSymbols match extracts (gated on originModuleGlobs when
   *    the rule carries them — names like `Response` are shared by the DOM,
   *    frameworks, and HTTP clients)
   * 2. machineryIndicators only trigger unwrap if originModuleGlobs also match
   * 3. Handle unions and intersections
   * 4. Support recursive unwrapping with depth limits
   * 5. A rule that matches but extracts nothing never blocks later rules;
   *    only after every rule has run does an origin-verified match with no
   *    recoverable payload collapse to `unknown`
   */
  private unwrapType(
    type: Type,
    node: Node,
    config: ExtractionConfig,
    depth: number
  ): UnwrapResult {
    const maxGlobalDepth = 10; // Safety limit
    if (depth >= maxGlobalDepth) {
      return this.noUnwrap(type, node);
    }

    // Handle union types: Response<A> | Response<B> → unwrap to A | B
    if (type.isUnion()) {
      const unionTypes = type.getUnionTypes();
      const unwrappedParts: string[] = [];
      /** Branches that read as `unknown`: verified machinery, no payload. */
      const unreadBranches: string[] = [];
      let anyUnwrapped = false;

      for (const unionType of unionTypes) {
        const result = this.unwrapType(unionType, node, config, depth + 1);
        unwrappedParts.push(result.typeString);
        if (result.wasUnwrapped) {
          anyUnwrapped = true;
        }
        if (result.typeString.trim() === 'unknown') {
          unreadBranches.push(this.pathFreeTypeLabel(unionType));
        }
      }

      if (anyUnwrapped) {
        const unique = [...new Set(unwrappedParts)];
        const informative = unique.filter((part) => part !== 'unknown');

        // Every branch collapsed, so the union as a whole recovered no
        // payload — the same standing as a single verified-machinery
        // collapse, and the caller's own recovery should still get a turn.
        if (informative.length === 0) {
          return {
            typeString: 'unknown',
            isExplicit: false,
            wasUnwrapped: true,
            verifiedMachinery: true,
          };
        }

        // carrick#166: SOME branches read and some did not. Joining the ones
        // that did states a partial contract as the whole one — on the live
        // repro, one guard branch's status envelope stood in for a handler
        // that also returns the real payload — and a contract known to be
        // partial is a false match, which is worse than the `any` it
        // replaced. Answer `unknown` and carry the reason instead. This is
        // NOT the verified-machinery verdict: the union did resolve
        // something, so the return-statement recovery is not the right next
        // move, and reading a payload out of the unread branches is its own
        // work.
        if (unreadBranches.length > 0) {
          return {
            typeString: 'unknown',
            isExplicit: false,
            wasUnwrapped: true,
            unreadUnionBranches: unreadBranches,
          };
        }

        return {
          typeString:
            informative.length === 1 ? informative[0] : informative.join(' | '),
          isExplicit: false,
          wasUnwrapped: true,
        };
      }
    }

    // Handle intersection types: Response<A> & X → try to unwrap Response<A>
    if (type.isIntersection()) {
      const intersectionTypes = type.getIntersectionTypes();
      for (const intersectType of intersectionTypes) {
        const result = this.unwrapType(intersectType, node, config, depth + 1);
        if (result.wasUnwrapped) {
          return result;
        }
      }
    }

    // Try each rule. A rule that verifies the wrapper's identity but cannot
    // recover a payload must not stop the loop — the model is encouraged to
    // emit several overlapping rules (e.g. a generic-index variant and a
    // property-path variant for the same wrapper), and a later one may still
    // extract. Only when every rule has had its chance does a verified match
    // collapse to `unknown`: the wrapper itself is never the contract, and
    // downstream treats `unknown` as unresolved instead of comparing it.
    let verifiedMachinery = false;
    for (const rule of config.rules) {
      const attempt = this.tryUnwrapWithRule(type, node, rule, config, depth);
      if (attempt.kind === 'extracted') {
        return attempt.result;
      }
      if (attempt.kind === 'verified-no-payload') {
        verifiedMachinery = true;
      }
    }

    if (verifiedMachinery) {
      return {
        typeString: 'unknown',
        isExplicit: false,
        wasUnwrapped: true,
        verifiedMachinery: true,
      };
    }

    return this.noUnwrap(type, node);
  }

  /**
   * Try to unwrap a type using a single ExtractionRule.
   */
  private tryUnwrapWithRule(
    type: Type,
    node: Node,
    rule: ExtractionRule,
    config: ExtractionConfig,
    depth: number
  ): RuleAttempt {
    const maxDepth = rule.maxDepth ?? 4;
    if (depth >= maxDepth) {
      return { kind: 'no-match' };
    }

    const readings = this.wrapperReadings(type);

    // 1. Check exact wrapperSymbols match, against either name the type goes
    // by (carrick#1843). When the rule also carries originModuleGlobs, the
    // declaration of the name that matched must come from a matching module
    // — names like `Response` are shared by the DOM, frameworks, and HTTP
    // clients, so a bare name match would unwrap unrelated types. A name
    // that fails that gate is no match, and the rule's test of members and
    // origin below still has its turn: a service's alias that shares the
    // rule's name can stand around the very type the rule verifies.
    const named = readings.filter((reading) =>
      rule.wrapperSymbols?.includes(reading.symbol.getName())
    );
    const globs = rule.originModuleGlobs ?? [];
    const originGated = globs.length > 0;
    const matched = originGated
      ? named.find((reading) => this.symbolOriginatesFromModules(reading.symbol, globs))
      : named[0];
    if (matched) {
      const extracted = this.extractPayloadFromWrapper(type, matched, node, rule, config, depth);
      if (extracted) {
        return { kind: 'extracted', result: extracted };
      }
      // A name-only match is not proof of machinery: a local type that
      // happens to share the name must keep its real structural type when
      // nothing was extracted. Only origin-verified matches may collapse
      // to `unknown`.
      return originGated ? { kind: 'verified-no-payload' } : { kind: 'no-match' };
    }

    // 2. Check machineryIndicators + originModuleGlobs. Indicators alone are
    // too many false positives, so the origin gate is mandatory here — which
    // also means a match in this branch is always origin-verified.
    if (rule.machineryIndicators && rule.machineryIndicators.length > 0) {
      if (!rule.originModuleGlobs || rule.originModuleGlobs.length === 0) {
        return { kind: 'no-match' };
      }

      if (!this.typeHasMachineryIndicators(type, rule.machineryIndicators)) {
        return { kind: 'no-match' };
      }

      // The rule named neither of the type's names, so this branch reads
      // what it always read: the first symbol the type has, and the type's
      // own arguments. An alias the rule did not name says nothing about
      // where a payload is, whoever declares it (carrick#1843):
      // `Omit<Response, 'json'>` is written through a library alias whose
      // first argument is the transport itself, and a service's own alias
      // around a library's object orders its parameters as it likes.
      const symbol = type.getSymbol() || type.getAliasSymbol();
      if (!symbol || !this.symbolOriginatesFromModules(symbol, rule.originModuleGlobs)) {
        return { kind: 'no-match' };
      }

      const extracted = this.extractPayloadFromWrapper(
        type,
        { symbol, typeArguments: type.getTypeArguments(), viaAlias: false },
        node,
        rule,
        config,
        depth
      );
      if (extracted) {
        return { kind: 'extracted', result: extracted };
      }
      return { kind: 'verified-no-payload' };
    }

    return { kind: 'no-match' };
  }

  /**
   * The names `type` goes by, own symbol first (carrick#1843).
   *
   * `type Task<A> = __Task<A>` has the class's symbol and arguments, and the
   * alias's beside them. `type Reply<T> = { ... }` has the anonymous `__type`
   * with no arguments of its own. `type Outcome<A, E> = Done<A, E> |
   * Failed<A, E>` has no symbol of its own at all. In each, the alias and its
   * arguments are what a rule naming `Task`, `Reply` or `Outcome` describes.
   */
  private wrapperReadings(type: Type): WrapperReading[] {
    const readings: WrapperReading[] = [];
    const own = type.getSymbol();
    if (own) {
      readings.push({ symbol: own, typeArguments: type.getTypeArguments(), viaAlias: false });
    }
    const alias = type.getAliasSymbol();
    if (alias && alias !== own) {
      readings.push({
        symbol: alias,
        typeArguments: type.getAliasTypeArguments(),
        viaAlias: true,
      });
    }
    return readings;
  }

  /**
   * The type of `type`'s member `name` at `node`, or `undefined` when the type
   * has no such member. The one property read the payload-property rules and
   * the library store's result label (carrick#2818) share.
   */
  private readMemberType(type: Type, name: string, node: Node): Type | undefined {
    return type.getProperty(name)?.getTypeAtLocation(node);
  }

  /**
   * Extract the payload type from a matched wrapper. Returns null when the
   * rule matched the wrapper but no payload is recoverable from generics or
   * property paths — the caller decides what a payload-less match means
   * (verified machinery collapses to `unknown` after every rule has run;
   * a name-only match leaves the type untouched).
   *
   * `payloadGenericIndex` counts the arguments of `reading`, the name the
   * rule matched (carrick#1843): an alias is free to order its parameters
   * differently from the type it stands for, so the same index into the
   * other list is a different argument.
   */
  private extractPayloadFromWrapper(
    type: Type,
    reading: WrapperReading,
    node: Node,
    rule: ExtractionRule,
    config: ExtractionConfig,
    depth: number
  ): UnwrapResult | null {
    // The outer extraction already succeeded on the paths below; a recursive
    // inner pass that finds nothing more must not demote the result back to
    // "not unwrapped" (which would discard the recovered payload). Only when
    // the inner pass unwrapped NOTHING is this level's payload the recovered
    // type; an inner pass that did unwrap already decided its own payloadType,
    // and its absence is deliberate — a union join has no single payload, and
    // a verified-machinery collapse to `unknown` recovered none, so this
    // level's (wrapper-shaped) payload must not resurface as an anchor.
    const recurse = (payload: Type): UnwrapResult => {
      const inner = this.unwrapType(payload, node, config, depth + 1);
      return {
        ...inner,
        wasUnwrapped: true,
        payloadType: inner.wasUnwrapped ? inner.payloadType : payload,
      };
    };

    // 1. Try generic type argument at payloadGenericIndex
    const genericIndex = rule.payloadGenericIndex ?? 0;
    const typeArgs = reading.typeArguments;

    if (typeArgs.length > genericIndex) {
      const payloadArg = typeArgs[genericIndex];

      // Check if it's a useful type (not any/unknown/never)
      const argText = typeText(payloadArg, node);
      if (!this.isUselessType(argText)) {
        // Recursive unwrap if configured
        if (rule.unwrapRecursively) {
          return recurse(payloadArg);
        }
        return {
          typeString: argText,
          isExplicit: true,
          wasUnwrapped: true,
          payloadType: payloadArg,
        };
      }

      // Try "first useful generic" heuristic. It is a guess about a type's
      // own arguments and is not extended to an alias's (carrick#1843): the
      // alias this reaches most often is a result carrier, `Outcome<A, E>`,
      // whose next argument after an open payload is the error side.
      for (let i = 0; !reading.viaAlias && i < typeArgs.length; i++) {
        const argType = typeArgs[i];
        const text = typeText(argType, node);
        if (!this.isUselessType(text)) {
          if (rule.unwrapRecursively) {
            return recurse(argType);
          }
          return {
            typeString: text,
            isExplicit: true,
            wasUnwrapped: true,
            payloadType: argType,
          };
        }
      }
    }

    // 2. Try payloadPropertyPath
    if (rule.payloadPropertyPath && rule.payloadPropertyPath.length > 0) {
      let currentType = type;

      for (const propName of rule.payloadPropertyPath) {
        const propType = this.readMemberType(currentType, propName, node);
        if (!propType) {
          break;
        }
        currentType = propType;
      }

      if (currentType !== type) {
        const propText = typeText(currentType, node);
        if (!this.isUselessType(propText)) {
          if (rule.unwrapRecursively) {
            return recurse(currentType);
          }
          return {
            typeString: propText,
            isExplicit: false,
            wasUnwrapped: true,
            payloadType: currentType,
          };
        }
      }
    }

    return null;
  }

  /**
   * True when a producer RESPONSE type IS or CONTAINS framework transport
   * machinery (a fetch/DOM `Response`, a Node `ServerResponse`, a reply object)
   * — the artifact behind carrick#371, where a wrapped handler's literal return
   * envelope `{ response: Response; error?: undefined } | { ...; error: Error }`
   * was captured as the response contract. Machinery is never a comparable
   * contract, so a response path that resolves here abstains (honest `unknown`)
   * rather than emit a concrete-but-false type.
   *
   * DETECTS, exactly (see `typeIsOrContainsMachinery`):
   *   1. the type itself is machinery (`typeIsFrameworkMachinery`);
   *   2. a union/intersection member is machinery (the envelope union);
   *   3. a DIRECT property's type is machinery, ONE level of descent only
   *      (`{ response: Response; error }`).
   *
   * DELIBERATELY NOT DETECTED — listed so this comment never overstates the
   * guarantee (an overstated safety comment is what bit sibling PR #442). Each is
   * a non-regression: it leaves the pre-existing verdict unchanged and never
   * manufactures a new wrong one, tracked as a follow-up:
   *   - machinery nested deeper than one property level (a property whose type
   *     is itself a nested object wrapping the machinery);
   *   - a property typed `Promise<Response>` (the property type is NOT awaited /
   *     Promise-unwrapped before the machinery check);
   *   - an array element type: `Response[]` is not descended to its element;
   *   - `interface X extends Response` declared in USER source — the origin gate
   *     is lib/installed-dependency only, so a user-declared subtype reads as a real
   *     contract, not machinery;
   *   - a function / call-signature return type: the response paths that call
   *     this resolve a handler's RETURN (an envelope/object), never a function
   *     value, so no call-signature descent happens here. The capture-seam
   *     mirror `capture/machinery.ts` DOES descend call signatures — that is
   *     where the wrapper-FUNCTION type the Infer fallback resolves is caught.
   *
   * The origin gate in `typeIsFrameworkMachinery` keeps a user object whose
   * fields merely share a member name from tripping.
   */
  private typeIsOrContainsResponseMachinery(type: Type): boolean {
    return this.typeIsOrContainsMachinery(type, 0);
  }

  private typeIsOrContainsMachinery(type: Type, depth: number): boolean {
    if (this.typeIsFrameworkMachinery(type)) {
      return true;
    }
    // Union/intersection: any envelope member wrapping machinery taints it.
    if (type.isUnion()) {
      return type
        .getUnionTypes()
        .some((part) => this.typeIsOrContainsMachinery(part, depth));
    }
    if (type.isIntersection()) {
      return type
        .getIntersectionTypes()
        .some((part) => this.typeIsOrContainsMachinery(part, depth));
    }
    // One level of property descent: `{ response: Response; error?: undefined }`.
    if (depth < 1) {
      for (const prop of type.getProperties()) {
        const decl = prop.getDeclarations()[0];
        if (!decl) continue;
        const propType = prop.getTypeAtLocation(decl);
        if (this.typeIsOrContainsMachinery(propType, depth + 1)) {
          return true;
        }
      }
    }
    return false;
  }

  /**
   * True when `type` itself is an HTTP-machinery type: it structurally carries
   * at least `MACHINERY_INDICATOR_THRESHOLD` of the strongly-discriminating
   * `MACHINERY_MEMBER_INDICATORS`, AND its symbol is declared in a lib
   * (`lib.dom.d.ts`, ...) or installed-dependency origin. Both gates are required —
   * the indicator subset alone essentially never matches a JSON payload, and
   * the origin gate makes certain a user's own local type sharing those member
   * names is never mistaken for framework machinery (the advisor's guard).
   */
  private typeIsFrameworkMachinery(type: Type): boolean {
    if (!this.hasMachineryIndicatorThreshold(type)) {
      return false;
    }
    const symbol = type.getSymbol() ?? type.getAliasSymbol();
    return this.symbolIsLibOrExternalOrigin(symbol);
  }

  /**
   * True once the type carries at least `MACHINERY_INDICATOR_THRESHOLD` DISTINCT
   * `MACHINERY_MEMBER_INDICATORS` (own + apparent). Deduplicates by name (own and
   * apparent property lists overlap) and early-returns the moment the threshold
   * is reached — the callers only need the boolean, never the full count.
   */
  private hasMachineryIndicatorThreshold(type: Type): boolean {
    const matched = new Set<string>();
    const consider = (name: string): boolean => {
      if (MACHINERY_MEMBER_INDICATORS.has(name)) {
        matched.add(name);
      }
      return matched.size >= MACHINERY_INDICATOR_THRESHOLD;
    };
    for (const prop of type.getProperties()) {
      if (consider(prop.getName())) return true;
    }
    for (const prop of type.getApparentProperties()) {
      if (consider(prop.getName())) return true;
    }
    return false;
  }

  /**
   * True when the symbol is declared in a runtime/library origin
   * (`isExternalOrigin`): a TypeScript lib, an installed dependency however
   * the program resolved it, or the runtime declarations Carrick materialises
   * under `.carrick/deno/` — framework/runtime machinery, not user source.
   * Works on a bare checkout: the DOM `Response`/`Request` resolve from the
   * bundled `lib.dom.d.ts` even with no installed dependencies. The program is
   * read per call: the project adds source files lazily and rebuilds it.
   */
  private symbolIsLibOrExternalOrigin(symbol: TsSymbol | undefined): boolean {
    if (!symbol) {
      return false;
    }
    const program = this.project.getProgram().compilerObject;
    for (const decl of symbol.getDeclarations()) {
      if (isExternalOrigin(program, decl.getSourceFile().compilerNode, this.repoRoot, externalImportsOf(this.project))) {
        return true;
      }
    }
    try {
      const aliased = symbol.getAliasedSymbol?.();
      if (aliased && aliased !== symbol) {
        return this.symbolIsLibOrExternalOrigin(aliased);
      }
    } catch {
      // Ignore errors when resolving the aliased symbol.
    }
    return false;
  }

  /**
   * Check if a type has machinery indicator properties/methods.
   */
  private typeHasMachineryIndicators(type: Type, indicators: string[]): boolean {
    const properties = type.getProperties();
    const propertyNames = properties.map((p) => p.getName());

    for (const indicator of indicators) {
      if (propertyNames.includes(indicator)) {
        return true;
      }
    }

    // Also check apparent properties (for interfaces, etc.)
    const apparentProperties = type.getApparentProperties();
    const apparentNames = apparentProperties.map((p) => p.getName());

    for (const indicator of indicators) {
      if (apparentNames.includes(indicator)) {
        return true;
      }
    }

    return false;
  }

  /**
   * Check if a symbol's declarations originate from modules matching the globs.
   */
  private symbolOriginatesFromModules(symbol: TsSymbol | undefined, moduleGlobs: string[]): boolean {
    if (!symbol) {
      return false;
    }

    const declarations = symbol.getDeclarations();
    for (const decl of declarations) {
      const sourceFile = decl.getSourceFile();
      const filePath = sourceFile.getFilePath();

      for (const glob of moduleGlobs) {
        if (this.filePathMatchesModuleGlob(filePath, glob)) {
          return true;
        }
      }
    }

    // Also check aliased symbol
    try {
      const aliased = symbol.getAliasedSymbol?.();
      if (aliased && aliased !== symbol) {
        return this.symbolOriginatesFromModules(aliased, moduleGlobs);
      }
    } catch {
      // Ignore errors when getting aliased symbol
    }

    return false;
  }

  /**
   * Simple glob matching for module paths.
   * Supports: exact match, a trailing "*" wildcard, and "package/*" patterns.
   *
   * Matches are segment-bounded: the glob names a package (or package
   * subpath) under node_modules, and the match must end at a path-segment
   * boundary — `got` matches `node_modules/got/...` but never
   * `node_modules/got-scraping/...`. This matters because the exact-symbol
   * origin gate routes shared names like `Response` through here.
   */
  private filePathMatchesModuleGlob(filePath: string, glob: string): boolean {
    const normalizedPath = filePath.replace(/\\/g, '/');

    const candidates = [glob];
    if (!glob.startsWith('@types/')) {
      // Auto-try the DefinitelyTyped variant: pkg → @types/pkg,
      // @scope/pkg → @types/scope__pkg.
      candidates.push(
        glob.startsWith('@')
          ? `@types/${glob.slice(1).replace('/', '__')}`
          : `@types/${glob}`
      );
    }

    return candidates.some((candidate) => {
      const base = candidate.replace(/\/?\*+$/, '').replace(/\*/g, '');
      if (base === '') {
        return false;
      }
      const needle = `node_modules/${base}`;
      let idx = normalizedPath.indexOf(needle);
      while (idx !== -1) {
        const next = normalizedPath[idx + needle.length];
        if (next === undefined || next === '/') {
          return true;
        }
        idx = normalizedPath.indexOf(needle, idx + 1);
      }
      return false;
    });
  }

  /**
   * Check if a type string is "useless" for payload purposes.
   */
  private isUselessType(typeString: string): boolean {
    const useless = ['any', 'unknown', 'never', 'void', 'undefined', 'null', 'object', '{}'];
    const trimmed = typeString.trim();
    return useless.includes(trimmed) || trimmed === '';
  }

  /**
   * A type that can be CALLED or constructed is machinery, never a payload
   * (carrick#964).
   *
   * A context-object framework hands the handler one object that both reads the
   * request and sends the response, so that object has a `body` MEMBER whose
   * type is the response sender. Every anchor that reads a member off the
   * handler's parameter, and every locator that lands on such a member, can
   * therefore resolve to a perfectly concrete type that is the wrong side of
   * the exchange — and, being concrete, it sails past the useless-type guard and
   * publishes as an explicit contract.
   *
   * Nothing crosses an HTTP (or queue, or topic) boundary as a function, so the
   * presence of call or construct signatures is a structural, framework-free
   * proof that the resolution landed on machinery. The caller abstains, which
   * leaves the row honestly unresolved instead of confidently wrong.
   */
  private isCallableType(type: Type): boolean {
    try {
      return (
        type.getCallSignatures().length > 0 ||
        type.getConstructSignatures().length > 0
      );
    } catch {
      return false;
    }
  }

  private unwrapExpressionNode(node: Node | undefined): Node {
    let current = node;
    while (current) {
      if (Node.isParenthesizedExpression(current)) {
        current = current.getExpression();
        continue;
      }
      if (Node.isAwaitExpression(current)) {
        current = current.getExpression();
        continue;
      }
      if (Node.isAsExpression(current)) {
        current = current.getExpression();
        continue;
      }
      if (Node.isNonNullExpression(current)) {
        current = current.getExpression();
        continue;
      }
      break;
    }
    return current ?? node!;
  }

  /**
   * If `node` is a `JSON.stringify(arg)` call, return the (expression-unwrapped)
   * first argument so its type is read instead of the call's `string` result.
   * Otherwise return `node` unchanged. Any non-`JSON.stringify` call, or a
   * `JSON.stringify()` with no argument, is left alone.
   */
  private unwrapJsonStringifyArg(node: Node): Node {
    if (!Node.isCallExpression(node)) {
      return node;
    }
    const callee = node.getExpression();
    if (!Node.isPropertyAccessExpression(callee)) {
      return node;
    }
    const obj = callee.getExpression();
    if (
      !Node.isIdentifier(obj) ||
      obj.getText() !== "JSON" ||
      callee.getName() !== "stringify"
    ) {
      return node;
    }
    const args = node.getArguments();
    if (args.length === 0) {
      return node;
    }
    return this.unwrapExpressionNode(args[0]);
  }

  private extractExplicitTypeFromAncestor(node: Node): string | null {
    const typeNode = this.explicitTypeNodeFromAncestor(node);
    return typeNode ? this.expandAnnotationTypeNode(typeNode) : null;
  }

  /** The annotation `extractExplicitTypeFromAncestor` prints, as a node. */
  private explicitTypeNodeFromAncestor(node: Node): Node | undefined {
    const varDecl = node.getFirstAncestorByKind(SyntaxKind.VariableDeclaration);
    if (varDecl) {
      const typeNode = varDecl.getTypeNode();
      if (typeNode) {
        return typeNode;
      }
    }

    // Consider the node ITSELF as well as its ancestors: the `call_result`
    // path's terminal node for `return res.json() as Promise<T>` IS the
    // `as` cast (an ancestor walk from it would miss it), so the #257 consumer
    // shape would never be recovered. `as T` and `<T>x` assertions both apply.
    const asExpr = Node.isAsExpression(node)
      ? node
      : node.getFirstAncestorByKind(SyntaxKind.AsExpression);
    const asType = statedAssertionType(asExpr?.getTypeNode());
    if (asType) {
      return asType;
    }

    const typeAssertion = Node.isTypeAssertion(node)
      ? node
      : node.getFirstAncestorByKind(SyntaxKind.TypeAssertionExpression);
    return statedAssertionType(typeAssertion?.getTypeNode());
  }

  /**
   * Render an explicit annotation (`as T`, `<T>`, or a typed binding) as
   * fully-structural text.
   *
   * `typeNode.getText()` keeps a named type as its bare identifier
   * (`OrderView`, `Promise<Payment>`). A bare name is fine inside the source
   * project but becomes a dangling reference in the cross-repo `.d.ts` bundle,
   * which carries only alias lines and no source declarations — it resolves to
   * `any` and the comparison reads `unverifiable`. Resolving the annotation to
   * its `Type`, stripping `Promise<…>` at the type level, and expanding the
   * object structurally (shared with `definition-resolver.ts`) lands the real
   * shape (`{ id: string; currency: string }`) in the bundle so the consumer
   * can actually be compared.
   *
   * Falls back to the bare annotation text when the resolved type can't be
   * expanded to a structural form (primitives, library types, unresolvable
   * references), so a non-object annotation behaves exactly as before.
   */
  private expandAnnotationTypeNode(
    typeNode: Node,
    wire: WireFormat = 'declared'
  ): string {
    const fallback = typeNode.getText();
    try {
      const annotationType = this.unwrapPromiseType(typeNode.getType());
      const expanded = expandTypeStructural(annotationType, this.expandOrigin(), { wire });
      // Only prefer the structural form when expansion actually inlined an
      // object shape; otherwise keep the annotation text (e.g. a bare
      // primitive or a library type the expander leaves by name). A wire
      // print that differs from the declared one is a real answer too: a
      // `Date` annotation sends a string (carrick#1163).
      if (expanded.startsWith('{')) return expanded;
      return wire === 'json' && expanded !== expandTypeStructural(annotationType, this.expandOrigin())
        ? expanded
        : fallback;
    } catch {
      return fallback;
    }
  }

  /**
   * Producer-side analogue of `expandAnnotationTypeNode` that works from a
   * resolved `Type` rather than a syntactic annotation node.
   *
   * `inferResponseBody`/`inferFunctionReturn` resolve a payload to a named
   * object type (e.g. `Payment`), then render it with `typeText`, which keeps
   * the bare name. In the source-less cross-repo `.d.ts` bundle that name is a
   * dangling `export type <alias> = Payment;` → resolves to `any` →
   * `unverifiable` → `compat = None`. Expanding the resolved object structurally
   * lands the real members (`{ id: string; … }`) in the bundle so the producer
   * can be compared. Mirror of #257's consumer-side fix; keeps the bare text for
   * primitives, library types, and anything the expander leaves by name.
   *
   * `fallback` is the already-computed type text (post Promise/wrapper unwrap),
   * preserved verbatim when expansion does not inline an object.
   */
  private expandResolvedTypeStructural(
    type: Type,
    fallback: string,
    wire: WireFormat = 'declared'
  ): string {
    try {
      // carrick#2116: a narrowed `unknown` or `object`, at the root or as a
      // member of the root union, states no shape. It would print `{}` or
      // `object` and be judged as a body that lacks every field.
      if (statesNoShape(this.project.getTypeChecker().compilerObject, type.compilerType)) {
        return 'unknown';
      }
      const expanded = expandTypeStructural(type, this.expandOrigin(), { wire });
      // A wire print that differs from the declared one is the answer even
      // without an inlined object: a bare `Date` payload sends a string.
      if (
        wire === 'json' &&
        !expanded.includes('{') &&
        expanded !== expandTypeStructural(type, this.expandOrigin())
      ) {
        return expanded;
      }
      // Prefer the expanded form whenever an object got inlined, not only when
      // it leads with `{`. `expandTypeStructural` wraps arrays and unions, so a
      // resolved `Payment[]` or `(Payment | null)[]` renders as `{…}[]` or
      // `({…} | null)[]`; a `startsWith('{')` test would miss those and fall
      // back to the bare, dangling name. Any `{` means real members reached the
      // bundle.
      return expanded.includes('{') ? expanded : fallback;
    } catch {
      return fallback;
    }
  }

  // ===========================================================================
  // Response-helper argument recovery (carrick#631)
  // ===========================================================================

  /**
   * Recover a response contract from a handler's own `return` statements when
   * the resolved return type carries none.
   *
   * A file-based route `loader`/`action` almost never annotates its return: the
   * payload goes through a helper — `json(payload)`, `reply(payload, opts)`,
   * `wrap(request, json(payload))` — and the helper's result is transport, not
   * contract. Two ways that collapses today:
   *
   *   * the helper is typed and returns machinery (`Promise<Response>`), so the
   *     carrick#371 guard abstains and the manifest reads `unknown`;
   *   * the helper comes from a package with no installed declaration (the
   *     scanner reads a bare checkout), so it resolves to `any` and the whole
   *     response reads `any`.
   *
   * In both, the contract is the helper's ARGUMENT — every framework that ships
   * such a helper is generic over it. This walks the handler's returned
   * expressions, descends through wrapper calls to the first argument that
   * carries a payload, and unions the surviving branches. It is structural
   * throughout: no helper, framework or package name is matched anywhere, and
   * the recovery only runs where the existing path already had nothing.
   *
   * `statedOnly` is the guard for the unresolvable-callee case. A return type
   * of `any` does not say the callee was a response helper, only that it could
   * not be resolved, and on a bare checkout that is true of most imported
   * callees: `return db.findMany({ where })` would otherwise report the query
   * object as the endpoint's contract, which is a false contract and worse
   * than `unknown`. With `statedOnly` set, only an argument the source
   * annotates (`satisfies` / `as` / `<T>`) counts, because there the developer
   * stated the contract. The machinery case leaves it clear: the callee is
   * known transport, so its argument is the payload whatever its type.
   *
   * Returns `null` (a logged limitation, never a guess) when no returned
   * expression yields an argument type this may read.
   */
  private recoverPayloadFromReturnStatements(
    func: FunctionLike,
    statedOnly: boolean,
    wire: WireFormat
  ): RecoveredPayload | null {
    return this.recoverPayloadFromResponseExpressions(
      this.responseReturnedExpressions(func),
      statedOnly,
      wire
    );
  }

  /**
   * The response payload carried by a set of response-carrying expressions —
   * a handler's return expressions, or the single expression a producer
   * response locator resolved to (carrick#1017).
   *
   * Each expression is first split into its BRANCHES, so a route that answers
   * `found ? json(entry) : json(problem, { status: 404 })` contributes both
   * sides: the payload rule reads each one, the error branch drops out on its
   * stated status, and what survives is joined as a union exactly as several
   * return statements are.
   */
  private recoverPayloadFromResponseExpressions(
    expressions: Node[],
    statedOnly: boolean,
    wire: WireFormat,
    alsoSerialisers?: ReadonlySet<string>
  ): RecoveredPayload | null {
    const candidates: PayloadCandidate[] = [];

    const returned = expressions.flatMap((expression) =>
      this.expandResponseBranches(expression, 0)
    );
    // `alsoSerialisers` are callees a caller has its own evidence for
    // (`calleeIdentity` keys).
    const serialisers = new Set<string>(alsoSerialisers ?? []);

    for (const expression of returned) {
      const payloadNode = this.responseHelperPayloadNode(
        expression,
        0,
        statedOnly,
        serialisers
      );
      if (!payloadNode) continue;
      candidates.push(this.payloadCandidateOf(payloadNode, wire));
    }
    return this.joinPayloadCandidates(candidates);
  }

  /** How one payload node reads: its stated annotation, else its resolved type. */
  private payloadCandidateOf(payloadNode: Node, wire: WireFormat): PayloadCandidate {
    // A `satisfies X` / `as X` on the argument states the contract in source:
    // read the ANNOTATION, not the literal the compiler widened it from.
    // `expandAnnotationTypeNode` keeps the bare name when the declaration is
    // not resolvable, which is the honest answer on a bare checkout.
    const stated = this.statedTypeNodeOf(payloadNode);
    if (stated) {
      return this.statedPayloadCandidate(payloadNode, stated, wire);
    }
    const payloadType = this.unwrapPromiseType(payloadNode.getType());
    return {
      typeString: this.expandResolvedTypeStructural(
        payloadType,
        typeText(payloadType, payloadNode),
        wire
      ),
      isExplicit: false,
      node: payloadNode,
      anchorType: payloadType,
    };
  }

  /** A payload whose contract a type node in the source states. */
  private statedPayloadCandidate(
    payloadNode: Node,
    stated: Node,
    wire: WireFormat
  ): PayloadCandidate {
    return {
      typeString: this.expandAnnotationTypeNode(stated, wire),
      isExplicit: true,
      node: payloadNode,
      anchorType: this.unwrapPromiseType(stated.getType()),
      statedTypeNode: stated,
    };
  }

  /**
   * Join the payloads of several response branches into one contract, as
   * several return statements are joined: an empty object drops beside a
   * branch with members, equal texts dedupe, and the rest is a union.
   */
  private joinPayloadCandidates(candidates: PayloadCandidate[]): RecoveredPayload | null {
    if (candidates.length === 0) {
      return null;
    }

    // An empty payload (`json({})` on a preflight branch) declares nothing, and
    // `{}` in a union would swallow the branch that does declare something. Drop
    // it only when a branch with members survives — a handler whose ONLY payload
    // is `{}` is honestly reporting an empty body.
    const isEmptyObject = (text: string): boolean =>
      text.replace(/\s+/g, '') === '{}';
    const withMembers = candidates.filter((c) => !isEmptyObject(c.typeString));
    const kept = withMembers.length > 0 ? withMembers : candidates;

    const distinct = distinctByText(kept, (c) => c.typeString);
    const typeString = distinct.map((c) => c.typeString).join(' | ');
    return {
      // carrick#2054: which request field picks each member of the union.
      responseModes:
        distinct.length >= 2
          ? this.responseModesOf(kept.map((c) => ({ node: c.node, type_string: c.typeString })))
          : undefined,
      typeString,
      // Every payload node that fed the union, deduped ones included: a
      // caller asks whether the expression it located is one of them.
      nodes: kept.map((c) => c.node),
      // Only a contract every surviving branch STATES in source is explicit.
      isExplicit: distinct.every((c) => c.isExplicit),
      node: distinct[0].node,
      // A union joined several payloads, so there is no single anchor — the
      // same rule the wrapper-unwrap path applies.
      anchorType: distinct.length === 1 ? distinct[0].anchorType : undefined,
      readTypes: distinct.map((c) => c.anchorType),
      statedTypeNode:
        distinct.length === 1 ? distinct[0].statedTypeNode : undefined,
    };
  }

  // ===========================================================================
  // Serialised-body envelopes (carrick#2200)
  // ===========================================================================

  /**
   * True when a handler's awaited return, or a member of its union, is a
   * serialised-body envelope: an object with a numeric `statusCode` or
   * `status` member and a `body` member typed `string`. Either member may be
   * optional, as a platform's result type declares them.
   *
   * HTTP vocabulary only, the class `RESPONSE_INIT_MEMBER_NAMES` is. A `body`
   * typed as an object is not an envelope: a runtime that serialises the
   * handler's return value sends that object as the payload, so the shape is
   * ambiguous and keeps the ordinary reading.
   */
  private typeIsSerialisedBodyEnvelope(type: Type, at: Node): boolean {
    const members = type.isUnion() ? type.getUnionTypes() : [type];
    return members.some((member) => {
      if (!member.isObject() || member.isArray() || member.getCallSignatures().length > 0) {
        return false;
      }
      const body = this.envelopeMemberType(member, 'body', at);
      if (!body || !(body.isString() || body.isStringLiteral())) return false;
      return STATUS_MEMBER_NAMES.some((name) => {
        const status = this.envelopeMemberType(member, name, at);
        return status !== undefined && (status.isNumber() || statusCodesOfType(status) !== undefined);
      });
    });
  }

  /** A member's type with `undefined` and `null` removed, or `undefined` if absent. */
  private envelopeMemberType(type: Type, name: string, at: Node): Type | undefined {
    const property = type.getProperty(name);
    if (!property) return undefined;
    return property.getTypeAtLocation(at).getNonNullableType();
  }

  /**
   * True when a handler's return is a serialised-body envelope AND the source
   * shows the body is serialised, so the envelope reading applies:
   *  - a returned branch builds an envelope whose `body` is `JSON.stringify`
   *    of a value, inline, through a `const`, or in a builder the reading can
   *    follow (`branchShowsSerialisation`); or
   *  - the envelope type is declared outside the repository (a platform's
   *    result type) with every member optional. Nothing else returns such a
   *    type, so a value of it returned as-is is still transport. The repo's
   *    own all-optional types (`Partial<Article>`) engage only on evidence.
   *
   * A domain object that merely has `status` and `body` members (an article,
   * a message) shows neither and keeps the ordinary reading.
   */
  private serialisedBodyEnvelopeEngages(type: Type, func: FunctionLike): boolean {
    if (!this.typeIsSerialisedBodyEnvelope(type, func)) return false;
    const members = type.isUnion() ? type.getUnionTypes() : [type];
    const platformShaped = members.some((member) => {
      if (!this.typeIsSerialisedBodyEnvelope(member, func)) return false;
      const properties = member.getProperties();
      return properties.every(
        (property) =>
          property.hasFlags(ts.SymbolFlags.Optional) &&
          this.symbolIsLibOrExternalOrigin(property)
      );
    });
    if (platformShaped) return true;
    return this.envelopeBranches(func).some((branch) =>
      this.branchShowsSerialisation(branch, func, 0)
    );
  }

  /**
   * True when a returned branch builds an envelope from a serialised value:
   * a literal whose `body` is `JSON.stringify(...)`, a call to a builder the
   * reading can follow, or a call handing on one such envelope.
   */
  private branchShowsSerialisation(expression: Node, scope: FunctionLike, depth: number): boolean {
    if (depth > RESPONSE_HELPER_MAX_DEPTH) return false;
    const node = this.peelTransparentExpression(expression);
    if (Node.isObjectLiteralExpression(node)) {
      return this.serialisedBodyArgument(node, scope) !== undefined;
    }
    if (!Node.isCallExpression(node)) return false;
    if (this.envelopeBuilderOf(node) !== undefined) return true;
    return this.handedOnArguments(node).some((arg) =>
      this.branchShowsSerialisation(arg, scope, depth + 1)
    );
  }

  /**
   * The arguments a call could hand back as its result: every argument but
   * an object literal, which is the call's input (`create({ body:
   * JSON.stringify(doc) })` stores a document, it does not respond with it),
   * and but a function, whose returns are read as the call's callbacks.
   */
  private handedOnArguments(call: CallExpression): Node[] {
    return call
      .getArguments()
      .map((arg) => this.peelTransparentExpression(arg))
      .filter(
        (arg) =>
          !Node.isObjectLiteralExpression(arg) &&
          !Node.isArrowFunction(arg) &&
          !Node.isFunctionExpression(arg)
      );
  }

  /**
   * The returned branches of an envelope handler: each returned expression,
   * conditional sides split. A returned call whose callbacks return values
   * (`settle().then(ok, fail)`) is carried by those returns, plus every
   * envelope it is handed directly (`withFallback(() => a, b)` can answer
   * `b`); a call whose callbacks return nothing stays a branch of its own.
   */
  private envelopeBranches(func: FunctionLike): Node[] {
    const carriedByCallbacks = new Set(
      this.returnedExpressions(func).filter((expression) => {
        const call = this.peelTransparentExpression(expression);
        if (!Node.isCallExpression(call)) return false;
        return call.getArguments().some((arg) => {
          const callback = this.peelTransparentExpression(arg);
          return (
            (Node.isArrowFunction(callback) || Node.isFunctionExpression(callback)) &&
            this.returnedExpressions(callback).length > 0
          );
        });
      })
    );
    const handedEnvelopes = [...carriedByCallbacks].flatMap((expression) => {
      const call = this.peelTransparentExpression(expression);
      if (!Node.isCallExpression(call)) return [];
      return call
        .getArguments()
        .map((arg) => this.peelTransparentExpression(arg))
        .filter(
          (arg) =>
            !Node.isArrowFunction(arg) &&
            !Node.isFunctionExpression(arg) &&
            this.typeIsSerialisedBodyEnvelope(arg.getType(), arg)
        );
    });
    return [
      ...this.responseReturnedExpressions(func).filter(
        (expression) => !carriedByCallbacks.has(expression)
      ),
      ...handedEnvelopes,
    ].flatMap((expression) => this.expandResponseBranches(expression, 0));
  }

  /**
   * The `function_return` answer for a handler that returns a serialised-body
   * envelope: the union of the values its success branches serialised.
   *
   * Each returned branch is read by `readEnvelopeBranch`. Error and redirect
   * branches contribute nothing (carrick#1161). If any branch cannot be read,
   * the answer is `unknown` with the decided reason `serialised_body_unread`
   * and one sentence per cause: the branches that did read are part of the
   * contract, and neither the envelope nor its `string` body is any of it.
   */
  private inferSerialisedBodyEnvelope(
    request: InferRequestItem,
    func: FunctionLike
  ): InferredType {
    const wire = this.wireFormatFor(request);
    const branches = this.envelopeBranches(func);

    const candidates: PayloadCandidate[] = [];
    const causes = new Set<EnvelopeUnreadCause>();
    for (const branch of branches) {
      const read = this.readEnvelopeBranch(branch, func, wire, 0);
      if (read === 'dropped') continue;
      if ('unread' in read) causes.add(read.unread);
      else candidates.push(read);
    }

    const location = this.getNodeLocation(func);
    if (causes.size > 0) {
      this.log(
        `Return at ${request.file_path}:${request.line_number} is a serialised-body ` +
          `envelope and a returned branch was not read (${[...causes].join(', ')}); abstaining`
      );
      return this.decidedAbstain(request, location, {
        path: '',
        kind: 'unknown',
        reason: 'serialised_body_unread',
        detail: [...causes].map((cause) => SERIALISED_BODY_UNREAD_DETAIL[cause]).join(' '),
      });
    }
    const recovered = this.joinPayloadCandidates(candidates);
    if (!recovered) {
      return this.decidedAbstain(request, location, {
        path: '',
        kind: 'unknown',
        reason: 'no_success_payload',
        detail:
          'every status and body this handler returns states an error or redirect status, so it publishes no success body',
      });
    }
    return this.inferredFromRecoveredPayload(request, recovered);
  }

  /**
   * carrick#2674: the locator names a request call's options object literal
   * and the member that carries the call's body. The call sends no request
   * body when the literal can write no such member: no spread, no computed
   * key, and no member of that name in any form (assignment, shorthand,
   * method, accessor, quoted key), nor any other body-named member
   * (`BODY_MEMBER_NAMES`, `REQUEST_BODY_PARTS`). Anything else is not decided here, so the
   * answer is null. `configArgumentBody` stays the reader of the member
   * itself; the checks above it are this kind's own.
   */
  private inferRequestConfig(
    sourceFile: SourceFile,
    request: InferRequestItem
  ): InferredType | null {
    const member = request.member;
    if (!member) return null;
    const located = this.resolveTargetNode(sourceFile, request);
    if (!located) return null;
    const literal = this.unwrapExpressionNode(located);
    if (!Node.isObjectLiteralExpression(literal)) return null;
    for (const property of literal.getProperties()) {
      // A spread has no name node, so it falls out here with anything else
      // unnamed: the literal may write the member through it.
      const nameNode = Node.isShorthandPropertyAssignment(property)
        ? property.getNameNode()
        : (property as { getNameNode?: () => Node }).getNameNode?.();
      if (!nameNode) return null;
      if (Node.isComputedPropertyName(nameNode)) return null;
      const name =
        Node.isStringLiteral(nameNode) || Node.isNoSubstitutionTemplateLiteral(nameNode)
          ? nameNode.getLiteralText()
          : nameNode.getText();
      // A client may take its body through a member other than the one the
      // locator names (`json`, `form`, `data`), so any body-named key refuses,
      // the same rule `requestObjectBody` keeps.
      if (name === member || BODY_MEMBER_NAMES.has(name) || REQUEST_BODY_PARTS.has(name)) {
        return null;
      }
    }
    const read = this.configArgumentBody(literal, member, "the call's request config");
    if (read.kind !== 'no_body') return null;
    this.log(
      `Request config at ${request.file_path}:${request.line_number} writes no '${member}' member; ` +
        'the call sends no request body'
    );
    return this.decidedAbstain(request, this.getNodeLocation(literal), {
      path: '',
      kind: 'unknown',
      reason: 'no_request_body',
      detail:
        `the call's request config sets no '${read.member}' member, which is where the call ` +
        'takes its body, so the call sends no request body',
    });
  }

  /** An `unknown` answer the inferrer decided, carrying its reason at the root. */
  private decidedAbstain(
    request: InferRequestItem,
    location: SourceLocation,
    reason: TypeProvenance
  ): InferredType {
    const abstain = this.createInferredType(request, 'unknown', false, location);
    abstain.any_provenance = [reason];
    return abstain;
  }

  /**
   * What one returned branch of an envelope handler serialised:
   *  - an envelope object literal: the argument of the `JSON.stringify` that
   *    produced its `body`, inline or through a `const` in the same function;
   *    its status member decides the branch;
   *  - a call to a response builder this repository declares
   *    (`envelopeBuilderOf`): the argument the builder serialises; the
   *    argument or literal its status member takes decides the branch;
   *  - any other call handing on exactly one envelope argument (never an
   *    object literal, which is the call's input): that
   *    envelope is read in its place. Its own stated status decides the
   *    branch; only when it states none does a status the outer call's
   *    arguments state decide (`log(ctx, 503, "code", prebuilt)`).
   * Anything else, and a payload holding `any` or `unknown`, is unread, with
   * its cause.
   */
  private readEnvelopeBranch(
    expression: Node,
    scope: FunctionLike,
    wire: WireFormat,
    depth: number
  ): EnvelopeBranchRead {
    if (depth > RESPONSE_HELPER_MAX_DEPTH) return { unread: 'builder' };
    const node = this.peelTransparentExpression(expression);

    if (Node.isObjectLiteralExpression(node)) {
      const status = this.envelopeLiteralStatus(node);
      if (status === 'error' || status === 'redirect') return 'dropped';
      const payload = this.serialisedBodyArgument(node, scope);
      return payload ? this.envelopePayloadCandidate(payload, wire) : { unread: 'not_serialised' };
    }

    if (!Node.isCallExpression(node)) return { unread: 'prebuilt' };

    const builder = this.envelopeBuilderOf(node);
    if (builder === 'failure') return 'dropped';
    if (builder) {
      const status = this.builderCallStatus(node, builder);
      if (status === 'error' || status === 'redirect') return 'dropped';
      const argument = node.getArguments()[builder.payloadParameter];
      if (!argument) return { unread: 'builder' };
      return this.builderPayloadCandidate(node, builder, argument, wire);
    }

    const envelopes = this.handedOnArguments(node).filter((arg) =>
      this.typeIsSerialisedBodyEnvelope(arg.getType(), arg)
    );
    if (envelopes.length > 1) return { unread: 'builder' };
    const inner = envelopes[0];
    const innerStatus = inner ? this.envelopeOwnStatus(inner) : 'undecided';
    if (innerStatus === 'undecided' || innerStatus === 'variable') {
      const outer = this.responseSiteStatus(node);
      if (outer === 'error' || outer === 'redirect') return 'dropped';
    }
    if (!inner) return { unread: 'builder' };
    return this.readEnvelopeBranch(inner, scope, wire, depth + 1);
  }

  /** The status a builder call sends: its status argument, or the builder's literal. */
  private builderCallStatus(call: CallExpression, builder: EnvelopeBuilder): ResponseSiteStatus {
    if (builder.statusParameter !== undefined) {
      return this.statusOfValue(call.getArguments()[builder.statusParameter]);
    }
    return builder.fixedStatus ?? 'undecided';
  }

  /**
   * The status an envelope expression states itself: a literal's status
   * member, or what a followable builder call sends. `'undecided'` for
   * anything else (a variable holding an envelope states nothing here).
   */
  private envelopeOwnStatus(expression: Node): ResponseSiteStatus {
    const node = this.peelTransparentExpression(expression);
    if (Node.isObjectLiteralExpression(node)) return this.envelopeLiteralStatus(node);
    if (!Node.isCallExpression(node)) return 'undecided';
    const builder = this.envelopeBuilderOf(node);
    if (builder === 'failure') return 'error';
    return builder ? this.builderCallStatus(node, builder) : 'undecided';
  }

  /** The status an envelope literal's `statusCode` or `status` member states. */
  private envelopeLiteralStatus(literal: ObjectLiteralExpression): ResponseSiteStatus {
    for (const name of STATUS_MEMBER_NAMES) {
      const value = this.envelopeMemberValue(literal, name);
      if (value) return this.statusOfValue(value);
    }
    return 'undecided';
  }

  /** The status a value states, classified as a send's status argument is. */
  private statusOfValue(value: Node | undefined): ResponseSiteStatus {
    if (!value) return 'undecided';
    const read = this.statedStatusCodes(this.peelTransparentExpression(value));
    if (read === 'variable') return 'variable';
    if (!read) return 'undecided';
    const kind = classifyStatusCodes(read);
    return kind === 'mixed' ? 'variable' : kind;
  }

  /**
   * The value node an object literal gives a member: a property assignment's
   * initializer, or a shorthand's own name.
   */
  private envelopeMemberValue(literal: ObjectLiteralExpression, name: string): Node | undefined {
    const property = literal.getProperty(name);
    if (property && Node.isPropertyAssignment(property)) return property.getInitializer();
    if (property && Node.isShorthandPropertyAssignment(property)) return property.getNameNode();
    return undefined;
  }

  /**
   * The value an envelope literal's `body` serialises: the argument of a
   * `JSON.stringify` written there, or written as the initializer of a
   * `const` that `scope` declares and `body` names. `undefined` when `body`
   * is built any other way.
   */
  private serialisedBodyArgument(
    literal: ObjectLiteralExpression,
    scope: FunctionLike
  ): Node | undefined {
    const raw = this.envelopeMemberValue(literal, 'body');
    if (!raw) return undefined;
    const value = this.peelTransparentExpression(raw);
    const direct = this.unwrapJsonStringifyArg(value);
    if (direct !== value) return direct;
    const initializer = this.constInitializerInScope(value, scope);
    if (!initializer) return undefined;
    const bound = this.peelTransparentExpression(initializer);
    const viaConst = this.unwrapJsonStringifyArg(bound);
    return viaConst !== bound ? viaConst : undefined;
  }

  /**
   * The initializer of the `const` an identifier (or a shorthand member's
   * name) refers to, when `scope` itself declares it.
   */
  private constInitializerInScope(node: Node, scope: FunctionLike): Node | undefined {
    for (const declaration of this.valueSymbolOf(node)?.getDeclarations() ?? []) {
      if (!Node.isVariableDeclaration(declaration)) continue;
      if (
        declaration.getVariableStatement()?.getDeclarationKind() !==
        VariableDeclarationKind.Const
      ) {
        continue;
      }
      if (this.findContainingFunctionForNode(declaration) !== scope) continue;
      return declaration.getInitializer();
    }
    return undefined;
  }

  /**
   * The symbol of the value a node names: a shorthand member's value symbol
   * (its own symbol is the property), else the identifier's symbol.
   */
  private valueSymbolOf(node: Node): TsSymbol | undefined {
    const parent = node.getParent();
    if (parent && Node.isShorthandPropertyAssignment(parent) && parent.getNameNode() === node) {
      return parent.getValueSymbol();
    }
    return Node.isIdentifier(node) ? node.getSymbol() : undefined;
  }

  /** The index of the `params` entry a node names, or `undefined`. */
  private parameterIndexOf(node: Node, params: ParameterDeclaration[]): number | undefined {
    for (const declaration of this.valueSymbolOf(node)?.getDeclarations() ?? []) {
      const index = params.findIndex((param) => param === declaration);
      if (index >= 0) return index;
    }
    return undefined;
  }

  /**
   * How a call's callee builds an envelope, when the callee is a function this
   * repository declares (not lib, not an installed package) and every return
   * it makes is an envelope literal:
   *  - returns whose status member is a literal 4xx or 5xx are its failure
   *    path and are ignored; a builder with nothing else is `'failure'`, so a
   *    call to it is an error branch;
   *  - every other return must serialise the same parameter into `body` and
   *    take its status from the same parameter, or state the same literal
   *    success or redirect status.
   * `undefined` when the callee is anything else.
   */
  private envelopeBuilderOf(call: CallExpression): EnvelopeBuilder | 'failure' | undefined {
    let symbol = call.getExpression().getSymbol();
    if (symbol?.isAlias()) symbol = symbol.getAliasedSymbol() ?? symbol;
    if (!symbol || this.symbolIsLibOrExternalOrigin(symbol)) return undefined;
    const builder = this.builderFunctionOf(symbol);
    if (!builder) return undefined;

    const params = builder.getParameters();
    const returned = this.returnedExpressions(builder).flatMap((expression) =>
      this.expandResponseBranches(expression, 0)
    );
    let mapping: EnvelopeBuilder | undefined;
    let sawFailure = false;
    for (const expression of returned) {
      const literal = this.peelTransparentExpression(expression);
      if (!Node.isObjectLiteralExpression(literal)) return undefined;
      const statusValue = STATUS_MEMBER_NAMES.map((name) =>
        this.envelopeMemberValue(literal, name)
      ).find((value) => value !== undefined);
      if (!statusValue) return undefined;
      const statusParameter = this.parameterIndexOf(statusValue, params);
      let fixedStatus: ResponseSiteStatus | undefined;
      if (statusParameter === undefined) {
        const codes = this.statedStatusCodes(this.peelTransparentExpression(statusValue));
        if (!Array.isArray(codes)) return undefined;
        const kind = classifyStatusCodes(codes);
        if (kind === 'error') {
          sawFailure = true;
          continue;
        }
        if (kind === 'mixed') return undefined;
        fixedStatus = kind;
      }
      const serialised = this.serialisedBodyArgument(literal, builder);
      const payloadParameter = serialised ? this.parameterIndexOf(serialised, params) : undefined;
      if (payloadParameter === undefined) return undefined;
      const here: EnvelopeBuilder = { builder, payloadParameter, statusParameter, fixedStatus };
      if (
        mapping &&
        (mapping.payloadParameter !== here.payloadParameter ||
          mapping.statusParameter !== here.statusParameter ||
          mapping.fixedStatus !== here.fixedStatus)
      ) {
        return undefined;
      }
      mapping = here;
    }
    if (mapping) return mapping;
    return sawFailure ? 'failure' : undefined;
  }

  /**
   * The single function a callee symbol declares with a body: a function
   * declaration, or a variable initialised with an arrow or function
   * expression.
   */
  private builderFunctionOf(symbol: TsSymbol): BuilderFunction | undefined {
    const bodies: BuilderFunction[] = [];
    for (const declaration of symbol.getDeclarations()) {
      if (Node.isFunctionDeclaration(declaration) && declaration.getBody()) {
        bodies.push(declaration);
      } else if (Node.isVariableDeclaration(declaration)) {
        const initializer = declaration.getInitializer();
        const fn = initializer ? this.peelTransparentExpression(initializer) : undefined;
        if (fn && (Node.isArrowFunction(fn) || Node.isFunctionExpression(fn))) bodies.push(fn);
      }
    }
    return bodies.length === 1 ? bodies[0] : undefined;
  }

  /**
   * The payload a builder call serialises. The source states it, and the
   * answer is explicit, when the argument carries `satisfies`/`as`, when the
   * builder's parameter is one of its type parameters and the call passes
   * type arguments (`reply<AddNoteResult>(201, x)`), or when the parameter is
   * declared with a type that says something. Otherwise the argument's own
   * type is read.
   */
  private builderPayloadCandidate(
    call: CallExpression,
    builder: EnvelopeBuilder,
    argument: Node,
    wire: WireFormat
  ): EnvelopeBranchRead {
    const payloadNode = this.peelTransparentExpression(argument);
    if (this.statedTypeNodeOf(payloadNode)) {
      return this.envelopePayloadCandidate(payloadNode, wire);
    }
    const declared = builder.builder.getParameters()[builder.payloadParameter]?.getTypeNode();
    if (declared) {
      const typeParameterIndex = builder.builder
        .getTypeParameters()
        .findIndex((tp) => tp.getName() === declared.getText().trim());
      if (typeParameterIndex >= 0) {
        const typeArgument = call.getTypeArguments()[typeParameterIndex];
        if (typeArgument) {
          return this.checkedEnvelopeCandidate(
            this.statedPayloadCandidate(payloadNode, typeArgument, wire)
          );
        }
      } else if (!this.typeNodeStatesNothing(declared)) {
        return this.checkedEnvelopeCandidate(
          this.statedPayloadCandidate(payloadNode, declared, wire)
        );
      }
    }
    return this.envelopePayloadCandidate(payloadNode, wire);
  }

  /**
   * True when a declared parameter type does not say what is sent: a top
   * type, the `object` keyword, an object type that names no member (`{}`,
   * `Record<string, unknown>`), a union that admits both primitives and
   * objects (a JSON-value type such as `string | number | Json[] | { [k:
   * string]: Json }`), or a type that mentions a type parameter. A builder
   * that takes "any value" states nothing about the payload a call hands it,
   * so the argument's own type is read instead.
   */
  private typeNodeStatesNothing(typeNode: Node): boolean {
    const type = typeNode.getType();
    if (type.isAny() || type.isUnknown() || type.isTypeParameter()) return true;
    if (type.getFlags() & ts.TypeFlags.NonPrimitive) return true;
    if (type.isObject() && !type.isArray() && type.getProperties().length === 0) {
      return true;
    }
    if (type.isUnion()) {
      const members = type.getUnionTypes();
      const primitive = members.some(
        (member) =>
          member.isString() ||
          member.isNumber() ||
          member.isBoolean() ||
          member.isBooleanLiteral()
      );
      if (primitive && members.some((member) => member.isObject())) return true;
    }
    return [typeNode, ...typeNode.getDescendants()].some(
      (node) => Node.isTypeReference(node) && node.getType().isTypeParameter()
    );
  }

  /** A serialised value's candidate, unread when it holds a top type. */
  private envelopePayloadCandidate(payloadNode: Node, wire: WireFormat): EnvelopeBranchRead {
    return this.checkedEnvelopeCandidate(this.payloadCandidateOf(payloadNode, wire));
  }

  /**
   * A payload candidate, or unread when its printed type is or holds `any`
   * or `unknown` anywhere (`{ [x: string]: any }`, `{ data: any }`). A
   * contract with a hole in it is not published on this path: the capture
   * would refuse the text and re-read the line, which can publish the
   * envelope.
   */
  private checkedEnvelopeCandidate(candidate: PayloadCandidate): EnvelopeBranchRead {
    return TOP_TYPE_TOKEN.test(candidate.typeString) ? { unread: 'top_type' } : candidate;
  }

  /**
   * The request field each member of a joined union answers
   * (`response-modes.ts`). Case texts are joined by the published union's
   * rule. A reading that fails is no reading: it must never cost the union.
   */
  private responseModesOf(bodies: JoinedBody[]): ResponseModes | undefined {
    try {
      return readResponseModes(
        bodies,
        (texts) => distinctByText(texts, (text) => text).join(' | '),
        (symbol) => this.symbolIsLibOrExternalOrigin(symbol)
      );
    } catch (error) {
      this.log(`Response modes not read: ${error instanceof Error ? error.message : String(error)}`);
      return undefined;
    }
  }

  /**
   * The expressions a function returns, excluding returns belonging to nested
   * functions (a `.map(x => …)` callback returns a row, not the response).
   * A concise arrow body is itself the returned expression.
   */
  private returnedExpressions(func: FunctionLike): Node[] {
    const body = func.getBody();
    if (!body) return [];
    if (!Node.isBlock(body)) return [body];

    const returned: Node[] = [];
    for (const statement of body.getDescendantsOfKind(SyntaxKind.ReturnStatement)) {
      if (this.findContainingFunctionForNode(statement) !== func) continue;
      const expression = statement.getExpression();
      if (expression) returned.push(expression);
    }
    return returned;
  }

  /**
   * The expressions that can carry this handler's response: its own returned
   * expressions, plus the returned expressions of any function the handler
   * hands DIRECTLY to a returned call (carrick#707).
   *
   * `return settle(...).then(ok => serialise(body), err => serialise(problem))`
   * returns a call whose result is whatever the callbacks produce, so the
   * callbacks' returns are this handler's returns as surely as its own. That
   * shape is how a result-type API, a promise combinator or a matcher is
   * written, and on a bare checkout the callee resolves to nothing, so the
   * outer call's type says nothing at all.
   *
   * Only ONE level, and only from the returned call's own arguments. A
   * callback nested deeper is inside an expression the handler computes, not
   * an expression it returns — `return rows.map(r => …)` returns a row list,
   * and reading the callback there would report a row as the response. Each
   * flattened expression still has to pass the payload rule below, which no
   * bare object literal does.
   */
  private responseReturnedExpressions(func: FunctionLike): Node[] {
    const own = this.returnedExpressions(func);
    const flattened: Node[] = [...own];
    for (const expression of own) {
      const call = this.peelTransparentExpression(expression);
      if (!Node.isCallExpression(call)) continue;
      for (const arg of call.getArguments()) {
        const callback = this.peelTransparentExpression(arg);
        if (
          !Node.isArrowFunction(callback) &&
          !Node.isFunctionExpression(callback)
        ) {
          continue;
        }
        flattened.push(...this.returnedExpressions(callback));
      }
    }
    return flattened;
  }

  /**
   * Stable identity for a call's callee within one file: its resolved symbol
   * when the compiler has one, else the callee's source text.
   */
  private calleeIdentity(call: Node): string | undefined {
    if (!Node.isCallExpression(call) && !Node.isNewExpression(call)) {
      return undefined;
    }
    const callee = call.getExpression();
    const symbol = callee.getSymbol();
    if (symbol) return `symbol:${symbol.getFullyQualifiedName()}`;
    const text = callee.getText().trim();
    return text.length > 0 ? `text:${text}` : undefined;
  }

  /**
   * The payload argument of a returned response-helper call, or `undefined`.
   *
   * Walks the call's arguments in source order: the first whose type reads as a
   * contract wins; an argument that is itself a call is descended into, which is
   * what recovers `wrap(request, json(payload))` — the transport wrapper's own
   * first argument is the request, which never reads as a contract.
   *
   * A call whose sibling options object states a >= 400 status is an error
   * branch and contributes nothing: the response contract of an endpoint is the
   * shape it returns when it succeeds.
   *
   * `statedOnly` narrows what counts as a payload to an argument the source
   * annotates; see `recoverPayloadFromReturnStatements`. It is lifted for a
   * callee a caller has its own evidence for (`serialisers`).
   */
  private responseHelperPayloadNode(
    expression: Node,
    depth: number,
    statedOnly: boolean,
    serialisers: Set<string>
  ): Node | undefined {
    if (depth > RESPONSE_HELPER_MAX_DEPTH) return undefined;

    const call = this.peelTransparentExpression(expression);
    // A `new` of the platform response constructor carries its body exactly
    // where a call does — `new Response(JSON.stringify(payload), { status })`
    // is the same statement as `Response.json(payload, { status })`
    // (carrick#1017). Both node kinds answer `getArguments()`.
    if (!Node.isCallExpression(call) && !Node.isNewExpression(call)) {
      return undefined;
    }

    const args = call.getArguments().map((a) => this.peelTransparentExpression(a));
    if (args.length === 0) return undefined;
    // An error or redirect branch sends no success body (carrick#1161).
    const status = this.responseSiteStatus(call);
    if (status === 'error' || status === 'redirect') return undefined;

    const identity = this.calleeIdentity(call);
    const proven = identity !== undefined && serialisers.has(identity);
    const effectiveStatedOnly = statedOnly && !proven;

    for (const [index, arg] of args.entries()) {
      // An options object BESIDE the body states how to send it, never what is
      // sent (carrick#1017): `new Response(null, { status: 204 })` has no body
      // at all, and reading its second argument would publish `{ status:
      // number }` as the endpoint's contract. The first argument is the body
      // by construction, so the rule applies only past it.
      if (index > 0 && this.statesResponseInit(arg)) continue;
      const candidate = this.unwrapJsonStringifyArg(arg);
      if (this.nodeCarriesPayloadContract(candidate, effectiveStatedOnly)) {
        return candidate;
      }
      const nested = this.responseHelperPayloadNode(
        candidate,
        depth + 1,
        statedOnly,
        serialisers
      );
      if (nested) return nested;
    }
    return undefined;
  }

  /**
   * True when a node's type reads as a response payload.
   *
   * Object-shaped only (an object literal, a named interface, an array of
   * either, or a union containing one). A bare primitive argument is a
   * redirect location, a status code or a body string — `redirectTo("/next")`
   * must never report `"/next"` as the endpoint's contract. A stated
   * `satisfies`/`as` annotation counts even when its declaration is missing:
   * the source says what the contract is.
   *
   * Under `statedOnly` the annotation is the ONLY thing that counts, so an
   * unresolvable callee's arguments never become a contract by accident.
   *
   * `asSent` judges the object in the form it is sent in, which is what an
   * ARGUMENT handed to a sender is asked; see `typeIsObjectShaped`.
   */
  private nodeCarriesPayloadContract(
    node: Node,
    statedOnly: boolean,
    asSent = true
  ): boolean {
    if (this.statedTypeNodeOf(node)) return true;
    if (statedOnly) return false;

    const type = this.unwrapPromiseType(node.getType());
    if (
      type.isAny() ||
      type.isUnknown() ||
      type.isNever() ||
      type.isVoid() ||
      type.isUndefined() ||
      type.isNull()
    ) {
      return false;
    }
    if (type.getCallSignatures().length > 0) return false;
    if (this.typeIsOrContainsResponseMachinery(type)) return false;
    return this.typeIsObjectShaped(type, 0, asSent);
  }

  /**
   * Object, array-of-object, or a union/intersection containing one.
   *
   * With `asSent` the object is judged in the form it is SENT in
   * (carrick#1163): a value that declares `toJSON()` travels as what that
   * returns. A `URL` or a `Date` is therefore the string it serialises to,
   * the bare primitive this rule already refuses, and `redirect(new URL(path,
   * base))` hands over a location exactly as `redirect("/next")` does
   * (carrick#807). A value whose JSON form is itself an object is a body like
   * any other.
   */
  private typeIsObjectShaped(type: Type, depth: number, asSent: boolean): boolean {
    if (depth > 2) return false;
    if (type.isUnion()) {
      return type.getUnionTypes().some((m) => this.typeIsObjectShaped(m, depth + 1, asSent));
    }
    if (type.isIntersection()) {
      return type
        .getIntersectionTypes()
        .some((m) => this.typeIsObjectShaped(m, depth + 1, asSent));
    }
    if (type.isArray()) {
      const element = type.getArrayElementType();
      return element ? this.typeIsObjectShaped(element, depth + 1, asSent) : false;
    }
    const sent = asSent ? jsonWireType(type) : undefined;
    if (sent) return this.typeIsObjectShaped(sent, depth + 1, asSent);
    return type.isObject() && !type.isTuple();
  }

  /**
   * The `satisfies X` / `as X` / `<X>` annotation node on an expression, when
   * the source states its type. `satisfies` is the shape a route uses to claim
   * a shared response contract without widening the literal.
   */
  private statedTypeNodeOf(node: Node): Node | undefined {
    if (Node.isSatisfiesExpression(node) || Node.isAsExpression(node)) {
      return node.getTypeNode();
    }
    if (Node.isTypeAssertion(node)) {
      return node.getTypeNode();
    }
    return undefined;
  }

  /**
   * True when an object literal argument is response INIT rather than a body:
   * every property it declares is one the standard `ResponseInit` declares
   * (`status`, `statusText`, `headers`), and it states at least one of them as
   * init really does — a status in the HTTP range, or headers.
   *
   * The status does NOT have to be a literal code. A route that carries its
   * outcome in a value writes `new Response(body, { status: result.status })`,
   * where the source fixes no code and `statedStatusCodes` answers
   * `'variable'` — status-shaped, just not pinned. Requiring a literal there
   * left the whole init object reading as a body, so an endpoint whose payload
   * this layer does not publish (a string body) published `{ status: number }`
   * as its response contract instead: a wrong contract, served, where an
   * abstention was the honest answer.
   *
   * A payload that merely has a `status` member of its own (`{ status: "ok",
   * service: "ledger" }`) declares members init does not, or states `status` as
   * something other than an HTTP code, so it stays a payload.
   */
  private statesResponseInit(node: Node): boolean {
    if (!Node.isObjectLiteralExpression(node)) return false;
    const properties = node.getProperties();
    if (properties.length === 0) return false;
    let statesInit = false;
    for (const property of properties) {
      // `{ status: 202, headers }` names its headers by shorthand, which
      // states init exactly as `headers: headers` does.
      const shorthand = Node.isShorthandPropertyAssignment(property);
      if (!shorthand && !Node.isPropertyAssignment(property)) return false;
      const name = property.getName();
      if (!RESPONSE_INIT_MEMBER_NAMES.has(name)) return false;
      const value = shorthand ? property.getNameNode() : property.getInitializer();
      if (!value) return false;
      if (name === 'headers') {
        statesInit = true;
        continue;
      }
      if (name === 'status' && this.statedStatusCodes(value) !== undefined) {
        statesInit = true;
      }
    }
    return statesInit;
  }

  /**
   * What a response send states about its HTTP status (carrick#1161).
   *
   * Read in order, and the first that states anything decides:
   *
   *  1. an argument past the body that carries a status: a numeric literal
   *     (`send(body, 404)`), an options object (`{ status: 401 }`), or any
   *     expression whose TYPE is a status literal or a union of them
   *     (`let code: 400 | 500`). A numeric argument whose type is a plain
   *     `number` states a status the source does not fix, which is
   *     `variable`: the branch fails open into the union and is logged.
   *  2. the send call's own RESULT type, when a member of it is typed as a
   *     status literal. A framework that types its sends records the status
   *     there, and a redirect helper's default `302` is only visible there.
   *     A result whose status member spans success and error codes (the
   *     default of a typed `json(body, status?)`) says nothing either way.
   *
   * A status in the FIRST argument is a field of the body and is never read.
   * No method or framework name is consulted anywhere.
   */
  private responseSiteStatus(expression: Node, readChain = false): ResponseSiteStatus {
    const call = this.peelTransparentExpression(expression);
    if (!Node.isCallExpression(call) && !Node.isNewExpression(call)) {
      return 'undecided';
    }
    const args = call.getArguments().map((a) => this.peelTransparentExpression(a));
    const stated: number[] = [];
    let variable = false;
    for (const arg of args.slice(1)) {
      const read = this.statedStatusCodes(arg);
      if (read === 'variable') {
        variable = true;
      } else if (read) {
        stated.push(...read);
      }
    }
    if (stated.length > 0) {
      const kind = classifyStatusCodes(stated);
      return kind === 'mixed' ? 'variable' : kind;
    }
    if (variable) return 'variable';

    // carrick#2005: a status an earlier call of the same chain states
    // (`reply.status(401).json(body)`). Read only where the caller asks for
    // it; the return walk does not read it yet (carrick#1962).
    if (readChain) {
      const chained = this.chainStatus(call);
      if (chained) return chained;
    }

    const typed = this.sendResultStatusCodes(call);
    if (typed) {
      const kind = classifyStatusCodes(typed);
      if (kind !== 'mixed') return kind;
    }
    return 'undecided';
  }

  /**
   * The status an earlier call of a send's own chain states: the receiver
   * calls `call` is invoked on (`a.status(401).json(body)` has `a.status(401)`),
   * innermost last. Each receiver call's first argument is read by the rule
   * an argument past a body is, so a numeric literal or a literal-typed value
   * counts and a plain `number` is `variable`. `undefined` when no receiver
   * call states a status. No method name is consulted: a receiver call that
   * is handed no status-shaped value says nothing.
   */
  private chainStatus(call: Node): ResponseSiteStatus | undefined {
    if (!Node.isCallExpression(call)) return undefined;
    let callee = call.getExpression();
    while (Node.isPropertyAccessExpression(callee)) {
      const receiver = this.peelTransparentExpression(callee.getExpression());
      if (!Node.isCallExpression(receiver)) return undefined;
      const status = this.statusOfValue(receiver.getArguments()[0]);
      if (status !== 'undecided') return status;
      callee = receiver.getExpression();
    }
    return undefined;
  }

  /**
   * The HTTP status codes an argument states, `'variable'` when it is a
   * status-shaped value the source does not fix, or `undefined` when it says
   * nothing about a status.
   *
   * Read from the AST first: `{ status: 400 }` in an argument position widens
   * to `{ status: number }`, so the literal only survives syntactically. The
   * type read behind it catches `as const`, hoisted option objects and
   * literal-typed variables. Only values in the HTTP range count: an
   * arbitrary number named `status` on a domain object (`{ status: 2 }`)
   * states nothing about transport.
   */
  private statedStatusCodes(node: Node): number[] | 'variable' | undefined {
    if (Node.isNumericLiteral(node)) {
      const code = httpStatus(node.getLiteralValue());
      return code === undefined ? undefined : [code];
    }

    if (Node.isObjectLiteralExpression(node)) {
      for (const name of STATUS_MEMBER_NAMES) {
        const property = node.getProperty(name);
        if (!property || !Node.isPropertyAssignment(property)) continue;
        const initializer = property.getInitializer();
        if (!initializer) continue;
        const read = this.statedStatusCodes(this.peelTransparentExpression(initializer));
        if (read) return read;
      }
      return undefined;
    }

    const type = node.getType();
    const fromType = statusCodesOfType(type);
    if (fromType) return fromType;
    if (type.isNumber()) return 'variable';
    for (const name of STATUS_MEMBER_NAMES) {
      const property = type.getProperty(name);
      const declaration = property?.getDeclarations()[0];
      if (!property || !declaration) continue;
      const propertyType = property.getTypeAtLocation(declaration).getNonNullableType();
      const codes = statusCodesOfType(propertyType);
      if (codes) return codes;
      if (propertyType.isNumber()) return 'variable';
    }
    return undefined;
  }

  /**
   * Status codes a send call's result type records: the members (of the
   * result, or of each part of an intersection result) typed as a status
   * literal or a union of them. `undefined` when none is.
   */
  private sendResultStatusCodes(call: Node): number[] | undefined {
    const result = this.unwrapPromiseType(call.getType());
    const parts = result.isIntersection() ? result.getIntersectionTypes() : [result];
    const codes: number[] = [];
    for (const part of parts) {
      if (part.isAny() || part.isUnknown() || !part.isObject()) continue;
      for (const property of part.getProperties()) {
        const codesHere = statusCodesOfType(property.getTypeAtLocation(call));
        if (codesHere) codes.push(...codesHere);
      }
    }
    return codes.length > 0 ? codes : undefined;
  }

  /**
   * Peel the wrappers that do not change an expression's payload: parentheses
   * and `await`. Unlike `unwrapExpressionNode` this KEEPS `as`/`satisfies`,
   * because the annotation is exactly what the recovery wants to read.
   */
  private peelTransparentExpression(node: Node): Node {
    let current = node;
    while (
      Node.isParenthesizedExpression(current) ||
      Node.isAwaitExpression(current)
    ) {
      current = current.getExpression();
    }
    return current;
  }

  /**
   * The branches a single response expression can take — one entry for a plain
   * expression, one per side of a conditional (carrick#1017).
   *
   * `return entry ? json(entry) : json({ error }, { status: 404 })` is one
   * return statement holding two responses. Read whole, its type is the
   * transport wrapper both sides share and the payload is lost; read per
   * branch, each side goes through the ordinary payload rule, which is also
   * what drops the error branch on its stated status. Nested conditionals are
   * followed to a small depth; anything else is returned as itself.
   */
  private expandResponseBranches(node: Node, depth: number): Node[] {
    if (depth > RESPONSE_BRANCH_MAX_DEPTH) return [node];
    const expression = this.peelTransparentExpression(node);
    if (Node.isConditionalExpression(expression)) {
      return [
        ...this.expandResponseBranches(expression.getWhenTrue(), depth + 1),
        ...this.expandResponseBranches(expression.getWhenFalse(), depth + 1),
      ];
    }
    return [expression];
  }

  /**
   * The symbol an anchor is named from: the type's own symbol, unless that
   * symbol's name is synthetic (`__type`, `__object`, ...), in which case the
   * alias symbol (carrick#2148).
   *
   * `type Widget = { id: string }` and an instantiated `Page<Order>` carry
   * the anonymous `__type` as their own symbol and the alias as their alias
   * symbol. Reading the own symbol first and stopping because it is truthy
   * left every object-literal alias without an anchor, while the same shape
   * declared as an interface had one. The own symbol still wins whenever it
   * has a real name, so every type anchored before keeps its anchor.
   */
  private anchorSymbolOf(type: Type): TsSymbol | undefined {
    const own = type.getSymbol();
    if (own && !own.getName().startsWith('__')) {
      return own;
    }
    return type.getAliasSymbol() ?? own;
  }

  /**
   * The deterministic source symbol of a resolved type (`Payment` for a payload
   * typed `Payment`), or `undefined` when there is no single user-defined
   * symbol to anchor on. This is the same `getSymbol() || getAliasSymbol()` name
   * the socket anchor already derives (`socket_io.rs`), filtered through
   * `BUILTIN_ANCHOR_SYMBOLS` so TS/lib globals (`Promise`, `Array`, `Date`,
   * primitives, …) never become an anchor. Used to populate
   * `primary_type_symbol` so the manifest anchor no longer depends on the LLM.
   */
  private primaryTypeSymbol(type: Type): string | undefined {
    const name = this.anchorSymbolOf(type)?.getName();
    // Reject the compiler's synthetic names for anonymous shapes (`__type`,
    // `__object`, `__function`, …). They are not user-facing type names, so
    // they would be a meaningless — and non-resolvable — anchor.
    if (!name || name.startsWith('__') || BUILTIN_ANCHOR_SYMBOLS.has(name)) {
      return undefined;
    }
    return name;
  }

  /**
   * Declaration file (absolute path) of the anchor symbol
   * `primaryTypeSymbol` reports for this type, or `undefined` when the type
   * has no user-facing anchor or no source declaration. Every path that
   * reports the symbol reports this beside it (carrick#1819): it is where a
   * reader finds the type an anchor names. The scanner's pub/sub two-anchor
   * arbitration (carrick#413) also uses it to re-aim a demoted explicit
   * bundle request: the bundler resolves a `SymbolRequest` only against
   * declarations IN its `source_file`, so the request must point at the file
   * that actually declares the tsc-witnessed payload type.
   *
   * The declarations read are those of the symbol the anchor is named from
   * (`anchorSymbolOf`: the type's own symbol, or its alias's), so a name
   * imported through a barrel reports the file that declares it, and a name
   * two files declare reports the one this type resolves to. A declaration in
   * an installed package or a TypeScript lib is reported as it is found, as
   * an absolute path; what a reader is shown for it is the scanner's call.
   *
   * Only declaration kinds the bundler's `validateSymbols` can resolve
   * (interface, type alias, class, enum, function, variable) count. A
   * symbol's declaration list can also contain re-export machinery — a
   * barrel's `ExportSpecifier` (`export { Foo } from './foo'`) points at a
   * file that does not DECLARE the type, and a request re-aimed there would
   * fail validation, turning a resolvable explicit type into `unknown`.
   * With no declaring node, no source is reported and the arbitration
   * fails closed to the explicit anchor.
   */
  private primaryTypeSymbolSource(type: Type): string | undefined {
    if (this.primaryTypeSymbol(type) === undefined) {
      return undefined;
    }
    const decls = this.anchorSymbolOf(type)?.getDeclarations() ?? [];
    const declaring = decls.find(
      (d) =>
        Node.isInterfaceDeclaration(d) ||
        Node.isTypeAliasDeclaration(d) ||
        Node.isClassDeclaration(d) ||
        Node.isEnumDeclaration(d) ||
        Node.isFunctionDeclaration(d) ||
        Node.isVariableDeclaration(d)
    );
    return declaring?.getSourceFile().getFilePath();
  }

  /**
   * The anchor a `satisfies X` / `as X` / `<X>` annotation states IN SOURCE:
   * the name written at the annotation, where its type is declared, and the
   * array levels wrapped around it (`satisfies Order[]` → `Order`, depth 1).
   *
   * carrick#768: reading the anchor off the RESOLVED type works for an
   * interface, whose type carries its own symbol, and fails for
   *
   *     export type OrderBody = Inferred<typeof OrderSchema>;
   *
   * because the alias resolves to an instantiated type TypeScript keeps no
   * alias symbol on, so `getSymbol()` answers the synthetic `__type`. The
   * route then published a correct shape with no name, and
   * `primary_type_symbol` is precisely how a reader gets from a route to the
   * type its consumer imports. The annotation names it either way, so read
   * the name from the AST and confirm it against the declaration it resolves
   * to. This is the fallback for a resolved type that anchored nothing, never
   * an override of one that did.
   *
   * Four things are rejected, so a name only anchors when it is genuinely the
   * contract a consumer would import:
   *  - a generic instantiation (`satisfies Envelope<Order>`) names the
   *    WRAPPER, and a bare `Envelope` is not the payload;
   *  - a name resolving to no type declaration — an import of a value, a
   *    re-export the checker cannot follow — is not importable as a type;
   *  - a declaration in the TypeScript default library, and every
   *    `BUILTIN_ANCHOR_SYMBOLS` name, which describe machinery not contracts;
   *  - an inline annotation (`satisfies { id: string }`), which names nothing.
   */
  private writtenAnchorOf(
    typeNode: Node
  ): { symbol: string; source: string; depth: number } | undefined {
    const MAX_ANCHOR_ARRAY_DEPTH = 10;
    let current = typeNode;
    let depth = 0;
    while (Node.isArrayTypeNode(current) && depth < MAX_ANCHOR_ARRAY_DEPTH) {
      current = current.getElementTypeNode();
      depth++;
    }

    if (!Node.isTypeReference(current)) return undefined;
    // A type reference carrying arguments names the wrapper, not the payload.
    if (current.getTypeArguments().length > 0) return undefined;

    const entityName = current.getTypeName();
    // `ns.Order` states `Order`; the qualifier is the module, not the type.
    const identifier = Node.isQualifiedName(entityName)
      ? entityName.getRight()
      : entityName;
    const name = identifier.getText();
    if (!name || name.startsWith('__') || BUILTIN_ANCHOR_SYMBOLS.has(name)) {
      return undefined;
    }

    const symbol = identifier.getSymbol();
    if (!symbol) return undefined;
    const resolved = symbol.getAliasedSymbol() ?? symbol;
    const declaration = resolved
      .getDeclarations()
      .find(
        (d) =>
          Node.isTypeAliasDeclaration(d) ||
          Node.isInterfaceDeclaration(d) ||
          Node.isClassDeclaration(d) ||
          Node.isEnumDeclaration(d)
      );
    if (!declaration) return undefined;

    const sourceFile = declaration.getSourceFile();
    if (sourceFile.compilerNode.hasNoDefaultLib) return undefined;

    return { symbol: name, source: sourceFile.getFilePath(), depth };
  }

  /**
   * Peel array levels off a resolved type: `TimelineEvent[]` → element
   * `TimelineEvent`, depth 1. An array type's own symbol is `Array` (builtin,
   * filtered), so without this a `T[]` payload has NO anchor and — worse — an
   * explicit anchor bundled for the same alias silently drops the array-ness
   * (#306: array-vs-scalar scored compatible). The element drives the anchor
   * symbol; the depth is reported on the `InferredType` so the bundler's
   * existing `array_depth` wrap (#248) can restore the `[]` levels on the
   * explicit bundle. Depth is capped at the bundler's sane ceiling; a deeper
   * type is treated as depth 0 rather than a runaway loop.
   */
  private unwrapArrayLevels(type: Type): { element: Type; depth: number } {
    const MAX_ANCHOR_ARRAY_DEPTH = 10;
    let element = type;
    let depth = 0;
    while (depth < MAX_ANCHOR_ARRAY_DEPTH) {
      const el = element.getArrayElementType();
      if (!el) {
        return { element, depth };
      }
      element = el;
      depth++;
    }
    return { element: type, depth: 0 };
  }

  // ===========================================================================
  // Node Finding
  // ===========================================================================

  // ===========================================================================
  // Text-Based Node Resolution (Gemini expression text + line)
  // ===========================================================================

  /**
   * Resolve the target node using the best available locator:
   * 1. If span_start + span_end present → findNodeAtSpan (SWC byte offsets)
   * 2. If expression_text present → findNodeByText (Gemini text + line)
   * 3. Otherwise → undefined
   */
  /**
   * carrick#695: the type of the RECEIVER of a member call, plus the two facts
   * a role decision needs about it — which package declares that type, and what
   * the invoked member returns.
   *
   * Nothing here classifies. A member call with a route-shaped literal
   * (`x.verb("/lit", arg)`) is a route registration or a request depending on
   * what `x` IS, and that is the only question this answers. The Rust driver
   * maps the declaring package onto the detected framework / data-fetcher
   * lists; the compiler layer names no library and applies no shape rule.
   *
   * A receiver that does not resolve reports its top type (`any`) with no
   * package, so the caller can tell "unresolved" from "declared here": on a
   * checkout with no installed dependencies EVERY dependency-typed receiver is
   * `any`, and treating that as workspace-owned would invent a role.
   */
  private inferReceiverType(
    sourceFile: SourceFile,
    request: InferRequestItem
  ): InferredType | null {
    const call = this.resolveTargetCallExpression(sourceFile, request);
    if (!call) {
      return null;
    }

    const callee = call.getExpression();
    // The receiver exists only for a MEMBER call. A bare `fn("/lit", arg)`
    // has no receiver to classify and is not this kind's question.
    if (!Node.isPropertyAccessExpression(callee) && !Node.isElementAccessExpression(callee)) {
      return null;
    }

    const receiver = callee.getExpression();
    const receiverType = receiver.getType();
    const typeString = this.namedTypeLabel(receiverType, receiver);

    return {
      ...this.createInferredType(
        request,
        typeString,
        false,
        this.getNodeLocation(receiver)
      ),
      declaring_package: this.declaringPackageOf(receiverType),
      member_return_type: this.memberReturnTypeOf(call, receiver),
    };
  }

  /**
   * The npm package name that declares a type. `undefined` when the type has
   * no declaration to read (a top type, a primitive, an anonymous object
   * literal) or when nothing owns its declaration but the workspace itself.
   * The declaration is the attributed one (carrick#2031), and the package is
   * named by the module graph first, then by the path (`installedPackageOf`).
   */
  private declaringPackageOf(type: Type): string | undefined {
    const symbol = type.getSymbol() ?? type.getAliasSymbol();
    const declaration = attributedDeclaration(
      () => this.project.getProgram().compilerObject,
      (symbol?.getDeclarations() ?? []).map((node) => node.compilerNode)
    );
    if (!declaration) {
      return undefined;
    }
    return installedPackageOf(declaration.getSourceFile().fileName, this.packageOf);
  }

  /**
   * The awaited return type of the member invoked on the receiver. Reported as
   * a fact next to the receiver; a caller that classified on it alone would be
   * back to a shape rule (any async helper taking a path would match).
   */
  private memberReturnTypeOf(call: CallExpression, receiver: Node): string | undefined {
    const signature = call.getReturnType();
    if (!signature) {
      return undefined;
    }
    const awaited = this.unwrapPromiseType(signature);
    return this.namedTypeLabel(awaited, receiver);
  }

  /**
   * A type's own name when it has one, else its printed form.
   *
   * The printed form of a type whose declaring module the use site does not
   * import is `import("/abs/path/to/module").Name` — an absolute path that
   * would be persisted and rendered. The name alone is both stable and the
   * part a reader (or a package-list lookup) uses; anonymous types, top types
   * and primitives have no name and keep their printed text.
   */
  private namedTypeLabel(type: Type, enclosingNode: Node): string {
    const symbol = type.getSymbol() ?? type.getAliasSymbol();
    const name = symbol?.getName();
    if (name && !name.startsWith('__') && name !== 'unknown') {
      return name;
    }
    return typeText(type, enclosingNode);
  }

  /**
   * A type's own name, or `an unnamed branch` when it has none.
   *
   * Unlike `namedTypeLabel` this never falls back to the printed form, because
   * its output reaches a reader as a provenance `detail`, and the printed form
   * of a type whose declaring module the use site does not import is
   * `import("/abs/path").Name`. The bar for a detail is "one scrubbed sentence,
   * never an absolute path", so an anonymous branch is described rather than
   * printed.
   */
  private pathFreeTypeLabel(type: Type): string {
    const name = (type.getSymbol() ?? type.getAliasSymbol())?.getName();
    if (name && !name.startsWith('__') && name !== 'unknown') {
      return name;
    }
    return 'an unnamed branch';
  }

  private resolveTargetNode(
    sourceFile: SourceFile,
    request: InferRequestItem
  ): Node | undefined {
    if (request.span_start !== undefined && request.span_end !== undefined) {
      return this.findNodeAtSpan(sourceFile, request.span_start, request.span_end);
    }
    if (request.expression_text) {
      return this.findNodeByText(sourceFile, request.expression_text, request.expression_line);
    }
    return undefined;
  }

  /**
   * Resolve to a CallExpression using the best available locator.
   */
  private resolveTargetCallExpression(
    sourceFile: SourceFile,
    request: InferRequestItem
  ): CallExpression | undefined {
    if (request.span_start !== undefined && request.span_end !== undefined) {
      return this.findCallExpressionAtSpan(sourceFile, request.span_start, request.span_end);
    }
    if (request.expression_text) {
      return this.findCallExpressionByText(sourceFile, request.expression_text, request.expression_line);
    }
    return undefined;
  }

  /**
   * Resolve to a containing function using the best available locator.
   */
  private resolveContainingFunction(
    sourceFile: SourceFile,
    request: InferRequestItem
  ): FunctionLike | undefined {
    if (request.span_start !== undefined && request.span_end !== undefined) {
      return this.findContainingFunctionBySpan(sourceFile, request.span_start, request.span_end);
    }
    if (request.expression_text) {
      const node = this.findNodeByText(sourceFile, request.expression_text, request.expression_line);
      if (!node) return undefined;
      return this.findContainingFunctionForNode(node);
    }
    // Signature requests carry only the function's start line (FunctionDefinition
    // has no byte span). Fall back to locating the function by that line.
    return this.findFunctionByLine(sourceFile, request.line_number);
  }

  /**
   * Find the function whose declaration starts at (or within a couple of lines
   * of) the given line. Used for signature inference, where the only locator is
   * the function's start line as recorded by the scanner. Ties break toward the
   * innermost (smallest) function. Returns undefined if nothing is close enough,
   * to avoid binding to an unrelated function.
   *
   * The tolerance is one-directional in effect (carrick#766). Looking BACK is
   * free: a function starting before the anchor is one the anchor sits inside
   * or just after, which is what an anchor recorded on a signature or body line
   * means. Looking FORWARD is how the anchor reaches a handler that starts a
   * line or two into the registration or binding it names — and it is also how,
   * unguarded, an anchor landing on a statement that declares no function
   * (`export { action, loader };`) was answered by the NEXT declaration in the
   * file. That answer is silent: nothing about it says it came from a
   * neighbouring helper.
   *
   * So a forward candidate must be one the anchor line's own statement leads
   * to: rejected when a statement begins at or after the anchor line and does
   * not contain it. Nothing but trivia may sit between the anchor and the
   * function it is taken to name. With no candidate left the caller abstains,
   * which is the honest answer and the one a consumer check can act on.
   */
  private findFunctionByLine(
    sourceFile: SourceFile,
    line: number
  ): FunctionLike | undefined {
    return functionAtLine(sourceFile, line);
  }

  /**
   * Walk up from a node to find its innermost containing function.
   */
  private findContainingFunctionForNode(node: Node): FunctionLike | undefined {
    return node.getFirstAncestor(
      (n): n is FunctionLike =>
        Node.isFunctionDeclaration(n) ||
        Node.isArrowFunction(n) ||
        Node.isFunctionExpression(n) ||
        Node.isMethodDeclaration(n)
    );
  }

  // ===========================================================================
  // Route-Registration Handler Resolution
  // ===========================================================================

  /**
   * Follow a LINE-ONLY route-registration anchor to its handler function. The
   * scanner sends a bare line number (no span/text) for a named-handler
   * registration whose handler is declared away from the registration line
   * (`InferLocator::Line`). Find a registration node — a call or object literal
   * — that STARTS on that line and whose handler resolves, and return it.
   * Structural (call with a function-typed arg, or handler-shaped object-literal
   * property), no framework name-lists. Used only when the primary locator did
   * not resolve a node, so no existing path changes.
   */
  private handlerAtLine(
    sourceFile: SourceFile,
    line: number
  ): FunctionLike | undefined {
    return this.registrationAtLine(sourceFile, line)?.handler;
  }

  /**
   * The route registration on `line` together with the handler it references.
   *
   * `handlerAtLine` returns only the handler, but the route's DECLARED contract
   * (a validation-schema object passed alongside the handler) lives on the
   * registration node itself, so the schema anchors need both halves.
   */
  private registrationAtLine(
    sourceFile: SourceFile,
    line: number
  ): { registration: Node; handler: FunctionLike } | undefined {
    for (const entry of descendantsOnLine(sourceFile, line)) {
      if (
        entry.kind === SyntaxKind.CallExpression ||
        entry.kind === SyntaxKind.ObjectLiteralExpression
      ) {
        const node = wrap(sourceFile, entry);
        const handler = this.resolveRegisteredHandler(node);
        if (handler) {
          return { registration: node, handler };
        }
      }
    }
    return undefined;
  }

  /**
   * Follow a route-registration locator ONE indirection to the handler function.
   *
   * The scanner points the infer request at the registration site, but the
   * type-bearing expression (the handler's return, or its first request read)
   * lives inside the handler function, which the registration only *references*.
   * This resolves that reference to the handler's `FunctionLike` node, purely
   * structurally, in three shapes:
   *
   *  1. INLINE — the node is (or is inside) a call whose arguments include an
   *     arrow/function expression: `app.post('/x', async (c) => { … })`. The
   *     inline function IS the handler.
   *  2. OBJECT-LITERAL HANDLER PROPERTY — the node is (or is inside) an object
   *     literal with a `handler`-like property whose value is either an inline
   *     function or an identifier bound to a function:
   *     `{ method, path, handler: healthCheckHandler }`. Resolve that value.
   *  3. IDENTIFIER ARGUMENT — the node is (or is inside) a call whose 2nd+
   *     argument is an identifier bound to a function:
   *     `app.get('/x', healthCheckHandler)`. Resolve that identifier.
   *
   * No framework name, method name, or property name beyond the generic
   * `handler`-shaped key is hardcoded — the match is on node SHAPE (call with a
   * function-typed arg / object literal with a function-valued property), which
   * is what "route registration" structurally is across Express, Fastify, Hono,
   * Koa, a hand-rolled registry array, etc. Returns undefined when the locator
   * is not a registration shape, so every existing non-registration path is
   * untouched.
   */
  private resolveRegisteredHandler(node: Node): FunctionLike | undefined {
    // 1 & 3: the node is, or is inside, a route-registration CALL.
    const call = Node.isCallExpression(node)
      ? node
      : node.getFirstAncestorByKind(SyntaxKind.CallExpression);
    if (call) {
      const fromCall = this.handlerFromRegistrationCall(call);
      if (fromCall) {
        return fromCall;
      }
    }

    // 2: the node is, or is inside, an object literal carrying the handler.
    const objectLiteral = Node.isObjectLiteralExpression(node)
      ? node
      : node.getFirstAncestorByKind(SyntaxKind.ObjectLiteralExpression);
    if (objectLiteral) {
      const fromObject = this.handlerFromObjectLiteral(objectLiteral);
      if (fromObject) {
        return fromObject;
      }
    }

    return undefined;
  }

  /**
   * Request-path gate for handler-following: return the handler function ONLY
   * when the located node is ITSELF (after expression-unwrap) the registration
   * — a call whose arguments include a handler function, or a route-registry
   * object literal with a handler property. Unlike `resolveRegisteredHandler`,
   * this does NOT walk up to an ancestor call, so a consumer's inner
   * `JSON.stringify(payload)` (nested inside a `fetch(...)` that may carry other
   * callbacks) is never mistaken for a registration and the existing consumer
   * request paths stay exactly as they were.
   */
  private registrationHandlerAt(located: Node): FunctionLike | undefined {
    const node = this.unwrapExpressionNode(located);

    if (Node.isCallExpression(node)) {
      return this.handlerFromRegistrationCall(node);
    }

    if (Node.isObjectLiteralExpression(node)) {
      return this.handlerFromObjectLiteral(node);
    }

    return undefined;
  }

  /**
   * The handler function referenced by a route-registration call: the LAST
   * argument that is an inline function, or an identifier bound to a function
   * (carrick#2472 P3-2b). The functions before it are middleware
   * (`route(path).post(auth, audit, handler)`), which runs first and hands on;
   * reading the first function read a middleware as the route. The path
   * literal is not function-valued, so it is skipped naturally. No callee-name
   * check: a call whose argument resolves to a function is structurally a
   * registration regardless of the framework.
   */
  private handlerFromRegistrationCall(
    call: CallExpression
  ): FunctionLike | undefined {
    for (const arg of [...call.getArguments()].reverse()) {
      const handler = this.asHandlerFunction(arg);
      if (handler) {
        return handler;
      }
    }
    return undefined;
  }

  /**
   * The handler function carried by a route-registry object literal. Matches a
   * property whose name is `handler`-shaped (case-insensitive `handler`) and
   * whose value is an inline function or an identifier bound to a function.
   */
  private handlerFromObjectLiteral(
    objectLiteral: Node
  ): FunctionLike | undefined {
    if (!Node.isObjectLiteralExpression(objectLiteral)) {
      return undefined;
    }
    for (const prop of objectLiteral.getProperties()) {
      if (!Node.isPropertyAssignment(prop)) {
        continue;
      }
      const name = prop.getName().replace(/['"]/g, '').toLowerCase();
      if (name !== 'handler') {
        continue;
      }
      const initializer = prop.getInitializer();
      const handler = initializer ? this.asHandlerFunction(initializer) : undefined;
      if (handler) {
        return handler;
      }
    }
    return undefined;
  }

  /**
   * Resolve a node to a handler `FunctionLike`: an inline arrow/function
   * expression is returned directly; an identifier is followed to its binding
   * declaration (via the compiler's definition nodes) and returned when that
   * declaration is — or initializes to — a function. Anything else (a path
   * literal, an object, a non-function binding) yields undefined.
   * `followWrapperCalls` false reads a binding to a wrapper call as no
   * function, so a name the wrapper walk meets is never walked again.
   */
  private asHandlerFunction(node: Node, followWrapperCalls = true): FunctionLike | undefined {
    const expr = this.unwrapExpressionNode(node);

    if (Node.isArrowFunction(expr) || Node.isFunctionExpression(expr)) {
      return expr;
    }

    if (Node.isIdentifier(expr)) {
      for (const def of expr.getDefinitionNodes()) {
        const func = this.functionFromDeclaration(def, followWrapperCalls);
        if (func) {
          return func;
        }
      }
    }

    return undefined;
  }

  /**
   * Extract a `FunctionLike` from a declaration node the compiler resolved an
   * identifier to: a function declaration is itself the function; a variable
   * declaration / binding whose initializer is an inline function yields that
   * function (`const h = async () => { … }`), and one whose initializer is a
   * wrapper call yields the function the call wraps
   * (`const h = wrap<Contract>(async (req, res) => { … })`, carrick#2472
   * P3-2b; `functionWrappedBy`). Import/re-export shims are walked by
   * ts-morph's `getDefinitionNodes`, so no manual import chasing is needed.
   */
  private functionFromDeclaration(
    decl: Node,
    followWrapperCalls = true
  ): FunctionLike | undefined {
    if (Node.isFunctionDeclaration(decl)) {
      return decl;
    }

    if (Node.isArrowFunction(decl) || Node.isFunctionExpression(decl)) {
      return decl;
    }

    // `getDefinitionNodes()` may return the name identifier of a `const h = …`
    // binding rather than the declaration; step up to the variable declaration
    // it NAMES. Only that one step: a parameter of `const h = (input) => …` also
    // has `h` as an ancestor, and climbing to it made the parameter read as the
    // function it is declared inside (carrick#1162).
    const parent = decl.getParent();
    const varDecl = Node.isVariableDeclaration(decl)
      ? decl
      : parent && Node.isVariableDeclaration(parent) && parent.getNameNode() === decl
        ? parent
        : undefined;
    if (varDecl) {
      const initializer = varDecl.getInitializer();
      if (initializer) {
        const inner = this.unwrapExpressionNode(initializer);
        if (Node.isArrowFunction(inner) || Node.isFunctionExpression(inner)) {
          return inner;
        }
        if (followWrapperCalls && Node.isCallExpression(inner)) {
          return this.functionWrappedBy(inner, 0);
        }
      }
    }

    return undefined;
  }

  /**
   * The function a call wraps, for a name bound to that call and used as a
   * handler (carrick#2472 P3-2b): the call's one argument that is a function
   * literal, a name bound to one, or another such call, a few calls deep.
   *
   * Two conditions keep this to wrappers. The call's own value has to be
   * callable, because the name is used as a function: `rows.map((row) => …)`
   * is handed a function and yields a list. And exactly one argument may be a
   * function: with two there is no saying which of them is wrapped.
   */
  private functionWrappedBy(call: CallExpression, depth: number): FunctionLike | undefined {
    if (depth >= WRAPPER_CALL_MAX_DEPTH) return undefined;
    if (!this.isCallableType(call.getType())) return undefined;
    const wrapped: FunctionLike[] = [];
    for (const arg of call.getArguments()) {
      const value = this.unwrapExpressionNode(arg);
      const func =
        Node.isArrowFunction(value) || Node.isFunctionExpression(value)
          ? value
          : Node.isCallExpression(value)
            ? this.functionWrappedBy(value, depth + 1)
            : Node.isIdentifier(value)
              ? this.asHandlerFunction(value, false)
              : undefined;
      if (func) wrapped.push(func);
    }
    return wrapped.length === 1 ? wrapped[0] : undefined;
  }

  /**
   * True when `handler` reads nothing from the incoming message (carrick#827):
   * every use of one of its parameters, named or destructured, is the receiver
   * of a call chain whose value the handler discards or returns
   * (`reply.json(result)`, `return c.status(201).json(result)`). That is a
   * send. A returned chain must end in a call that takes an argument, so a
   * read handed straight back (`return request.json()`) is not one.
   *
   * Anything else may be a read of the body and makes this false: a property
   * read, a call whose value is used, a parameter handed whole to another
   * function, or a call in the chain handed an argument that can receive what
   * the parameter gives (`argumentCanReceive`: a callback, an object with a
   * method), as in `request.pipe(dest)`, `request.on('data', cb)` and
   * `request.json().then(cb)`. No method or property name is consulted. A
   * handler with no parameter, or one that uses `this`, proves nothing: it may
   * read the message from `this` or a closure.
   */
  private handlerReadsNothingFromItsParameters(handler: FunctionLike): boolean {
    // A declared `this` parameter, or any `this` in the body, is a way in the
    // walk below does not follow.
    const parameters = new Set<Node>(
      handler.getParameters().filter((parameter) => parameter.getName() !== 'this')
    );
    if (parameters.size === 0) return false;
    const body = handler.getBody();
    if (!body) return false;
    if (body.getFirstDescendantByKind(SyntaxKind.ThisKeyword)) return false;
    const namesAParameter = (identifier: Node): boolean =>
      (identifier.getSymbol()?.getDeclarations() ?? []).some((declaration) => {
        const parameter = Node.isParameterDeclaration(declaration)
          ? declaration
          : Node.isBindingElement(declaration)
            ? declaration.getFirstAncestorByKind(SyntaxKind.Parameter)
            : undefined;
        return parameter !== undefined && parameters.has(parameter);
      });
    for (const identifier of body.getDescendantsOfKind(SyntaxKind.Identifier)) {
      if (!namesAParameter(identifier)) continue;
      // Climb the call chain the parameter is the root receiver of.
      let top: Node = identifier;
      let lastCall: CallExpression | undefined;
      for (;;) {
        const parent = top.getParent();
        if (!parent) break;
        if (Node.isPropertyAccessExpression(parent) && parent.getExpression() === top) {
          top = parent;
        } else if (Node.isCallExpression(parent) && parent.getExpression() === top) {
          // A send hands the parameter data; an argument able to receive may
          // be where the body goes.
          if (parent.getArguments().some((argument) => this.argumentCanReceive(argument))) {
            return false;
          }
          lastCall = parent;
          top = parent;
        } else if (
          (Node.isAwaitExpression(parent) ||
            Node.isParenthesizedExpression(parent) ||
            Node.isNonNullExpression(parent)) &&
          parent.getExpression() === top
        ) {
          top = parent;
        } else {
          break;
        }
      }
      // The chain has to end in a call: a property read yields a value.
      if (!lastCall || this.unwrapExpressionNode(top) !== lastCall) return false;
      const holder = top.getParent();
      const discarded = !!holder && Node.isExpressionStatement(holder);
      const returned =
        !!holder &&
        (Node.isReturnStatement(holder) || (holder === handler && handler.getBody() === top)) &&
        lastCall.getArguments().length > 0;
      if (!discarded && !returned) return false;
    }
    return true;
  }

  /**
   * True unless `argument` is proven to be data that cannot receive anything
   * from the call it is handed to (carrick#827). Data is a primitive or literal,
   * `null`/`undefined`, or an object with no call or construct signature and
   * no callable member; an array of such, or a union all of whose members are.
   * A callback, an object with a method (a stream, an emitter), or a type the
   * compiler cannot state (`any`, `unknown`, a type parameter) can receive.
   * Sibling of `callResultIsPlainData`, which reads a call's result and so
   * rejects primitives and empty objects; an argument of either is data.
   */
  private argumentCanReceive(argument: Node): boolean {
    const canReceive = (type: Type, depth: number): boolean => {
      if (depth > 3) return true;
      if (type.isAny() || type.isUnknown() || type.isTypeParameter()) return true;
      if (type.isUnion()) {
        return type.getUnionTypes().some((member) => canReceive(member, depth + 1));
      }
      if (type.isIntersection()) {
        return type.getIntersectionTypes().some((member) => canReceive(member, depth + 1));
      }
      const element = type.getArrayElementType();
      if (element) return canReceive(element, depth + 1);
      if (type.isTuple()) {
        return type.getTupleElements().some((member) => canReceive(member, depth + 1));
      }
      if (!type.isObject()) return false;
      if (this.isCallableType(type)) return true;
      const indexed = type.getStringIndexType() ?? type.getNumberIndexType();
      const callable = (held: Type): boolean =>
        held.isAny() ||
        (held.isUnion()
          ? held.getUnionTypes().some((member) => this.isCallableType(member))
          : this.isCallableType(held));
      if (indexed && callable(indexed)) return true;
      return type.getProperties().some((member) => callable(member.getTypeAtLocation(argument)));
    };
    return canReceive(argument.getType(), 0);
  }

  /**
   * Scan a handler function body for the FIRST request-body read that carries a
   * type, and return that type's structural text — or null when the body holds
   * no typed request read.
   *
   * "Typed request read" is matched on SHAPE, never on a callee/method name:
   *
   *  A. a CALL that carries an explicit type argument — `c.req.json<T>()`,
   *     `parseBody<T>(req)`; the first type argument is `T`; or
   *  B. an expression with an explicit type annotation or cast in its immediate
   *     binding context — `const b: T = …`, `req.body as T`.
   *
   * The body is walked in source order (`forEachDescendant` is a pre-order
   * traversal), so the FIRST such read wins, mirroring "the request type lives
   * at the first body read" without assuming which framework produced it.
   * Returns null (not a spurious type) for a genuinely payload-less handler.
   */
  private inferRequestReadFromHandler(func: FunctionLike): string | null {
    const body = func.getBody();
    if (!body) {
      return null;
    }

    let found: string | null = null;
    body.forEachDescendant((descendant, traversal) => {
      if (found !== null) {
        traversal.stop();
        return;
      }

      // A. call with an explicit type argument: `c.req.json<TrackRequest>()`.
      if (Node.isCallExpression(descendant)) {
        const typeArgs = descendant.getTypeArguments();
        if (typeArgs.length > 0) {
          const resolved = this.structuralTextFromTypeNode(typeArgs[0]);
          if (resolved) {
            found = resolved;
            traversal.stop();
            return;
          }
        }
      }

      // B. an `as T` cast (`req.body as TrackRequest`) or a typed binding
      // (`const b: TrackRequest = …`). Read the annotation/cast target.
      if (Node.isAsExpression(descendant)) {
        const typeNode = descendant.getTypeNode();
        const resolved = typeNode
          ? this.structuralTextFromTypeNode(typeNode)
          : null;
        if (resolved) {
          found = resolved;
          traversal.stop();
          return;
        }
      }
    });

    return found;
  }

  /**
   * The call in a handler that reads the request's body off the platform
   * request the handler was handed, or `undefined` (carrick#807).
   *
   * Read by shape, with no method name consulted:
   *
   *  - a call that takes nothing, on a member of a value whose type is request
   *    machinery (`typeIsFrameworkMachinery`: declared by the platform or an
   *    installed library, and carrying its body readers);
   *  - that value is rooted at one of the handler's OWN parameters, named or
   *    destructured. A response read off an outbound call inside the handler
   *    (`(await upstream.json()) as Rate`) has the same shape one variable
   *    away, and is the opposite side of a different exchange;
   *  - and the call's result, awaited, is `any` or `unknown`: the platform's
   *    untyped parse. A read that states its own type (`formData()`, a typed
   *    `json<T>()`) is not this shape and is left to the readers that
   *    already handle it.
   *
   * The first such call in source order: a body is read once.
   */
  private platformBodyReadIn(handler: FunctionLike): CallExpression | undefined {
    const parameters = new Set<Node>(handler.getParameters());
    const rootedAtParameter = (expression: Node): boolean => {
      let root = expression;
      while (
        Node.isPropertyAccessExpression(root) ||
        Node.isNonNullExpression(root) ||
        Node.isParenthesizedExpression(root)
      ) {
        root = root.getExpression();
      }
      if (!Node.isIdentifier(root)) return false;
      return (root.getSymbol()?.getDeclarations() ?? []).some((declaration) => {
        const parameter = Node.isParameterDeclaration(declaration)
          ? declaration
          : Node.isBindingElement(declaration)
            ? declaration.getFirstAncestorByKind(SyntaxKind.Parameter)
            : undefined;
        return parameter !== undefined && parameters.has(parameter);
      });
    };

    for (const call of handler.getDescendantsOfKind(SyntaxKind.CallExpression)) {
      if (call.getArguments().length > 0) continue;
      const callee = call.getExpression();
      if (!Node.isPropertyAccessExpression(callee)) continue;
      const receiver = callee.getExpression();
      if (!rootedAtParameter(receiver)) continue;
      const yielded = this.unwrapPromiseType(call.getType());
      if (!yielded.isAny() && !yielded.isUnknown()) continue;
      if (!this.typeIsFrameworkMachinery(receiver.getType())) continue;
      return call;
    }
    return undefined;
  }

  /**
   * The platform body read whose result `located` is a MEMBER of, or
   * `undefined` (carrick#2057).
   *
   * A member is a property or element access off a binding, or a name
   * destructured out of one. The binding must be initialised by the body read
   * `platformBodyReadIn` finds in the function that declares it, with only
   * transparent wrappers between the two (`await`, parentheses, `!`, a cast,
   * `satisfies`). The binding itself is not a member, and neither is a member
   * of anything else (a query object, a parameter, an outbound response).
   */
  private parsedBodyReadHolding(located: Node): CallExpression | undefined {
    let node = this.unwrapExpressionNode(located);
    let member = false;
    while (
      Node.isPropertyAccessExpression(node) ||
      Node.isElementAccessExpression(node) ||
      Node.isNonNullExpression(node) ||
      Node.isParenthesizedExpression(node)
    ) {
      if (Node.isPropertyAccessExpression(node) || Node.isElementAccessExpression(node)) {
        member = true;
      }
      node = node.getExpression();
    }
    if (!Node.isIdentifier(node)) return undefined;

    let declaration: Node | undefined = node.getSymbol()?.getDeclarations()?.[0];
    while (declaration && Node.isBindingElement(declaration)) {
      member = true;
      declaration = declaration.getParent()?.getParent();
    }
    if (!member || !declaration || !Node.isVariableDeclaration(declaration)) return undefined;

    const handler = this.findContainingFunctionForNode(declaration);
    const read = handler ? this.platformBodyReadIn(handler) : undefined;
    if (!read) return undefined;
    let current: Node = read;
    for (;;) {
      const parent = current.getParent();
      if (!parent) return undefined;
      if (parent === declaration) {
        return declaration.getInitializer() === current ? read : undefined;
      }
      if (!this.wrapsReadTransparently(parent)) {
        return undefined;
      }
      current = parent;
    }
  }

  /**
   * The request contract the source states AT a body read, or null when it
   * states none there (carrick#807).
   *
   * Three statements count, each of them on the read itself:
   *
   *  - a cast on it: `(await request.json()) as NewWidget`;
   *  - the annotation of the binding it initialises:
   *    `const input: NewWidget = await request.json()`;
   *  - a type the handler states for the read further on
   *    (`statedInFunction`): a schema it is handed to, the annotation of a
   *    name it is assigned to, a pass-through function, a cast of the whole
   *    value.
   *
   * The first two are `typeNodeStatedAtRead`, the reading a consumer's
   * response read already gets. An annotation further out is not one of them:
   * `const saved: Saved = await save(await request.json())` types what `save`
   * returned, and reading it as the body would publish the wrong side of the
   * handler. Nor is a placeholder (`as unknown`, `Record<string, unknown>`).
   */
  private requestStatedAtBodyRead(
    request: InferRequestItem,
    read: CallExpression
  ): InferredType | null {
    const statedNode = this.typeNodeStatedAtRead(read);
    const statedText = statedNode ? this.structuralTextFromTypeNode(statedNode) : null;
    if (statedText) {
      this.log(
        `Request at ${request.file_path}:${request.line_number} is the body its handler ` +
          'reads off the platform request, as the source types that read'
      );
      return this.createInferredType(request, statedText, true, this.getNodeLocation(read));
    }

    const func = this.findContainingFunctionForNode(read);
    const validated = func
      ? this.statedInFunction(read, func, { returned: false, parameters: false, skipFailurePaths: false })
      : null;
    if (validated) {
      this.log(
        `Request at ${request.file_path}:${request.line_number} is the body its handler ` +
          `reads off the platform request and states a type for (${validated.how}); publishing that type`
      );
      return this.declaredRequestInferredType(request, validated, this.getNodeLocation(read));
    }
    return null;
  }

  /**
   * Resolve a type-annotation/type-argument node to fully-structural text,
   * dropping any `Promise<…>` wrapper (`c.req.json<T>()` returns `Promise<T>`,
   * but the annotation node is `T` directly; the guard is harmless either way).
   * Returns null when the node resolves to a useless/library type that carries
   * no member shape, so the caller keeps scanning rather than locking onto
   * `any`/`unknown`.
   */
  private structuralTextFromTypeNode(typeNode: Node): string | null {
    try {
      const resolved = this.unwrapPromiseType(typeNode.getType());
      if (this.isCallableType(resolved)) {
        return null;
      }
      const bare = typeText(resolved, typeNode);
      if (this.isUselessType(bare)) {
        return null;
      }
      return this.expandResolvedTypeStructural(resolved, bare);
    } catch {
      return null;
    }
  }

  /**
   * Same reduction as `structuralTextFromTypeNode`, but starting from a resolved
   * `Type` rather than a syntax node. `at` only supplies the scope the type is
   * rendered in.
   */
  private structuralTextFromType(type: Type, at: Node): string | null {
    try {
      const resolved = this.unwrapPromiseType(type);
      if (this.isCallableType(resolved)) {
        return null;
      }
      const bare = typeText(resolved, at);
      if (this.isUselessType(bare)) {
        return null;
      }
      const expanded = this.expandResolvedTypeStructural(resolved, bare);
      return this.isUselessType(expanded) ? null : expanded;
    } catch {
      return null;
    }
  }

  // ===========================================================================
  // Route-contract anchors (carrick#528)
  // ===========================================================================
  //
  // A route registration can DECLARE its request/response contract in two
  // places the payload locators never reach, because neither is an expression
  // the handler evaluates:
  //
  //  (a) the handler's own parameter annotation — a request parameter typed
  //      `T<{ Body: CreateWidget }>` exposes the contract as the `body` member
  //      of that parameter's type;
  //  (b) a validation-schema object passed alongside the handler — `{ schema:
  //      { body: ref('CreateWidget'), response: { 200: ref('Widget') } } }` —
  //      whose entries reference schema VALUES that declare their input and
  //      output types.
  //
  // Both are read structurally: (a) is "the `body` member of a parameter's
  // type", (b) is "a `schema` property on a registration argument, whose
  // entries resolve to a value that declares its types" (the body reads the
  // input, the response the output; carrick#1101). No
  // framework, library, or method-name list is involved — a framework whose
  // request object exposes `body` and whose route options carry `schema` is
  // read the same way regardless of which one it is.

  /**
   * The route registration a request's locator points AT, or undefined when it
   * points at anything else.
   *
   * Deliberately strict: the located node must itself be the registration (a
   * call registering a handler, or a route-registry object literal), never an
   * ancestor of it, so an expression inside a handler is not mistaken for the
   * route it belongs to. A locator that resolves nothing falls back to the
   * registration on the request's line.
   */
  private routeRegistrationForRequest(
    sourceFile: SourceFile,
    request: InferRequestItem
  ): Node | undefined {
    const located = this.resolveTargetNode(sourceFile, request);
    if (located) {
      const node = this.unwrapExpressionNode(located);
      return this.registrationHandlerAt(node) ? node : undefined;
    }
    return this.registrationAtLine(sourceFile, request.line_number)?.registration;
  }

  /**
   * Anchor (b) on the response side: the success-status contract declared in the
   * registration's schema, as an `InferredType` ready to return. Null when the
   * registration declares no resolvable response schema, so the caller falls
   * back to following the handler's return.
   */
  private declaredResponseInferredType(
    request: InferRequestItem,
    registration: Node
  ): InferredType | null {
    const declared = this.routeSchemaContract(registration, 'response');
    if (!declared) {
      return null;
    }
    this.log(
      `Route registration at ${request.file_path}:${request.line_number} declares its ` +
        'response schema; using the declared contract'
    );
    return this.createInferredType(
      request,
      declared.text,
      true,
      this.getNodeLocation(registration),
      undefined,
      undefined
    );
  }

  /**
   * The request contract of a route registration, in anchor order: the
   * handler's parameter annotation (a), the registration's declared schema (b),
   * then the first typed request read in the handler body (the pre-existing
   * behaviour). The first anchor yielding a non-useless type wins; null means
   * the route declares its request nowhere we can read.
   */
  private requestContractFromRegistration(
    registration: Node,
    handler: FunctionLike
  ): DeclaredContract | null {
    const declared = this.declaredRequestContract(registration, handler);
    if (declared) {
      return declared;
    }
    const read = this.inferRequestReadFromHandler(handler);
    return read ? { text: read } : null;
  }

  /**
   * carrick-cloud#1366, shape 1: the request contract of a parameter a KEYED
   * parameter decorator binds to one field of the body (`@Body('key') x: T`).
   *
   * The decorator is recognised by shape, not by name: a call decorator whose
   * first argument is a string literal. Every parameter of the same handler
   * that the SAME decorator binds by key contributes one member, so two keyed
   * reads of one body publish one object. A sibling the same decorator binds
   * WITHOUT a key (no argument, or a non-string one such as a pipe) receives
   * the whole body, so the contract is that sibling's type intersected with
   * the keyed members.
   *
   * Returns null when the located node is not a keyed parameter, so every
   * other request path is untouched.
   */
  private keyedBodyParamContract(located: Node): DeclaredContract | null {
    const param = this.parameterNamedBy(located);
    if (!param) return null;
    const binding = this.callDecoratorBindings(param).find((b) => b.key !== undefined);
    if (!binding) return null;

    const func = param.getParent();
    if (!func || !('getParameters' in func)) return null;
    const siblings = (func as FunctionLike).getParameters();

    const members: string[] = [];
    const seen = new Set<string>();
    let wholeBody: ParameterDeclaration | undefined;
    for (const sibling of siblings) {
      const same = this.callDecoratorBindings(sibling).filter((b) => b.name === binding.name);
      if (same.length === 0) continue;
      const keyed = same.find((b) => b.key !== undefined);
      if (!keyed || keyed.key === undefined) {
        wholeBody ??= sibling;
        continue;
      }
      if (seen.has(keyed.key)) continue;
      seen.add(keyed.key);
      const optional = sibling.hasQuestionToken() || sibling.hasInitializer();
      let type = sibling.getType();
      if (optional) type = type.getNonNullableType();
      const text = this.expandResolvedTypeStructural(type, typeText(type, sibling));
      const key = /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(keyed.key)
        ? keyed.key
        : JSON.stringify(keyed.key);
      members.push(`${key}${optional ? '?' : ''}: ${text};`);
    }
    if (members.length === 0) return null;
    const keyedText = `{ ${members.join(' ')} }`;
    if (!wholeBody) return { text: keyedText };
    const wholeType = wholeBody.getType();
    const wholeText = this.expandResolvedTypeStructural(wholeType, typeText(wholeType, wholeBody));
    const operand = /^\{[\s\S]*\}$/.test(wholeText) ? wholeText : `(${wholeText})`;
    return { text: `${operand} & ${keyedText}` };
  }

  /**
   * The handler parameter a request locator names: the parameter itself, its
   * name, a decorator on it, or an identifier whose declaration is one. A
   * member read of a parameter (`x.length`) is not the parameter and is left
   * to the expression path.
   */
  private parameterNamedBy(located: Node): ParameterDeclaration | undefined {
    if (Node.isParameterDeclaration(located)) return located;
    const decoratedParam = located.getFirstAncestor(
      (n): n is ParameterDeclaration =>
        Node.isParameterDeclaration(n) &&
        n.getDecorators().some((d) => d === located || d.containsRange(located.getPos(), located.getEnd()))
    );
    if (decoratedParam) return decoratedParam;
    if (!Node.isIdentifier(located)) return undefined;
    const parent = located.getParent();
    if (Node.isParameterDeclaration(parent) && parent.getNameNode() === located) {
      return parent;
    }
    for (const def of located.getDefinitionNodes()) {
      if (Node.isParameterDeclaration(def)) return def;
    }
    return undefined;
  }

  /**
   * Every call decorator on the parameter, with the string key its first
   * argument names (`@Body('key')` -> `{ name: 'Body', key: 'key' }`). A call
   * decorator with no argument, or a first argument that is not a string (a
   * pipe instance), binds without a key.
   */
  private callDecoratorBindings(
    param: ParameterDeclaration
  ): Array<{ name: string; key?: string }> {
    const bindings: Array<{ name: string; key?: string }> = [];
    for (const decorator of param.getDecorators()) {
      const call = decorator.getCallExpression();
      if (!call) continue;
      const first = call.getArguments()[0];
      if (first && (Node.isStringLiteral(first) || Node.isNoSubstitutionTemplateLiteral(first))) {
        bindings.push({ name: decorator.getName(), key: first.getLiteralValue() });
      } else {
        bindings.push({ name: decorator.getName() });
      }
    }
    return bindings;
  }

  /**
   * carrick-cloud#1366, shapes 2 and 3: a request locator that names the
   * request CALL, or a request CONFIG argument, instead of the body.
   *
   * The body is found from the callee's DECLARED signature:
   *
   *  - a body slot is a parameter named as a body (`data`, `body`, `json`)
   *    and typed by one of the signature's own type parameters (`data?: D`);
   *  - a config slot is a parameter typed by a generic config type whose
   *    body-named member is typed by the config's own type parameter
   *    (`config?: Config<D>` with `data?: D`). A generic config with no such
   *    member (its type parameter types something else, a response mode say)
   *    is opaque: nothing can be said about its body.
   *
   * A located CALL is taken as the request call only when its first argument
   * is string-typed (the URL) and one argument supplies a body through a slot;
   * anything else is left alone, so an awaited payload builder keeps its own
   * type. A located argument in a config slot yields the payload member: from
   * an object literal directly, from a variable through its type. A literal
   * that does not set the member means the call sends no body; a variable
   * whose type does not resolve the member, or an opaque config, abstains.
   */
  private requestBodyInCall(
    node: Node
  ):
    | { kind: 'none' }
    | { kind: 'abstain'; why: string }
    | { kind: 'body'; node: Node; from: string }
    | { kind: 'body_type'; type: Type; at: Node; from: string }
    | { kind: 'no_body'; member: string } {
    if (Node.isCallExpression(node)) {
      const args = node.getArguments();
      if (args.length === 1) return this.requestObjectBody(args[0]);
      if (args.length < 2) return { kind: 'none' };
      if (!this.isStringLikeType(args[0].getType())) return { kind: 'none' };
      const slots = this.requestBodySlots(node);
      for (let i = 1; i < args.length; i++) {
        const slot = slots[i];
        if (!slot || slot.kind === 'opaque_config') continue;
        if (slot.kind === 'body') {
          return { kind: 'body', node: args[i], from: 'the request call' };
        }
        const fromConfig = this.configArgumentBody(args[i], slot.member, 'the request call');
        if (fromConfig.kind === 'body' || fromConfig.kind === 'body_type') return fromConfig;
      }
      return { kind: 'none' };
    }

    if (Node.isObjectLiteralExpression(node) || Node.isIdentifier(node)) {
      const call = node.getParent();
      if (!call || !Node.isCallExpression(call)) return { kind: 'none' };
      const index = call.getArguments().indexOf(node);
      if (index < 1) return { kind: 'none' };
      const slot = this.requestBodySlots(call)[index];
      if (!slot || slot.kind === 'body') return { kind: 'none' };
      if (slot.kind === 'opaque_config') {
        return {
          kind: 'abstain',
          why: "the located argument is a request config whose declared type names no body member",
        };
      }
      return this.configArgumentBody(node, slot.member, "the call's request config");
    }

    return { kind: 'none' };
  }

  /**
   * carrick#2628: the located node is an argument of a call into a function
   * whose body the program holds (an in-repo client method,
   * `client.addTags(project, unique(tags))`), at a parameter that is not a
   * body slot. What the call sends is decided inside that function, so the
   * argument is the body only when the function sends it unchanged. One
   * level, read off each use of the parameter in the function's body:
   *
   *  - the use IS the value of a body-named member (`body: tags`,
   *    `body: JSON.stringify(tags)`) or of a body slot of a call: the
   *    argument is the body, and its own type stands (`none`);
   *  - the use sits inside such a value (`body: { tags }`,
   *    `body: (event ?? {}) as never`): the body is that value, read at the
   *    function without the casts around it;
   *  - the use sits inside an object or array literal that is no body value
   *    (`send(project, [labels])`): the body cannot be told, abstain.
   *
   * Uses that disagree abstain. A generic function's body may be typed by
   * its type parameters, so it abstains too. A library's declaration
   * (`fetch`, an installed client) has no body to read and is left alone,
   * and so is an untyped argument (a handler's `request.body` handed to a
   * helper): it states no contract to mistake, and the untyped-read path
   * below reads it.
   */
  private bodyBuiltByCallee(
    node: Node
  ):
    | { kind: 'none' }
    | { kind: 'abstain'; why: string }
    | { kind: 'body_type'; type: Type; at: Node; from: string } {
    const read = this.calleeBodyRead(node, 0);
    return read.kind === 'passes' ? { kind: 'none' } : read;
  }

  /**
   * `bodyBuiltByCallee`, telling apart a callee that sends the argument
   * unchanged (`passes`) from one that states nothing about it (`none`).
   * `depth` counts the forwarded locals followed (carrick#2671).
   */
  private calleeBodyRead(
    node: Node,
    depth: number
  ):
    | { kind: 'none' }
    | { kind: 'passes' }
    | { kind: 'abstain'; why: string }
    | { kind: 'body_type'; type: Type; at: Node; from: string } {
    const own = node.getType();
    if (own.isAny() || own.isUnknown()) return { kind: 'none' };
    const call = this.receivingCallOf(node, (candidate) => Node.isCallExpression(candidate));
    if (!call || !Node.isCallExpression(call)) return { kind: 'none' };
    const index = call
      .getArguments()
      .findIndex((a) => a.getPos() <= node.getPos() && node.getEnd() <= a.getEnd());
    if (index < 0) return { kind: 'none' };
    if (this.requestBodySlots(call)[index]) return { kind: 'none' };

    const callee = this.project.getTypeChecker().getResolvedSignature(call)?.getDeclaration();
    if (
      !callee ||
      !(
        Node.isFunctionDeclaration(callee) ||
        Node.isMethodDeclaration(callee) ||
        Node.isArrowFunction(callee) ||
        Node.isFunctionExpression(callee)
      )
    ) {
      return { kind: 'none' };
    }
    const body = callee.getBody();
    if (!body) return { kind: 'none' };
    const program = this.project.getProgram().compilerObject;
    if (
      isExternalOrigin(
        program,
        callee.getSourceFile().compilerNode,
        this.repoRoot,
        externalImportsOf(this.project)
      )
    ) {
      return { kind: 'none' };
    }
    const param = callee.getParameters()[index];
    if (!param || param.isRestParameter()) return { kind: 'none' };
    const paramName = param.getNameNode();
    if (!Node.isIdentifier(paramName)) return { kind: 'none' };
    const paramSymbol = paramName.getSymbol()?.compilerSymbol;
    if (!paramSymbol) return { kind: 'none' };

    const checker = this.project.getTypeChecker().compilerObject;
    const uses = body.getDescendantsOfKind(SyntaxKind.Identifier).filter((id) => {
      const parent = id.getParent();
      const symbol =
        parent && Node.isShorthandPropertyAssignment(parent)
          ? checker.getShorthandAssignmentValueSymbol(parent.compilerNode)
          : id.getSymbol()?.compilerSymbol;
      return symbol === paramSymbol;
    });

    const from = 'a call into a function that builds the request body inside itself';
    let passedThrough = false;
    const built: Node[] = [];
    for (const use of uses) {
      // carrick#2668: `data ? JSON.stringify(data) : undefined` tests the
      // argument before it sends it; the test is no body.
      if (this.inSendNothingTest(use, body)) continue;
      const value = this.bodyValueAround(use, body);
      if (value === use) {
        passedThrough = true;
        continue;
      }
      if (value) {
        built.push(value);
        continue;
      }
      // carrick#2671: the use builds a local (`const payload = { ...input,
      // code: norm(input.code) }`) the function then sends unchanged, itself
      // or through a helper that passes it on as the body. The body is that
      // local.
      const forwarded = this.forwardedLocalHolding(use, body, depth);
      if (forwarded) {
        built.push(forwarded);
        continue;
      }
      const ancestors = use.getAncestors();
      const insideLiteral = ancestors
        .slice(0, ancestors.indexOf(body))
        .some((a) => Node.isObjectLiteralExpression(a) || Node.isArrayLiteralExpression(a));
      if (insideLiteral) {
        return {
          kind: 'abstain',
          why: `the located argument goes into a value the called function builds and does not send as a body (${from})`,
        };
      }
    }
    if (built.length === 0) return passedThrough ? { kind: 'passes' } : { kind: 'none' };
    if (passedThrough) {
      return { kind: 'abstain', why: `the called function both sends the argument and builds a body from it (${from})` };
    }
    if (callee.getTypeParameters().length > 0) {
      return { kind: 'abstain', why: `the called function is generic, so the body it builds may be typed by its type parameters (${from})` };
    }
    const types = built.map((value) => this.sentValue(value));
    const texts = new Set(types.map((value) => typeText(value.getType(), value)));
    if (texts.size !== 1) {
      return { kind: 'abstain', why: `the called function builds more than one body from the argument (${from})` };
    }
    const at = types[0];
    const type = at.getType();
    if (type.isTypeParameter() || printedTypeHoldsTopType([...texts][0])) {
      return { kind: 'abstain', why: `the body the called function builds is not fully typed (${from})` };
    }
    return { kind: 'body_type', type, at, from };
  }

  /**
   * The request body value `use` is part of, inside `scope`: the value of a
   * body-named object member, or the argument at a body slot of a call,
   * looking through parentheses, casts, `await` and a `JSON.stringify`
   * around it. `use` itself when it is that value whole.
   */
  private bodyValueAround(use: Node, scope: Node): Node | undefined {
    let current: Node = use;
    while (current !== scope) {
      const parent = current.getParent();
      if (!parent) return undefined;
      if (
        Node.isShorthandPropertyAssignment(parent) &&
        current === use &&
        BODY_MEMBER_NAMES.has(parent.getName())
      ) {
        return use;
      }
      if (
        Node.isPropertyAssignment(parent) &&
        parent.getInitializer() === current &&
        BODY_MEMBER_NAMES.has(parent.getName())
      ) {
        return this.isSameValue(current, use) ? use : current;
      }
      if (Node.isCallExpression(parent)) {
        const index = parent.getArguments().indexOf(current);
        if (index >= 0 && this.requestBodySlots(parent)[index]?.kind === 'body') {
          return this.isSameValue(current, use) ? use : current;
        }
      }
      current = parent;
    }
    return undefined;
  }

  /**
   * carrick#2671: the `const` local whose initializer holds `use`, inside
   * `scope`, when `scope` sends that local unchanged as the body: whole in a
   * body slot, or as an argument into a function that passes it on as its
   * body. The local's name, so its declared type is read. Undefined when the
   * local is never sent, when a further body is built from it, or when a
   * function lies between `use` and the declaration.
   */
  private forwardedLocalHolding(use: Node, scope: Node, depth: number): Node | undefined {
    if (depth >= 2) return undefined;
    let current: Node = use;
    let declaration: VariableDeclaration | undefined;
    while (current !== scope) {
      const parent = current.getParent();
      if (!parent) return undefined;
      if (
        Node.isArrowFunction(parent) ||
        Node.isFunctionExpression(parent) ||
        Node.isFunctionDeclaration(parent) ||
        Node.isMethodDeclaration(parent)
      ) {
        return undefined;
      }
      if (Node.isVariableDeclaration(parent) && parent.getInitializer() === current) {
        declaration = parent;
        break;
      }
      current = parent;
    }
    if (!declaration) return undefined;
    if (declaration.getVariableStatement()?.getDeclarationKind() !== VariableDeclarationKind.Const) {
      return undefined;
    }
    const name = declaration.getNameNode();
    if (!Node.isIdentifier(name)) return undefined;
    const symbol = name.getSymbol()?.compilerSymbol;
    if (!symbol) return undefined;

    const checker = this.project.getTypeChecker().compilerObject;
    let sent = false;
    for (const ref of scope.getDescendantsOfKind(SyntaxKind.Identifier)) {
      if (ref === name) continue;
      const parent = ref.getParent();
      const refSymbol =
        parent && Node.isShorthandPropertyAssignment(parent)
          ? checker.getShorthandAssignmentValueSymbol(parent.compilerNode)
          : ref.getSymbol()?.compilerSymbol;
      if (refSymbol !== symbol) continue;
      if (this.inSendNothingTest(ref, scope)) continue;
      const value = this.bodyValueAround(ref, scope);
      if (value === ref) {
        sent = true;
        continue;
      }
      if (value) return undefined;
      if (parent && Node.isCallExpression(parent) && parent.getArguments().includes(ref)) {
        const onward = this.calleeBodyRead(ref, depth + 1);
        if (onward.kind === 'passes') {
          sent = true;
          continue;
        }
        if (onward.kind !== 'none') return undefined;
      }
    }
    return sent ? name : undefined;
  }

  /** `value` is `use` behind what `sentValue` sees through. */
  private isSameValue(value: Node, use: Node): boolean {
    return this.sentValue(value) === use;
  }

  /**
   * The value a body slot sends when it sends anything, behind parentheses,
   * casts, `await`, a `JSON.stringify`, and (carrick#2668) a choice whose
   * other side sends nothing: `x ? JSON.stringify(x) : undefined`,
   * `x || null`, `x ?? undefined`. A `?? {}` sends `{}`, so it is a body the
   * function builds and is not seen through.
   */
  private sentValue(value: Node): Node {
    let current = value;
    for (;;) {
      let inner = this.unwrapJsonStringifyArg(this.unwrapExpressionNode(current));
      if (Node.isConditionalExpression(inner)) {
        inner = this.sendingBranch(inner) ?? inner;
      } else if (Node.isBinaryExpression(inner)) {
        const operator = inner.getOperatorToken().getKind();
        if (
          (operator === SyntaxKind.BarBarToken || operator === SyntaxKind.QuestionQuestionToken) &&
          this.sendsNothing(inner.getRight(), false)
        ) {
          inner = inner.getLeft();
        }
      }
      if (inner === current) return current;
      current = inner;
    }
  }

  /**
   * The branch of `choice` that sends something, when its other branch sends
   * nothing (`undefined`, `null`, `void 0`, or `{}` spread into a request).
   */
  private sendingBranch(choice: ConditionalExpression): Node | undefined {
    const whenTrue = choice.getWhenTrue();
    const whenFalse = choice.getWhenFalse();
    if (this.sendsNothing(whenFalse, true) && !this.sendsNothing(whenTrue, true)) return whenTrue;
    if (this.sendsNothing(whenTrue, true) && !this.sendsNothing(whenFalse, true)) return whenFalse;
    return undefined;
  }

  /** `undefined`, `null`, `void 0`, or (where `emptyObject`) `{}`. */
  private sendsNothing(node: Node, emptyObject: boolean): boolean {
    const value = this.unwrapExpressionNode(node);
    if (Node.isNullLiteral(value) || Node.isVoidExpression(value)) return true;
    if (Node.isIdentifier(value) && value.getText() === 'undefined') return true;
    return (
      emptyObject &&
      Node.isObjectLiteralExpression(value) &&
      value.getProperties().length === 0
    );
  }

  /**
   * carrick#2668, #2664: `use` sits in the test of a choice whose other side
   * sends nothing (`body ? JSON.stringify(body) : null`,
   * `...(body === undefined ? {} : { body: JSON.stringify(body) })`). The
   * test only decides whether the argument is sent.
   */
  private inSendNothingTest(use: Node, scope: Node): boolean {
    let current: Node = use;
    while (current !== scope) {
      const parent = current.getParent();
      if (!parent) return false;
      if (
        Node.isConditionalExpression(parent) &&
        parent.getCondition() === current &&
        this.sendingBranch(parent)
      ) {
        return true;
      }
      current = parent;
    }
    return false;
  }

  /** A string, a string literal, a template literal, or a union of strings. */
  private isStringLikeType(type: Type): boolean {
    return (
      type.isString() ||
      type.isStringLiteral() ||
      type.isTemplateLiteral() ||
      (type.isUnion() && type.getUnionTypes().every((t) => t.isString() || t.isStringLiteral()))
    );
  }

  /**
   * carrick#1841, request half: a request call that states its URL on its one
   * object argument (`client.post({ url: '/items', ...options })`, the way a
   * generated client issues every operation). The method's own parameter
   * types the body `unknown`, so the object is the only statement of it: its
   * `body` member, the member `RequestInit` defines, written on the object or
   * carried in by a spread whose type says what it is.
   *
   * - A `body` written after the object's last spread is the body.
   * - Otherwise the object's own type has the answer. No `body` member, or
   *   one typed `undefined` or `never`, means the call sends no body.
   * - A member typed `any`, `unknown` or a type parameter, or one only the
   *   compiler's default library declares (a spread `RequestInit` carries the
   *   serialised `BodyInit`, not a payload), states no payload: abstain. So
   *   does an object that carries another body-named member and no `body`.
   *
   * An object with no string-typed `url` member is not a request object.
   */
  private requestObjectBody(
    arg: Node
  ):
    | { kind: 'none' }
    | { kind: 'abstain'; why: string }
    | { kind: 'body'; node: Node; from: string }
    | { kind: 'body_type'; type: Type; at: Node; from: string }
    | { kind: 'no_body'; member: string } {
    const literal = this.unwrapExpressionNode(arg);
    if (!Node.isObjectLiteralExpression(literal)) return { kind: 'none' };
    const objectType = literal.getType();
    const url = objectType.getProperty('url');
    if (!url || !this.isStringLikeType(url.getTypeAtLocation(literal).getNonNullableType())) {
      return { kind: 'none' };
    }
    const from = "the request call's request object";
    const member = 'body';

    const properties = literal.getProperties();
    const written = properties.findIndex(
      (p) =>
        (Node.isPropertyAssignment(p) || Node.isShorthandPropertyAssignment(p)) &&
        p.getName() === member
    );
    const spreadAfter = properties.some((p, i) => i > written && Node.isSpreadAssignment(p));
    if (written >= 0 && !spreadAfter) {
      const value = this.objectLiteralMemberValue(literal, member);
      if (value) return { kind: 'body', node: value, from };
    }

    const property = objectType.getProperty(member);
    if (!property) {
      const otherBodyMember = [...BODY_MEMBER_NAMES].some(
        (name) => name !== member && objectType.getProperty(name)
      );
      return otherBodyMember
        ? { kind: 'abstain', why: `the request object carries no '${member}' member but another body-named one` }
        : { kind: 'no_body', member };
    }
    const type = property.getTypeAtLocation(literal);
    if (type.isAny() || type.isUnknown() || type.isTypeParameter()) {
      return { kind: 'abstain', why: `the request object's '${member}' member is not typed` };
    }
    const bare = type.getNonNullableType();
    if (bare.isNever() || bare.isUndefined() || bare.isVoid()) {
      return { kind: 'no_body', member };
    }
    if (bare.isAny() || bare.isUnknown() || bare.isTypeParameter()) {
      return { kind: 'abstain', why: `the request object's '${member}' member is not typed` };
    }
    const program = this.project.getProgram().compilerObject;
    const declarations = property.getDeclarations();
    if (
      declarations.length > 0 &&
      declarations.every((d) => program.isSourceFileDefaultLibrary(d.getSourceFile().compilerNode))
    ) {
      return {
        kind: 'abstain',
        why: `the request object's '${member}' member is the platform's serialised body, not a payload`,
      };
    }
    return { kind: 'body_type', type: bare, at: literal, from };
  }

  /**
   * The body a config-slot argument supplies through `member`: an object
   * literal's own member, or a variable's member read off its type.
   */
  private configArgumentBody(
    arg: Node,
    member: string,
    from: string
  ):
    | { kind: 'abstain'; why: string }
    | { kind: 'body'; node: Node; from: string }
    | { kind: 'body_type'; type: Type; at: Node; from: string }
    | { kind: 'no_body'; member: string } {
    const unwrapped = this.unwrapExpressionNode(arg);
    if (Node.isObjectLiteralExpression(unwrapped)) {
      const value = this.objectLiteralMemberValue(unwrapped, member);
      if (value) return { kind: 'body', node: value, from };
      if (this.literalMemberNamed(unwrapped, member)) {
        return {
          kind: 'abstain',
          why: `the request config writes its '${member}' member as a method or accessor, so its value is unread`,
        };
      }
      if (
        unwrapped
          .getProperties()
          .some((p) => Node.isSpreadAssignment(p) || this.isComputedMember(p))
      ) {
        return this.spreadLiteralBody(unwrapped, member);
      }
      return { kind: 'no_body', member };
    }
    const property = unwrapped.getType().getProperty(member);
    if (property) {
      const type = property.getTypeAtLocation(unwrapped);
      // Top types first: the non-nullable form of `unknown` is `{}`.
      const bare = type.isAny() || type.isUnknown() ? type : type.getNonNullableType();
      if (!bare.isAny() && !bare.isUnknown() && !bare.isTypeParameter()) {
        return { kind: 'body_type', type: bare, at: unwrapped, from };
      }
    }
    return {
      kind: 'abstain',
      why: `the located request config's type does not resolve its '${member}' member`,
    };
  }

  /** A property whose name is computed, so the literal's written keys do not name it. */
  private isComputedMember(property: Node): boolean {
    return (
      (Node.isPropertyAssignment(property) ||
        Node.isShorthandPropertyAssignment(property) ||
        Node.isMethodDeclaration(property)) &&
      Node.isComputedPropertyName(property.getNameNode())
    );
  }

  /**
   * A config literal that spreads another object, or writes a computed key,
   * names no `body` itself, so the literal's own type is read: the call sends
   * no body only when every branch of that type declares no `body` member, or
   * one typed `undefined` or `never`. The literal and each spread are first
   * read for a type that rules nothing out: one not known (a spread of an
   * untyped object), a generic one (a type parameter, or an intersection with
   * one), or one with a string index signature (which the compiler drops
   * from a spread's result). Any of those abstains.
   */
  private spreadLiteralBody(
    literal: ObjectLiteralExpression,
    member: string
  ): { kind: 'abstain'; why: string } | { kind: 'no_body'; member: string } {
    const literalType = literal.getType();
    const read = [
      literalType,
      ...literal
        .getProperties()
        .filter(Node.isSpreadAssignment)
        .map((spread) => spread.getExpression().getType()),
    ];
    for (const type of read) {
      const unread = this.spreadTypeUnread(type);
      if (unread) {
        return {
          kind: 'abstain',
          why: `the request config spreads an object whose type ${unread}, so its '${member}' member is unread`,
        };
      }
    }
    const branches = literalType.isUnion() ? literalType.getUnionTypes() : [literalType];
    for (const branch of branches) {
      const property = branch.getProperty(member);
      if (!property) continue;
      const type = property.getTypeAtLocation(literal);
      const bare = type.isAny() || type.isUnknown() ? type : type.getNonNullableType();
      if (bare.isNever() || bare.isUndefined() || bare.isVoid()) continue;
      return {
        kind: 'abstain',
        why: `the request config spreads an object whose '${member}' member may carry a body`,
      };
    }
    return { kind: 'no_body', member };
  }

  /** Why a spread-side type rules out no member, or undefined when it can be read. */
  private spreadTypeUnread(type: Type): string | undefined {
    const branches = type.isUnion() ? type.getUnionTypes() : [type];
    for (const branch of branches) {
      const parts = branch.isIntersection() ? branch.getIntersectionTypes() : [branch];
      for (const part of parts) {
        if (part.isAny() || part.isUnknown()) return 'is not known';
        if ((part.compilerType.flags & ts.TypeFlags.Instantiable) !== 0) return 'is generic';
      }
      if (branch.getStringIndexType()) return 'has an index signature';
    }
    return undefined;
  }

  /**
   * Per parameter position of the call's resolved declaration: a body slot, a
   * config slot (with the member that carries the body), an opaque generic
   * config, or neither (undefined). Empty when the callee has no declaration.
   */
  private requestBodySlots(
    call: CallExpression
  ): Array<
    { kind: 'body' } | { kind: 'config'; member: string } | { kind: 'opaque_config' } | undefined
  > {
    const checker = this.project.getTypeChecker().compilerObject;
    let declaration: ts.SignatureDeclaration | undefined;
    try {
      declaration = checker.getResolvedSignature(call.compilerNode)?.getDeclaration();
    } catch {
      return [];
    }
    if (!declaration || !('parameters' in declaration)) return [];
    const ownTypeParams = new Set(
      (declaration.typeParameters ?? []).map((tp) => tp.name.text)
    );
    return declaration.parameters.map((param) => {
      if (param.dotDotDotToken) return undefined;
      const typeNode = param.type;
      if (!typeNode || !ts.isTypeReferenceNode(typeNode)) return undefined;
      if (
        ts.isIdentifier(typeNode.typeName) &&
        !typeNode.typeArguments &&
        ownTypeParams.has(typeNode.typeName.text)
      ) {
        return ts.isIdentifier(param.name) && BODY_MEMBER_NAMES.has(param.name.text)
          ? { kind: 'body' as const }
          : undefined;
      }
      return this.configSlot(checker, typeNode);
    });
  }

  /**
   * A generic config type's slot: `config` with the body-named member typed by
   * the config's own type parameter (`interface Config<D> { data?: D }`), or
   * `opaque_config` when the config is generic but no body-named member is
   * (`interface Options<R> { responseType?: R }`). Undefined for a type that
   * is not a generic config, and for two candidate members (ambiguous).
   */
  private configSlot(
    checker: ts.TypeChecker,
    typeNode: ts.TypeReferenceNode
  ): { kind: 'config'; member: string } | { kind: 'opaque_config' } | undefined {
    let symbol = checker.getSymbolAtLocation(typeNode.typeName);
    if (symbol && symbol.flags & ts.SymbolFlags.Alias) {
      symbol = checker.getAliasedSymbol(symbol);
    }
    let generic = false;
    const found = new Set<string>();
    for (const decl of symbol?.getDeclarations() ?? []) {
      if (!ts.isInterfaceDeclaration(decl) && !ts.isTypeAliasDeclaration(decl)) continue;
      const own = new Set((decl.typeParameters ?? []).map((tp) => tp.name.text));
      if (own.size === 0) continue;
      const members = ts.isInterfaceDeclaration(decl)
        ? decl.members
        : ts.isTypeLiteralNode(decl.type)
          ? decl.type.members
          : undefined;
      if (!members) continue;
      generic = true;
      for (const member of members) {
        if (!ts.isPropertySignature(member) || !member.type || !member.name) continue;
        if (!ts.isIdentifier(member.name) && !ts.isStringLiteral(member.name)) continue;
        if (!BODY_MEMBER_NAMES.has(member.name.text)) continue;
        const t = member.type;
        if (
          ts.isTypeReferenceNode(t) &&
          ts.isIdentifier(t.typeName) &&
          !t.typeArguments &&
          own.has(t.typeName.text)
        ) {
          found.add(member.name.text);
        }
      }
    }
    if (found.size === 1) return { kind: 'config', member: [...found][0] };
    return generic && found.size === 0 ? { kind: 'opaque_config' } : undefined;
  }

  /** The value an object literal gives `name`: an assignment's initializer or a shorthand's identifier. */
  private objectLiteralMemberValue(
    literal: ObjectLiteralExpression,
    name: string
  ): Node | undefined {
    const property = this.literalMemberNamed(literal, name);
    if (!property) return undefined;
    if (Node.isPropertyAssignment(property)) return property.getInitializer();
    if (Node.isShorthandPropertyAssignment(property)) return property.getNameNode();
    return undefined;
  }

  /**
   * The last member an object literal writes under `name`, in any form
   * (assignment, shorthand, method, accessor), with a quoted or numeric key
   * read by its text. Spreads and computed keys name nothing here.
   */
  private literalMemberNamed(literal: ObjectLiteralExpression, name: string): Node | undefined {
    let found: Node | undefined;
    for (const property of literal.getProperties()) {
      if (Node.isSpreadAssignment(property)) continue;
      const nameNode = property.getNameNode();
      if (Node.isComputedPropertyName(nameNode)) continue;
      const text =
        Node.isStringLiteral(nameNode) ||
        Node.isNoSubstitutionTemplateLiteral(nameNode) ||
        Node.isNumericLiteral(nameNode)
          ? nameNode.getLiteralText()
          : nameNode.getText();
      if (text === name) found = property;
    }
    return found;
  }

  /**
   * An explicit request `InferredType` for a contract the route declares,
   * carrying the provenance the reading recorded.
   */
  private declaredRequestInferredType(
    request: InferRequestItem,
    contract: DeclaredContract,
    location: SourceLocation
  ): InferredType {
    if (contract.noRequestBody !== undefined) {
      this.log(
        `Route at ${request.file_path}:${request.line_number} states no request body: ${contract.noRequestBody}`
      );
      return this.decidedAbstain(request, location, {
        path: '',
        kind: 'unknown',
        reason: 'no_request_body',
        detail: contract.noRequestBody,
      });
    }
    const inferred = this.createInferredType(
      request,
      contract.text,
      true,
      location,
      undefined,
      undefined
    );
    if (contract.provenance && contract.provenance.length > 0) {
      inferred.any_provenance = contract.provenance;
    }
    return inferred;
  }

  /**
   * The request contract a route DECLARES, in anchor order: the handler's
   * parameter annotation (a), the registration's schema object (b), then a
   * validator middleware bound to the request body (b2).
   *
   * Both callers — the registration locator and the expression locator whose
   * line is a registration — consult exactly this set, so the two cannot drift.
   * The typed request READ inside the handler body is deliberately not part of
   * it: that anchor is itself an expression, and the expression path keeps its
   * own locator authoritative when a route declares nothing.
   */
  private declaredRequestContract(
    registration: Node,
    handler: FunctionLike
  ): DeclaredContract | null {
    // carrick#2674: the body member a verified label names, read at the
    // handler's parameter, comes first. Where no label answers, the old rule
    // that reads a member by its name stands.
    const stated = this.statedParams.request(handler, registration);
    if (stated?.kind === 'body') {
      const text = this.structuralTextFromType(stated.type, stated.at);
      if (text) {
        return { text };
      }
    }
    const annotated = this.requestBodyFromHandlerParams(handler);
    if (annotated) {
      return { text: annotated };
    }
    const schema =
      this.routeSchemaContract(registration, 'body') ??
      this.validatedBodyContract(registration);
    if (schema) {
      return schema;
    }
    return stated?.kind === 'no_body' ? { text: 'unknown', noRequestBody: stated.detail } : null;
  }

  /** The stated-parameter reader over `labels`, judging files and statuses as this inferrer does. */
  private statedParamsFor(labels: LabelledMembers): StatedParams {
    return new StatedParams(
      labels,
      (file) => {
        const sourceFile = file.getSourceFile();
        if (sourceFile.isDeclarationFile()) return false;
        return !isExternalOrigin(
          this.project.getProgram().compilerObject,
          sourceFile.compilerNode,
          this.repoRoot,
          externalImportsOf(this.project)
        );
      },
      (call) => this.chainStatus(call),
      (value) => this.statusOfValue(value),
      (value) => this.unwrapJsonStringifyArg(this.unwrapExpressionNode(value))
    );
  }

  /**
   * carrick#2674: where today's reading of a route's response is empty
   * (nothing, `void`, or an `unknown` the inferrer did not decide), the reply
   * its handler's parameter states, cut to its success variants. A typed
   * reading, or a decided abstain, stands as it is.
   */
  private withStatedReply(
    sourceFile: SourceFile,
    request: InferRequestItem,
    today: InferredType | null
  ): InferredType | null {
    if (!this.statedParams.readsReplies) return today;
    if (today && !this.isEmptyResponseReading(today)) return today;
    const handler =
      this.registrationAtLine(sourceFile, request.line_number)?.handler ??
      this.exportedFunctionOpeningOn(sourceFile, request.line_number);
    if (!handler) return today;
    const stated = this.statedParams.reply(handler);
    if (!stated) return today;
    const texts: string[] = [];
    for (const variant of stated.variants) {
      const text = this.structuralTextFromType(variant, stated.at);
      // A variant holding `any` or `unknown` is one the check refuses as an
      // anchor (`usable_inferred_text`): publishing it would fall back to the
      // raw anchor under an explicit flag, so today's reading stands.
      if (!text || printedTypeHoldsTopType(text)) return today;
      texts.push(text);
    }
    this.log(
      `Route at ${request.file_path}:${request.line_number}: its handler's reply parameter states the body`
    );
    return this.createInferredType(
      request,
      texts.length === 1 ? texts[0] : texts.map((text) => `(${text})`).join(' | '),
      true,
      this.getNodeLocation(handler)
    );
  }

  /** Nothing read: `void`, or an `unknown` with no decided reason at its root. */
  private isEmptyResponseReading(inferred: InferredType): boolean {
    if (inferred.type_string === 'void') return true;
    if (inferred.type_string !== 'unknown') return false;
    return !(inferred.any_provenance ?? []).some(
      (provenance) => provenance.path === '' && DECIDED_ABSTAIN_REASONS.has(provenance.reason)
    );
  }

  /**
   * Anchor (a): the request contract declared on the handler's own signature.
   *
   * A typed request parameter (`request: CreateWidgetRequest` where that alias
   * resolves to a request type parameterised with the body shape) exposes the
   * contract as the `body` member of the parameter's type. Parameters are
   * scanned in order and the first one carrying a non-useless `body` member
   * wins; a reply/response parameter has no `body` member, and an unparameterised
   * request type resolves `body` to `unknown`/`any`, which the useless-type
   * guard rejects. So a handler that declares nothing yields null and the next
   * anchor runs.
   *
   * The member has to be one the route's own annotation can have filled
   * (carrick#807). The platform `Request` declares `body` too, as the byte
   * stream every request carries, and so does each library type that extends
   * it. That is concrete, so it passed the useless-type guard and a handler
   * taking the platform request published a stream as its request contract,
   * ahead of the body read inside it. A `body` a library declares with one
   * fixed type says nothing about this route and is skipped.
   */
  private requestBodyFromHandlerParams(func: FunctionLike): string | null {
    for (const param of func.getParameters()) {
      let paramType: Type;
      try {
        paramType = param.getType();
      } catch {
        continue;
      }
      const bodySymbol = paramType.getProperty('body');
      if (!bodySymbol || this.memberIsFixedByItsLibrary(bodySymbol)) {
        continue;
      }
      let bodyType: Type;
      try {
        bodyType = bodySymbol.getTypeAtLocation(param);
      } catch {
        continue;
      }
      const text = this.structuralTextFromType(bodyType, param);
      if (text) {
        return text;
      }
    }
    return null;
  }

  /**
   * True when a member is declared by a library, or by the platform, with a
   * type that names none of its declaring type's parameters: the same type on
   * every value, whichever route the value belongs to.
   *
   * `body: ReqBody` on a request type generic in its body is a slot, and the
   * handler's annotation fills it. `readonly body: ReadableStream<Uint8Array>
   * | null` on the platform request is not. A member the repo declares itself
   * is never fixed in this sense: writing `body: NewWidget` on the handler's
   * own request type is the annotation, and one declaration of the repo's
   * among several (an intersection with the platform type) decides it. A
   * declaration that states no type at all is left to the guards that read
   * the type.
   */
  private memberIsFixedByItsLibrary(member: TsSymbol): boolean {
    const declarations = member.getDeclarations();
    if (declarations.length === 0) return false;
    const program = this.project.getProgram().compilerObject;
    const imports = externalImportsOf(this.project);
    return declarations.every((declaration) => {
      const declaredByLibrary = isExternalOrigin(
        program,
        declaration.getSourceFile().compilerNode,
        this.repoRoot,
        imports
      );
      if (!declaredByLibrary) return false;
      const typeNode =
        Node.isPropertySignature(declaration) || Node.isPropertyDeclaration(declaration)
          ? declaration.getTypeNode()
          : Node.isGetAccessorDeclaration(declaration)
            ? declaration.getReturnTypeNode()
            : undefined;
      if (!typeNode) return false;
      const names = [typeNode, ...typeNode.getDescendants()].filter(Node.isIdentifier);
      return !names.some((name) =>
        (name.getSymbol()?.getDeclarations() ?? []).some(Node.isTypeParameterDeclaration)
      );
    });
  }

  /**
   * Anchor (b2): the contract declared by a VALIDATOR MIDDLEWARE on the
   * registration (carrick#964).
   *
   * A route can declare its body by binding a schema to a named part of the
   * request in a middleware the registration carries alongside the handler:
   *
   *   router.post('/search', validate('json', PayloadSchema), async (c) => …)
   *
   * The shape is a CALL among the registration's arguments whose own arguments
   * are a request part and a schema value. Neither the middleware's name nor
   * the schema library is checked: the part is read off the string literal, and
   * the schema is whatever declares its types (`schemaContract`) — the same
   * test anchor (b) already applies to a `schema: { body: … }` object. The
   * contract is the schema's INPUT, what a caller sends (carrick#1101).
   *
   * `REQUEST_BODY_PARTS` is HTTP vocabulary for how a body is carried, not a
   * framework list. A middleware bound to any other part (`query`, `param`,
   * `header`, `cookie`) declares no BODY, and a middleware that names no part
   * at all is not read: publishing a query schema as the request contract would
   * be the same confident-and-wrong answer this anchor exists to remove.
   */
  private validatedBodyContract(registration: Node): DeclaredContract | null {
    for (const candidate of this.middlewareBodySchemaValues(registration)) {
      const declared = this.schemaContract(candidate, 'input');
      if (declared) {
        return declared;
      }
    }
    return null;
  }

  /**
   * The values a registration's validator middleware binds to a body part, in
   * argument order: every non-literal argument of a middleware call that names
   * a `REQUEST_BODY_PARTS` part. Whether each one IS a schema is the reader's
   * question (`schemaContract`), not this walk's.
   */
  private middlewareBodySchemaValues(registration: Node): Node[] {
    if (!Node.isCallExpression(registration)) {
      return [];
    }

    const values: Node[] = [];
    for (const argument of registration.getArguments()) {
      const middleware = this.unwrapExpressionNode(argument);
      if (!Node.isCallExpression(middleware)) {
        continue;
      }

      const middlewareArgs = middleware
        .getArguments()
        .map((arg) => this.unwrapExpressionNode(arg));
      const bindsBody = middlewareArgs.some(
        (arg) =>
          Node.isStringLiteral(arg) &&
          REQUEST_BODY_PARTS.has(arg.getLiteralValue().toLowerCase())
      );
      if (!bindsBody) {
        continue;
      }

      values.push(...middlewareArgs.filter((arg) => !Node.isStringLiteral(arg)));
    }
    return values;
  }

  /**
   * The type the function a value is made in states for it, where the
   * compiler types the value itself `any` or `unknown` (carrick#2594), or
   * null.
   *
   * `value` is the located expression: a body read, or a call whose result is
   * untyped. The value is followed through the names that hold it: a
   * declaration it initialises, and a name it is assigned to later
   * (`let body: unknown; body = await read()`). At each place the value or
   * one of those names is used whole, the source can state a type:
   *
   *  0. the annotation of the name that holds it (`let body: Upload`), or the
   *     declared return type of the function that returns it unchanged (only
   *     with `reads.returned`);
   *  1. a schema it is handed to (`Schema.safeParse(body ?? {})`,
   *     `parse(Schema, body)`): the schema's input is what a sender may send
   *     (carrick#1166). A candidate counts only when it declares schema types,
   *     so no method or library name is matched;
   *  2. a function of the project it is handed to that returns that argument
   *     unchanged (through casts) or an empty default: its declared return
   *     type. A function that builds a new value from its argument is a
   *     converter, and its return type describes what it built, never the
   *     value it was handed, so it states nothing here;
   *  3. a cast of the whole value (`(body as { phone: string }).phone`);
   *  4. the parameter of a call it is handed to (only with
   *     `reads.parameters`), where that parameter's type is a body shape once
   *     any callable member is dropped (a function never crosses the wire).
   *
   * The first rank with an answer decides. A placeholder (`unknown`,
   * `Record<string, unknown>`, any type node with `any` or `unknown` written
   * in it) states nothing, and two different statements in one rank decide
   * nothing: the answer is null.
   */
  private statedInFunction(
    value: Node,
    func: FunctionLike,
    reads: { returned: boolean; parameters: boolean; skipFailurePaths: boolean }
  ): StatedInFunction | null {
    const holders = new Set<ts.Symbol>();
    const positions: Node[] = [value];
    const holderUse = (node: Node): boolean => {
      if (!Node.isIdentifier(node)) return false;
      const symbol = node.getSymbol()?.compilerSymbol;
      return !!symbol && holders.has(symbol);
    };
    const hold = (name: Node): void => {
      if (!Node.isIdentifier(name)) return;
      const symbol = name.getSymbol()?.compilerSymbol;
      if (!symbol || holders.has(symbol)) return;
      holders.add(symbol);
      for (const id of func.getDescendantsOfKind(SyntaxKind.Identifier)) {
        if (id === name || id.getSymbol()?.compilerSymbol !== symbol) continue;
        const parent = id.getParent();
        if (Node.isVariableDeclaration(parent) && parent.getNameNode() === id) continue;
        if (
          Node.isBinaryExpression(parent) &&
          parent.getLeft() === id &&
          parent.getOperatorToken().getKind() === SyntaxKind.EqualsToken
        ) {
          continue;
        }
        positions.push(id);
      }
    };
    hold(value);

    const found: Array<{
      rank: number;
      text: string;
      provenance?: TypeProvenance[];
      at: Node;
      how: string;
      typeNode?: Node;
    }> = [];
    const stateNode = (rank: number, typeNode: Node | undefined, at: Node, how: string): void => {
      if (!typeNode || this.leavesAPositionOpen(typeNode)) return;
      const text = this.structuralTextFromTypeNode(typeNode);
      if (text) found.push({ rank, text, at, how, typeNode });
    };

    for (let i = 0; i < positions.length; i++) {
      const position = positions[i];
      if (
        reads.skipFailurePaths &&
        position !== value &&
        reachedOnlyOnFailure(position, func, holderUse)
      ) {
        continue;
      }

      // Climb the wrappers that leave the value what it was: parentheses,
      // `await`, `!`, `satisfies`, a placeholder cast, and an empty default
      // (`body ?? {}`).
      let outer: Node = position;
      for (;;) {
        const parent = outer.getParent();
        if (!parent) break;
        if (
          (Node.isParenthesizedExpression(parent) ||
            Node.isAwaitExpression(parent) ||
            Node.isNonNullExpression(parent) ||
            Node.isSatisfiesExpression(parent)) &&
          parent.getExpression() === outer
        ) {
          outer = parent;
          continue;
        }
        if (
          (Node.isAsExpression(parent) || Node.isTypeAssertion(parent)) &&
          parent.getExpression() === outer
        ) {
          const typeNode = parent.getTypeNode();
          if (typeNode && !this.leavesAPositionOpen(typeNode)) {
            stateNode(3, typeNode, parent, 'a cast of the whole value');
            break;
          }
          outer = parent;
          continue;
        }
        if (
          Node.isBinaryExpression(parent) &&
          parent.getLeft() === outer &&
          [SyntaxKind.QuestionQuestionToken, SyntaxKind.BarBarToken].includes(
            parent.getOperatorToken().getKind()
          ) &&
          this.isEmptyDefault(parent.getRight())
        ) {
          outer = parent;
          continue;
        }
        break;
      }

      const parent = outer.getParent();
      if (!parent) continue;

      if (Node.isVariableDeclaration(parent) && parent.getInitializer() === outer) {
        stateNode(0, parent.getTypeNode(), parent, 'the annotation of the name that holds it');
        hold(parent.getNameNode());
        continue;
      }

      if (
        Node.isBinaryExpression(parent) &&
        parent.getRight() === outer &&
        parent.getOperatorToken().getKind() === SyntaxKind.EqualsToken &&
        Node.isIdentifier(parent.getLeft())
      ) {
        const declaration = parent.getLeft().getSymbol()?.getValueDeclaration();
        if (declaration && Node.isVariableDeclaration(declaration)) {
          stateNode(0, declaration.getTypeNode(), declaration, 'the annotation of the name it is assigned to');
        }
        hold(parent.getLeft());
        continue;
      }

      if (reads.returned) {
        const returnedHere =
          (Node.isReturnStatement(parent) &&
            parent.getExpression() === outer &&
            this.findContainingFunctionForNode(parent) === func) ||
          (Node.isArrowFunction(parent) && parent === func && parent.getBody() === outer);
        if (returnedHere) {
          stateNode(0, func.getReturnTypeNode(), func, 'the declared return type of the function that returns it');
          continue;
        }
      }

      if (Node.isCallExpression(parent) && parent.getArguments().includes(outer)) {
        this.stateAtReceivingCall(parent, parent.getArguments().indexOf(outer as never), reads, found, stateNode);
      }
    }

    if (found.length === 0) return null;
    const rank = Math.min(...found.map((entry) => entry.rank));
    const decided = found.filter((entry) => entry.rank === rank);
    if (new Set(decided.map((entry) => entry.text)).size !== 1) {
      this.log(
        `A value at ${this.getNodeLocation(value).file_path}:${this.getNodeLocation(value).start_line} ` +
          `is stated as ${decided.length} different types in its function; stating none`
      );
      return null;
    }
    const [first] = decided;
    return { text: first.text, provenance: first.provenance, at: first.at, how: first.how, typeNode: first.typeNode };
  }

  /**
   * What a call that takes the value at argument `index` states for it:
   * a schema among its receiver and other arguments (rank 1), a callee of the
   * project that returns that argument unchanged (rank 2), or, with
   * `reads.parameters`, the parameter's own type (rank 4).
   */
  private stateAtReceivingCall(
    call: CallExpression,
    index: number,
    reads: { parameters: boolean },
    found: Array<{ rank: number; text: string; provenance?: TypeProvenance[]; at: Node; how: string }>,
    stateNode: (rank: number, typeNode: Node | undefined, at: Node, how: string) => void
  ): void {
    const contract = this.schemaHandedArgument(call, index);
    if (contract) {
      found.push({ rank: 1, text: contract.text, provenance: contract.provenance, at: call, how: 'a schema it is handed to' });
      return;
    }

    const passThrough = this.passThroughFunction(call, index);
    if (passThrough) {
      stateNode(2, passThrough.getReturnTypeNode(), call, 'a function that returns it unchanged');
      return;
    }

    if (!reads.parameters) return;
    const signature = this.project.getTypeChecker().getResolvedSignature(call);
    const parameter = signature?.getParameters()[index];
    const declaration = parameter?.getValueDeclaration();
    if (!parameter || (declaration && Node.isParameterDeclaration(declaration) && declaration.isRestParameter())) {
      return;
    }
    let parameterType = parameter.getTypeAtLocation(call);
    if (parameterType.isUnion()) {
      const bodies = parameterType.getUnionTypes().filter((part) => !this.isCallableType(part));
      if (bodies.length !== 1) return;
      parameterType = bodies[0];
    }
    if (!this.isObjectShape(parameterType)) return;
    const text = this.structuralTextFromType(parameterType, call);
    if (!text || /\b(any|unknown)\b/.test(text)) return;
    found.push({ rank: 4, text, at: call, how: 'the parameter of a call it is handed to' });
  }

  /**
   * The input contract of a schema `call` validates its argument at `index`
   * with: its receiver (`Schema.safeParse(body)`) or one of its other
   * arguments (`parse(Schema, body)`), or null. A candidate counts only when
   * it declares schema types, so no method or library name is matched
   * (carrick#1166).
   */
  private schemaHandedArgument(call: CallExpression, index: number): DeclaredContract | null {
    const candidates: Node[] = [];
    const callee = call.getExpression();
    if (Node.isPropertyAccessExpression(callee)) candidates.push(callee.getExpression());
    call.getArguments().forEach((argument, at) => {
      if (at !== index) candidates.push(this.unwrapExpressionNode(argument));
    });
    for (const candidate of candidates) {
      const candidateType = candidate.getType();
      if (
        !this.schemaOutputType(candidateType, candidate) &&
        !this.schemaInputType(candidateType, candidate)
      ) {
        continue;
      }
      const contract = this.schemaContract(candidate, 'input');
      if (contract) return contract;
    }
    return null;
  }

  /**
   * `call`'s result describes the value it takes at argument `index`: it
   * validates it with a schema, or its callee returns it unchanged
   * (`passThroughFunction`). Any other call builds something new from the
   * value, and its result describes what it built, never the value it was
   * handed (carrick#2595).
   */
  private callStatesItsArgument(call: CallExpression, index: number): boolean {
    return index >= 0 && (!!this.schemaHandedArgument(call, index) || !!this.passThroughFunction(call, index));
  }

  /**
   * The project function `call` invokes, when every value it returns is its
   * argument at `index` as it was handed in (through parentheses, casts,
   * `satisfies` and `!`) or an empty default (`{}`, `[]`, `null`,
   * `undefined`), and at least one return is that argument. Such a function
   * narrows its argument, so its declared return type describes the value it
   * was handed. Any other function builds something new from it, and is not
   * this.
   */
  private passThroughFunction(call: CallExpression, index: number): FunctionLike | undefined {
    let symbol = call.getExpression().getSymbol();
    if (symbol?.isAlias()) symbol = symbol.getAliasedSymbol() ?? symbol;
    for (const declaration of symbol?.getDeclarations() ?? []) {
      const fn =
        Node.isFunctionDeclaration(declaration) || Node.isMethodDeclaration(declaration)
          ? declaration
          : Node.isVariableDeclaration(declaration)
            ? this.unwrapExpressionNode(declaration.getInitializer())
            : undefined;
      if (
        !fn ||
        !(Node.isFunctionDeclaration(fn) ||
          Node.isMethodDeclaration(fn) ||
          Node.isArrowFunction(fn) ||
          Node.isFunctionExpression(fn)) ||
        !fn.getBody() ||
        fn.getSourceFile().isDeclarationFile() ||
        fn.getSourceFile().isInNodeModules()
      ) {
        continue;
      }
      const parameter = fn.getParameters()[index];
      const parameterSymbol = parameter?.getNameNode().getSymbol()?.compilerSymbol;
      if (!parameter || parameter.isRestParameter() || !parameterSymbol) continue;

      const returned = this.returnedExpressions(fn);
      let returnsArgument = false;
      const everyReturnPasses = returned.every((expression) => {
        let node: Node = expression;
        while (
          Node.isParenthesizedExpression(node) ||
          Node.isAsExpression(node) ||
          Node.isTypeAssertion(node) ||
          Node.isSatisfiesExpression(node) ||
          Node.isNonNullExpression(node)
        ) {
          node = node.getExpression();
        }
        if (Node.isIdentifier(node) && node.getSymbol()?.compilerSymbol === parameterSymbol) {
          returnsArgument = true;
          return true;
        }
        return this.isEmptyDefault(node);
      });
      if (everyReturnPasses && returnsArgument) return fn;
    }
    return undefined;
  }

  /** `{}`, `[]`, `null` or `undefined`: a default that states no member. */
  private isEmptyDefault(node: Node): boolean {
    const bare = this.unwrapExpressionNode(node);
    return (
      (Node.isObjectLiteralExpression(bare) && bare.getProperties().length === 0) ||
      (Node.isArrayLiteralExpression(bare) && bare.getElements().length === 0) ||
      Node.isNullLiteral(bare) ||
      (Node.isIdentifier(bare) && bare.getText() === 'undefined')
    );
  }

  /**
   * The part a validated read names when that part is not a body, or
   * `undefined` (carrick#1166).
   *
   * The read is a call whose only argument is a string literal, and the
   * registration carries a validator middleware binding that same literal.
   * The part names match through the source's own literal, so no framework
   * vocabulary is assumed beyond `REQUEST_BODY_PARTS`, which already decides
   * what a body is for the middleware anchor.
   */
  private nonBodyValidatedPart(read: Node, registration: Node): string | undefined {
    if (!Node.isCallExpression(read) || !Node.isCallExpression(registration)) {
      return undefined;
    }
    const readArgs = read.getArguments().map((arg) => this.unwrapExpressionNode(arg));
    if (readArgs.length !== 1 || !Node.isStringLiteral(readArgs[0])) return undefined;
    const part = readArgs[0].getLiteralValue();
    if (REQUEST_BODY_PARTS.has(part.toLowerCase())) return undefined;

    for (const argument of registration.getArguments()) {
      const middleware = this.unwrapExpressionNode(argument);
      if (!Node.isCallExpression(middleware)) continue;
      const bindsPart = middleware
        .getArguments()
        .map((arg) => this.unwrapExpressionNode(arg))
        .some((arg) => Node.isStringLiteral(arg) && arg.getLiteralValue() === part);
      if (bindsPart) return part;
    }
    return undefined;
  }

  /**
   * The request contract behind a VALIDATED READ: a call inside a registered
   * handler whose type is exactly the parsed output of a body schema the
   * enclosing registration declares (carrick#1101).
   *
   * The read's own type is what validation hands the handler, the schema's
   * output, so a locator that lands on it would publish the output as the
   * request. The registration is found by walking the read's ancestors to each
   * call or registry object whose handler contains the read and which declares
   * a body schema (anchor (b) or (b2)). The match is on the printed type: only
   * a read whose structural text equals that schema's output is substituted, so
   * any other expression in the handler (a query read, a payload of a different
   * shape) keeps its own type.
   */
  private validatedReadRequestContract(read: CallExpression): DeclaredContract | null {
    let readText: string | null | undefined;
    for (const ancestor of read.getAncestors()) {
      if (
        !Node.isCallExpression(ancestor) &&
        !Node.isObjectLiteralExpression(ancestor)
      ) {
        continue;
      }
      const handler = this.registrationHandlerAt(ancestor);
      if (
        !handler ||
        read.getStart() < handler.getStart() ||
        read.getEnd() > handler.getEnd()
      ) {
        continue;
      }

      const schemaValues = [
        ...this.routeSchemaEntryValues(ancestor, 'body'),
        ...this.middlewareBodySchemaValues(ancestor),
      ];
      for (const value of schemaValues) {
        const output = this.schemaContract(value, 'output');
        if (!output) {
          continue;
        }
        if (readText === undefined) {
          readText = this.structuralTextFromType(read.getType(), read);
        }
        if (readText !== output.text) {
          continue;
        }
        return this.schemaContract(value, 'input');
      }
    }
    return null;
  }

  /**
   * Anchor (b): the contract declared in the route's validation schema.
   *
   * `part` is `'body'` for the request contract and `'response'` for the
   * response contract; a `response` entry keyed by status code resolves to its
   * success entry. The body reads the schema's input and the response its
   * output (carrick#1101). Returns null when the registration carries no
   * schema, when the entry references something whose declared types cannot be
   * resolved, or when the schema is a plain JSON-schema literal (whose own
   * object type is the JSON-Schema document, not the payload — emitting that
   * would be worse than abstaining).
   */
  private routeSchemaContract(
    registration: Node,
    part: 'body' | 'response'
  ): DeclaredContract | null {
    const [value] = this.routeSchemaEntryValues(registration, part);
    if (!value) {
      return null;
    }
    return this.schemaContract(value, part === 'body' ? 'input' : 'output');
  }

  /**
   * The value a route `schema` object declares for `part`, as a zero- or
   * one-element list: the `body` entry itself, or the success entry of a
   * status-keyed `response` map.
   */
  private routeSchemaEntryValues(
    registration: Node,
    part: 'body' | 'response'
  ): Node[] {
    const schemaObject = this.routeSchemaObject(registration);
    if (!schemaObject) {
      return [];
    }
    const entry = schemaObject.getProperty(part);
    if (!entry || !Node.isPropertyAssignment(entry)) {
      return [];
    }
    const initializer = entry.getInitializer();
    if (!initializer) {
      return [];
    }
    return [
      part === 'response'
        ? (this.successStatusEntry(initializer) ?? initializer)
        : initializer,
    ];
  }

  /**
   * The `schema` object literal carried by a route registration: scan the
   * registration call's arguments (or the registry object literal itself) for a
   * `schema` property whose value is an object literal.
   */
  private routeSchemaObject(
    registration: Node
  ): ObjectLiteralExpression | undefined {
    const candidates: Node[] = [];
    if (Node.isCallExpression(registration)) {
      candidates.push(...registration.getArguments());
    } else if (Node.isObjectLiteralExpression(registration)) {
      candidates.push(registration);
    }
    for (const candidate of candidates) {
      if (!Node.isObjectLiteralExpression(candidate)) {
        continue;
      }
      const schemaProp = candidate.getProperty('schema');
      if (!schemaProp || !Node.isPropertyAssignment(schemaProp)) {
        continue;
      }
      const schemaValue = schemaProp.getInitializer();
      if (schemaValue && Node.isObjectLiteralExpression(schemaValue)) {
        return schemaValue;
      }
    }
    return undefined;
  }

  /**
   * The success entry of a status-keyed response map (`{ 200: …, 4xx: … }`):
   * exact `200` when present, else the lowest 2xx key. Returns undefined when
   * the value is not a status-keyed map, so the caller reads it as the schema
   * itself.
   */
  private successStatusEntry(value: Node): Node | undefined {
    if (!Node.isObjectLiteralExpression(value)) {
      return undefined;
    }
    let best: { code: number; node: Node } | undefined;
    for (const prop of value.getProperties()) {
      if (!Node.isPropertyAssignment(prop)) {
        continue;
      }
      // `2xx` is a status RANGE key; it reads as the bottom of its range.
      const code = statusKeyCode(prop.getName());
      if (code === undefined || code < 200 || code > 299) {
        continue;
      }
      const initializer = prop.getInitializer();
      if (!initializer) {
        continue;
      }
      if (!best || code < best.code) {
        best = { code, node: initializer };
      }
    }
    return best?.node;
  }

  /**
   * The payload type a schema entry declares, read in `direction`.
   *
   * The entry is either a REFERENCE call — `ref('CreateWidget')`, a
   * name-to-schema indirection whose registry is the argument the ref function
   * was built from — or the schema value itself. A value that declares no type
   * in either direction is not a schema and yields null.
   *
   * `output` is the parsed value (`schemaOutputType`). `input` is what a caller
   * sends (`schemaInputType`), and a schema that declares no input member reads
   * its output instead, which is the whole of what is knowable about it.
   *
   * The input is decided member by member (carrick#1105). A member whose input
   * is `any`/`unknown` where the output is concrete is what a coercion declares
   * (it accepts any value and parses it into, say, a number). A top type in a
   * published contract disqualifies the whole row downstream, so THAT member
   * prints its output type, keeps the input key's optionality, and is recorded
   * as `coerced_input` provenance. Every other member keeps its input, so a
   * defaulted key beside a coerced one stays optional to send.
   *
   * Limits, each logged where it bites:
   *  - the position walk is bounded (`walkTypePositions`), so a top type below
   *    the bound is not compared and prints as the input declares it;
   *  - a coerced position that cannot be substituted (its output is absent or
   *    differs across union branches, or the printer cannot reach it inside a
   *    tuple, behind a cycle or past the expansion depth) makes the row fall
   *    back to the whole parsed output with every coerced member labelled. That
   *    was the answer before the per-member reading, and it never publishes an
   *    `unknown`.
   */
  private schemaContract(
    value: Node,
    direction: SchemaDirection
  ): DeclaredContract | null {
    const node = this.unwrapExpressionNode(value);

    let schemaType: Type | undefined;
    if (Node.isCallExpression(node)) {
      schemaType = this.registrySchemaType(node);
    }
    if (!schemaType) {
      try {
        schemaType = node.getType();
      } catch {
        return null;
      }
    }

    const location = this.getNodeLocation(node);
    const where = `${location.file_path}:${location.start_line}`;
    const output = this.schemaOutputType(schemaType, node);
    const input =
      direction === 'input' ? this.schemaInputType(schemaType, node) : undefined;

    if (!output && !input) {
      this.log(
        `Route schema entry at ${where} declares no resolvable type (schema library ` +
          'types unavailable, or a plain JSON-Schema literal); leaving unresolved'
      );
      return null;
    }

    if (direction === 'output' || !input) {
      if (direction === 'input') {
        this.log(
          `Route schema entry at ${where} declares no input type; publishing its parsed output`
        );
      }
      const text = output ? this.structuralTextFromType(output, node) : null;
      return text ? { text } : null;
    }

    const inputTop = this.topTypePositions(input, node, where);
    const outputTop =
      output && inputTop.size > 0 ? this.topTypePositions(output, node, where) : undefined;
    const coerced = outputTop
      ? [...inputTop.entries()]
          .filter(([position]) => !outputTop.has(position))
          .sort(([a], [b]) => a.localeCompare(b))
      : [];

    if (!output || coerced.length === 0) {
      const text = this.structuralTextFromType(input, node);
      return text ? { text } : null;
    }

    const positions = coerced.map(([position]) => position || '<root>').join(', ');
    const provenance: TypeProvenance[] = coerced.map(([position, kind]) => ({
      path: position,
      kind,
      reason: 'coerced_input',
      detail:
        `the schema accepts any value here and coerces it, so the published type ` +
        `is what parsing produces, not a limit on what a caller may send`,
    }));

    const text = this.inputWithCoercedMembers(
      input,
      output,
      coerced.map(([position]) => position),
      node,
      where
    );
    if (text) {
      this.log(
        `Route schema entry at ${where} accepts any value at ${positions} on input; ` +
          'publishing the parsed type there and the input everywhere else'
      );
      return { text, provenance };
    }

    const whole = this.structuralTextFromType(output, node);
    if (!whole) {
      return null;
    }
    this.log(
      `Route schema entry at ${where} accepts any value at ${positions} on input, and not ` +
        'every one could be substituted member by member; publishing its whole parsed output'
    );
    return { text: whole, provenance };
  }

  /**
   * Print a schema's input with each coerced position replaced by the output's
   * type at that position (carrick#1105). Null when any position cannot be
   * substituted: its output is missing or differs across union branches, or
   * the printer never reached it. A partial substitution would publish an
   * `unknown`, so it is not an answer.
   */
  private inputWithCoercedMembers(
    input: Type,
    output: Type,
    coerced: string[],
    at: Node,
    where: string
  ): string | null {
    const wanted = new Set(coerced);
    const types = new Map<string, Type>();
    const ambiguous = new Set<string>();
    this.walkTypePositions(output, at, where, (type, position, unionPart) => {
      // A union's parts share its position; the union itself is the type there.
      if (wanted.has(position) && !unionPart) {
        const seen = types.get(position);
        if (!seen) {
          types.set(position, type);
        } else if (seen.compilerType !== type.compilerType) {
          ambiguous.add(position);
        }
      }
      return true;
    });
    for (const position of ambiguous) {
      types.delete(position);
    }
    const missing = coerced.filter((position) => !types.has(position));
    if (missing.length > 0) {
      this.log(
        `Route schema entry at ${where}: no single parsed type at ` +
          `${missing.map((position) => position || '<root>').join(', ')} to substitute ` +
          'for the coerced input'
      );
      return null;
    }

    const root = types.get('');
    if (root) {
      return this.structuralTextFromType(root, at);
    }

    const overrides: MemberOverrides = { types, applied: new Set(), at };
    let text: string;
    try {
      text = expandTypeStructural(this.unwrapPromiseType(input), this.expandOrigin(), {
        overrides,
      });
    } catch {
      return null;
    }
    const unreached = coerced.filter((position) => !overrides.applied.has(position));
    if (unreached.length > 0) {
      this.log(
        `Route schema entry at ${where}: the printer did not reach coerced ` +
          `${unreached.join(', ')} (tuple, cycle or depth bound)`
      );
      return null;
    }
    return this.isUselessType(text) ? null : text;
  }

  /**
   * Member positions inside `root` that are `any` or `unknown`, keyed by the
   * member-path notation the provenance entries use (`''` for the root,
   * `sub.field`, `items<0>` for an array element). Union and intersection
   * members share their parent's position, so `unknown | undefined` on an
   * optional key reads as `unknown` there.
   */
  private topTypePositions(
    root: Type,
    at: Node,
    where: string
  ): Map<string, 'any' | 'unknown'> {
    const found = new Map<string, 'any' | 'unknown'>();
    this.walkTypePositions(root, at, where, (type, position) => {
      if (type.isAny() || type.isUnknown()) {
        found.set(position, type.isAny() ? 'any' : 'unknown');
        return false;
      }
      return true;
    });
    return found;
  }

  /**
   * Visit every member position of `root`, in the notation `topTypePositions`
   * documents. `visit` returns false to stop descending below a position;
   * `unionPart` is true when the type is a union or intersection part visited
   * at its parent's position. Callables are not descended into.
   *
   * Bounded and cycle-safe. It only has to tell a schema's input apart from its
   * output, so a subtree past the bound is not compared, and that is logged:
   * a coercion below the bound prints as its input declares it.
   */
  private walkTypePositions(
    root: Type,
    at: Node,
    where: string,
    visit: (type: Type, position: string, unionPart: boolean) => boolean
  ): void {
    const MAX_DEPTH = 8;
    const MAX_VISITED = 512;
    const onPath = new Set<ts.Type>();
    let visited = 0;
    let bounded = false;

    const walk = (type: Type, position: string, depth: number, unionPart: boolean): void => {
      if (depth > MAX_DEPTH || visited > MAX_VISITED) {
        bounded = true;
        return;
      }
      if (!visit(type, position, unionPart)) {
        return;
      }
      const compilerType = type.compilerType;
      if (onPath.has(compilerType)) {
        return;
      }
      onPath.add(compilerType);
      visited += 1;
      try {
        const parts = type.isUnion()
          ? type.getUnionTypes()
          : type.isIntersection()
            ? type.getIntersectionTypes()
            : undefined;
        if (parts) {
          for (const part of parts) {
            walk(part, position, depth + 1, true);
          }
          return;
        }
        if (type.isArray()) {
          const element = type.getArrayElementType();
          if (element) {
            walk(element, `${position}<0>`, depth + 1, false);
          }
          return;
        }
        if (!type.isObject() || this.isCallableType(type)) {
          return;
        }
        for (const property of type.getProperties()) {
          let propertyType: Type;
          try {
            propertyType = property.getTypeAtLocation(at);
          } catch {
            continue;
          }
          const name = property.getName();
          walk(
            propertyType,
            position === '' ? name : `${position}.${name}`,
            depth + 1,
            false
          );
        }
      } finally {
        onPath.delete(compilerType);
      }
    };

    walk(root, '', 0, false);
    if (bounded) {
      this.log(
        `Schema type at ${where} is deeper or wider than the position walk bound ` +
          `(depth ${MAX_DEPTH}, ${MAX_VISITED} members); members past it are not ` +
          'compared for coercion'
      );
    }
  }

  /**
   * Follow a schema REFERENCE call (`ref('CreateWidget')`) to the schema value
   * it names.
   *
   * The ref function is produced by a registry-building call — `const { $ref } =
   * build({ CreateWidget, Widget })` — so the registry is that call's first
   * argument, and the referenced key is a property of it. Resolve the callee to
   * its declaration, walk to the variable declaration it is bound in, and read
   * the key off the builder argument's type. Returns undefined for anything
   * that is not this shape.
   */
  private registrySchemaType(call: CallExpression): Type | undefined {
    const args = call.getArguments();
    if (args.length === 0) {
      return undefined;
    }
    const keyArg = this.unwrapExpressionNode(args[0]);
    if (!Node.isStringLiteral(keyArg)) {
      return undefined;
    }
    const key = keyArg.getLiteralValue();

    const callee = call.getExpression();
    const identifier = Node.isIdentifier(callee)
      ? callee
      : Node.isPropertyAccessExpression(callee)
        ? callee.getNameNode()
        : undefined;
    if (!identifier || !Node.isIdentifier(identifier)) {
      return undefined;
    }

    for (const def of identifier.getDefinitionNodes()) {
      const varDecl = Node.isVariableDeclaration(def)
        ? def
        : def.getFirstAncestorByKind(SyntaxKind.VariableDeclaration);
      const initializer = varDecl?.getInitializer();
      if (!initializer) {
        continue;
      }
      const builderCall = this.unwrapExpressionNode(initializer);
      if (!Node.isCallExpression(builderCall)) {
        continue;
      }
      const registryArg = builderCall.getArguments()[0];
      if (!registryArg) {
        continue;
      }
      let registryType: Type;
      try {
        registryType = registryArg.getType();
      } catch {
        continue;
      }
      const schemaSymbol = registryType.getProperty(key);
      if (!schemaSymbol) {
        continue;
      }
      try {
        return schemaSymbol.getTypeAtLocation(registryArg);
      } catch {
        continue;
      }
    }
    return undefined;
  }

  /**
   * The parsed output type of a schema value: the return type of its `parse`
   * method, else a declared `_output` member, else the Standard Schema member
   * `~standard.types.output` (a library that exposes nothing else). Returns
   * undefined when none carries a usable type — including when the schema library's own types are
   * unavailable (an uninstalled dependency resolves the schema to `any`), which
   * must abstain rather than publish `any` as a contract.
   */
  private schemaOutputType(schemaType: Type, at: Node): Type | undefined {
    const usable = (type: Type | undefined): Type | undefined => {
      if (!type) {
        return undefined;
      }
      return this.isUselessType(typeText(type, at)) ? undefined : type;
    };

    const parseSymbol = schemaType.getProperty('parse');
    if (parseSymbol) {
      try {
        const signatures = parseSymbol.getTypeAtLocation(at).getCallSignatures();
        const returned = usable(signatures[0]?.getReturnType());
        if (returned) {
          return returned;
        }
      } catch {
        // fall through to the declared-output member
      }
    }

    const outputSymbol = schemaType.getProperty('_output');
    if (outputSymbol) {
      try {
        const declared = usable(outputSymbol.getTypeAtLocation(at));
        if (declared) {
          return declared;
        }
      } catch {
        // fall through to the Standard Schema member
      }
    }

    return usable(this.standardSchemaType(schemaType, at, 'output'));
  }

  /**
   * The input type of a schema value, what a caller may send (carrick#1101):
   * the Standard Schema member `~standard.types.input`, else a declared
   * `_input` member. Undefined when the schema declares neither.
   *
   * Unlike the output, an input of `unknown` is kept: it is a declared fact
   * (a coercion accepts any value), and `schemaContract` decides what to
   * publish for it. An `any` that is really an unresolved library still reads
   * as no input, because an unresolved schema type has no members to read.
   */
  private schemaInputType(schemaType: Type, at: Node): Type | undefined {
    const standard = this.standardSchemaType(schemaType, at, 'input');
    if (standard) {
      return standard;
    }
    const inputSymbol = schemaType.getProperty('_input');
    if (!inputSymbol) {
      return undefined;
    }
    try {
      return inputSymbol.getTypeAtLocation(at);
    } catch {
      return undefined;
    }
  }

  /**
   * `~standard.types.<side>` on a schema value: the Standard Schema
   * specification's library-neutral statement of a schema's input and output
   * types. `types` is declared optional, so its `undefined` is stripped before
   * the side is read.
   */
  private standardSchemaType(
    schemaType: Type,
    at: Node,
    side: SchemaDirection
  ): Type | undefined {
    try {
      const standard = schemaType.getProperty('~standard');
      const types = standard?.getTypeAtLocation(at).getProperty('types');
      const member = types
        ?.getTypeAtLocation(at)
        .getNonNullableType()
        .getProperty(side);
      return member?.getTypeAtLocation(at);
    } catch {
      return undefined;
    }
  }

  /**
   * Find a node by matching expression text near a target line.
   *
   * Strategy:
   * 1. Get all descendant nodes within [lineNumber - searchRadius, lineNumber + searchRadius]
   * 2. Normalize whitespace for comparison
   * 3. Try exact match first (after normalization), prefer closest to target line
   * 4. Fall back to substring match (LLM text in node text, or vice versa)
   * 5. Return smallest matching node closest to target line
   */
  private findNodeByText(
    sourceFile: SourceFile,
    expressionText: string,
    lineNumber?: number,
    searchRadius: number = 5
  ): Node | undefined {
    // Only the window's nodes are wrapped: `matchByText` keeps no other.
    const inWindow = lineNumber
      ? descendantsOnLines(sourceFile, lineNumber - searchRadius, lineNumber + searchRadius)
      : descendantsOf(sourceFile);
    const nodes = inWindow
      .filter((entry) => entry.kind !== SyntaxKind.SyntaxList)
      .map((entry) => wrap(sourceFile, entry));
    return this.matchByText(nodes, expressionText, lineNumber, searchRadius);
  }

  /**
   * Find a CallExpression by matching expression text near a target line.
   */
  private findCallExpressionByText(
    sourceFile: SourceFile,
    expressionText: string,
    lineNumber?: number,
    searchRadius: number = 5
  ): CallExpression | undefined {
    const inWindow = lineNumber
      ? callsOf(sourceFile).filter(
          (entry) =>
            entry.line >= lineNumber - searchRadius && entry.line <= lineNumber + searchRadius
        )
      : callsOf(sourceFile);
    const callExpressions = inWindow.map((entry) => wrap<CallExpression>(sourceFile, entry));
    return this.matchByText(callExpressions, expressionText, lineNumber, searchRadius) as
      | CallExpression
      | undefined;
  }

  /**
   * Shared text-matching logic for node resolution.
   * Normalizes whitespace once per candidate, then tries exact match,
   * then substring match (preferring containing matches).
   */
  private matchByText<T extends Node>(
    nodes: T[],
    expressionText: string,
    lineNumber?: number,
    searchRadius: number = 5
  ): T | undefined {
    const normalizedTarget = this.normalizeWhitespace(expressionText);
    if (!normalizedTarget) return undefined;

    // Filter to nodes within the search window and pre-compute normalized text
    const candidates = (
      lineNumber
        ? nodes.filter((node) => {
            const nodeLine = startLineOf(node);
            return nodeLine >= lineNumber - searchRadius && nodeLine <= lineNumber + searchRadius;
          })
        : nodes
    ).map((node) => ({ node, text: this.normalizeWhitespace(node.getText()) }));

    if (candidates.length === 0) return undefined;

    // Try exact match (normalized whitespace)
    const exactMatches = candidates.filter((c) => c.text === normalizedTarget);
    if (exactMatches.length > 0) {
      return this.pickBestMatch(exactMatches.map((c) => c.node), lineNumber) as T;
    }

    // A bare identifier target must match a node exactly: substring matching
    // would bind `users` to `usersCsv` (or to any enclosing node that merely
    // contains the identifier somewhere) and report a confidently wrong type.
    // Failing here is correct — the caller records an error and the alias
    // pads to `unknown` downstream.
    if (/^[A-Za-z_$][A-Za-z0-9_$]*$/.test(normalizedTarget)) {
      return undefined;
    }

    // A locator that elides members (`body: { a: x, ... }`) stands for the
    // whole node it opens: the text before the elision is where the source
    // opens. Never a node the text merely contains, which is one member of the
    // payload it names.
    // An opening shorter than this (`{`) names no node in particular.
    const MIN_ELIDED_OPENING_LEN = 8;
    const elidedOpening = this.elidedLocatorOpening(normalizedTarget);
    if (elidedOpening !== undefined) {
      if (elidedOpening.length < MIN_ELIDED_OPENING_LEN) return undefined;
      const opened = candidates.filter((c) => c.text.startsWith(elidedOpening));
      if (opened.length === 0) return undefined;
      if (lineNumber !== undefined) {
        const distance = (c: { node: T }) => Math.abs(startLineOf(c.node) - lineNumber);
        const nearest = Math.min(...opened.map(distance));
        return this.pickBestMatch(
          opened.filter((c) => distance(c) === nearest).map((c) => c.node),
          lineNumber
        ) as T;
      }
      return this.pickBestMatch(opened.map((c) => c.node), lineNumber) as T;
    }

    // Fall back to substring match
    // For the reverse direction (target contains node text), require a minimum node text
    // length to avoid matching tiny identifiers like "res" or "body" too broadly.
    // Also require the node to cover at least half of the target: the reverse
    // branch exists for a locator with minor syntactic drift around the true
    // node, not for binding to a fragment. Without the floor, a 100+ char
    // locator whose exact match failed would bind to the smallest embedded
    // sub-expression (pickBestMatch prefers small nodes), e.g. the 8-char
    // literal "active" inside a res.json object payload (#335).
    //
    // A node the text itself places strictly inside what it names is a
    // fragment, however much of the text it covers (carrick#2056): the
    // `new URLSearchParams(…)` inside a call's URL argument, or the object a
    // callback nested in the named object returns. Only the node the text
    // names, or a wrapper the text adds around it, may be bound this way. A
    // fragment still points at what the text names where the source agrees
    // with the text up to it: the enclosing node of the named kind whose text
    // opens with the text's own opening up to the fragment (`new Response(`
    // before a payload, when the text dropped the call's second argument).
    const MIN_REVERSE_MATCH_LEN = 8;
    const MIN_REVERSE_MATCH_COVERAGE = 0.5;
    const containingMatches = candidates.filter((c) => c.text.includes(normalizedTarget));
    if (containingMatches.length > 0) {
      // A text that is only a wrapped call's first line (`load({`) is held by
      // every sibling that opens alike, and the smallest of them is not the
      // one the locator names (carrick#2114). The nodes whose own text OPENS
      // with the text are the ones a first line names: among those the stated
      // line decides, then size. A fragment from the middle of a node opens
      // nothing, and stays on the smallest node holding it.
      const opening =
        lineNumber === undefined
          ? []
          : containingMatches.filter((c) => c.text.startsWith(normalizedTarget));
      if (lineNumber !== undefined && opening.length > 0) {
        const distance = (c: { node: T }) => Math.abs(startLineOf(c.node) - lineNumber);
        const nearest = Math.min(...opening.map(distance));
        return this.pickBestMatch(
          opening.filter((c) => distance(c) === nearest).map((c) => c.node),
          lineNumber
        ) as T;
      }
      return this.pickBestMatch(containingMatches.map((c) => c.node), lineNumber) as T;
    }
    const reverseMatches = candidates.filter(
      (c) =>
        c.text.length >= MIN_REVERSE_MATCH_LEN &&
        c.text.length >= normalizedTarget.length * MIN_REVERSE_MATCH_COVERAGE &&
        normalizedTarget.includes(c.text)
    );
    if (reverseMatches.length === 0) return undefined;

    const named = this.locatorNames(normalizedTarget);
    if (!named) {
      return this.pickBestMatch(reverseMatches.map((c) => c.node), lineNumber) as T;
    }
    const textOf = new Map<Node, string>(candidates.map((c) => [c.node, c.text]));
    const bound = new Set<Node>();
    for (const c of reverseMatches) {
      if (named.texts.has(c.text)) {
        bound.add(c.node);
        continue;
      }
      const at = normalizedTarget.indexOf(c.text, named.rootStart + 1);
      if (at < 0) continue;
      const opening = normalizedTarget.slice(named.rootStart, at);
      const enclosing = c.node.getFirstAncestor(
        (ancestor) =>
          ancestor.getKind() === named.rootKind &&
          textOf.get(ancestor)?.startsWith(opening) === true
      );
      if (enclosing) bound.add(enclosing);
    }
    return bound.size > 0 ? (this.pickBestMatch([...bound], lineNumber) as T) : undefined;
  }

  /**
   * The text a locator opens with before an elision, or `undefined` when the
   * locator has none. An elision is `...` followed only by closing brackets
   * (`{ a: x, ... }`); `...rest` is a spread and source text. An opening too
   * short to tell nodes apart is judged by the caller.
   */
  private elidedLocatorOpening(normalizedTarget: string): string | undefined {
    const match = /^(.*?)[\s,]*\.\.\.[\s,)}\]]*$/s.exec(normalizedTarget);
    if (!match) return undefined;
    return match[1];
  }

  /**
   * What a locator's text names, read off its own parse (carrick#2056):
   *
   *  - `texts`: the normalized text of the expression it names and of each
   *    wrapper it carries around it (`return`, a `const x =` binding, a label,
   *    `await`, parentheses, a cast, `satisfies`, `!`). A node strictly inside
   *    that expression is not among them;
   *  - `rootKind` and `rootStart`: the innermost of those, its kind, and where
   *    its text starts in the locator.
   *
   * `null` when the text does not parse as one expression or one statement:
   * there is no structure to read, and the caller keeps its text rule.
   */
  private locatorNames(
    normalizedTarget: string
  ): { texts: Set<string>; rootKind: SyntaxKind; rootStart: number } | null {
    const parsed = (source: string): ts.SourceFile | null => {
      const file = ts.createSourceFile('locator.ts', source, ts.ScriptTarget.Latest, true);
      const diagnostics = (file as unknown as { parseDiagnostics?: readonly unknown[] })
        .parseDiagnostics;
      return diagnostics && diagnostics.length === 0 && file.statements.length === 1 ? file : null;
    };

    // An object literal is a block as a statement, so try the text as an
    // expression first.
    const EXPRESSION_OPENING = '(\n';
    const asExpression = parsed(`${EXPRESSION_OPENING}${normalizedTarget}\n);`);
    let node: ts.Node | undefined;
    let file: ts.SourceFile | null = asExpression;
    let offset = 0;
    if (asExpression) {
      const statement = asExpression.statements[0];
      if (ts.isExpressionStatement(statement) && ts.isParenthesizedExpression(statement.expression)) {
        node = statement.expression.expression;
        offset = EXPRESSION_OPENING.length;
      }
    } else {
      file = parsed(normalizedTarget);
      node = file?.statements[0];
    }
    if (!file || !node) return null;

    const texts = new Set<string>();
    let root: ts.Node | undefined;
    while (node) {
      if (ts.isLabeledStatement(node)) {
        node = node.statement;
      } else if (ts.isExpressionStatement(node) || ts.isReturnStatement(node)) {
        node = node.expression;
      } else if (ts.isVariableStatement(node)) {
        const declarations = node.declarationList.declarations;
        node = declarations.length === 1 ? declarations[0].initializer : undefined;
      } else {
        texts.add(this.normalizeWhitespace(node.getText(file)));
        root = node;
        node =
          ts.isParenthesizedExpression(node) ||
          ts.isAwaitExpression(node) ||
          ts.isAsExpression(node) ||
          ts.isSatisfiesExpression(node) ||
          ts.isNonNullExpression(node) ||
          ts.isTypeAssertionExpression(node)
            ? node.expression
            : undefined;
      }
    }
    if (!root) return null;
    return { texts, rootKind: root.kind, rootStart: root.getStart(file) - offset };
  }

  /**
   * Pick the best match from a set of candidate nodes:
   * smallest range, then closest to target line.
   */
  private pickBestMatch(nodes: Node[], targetLine?: number): Node {
    return nodes.reduce((best, current) => {
      const bestRange = best.getEnd() - best.getStart();
      const currentRange = current.getEnd() - current.getStart();

      // Prefer smaller nodes
      if (currentRange !== bestRange) {
        return currentRange < bestRange ? current : best;
      }

      // Tie-break by proximity to target line
      if (targetLine !== undefined) {
        const bestDist = Math.abs(startLineOf(best) - targetLine);
        const currentDist = Math.abs(startLineOf(current) - targetLine);
        return currentDist < bestDist ? current : best;
      }

      return best;
    });
  }

  /**
   * Normalize whitespace for text comparison:
   * collapse runs of whitespace into single spaces, trim.
   *
   * Also strips trailing commas before `}` `)` `]`: multi-line source
   * literals carry them but the LLM's single-line locator print does not,
   * and that one comma used to defeat exact AND containment matching for
   * the payload and every enclosing node (#335). Applied symmetrically to
   * node text and target, so both sides compare equal.
   *
   * Also strips the space left AFTER `(` `[` `{` by the collapse: a
   * multi-line call whose arguments start on the next line normalizes to
   * `f( x)` while the LLM's compact print is `f(x)`, and that one space
   * defeated exact matching for the call and every enclosing node — the
   * opening-delimiter mirror of the #335 trailing comma (#336). Symmetric
   * for the same reason.
   */
  private normalizeWhitespace(text: string): string {
    const collapsed = text
      .replace(/\s+/g, ' ')
      // A member chain broken before its dot (`client\n  .list(…)`) reads the
      // same as `client.list(…)`; a space around `.` / `?.` means nothing
      // (carrick#1162).
      .replace(/\s*(\?\.|\.)\s*/g, '$1')
      .replace(/\s*,?\s*([}\)\]])/g, '$1')
      .replace(/([\(\[{])\s+/g, '$1')
      .trim();
    // A string literal reads the same in any quotes (carrick#2056).
    return canonicalStringQuotes(collapsed);
  }

  /**
   * Format a human-readable location string for error messages.
   */
  private formatRequestLocation(request: InferRequestItem): string {
    return request.expression_text
      ? `text="${request.expression_text}" line=${request.expression_line ?? '?'}`
      : `${request.span_start}-${request.span_end}`;
  }

  // ===========================================================================
  // Span-Based Node Lookup (SWC byte offsets)
  // ===========================================================================

  private findContainingFunctionBySpan(
    sourceFile: SourceFile,
    spanStart: number,
    spanEnd: number
  ): FunctionLike | undefined {
    const functions = sourceFile.getDescendants().filter(
      (node): node is FunctionLike =>
        Node.isFunctionDeclaration(node) ||
        Node.isArrowFunction(node) ||
        Node.isFunctionExpression(node) ||
        Node.isMethodDeclaration(node)
    );

    const containing = functions.filter((func) => {
      const start = func.getStart();
      const end = func.getEnd();
      return start <= spanStart && spanEnd <= end;
    });

    if (containing.length === 0) {
      return undefined;
    }

    // Return innermost function
    return containing.reduce((innermost, current) => {
      const innermostRange = innermost.getEnd() - innermost.getStart();
      const currentRange = current.getEnd() - current.getStart();
      return currentRange < innermostRange ? current : innermost;
    });
  }

  private findNodeAtSpan(
    sourceFile: SourceFile,
    spanStart: number,
    spanEnd: number
  ): Node | undefined {
    const containing = descendantsOf(sourceFile).filter(
      // Skip SyntaxList nodes as they have unreliable types
      (entry) =>
        entry.kind !== SyntaxKind.SyntaxList && entry.start <= spanStart && spanEnd <= entry.end
    );

    if (containing.length === 0) {
      return undefined;
    }

    const best = containing.reduce((best, current) => {
      const bestRange = best.end - best.start;
      const currentRange = current.end - current.start;

      // Prefer exact matches, then smallest containing
      const bestDelta = Math.abs(bestRange - (spanEnd - spanStart));
      const currentDelta = Math.abs(currentRange - (spanEnd - spanStart));
      if (currentDelta !== bestDelta) {
        return currentDelta < bestDelta ? current : best;
      }
      // carrick#1166: on a tie keep the INNER node. Without a trailing
      // semicolon a statement spans exactly the bytes of its expression, and
      // the statement's own type is `any`, so a registration located by its
      // span published `any` as its request body. Descendants follow their
      // ancestors in `getDescendants()` order, so a tied later node inside the
      // current best is the deeper one.
      return current.start >= best.start && current.end <= best.end ? current : best;
    });
    return wrap(sourceFile, best);
  }

  private findCallExpressionAtSpan(
    sourceFile: SourceFile,
    spanStart: number,
    spanEnd: number
  ): CallExpression | undefined {
    // Tolerate the SWC span convention (#336): the scanner sends raw SWC
    // BytePos offsets, which are 1-based (BytePos(1) is the file's first
    // byte), while ts-morph offsets are 0-based — so an SWC-sourced span
    // sits one byte past the call on BOTH ends. Under strict containment the
    // shifted end overshoots the call's own end, the true call is excluded,
    // and the smallest ENCLOSING call wins: for a data call inside a route
    // registration that is the whole `router.get("/status", handler)`
    // expression, anchoring the router type instead of the payload. The
    // slack admits both conventions; the size-delta pick below still prefers
    // the call whose extent matches the span, so the exactly-covered inner
    // call always beats its enclosing registration.
    //
    // Both producers of a span request now convert at the request boundary
    // (carrick#805, carrick#806), so nothing sends the other convention any
    // more and the slack only widens the candidate set. Removing it is
    // carrick#823, which wants its own before/after.
    const SPAN_SLACK = 2;
    const candidates = callsOf(sourceFile).filter(
      (entry) => entry.start - SPAN_SLACK <= spanStart && spanEnd <= entry.end + SPAN_SLACK
    );

    if (candidates.length === 0) {
      return undefined;
    }

    const best = candidates.reduce((best: IndexedNode, current) => {
      const bestRange = best.end - best.start;
      const currentRange = current.end - current.start;

      const bestDelta = Math.abs(bestRange - (spanEnd - spanStart));
      const currentDelta = Math.abs(currentRange - (spanEnd - spanStart));

      return currentDelta < bestDelta ? current : best;
    });
    return wrap<CallExpression>(sourceFile, best);
  }

  // ===========================================================================
  // Type Utilities
  // ===========================================================================

  private unwrapPromise(typeString: string, type: Type): string {
    // Operate per top-level union member: a naive `^Promise<(.+)>$` regex
    // matches the WHOLE of `Promise<A> | Promise<B>` and produces the
    // mangled capture `A> | Promise<B`.
    const parts = this.splitTopLevelUnion(typeString);
    const unwrapped = parts.map((part) => this.unwrapPromiseText(part));
    if (unwrapped.some((u, i) => u !== parts[i])) {
      return [...new Set(unwrapped)].join(' | ');
    }

    // Handle nested Promise via type arguments
    const typeArguments = type.getTypeArguments();
    if (
      typeArguments.length > 0 &&
      (typeString.startsWith('Promise<') || typeString.startsWith('PromiseLike<'))
    ) {
      return typeText(typeArguments[0]);
    }

    return typeString;
  }

  /**
   * Unwrap a single `Promise<...>` / `PromiseLike<...>` type string, only when
   * the inner text is bracket-balanced (so `Promise<A> | B` is left alone for
   * the caller's union handling rather than mangled).
   */
  private unwrapPromiseText(part: string): string {
    const prefix = part.startsWith('Promise<')
      ? 'Promise<'
      : part.startsWith('PromiseLike<')
        ? 'PromiseLike<'
        : null;
    if (prefix === null || !part.endsWith('>')) {
      return part;
    }
    const inner = part.slice(prefix.length, -1);
    return this.isBracketBalanced(inner) ? inner : part;
  }

  /**
   * Split a type string on `|` at bracket depth 0. `=>` is not treated as a
   * closing bracket.
   */
  private splitTopLevelUnion(typeString: string): string[] {
    const parts: string[] = [];
    let depth = 0;
    let start = 0;
    for (let i = 0; i < typeString.length; i++) {
      const ch = typeString[i];
      if (ch === '>' && typeString[i - 1] === '=') continue;
      if (ch === '<' || ch === '(' || ch === '[' || ch === '{') depth++;
      else if (ch === '>' || ch === ')' || ch === ']' || ch === '}') depth--;
      else if (ch === '|' && depth === 0) {
        parts.push(typeString.slice(start, i).trim());
        start = i + 1;
      }
    }
    parts.push(typeString.slice(start).trim());
    return parts;
  }

  private isBracketBalanced(text: string): boolean {
    let depth = 0;
    for (let i = 0; i < text.length; i++) {
      const ch = text[i];
      if (ch === '>' && text[i - 1] === '=') continue;
      if (ch === '<' || ch === '(' || ch === '[' || ch === '{') depth++;
      else if (ch === '>' || ch === ')' || ch === ']' || ch === '}') depth--;
      if (depth < 0) return false;
    }
    return depth === 0;
  }

  // ===========================================================================
  // Result Building
  // ===========================================================================

  private getNodeLocation(node: Node): SourceLocation {
    const startLinePos = startLineOf(node);
    const endLinePos = endLineOf(node);

    const location = {
      file_path: node.getSourceFile().getFilePath(),
      start_line: startLinePos,
      end_line: endLinePos,
      start_column: node.getStart() - node.getStartLinePos(),
      end_column: node.getEnd() - node.getStartLinePos(),
    };
    this.readNodes.set(location, node);
    return location;
  }

  private createInferredType(
    request: InferRequestItem,
    typeString: string,
    isExplicit: boolean,
    sourceLocation: SourceLocation,
    payloadTypeString?: string,
    primaryTypeSymbol?: string,
    arrayDepth?: number,
    primaryTypeSymbolSource?: string
  ): InferredType {
    const alias =
      request.alias ||
      this.generateAlias(request.file_path, request.line_number, request.infer_kind);

    return {
      alias,
      type_string: typeString,
      is_explicit: isExplicit,
      source_location: sourceLocation,
      infer_kind: request.infer_kind,
      payload_type_string: payloadTypeString,
      primary_type_symbol: primaryTypeSymbol,
      // Reported whether or not the element has a symbol (carrick#1967): an
      // array whose element prints structurally is still a list. The Rust
      // depth join copies it onto the model's symbol when the symbols agree,
      // or, at a handler's send, when the element has no symbol to disagree.
      array_depth: arrayDepth !== undefined && arrayDepth > 0 ? arrayDepth : undefined,
      // Same gate: a declaration source without an anchor symbol is useless.
      primary_type_symbol_source:
        primaryTypeSymbol !== undefined ? primaryTypeSymbolSource : undefined,
    };
  }

  private generateAlias(
    filePath: string,
    lineNumber: number,
    inferKind: InferKind
  ): string {
    const fileName = filePath
      .split('/')
      .pop()
      ?.replace(/\.(ts|tsx|js|jsx)$/, '') || 'unknown';

    const pascalName = fileName
      .split(/[-_.]/)
      .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
      .join('');

    const suffix = this.inferKindSuffix(inferKind);
    return `${pascalName}${suffix}L${lineNumber}`;
  }

  private inferKindSuffix(inferKind: InferKind): string {
    switch (inferKind) {
      case 'function_return':
        return 'Return';
      case 'receiver_type':
        return 'Receiver';
      case 'response_body':
        return 'Response';
      case 'call_result':
        return 'Result';
      case 'variable':
        return 'Var';
      case 'expression':
        return 'Expr';
      case 'request_body':
        return 'Request';
      case 'signature_return':
        return 'SigReturn';
      case 'function_param':
        return 'Param';
      case 'request_config':
        return 'RequestConfig';
      default:
        return 'Type';
    }
  }

  // ===========================================================================
  // Logging
  // ===========================================================================

  private log(message: string): void {
    console.error(`[sidecar:type-inferrer] ${message}`);
  }

  private logError(message: string): void {
    console.error(`[sidecar:type-inferrer:error] ${message}`);
  }
}

/**
 * What is left of one batch's unwidened reading allowance (carrick#2629),
 * shared by the responses' rewrite and the bodies' after it.
 */
interface UnwidenedWork {
  readings: number;
  files: number;
  /** The `now()` past which the reading stops: the wall-clock backstop. */
  backstopAt: number;
  step: UnwidenedStepDone;
}

/** A node kept by its place, for a reading made after its file was rewritten. */
interface SavedSpan {
  file: SourceFile;
  start: number;
  end: number;
  kind: SyntaxKind;
}

/** The node a `SavedSpan` was taken from, in the file as it reads now. */
function nodeAtSpan(span: SavedSpan): Node | undefined {
  let node: Node | undefined = span.file.getDescendantAtPos(span.start);
  while (
    node &&
    !(node.getStart() === span.start && node.getEnd() === span.end && node.getKind() === span.kind)
  ) {
    node = node.getParent();
  }
  return node;
}

/** `undefined`, `null` and `void`: what a labelled read removes (carrick#2818). */
function isNullishType(type: Type): boolean {
  return type.isUndefined() || type.isNull() || type.getText() === 'void';
}
