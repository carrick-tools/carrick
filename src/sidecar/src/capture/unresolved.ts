/**
 * What an anchor's source program could not resolve (carrick#1164).
 *
 * The node builder and declaration emit both print TypeScript's
 * unresolved-reference placeholder as the keyword `any`. After that the stub
 * text cannot say whether an `any` member was written by the author or left
 * behind by an import that did not resolve on the scanned checkout — a
 * dependency that was not installed, a generated module that was never
 * generated. The self-check reads only that text, so it used to label both
 * `declared`, and a reader told "declared that way in the source" stops
 * looking where the fix is to install or generate the missing module.
 *
 * The source program still holds the placeholder, so the capture records the
 * placeholder's member paths at anchor time, together with the module
 * specifiers the anchor's file can reach that did not resolve, and the
 * self-check labels the matching findings. Seam: node builtins + `typescript`.
 */

import ts from 'typescript';
import {
  findUncarriedPositions,
  findUnresolvedPlaceholders,
  isErrorPlaceholder,
  type UnresolvedAtAnchor,
} from './deep-walk.js';

/** Files the specifier walk visits from one anchor before it stops. */
const MAX_REACHABLE_FILES = 256;

/** Per-program memo: source file name -> unresolved specifiers it reaches. */
const reachableCache = new WeakMap<ts.Program, Map<string, string[]>>();

/** Per anchor program: the files it reads that the surface emit cannot carry. */
const uncarriedByProgram = new WeakMap<ts.Program, (fileName: string) => boolean>();

/**
 * Name the files of `program` the surface emit cannot carry (carrick#2418):
 * the sources of the projects its owner references, which the anchor program
 * reads and the emit reaches only as unbuilt output. A position typed from
 * one of them counts as unresolved, so a top type the emit leaves there is
 * labelled with its cause and never as declared.
 */
export function markUncarried(program: ts.Program, uncarried: (fileName: string) => boolean): void {
  uncarriedByProgram.set(program, uncarried);
}

/**
 * The unresolved placeholders inside `type` (read at `location` in `sourceFile`),
 * or `undefined` when it has none — the common case, which costs one walk.
 *
 * `pathPrefix` maps the anchor type's member paths onto the paths of the alias
 * the surface declares when the two differ: a symbol anchor restoring array
 * depth prints `import('./m').Row[]`, whose members sit under `<0>`.
 */
export function unresolvedAtAnchor(
  program: ts.Program,
  sourceFile: ts.SourceFile,
  type: ts.Type,
  location: ts.Node,
  pathPrefix = ''
): UnresolvedAtAnchor | undefined {
  const checker = program.getTypeChecker();
  const uncarried = uncarriedByProgram.get(program);
  const found = findUnresolvedPlaceholders(type, program, checker, location);
  if (uncarried) found.push(...findUncarriedPositions(type, program, checker, location, uncarried));
  // The member walk leaves the root to its callers. The root is the alias's
  // own type only when no prefix maps it under a member.
  if (pathPrefix === '' && rootReadsUnresolved(checker, type, location)) found.unshift('');
  const paths = [...new Set(found)].map((path) => prefixPath(pathPrefix, path));
  if (paths.length === 0) return undefined;
  return { paths, specifiers: unresolvedSpecifiersReachableFrom(program, sourceFile) };
}

/** How many syntax steps the callee walk takes before it gives up. */
const MAX_CALLEE_STEPS = 32;

/**
 * True when `type`, read at `location`, is a top type the source program got
 * from a reference that did not resolve (carrick#3995): the compiler's
 * placeholder itself, or a genuine `any`/`unknown` returned by a call whose
 * callee chain reaches the placeholder. A call through an unresolved value is
 * untyped, so its result is a plain `any` the emitted stub prints like an
 * author's; this is the one place the two can still be told apart.
 *
 * Only the callee chain is read, never the arguments: a resolved callee
 * (a repo wrapper stating `Promise<unknown>`, a library's `JSON.parse`)
 * declares its own result, whatever it is passed.
 */
