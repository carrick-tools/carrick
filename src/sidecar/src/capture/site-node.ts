/**
 * Where a site is, and whether its type is framework machinery: one answer for
 * the inferrer and the capture (carrick#3500).
 *
 * Both read a scanner site the same way: they find the node a locator's text
 * names near its line, then refuse to publish a type that is HTTP transport
 * machinery. Each used to keep its own copy, and the copies disagreed (string
 * quotes in the locator, which shapes of machinery were refused), so a wrong
 * node or a transport type could be published by one path and refused by the
 * other. The module lives inside `capture/` because that bundle may import
 * only itself and `typescript`; the inferrer reaches it through
 * `capture/index.js` and hands it compiler nodes.
 */

import ts from 'typescript';

// ===========================================================================
// Framework machinery
// ===========================================================================

/**
 * Strongly-discriminating member names of HTTP transport machinery: a
 * fetch/DOM `Response` & `Request`, a Node `http.ServerResponse`, a framework
 * reply object. A type declared in a lib or installed-dependency origin that
 * carries a subset of these is framework machinery, never a user contract: the
 * PRODUCER-side structural mirror of the consumer `machineryIndicators`
 * (ExtractionConfig), used to reject a wrapper envelope whose response field is
 * a raw `Response` (carrick#371) instead of emitting it as a comparable type.
 *
 * Framework-agnostic by construction: no framework NAME appears here, only the
 * shared HTTP-message surface. The names are deliberately the ones no
 * JSON payload ever carries (`ok`, `redirected`, `bodyUsed`, `arrayBuffer`,
 * `writeHead`, ...), so the origin gate + `MACHINERY_INDICATOR_THRESHOLD` never
 * fire on real data.
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
 * Whether a declaration's file is the runtime's or an installed package's
 * rather than the user's. The caller answers it: the inferrer reads the
 * resolver's record beside the program (`origin.ts`), the capture reads the
 * program alone (`machinery.ts`).
 */
export type DeclaredOutside = (sourceFile: ts.SourceFile) => boolean;

/**
 * True when `type` itself is an HTTP-machinery type: it structurally carries
 * at least `MACHINERY_INDICATOR_THRESHOLD` DISTINCT indicator members (own and
 * apparent), AND its symbol is declared in a lib or installed-dependency
 * origin. Both gates are required: the indicator subset alone essentially
 * never matches a JSON payload, and the origin gate keeps a user's own type
 * that shares those member names from reading as machinery.
 */
export function isFrameworkMachinery(
  checker: ts.TypeChecker,
  type: ts.Type,
  declaredOutside: DeclaredOutside
): boolean {
  if (!hasMachineryIndicatorThreshold(type)) {
    return false;
  }
  return symbolDeclaredOutside(checker, type.getSymbol() ?? type.aliasSymbol, declaredOutside);
}

