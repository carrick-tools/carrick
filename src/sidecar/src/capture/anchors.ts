/**
 * Anchor resolution for the surface entry: guards for addressable anchors
 * (symbol / handler_return) and the locator + node-builder path for
 * anonymous inferred types. Produces the final alias line for each anchor,
 * or a recorded demotion -- never a silently wrong line.
 */

import ts from 'typescript';
import * as path from 'node:path';
import type {
  CaptureAnchorRequest,
  InferAnchorRequest,
  SymbolAnchorRequest,
} from './api.js';
import { printTypeForDestination } from './node-builder.js';
import { typeIsOrContainsMachinery } from './machinery.js';

export interface ResolvedAnchor {
  request: CaptureAnchorRequest;
  /** RHS of `export type <alias> = ...;` in the surface entry. */
  aliasText: string;
  serialization: 'emitted' | 'node_builder' | 'structural_fallback';
  /** Present exactly for demotions (the alias line is `unknown`). */
  failureReason?: string;
  /**
   * Provenance for a non-demoting re-aim (#438/#439): recorded into the
   * alias's `self_check_detail` when the alias self-checks clean, so a symbol
   * anchor redirected from a value const to its `typeof` type sibling, or a
   * builder-chain anchor moved off its config descriptor, stays traceable.
   */
  reaimNote?: string;
}

/** Repo-root-relative source file -> extensionless specifier from entryDir. */
export function entryRelativeSpecifier(
  entryDir: string,
  repoRoot: string,
  sourceFile: string
): string {
  const target = path
    .join(repoRoot, sourceFile)
    .replace(/\.(ts|tsx|mts|cts)$/, '');
  let rel = path.relative(entryDir, target).split(path.sep).join('/');
  if (!rel.startsWith('.')) rel = `./${rel}`;
  return rel;
}

function moduleExport(
  checker: ts.TypeChecker,
  sourceFile: ts.SourceFile,
  name: string
): ts.Symbol | undefined {
  const moduleSymbol = checker.getSymbolAtLocation(sourceFile);
  if (!moduleSymbol) return undefined;
  return checker
    .getExportsOfModule(moduleSymbol)
    .find((s) => s.getName() === name);
}

/**
 * Resolve one anchor against the analysis program. `placeholder` is the
 * anchor's own placeholder alias declaration in the entry file -- the
 * destination node the node builder anchors to (banked correction 1).
 */
