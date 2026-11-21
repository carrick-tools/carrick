/**
 * The property keys found at key paths of a make call's type argument, with
 * each key's declaration: `createClient<Database>(...)` asked for
 * `["public", "Tables"]` lists the keys of `Database["public"]["Tables"]`.
 *
 * A general compiler read. It holds no library name, no path template and no
 * word of any one domain: what the keys mean is the caller's. Each query is
 * the span of a `call` position row (as `implementations` takes it), a type
 * argument index and key paths. The type argument is resolved by the checker,
 * and each path is walked property by property, so an alias, an interface, an
 * intersection or an imported type reads the same.
 *
 * Each answer holds one entry per asked path, in order, with the path as asked
 * (where the keys were read). A key is named as a position row names a
 * declaration: repo-relative file and 1-based line of where the key is
 * declared. A call with no type argument at that index reads no keys and says
 * `no_type_argument`; a path the type does not have reads no keys and says
 * `path_missing`.
 */

import * as path from 'node:path';
import { ts } from 'ts-morph';
import { positionAt } from './implementations.js';
import { PathNames, PositionWalker } from './position-rows.js';
import type { ProjectLoader } from './project-loader.js';
import type { ResourceKey, ResourceKeysAnswer, ResourceKeysPathAnswer, ResourceKeysQuery } from './types.js';

export interface ResourceKeysOptions {
  loader: ProjectLoader;
  /** Every file name in an answer is relative to it, as in the position rows. */
  scanRoot: string;
  queries: ResourceKeysQuery[];
  /** Called after each query is answered. */
  onAnswer?: () => void;
}

/** One answer per query, in request order. */
export function findResourceKeys(options: ResourceKeysOptions): ResourceKeysAnswer[] {
  const { loader, scanRoot, queries } = options;
  const paths = new PathNames(scanRoot);
  const answers = new Array<ResourceKeysAnswer>(queries.length);

  const groups = new Map<string, number[]>();
  queries.forEach((query, index) => {
    const key = loader.projectKeyFor(path.resolve(query.file));
    groups.set(key, [...(groups.get(key) ?? []), index]);
  });

  for (const [key, indexes] of groups) {
    const service = loader.getProjectFor(key).getLanguageService().compilerObject;
    const program = service.getProgram();
    if (!program) throw new Error('the language service built no program');
    const checker = program.getTypeChecker();
    const walker = new PositionWalker(program, paths, loader);
    for (const index of indexes) {
      answers[index] = answer(program, checker, walker, queries[index]);
      options.onAnswer?.();
    }
  }
  return answers;
}

function none(reason: NonNullable<ResourceKeysAnswer['reason']>): ResourceKeysAnswer {
  return { paths: [], reason };
}

function answer(
  program: ts.Program,
  checker: ts.TypeChecker,
  walker: PositionWalker,
  query: ResourceKeysQuery
): ResourceKeysAnswer {
  const sourceFile = program.getSourceFile(path.resolve(query.file));
  if (!sourceFile) return none('not_in_program');
  const found = positionAt(sourceFile, query.start, query.end);
  if (!found || found.kind !== 'call') return none('no_call');
  const typeArguments = ts.isCallExpression(found.node) || ts.isNewExpression(found.node) ? found.node.typeArguments : undefined;
  const typeArgument = typeArguments?.[query.type_arg];
  if (!typeArgument) return none('no_type_argument');
  try {
    const root = checker.getTypeFromTypeNode(typeArgument);
    return { paths: query.paths.map((keyPath) => readPath(checker, walker, typeArgument, root, keyPath)) };
  } catch {
    return none('checker_threw');
  }
}

/** The keys of the type reached by walking `keyPath` from `root`. */
function readPath(
  checker: ts.TypeChecker,
  walker: PositionWalker,
  at: ts.Node,
  root: ts.Type,
  keyPath: string[]
): ResourceKeysPathAnswer {
  let type = root;
  for (const step of keyPath) {
    const property = checker.getPropertyOfType(checker.getNonNullableType(type), step);
    if (!property) return { path: keyPath, keys: [], reason: 'path_missing' };
    type = checker.getTypeOfSymbolAtLocation(property, at);
  }
  const keys: ResourceKey[] = [];
  for (const property of checker.getPropertiesOfType(checker.getNonNullableType(type))) {
    // A symbol-keyed member is no name a caller can state.
    if (property.name.startsWith('__@')) continue;
    const declaration = property.declarations?.[0];
    if (!declaration) {
      keys.push({ name: property.name, decl_file: null, decl_line: null });
      continue;
    }
    const file = declaration.getSourceFile();
    const { declFile, declLine } = walker.located(file, declaration.getStart(file));
    keys.push({ name: property.name, decl_file: declFile, decl_line: declLine });
  }
  return { path: keyPath, keys };
}
