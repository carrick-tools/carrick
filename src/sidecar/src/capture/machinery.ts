/**
 * The capture's origin gate for the machinery test (carrick#371).
 *
 * The test itself is `typeIsOrContainsMachinery` in `site-node.ts`, the one
 * the inferrer runs too (carrick#3500). What the capture hands it is where a
 * declaration comes from, read off the program alone: the capture builds each
 * program once and never edits it.
 */

import ts from 'typescript';
import { isInsideCheckout } from './checkout.js';
import { typeIsOrContainsMachinery as siteTypeIsOrContainsMachinery } from './site-node.js';

/**
 * True when `type`, resolved in `program`, IS or CONTAINS framework
 * machinery, its origin read for the service at `repoRoot`.
 */
export function typeIsOrContainsMachinery(
  program: ts.Program,
  type: ts.Type,
  repoRoot: string
): boolean {
  return siteTypeIsOrContainsMachinery(program.getTypeChecker(), type, (sourceFile) =>
    isExternalOrigin(program, sourceFile, repoRoot)
  );
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
 * Lockstep mirror of `isExternalOrigin` in `origin.ts` (the capture seam
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
