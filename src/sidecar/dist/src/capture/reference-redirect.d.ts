/**
 * How a program built from one owner config reads the projects that config
 * references (carrick#2418): as an editor reads them.
 *
 * A workspace package a service references is usually not built on a scanned
 * checkout: its `package.json` points `types` at `dist/index.d.ts`, which
 * does not exist, so the import read as the error type. The compiler's editor
 * mode (`projectReferences` with `useSourceOfProjectReferenceRedirect`) reads
 * the package's source instead, under the package's own options. The
 * sidecar's programs cannot be handed references (ts-morph never passes them,
 * and the capture's emit would then skip the sources), so this module does
 * what the compiler's `updateHostForUseSourceOfProjectReferenceRedirect` does,
 * in the resolver:
 *
 *  1. Every project in the owner's reference closure is parsed through the
 *     read bound (`ProjectGraph`), its options through
 *     `sidecarCompilerOptions`.
 *  2. Each of its inputs maps to its declaration output, by the compiler's
 *     own `getOutputFileNames` with declarations on (a project with `outFile`
 *     has no per-file map and is skipped). A project that leaves `rootDir`
 *     unset is laid out as TypeScript 5 lays it out: under the config's
 *     directory when `composite`, else under the common directory of its
 *     inputs. TypeScript 6 puts every such project under the config's
 *     directory (`src/index.ts` to `dist/src/index.d.ts`), where a package
 *     written against 5 points its `types` at `dist/index.d.ts`.
 *  3. The resolver's host answers that an output exists while its source
 *     does, and that the directories outputs sit in exist. An output is
 *     looked up by the real path of its deepest existing ancestor: a package
 *     reached through a `node_modules` link has no `dist/`, so the real path
 *     of the output itself is the link path and would miss the map.
 *  4. An import made from a referenced project's own file resolves under that
 *     project's options (`redirectedReference`), and an answer that lands on
 *     an output becomes its source. A source inside the checkout and under no
 *     `node_modules` is not an external-library import, so the origin gates
 *     read it as the user's code.
 *
 * A package the owner does not reference stays unresolved, as in the editor.
 * Nothing here writes, and every read goes through the host it wraps.
 */
import ts from 'typescript';
import { type ServiceProject } from './project-references.js';
/** The part of a resolved module this module reads and rewrites. */
interface ResolvedModuleLike {
    resolvedFileName: string;
    extension?: string;
    isExternalLibraryImport?: boolean;
    resolvedUsingTsExtension?: boolean;
}
/** The part of a module resolution host this module wraps. */
interface ResolutionHostLike {
    fileExists(fileName: string): boolean;
    directoryExists?(directoryName: string): boolean;
}
export interface ReferenceRedirect {
    /**
     * The referenced project that lists `containingFile`, for
     * `ts.resolveModuleName`'s `redirectedReference`; `undefined` for a file
     * of the owner itself.
     */
    referenceFor(containingFile: string): ts.ResolvedProjectReference | undefined;
    /** `host` answering for the outputs of referenced projects (step 3). */
    resolutionHost<H extends ResolutionHostLike>(host: H): H;
    /** `answer`, or the source behind it when it is a referenced output (step 4). */
    sourceAnswer<R extends ResolvedModuleLike | undefined>(answer: R): R;
}
/**
 * The redirect for a program whose owner reaches `references` (its reference
 * closure, from `ProjectGraph.closureOf`), or `undefined` when there are none.
 * An output two of the projects write from different sources goes to the
 * first one, or, with `ambiguousUnresolved`, to neither: nothing says which
 * the entry is, so the import stays unresolved (carrick#1910).
 */
export declare function referenceRedirect(references: readonly ServiceProject[], repoRoot: string, { ambiguousUnresolved }?: {
    ambiguousUnresolved?: boolean;
}): ReferenceRedirect | undefined;
/**
 * For the loader, which builds a program from one config path: the redirect
 * for that config's reference closure. A config that references nothing is
 * not parsed (`references` is never inherited through `extends`).
 */
export declare function referenceRedirectOf(configPath: string, repoRoot: string): ReferenceRedirect | undefined;
/**
 * For the position rows' process only (carrick#1910): `referenceRedirectOf`,
 * with the projects of every workspace package the install links into the
 * checkout added to the reference closure, whether or not the config
 * references them. A package's projects are every `tsconfig*.json` in its
 * root. An output two of them write from different sources is followed to
 * neither. Every other program keeps `referenceRedirectOf`: infer's text is
 * the compat judge's anchor, so widening it would move verdicts.
 */
export declare function workspaceSourceRedirectOf(configPath: string, repoRoot: string): ReferenceRedirect | undefined;
/**
 * For the capture's anchor program: `host` resolving every import through
 * `redirect`, with the module resolution cache it uses handed to the program.
 * A host with its own resolver and no cache makes declaration emit name a
 * transitive package through another (TS2742).
 */
export declare function redirectingCompilerHost(host: ts.CompilerHost, options: ts.CompilerOptions, redirect: ReferenceRedirect): ts.CompilerHost;
export {};
