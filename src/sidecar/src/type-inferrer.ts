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

import {
  Project,
  SourceFile,
  Node,
  SyntaxKind,
  type FunctionDeclaration,
  type ArrowFunction,
  type FunctionExpression,
  type MethodDeclaration,
  type CallExpression,
  type ObjectLiteralExpression,
  type ParameterDeclaration,
  type PropertyAssignment,
  type Type,
  type Symbol as TsSymbol,
  ts,
} from 'ts-morph';
import type {
  InferRequestItem,
  InferResult,
  InferredType,
  InferKind,
  SourceLocation,

  ExtractionConfig,
  ExtractionRule,
} from './types.js';
import { validateInferRequestItem } from './validators.js';
import { expandTypeStructural } from './type-structural-expander.js';

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
 * reply object. A type declared in a lib or `node_modules` origin that carries
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
  return type.getText(enclosingNode, TYPE_TEXT_FLAGS);
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
 * Options for TypeInferrer construction
 */
export interface TypeInferrerOptions {
  /** The ts-morph Project instance */
  project: Project;
}

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
 * TypeInferrer - Extracts types from source code, both explicit and inferred
 *
 * Usage:
 *   const inferrer = new TypeInferrer({ project });
 *   const result = inferrer.infer(requests, extractionConfig);
 */
export class TypeInferrer {
  private readonly project: Project;

  constructor(options: TypeInferrerOptions) {
    this.project = options.project;
  }

