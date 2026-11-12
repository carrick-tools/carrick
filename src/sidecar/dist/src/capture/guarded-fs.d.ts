/**
 * Every file the sidecar writes or deletes goes through a {@link WriteGuard}
 * (carrick#1748).
 *
 * A scan reads the repo it scans and never writes it. carrick#1742 showed how
 * easily that breaks: the capture self-check reaches the repo's own sources
 * through a `node_modules` link, and a pass that "only rewrites its own
 * declarations" rewrote them. Each write site used to be safe only by its own
 * reasoning. Now each run names the directories it owns (its scratch, the stub
 * it emits, a runtime cache) and every write and delete is checked against
 * them, on the RESOLVED path, before it happens.
 *
 * Resolution follows the operation:
 *  - a write lands where the path's links lead, so the whole path is resolved,
 *    and a path that does not exist yet resolves through its deepest existing
 *    ancestor (a dangling link resolves through its target);
 *  - a delete, or a new link, touches the directory entry itself, so only the
 *    parent is resolved. Unlinking the stub's `node_modules` link is allowed;
 *    writing through it into the repo's packages is refused.
 *
 * `test/no-raw-writes.test.ts` fails when any other source file calls a
 * mutating `fs` API.
 */
import * as fs from 'node:fs';
/** A write or delete the guard refused, or a root it would not accept. */
export declare class WriteRefused extends Error {
    constructor(message: string);
}
export interface WriteRoots {
    /** Directories a run may write anywhere beneath, each one included. */
    dirs?: string[];
    /** Single files a run may write and delete, and nothing beside them. */
    files?: string[];
    /**
     * Trees the run reads: no root may be one of them or contain one. A root
     * that contained the scanned repo would let a stub's clean-up delete it.
     */
    protect?: string[];
}
export declare class WriteGuard {
    private readonly dirs;
    private readonly files;
    private readonly protect;
    private constructor();
    /**
     * A guard over `roots`. Throws when a root equals or contains a protected
     * tree, or when a directory root lies inside one anywhere but beneath a
     * `.carrick` directory (carrick#1768).
     */
    static of(roots: WriteRoots): WriteGuard;
    /**
     * A new, empty directory under `parent` (the OS temp dir by default) and a
     * guard over it alone. `mkdtemp` picks an unused name, so creating it cannot
     * overwrite anything; `parent` is created when missing.
     */
    static scratch(prefix: string, parent?: string): {
        dir: string;
        guard: WriteGuard;
    };
    /** This guard with more roots, under the same protected trees. */
    with(roots: Omit<WriteRoots, 'protect'>): WriteGuard;
    /** A guard over `dir` alone, which must already be writable under this one. */
    narrow(dir: string): WriteGuard;
    /** Whether a write to `p` would land inside this guard's roots. */
    allowsWrite(p: string): boolean;
    /** Throws unless `p` (a subprocess's working directory, say) lies inside the roots. */
    assertWithin(p: string): void;
    writeFile(p: string, data: string | NodeJS.ArrayBufferView): void;
    /** `mkdir -p`. A directory that already exists is no write and passes unchecked. */
    mkdir(p: string): void;
    copyFile(from: string, to: string): void;
    /** `cp -R`. Links in the source are copied as links, never written through. */
    copyTree(from: string, to: string, filter?: (source: string) => boolean): void;
    /** `rm -rf`. A link is removed itself; its target is left alone. */
    remove(p: string): void;
    unlink(p: string): void;
    /**
     * Move a file into place: both directory entries change, so both are
     * checked as a delete is. A write that must never be read half-done is
     * written beside its target and renamed over it.
     */
    rename(from: string, to: string): void;
    symlink(target: string, linkPath: string, type?: fs.symlink.Type): void;
    private holds;
    private check;
}