function rootReadsUnresolved(checker: ts.TypeChecker, type: ts.Type, location: ts.Node): boolean {
  if (isErrorPlaceholder(type)) return true;
  if (!(type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown))) return false;
  let node: ts.Node = location;
  for (let step = 0; step < MAX_CALLEE_STEPS; step++) {
    if (
      ts.isParenthesizedExpression(node) ||
      ts.isAwaitExpression(node) ||
      ts.isNonNullExpression(node)
    ) {
      node = node.expression;
    } else if (ts.isCallExpression(node)) {
      node = node.expression;
    } else if (ts.isPropertyAccessExpression(node) || ts.isElementAccessExpression(node)) {
      if (isErrorPlaceholder(checker.getTypeAtLocation(node))) return true;
      node = node.expression;
    } else if (ts.isIdentifier(node)) {
      if (isErrorPlaceholder(checker.getTypeAtLocation(node))) return true;
      // An unannotated binding reads its initializer's type: follow it.
      const declaration = checker.getSymbolAtLocation(node)?.valueDeclaration;
      if (
        !declaration ||
        !ts.isVariableDeclaration(declaration) ||
        declaration.type ||
        !declaration.initializer
      ) {
        return false;
      }
      node = declaration.initializer;
    } else {
      return false;
    }
  }
  return false;
}

/** Join a walk path under a prefix in the walk's own notation. */
function prefixPath(prefix: string, path: string): string {
  if (prefix === '') return path;
  if (path === '') return prefix;
  return /^[<[(]/.test(path) ? `${prefix}${path}` : `${prefix}.${path}`;
}

/**
 * Module specifiers, as written, that do not resolve from `sourceFile` or from
 * any source module it imports, breadth-first so the nearest come first, with
 * relative specifiers ahead of package names. Installed packages and the
 * default library are not descended into.
 */
export function unresolvedSpecifiersReachableFrom(
  program: ts.Program,
  sourceFile: ts.SourceFile
): string[] {
  let cache = reachableCache.get(program);
  if (!cache) {
    cache = new Map();
    reachableCache.set(program, cache);
  }
  const cached = cache.get(sourceFile.fileName);
  if (cached) return cached;

  const checker = program.getTypeChecker();
  const unresolved = new Set<string>();
  const visited = new Set<string>([sourceFile.fileName]);
  const queue: ts.SourceFile[] = [sourceFile];
  while (queue.length > 0 && visited.size <= MAX_REACHABLE_FILES) {
    const file = queue.shift()!;
    for (const literal of moduleSpecifiersOf(file)) {
      const module = checker.getSymbolAtLocation(literal);
      if (!module) {
        unresolved.add(literal.text);
        continue;
      }
      // An ambient `declare module 'x'` resolves to a module declaration, not
      // a file: resolved, with nothing further to walk.
      const target = module.declarations?.find(ts.isSourceFile);
      if (
        !target ||
        visited.has(target.fileName) ||
        program.isSourceFileDefaultLibrary(target) ||
        program.isSourceFileFromExternalLibrary(target)
      ) {
        continue;
      }
      visited.add(target.fileName);
      queue.push(target);
    }
  }
  const ordered = [...unresolved].sort(
    (a, b) => Number(!a.startsWith('.')) - Number(!b.startsWith('.'))
  );
  cache.set(sourceFile.fileName, ordered);
  return ordered;
}

/** The string-literal module specifiers of a file's imports and re-exports. */
function moduleSpecifiersOf(file: ts.SourceFile): ts.StringLiteral[] {
  const out: ts.StringLiteral[] = [];
  for (const statement of file.statements) {
    let specifier: ts.Expression | undefined;
    if (ts.isImportDeclaration(statement) || ts.isExportDeclaration(statement)) {
      specifier = statement.moduleSpecifier;
    } else if (
      ts.isImportEqualsDeclaration(statement) &&
      ts.isExternalModuleReference(statement.moduleReference)
    ) {
      specifier = statement.moduleReference.expression;
    }
    if (specifier && ts.isStringLiteral(specifier)) out.push(specifier);
  }
  return out;
}
