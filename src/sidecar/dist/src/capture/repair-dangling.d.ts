/**
 * Repair an emitted declaration whose own import does not resolve
 * (carrick#1397).
 *
 * A `symbol` anchor prints nothing: its surface line is
 * `import('./m').RenderRequest` and the shape lives in the `.d.ts` the
 * compiler emitted for `m`. When that file imports a module the scanned
 * checkout does not have — a dependency that was not installed, a generated
 * module that was never generated — the stub reports the module missing, the
 * self-check records a dangling internal specifier, and the scanner's publish
 * gate refuses the alias whole. Twenty resolved members are thrown away with
 * the one that did not resolve, which is exactly the loss carrick#1377 named
 * for the two PRINT paths.
 *
 * So the declaration is repaired: the import that did not resolve is dropped
 * and every type it bound is written `unknown` at its own position. This is
 * file-granular — the same emitted file backs every alias whose closure
 * reaches it — and it says nothing the source program did not already say:
 * those names were TypeScript's unresolved-reference placeholder there too,
 * which is why the anchor recorded their member paths as unresolved.
 *
 * Fail-closed in two places. A name used where `unknown` is not a type — a
 * heritage clause, a type-parameter position — leaves the file untouched, so
 * the alias keeps its honest refusal rather than gaining a broken declaration.
 * And the caller re-checks the repaired tree: a name the rewrite did not reach
 * turns into a `Cannot find name` diagnostic, which the self-check reads back
 * as the same dangling specifier.
 *
 * Seam: node builtins + `typescript`, like the rest of this directory.
 */
import type { WriteGuard } from './guarded-fs.js';
/** What one file's repair removed, for the caller's re-check. */
export interface RepairedFile {
    /** Specifiers whose import statements were dropped. */
    specifiers: string[];
    /** Names those imports bound, now written `unknown` wherever they were used. */
    names: string[];
}
/**
 * Drop every import of a failing specifier from each file and write `unknown`
 * where its names were used.
 *
 * `failing` maps an absolute file path to the specifiers the stub could not
 * resolve from it. Returns the files actually rewritten; a file whose names
 * cannot all be replaced by `unknown` is left exactly as it was.
 *
 * Only a file `tree` (a guard over the stub's types tree) lets it write is
 * repaired (carrick#1742). The self-check program also loads files the stub
 * merely reaches: the repo's own sources, through a workspace package linked
 * into the `node_modules` the stub borrows, or a runtime's dependency cache.
 * Those report their missing modules exactly as an emitted declaration does,
 * and they belong to the user: a scan reads them and never writes them. The
 * guard decides on the resolved path, so a file reached through the link is
 * the repo's, whatever path the program loaded it by.
 */
export declare function repairDanglingImports(failing: Map<string, Set<string>>, tree: WriteGuard): Map<string, RepairedFile>;
