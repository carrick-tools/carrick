/**
 * Place every emitted declaration inside the stub's types tree
 * (carrick#1770).
 *
 * The tree mirrors the emit's rootDir. A program can also reach a source
 * outside it: a service that reads a sibling package's SOURCE through a
 * `paths` mapping or a relative import. tsc does not emit that file's
 * declaration under `outDir`; it hands the write callback the source's own
 * path with `.d.ts`. Such a file is placed under `OUTSIDE_DIR`, mirroring its
 * path from the deepest directory it shares with rootDir, and every relative
 * specifier that crosses between the two parts of the tree is rewritten to
 * where its target now sits. Specifiers are read the way tsc wrote them:
 * relative to the SOURCE location of the file that holds them.
 *
 * A capture whose program stays inside rootDir is placed exactly as before.
 */

import * as path from 'node:path';
import { withoutExtension } from './installed-package.js';
import { rewriteSpecifiers } from './specifiers.js';

/** Tree directory holding declarations of sources outside rootDir. */
export const OUTSIDE_DIR = '__outside__';

export interface PlacedTree {
  /** Emitted file name (as tsc gave it) -> tree-relative POSIX path. */
  relOf: Map<string, string>;
  /** Emitted file name -> declaration text, cross-root specifiers rewritten. */
  textOf: Map<string, string>;
  /**
   * Absolute source-side path of each outside declaration, extension
   * stripped -> its tree-relative path. Empty when nothing was outside.
   */
  outside: Map<string, string>;
  /** Relative specifiers rewritten across the root. */
  rewrites: number;
}

const DECLARATION_EXT = /\.d\.(ts|mts|cts)$/;

/**
 * Where each emitted declaration goes. `emitted` maps tsc's file name to its
 * text; `surfaceDeclaration` is renamed to `surface.d.ts` at the tree root.
 */
export function placeEmittedTree(args: {
  emitted: Map<string, string>;
  staging: string;
  entryDir: string;
  surfaceDeclaration: string;
}): PlacedTree {
  const relOf = new Map<string, string>();
  const textOf = new Map(args.emitted);
  // Where tsc would have put each file in the source tree: the place its
  // relative specifiers are written from.
  const sourceSideOf = new Map<string, string>();
  const outsideFiles: string[] = [];
  for (const fileName of args.emitted.keys()) {
    sourceSideOf.set(fileName, sourceSidePath(fileName, args.staging, args.entryDir));
    const rel = posix(path.relative(args.staging, fileName));
    if (escapes(rel)) {
      outsideFiles.push(fileName);
      continue;
    }
    relOf.set(fileName, path.basename(rel) === args.surfaceDeclaration ? 'surface.d.ts' : rel);
  }
  const outside = new Map<string, string>();
  if (outsideFiles.length === 0) return { relOf, textOf, outside, rewrites: 0 };

  const shared = commonDirectory([args.entryDir, ...outsideFiles.map((f) => path.dirname(path.resolve(f)))]);
  for (const fileName of outsideFiles) {
    const rel = path.posix.join(OUTSIDE_DIR, posix(path.relative(shared, path.resolve(fileName))));
    relOf.set(fileName, rel);
    outside.set(path.resolve(fileName).replace(DECLARATION_EXT, ''), rel);
  }

  // Source-side module id (absolute, extensionless) -> tree path.
  const treeOf = new Map<string, string>();
  for (const [fileName, rel] of relOf) {
    treeOf.set(sourceSideOf.get(fileName)!.replace(DECLARATION_EXT, ''), rel);
  }

  let rewrites = 0;
  for (const [fileName, rel] of relOf) {
    const fromSource = path.dirname(sourceSideOf.get(fileName)!);
    const fromTree = path.posix.dirname(rel);
    const result = rewriteSpecifiers(textOf.get(fileName)!, (spec) => {
      if (!spec.startsWith('./') && !spec.startsWith('../')) return undefined;
      const extension = /\.(m|c)?js$/.exec(spec)?.[0] ?? '';
      const target = path.resolve(fromSource, spec.slice(0, spec.length - extension.length));
      const targetRel = treeOf.get(target) ?? treeOf.get(path.join(target, 'index'));
      if (targetRel === undefined) return undefined;
      const targetId = targetRel.replace(DECLARATION_EXT, '');
      // Unchanged when the specifier, read from the file's place in the tree,
      // already reaches the same declaration.
      const literal = path.posix.normalize(path.posix.join(fromTree, spec.slice(0, spec.length - extension.length)));
      if (literal === targetId || `${literal}/index` === targetId) return undefined;
      let next = path.posix.relative(fromTree, targetId);
      if (!next.startsWith('.')) next = `./${next}`;
      return next + extension;
    });
    textOf.set(fileName, result.text);
    rewrites += result.rewrites;
  }
  return { relOf, textOf, outside, rewrites };
}

/**
 * Where tsc would have written an emitted declaration in the source tree. A
 * file under the staging dir mirrors rootDir; one outside it arrived at its
 * source's own path.
 */
function sourceSidePath(fileName: string, staging: string, entryDir: string): string {
  const rel = posix(path.relative(staging, fileName));
  return escapes(rel) ? path.resolve(fileName) : path.join(entryDir, rel);
}

/**
 * A test for whether the tree holds the module a specifier in the surface
 * entry names (carrick#1773).
 *
 * The specifier is read the way the placement above reads one: resolved from
 * the surface's SOURCE-side directory, so an absolute path stays what it is
 * and `../` leaves rootDir, then looked up among the source-side paths of the
 * declarations the tree holds. Those are every emitted declaration, inside
 * rootDir or placed under `OUTSIDE_DIR`, and the declaration sources shipped
 * verbatim (`declarationSources`, relative to rootDir).
 */
export function surfaceModuleInTree(args: {
  /** tsc's file name for each emitted declaration. */
  emitted: Iterable<string>;
  declarationSources: Iterable<string>;
  staging: string;
  entryDir: string;
  surfaceDeclaration: string;
}): (spec: string) => boolean {
  const held = new Set<string>();
  let surfaceDir = args.entryDir;
  for (const fileName of args.emitted) {
    const sourceSide = sourceSidePath(fileName, args.staging, args.entryDir);
    if (path.basename(sourceSide) === args.surfaceDeclaration) surfaceDir = path.dirname(sourceSide);
    held.add(sourceSide.replace(DECLARATION_EXT, ''));
  }
  for (const rel of args.declarationSources) {
    held.add(path.join(args.entryDir, rel).replace(DECLARATION_EXT, ''));
  }
  return (spec) => {
    const target = path.resolve(surfaceDir, withoutExtension(spec));
    return held.has(target) || held.has(path.join(target, 'index'));
  };
}

function posix(p: string): string {
  return p.split(path.sep).join('/');
}

function escapes(rel: string): boolean {
  return rel === '..' || rel.startsWith('../') || path.isAbsolute(rel);
}

/** The deepest directory containing every one of `dirs` (absolute). */
function commonDirectory(dirs: string[]): string {
  let shared = path.resolve(dirs[0]);
  for (const dir of dirs.slice(1)) {
    const resolved = path.resolve(dir);
    while (shared !== path.dirname(shared) && path.relative(shared, resolved).startsWith('..')) {
      shared = path.dirname(shared);
    }
  }
  return shared;
}