  /**
   * Infer types for the given requests
   *
   * @param requests - Array of inference requests
   * @param extractionConfig - Agent-generated extraction config for payload unwrapping
   * @returns InferResult with inferred types or errors
   */
  infer(
    requests: InferRequestItem[],
    extractionConfig?: ExtractionConfig
  ): InferResult {
    const inferredTypes: InferredType[] = [];
    const errors: string[] = [];

    for (const request of requests) {
      // Plain JavaScript has no type annotations to extract, and `checkJs` is
      // off, so inferring against a `.js` file yields nothing useful — it only
      // crashes deep in the compiler API on undefined symbols (`escapedName`,
      // `flags`) and floods the log with the resulting error strings. Skip it.
      // `allowJs` stays on so `.ts` files can still resolve `.js` imports.
      if (/\.(js|jsx|mjs|cjs)$/i.test(request.file_path)) {
        continue;
      }
      try {
        const loc = this.formatRequestLocation(request);
        const itemError = validateInferRequestItem(request);
        if (itemError) {
          errors.push(
            `Invalid infer item at ${request.file_path}:${loc} (${request.infer_kind}): ${itemError}`
          );
          continue;
        }
        const result = this.inferSingle(request, extractionConfig);
        if (result) {
          inferredTypes.push(result);
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
      }
    }

    return {
      success: errors.length === 0 || inferredTypes.length > 0,
      inferred_types: inferredTypes.length > 0 ? inferredTypes : undefined,
      errors: errors.length > 0 ? errors : undefined,
    };
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
        return this.inferResponseBody(sourceFile, request, extractionConfig);
      case 'call_result':
        return this.inferCallResult(sourceFile, request, extractionConfig);
      case 'variable':
        return this.inferVariable(sourceFile, request, extractionConfig);
      case 'expression':
        return this.inferExpression(sourceFile, request, extractionConfig);
      case 'request_body':
        return this.inferRequestBody(sourceFile, request, extractionConfig);
      case 'signature_return':
        return this.inferSignatureReturn(sourceFile, request);
      case 'function_param':
        return this.inferFunctionParam(sourceFile, request);
      default:
        this.logError(`Unknown infer kind: ${request.infer_kind}`);
        return null;
    }
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

  private inferFunctionReturn(
    sourceFile: SourceFile,
    request: InferRequestItem,
    extractionConfig?: ExtractionConfig
  ): InferredType | null {
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
    const unwrapResult = this.unwrapTypeWithConfig(awaitedType, func, extractionConfig);
    if (unwrapResult.wasUnwrapped) {
      typeString = unwrapResult.typeString;
    } else {
      // No wrapper rule fired. Before treating the raw return as the contract,
      // reject a wrapper envelope that IS or CONTAINS framework machinery
      // (carrick#371): `withApiWrapper({ handler: () => ({ response: Response,
      // error })})` resolves the handler's return to `{ response: Response; ... }`,
      // which is transport machinery, not a payload. Emitting it manufactures a
      // false compat mismatch against the consumer's real payload. No rule
      // recovered a payload here, so abstain (honest `unknown`) — fail-closed.
      if (this.typeIsOrContainsResponseMachinery(awaitedType)) {
        this.log(
          `Return type at ${request.file_path}:${request.line_number} is or contains ` +
            'framework machinery (Response/Request-shaped); abstaining rather than ' +
            'capturing the wrapper envelope as a response contract'
        );
        return null;
      }
      // No wrapper rule fired: the (awaited) return resolved to its own type.
      // A named object return (`async (): Promise<Payment> => …`) renders as the
      // bare name `Payment`, which dangles in the source-less cross-repo bundle.
      // Expand the resolved object structurally so the real members reach the
      // bundle. `unwrapPromise` below is then a no-op on the structural form.
      typeString = this.expandResolvedTypeStructural(awaitedType, typeString);
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
    return this.createInferredType(
      request,
      typeString,
      isExplicit,
      this.getNodeLocation(func),
      unwrapResult.wasUnwrapped ? unwrapResult.typeString : undefined,
      this.primaryTypeSymbol(anchor.element),
      anchor.depth
    );
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
    const typeString = typeText(func.getReturnType(), func);

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
    const paramType = target.node.getType();
    const typeString = typeText(paramType, target.node);

    // Deterministic anchor for the pub/sub two-anchor arbitration
    // (carrick#413): report the payload type's root symbol, its declaration
    // file, and the peeled array depth. The type STRING deliberately keeps
    // ts-morph's default named form (see the doc comment above) — the
    // scanner re-aims the explicit bundle at the root symbol instead of
    // pasting this string, so the alias's definition stays self-contained.
    const anchor = this.unwrapArrayLevels(this.unwrapPromiseType(paramType));
    return this.createInferredType(
      request,
      typeString,
      isExplicit,
      this.getNodeLocation(target.node),
      undefined,
      this.primaryTypeSymbol(anchor.element),
      anchor.depth,
      this.primaryTypeSymbolSource(anchor.element)
    );
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
      // No locator, or locator didn't resolve — likely a payload-less handler
      // (redirect, 204, streaming). Infer the containing function's return type.
      this.log(
        `No payload node found for request at ${request.file_path}:${request.line_number}; falling back to function return`
      );
      return this.inferFunctionReturn(sourceFile, request, extractionConfig);
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
      const registersCallback = args.some(
        (arg) => Node.isArrowFunction(arg) || Node.isFunctionExpression(arg)
      );
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
      if (args.length > 0) {
        payloadNode = args[0];
      }
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
      // Same carrick#371 fail-closed guard as the function-return path: a
      // payload that IS or CONTAINS framework machinery (a raw `Response`, a
      // `{ response: Response; ... }` envelope) is not a contract. No rule
      // recovered a payload, so abstain rather than emit the machinery.
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
      typeString = this.expandResolvedTypeStructural(resolved, typeString);
    }

    const anchor = this.unwrapArrayLevels(this.unwrapPromiseType(payloadType));
    return this.createInferredType(
      request,
      typeString,
      false,
      this.getNodeLocation(payloadNode),
      unwrapResult.wasUnwrapped ? unwrapResult.typeString : undefined,
      this.primaryTypeSymbol(anchor.element),
      anchor.depth
    );
  }

  private inferCallResult(
    sourceFile: SourceFile,
    request: InferRequestItem,
    extractionConfig?: ExtractionConfig
  ): InferredType | null {
    const callExpr = this.resolveTargetCallExpression(sourceFile, request);

    if (!callExpr) {
      return this.inferExpression(sourceFile, request, extractionConfig);
    }

    // Walk up from the already-found call expression instead of re-searching
    const func = this.findContainingFunctionForNode(callExpr);
    const terminalNode = this.resolveCallResultTerminalNode(callExpr, func);
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

    const explicitType = this.extractExplicitTypeFromAncestor(terminalNode);
    if (explicitType) {
      typeString = explicitType;
      isExplicit = true;
    }

    typeString = this.unwrapPromise(typeString, returnType);

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
    const callPayloadType = this.unwrapPromiseType(callExpr.getType());
    const callUnwrap = this.unwrapTypeWithConfig(
      callPayloadType,
      callExpr,
      extractionConfig
    );
    const anchorSource = callUnwrap.wasUnwrapped
      ? callUnwrap.payloadType
      : callPayloadType;
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

    return this.createInferredType(
      request,
      typeString,
      isExplicit,
      this.getNodeLocation(terminalNode),
      unwrapResult.wasUnwrapped ? unwrapResult.typeString : undefined,
      anchor ? this.primaryTypeSymbol(anchor.element) : undefined,
      anchor?.depth
    );
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
        const requestType = this.requestContractFromRegistration(
          atLine.registration,
          atLine.handler
        );
        if (requestType) {
          return this.createInferredType(
            request,
            requestType,
            true,
            this.getNodeLocation(atLine.handler),
            undefined,
            undefined
          );
        }
      }
      return null;
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
        return this.createInferredType(
          request,
          requestType,
          true,
          this.getNodeLocation(registrationHandler),
          undefined,
          undefined
        );
      }
      // A genuinely payload-less handler (no typed request read): do NOT fall
      // through to read the registration call's return type, which would emit a
      // spurious framework type. Report unresolved, as before this capability.
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
    // exactly as it does on the response side, so read anchors (a) and (b)
    // first and fall through to the expression only when the route declares
    // its request nowhere. The third registration anchor — the typed request
    // READ inside the handler body — is deliberately not consulted here: that
    // one is itself an expression, so the locator's own expression stays
    // authoritative when nothing is declared.
    const declaredAt = this.registrationAtLine(sourceFile, request.line_number);
    if (declaredAt) {
      const declared =
        this.requestBodyFromHandlerParams(declaredAt.handler) ??
        this.routeSchemaContractText(declaredAt.registration, 'body');
      if (declared) {
        this.log(
          `Route registration at ${request.file_path}:${request.line_number} declares its ` +
            'request contract; using the declaration over the located expression'
        );
        return this.createInferredType(
          request,
          declared,
          true,
          this.getNodeLocation(declaredAt.handler),
          undefined,
          undefined
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

    // Mirror inferCallResult: strip `await`/`as`/parens/`!` so the inner
    // expression's type (not the surrounding `Promise<any>`) is read.
    const unwrapped = this.unwrapExpressionNode(located);

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
  ): Node {
    const returnStmt = callExpr.getFirstAncestorByKind(SyntaxKind.ReturnStatement);
    if (returnStmt) {
      const returnExpr = returnStmt.getExpression();
      if (returnExpr) {
        return returnExpr;
      }
    }

    const binding = this.extractBindingFromCall(callExpr);
    if (binding && func) {
      let currentNames = binding.names;
      let lastNode: Node = binding.node;
      const startPos = callExpr.getStart();
      const candidates = this.collectDefUseNodes(func);

      for (const expr of candidates) {
        if (expr.getStart() <= startPos) continue;

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

        if (this.expressionUsesNames(expr, currentNames)) {
          lastNode = expr;
        }
      }

      return lastNode;
    }

    return callExpr;
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
      let anyUnwrapped = false;

      for (const unionType of unionTypes) {
        const result = this.unwrapType(unionType, node, config, depth + 1);
        unwrappedParts.push(result.typeString);
        if (result.wasUnwrapped) {
          anyUnwrapped = true;
        }
      }

      if (anyUnwrapped) {
        // Dedupe and join. A member that collapsed to `unknown` (verified
        // machinery with no recoverable payload) must not pollute the join —
        // `unknown | User` would read downstream as a real composite type
        // instead of partially-unresolved.
        const unique = [...new Set(unwrappedParts)];
        const informative = unique.filter((part) => part !== 'unknown');
        const parts = informative.length > 0 ? informative : unique;
        return {
          typeString: parts.length === 1 ? parts[0] : parts.join(' | '),
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

    const symbol = type.getSymbol() || type.getAliasSymbol();
    const symbolName = symbol?.getName();

    // 1. Check exact wrapperSymbols match. When the rule also carries
    // originModuleGlobs, the symbol's declaration must come from a matching
    // module — names like `Response` are shared by the DOM, frameworks, and
    // HTTP clients, so a bare name match would unwrap unrelated types.
    if (rule.wrapperSymbols && symbolName && rule.wrapperSymbols.includes(symbolName)) {
      const originGated = !!(rule.originModuleGlobs && rule.originModuleGlobs.length > 0);
      if (!originGated || this.symbolOriginatesFromModules(symbol, rule.originModuleGlobs!)) {
        const extracted = this.extractPayloadFromWrapper(type, node, rule, config, depth);
        if (extracted) {
          return { kind: 'extracted', result: extracted };
        }
        // A name-only match is not proof of machinery: a local type that
        // happens to share the name must keep its real structural type when
        // nothing was extracted. Only origin-verified matches may collapse
        // to `unknown`.
        return originGated ? { kind: 'verified-no-payload' } : { kind: 'no-match' };
      }
      return { kind: 'no-match' };
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

      if (!this.symbolOriginatesFromModules(symbol, rule.originModuleGlobs)) {
        return { kind: 'no-match' };
      }

      const extracted = this.extractPayloadFromWrapper(type, node, rule, config, depth);
      if (extracted) {
        return { kind: 'extracted', result: extracted };
      }
      return { kind: 'verified-no-payload' };
    }

    return { kind: 'no-match' };
  }

  /**
   * Extract the payload type from a matched wrapper. Returns null when the
   * rule matched the wrapper but no payload is recoverable from generics or
   * property paths — the caller decides what a payload-less match means
   * (verified machinery collapses to `unknown` after every rule has run;
   * a name-only match leaves the type untouched).
   */
  private extractPayloadFromWrapper(
    type: Type,
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
    const typeArgs = type.getTypeArguments();

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

      // Try "first useful generic" heuristic
      for (let i = 0; i < typeArgs.length; i++) {
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
        const prop = currentType.getProperty(propName);
        if (!prop) {
          break;
        }
        const propType = prop.getTypeAtLocation(node);
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
   *     is lib/`node_modules` only, so a user-declared subtype reads as a real
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
   * (`lib.dom.d.ts`, ...) or `node_modules` origin. Both gates are required —
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
   * True when the symbol is declared in a TypeScript lib file (`lib.*.d.ts`) or
   * under `node_modules` — i.e. framework/runtime machinery, not user source.
   * Works on a bare checkout: the DOM `Response`/`Request` resolve from the
   * bundled `lib.dom.d.ts` even with no installed dependencies.
   */
  private symbolIsLibOrExternalOrigin(symbol: TsSymbol | undefined): boolean {
    if (!symbol) {
      return false;
    }
    const isExternalPath = (filePath: string): boolean => {
      const normalized = filePath.replace(/\\/g, '/');
      return (
        normalized.includes('/node_modules/') ||
        /\/lib\.[^/]*\.d\.ts$/.test(normalized)
      );
    };
    for (const decl of symbol.getDeclarations()) {
      if (isExternalPath(decl.getSourceFile().getFilePath())) {
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
    const varDecl = node.getFirstAncestorByKind(SyntaxKind.VariableDeclaration);
    if (varDecl) {
      const typeNode = varDecl.getTypeNode();
      if (typeNode) {
        return this.expandAnnotationTypeNode(typeNode);
      }
    }

    // Consider the node ITSELF as well as its ancestors: the `call_result`
    // path's terminal node for `return res.json() as Promise<T>` IS the
    // `as` cast (an ancestor walk from it would miss it), so the #257 consumer
    // shape would never be recovered. `as T` and `<T>x` assertions both apply.
    const asExpr = Node.isAsExpression(node)
      ? node
      : node.getFirstAncestorByKind(SyntaxKind.AsExpression);
    if (asExpr) {
      const typeNode = asExpr.getTypeNode();
      if (typeNode) {
        return this.expandAnnotationTypeNode(typeNode);
      }
    }

    const typeAssertion = Node.isTypeAssertion(node)
      ? node
      : node.getFirstAncestorByKind(SyntaxKind.TypeAssertionExpression);
    if (typeAssertion) {
      const typeNode = typeAssertion.getTypeNode();
      if (typeNode) {
        return this.expandAnnotationTypeNode(typeNode);
      }
    }

    return null;
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
  private expandAnnotationTypeNode(typeNode: Node): string {
    const fallback = typeNode.getText();
    try {
      const annotationType = this.unwrapPromiseType(typeNode.getType());
      const expanded = expandTypeStructural(annotationType);
      // Only prefer the structural form when expansion actually inlined an
      // object shape; otherwise keep the annotation text (e.g. a bare
      // primitive or a library type the expander leaves by name).
      return expanded.startsWith('{') ? expanded : fallback;
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
  private expandResolvedTypeStructural(type: Type, fallback: string): string {
    try {
      const expanded = expandTypeStructural(type);
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
    const name = (type.getSymbol() || type.getAliasSymbol())?.getName();
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
   * has no user-facing anchor or no source declaration. The scanner's
   * pub/sub two-anchor arbitration (carrick#413) uses this to re-aim a
   * demoted explicit bundle request: the bundler resolves a `SymbolRequest`
   * only against declarations IN its `source_file`, so the request must
   * point at the file that actually declares the tsc-witnessed payload type.
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
    const decls = (type.getSymbol() || type.getAliasSymbol())?.getDeclarations() ?? [];
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
   */
  private findFunctionByLine(
    sourceFile: SourceFile,
    line: number
  ): FunctionLike | undefined {
    const LINE_TOLERANCE = 2;
    const functions = sourceFile.getDescendants().filter(
      (node): node is FunctionLike =>
        Node.isFunctionDeclaration(node) ||
        Node.isArrowFunction(node) ||
        Node.isFunctionExpression(node) ||
        Node.isMethodDeclaration(node)
    );

    let best: FunctionLike | undefined;
    let bestDelta = Infinity;
    for (const fn of functions) {
      const delta = Math.abs(fn.getStartLineNumber() - line);
      if (delta > LINE_TOLERANCE) continue;
      const isCloser = delta < bestDelta;
      const isInnermostTie =
        delta === bestDelta &&
        best !== undefined &&
        fn.getEnd() - fn.getStart() < best.getEnd() - best.getStart();
      if (isCloser || isInnermostTie) {
        best = fn;
        bestDelta = delta;
      }
    }
    return best;
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
    for (const node of sourceFile.getDescendants()) {
      if (
        (Node.isCallExpression(node) ||
          Node.isObjectLiteralExpression(node)) &&
        node.getStartLineNumber() === line
      ) {
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
   * The handler function referenced by a route-registration call: the first
   * argument that is an inline function, or an identifier bound to a function
   * (typically the 2nd+ arg — the path literal is not function-valued, so it is
   * skipped naturally). No callee-name check: a call whose argument resolves to
   * a function is structurally a registration regardless of the framework.
   */
  private handlerFromRegistrationCall(
    call: CallExpression
  ): FunctionLike | undefined {
    for (const arg of call.getArguments()) {
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
   */
  private asHandlerFunction(node: Node): FunctionLike | undefined {
    const expr = this.unwrapExpressionNode(node);

    if (Node.isArrowFunction(expr) || Node.isFunctionExpression(expr)) {
      return expr;
    }

    if (Node.isIdentifier(expr)) {
      for (const def of expr.getDefinitionNodes()) {
        const func = this.functionFromDeclaration(def);
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
   * function (`const h = async () => { … }`). Import/re-export shims are walked
   * by ts-morph's `getDefinitionNodes`, so no manual import chasing is needed.
   */
  private functionFromDeclaration(decl: Node): FunctionLike | undefined {
    if (Node.isFunctionDeclaration(decl)) {
      return decl;
    }

    if (Node.isArrowFunction(decl) || Node.isFunctionExpression(decl)) {
      return decl;
    }

    // `getDefinitionNodes()` may return the name identifier of a `const h = …`
    // binding rather than the declaration; walk to the variable declaration.
    const varDecl = Node.isVariableDeclaration(decl)
      ? decl
      : decl.getFirstAncestorByKind(SyntaxKind.VariableDeclaration);
    if (varDecl) {
      const initializer = varDecl.getInitializer();
      if (initializer) {
        const inner = this.unwrapExpressionNode(initializer);
        if (Node.isArrowFunction(inner) || Node.isFunctionExpression(inner)) {
          return inner;
        }
      }
    }

    return undefined;
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
  //      whose entries reference schema VALUES that carry their parsed output
  //      type.
  //
  // Both are read structurally: (a) is "the `body` member of a parameter's
  // type", (b) is "a `schema` property on a registration argument, whose
  // entries resolve to a value whose `parse` returns the contract". No
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
    const declared = this.routeSchemaContractText(registration, 'response');
    if (!declared) {
      return null;
    }
    this.log(
      `Route registration at ${request.file_path}:${request.line_number} declares its ` +
        'response schema; using the declared contract'
    );
    return this.createInferredType(
      request,
      declared,
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
  ): string | null {
    return (
      this.requestBodyFromHandlerParams(handler) ??
      this.routeSchemaContractText(registration, 'body') ??
      this.inferRequestReadFromHandler(handler)
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
      if (!bodySymbol) {
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
   * Anchor (b): the contract declared in the route's validation schema.
   *
   * `part` is `'body'` for the request contract and `'response'` for the
   * response contract; a `response` entry keyed by status code resolves to its
   * success entry. Returns null when the registration carries no schema, when
   * the entry references something whose parsed output cannot be resolved, or
   * when the schema is a plain JSON-schema literal (whose own object type is
   * the JSON-Schema document, not the payload — emitting that would be worse
   * than abstaining).
   */
  private routeSchemaContractText(
    registration: Node,
    part: 'body' | 'response'
  ): string | null {
    const schemaObject = this.routeSchemaObject(registration);
    if (!schemaObject) {
      return null;
    }
    const entry = schemaObject.getProperty(part);
    if (!entry || !Node.isPropertyAssignment(entry)) {
      return null;
    }
    const initializer = entry.getInitializer();
    if (!initializer) {
      return null;
    }
    const value: Node =
      part === 'response'
        ? (this.successStatusEntry(initializer) ?? initializer)
        : initializer;
    return this.schemaOutputTypeText(value);
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
      const name = prop.getName().replace(/['"`]/g, '').toLowerCase();
      // `2xx` is a status RANGE key; treat it as the bottom of its range.
      const code = /^\d{3}$/.test(name)
        ? Number(name)
        : /^\dxx$/.test(name)
          ? Number(name[0]) * 100
          : NaN;
      if (!Number.isFinite(code) || code < 200 || code > 299) {
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
   * The payload type a schema entry declares.
   *
   * The entry is either a REFERENCE call — `ref('CreateWidget')`, a
   * name-to-schema indirection whose registry is the argument the ref function
   * was built from — or the schema value itself. Either way the payload is the
   * schema's parsed output: the return type of its `parse` method (the shape
   * every schema value exposes as its public validate-and-return API), falling
   * back to a declared `_output` member. A value with neither is not a schema
   * and yields null.
   */
  private schemaOutputTypeText(value: Node): string | null {
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

    const output = this.schemaOutputType(schemaType, node);
    if (!output) {
      this.log(
        `Route schema entry at ${this.getNodeLocation(node).file_path}:${
          this.getNodeLocation(node).start_line
        } declares no resolvable parsed output (schema library types unavailable, ` +
          'or a plain JSON-Schema literal); leaving unresolved'
      );
      return null;
    }
    return this.structuralTextFromType(output, node);
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
   * method, else a declared `_output` member. Returns undefined when neither
   * carries a usable type — including when the schema library's own types are
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
        return usable(outputSymbol.getTypeAtLocation(at));
      } catch {
        return undefined;
      }
    }

    return undefined;
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
    const allNodes = sourceFile.getDescendants().filter((node) => {
      if (Node.isSourceFile(node)) return false;
      if (node.getKind() === SyntaxKind.SyntaxList) return false;
      return true;
    });
    return this.matchByText(allNodes, expressionText, lineNumber, searchRadius);
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
    const callExpressions = sourceFile.getDescendantsOfKind(SyntaxKind.CallExpression);
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
            const nodeLine = node.getStartLineNumber();
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

    // Fall back to substring match
    // For the reverse direction (target contains node text), require a minimum node text
    // length to avoid matching tiny identifiers like "res" or "body" too broadly.
    // Also require the node to cover at least half of the target: the reverse
    // branch exists for a locator with minor syntactic drift around the true
    // node, not for binding to a fragment. Without the floor, a 100+ char
    // locator whose exact match failed would bind to the smallest embedded
    // sub-expression (pickBestMatch prefers small nodes), e.g. the 8-char
    // literal "active" inside a res.json object payload (#335).
    const MIN_REVERSE_MATCH_LEN = 8;
    const MIN_REVERSE_MATCH_COVERAGE = 0.5;
    const substringMatches = candidates.filter(
      (c) =>
        c.text.includes(normalizedTarget) ||
        (c.text.length >= MIN_REVERSE_MATCH_LEN &&
          c.text.length >= normalizedTarget.length * MIN_REVERSE_MATCH_COVERAGE &&
          normalizedTarget.includes(c.text))
    );

    if (substringMatches.length > 0) {
      // Prefer nodes where the LLM text is contained in the node text
      const containingMatches = substringMatches.filter((c) =>
        c.text.includes(normalizedTarget)
      );

      if (containingMatches.length > 0) {
        return this.pickBestMatch(containingMatches.map((c) => c.node), lineNumber) as T;
      }

      return this.pickBestMatch(substringMatches.map((c) => c.node), lineNumber) as T;
    }

    return undefined;
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
        const bestDist = Math.abs(best.getStartLineNumber() - targetLine);
        const currentDist = Math.abs(current.getStartLineNumber() - targetLine);
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
    return text
      .replace(/\s+/g, ' ')
      .replace(/\s*,?\s*([}\)\]])/g, '$1')
      .replace(/([\(\[{])\s+/g, '$1')
      .trim();
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
    const allNodes = sourceFile.getDescendants();
    const containing = allNodes.filter((node) => {
      if (Node.isSourceFile(node)) return false;
      // Skip SyntaxList nodes as they have unreliable types
      if (node.getKind() === SyntaxKind.SyntaxList) return false;
      const start = node.getStart();
      const end = node.getEnd();
      return start <= spanStart && spanEnd <= end;
    });

    if (containing.length === 0) {
      return undefined;
    }

    return containing.reduce((best, current) => {
      const bestRange = best.getEnd() - best.getStart();
      const currentRange = current.getEnd() - current.getStart();

      // Prefer exact matches, then smallest containing
      const bestDelta = Math.abs(bestRange - (spanEnd - spanStart));
      const currentDelta = Math.abs(currentRange - (spanEnd - spanStart));

      return currentDelta < bestDelta ? current : best;
    });
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
    const SPAN_SLACK = 2;
    const callExpressions = sourceFile
      .getDescendantsOfKind(SyntaxKind.CallExpression);
    const candidates = callExpressions.filter((expr) => {
      const start = expr.getStart();
      const end = expr.getEnd();
      return start - SPAN_SLACK <= spanStart && spanEnd <= end + SPAN_SLACK;
    });

    if (candidates.length === 0) {
      return undefined;
    }

    return candidates.reduce((best, current) => {
      const bestRange = best.getEnd() - best.getStart();
      const currentRange = current.getEnd() - current.getStart();

      const bestDelta = Math.abs(bestRange - (spanEnd - spanStart));
      const currentDelta = Math.abs(currentRange - (spanEnd - spanStart));

      return currentDelta < bestDelta ? current : best;
    });
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
    const startLinePos = node.getStartLineNumber();
    const endLinePos = node.getEndLineNumber();

    return {
      file_path: node.getSourceFile().getFilePath(),
      start_line: startLinePos,
      end_line: endLinePos,
      start_column: node.getStart() - node.getStartLinePos(),
      end_column: node.getEnd() - node.getStartLinePos(),
    };
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
      // Only meaningful relative to the anchor symbol: without an element
      // symbol there is nothing for the explicit-bundle correction to match.
      array_depth:
        primaryTypeSymbol !== undefined && arrayDepth !== undefined && arrayDepth > 0
          ? arrayDepth
          : undefined,
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
