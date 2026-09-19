/**
 * Framework-machinery detection for the v2 capture path (carrick#371).
 *
 * A producer response anchor whose resolved type IS or CONTAINS HTTP transport
 * machinery — a fetch/DOM `Response`/`Request`, a Node `http.ServerResponse`, a
 * wrapper envelope `{ response: Response; error?: undefined } | { ...; error }`,
 * or a wrapper function `(req: Request) => Promise<Response>` — must never be
 * emitted as a comparable contract. Comparing it against the consumer's real
 * payload manufactures a false compat mismatch (the bug this module fixes).
 *
 * This is the raw-`ts` seam mirror of `type-inferrer.ts`'s
 * `typeIsOrContainsResponseMachinery` (ts-morph): the capture/ seam forbids
 * importing a module from outside it, so the canonical indicator set and the
 * detection shape are duplicated here in lockstep — the same pattern by which
 * `BUILTIN_ANCHOR_SYMBOLS` mirrors `socket_io.rs`. Detection is STRUCTURAL and
 * framework-agnostic: no framework NAME appears, only the shared HTTP-message
 * member surface, gated by a lib / installed-dependency declaration origin so a
 * user payload that merely shares a member name can never trip it.
 */

import ts from 'typescript';
import * as fs from 'node:fs';
import * as path from 'node:path';

/**
 * Strongly-discriminating member names of HTTP transport machinery. Kept
 * identical to `MACHINERY_MEMBER_INDICATORS` in `type-inferrer.ts`. These are
 * the names no JSON payload carries (`ok`, `redirected`, `bodyUsed`,
 * `writeHead`, ...), so the origin gate + threshold never fire on real data.
 * Exported so a drift-guard test (`machinery-indicator-mirror.test.ts`) asserts
 * it stays equal to the `type-inferrer.ts` copy — nothing else enforces the
 * lockstep, and if the two drift one path silently stops abstaining.
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
 * True when `type`, resolved against `node`, IS or CONTAINS framework
 * machinery.
 *
 * DETECTS, exactly:
 *   1. the type itself is machinery (`isFrameworkMachinery`);
 *   2. a union/intersection member is machinery (the envelope union);
 *   3. a DIRECT property whose type is machinery (`{ response: Response }`),
 *      ONE level of descent only;
 *   4. a CALL SIGNATURE whose AWAITED return type is machinery — the wrapper
 *      function `(req) => Promise<Response>` the Infer fallback resolves to.
 *      (Only the call return is awaited; property types below are not.)
 *
 * DELIBERATELY NOT DETECTED — stated so this comment never overstates the
 * guarantee (mirrors the same list in `type-inferrer.ts`). Each is a
 * non-regression (pre-existing verdict unchanged, never a new wrong one), a
 * tracked follow-up:
 *   - machinery nested deeper than one property level;
 *   - a PROPERTY typed `Promise<Response>` (property types are not awaited /
 *     Promise-unwrapped before the check — only call-signature returns are);
 *   - an array element type: `Response[]` is not descended to its element;
 *   - `interface X extends Response` declared in USER source — the origin gate
 *     is lib/installed-dependency only, so a user-declared subtype reads as a real
 *     contract.
 *
 * The depth cap + origin gate keep a legitimate payload that merely references a
 * machinery type far inside from over-abstaining.
 */
export function typeIsOrContainsMachinery(
  program: ts.Program,
  type: ts.Type,
  node: ts.Node,
  repoRoot: string
): boolean {
  const scope: OriginScope = { program, checker: program.getTypeChecker(), repoRoot };
  return isOrContains(scope, type, node, 0);
}

/** What the origin gate reads: the program that resolved the declaration and
 * the service root it was resolved for. */
interface OriginScope {
  program: ts.Program;
  checker: ts.TypeChecker;
  repoRoot: string;
}

function isOrContains(
  scope: OriginScope,
  type: ts.Type,
  node: ts.Node,
  depth: number
): boolean {
  const { checker } = scope;
  if (isFrameworkMachinery(scope, type)) {
    return true;
  }

  if (type.isUnion() || type.isIntersection()) {
    return (type as ts.UnionOrIntersectionType).types.some((part) =>
      isOrContains(scope, part, node, depth)
    );
  }

  if (depth >= 1) {
    return false;
  }

  // A wrapper function value: descend into its (awaited) return type.
  for (const sig of checker.getSignaturesOfType(type, ts.SignatureKind.Call)) {
    const returnType = sig.getReturnType();
    const awaited = checker.getAwaitedType?.(returnType) ?? returnType;
    if (isOrContains(scope, awaited, node, depth + 1)) {
      return true;
    }
  }

  // Direct properties: the `{ response: Response; error }` envelope shape.
  for (const prop of checker.getPropertiesOfType(type)) {
    const propType = checker.getTypeOfSymbolAtLocation(prop, node);
    if (isOrContains(scope, propType, node, depth + 1)) {
      return true;
    }
  }

  return false;
}