export function resolveAnchor(
  program: ts.Program,
  request: CaptureAnchorRequest,
  args: {
    repoRoot: string;
    entryDir: string;
    placeholder: ts.TypeAliasDeclaration | undefined;
    /**
     * symbol_name -> entry-relative specifier for every sibling `symbol`
     * anchor. A literal anchor whose text is a bare identifier naming one of
     * these resolves through that module instead of dangling in the entry.
     */
    siblingSymbolSpecs?: Map<string, string>;
  }
): ResolvedAnchor {
  const checker = program.getTypeChecker();
  const demote = (reason: string): ResolvedAnchor => ({
    request,
    aliasText: 'unknown',
    serialization: 'structural_fallback',
    failureReason: reason,
  });

  if (request.kind === 'literal') {
    const text = request.type_text.trim();
    if (!text) {
      return demote('empty literal type text');
    }
    const bareIdentifier = /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(text);
    const siblingSpec = bareIdentifier
      ? args.siblingSymbolSpecs?.get(text)
      : undefined;
    return {
      request,
      aliasText: siblingSpec ? `import('${siblingSpec}').${text}` : text,
      // Literal anchors ARE the legacy-text tier (WP3 wiring of the design's
      // structural_fallback): hand-produced type text riding the surface.
      // The self-check still classifies decay; the fidelity metric counts
      // them at this tier so the legacy dependence stays measurable and
      // ratchetable. Demotions are distinguished by failureReason.
      serialization: 'structural_fallback',
    };
  }

  const sourceAbs = path.join(args.repoRoot, request.source_file);
  const sourceFile = program.getSourceFile(sourceAbs);
  if (!sourceFile) {
    return demote(`source file not in program: ${request.source_file}`);
  }
  const spec = entryRelativeSpecifier(args.entryDir, args.repoRoot, request.source_file);

  if (request.kind === 'symbol') {
    const exported = moduleExport(checker, sourceFile, request.symbol_name);
    if (!exported) {
      return demote(
        `'${request.symbol_name}' is not an export of ${request.source_file}`
      );
    }
    // The anchor is the ELEMENT symbol; array_depth restores the use-site's
    // `[]` levels (#248/#306) so an array-vs-scalar mismatch stays visible.
    const arraySuffix = '[]'.repeat(Math.max(0, request.array_depth ?? 0));

    // #438/#439: an LLM symbol anchor may name a schema VALUE const
    // (`export const ZFooSchema = z.object({...})`) whose sibling
    // `export type TFoo = z.infer<typeof ZFooSchema>` was the intended anchor.
    // A value has no type-space meaning, so `import('./m').ZFooSchema` in TYPE
    // position raises TS2694 in the stub tree and (before #438 part 2) poisons
    // every producer pair. Guard structurally on symbol meaning, never on the
    // name: resolve re-exports, then test for any type-space flag.
    const resolvedExport = resolveSymbolAliases(checker, exported);
    if (!symbolHasTypeMeaning(resolvedExport)) {
      // #439 part 2: re-aim at a sibling type alias defined via a `typeof`
      // type-query of this const (covers `z.infer<typeof C>` and equivalents;
      // matched structurally, no library names). Guard A is the fallback.
      const reaimed = reaimValueSymbolToTypeSibling(checker, sourceFile, {
        request,
        valueSymbol: resolvedExport,
        spec,
        arraySuffix,
      });
      if (reaimed) return reaimed;
      // #438 part 1: value-only export with no type sibling. Demote so the
      // existing demotion/backfill path engages (honest `unknown`) instead of
      // emitting a surface line that references a value in type position.
      return demote(
        `'${request.symbol_name}' is a value export with no type-space meaning, ` +
          `and no sibling type alias derives from it (e.g. \`typeof ${request.symbol_name}\`); ` +
          'demoted so the surface line stays valid'
      );
    }

    return {
      request,
      aliasText: `import('${spec}').${request.symbol_name}${arraySuffix}`,
      serialization: 'emitted',
    };
  }

  if (request.kind === 'handler_return') {
    // Design-doc Capture step 1 guards, all three verified failure modes.
    const exported = moduleExport(checker, sourceFile, request.symbol_name);
    if (!exported) {
      return demote(
        `handler '${request.symbol_name}' is not an export of ${request.source_file}`
      );
    }
    const declaration = exported.valueDeclaration ?? exported.declarations?.[0];
    const handlerType = checker.getTypeOfSymbolAtLocation(
      exported,
      declaration ?? sourceFile
    );
    const callSignatures = handlerType.getCallSignatures();
    if (callSignatures.length === 0) {
      return demote(`handler '${request.symbol_name}' has no call signatures`);
    }
    if (callSignatures.length > 1) {
      // ReturnType<> silently resolves the LAST overload only.
      return demote(
        `handler '${request.symbol_name}' is an overload set (${callSignatures.length} signatures)`
      );
    }
    if ((callSignatures[0].getTypeParameters() ?? []).length > 0) {
      // Type parameters erase to their constraint/unknown under ReturnType<>.
      return demote(`handler '${request.symbol_name}' is generic`);
    }
    return {
      request,
      aliasText: `Awaited<ReturnType<typeof import('${spec}').${request.symbol_name}>>`,
      serialization: 'emitted',
    };
  }

  // kind === 'infer'
  let located = locateNode(sourceFile, request);
  if (!located) {
    return demote(locatorFailureReason(request));
  }
  // #439 part 1: a producer anchor whose locator landed inside a fluent
  // builder chain's config-descriptor argument (an all-literal metadata
  // object) must never capture that descriptor as the request type. Re-aim at
  // the chain's payload/schema argument, or demote when it cannot be picked
  // unambiguously — never leave the descriptor captured (the artifact behind
  // v1's false-incompatible verdicts; kept coupled with #438's containment).
  let reaimNote: string | undefined;
  const builderReaim = reaimBuilderChainPayload(checker, located);
  if (builderReaim.kind === 'demote') {
    return demote(builderReaim.reason);
  }
  if (builderReaim.kind === 'reaim') {
    located = builderReaim.node;
    reaimNote = builderReaim.note;
  }
  let type = checker.getTypeAtLocation(located);
  if ((request.unwrap ?? 'awaited') === 'awaited') {
    type = checker.getAwaitedType(type) ?? type;
  }
  // #433: the checker gave us nothing (whole top type) — try the syntactic
  // recovery for an inline literal type argument of an unresolvable generic
  // annotation. The literal is dependency-free source syntax; only the outer
  // generic needed the missing package.
  if (isTopType(type)) {
    const literalNode = recoverInlineLiteralTypeArgument(
      checker,
      sourceFile,
      request,
      located
    );
    if (literalNode) {
      const recovered = checker.getTypeAtLocation(literalNode);
      if (!isTopType(recovered)) type = recovered;
    }
  }
  // carrick#371 fail-closed guard: a producer anchor whose resolved type IS or
  // CONTAINS framework machinery (a raw `Response`/`Request`, a wrapper envelope
  // `{ response: Response; error }`, a wrapper function `(req) => Promise<Response>`)
  // must never be emitted as a comparable contract — that manufactures a false
  // compat mismatch against the consumer's real payload. This is the capture
  // fallback for the case the v1-inferrer machinery guard leaves behind: when v1
  // abstains on the envelope, `derive_capture_anchors` re-routes the alias to
  // this locator-based infer anchor, which then resolves the WRAPPER FUNCTION
  // type. No source locator points at a distinct clean payload here (the payload
  // lives inside the wrapper's response builder), so degrade to `unknown` —
  // abstain, never a concrete verdict off the machinery.
  if (typeIsOrContainsMachinery(checker, type, located)) {
    return demote(
      'resolved type is or contains framework machinery (Response/Request-shaped); ' +
        'degraded to unknown rather than emit a wrapper envelope as a response contract'
    );
  }
  if (!args.placeholder) {
    return demote('internal: no placeholder destination for infer anchor');
  }
  const printed = printTypeForDestination(program, type, args.placeholder);
  if (!printed.text) {
    return demote(printed.failure ?? 'node builder print failed');
  }
  return {
    request,
    aliasText: printed.text,
    serialization: 'node_builder',
    ...(reaimNote ? { reaimNote } : {}),
  };
}

