/**
 * The files a program was built from, and files added to a program in an
 * order the caller gives (carrick#2027).
 *
 * With `stableTypeOrdering` the compiler orders two types of the same name by
 * where their declarations sit in the program's file list. That list follows
 * the program's root files: the compiler lists each root after the files it
 * imports. ts-morph hands the compiler every file it has loaded as a root, in
 * the order it loaded them, and a process asked about a file outside its
 * tsconfig loads it when it is asked. Two processes asked the same questions
 * in different orders therefore build from roots in different orders, and
 * print a union of two same-named types in different orders.
 *
 * The roots decide the whole program, not only its order. A dependency file
 * the compiler reached by resolving an import is not the same to it as the
 * same file named as a root: a second copy of a package reached by resolution
 * is folded into the first, and named as a root it is read as a file of its
 * own and its imports are followed. So the list here is the roots themselves.
 * A process that has built only what its tsconfig names, given another
 * process's roots in order, ends with the same roots and builds the same
 * program.
 */

import type { Project } from 'ts-morph';

/**
 * The root files `project`'s program was built from, in order, as absolute
 * paths: every file the project had loaded when the program was last built,
 * the compiler's library files and the dependencies earlier builds reached
 * among them.
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
