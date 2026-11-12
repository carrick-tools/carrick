/**
 * Type-compat v2 capture core: "tsc as the serializer"
 * (docs/reference/type-compat-synthetic-monorepo.md, Capture phase).
 *
 * Produces a types-only stub package for one service:
 *
 *   @carrick/<service>/
 *   |- package.json            name, types entry, pinned deps (exact versions)
 *   |- tsconfig.snapshot.json
 *   |- carrick-manifest.json   per-alias records + fidelity metric
 *   `- types/
 *      |- surface.d.ts         entry: export type <alias> = ...
 *      `- nested .d.ts tree    compiler-emitted declaration closure
 *
 * Two-phase flow:
 *   Phase A (analysis): a placeholder surface entry + the anchors' source
 *     files form a program; addressable anchors run their guards, anonymous
 *     anchors are located and printed via the SymbolTracker-backed node
 *     builder (anchored at their placeholder -- the destination file).
 *   Phase B (emit): the final entry runs `tsc --noCheck --declaration
 *     --emitDeclarationOnly` with the repo's own parsed options, plus every
 *     detected augmentation file as an extra root; the tree is relocated
 *     into the stub, specifiers are rewritten, deps pinned, and the
 *     per-alias self-check classifies the result.
 *
 * Seam note: this directory is the whole v2 capture bundle. It imports only
 * node builtins and `typescript`; the rest of the sidecar reaches it only
 * through ./api.js types and this file's `captureStub`.
 */
import type { CaptureStubOptions, CaptureStubResult } from './api.js';
import { WriteGuard } from './guarded-fs.js';
export type { CaptureStubOptions, CaptureStubResult } from './api.js';
export { DenoProject, findDenoConfig } from './deno-project.js';
export { sidecarCompilerOptions } from './compiler-options.js';
export { isInsideCheckout, untrustedReadBound, UNTRUSTED_TREE_ENV, type ReadBound } from './checkout.js';
export { serviceConfigPath } from './project-references.js';
export { referenceRedirectOf, workspaceSourceRedirectOf } from './reference-redirect.js';
export { findServiceTsconfig, GoverningConfigs } from './service-config.js';
export { WriteGuard } from './guarded-fs.js';
export { installedPackageDirectory } from './installed-package-directory.js';
export { isDeclarationFileName, packageNameOf } from './specifiers.js';
export type { InternalTypeChecker } from './node-builder.js';
export { toPosix } from './installed-package.js';
export { constituentsOf } from './result-label.js';
export { moduleExport } from './anchors.js';
export { runCheck } from './check.js';
export type { CheckProgress } from './check.js';
export { jsonWireDeclarations } from './check-probe.js';
export { findDisqualifyingTopTypes } from './deep-walk.js';
/**
 * The name of THIS capture's surface entry, without an extension
 * (carrick#1046).
 *
 * The entry has to live inside the effective `rootDir` — an entry beside a
 * `rootDir` of `src` fails TS6059 — so it is written into the scanned tree and
 * unlinked afterwards. One name per repo root made two captures of the same
 * tree share a single file: whichever finished first unlinked it while the
 * other's program was still reading it, and every alias whose print anchors in
 * that destination then demoted to `structural_fallback` with an accessibility
 * reason that described the harness rather than the code. Concurrent captures
 * of one tree are ordinary — the test suite does it on every run, and two
 * services of a monorepo can share a root — so the name, not the locking, is
 * what has to give.
 *
 * A leftover from an interrupted capture is also identifiable as one process's
 * (carrick#1069), rather than a fixed name the next scan reads as source.
 */
export declare function surfaceEntryFileName(): string;
/**
 * Removes every surface entry this process still holds. Runs when the process
 * exits, and the sidecar's SIGTERM and SIGINT handlers exit through it, so a
 * check killed at its budget does not leave an entry in the user's tree
 * (carrick#3246). A SIGKILL cannot run it; the next capture's
 * [`removeStaleSurfaceEntries`] covers that.
 */
export declare function removeLiveSurfaceEntries(): void;
/** Records `entryPath` as held by this process until it is removed. */
export declare function holdSurfaceEntry(entryPath: string, guard: WriteGuard): void;
/**
 * Removes surface entries in `dir` left by a process that is no longer running
 * (a SIGKILLed check), before this capture writes its own (carrick#3246).
 * An entry of a running process, this one included, is another capture's
 * live file and stays.
 */
export declare function removeStaleSurfaceEntries(dir: string, guard: WriteGuard): void;
/** Same normalization intent as bundle_file_stems on the Rust side. */
export declare function sanitizeServiceName(name: string): string;
export declare function captureStub(opts: CaptureStubOptions): CaptureStubResult;
export { classifyStatusCodes, httpStatus, statesNoBody, statesNoShape, statusKeyCode, statusTableBody, statusTableOf, successEntries, } from './status-table.js';