function isTopType(type: ts.Type): boolean {
  return (
    (type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown | ts.TypeFlags.Never)) !== 0
  );
}

/**
 * The checker's intrinsic ERROR type: resolution of the reference FAILED
 * (missing package on a bare checkout, unresolvable symbol). Distinct from a
 * legitimate `any`, whose intrinsic name is `any` — an author-written or
 * alias-resolved `any` must never trigger the syntactic recovery.
 * `intrinsicName` is internal but stable since TS 1.x (same standing as the
 * internals node-builder.ts already mirrors).
 */
function isErrorType(type: ts.Type): boolean {
  if (!(type.flags & ts.TypeFlags.Any)) return false;
  const name = (type as unknown as { intrinsicName?: string }).intrinsicName;
  return name === 'error' || name === 'unresolved';
}

/**
 * #433 syntactic recovery: an infer anchor whose checker type decayed to a
 * whole top type may sit under a DECLARED annotation of the form
 * `SomeGeneric<{ ...literal... }>` where only the outer generic failed to
 * resolve (error-typed; its package is absent on a bare checkout) while the
 * literal type argument is dependency-free source syntax referencing only
 * locally-resolvable symbols. Trigger is purely structural — no framework
 * names anywhere:
 *
 *  1. Data-flow path: the payload node (the located node, or the anchor's
 *     expression_text relocated) is an argument of a call — or IS a send
 *     call — whose callee chain roots at an identifier whose declaration
 *     carries the annotation (`res.json(payload)` -> `res`).
 *  2. Registration path: the located node is a call registering inline
 *     callbacks; the callbacks' parameter annotations are scanned.
 *
 * Either path recovers ONLY when exactly one literal type argument is in
 * scope — ambiguity refuses recovery and the alias keeps decaying honestly.
 * The recovered literal node's own type is printed through the normal
 * node-builder path, so the alias still faces the real self-check.
 */
function recoverInlineLiteralTypeArgument(
  checker: ts.TypeChecker,
  sourceFile: ts.SourceFile,
  request: InferAnchorRequest,
  located: ts.Node
): ts.TypeLiteralNode | undefined {
  const candidates: ts.Node[] = [located];
  if (request.expression_text) {
    const byText = nodeByExpressionText(
      sourceFile,
      request.expression_text,
      request.line_number
    );
    if (byText && byText !== located) candidates.push(byText);
  }

  // Data-flow first: the payload's own call names the annotated receiver.
  for (const node of candidates) {
    const call = governingCall(node);
    if (!call) continue;
    const root = calleeRootIdentifier(call.expression);
    if (!root) continue;
    const annotation = declaredAnnotationOf(checker, root);
    if (!annotation || !isErroredGenericReference(checker, annotation)) continue;
    // The payload carrier is identified; its annotation's verdict is FINAL —
    // never fall through to the registration scan, which could pick a
    // different parameter's literal for this payload.
    const literals = literalTypeArguments(annotation);
    if (literals.length !== 1) return undefined;
    return literalResolvesLocally(checker, literals[0]) ? literals[0] : undefined;
  }

  // Registration shape: the located call passes inline callbacks whose
  // parameters carry the annotations. Ambiguity is counted BEFORE any
  // resolvability filtering: two literal-carrying annotations refuse even
  // when one of them would be rejected later — filtering first could crown
  // the wrong parameter's literal.
  for (const node of candidates) {
    if (!ts.isCallExpression(node)) continue;
    const literals: ts.TypeLiteralNode[] = [];
    for (const arg of node.arguments) {
      if (!isFunctionArgument(arg)) continue;
      for (const param of arg.parameters) {
        if (param.type && isErroredGenericReference(checker, param.type)) {
          literals.push(...literalTypeArguments(param.type));
        }
      }
    }
    if (literals.length !== 1) continue;
    return literalResolvesLocally(checker, literals[0]) ? literals[0] : undefined;
  }
  return undefined;
}

/**
 * The call whose ARGUMENT list carries `node` (payload in argument position),
 * or `node` itself when it is a call (the send call). Calls that register
 * callbacks are excluded: their receiver is the framework app/router value,
 * not a payload carrier — those go through the registration path instead.
 */
function governingCall(node: ts.Node): ts.CallExpression | undefined {
  if (ts.isCallExpression(node)) {
    return node.arguments.some(isFunctionArgument) ? undefined : node;
  }
  const parent = node.parent;
  if (
    parent !== undefined &&
    ts.isCallExpression(parent) &&
    node !== parent.expression &&
    (parent.arguments as readonly ts.Node[]).includes(node)
  ) {
    return parent.arguments.some(isFunctionArgument) ? undefined : parent;
  }
  return undefined;
}

function isFunctionArgument(
  arg: ts.Expression
): arg is ts.ArrowFunction | ts.FunctionExpression {
  return ts.isArrowFunction(arg) || ts.isFunctionExpression(arg);
}

