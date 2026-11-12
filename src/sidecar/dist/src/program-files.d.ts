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
import { type Project, type SourceFile } from 'ts-morph';
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
export declare function pinProgramRoots(project: Project, resolvedByProject: readonly SourceFile[]): void;
/**
 * Set `sourceFile`'s text and keep the module format the project's resolver
 * stamped on it (carrick#3146). Under `node16`..`nodenext` ts-morph's
 * `replaceWithText` makes a fresh node without that format, and TypeScript
 * then reuses only part of the old program. A text edit cannot change a file's
 * format, which comes from its extension and its `package.json` scope, and the
 * resolver stamps the same value in the rebuild, so the program is unchanged
 * and the old one is reused whole.
 */
export declare function replaceFileText(sourceFile: SourceFile, text: string): void;
/**
 * The root files `project`'s program was built from, in order, as absolute
 * paths: the project's own files, never the compiler's library files or
 * installed dependencies (carrick#3146).
 */
export declare function listProgramFiles(project: Project): string[];
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
export declare function addProgramFiles(project: Project, files: readonly string[], onSkipped: (file: string, reason: string) => void): number;
