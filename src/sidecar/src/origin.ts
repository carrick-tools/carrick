/**
 * Where a declaration comes from: the user's own source, or the runtime and
 * the packages it installed.
 *
 * One module because more than one layer asks the question — the inference
 * path (`type-inferrer.ts`, deciding whether a type is framework machinery)
 * and the structural printer (`type-structural-expander.ts`, deciding whether
 * to inline a type's members or keep it by name). They must answer it the same
 * way or one layer inlines what the other suppresses.
 *
 * The capture side keeps its own copy in `capture/machinery.ts`: that seam
 * forbids importing anything but node builtins, `typescript` and its own
 * bundle, so the two are kept in lockstep by `machinery-indicator-mirror.test.ts`
 * rather than by sharing this module.
 */

import * as fs from 'node:fs';
import * as path from 'node:path';
import type { ts } from 'ts-morph';

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
 * Lockstep mirror of `isExternalOrigin` in `capture/machinery.ts` (the capture
 * seam forbids sharing a module); `machinery-indicator-mirror.test.ts` guards
 * the pair on a real program.
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