/** Root identifier of a callee chain: `res.status(500).json` -> `res`. */
function calleeRootIdentifier(expr: ts.Expression): ts.Identifier | undefined {
  let current: ts.Expression = expr;
  while (
    ts.isPropertyAccessExpression(current) ||
    ts.isElementAccessExpression(current) ||
    ts.isCallExpression(current) ||
    ts.isNonNullExpression(current) ||
    ts.isParenthesizedExpression(current)
  ) {
    current = current.expression;
  }
  return ts.isIdentifier(current) ? current : undefined;
}

/** The explicit type annotation on the identifier's declaration, if any. */
function declaredAnnotationOf(
  checker: ts.TypeChecker,
  identifier: ts.Identifier
): ts.TypeNode | undefined {
  const symbol = checker.getSymbolAtLocation(identifier);
  const decl = symbol?.valueDeclaration ?? symbol?.declarations?.[0];
  if (!decl) return undefined;
  if (
    ts.isParameter(decl) ||
    ts.isVariableDeclaration(decl) ||
    ts.isPropertySignature(decl) ||
    ts.isPropertyDeclaration(decl)
  ) {
    return decl.type;
  }
  return undefined;
}

/**
 * A generic type reference whose OUTER resolution failed: the annotation is
 * `Name<...args>` and its own type is the checker's error type. A resolvable
 * generic (however it resolves) never triggers recovery; its checker answer
 * is the truth.
 */
function isErroredGenericReference(
  checker: ts.TypeChecker,
  annotation: ts.TypeNode
): annotation is ts.TypeReferenceNode {
  return (
    ts.isTypeReferenceNode(annotation) &&
    (annotation.typeArguments?.length ?? 0) > 0 &&
    isErrorType(checker.getTypeFromTypeNode(annotation))
  );
}

/** Type-literal type arguments of a generic reference (parens unwrapped). */
function literalTypeArguments(annotation: ts.TypeReferenceNode): ts.TypeLiteralNode[] {
  const literals: ts.TypeLiteralNode[] = [];
  for (const arg of annotation.typeArguments ?? []) {
    let node: ts.TypeNode = arg;
    while (ts.isParenthesizedTypeNode(node)) node = node.type;
    if (ts.isTypeLiteralNode(node)) literals.push(node);
  }
  return literals;
}

/**
 * Every type reference inside the literal must itself RESOLVE. A literal
 * leaning on a third-party type (`{ thing: LibThing }` with the lib absent)
 * is not the tractable class — recovering it would bake `any` at a member;
 * it keeps decaying honestly instead. Author-written `any`/`unknown`
 * keywords are allowed through: they are the source's truth, and the
 * self-check's deep walk still owns that verdict.
 */
function literalResolvesLocally(
  checker: ts.TypeChecker,
  literal: ts.TypeLiteralNode
): boolean {
  let resolves = true;
  const visit = (node: ts.Node): void => {
    if (!resolves) return;
    if (
      (ts.isTypeReferenceNode(node) ||
        ts.isImportTypeNode(node) ||
        ts.isTypeQueryNode(node) ||
        ts.isExpressionWithTypeArguments(node)) &&
      isErrorType(checker.getTypeFromTypeNode(node))
    ) {
      resolves = false;
      return;
    }
    node.forEachChild(visit);
  };
  visit(literal);
  return resolves;
}

function locatorFailureReason(request: InferAnchorRequest): string {
  const hints: string[] = [];
  if (request.span_start !== undefined) hints.push(`span ${request.span_start}-${request.span_end}`);
  if (request.expression_text) hints.push(`expression '${request.expression_text.slice(0, 40)}'`);
  if (request.line_number !== undefined) hints.push(`line ${request.line_number}`);
  return `no node located in ${request.source_file} (${hints.join(', ') || 'no locator hints'})`;
}

/**
 * Locate the target node: tightest expression covering the byte span when
 * given; else the expression matching expression_text on/after line_number;
 * else the first expression starting on line_number.
 */
export function locateNode(
  sourceFile: ts.SourceFile,
  request: InferAnchorRequest
): ts.Node | undefined {
  if (request.span_start !== undefined && request.span_end !== undefined) {
    const bySpan = tightestCoveringNode(sourceFile, request.span_start, request.span_end);
    if (bySpan) return bySpan;
  }
  if (request.expression_text) {
    const byText = nodeByExpressionText(sourceFile, request.expression_text, request.line_number);
    if (byText) return byText;
  }
  if (request.line_number !== undefined) {
    return firstExpressionOnLine(sourceFile, request.line_number);
  }
  return undefined;
}

function isPreferredTarget(node: ts.Node): boolean {
  return (
    ts.isExpression(node) ||
    ts.isVariableDeclaration(node) ||
    ts.isParameter(node) ||
    ts.isPropertyAssignment(node)
  );
}

function tightestCoveringNode(
  sourceFile: ts.SourceFile,
  start: number,
  end: number
): ts.Node | undefined {
  let best: ts.Node | undefined;
  const visit = (node: ts.Node) => {
    if (node.getStart(sourceFile) <= start && node.getEnd() >= end) {
      if (isPreferredTarget(node)) best = node;
      node.forEachChild(visit);
    } else {
      node.forEachChild(visit);
    }
  };
  visit(sourceFile);
  return best;
}

