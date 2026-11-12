/**
 * The checkout a service sits in, and the bound an untrusted scan reads
 * inside (carrick#2230).
 *
 * `CARRICK_UNTRUSTED_TREE` (any non-empty value) is the scan's switch, read
 * the same way the scanner reads it. Unset, nothing here constructs a
 * wrapper and every caller passes the compiler what it passed before. Set,
 * the compiler reads no file whose REAL path (links followed) is outside the
 * checkout, outside the sidecar's own install (its TypeScript library files),
 * and outside the directories the caller names as its own (a capture's stub).
 * A refused file reads as missing, so its types become `any`: the posture an
 * unprepared tree already has.
 */
import ts from 'typescript';
export declare const UNTRUSTED_TREE_ENV = "CARRICK_UNTRUSTED_TREE";
/** The checkout the service root sits in: the nearest ancestor holding a
 * `.git` entry (a directory, or the file a worktree carries), else the service
 * root itself. */
export declare function checkoutRootOf(repoRoot: string): string;
export declare function isInsideCheckout(file: string, repoRoot: string): boolean;
/** True when the scan runs under the untrusted-tree switch. */
export declare function untrustedTree(): boolean;
/** The real roots an untrusted scan may read beneath. */
export interface ReadBound {
    readonly roots: readonly string[];
    /** Whether the real path of `file` is beneath a root. Never throws. */
    allows(file: string): boolean;
}
/** `p` with every link resolved; a path that does not exist yet resolves
 * through its deepest existing ancestor. */
export declare function realOf(p: string): string;
/**
 * The bound for a scan of the service at `repoRoot`, or `undefined` when the
 * switch is off. `own` names directories the caller itself owns and reads
 * back (a capture's stub, its scratch).
 */
export declare function untrustedReadBound(repoRoot: string, own?: readonly string[]): ReadBound | undefined;
/** `host` with every read the bound refuses answered as a missing file. */
export declare function boundedCompilerHost(host: ts.CompilerHost, bound: ReadBound | undefined): ts.CompilerHost;
/** The compiler's own file system, bounded; `ts.sys` itself when no bound. */
export declare function boundedSys(bound: ReadBound | undefined): ts.System;