function hasMachineryIndicatorThreshold(type: ts.Type): boolean {
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
 * True when one of the symbol's declarations is outside the user's source, or
 * the declaration an import alias stands for is. Works on a bare checkout:
 * the DOM `Response`/`Request` resolve from the bundled `lib.dom.d.ts` even
 * with no installed dependencies.
 */
export function symbolDeclaredOutside(
  checker: ts.TypeChecker,
  symbol: ts.Symbol | undefined,
  declaredOutside: DeclaredOutside
): boolean {
  if (!symbol) {
    return false;
  }
  for (const decl of symbol.getDeclarations() ?? []) {
    if (declaredOutside(decl.getSourceFile())) {
      return true;
    }
  }
  if (symbol.flags & ts.SymbolFlags.Alias) {
    try {
      const aliased = checker.getAliasedSymbol(symbol);
      if (aliased && aliased !== symbol) {
        return symbolDeclaredOutside(checker, aliased, declaredOutside);
      }
    } catch {
      // Ignore errors when resolving the aliased symbol.
    }
  }
  return false;
}

/**
 * True when `type` IS or CONTAINS framework machinery: a type that resolves
 * here is never a comparable contract, so a path that reads it abstains
 * (honest `unknown`) rather than publish a concrete-but-false type.
 *
 * DETECTS, exactly:
 *   1. the type itself is machinery (`isFrameworkMachinery`);
 *   2. a union/intersection member is machinery (the envelope union
 *      `{ response: Response; error?: undefined } | { ...; error: Error }`);
 *   3. a DIRECT property whose type is machinery (`{ response: Response }`),
 *      ONE level of descent only, including a property no declaration states
 *      (a mapped `Record<'response', Response>`);
 *   4. a CALL SIGNATURE whose AWAITED return type is machinery: the wrapper
 *      function `(req) => Promise<Response>`.
 *
 * DELIBERATELY NOT DETECTED (carrick#447), stated so this comment never
 * overstates the guarantee. Each leaves the pre-existing verdict in place:
 *   - machinery nested deeper than one level;
 *   - a PROPERTY typed `Promise<Response>` (only call-signature returns are
 *     awaited);
 *   - an array element type: `Response[]` is not descended to its element;
 *   - `interface X extends Response` declared in USER source: the origin gate
 *     is lib/installed-dependency only.
 *
 * The depth cap + origin gate keep a legitimate payload that merely references
 * a machinery type far inside from over-abstaining.
 */
export function typeIsOrContainsMachinery(
  checker: ts.TypeChecker,
  type: ts.Type,
  declaredOutside: DeclaredOutside
): boolean {
  return isOrContains(checker, type, declaredOutside, 0);
}

function isOrContains(
  checker: ts.TypeChecker,
  type: ts.Type,
  declaredOutside: DeclaredOutside,
  depth: number
): boolean {
  if (isFrameworkMachinery(checker, type, declaredOutside)) {
    return true;
  }
  if (type.isUnion() || type.isIntersection()) {
    return type.types.some((part) => isOrContains(checker, part, declaredOutside, depth));
  }
  if (depth >= 1) {
    return false;
  }
  for (const sig of checker.getSignaturesOfType(type, ts.SignatureKind.Call)) {
    const returnType = sig.getReturnType();
    const awaited = checker.getAwaitedType(returnType) ?? returnType;
    if (isOrContains(checker, awaited, declaredOutside, depth + 1)) {
      return true;
    }
  }
  for (const prop of checker.getPropertiesOfType(type)) {
    if (isOrContains(checker, checker.getTypeOfSymbol(prop), declaredOutside, depth + 1)) {
      return true;
    }
  }
  return false;
}

// ===========================================================================
// Node locator
// ===========================================================================

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
 * Normalize whitespace for text comparison:
 * collapse runs of whitespace into single spaces, trim.
 *
 * A member chain broken before its dot (`client\n  .list(…)`) reads the same
 * as `client.list(…)`; a space around `.` / `?.` means nothing (carrick#1162).
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
 *
 * A string literal reads the same in any quotes (carrick#2056).
 */
export function normalizeLocatorText(text: string): string {
  const collapsed = text
    .replace(/\s+/g, ' ')
    .replace(/\s*(\?\.|\.)\s*/g, '$1')
    .replace(/\s*,?\s*([}\)\]])/g, '$1')
    .replace(/([\(\[{])\s+/g, '$1')
    .trim();
  return canonicalStringQuotes(collapsed);
}

/** The default number of lines either side of a locator's line it may match. */
export const LOCATOR_SEARCH_RADIUS = 5;

function startLine(sourceFile: ts.SourceFile, node: ts.Node): number {
  return sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1;
}

/**
 * Every node of `sourceFile` (tokens included, syntax lists left out) that
 * starts within `radius` lines of `line`, or every node when no line is
 * given, in pre-order. The candidates a caller hands `nodeByLocatorText` when
 * it keeps no index of its own.
 */
export function nodesNearLine(
  sourceFile: ts.SourceFile,
  line?: number,
  radius: number = LOCATOR_SEARCH_RADIUS
): ts.Node[] {
  const first = line === undefined ? undefined : line - radius;
  const last = line === undefined ? undefined : line + radius;
  const lineAt = (pos: number): number => sourceFile.getLineAndCharacterOfPosition(pos).line + 1;
  const found: ts.Node[] = [];
  const visit = (node: ts.Node): void => {
    for (const child of node.getChildren(sourceFile)) {
      // A subtree that ends before the window, or starts after it, holds
      // nothing that starts inside it.
      if (first !== undefined && lineAt(child.getEnd()) < first) continue;
      if (last !== undefined && lineAt(child.getStart(sourceFile)) > last) continue;
      if (child.kind !== ts.SyntaxKind.SyntaxList) {
        const at = lineAt(child.getStart(sourceFile));
        if (first === undefined || (at >= first && at <= last!)) found.push(child);
      }
      visit(child);
    }
  };
  visit(sourceFile);
  return found;
}

/**
 * The node a locator's text names, among `nodes`, near `lineNumber`.
 *
 * Normalizes whitespace once per candidate, then tries an exact match, then a
 * locator that elides members, then a node containing the text (preferring
 * the nodes the text opens), then a node the text contains. Among several,
 * the smallest node wins, then the one nearest the line. The caller chooses
 * the candidates (every node, or only calls, or only nodes it can read a
 * value type at).
 */
export function nodeByLocatorText<T extends ts.Node>(
  sourceFile: ts.SourceFile,
  nodes: readonly T[],
  expressionText: string,
  lineNumber?: number,
  searchRadius: number = LOCATOR_SEARCH_RADIUS
): T | undefined {
  const normalizedTarget = normalizeLocatorText(expressionText);
  if (!normalizedTarget) return undefined;
  const lineOf = (node: ts.Node): number => startLine(sourceFile, node);

  // Filter to nodes within the search window and pre-compute normalized text
  const candidates = (
    lineNumber
      ? nodes.filter((node) => {
          const nodeLine = lineOf(node);
          return nodeLine >= lineNumber - searchRadius && nodeLine <= lineNumber + searchRadius;
        })
      : nodes
  ).map((node) => ({ node, text: normalizeLocatorText(node.getText(sourceFile)) }));

  if (candidates.length === 0) return undefined;
  const best = (found: T[]): T => pickBestMatch(sourceFile, found, lineNumber);

  // Try exact match (normalized whitespace)
  const exactMatches = candidates.filter((c) => c.text === normalizedTarget);
  if (exactMatches.length > 0) {
    return best(exactMatches.map((c) => c.node));
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
  const elidedOpening = elidedLocatorOpening(normalizedTarget);
  if (elidedOpening !== undefined) {
    if (elidedOpening.length < MIN_ELIDED_OPENING_LEN) return undefined;
    const opened = candidates.filter((c) => c.text.startsWith(elidedOpening));
    if (opened.length === 0) return undefined;
    if (lineNumber !== undefined) {
      const distance = (c: { node: T }) => Math.abs(lineOf(c.node) - lineNumber);
      const nearest = Math.min(...opened.map(distance));
      return best(opened.filter((c) => distance(c) === nearest).map((c) => c.node));
    }
    return best(opened.map((c) => c.node));
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
      const distance = (c: { node: T }) => Math.abs(lineOf(c.node) - lineNumber);
      const nearest = Math.min(...opening.map(distance));
      return best(opening.filter((c) => distance(c) === nearest).map((c) => c.node));
    }
    return best(containingMatches.map((c) => c.node));
  }
  const reverseMatches = candidates.filter(
    (c) =>
      c.text.length >= MIN_REVERSE_MATCH_LEN &&
      c.text.length >= normalizedTarget.length * MIN_REVERSE_MATCH_COVERAGE &&
      normalizedTarget.includes(c.text)
  );
  if (reverseMatches.length === 0) return undefined;

  const named = locatorNames(normalizedTarget);
  if (!named) {
    return best(reverseMatches.map((c) => c.node));
  }
  const textOf = new Map<ts.Node, string>(candidates.map((c) => [c.node, c.text]));
  const bound = new Set<T>();
  for (const c of reverseMatches) {
    if (named.texts.has(c.text)) {
      bound.add(c.node);
      continue;
    }
    const at = normalizedTarget.indexOf(c.text, named.rootStart + 1);
    if (at < 0) continue;
    const opening = normalizedTarget.slice(named.rootStart, at);
    let enclosing: ts.Node | undefined = c.node.parent;
    while (
      enclosing &&
      !(enclosing.kind === named.rootKind && textOf.get(enclosing)?.startsWith(opening) === true)
    ) {
      enclosing = enclosing.parent;
    }
    if (enclosing) bound.add(enclosing as T);
  }
  return bound.size > 0 ? best([...bound]) : undefined;
}

/**
 * The text a locator opens with before an elision, or `undefined` when the
 * locator has none. An elision is `...` followed only by closing brackets
 * (`{ a: x, ... }`); `...rest` is a spread and source text. An opening too
 * short to tell nodes apart is judged by the caller.
 */
function elidedLocatorOpening(normalizedTarget: string): string | undefined {
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
function locatorNames(
  normalizedTarget: string
): { texts: Set<string>; rootKind: ts.SyntaxKind; rootStart: number } | null {
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
      texts.add(normalizeLocatorText(node.getText(file)));
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
function pickBestMatch<T extends ts.Node>(
  sourceFile: ts.SourceFile,
  nodes: T[],
  targetLine?: number
): T {
  return nodes.reduce((best, current) => {
    const bestRange = best.getEnd() - best.getStart(sourceFile);
    const currentRange = current.getEnd() - current.getStart(sourceFile);

    // Prefer smaller nodes
    if (currentRange !== bestRange) {
      return currentRange < bestRange ? current : best;
    }

    // Tie-break by proximity to target line
    if (targetLine !== undefined) {
      const bestDist = Math.abs(startLine(sourceFile, best) - targetLine);
      const currentDist = Math.abs(startLine(sourceFile, current) - targetLine);
      return currentDist < bestDist ? current : best;
    }

    return best;
  });
}