function nodeByExpressionText(
  sourceFile: ts.SourceFile,
  text: string,
  fromLine?: number
): ts.Node | undefined {
  const wanted = text.replace(/\s+/g, ' ').trim();
  let best: ts.Node | undefined;
  const visit = (node: ts.Node) => {
    if (best) return;
    if (isPreferredTarget(node)) {
      const nodeText = node.getText(sourceFile).replace(/\s+/g, ' ').trim();
      if (nodeText === wanted) {
        const line =
          sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1;
        if (fromLine === undefined || line >= fromLine) {
          best = node;
          return;
        }
      }
    }
    node.forEachChild(visit);
  };
  visit(sourceFile);
  return best;
}

function firstExpressionOnLine(
  sourceFile: ts.SourceFile,
  line: number
): ts.Node | undefined {
  let best: ts.Node | undefined;
  const visit = (node: ts.Node) => {
    const nodeLine =
      sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1;
    if (nodeLine === line && isPreferredTarget(node) && !best) {
      best = node;
      return;
    }
    node.forEachChild(visit);
  };
  visit(sourceFile);
  return best;
}

// ===========================================================================
// #438/#439 symbol-anchor guards: value-only demotion + const->type re-aim.
// ===========================================================================

/** Follow re-export aliases to the symbol they ultimately denote. */
function resolveSymbolAliases(checker: ts.TypeChecker, symbol: ts.Symbol): ts.Symbol {
  let current = symbol;
  // Bounded: alias chains are short; the guard is against a pathological cycle.
  for (let i = 0; i < 16 && (current.flags & ts.SymbolFlags.Alias) !== 0; i++) {
    let next: ts.Symbol | undefined;
    try {
      next = checker.getAliasedSymbol(current);
    } catch {
      break;
    }
    if (!next || next === current) break;
    current = next;
  }
  return current;
}

/**
 * True when the symbol can stand in TYPE position (class, interface, enum,
 * type alias, type parameter, ...). A pure value export (const / let / var /
 * function) has no type-space meaning, so referencing it as a type is TS2694.
 * Structural — never a name check. `ts.SymbolFlags.Type` is the compiler's own
 * composite of the type-space meanings.
 */
function symbolHasTypeMeaning(symbol: ts.Symbol): boolean {
  return (symbol.flags & ts.SymbolFlags.Type) !== 0;
}

/**
 * #439 part 2: when a symbol anchor names a value-only export, look in the
 * SAME module for a single exported type alias whose type node contains a
 * `typeof <const>` type-query (covers `z.infer<typeof C>`, `Infer<typeof C>`,
 * and any equivalent — matched structurally, no library names). Exactly one
 * match re-aims; zero or several fall back to the value-only demotion (never
 * guess which of an input/output pair is the request).
 */
function reaimValueSymbolToTypeSibling(
  checker: ts.TypeChecker,
  sourceFile: ts.SourceFile,
  ctx: {
    request: SymbolAnchorRequest;
    valueSymbol: ts.Symbol;
    spec: string;
    arraySuffix: string;
  }
): ResolvedAnchor | undefined {
  const moduleSymbol = checker.getSymbolAtLocation(sourceFile);
  if (!moduleSymbol) return undefined;
  const matches: string[] = [];
  for (const exported of checker.getExportsOfModule(moduleSymbol)) {
    const resolved = resolveSymbolAliases(checker, exported);
    if ((resolved.flags & ts.SymbolFlags.TypeAlias) === 0) continue;
    const decl = resolved.declarations?.find(ts.isTypeAliasDeclaration);
    if (!decl) continue;
    if (
      typeAliasRhsInfersConst(checker, decl.type, ctx.request.symbol_name, ctx.valueSymbol)
    ) {
      matches.push(exported.getName());
    }
  }
  if (matches.length !== 1) return undefined;
  return {
    request: ctx.request,
    aliasText: `import('${ctx.spec}').${matches[0]}${ctx.arraySuffix}`,
    serialization: 'emitted',
    reaimNote:
      `symbol anchor '${ctx.request.symbol_name}' is a value; re-aimed at sibling ` +
      `type alias '${matches[0]}' (derived via \`typeof ${ctx.request.symbol_name}\`)`,
  };
}

/**
 * True iff the type alias RHS IS the inferred type derived from a
 * `typeof <const>` query — the ordinary `export type T = z.infer<typeof C>`
 * shape — and NOT a wrapper that merely embeds it. Structurally: the whole
 * type node is a generic type reference (`Infer<typeof C>` /
 * `SomeGeneric<typeof C>`) whose DIRECT type arguments include a `typeof C`
 * query.
 *
 * An object / union / intersection / array that embeds the query as a member
 * or element (`{ data: Infer<typeof C>; meta: string }`) is a WRAPPER: re-aiming
 * at it would capture the wrapper shape, not the payload, and (since the
 * wrapper self-checks concretely) manufacture a false incompatible. Those fall
 * back to the value-only demotion — a guaranteed abstain, never a wrong
 * concrete verdict. A bare `typeof C` alias is also rejected: it denotes the
 * schema wrapper value's type, not the inferred payload. Still no library-name
 * checks: matched purely on shape.
 */
