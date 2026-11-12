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
import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';
export const UNTRUSTED_TREE_ENV = 'CARRICK_UNTRUSTED_TREE';
const checkoutRoots = new Map();
/** The checkout the service root sits in: the nearest ancestor holding a
 * `.git` entry (a directory, or the file a worktree carries), else the service
 * root itself. */
export function checkoutRootOf(repoRoot) {
    const key = path.resolve(repoRoot);
    const cached = checkoutRoots.get(key);
    if (cached)
        return cached;
    let dir = key;
    let root = key;
    for (;;) {
        if (fs.existsSync(path.join(dir, '.git'))) {
            root = dir;
            break;
        }
        const parent = path.dirname(dir);
        if (parent === dir)
            break;
        dir = parent;
    }
    checkoutRoots.set(key, root);
    return root;
}
export function isInsideCheckout(file, repoRoot) {
    const root = checkoutRootOf(repoRoot).replace(/\\/g, '/');
    return file === root || file.startsWith(root.endsWith('/') ? root : root + '/');
}
/** True when the scan runs under the untrusted-tree switch. */
export function untrustedTree() {
    const value = process.env[UNTRUSTED_TREE_ENV];
    return value !== undefined && value !== '';
}
/** `p` with every link resolved; a path that does not exist yet resolves
 * through its deepest existing ancestor. */
export function realOf(p) {
    const abs = path.resolve(p);
    const tail = [];
    let dir = abs;
    for (;;) {
        try {
            return path.join(fs.realpathSync(dir), ...tail.reverse());
        }
        catch {
            const parent = path.dirname(dir);
            if (parent === dir)
                return abs;
            tail.push(path.basename(dir));
            dir = parent;
        }
    }
}
function within(real, root) {
    return real === root || real.startsWith(root.endsWith(path.sep) ? root : root + path.sep);
}
/** Where the sidecar's own files live: its install, and the TypeScript package
 * whose `lib.*.d.ts` every program reads (beside the sidecar in the npm
 * package, so not always beneath it). */
function sidecarRoots() {
    const here = path.dirname(fileURLToPath(import.meta.url));
    const roots = [
        path.resolve(here, '..', '..', '..'),
        path.dirname(path.dirname(ts.getDefaultLibFilePath({}))),
    ];
    const configured = process.env.CARRICK_SIDECAR_DIR;
    if (configured && configured.trim() !== '')
        roots.push(path.resolve(configured));
    return roots;
}
/**
 * The bound for a scan of the service at `repoRoot`, or `undefined` when the
 * switch is off. `own` names directories the caller itself owns and reads
 * back (a capture's stub, its scratch).
 */
export function untrustedReadBound(repoRoot, own = []) {
    if (!untrustedTree())
        return undefined;
    const roots = [...new Set([checkoutRootOf(repoRoot), ...sidecarRoots(), ...own].map(realOf))];
    return {
        roots,
        allows: (file) => {
            const real = realOf(file);
            return roots.some((root) => within(real, root));
        },
    };
}
/** `host` with every read the bound refuses answered as a missing file. */
export function boundedCompilerHost(host, bound) {
    if (!bound)
        return host;
    const getSourceFile = host.getSourceFile.bind(host);
    const read = host.readFile.bind(host);
    const exists = host.fileExists.bind(host);
    const dirExists = host.directoryExists?.bind(host);
    const dirs = host.getDirectories?.bind(host);
    const readDirectory = host.readDirectory?.bind(host);
    host.getSourceFile = (file, ...rest) => (bound.allows(file) ? getSourceFile(file, ...rest) : undefined);
    host.readFile = (file) => (bound.allows(file) ? read(file) : undefined);
    host.fileExists = (file) => bound.allows(file) && exists(file);
    if (dirExists)
        host.directoryExists = (dir) => bound.allows(dir) && dirExists(dir);
    if (dirs)
        host.getDirectories = (dir) => (bound.allows(dir) ? dirs(dir) : []);
    if (readDirectory) {
        host.readDirectory = (root, ...rest) => readDirectory(root, ...rest).filter((file) => bound.allows(file));
    }
    return host;
}
/** The compiler's own file system, bounded; `ts.sys` itself when no bound. */
export function boundedSys(bound) {
    if (!bound)
        return ts.sys;
    return {
        ...ts.sys,
        readFile: (file, encoding) => (bound.allows(file) ? ts.sys.readFile(file, encoding) : undefined),
        fileExists: (file) => bound.allows(file) && ts.sys.fileExists(file),
        directoryExists: (dir) => bound.allows(dir) && ts.sys.directoryExists(dir),
        getDirectories: (dir) => (bound.allows(dir) ? ts.sys.getDirectories(dir) : []),
        readDirectory: (root, ...rest) => ts.sys.readDirectory(root, ...rest).filter((file) => bound.allows(file)),
    };
}
