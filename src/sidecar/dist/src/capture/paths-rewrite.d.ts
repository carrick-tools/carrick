/**
 * Post-emit specifier rewrite pass (design doc, Capture step 3 -- REQUIRED
 * component). Declaration emit ships tsconfig-`paths`-mapped specifiers
 * verbatim (`@app/models/item` dangles after relocation into the stub), and
 * node-builder-printed import types can carry absolute source paths. Both
 * are mapped onto tree-relative specifiers here, at capture time, in the
 * producer's own resolution context -- `paths` cannot be fixed at check time
 * (program-global, and namespaces collide across stubs).
 *
 * An absolute path into an installed package becomes that package's bare
 * specifier (installed-package.ts). Other specifiers that map outside the
 * emitted tree are left untouched: the per-alias self-check classifies them
 * as dangling internals with a recorded reason, which is the honest outcome.
 */
import ts from 'typescript';
import type { WriteGuard } from './guarded-fs.js';
import { type PathsPattern } from './specifiers.js';
export interface RewriteArgs {
    /** Writes are held to the stub (carrick#1748). */
    guard: WriteGuard;
    /** Absolute path of the stub's types/ directory. */
    typesDir: string;
    /** Tree-relative emitted file paths (as recorded in emitted_files, without
     * the leading "types/"). */
    files: string[];
    /** The producer repo's parsed compiler options. */
    options: ts.CompilerOptions;
    /** Absolute path of the tsconfig the options came from. */
    configPath: string;
    /** Effective rootDir the emit ran with (tree layout mirrors it). */
    entryDir: string;
    /**
     * Declarations of sources outside rootDir, placed under the tree
     * (carrick#1770): absolute source-side path without extension -> tree path.
     */
    outside: Map<string, string>;
}
export declare function parsePathsPatterns(options: ts.CompilerOptions, configPath: string): PathsPattern[];
export interface RewriteResult {
    /** Specifiers rewritten across every emitted file. */
    rewrites: number;
    /**
     * Installed packages an absolute specifier was rewritten into, with their
     * installed versions: the stub must pin them for the bare specifier to
     * resolve at check time.
     */
    pins: Record<string, string>;
    /**
     * The same packages' real install directories, for the self-check's
     * resolution only. Absolute paths: never written into the stub.
     */
    installs: Record<string, string>;
}
/**
 * Rewrite paths-mapped, absolute-internal and absolute installed-package
 * specifiers in every emitted file (carrick#1174 for the last: a path into
 * `node_modules` or a runtime npm cache becomes the package's bare specifier).
 */
export declare function rewriteEmittedSpecifiers(args: RewriteArgs): RewriteResult;