function typeAliasRhsInfersConst(
  checker: ts.TypeChecker,
  node: ts.TypeNode,
  symbolName: string,
  valueSymbol: ts.Symbol
): boolean {
  if (!ts.isTypeReferenceNode(node)) return false;
  for (const arg of node.typeArguments ?? []) {
    let inner: ts.TypeNode = arg;
    while (ts.isParenthesizedTypeNode(inner)) inner = inner.type;
    if (
      ts.isTypeQueryNode(inner) &&
      typeQueryMatchesSymbol(checker, inner, symbolName, valueSymbol)
    ) {
      return true;
    }
  }
  return false;
}

/**
 * Does a `typeof X` query denote `valueSymbol`? Symbol identity is primary
 * (resolves through re-exports and works on a bare checkout: the value symbol
 * resolves without the schema library present); a textual identifier match on
 * `symbolName` is the fallback.
 */
function typeQueryMatchesSymbol(
  checker: ts.TypeChecker,
  query: ts.TypeQueryNode,
  symbolName: string,
  valueSymbol: ts.Symbol
): boolean {
  const entity = query.exprName;
  const idNode = ts.isQualifiedName(entity) ? entity.right : entity;
  const queried =
    checker.getSymbolAtLocation(entity) ?? checker.getSymbolAtLocation(idNode);
  if (queried && resolveSymbolAliases(checker, queried) === valueSymbol) return true;
  return ts.isIdentifier(idNode) && idNode.text === symbolName;
}

// ===========================================================================
// #439 part 1: builder-chain payload selection.
//
// A producer request anchor whose line-based locator lands inside a fluent
// builder chain's config-descriptor argument (`.meta({ openapi: {...} })`)
// would otherwise capture that literal metadata object as the request type.
// We re-aim at the chain argument that carries a schema, or demote when the
// payload cannot be picked unambiguously.
//
// STRUCTURAL, NOT NAME-BASED: no method name (`meta` / `input` / `mutation`)
// is consulted anywhere. The signal that the located node is a config
// descriptor is purely that it is a non-empty, all-literal-typed object
// literal argument of a chain of >=2 fluent calls; a payload candidate is a
// chain argument whose RESOLVED type has object/payload meaning (a schema
// resolves to an object; a string/number tag does not).
//
// #497 widened the descriptor test one step: the descriptor also counts when it
// is REFERENCED by name (`.meta(createWidgetMeta)`, the const declared in a
// sibling module) rather than written inline. The reference is resolved through
// its symbol to its declaration's initialiser and tested there. That reference
// test is deliberately STRICTER than the inline one, for the reason set out on
// `isMetadataDescriptor`: an inline all-literal object argument is never a
// payload here, while a reference is an ordinary identifier that would
// otherwise be a payload candidate. The descriptor argument is then excluded
// from the candidate set by NODE IDENTITY, never by shape: a payload schema
// that happens to be an all-literal const must stay a candidate.
//
// SAFETY of the two outcomes:
//   - the DEMOTE branches (no object candidate, or several) are no-worse-than
//     the descriptor: within this trigger the locator already sat on the
//     descriptor, so an honest `unknown` can never be worse than the concrete,
//     wrong descriptor it replaces — a guaranteed abstain.
//   - the single-candidate RE-AIM branch is a HEURISTIC, not a guaranteed-safe
//     substitution: it swaps the concrete descriptor for a concrete payload
//     type, so it is deliberately guarded (object-typed candidate only, and
//     exactly one) to keep the genuine single-schema case working while a
//     non-object, ambiguous, or absent candidate abstains instead of guessing.
//
// LIMITATION (documented per the house rule that pragmatic is acceptable but
// silent brittleness is not): a chain carrying two object-typed arguments —
// e.g. both an input and an output schema — cannot be disambiguated as request
// vs response without method-name knowledge, so it demotes rather than risk
// the wrong side. Three further descriptor spellings are deliberately NOT
// recognised, none of them backed by an observed repro and each of them a
// widening of the trigger on guesswork: a descriptor produced by a call
// (`.meta(buildMeta())`, which would need the callee's return analysed); a
// descriptor literal carrying a spread (`{ ...base, openapi }`, which
// `isAllLiteralMetadata` rejects along with every other non-property member);
// and a FLAT referenced descriptor (`const m = { tag: "x" }`), excluded by the
// stricter reference test below. All three keep the pre-#497 behaviour: no
// re-aim, descriptor captured.
// ===========================================================================

type BuilderReaim =
  | { kind: 'none' }
  | { kind: 'reaim'; node: ts.Expression; note: string }
  | { kind: 'demote'; reason: string };

