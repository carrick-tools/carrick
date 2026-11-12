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
import { type UnresolvedAtAnchor } from './deep-walk.js';
/**
 * Name the files of `program` the surface emit cannot carry (carrick#2418):
 * the sources of the projects its owner references, which the anchor program
 * reads and the emit reaches only as unbuilt output. A position typed from
 * one of them counts as unresolved, so a top type the emit leaves there is
 * labelled with its cause and never as declared.
 */
export declare function markUncarried(program: ts.Program, uncarried: (fileName: string) => boolean): void;
/**
 * The unresolved placeholders inside `type` (read at `location` in `sourceFile`),
 * or `undefined` when it has none — the common case, which costs one walk.
 *
 * `pathPrefix` maps the anchor type's member paths onto the paths of the alias
 * the surface declares when the two differ: a symbol anchor restoring array
 * depth prints `import('./m').Row[]`, whose members sit under `<0>`.
 */
export declare function unresolvedAtAnchor(program: ts.Program, sourceFile: ts.SourceFile, type: ts.Type, location: ts.Node, pathPrefix?: string): UnresolvedAtAnchor | undefined;
/**
 * Module specifiers, as written, that do not resolve from `sourceFile` or from
 * any source module it imports, breadth-first so the nearest come first, with
 * relative specifiers ahead of package names. Installed packages and the
 * default library are not descended into.
 */
export declare function unresolvedSpecifiersReachableFrom(program: ts.Program, sourceFile: ts.SourceFile): string[];
