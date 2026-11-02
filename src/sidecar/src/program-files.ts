/**
 * The files a program was built from, and files added to a program in an
 * order the caller gives (carrick#2027, carrick#3146).
 *
 * With `stableTypeOrdering` the compiler orders two types of the same name by
 * where their declarations sit in the program's file list. That list follows
 * the program's root files: the compiler lists each root after the files it
 * imports. ts-morph hands the compiler every file it has loaded as a root, in
 * the order it loaded them, and a process asked about a file outside its
 * tsconfig loads it when it is asked. Two processes asked the same questions
 * in different orders would therefore build from roots in different orders,
 * and print a union of two same-named types in different orders.
 *
 * The roots decide the whole program, not only its order. A dependency file
 * the compiler reached by resolving an import is not the same to it as the
 * same file named as a root: a second copy of a package reached by resolution
 * is folded into the first, and named as a root it is read as a file of its
 * own and its imports are followed. ts-morph names every file it has loaded
 * as a root at each rebuild, the dependencies earlier builds reached among
 * them, so a process's roots grew with its rebuilds (282 to 3,680 in one
 * measured service) and a call's program depended on its process's history.
 * `pinProgramRoots` ends that: a program's roots are the project's own files,
 * never the compiler's library files or installed dependencies, in ts-morph's
 * load order, and a rebuild does not change them.
 */

import { Program, type Project, type SourceFile } from 'ts-morph';

interface ResettableProgram {
  _reset(rootNames: readonly string[], host: unknown): void;
}

// `pinProgramRoots` wraps ts-morph's private `_reset`. If an upgrade changes
// its shape, fail at load rather than bring the growth back silently.
if (typeof (Program.prototype as unknown as ResettableProgram)._reset !== 'function' ||
    (Program.prototype as unknown as ResettableProgram)._reset.length !== 2) {
  throw new Error('ts-morph Program._reset changed: pinProgramRoots must be revisited (carrick#3146)');
}

/**
 * Pin the roots of every program `project` builds (carrick#3146): its own
 * files (ts-morph's in-project files, in load order), less `resolvedByProject`,
 * the files ts-morph's dependency resolution marked in-project when the
 * project was built (a workspace or linked package outside `node_modules`).
 * That is the first build's roots plus every file added through the project
 * API since. The compiler's library files and the dependencies it reached are
 * not roots, so a rebuild after an edit builds the same program and
 * TypeScript can reuse the old one.
 */
export function pinProgramRoots(project: Project, resolvedByProject: readonly SourceFile[]): void {
  const resolved = new Set<string>(resolvedByProject.map((file) => file.getFilePath()));
  const program = project.getProgram() as unknown as ResettableProgram;
  const reset = program._reset.bind(program);
  program._reset = (rootNames, host) => {
    const own = new Set<string>(project.getSourceFiles().map((file) => file.getFilePath()));
    reset(rootNames.filter((name) => own.has(name) && !resolved.has(name)), host);
  };
}

/**
 * Set `sourceFile`'s text and keep the module format the project's resolver
 * stamped on it (carrick#3146). Under `node16`..`nodenext` ts-morph's
 * `replaceWithText` makes a fresh node without that format, and TypeScript
 * then reuses only part of the old program. A text edit cannot change a file's
 * format, which comes from its extension and its `package.json` scope, and the
 * resolver stamps the same value in the rebuild, so the program is unchanged
 * and the old one is reused whole.
 */
export function replaceFileText(sourceFile: SourceFile, text: string): void {
  const format = sourceFile.compilerNode.impliedNodeFormat;
  sourceFile.replaceWithText(text);
  (sourceFile.compilerNode as { impliedNodeFormat?: typeof format }).impliedNodeFormat = format;
}

/**
 * The root files `project`'s program was built from, in order, as absolute
 * paths: the project's own files, never the compiler's library files or
 * installed dependencies (carrick#3146).
 */
export function listProgramFiles(project: Project): string[] {
  return [...project.getProgram().compilerObject.getRootFileNames()];
}

/**
 * Add to `project` each of `files` it has not loaded, in the given order, and
 * build the program once if any was added. A file it has loaded keeps its
 * place. A file that cannot be read is skipped and reported to `onSkipped`.
 *
 * Nothing is built before the files join. A project built from a tsconfig has
 * built once already, from the files the tsconfig names, as every process's
 * does; one built from source patterns or a Deno graph has loaded only its
 * own files, and the given list names whatever the other process's first
 * build loaded, in the place that build put it.
 *
 * @returns how many files were added
 */
export function addProgramFiles(
  project: Project,
  files: readonly string[],
  onSkipped: (file: string, reason: string) => void
): number {
  let added = 0;
  for (const file of new Set(files)) {
    if (project.getSourceFile(file) !== undefined) continue;
    try {
      project.addSourceFileAtPath(file);
      added += 1;
    } catch (err) {
      onSkipped(file, err instanceof Error ? err.message : String(err));
    }
  }
  // ts-morph drops its program on each add and builds the next one when it is
  // read, so reading it once here is the one rebuild the adds need, done in
  // this request rather than in the next one.
  if (added > 0) project.getProgram().compilerObject;
  return added;
}