function reaimBuilderChainPayload(
  checker: ts.TypeChecker,
  located: ts.Node
): BuilderReaim {
  const descriptor = enclosingChainDescriptorArgument(checker, located);
  if (!descriptor) return { kind: 'none' };
  const chain = fluentChainCalls(descriptor.call);
  if (chain.length < 2) return { kind: 'none' };

  const candidates = payloadCandidates(checker, chain, descriptor.argument);
  if (candidates.length === 1) {
    return {
      kind: 'reaim',
      node: candidates[0],
      note:
        'producer anchor landed on a builder-chain config descriptor; ' +
        're-aimed at the chain schema argument',
    };
  }
  // Zero (a descriptor with no schema argument) or several (ambiguous
  // input/output schemas): never keep the descriptor captured.
  return {
    kind: 'demote',
    reason:
      candidates.length === 0
        ? 'producer anchor landed on a builder-chain config descriptor with no ' +
          'identifiable schema argument; demoted to keep the request type honest'
        : `producer anchor landed on a builder-chain config descriptor; ` +
          `${candidates.length} schema arguments are present and cannot be ` +
          'disambiguated structurally; demoted to keep the request type honest',
  };
}

/**
 * Walk up from the located node to the enclosing call argument that carries the
 * descriptor signature: all-literal metadata, written inline or referenced by
 * name (#497). Returns the call and the descriptor argument itself, or
 * undefined when the located node is not inside such a descriptor. The argument
 * comes back so the caller can exclude exactly that node from the payload
 * candidates by identity.
 */
function enclosingChainDescriptorArgument(
  checker: ts.TypeChecker,
  located: ts.Node
): { call: ts.CallExpression; argument: ts.Expression } | undefined {
  let node: ts.Node | undefined = located;
  while (node) {
    const parent: ts.Node | undefined = node.parent;
    if (
      parent &&
      ts.isCallExpression(parent) &&
      (parent.arguments as readonly ts.Node[]).includes(node) &&
      isMetadataDescriptor(checker, node as ts.Expression)
    ) {
      return { call: parent, argument: node as ts.Expression };
    }
    node = parent;
  }
  return undefined;
}

/**
 * A config-descriptor argument: a non-empty, all-literal object literal, either
 * written inline or named by a reference whose declaration initialises it to
 * one (#497 — `.meta(createWidgetMeta)` with the const in a sibling module).
 *
 * The REFERENCE spelling is deliberately STRICTER than the inline one, and the
 * asymmetry is load-bearing. In this file's model an inline all-literal object
 * argument is never a payload, so the inline test can stay loose. A reference is
 * an ordinary identifier that `payloadCandidates` otherwise treats as a payload
 * candidate, so classifying one as a descriptor takes it out of the running: a
 * flat hand-rolled const (`const s = { id: 0, name: "" }` passed to `.input`)
 * would be misread as the descriptor and the capture would re-aim onto the
 * metadata beside it — manufacturing the exact defect #497 fixes. Requiring a
 * nested object-literal property (the `{ openapi: { ... } }` shape both observed
 * variants carry) separates a descriptor from a flat payload const without
 * consulting a single method or library name.
 */
function isMetadataDescriptor(checker: ts.TypeChecker, arg: ts.Expression): boolean {
  if (ts.isObjectLiteralExpression(arg)) return isAllLiteralMetadata(arg);
  const literal = referencedObjectLiteral(checker, arg);
  return (
    literal !== undefined &&
    isAllLiteralMetadata(literal) &&
    literal.properties.some(
      (p) => ts.isPropertyAssignment(p) && ts.isObjectLiteralExpression(p.initializer)
    )
  );
}

/**
 * The object literal a reference ultimately initialises to, or undefined. Only
 * plain name references are followed (an identifier or a property access);
 * re-export aliases are resolved so an imported const reaches its declaration,
 * and `as const` / `satisfies` / parenthesised initialisers are unwrapped.
 */
function referencedObjectLiteral(
  checker: ts.TypeChecker,
  arg: ts.Expression
): ts.ObjectLiteralExpression | undefined {
  if (!ts.isIdentifier(arg) && !ts.isPropertyAccessExpression(arg)) return undefined;
  const symbol = checker.getSymbolAtLocation(arg);
  if (!symbol) return undefined;
  const declaration = resolveSymbolAliases(checker, symbol).valueDeclaration;
  if (!declaration) return undefined;
  let initializer: ts.Expression | undefined;
  if (ts.isVariableDeclaration(declaration) || ts.isPropertyAssignment(declaration)) {
    initializer = declaration.initializer;
  }
  while (
    initializer &&
    (ts.isAsExpression(initializer) ||
      ts.isSatisfiesExpression(initializer) ||
      ts.isParenthesizedExpression(initializer) ||
      ts.isTypeAssertionExpression(initializer))
  ) {
    initializer = initializer.expression;
  }
  return initializer && ts.isObjectLiteralExpression(initializer)
    ? initializer
    : undefined;
}

/**
 * The full set of calls in the fluent chain containing `anyCall`
 * (`x.a(...).b(...).c(...)` -> the three calls). Climbs to the outermost call,
 * then descends the callee chain.
 */
