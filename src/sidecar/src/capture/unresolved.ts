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
import { findUnresolvedPlaceholders, type UnresolvedAtAnchor } from './deep-walk.js';

/** Files the specifier walk visits from one anchor before it stops. */
const MAX_REACHABLE_FILES = 256;

/** Per-program memo: source file name -> unresolved specifiers it reaches. */
const reachableCache = new WeakMap<ts.Program, Map<string, string[]>>();

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
  const paths = findUnresolvedPlaceholders(type, program, checker, location).map((path) =>
    prefixPath(pathPrefix, path)
  );
  if (paths.length === 0) return undefined;
  return { paths, specifiers: unresolvedSpecifiersReachableFrom(program, sourceFile) };
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
