/**
 * v2 check core: "tsc as the judge"
 * (docs/reference/type-compat-synthetic-monorepo.md, Check phase).
 *
 * Consumes two or more capture stub packages, assembles a scratch synthetic
 * monorepo, installs pinned deps with the vendored pnpm (isolated linker),
 * generates one probe per matched pair, runs the vendored `tsc` CLI, and
 * classifies diagnostics into the four buckets. Deterministic given the stubs
 * and pairs: verdicts are byte-stable across runs (the scrub pass removes the
 * per-run temp path, and the pair IDs / ordering derive from the pair specs).
 *
 * Seam: this file imports only node builtins, `typescript` (for the version
 * string), and the rest of this bundle. The sidecar reaches it only through
 * ./index.js. The judge is the `tsc` CLI (no compiler API), which keeps the
 * check phase TS7-ready by construction.
 */
import type { CheckOptions, CheckProgressPhase, CheckResult } from './api.js';
import { type RawDiagnostic } from './check-classify.js';
export type CheckProgress = (phase: CheckProgressPhase, message: string) => void;
interface SpawnResult {
    code: number | null;
    /** The signal that ended the process, when one did. */
    signal: NodeJS.Signals | null;
    stdout: string;
    stderr: string;
    /** Which time limit killed the process, when one did. */
    timedOut: false | 'total' | 'idle';
}
/**
 * The check install's limits (carrick#3021). A healthy install prints
 * progress about once a second (longest gap measured: 1 s), so silence is the
 * sign of a hang: the install is killed after `DEFAULT_INSTALL_IDLE_MS` with
 * no output, and a total cap bounds one that keeps printing. Both are far
 * above the slowest healthy run measured (54 s, install plus judge).
 * `CARRICK_CHECK_INSTALL_TIMEOUT_MS` replaces the total cap.
 */
export declare const DEFAULT_INSTALL_IDLE_MS = 120000;
export declare const DEFAULT_INSTALL_TIMEOUT_MS = 1800000;
/** The total cap: the option, else the environment, else the default. */
export declare function installTotalLimitMs(opt: number | undefined, env?: NodeJS.ProcessEnv): number;
/**
 * Why the check install did not finish, as a gate and a sentence the verdicts
 * carry. A hang and a refused connection are named apart from a plain failure.
 */
export declare function installFailure(install: Pick<SpawnResult, 'timedOut' | 'stderr' | 'stdout'>, limits: {
    idleMs: number;
    totalMs: number;
}): {
    gate: string;
    reason: string;
};
/**
 * Whether a process's stderr carries V8's fatal heap-exhaustion line
 * (`Reached heap limit`, `Ineffective mark-compacts near heap limit`, and the
 * other forms all end in `JavaScript heap out of memory`), and not one of the
 * hard size limits that end the same way (carrick#2209).
 *
 * The scanner applies the same rule to the sidecar's own stderr
 * (carrick#2208); the two must stay in step.
 */
export declare function carriesHeapLimitLine(stderr: string): boolean;
/**
 * Whether the native compiler (TypeScript 7, a Go program) died out of
 * memory. The Go runtime prints `fatal error: runtime: out of memory` and
 * exits 2; V8's line above never appears, and a heap cap passed through
 * `NODE_OPTIONS` reaches only the launcher, not the compiler.
 */
export declare function carriesNativeOutOfMemoryLine(output: string): boolean;
/**
 * Whether a diagnostic is about the compiler's configuration rather than a
 * file it checked: one placed in the tsconfig the judge wrote, or "no inputs"
 * (TS18003). Keyed on the file, not on the TS5xxx option range: some of those
 * codes sit in a source file (TS5097, an import ending in `.ts`) and poison
 * only the stub that holds it.
 */
export declare function isConfigDiagnostic(d: RawDiagnostic): boolean;
/** Why the compiler ended abnormally, when its output names a known cause. */
export declare function abnormalCause(output: string): string | undefined;
/** What a Node child of the sidecar is started with. */
export interface ChildHeap {
    /** `NODE_OPTIONS` for the child. */
    nodeOptions: string | undefined;
    /** The heap cap passed to the child, in MB, or null when it runs at V8's
     * default. */
    heapCapMb: number | null;
}
/**
 * The heap a Node child of the sidecar (the check's compiler) is given
 * (carrick#2209).
 *
 * The scanner sizes the sidecar's cap to the machine and passes it as a
 * command-line flag, which a child does not inherit, so the child ran at V8's
 * default. Handing it the same cap would overcommit the box: the sidecar's
 * cap is already 3/4 of memory, and two such caps turn a heap error into an
 * OS kill, which the sizing exists to avoid. So the child gets what is left of
 * the sidecar's cap after what the sidecar holds now (`rssMb`), and only when
 * that is more than V8's default; otherwise `NODE_OPTIONS` is left as it is
 * and the child runs at the default, as before.
 */
export declare function childHeap(execArgv: readonly string[], existing: string | undefined, rssMb: number): ChildHeap;
/**
 * Locate a vendored executable the way node locates a package: walk up from
 * the bundle and take the first `node_modules/.bin/<name>` that exists.
 *
 * Where the bins sit depends on how the sidecar arrived. A checkout and the
 * release tarball both keep them in the sidecar's own `node_modules`, so the
 * first candidate hits and nothing changes. An npm install never can: npm
 * strips nested `node_modules` from a published tarball, so the sidecar ships
 * as a plain directory inside the package and its dependencies are installed
 * by the package that carries it — hoisted to the install root, or nested one
 * level up when a version conflict forces it. Assuming one shape is what made
 * an installed `carrick` degrade every pair to unverifiable (carrick#833).
 *
 * `from` is injectable so the layouts can be pinned by test; production always
 * starts at the bundle. Returns the sidecar-local path when nothing exists,
 * which is the caller's "not found" signal and the most useful thing to name.
 */
export declare function resolveVendoredBin(name: string, from?: string): string;
/**
 * The first `node_modules/<rel>` at or above `from`, the same walk as
 * {@link resolveVendoredBin}; the sidecar-local path when none exists.
 */
export declare function resolveVendored(rel: string, from?: string): string;
/**
 * The package that brings the judge's compiler: TypeScript 7 under an alias
 * beside the TypeScript 6 that capture and inference read through ts-morph.
 * Its platform package holds the native executable the judge runs. Both
 * packages declare a `tsc` bin, so `node_modules/.bin/tsc` may be either; the
 * judge never reads it.
 */
export declare const JUDGE_COMPILER_PACKAGE = "typescript-native";
/**
 * The judge's tsc: the host's native executable, from the platform package
 * the compiler installs as an optional dependency (`@typescript/
 * typescript-<platform>-<arch>`), found by the same walk as the bins. Where it
 * is not installed (an install that omits optional dependencies, such as the
 * cloud runner's, or a pnpm store the walk does not reach), TypeScript 6's own
 * tsc, which ships for ts-morph anyway. The two read every probe the same
 * (measured on 1,018 real probes; the one wording difference moves only a
 * verdict's `codes`), so the fallback changes no verdict, only the time.
 */
export declare function judgeTscPath(from?: string, platform?: string, arch?: string): string;
/** Which compiler a tsc path is, in words, for the run log and the result. */
export declare function judgeLabel(tscPath: string): string;
/** Run the deterministic check phase. Async: install + tsc run off the event
 * loop so the sidecar stays responsive and can emit keepalive frames. */
export declare function runCheck(opts: CheckOptions, onProgress?: CheckProgress): Promise<CheckResult>;
export {};