function fluentChainCalls(anyCall: ts.CallExpression): ts.CallExpression[] {
  let outer = anyCall;
  for (;;) {
    const access = outer.parent;
    if (
      access &&
      (ts.isPropertyAccessExpression(access) || ts.isElementAccessExpression(access)) &&
      access.expression === outer &&
      access.parent &&
      ts.isCallExpression(access.parent) &&
      access.parent.expression === access
    ) {
      outer = access.parent;
      continue;
    }
    break;
  }
  const calls: ts.CallExpression[] = [];
  let current: ts.Expression = outer;
  while (ts.isCallExpression(current)) {
    calls.push(current);
    const callee = current.expression;
    if (ts.isPropertyAccessExpression(callee) || ts.isElementAccessExpression(callee)) {
      current = callee.expression;
    } else {
      break;
    }
  }
  return calls;
}

/**
 * Chain arguments that carry type-space PAYLOAD meaning: a named or constructed
 * schema (identifier / member access / call) whose RESOLVED type is an object
 * shape. Two filters, both load-bearing:
 *  - syntactic: inline object/array literals and inline functions (handlers)
 *    are never candidates;
 *  - semantic: a candidate whose resolved type is a primitive or literal
 *    (`.tag(routeName)` with `routeName: string`), a bare callable (a named
 *    handler), or a top type is REJECTED — capturing a string-literal type as a
 *    request payload manufactures a false incompatible. Only an object-typed
 *    argument is a real schema.
 * With this filter the single-candidate re-aim fires only on a genuine schema,
 * and a non-object, ambiguous (>1), or absent candidate abstains via demote.
 *
 * `descriptor` is the argument the anchor landed in, excluded by NODE IDENTITY
 * (#497). Identity, not shape: an inline descriptor was already excluded by the
 * syntactic filter, but a descriptor named by reference is an identifier like
 * any other, and excluding it by shape would also strike out a payload schema
 * that happens to be an all-literal const.
 */
function payloadCandidates(
  checker: ts.TypeChecker,
  chain: ts.CallExpression[],
  descriptor: ts.Expression
): ts.Expression[] {
  const out: ts.Expression[] = [];
  for (const call of chain) {
    for (const arg of call.arguments) {
      if (arg === descriptor) continue;
      if (
        (ts.isIdentifier(arg) ||
          ts.isPropertyAccessExpression(arg) ||
          ts.isElementAccessExpression(arg) ||
          ts.isCallExpression(arg)) &&
        typeIsObjectPayload(checker.getTypeAtLocation(arg))
      ) {
        out.push(arg);
      }
    }
  }
  return out;
}

/**
 * A type with object/payload meaning: a real object shape (or a union /
 * intersection of them), never a primitive, string/number/boolean/bigint/enum
 * literal, null/undefined/void, a top type (`any`/`unknown`/`never` — cannot be
 * confirmed a payload), or a bare callable value (a handler function). This is
 * the gate that keeps the builder-chain re-aim from capturing a non-schema.
 */
function typeIsObjectPayload(type: ts.Type): boolean {
  const nonObject =
    ts.TypeFlags.Any |
    ts.TypeFlags.Unknown |
    ts.TypeFlags.Never |
    ts.TypeFlags.Null |
    ts.TypeFlags.Undefined |
    ts.TypeFlags.Void |
    ts.TypeFlags.StringLike |
    ts.TypeFlags.NumberLike |
    ts.TypeFlags.BooleanLike |
    ts.TypeFlags.BigIntLike |
    ts.TypeFlags.ESSymbolLike |
    ts.TypeFlags.EnumLike;
  if (type.flags & nonObject) return false;
  if (type.flags & (ts.TypeFlags.Union | ts.TypeFlags.Intersection)) {
    // Every constituent must be an object shape: `string | { a }` is not a
    // clean payload, while a union of objects stays eligible.
    return (type as ts.UnionOrIntersectionType).types.every(typeIsObjectPayload);
  }
  if (type.flags & ts.TypeFlags.Object) {
    // A bare callable (a named handler passed as a value) is not a payload; a
    // schema object carries no top-level call/construct signature.
    if (
      type.getCallSignatures().length > 0 ||
      type.getConstructSignatures().length > 0
    ) {
      return false;
    }
    return true;
  }
  return false;
}

/** A non-empty object literal whose every property is a literal-typed value. */
function isAllLiteralMetadata(obj: ts.ObjectLiteralExpression): boolean {
  if (obj.properties.length === 0) return false;
  for (const prop of obj.properties) {
    if (!ts.isPropertyAssignment(prop)) return false;
    if (!isLiteralValueExpression(prop.initializer)) return false;
  }
  return true;
}

function isLiteralValueExpression(expr: ts.Expression): boolean {
  switch (expr.kind) {
    case ts.SyntaxKind.StringLiteral:
    case ts.SyntaxKind.NoSubstitutionTemplateLiteral:
    case ts.SyntaxKind.NumericLiteral:
    case ts.SyntaxKind.TrueKeyword:
    case ts.SyntaxKind.FalseKeyword:
    case ts.SyntaxKind.NullKeyword:
      return true;
  }
  if (ts.isPrefixUnaryExpression(expr)) return isLiteralValueExpression(expr.operand);
  if (ts.isArrayLiteralExpression(expr)) {
    return expr.elements.every(isLiteralValueExpression);
  }
  if (ts.isObjectLiteralExpression(expr)) return isAllLiteralMetadata(expr);
  return false;
}
