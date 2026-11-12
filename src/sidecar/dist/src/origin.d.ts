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
import type { Project, ResolutionHostFactory, ts } from 'ts-morph';
import type { ExpandOrigin } from './type-structural-expander.js';
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
export declare function isExternalOrigin(program: ts.Program, sourceFile: ts.SourceFile, repoRoot: string, imports?: ExternalImports): boolean;
/**
 * Every file a ts-morph project's resolver reached as an external-library
 * import, recorded as the resolver answers (carrick#1731).
 *
 * The compiler keeps that verdict on each program (rule 4 of
 * `isExternalOrigin`), but ts-morph rebuilds the program after any edit to the
 * project, such as a probe file created and removed or a file rewritten and
 * restored, and by default passes every file it has loaded as a ROOT file.
 * The compiler marks no root file as external, so after the first edit no
 * dependency was. Where a path test cannot answer (a Deno project serves npm
 * types from its own cache), every library type then read as the user's:
 * framework machinery was published as a contract and the printer inlined
 * library types it keeps by name. The resolver's answer for a file does not
 * change with the project's history, so it is kept here for as long as the
 * project lives. The loader's projects now pin their roots (carrick#3146), so
 * a rebuild no longer names a dependency as a root; the record stays for the
 * Deno resolver and for any project the loader does not build.
 *
 * The compiler's verdict also carries down: a file a library imports, even by
 * a relative path the resolver answers as not external, is reached while the
 * compiler is inside an external import and is marked external too. A Deno
 * npm package's own relative imports are answered that way, so an answer for
 * an import made from a recorded file is recorded as well.
 */
export declare class ExternalImports {
    private readonly files;
    /** `factory` as it was, with every answer it gives recorded. */
    recording(factory: ResolutionHostFactory): ResolutionHostFactory;
    /** Whether a resolution reached `fileName` as an external-library import. */
    has(fileName: string): boolean;
    /** Record the external answers to `containingFile`'s imports. */
    private record;
}
/** Keep `imports` as the record of `project`, which resolves through it. */
export declare function registerExternalImports(project: Project, imports: ExternalImports): void;
/**
 * The record of a project the loader built with a recording resolver, or
 * `undefined` for any other project, which reads the compiler's flag alone.
 */
export declare function externalImportsOf(project: Project): ExternalImports | undefined;
/** The program, service root and import record the printer decides library origin from. */
export declare function expandOriginOf(project: Project, repoRoot: string): ExpandOrigin;
