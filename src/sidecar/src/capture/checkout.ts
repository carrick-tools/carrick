/**
 * The checkout a service sits in, and the bound an untrusted scan reads
 * inside (carrick#2230).
 *
 * `CARRICK_UNTRUSTED_TREE` (any non-empty value) is the scan's switch, read
 * the same way the scanner reads it. Unset, nothing here constructs a
 * wrapper and every caller passes the compiler what it passed before. Set,
 * the compiler reads no file whose REAL path (links followed) is outside the
 * checkout, outside the sidecar's own install (its TypeScript library files),
 * and outside the directories the caller names as its own: a capture's stub,
 * and the npm package directories the scan's own Deno graph resolved,
 * beneath its npm cache (carrick#4076). A refused file reads as missing, so
 * its types become `any`: the posture an unprepared tree already has. The
 * Deno resolver and the Deno root-file list hand the compiler nothing the
 * bound refuses, so a refused file never fails the whole service.
 */

import * as fs from 'node:fs';
import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';

export const UNTRUSTED_TREE_ENV = 'CARRICK_UNTRUSTED_TREE';

const checkoutRoots = new Map<string, string>();

/** The checkout the service root sits in: the nearest ancestor holding a
 * `.git` entry (a directory, or the file a worktree carries), else the service
 * root itself. */
export function checkoutRootOf(repoRoot: string): string {
  const key = path.resolve(repoRoot);
  const cached = checkoutRoots.get(key);
  if (cached) return cached;
  let dir = key;
  let root = key;
  for (;;) {
    if (fs.existsSync(path.join(dir, '.git'))) {
      root = dir;
      break;
    }
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  checkoutRoots.set(key, root);
  return root;
}

export function isInsideCheckout(file: string, repoRoot: string): boolean {
  const root = checkoutRootOf(repoRoot).replace(/\\/g, '/');
  return file === root || file.startsWith(root.endsWith('/') ? root : root + '/');
}

/** True when the scan runs under the untrusted-tree switch. */
export function untrustedTree(): boolean {
  const value = process.env[UNTRUSTED_TREE_ENV];
  return value !== undefined && value !== '';
}

/** The real roots an untrusted scan may read beneath. */
export interface ReadBound {
  readonly roots: readonly string[];
  /** Whether the real path of `file` is beneath a root. Never throws. */
  allows(file: string): boolean;
}

/** `p` with every link resolved; a path that does not exist yet resolves
 * through its deepest existing ancestor. */
export function realOf(p: string): string {
  const abs = path.resolve(p);
  const tail: string[] = [];
  let dir = abs;
  for (;;) {
    try {
      return path.join(fs.realpathSync(dir), ...tail.reverse());
    } catch {
      const parent = path.dirname(dir);
      if (parent === dir) return abs;
      tail.push(path.basename(dir));
      dir = parent;
    }
  }
}

function within(real: string, root: string): boolean {
  return real === root || real.startsWith(root.endsWith(path.sep) ? root : root + path.sep);
}

/** The sidecar's own install directory: outside any scanned checkout, so a
 * command run there reads none of the tree's configuration. */
export function sidecarInstallDir(): string {
  return path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..');
}

/** Where the sidecar's own files live: its install, and the TypeScript package
 * whose `lib.*.d.ts` every program reads (beside the sidecar in the npm
 * package, so not always beneath it). */
function sidecarRoots(): string[] {
  const roots = [
    sidecarInstallDir(),
    path.dirname(path.dirname(ts.getDefaultLibFilePath({}))),
  ];
  const configured = process.env.CARRICK_SIDECAR_DIR;
  if (configured && configured.trim() !== '') roots.push(path.resolve(configured));
  return roots;
}

/**
 * The bound for a scan of the service at `repoRoot`, or `undefined` when the
 * switch is off. `own` names directories the caller itself owns and reads
 * back (a capture's stub, its scratch).
 */
export function untrustedReadBound(repoRoot: string, own: readonly string[] = []): ReadBound | undefined {
  if (!untrustedTree()) return undefined;
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
export function boundedCompilerHost(host: ts.CompilerHost, bound: ReadBound | undefined): ts.CompilerHost {
  if (!bound) return host;
  const getSourceFile = host.getSourceFile.bind(host);
  const read = host.readFile.bind(host);
  const exists = host.fileExists.bind(host);
  const dirExists = host.directoryExists?.bind(host);
  const dirs = host.getDirectories?.bind(host);
  const readDirectory = host.readDirectory?.bind(host);
  host.getSourceFile = (file, ...rest) => (bound.allows(file) ? getSourceFile(file, ...rest) : undefined);
  host.readFile = (file) => (bound.allows(file) ? read(file) : undefined);
  host.fileExists = (file) => bound.allows(file) && exists(file);
  if (dirExists) host.directoryExists = (dir) => bound.allows(dir) && dirExists(dir);
  if (dirs) host.getDirectories = (dir) => (bound.allows(dir) ? dirs(dir) : []);
  if (readDirectory) {
    host.readDirectory = (root, ...rest) => readDirectory(root, ...rest).filter((file) => bound.allows(file));
  }
  return host;
}

/** The compiler's own file system, bounded; `ts.sys` itself when no bound. */
export function boundedSys(bound: ReadBound | undefined): ts.System {
  if (!bound) return ts.sys;
  return {
    ...ts.sys,
    readFile: (file, encoding) => (bound.allows(file) ? ts.sys.readFile(file, encoding) : undefined),
    fileExists: (file) => bound.allows(file) && ts.sys.fileExists(file),
    directoryExists: (dir) => bound.allows(dir) && ts.sys.directoryExists(dir),
    getDirectories: (dir) => (bound.allows(dir) ? ts.sys.getDirectories(dir) : []),
    readDirectory: (root, ...rest) =>
      ts.sys.readDirectory(root, ...rest).filter((file) => bound.allows(file)),
  };
}