/**
 * True when `type` itself is an HTTP-machinery type: it structurally carries at
 * least `MACHINERY_INDICATOR_THRESHOLD` of the indicator members AND its symbol
 * is declared in a lib or installed-dependency origin.
 */
function isFrameworkMachinery(scope: OriginScope, type: ts.Type): boolean {
  let hits = 0;
  for (const prop of scope.checker.getPropertiesOfType(type)) {
    if (MACHINERY_MEMBER_INDICATORS.has(prop.getName())) {
      hits += 1;
      if (hits >= MACHINERY_INDICATOR_THRESHOLD) break;
    }
  }
  if (hits < MACHINERY_INDICATOR_THRESHOLD) {
    return false;
  }
  return symbolIsLibOrExternalOrigin(scope, type.getSymbol() ?? type.aliasSymbol);
}

/**
 * True when a declaration's source file is runtime/library origin rather than
 * user source. Four answers, in order:
 *
 *  1. The runtime declarations Carrick materialises for a non-Node runtime
 *     under `.carrick/deno/` (carrick#1017), and the remote (JSR, `https:`)
 *     modules it copies beside them. Carrick's own artefact layout, not a
 *     guess about anyone else's.
 *  2. A TypeScript default library (`lib.dom.d.ts`, ...), as the program
 *     classifies it; on a bare checkout the DOM `Response` resolves from here.
 *  3. An install under a `node_modules` segment, however it entered the
 *     program (an import, or a root the loader registered).
 *  4. A file the PROGRAM'S RESOLVER marked as an external library import.
 *     This is the graph-backed answer for a project whose resolution does not
 *     go through `node_modules` (carrick#1264): Deno serves an npm dependency's
 *     types from its own cache, a path with no `node_modules` segment, and
 *     `DenoProject.resolve` hands the compiler `isExternalLibraryImport` from
 *     the graph, which the compiler records on the file. Nothing crosses the
 *     capture seam; both programs are built with that host. One exclusion: a
 *     workspace package reached through a `node_modules` symlink is also
 *     marked external by the compiler but is the user's own source, so a file
 *     inside the checkout (the nearest `.git` above the service root) that
 *     carries no `node_modules` segment stays user source.
 *
 * Lockstep mirror of `isExternalOrigin` in `type-inferrer.ts` (the capture seam
 * forbids sharing a module); `machinery-indicator-mirror.test.ts` guards the
 * pair on a real program.
 */
export function isExternalOrigin(
  program: ts.Program,
  sourceFile: ts.SourceFile,
  repoRoot: string
): boolean {
  const file = sourceFile.fileName.replace(/\\/g, '/');
  if (file.includes('/.carrick/deno/')) {
    return true;
  }
  if (program.isSourceFileDefaultLibrary(sourceFile)) {
    return true;
  }
  if (file.includes('/node_modules/')) {
    return true;
  }
  return program.isSourceFileFromExternalLibrary(sourceFile) && !isInsideCheckout(file, repoRoot);
}

const checkoutRoots = new Map<string, string>();

/** The checkout the service root sits in: the nearest ancestor holding a
 * `.git` entry (a directory, or the file a worktree carries), else the service
 * root itself. */
function checkoutRootOf(repoRoot: string): string {
  const key = path.resolve(repoRoot);
  const cached = checkoutRoots.get(key);
  if (cached) return cached;
  let dir = key;
  let root = key;
  for (;;) {
    if (fs.existsSync(path.join(dir, '.git'))) {
      root = dir;
      break;
    }
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  checkoutRoots.set(key, root);
  return root;
}

function isInsideCheckout(file: string, repoRoot: string): boolean {
  const root = checkoutRootOf(repoRoot).replace(/\\/g, '/');
  return file === root || file.startsWith(root.endsWith('/') ? root : root + '/');
}

/**
 * True when the symbol is declared in a runtime/library origin. Works on a bare
 * checkout: the DOM `Response`/`Request` resolve from the bundled
 * `lib.dom.d.ts` even with no installed dependencies.
 */
function symbolIsLibOrExternalOrigin(
  scope: OriginScope,
  symbol: ts.Symbol | undefined
): boolean {
  if (!symbol) {
    return false;
  }
  for (const decl of symbol.getDeclarations() ?? []) {
    if (isExternalOrigin(scope.program, decl.getSourceFile(), scope.repoRoot)) {
      return true;
    }
  }
  if (symbol.flags & ts.SymbolFlags.Alias) {
    try {
      const aliased = scope.checker.getAliasedSymbol(symbol);
      if (aliased && aliased !== symbol) {
        return symbolIsLibOrExternalOrigin(scope, aliased);
      }
    } catch {
      // Ignore errors when resolving the aliased symbol.
    }
  }
  return false;
}
