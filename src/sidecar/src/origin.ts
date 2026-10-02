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
import type { Project, ResolutionHostFactory, ts } from 'ts-morph';

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
 *     capture seam; both programs are built with that host. The compiler's
 *     record does not survive an edit to a ts-morph project, so `imports`, the
 *     resolver's answers kept for the project's life, is read beside it
 *     (`ExternalImports`, carrick#1731). One exclusion: a workspace package
 *     reached through a `node_modules` symlink is also marked external by the
 *     compiler but is the user's own source, so a file inside the checkout
 *     (the nearest `.git` above the service root) that carries no
 *     `node_modules` segment stays user source.
 *
 * Lockstep mirror of `isExternalOrigin` in `capture/machinery.ts` (the capture
 * seam forbids sharing a module); `machinery-indicator-mirror.test.ts` guards
 * the pair on a real program. The capture builds each program once and never
 * edits it, so it has no `imports` to read.
 */
export function isExternalOrigin(
  program: ts.Program,
  sourceFile: ts.SourceFile,
  repoRoot: string,
  imports?: ExternalImports
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
  const external = program.isSourceFileFromExternalLibrary(sourceFile) || imports?.has(file) === true;
  return external && !isInsideCheckout(file, repoRoot);
}

/**
 * Every file a ts-morph project's resolver reached as an external-library
 * import, recorded as the resolver answers (carrick#1731).
 *
 * The compiler keeps that verdict on each program (rule 4 of
 * `isExternalOrigin`), but ts-morph rebuilds the program after any edit to the
 * project, such as a probe file created and removed or a file rewritten and
 * restored, and passes every file it has loaded as a ROOT file. The compiler
 * marks no root file as external, so after the first edit no dependency is.
 * Where a path test cannot answer (a Deno project serves npm types from its own
 * cache), every library type then read as the user's: framework machinery was
 * published as a contract and the printer inlined library types it keeps by
 * name. The resolver's answer for a file does not change with the project's
 * history, so it is kept here for as long as the project lives.
 *
 * The compiler's verdict also carries down: a file a library imports, even by
 * a relative path the resolver answers as not external, is reached while the
 * compiler is inside an external import and is marked external too. A Deno
 * npm package's own relative imports are answered that way, so an answer for
 * an import made from a recorded file is recorded as well.
 */
export class ExternalImports {
  private readonly files = new Set<string>();

  /** `factory` as it was, with every answer it gives recorded. */
  recording(factory: ResolutionHostFactory): ResolutionHostFactory {
    return (moduleResolutionHost, getCompilerOptions) => {
      const host = factory(moduleResolutionHost, getCompilerOptions);
      return {
        ...host,
        ...(host.resolveModuleNames && {
          resolveModuleNames: (...args: Parameters<NonNullable<typeof host.resolveModuleNames>>) =>
            this.record(args[1], host.resolveModuleNames!(...args)),
        }),
        ...(host.resolveTypeReferenceDirectives && {
          resolveTypeReferenceDirectives: (
            ...args: Parameters<NonNullable<typeof host.resolveTypeReferenceDirectives>>
          ) => this.record(args[1], host.resolveTypeReferenceDirectives!(...args)),
        }),
      };
    };
  }

  /** Whether a resolution reached `fileName` as an external-library import. */
  has(fileName: string): boolean {
    return this.files.has(normalised(fileName));
  }

  /** Record the external answers to `containingFile`'s imports. */
  private record<T extends ReadonlyArray<ExternalAnswer | undefined>>(containingFile: string, answers: T): T {
    const fromLibrary = this.has(containingFile);
    for (const answer of answers) {
      if (answer?.resolvedFileName && (fromLibrary || answer.isExternalLibraryImport)) {
        this.files.add(normalised(answer.resolvedFileName));
      }
    }
    return answers;
  }
}

interface ExternalAnswer {
  readonly resolvedFileName?: string;
  readonly isExternalLibraryImport?: boolean;
}

function normalised(fileName: string): string {
  return path.resolve(fileName).replace(/\\/g, '/');
}

const externalImportsByProject = new WeakMap<Project, ExternalImports>();

/** Keep `imports` as the record of `project`, which resolves through it. */
export function registerExternalImports(project: Project, imports: ExternalImports): void {
  externalImportsByProject.set(project, imports);
}

/**
 * The record of a project the loader built with a recording resolver, or
 * `undefined` for any other project, which reads the compiler's flag alone.
 */
export function externalImportsOf(project: Project): ExternalImports | undefined {
  return externalImportsByProject.get(project);
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
