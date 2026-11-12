/**
 * Capture-time self-check (design doc, Capture step 8, amendment 1), with
 * per-alias closure attribution.
 *
 * The stub tree is typechecked standalone with `skipLibCheck: false` --
 * spike-verified as load-bearing: the tree is entirely .d.ts and skipLibCheck
 * skips declaration files wholesale, making the gate vacuous. It also sets
 * `skipDefaultLibCheck: true`, which skips only the compiler's own default
 * lib files, never a stub file (see `SELF_CHECK_OPTIONS`). When the
 * source repo has node_modules, resolution is pointed at it via a temporary
 * node_modules symlink inside the stub, so externals resolve exactly as they
 * will at check time against installed pins.
 *
 * Classification per alias (three-way, keyed on diagnostics, never on
 * printed type text):
 *  - ok: resolves to a concrete type.
 *  - allowlisted_external: resolution failed only through external
 *    specifiers pinned in the stub's dependencies, on a bare checkout. The
 *    alias KEEPS its serialization tier; the check-phase probe gates
 *    (any/unknown/never, both sides) are the backstop.
 *  - decayed_internal: a dangling internal specifier, an unpinned external,
 *    or a top-type resolution with no allowlisted explanation.
 *
 * Attribution is per-alias closure: failed specifiers are blamed on an alias
 * only if they occur in a file reachable from that alias's surface statement
 * (import-type seeds, then BFS over relative imports). The spike's
 * file-granularity shortcut is gone -- including on the SURFACE file itself,
 * which holds every alias, so a file-granular bucket there blamed the whole
 * service for one alias's dangling specifier (cloud#1184). A surface
 * diagnostic is attributed by `export type` statement span, exactly as
 * check-poison.ts contains poison; one that no statement covers keeps the
 * service-wide bucket.
 */
import ts from 'typescript';
import type { CaptureAliasRecord } from './api.js';
import type { ResolvedAnchor } from './anchors.js';
import type { WriteGuard } from './guarded-fs.js';
/** One surface entry of the tree and the anchors its aliases came from. */
export interface SelfCheckSurface {
    surfaceAbsPath: string;
    resolved: ResolvedAnchor[];
}
export interface SelfCheckArgs {
    /** Writes are held to the stub (carrick#1748). */
    guard: WriteGuard;
    stubDir: string;
    /**
     * The surfaces checked over the tree, each with records of its own. Two when
     * a capture's literal backfill holds the same tree as its first surface
     * (carrick#2041): the second sits beside the first, a module nothing
     * imports, so neither changes what the other's aliases read.
     */
    surfaces: SelfCheckSurface[];
    pinned: Record<string, string>;
    bareCheckout: boolean;
    /** Producer repo root; its node_modules (if any) backs resolution. */
    repoRoot: string;
    compilerHost?: (options: ts.CompilerOptions) => ts.CompilerHost;
    /**
     * Absolute stub path of each declaration emitted for a source outside the
     * service's rootDir -> that source's own path. A bare specifier such a
     * declaration names that the stub's links do not reach is resolved from the
     * source's place in the checkout, as the compiler resolved it at emit.
     */
    outsideSources?: Readonly<Record<string, string>>;
}
/** The records of each surface, in `args.surfaces` order. */
export declare function selfCheckStub(args: SelfCheckArgs): CaptureAliasRecord[][];
/**
 * The options of every self-check program.
 *
 * `skipLibCheck: false` keeps every stub `.d.ts` checked (see module header).
 * `skipDefaultLibCheck: true` skips only files the program loaded as a default
 * lib (the default lib, `lib` entries, `/// <reference lib>` targets), which
 * was most of a capture's check time (carrick#2158). It is safe here because
 * the self-check reads only resolution diagnostics (2307, 2792, `Cannot find
 * name|namespace`), attributes them only through the stub tree, and the
 * compiler reports each at the referencing stub node when it checks that stub.
 *
 * It must NOT be set on the judge's `tsc` (`CHECKER_TSCONFIG`): some checks of
 * a merged interface run only when its FIRST declaration is checked, so a stub
 * that augments `interface Window { 0: number }` loses its TS2411 when
 * `lib.dom.d.ts` is skipped, and the judge counts any stub diagnostic.
 */
export declare const SELF_CHECK_OPTIONS: Readonly<ts.CompilerOptions>;
