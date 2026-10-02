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
import * as os from 'node:os';
import * as path from 'node:path';

/** A write or delete the guard refused, or a root it would not accept. */
export class WriteRefused extends Error {
  constructor(message: string) {
    super(`${message} (carrick#1748)`);
    this.name = 'WriteRefused';
  }
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

export class WriteGuard {
  private constructor(
    private readonly dirs: readonly string[],
    private readonly files: readonly string[],
    private readonly protect: readonly string[]
  ) {}

  /** A guard over `roots`. Throws when a root equals or contains a protected tree. */
  static of(roots: WriteRoots): WriteGuard {
    const protect = (roots.protect ?? []).map(landing);
    const dirs = (roots.dirs ?? []).map(landing);
    const files = (roots.files ?? []).map(entry);
    for (const root of [...dirs, ...files]) {
      const covered = protect.find((tree) => within(root, tree));
      if (covered !== undefined) {
        throw new WriteRefused(`refused write root ${root}: it is or contains the scanned tree ${covered}`);
      }
    }
    return new WriteGuard(dirs, files, protect);
  }

  /**
   * A new, empty directory under `parent` (the OS temp dir by default) and a
   * guard over it alone. `mkdtemp` picks an unused name, so creating it cannot
   * overwrite anything; `parent` is created when missing.
   */
  static scratch(prefix: string, parent: string = os.tmpdir()): { dir: string; guard: WriteGuard } {
    fs.mkdirSync(parent, { recursive: true });
    const dir = fs.mkdtempSync(path.join(parent, prefix));
    return { dir, guard: WriteGuard.of({ dirs: [dir] }) };
  }

  /** This guard with more roots, under the same protected trees. */
  with(roots: Omit<WriteRoots, 'protect'>): WriteGuard {
    return WriteGuard.of({
      dirs: [...this.dirs, ...(roots.dirs ?? [])],
      files: [...this.files, ...(roots.files ?? [])],
      protect: [...this.protect],
    });
  }

  /** A guard over `dir` alone, which must already be writable under this one. */
  narrow(dir: string): WriteGuard {
    this.check(dir, landing(dir), 'narrow to');
    return WriteGuard.of({ dirs: [dir], protect: [...this.protect] });
  }

  /** Whether a write to `p` would land inside this guard's roots. */
  allowsWrite(p: string): boolean {
    return this.holds(landing(p));
  }

  /** Throws unless `p` (a subprocess's working directory, say) lies inside the roots. */
  assertWithin(p: string): void {
    this.check(p, landing(p), 'work in');
  }

  writeFile(p: string, data: string | NodeJS.ArrayBufferView): void {
    this.check(p, landing(p), 'write');
    fs.writeFileSync(p, data);
  }

  /** `mkdir -p`. A directory that already exists is no write and passes unchecked. */
  mkdir(p: string): void {
    if (isDirectory(p)) return;
    this.check(p, landing(p), 'create directory');
    fs.mkdirSync(p, { recursive: true });
  }

  copyFile(from: string, to: string): void {
    this.check(to, landing(to), 'copy into');
    fs.copyFileSync(from, to);
  }

  /** `cp -R`. Links in the source are copied as links, never written through. */
  copyTree(from: string, to: string, filter?: (source: string) => boolean): void {
    this.check(to, landing(to), 'copy into');
    fs.cpSync(from, to, { recursive: true, filter });
  }

  /** `rm -rf`. A link is removed itself; its target is left alone. */
  remove(p: string): void {
    this.check(p, entry(p), 'delete');
    fs.rmSync(p, { recursive: true, force: true });
  }

  unlink(p: string): void {
    this.check(p, entry(p), 'delete');
    fs.unlinkSync(p);
  }

  symlink(target: string, linkPath: string, type?: fs.symlink.Type): void {
    this.check(linkPath, entry(linkPath), 'link');
    fs.symlinkSync(target, linkPath, type);
  }

  private holds(resolved: string): boolean {
    return this.files.includes(resolved) || this.dirs.some((root) => within(root, resolved));
  }

  private check(p: string, resolved: string, verb: string): void {
    if (this.holds(resolved)) return;
    throw new WriteRefused(
      `refused to ${verb} ${p}` +
        (resolved === path.resolve(p) ? '' : ` (resolves to ${resolved})`) +
        ': outside this run\'s scratch, stub and cache roots'
    );
  }
}

/** Whether `p` is `root` or lies beneath it. Both are resolved paths. */
function within(root: string, p: string): boolean {
  const rel = path.relative(root, p);
  return rel === '' || (rel !== '..' && !rel.startsWith(`..${path.sep}`) && !path.isAbsolute(rel));
}

function isDirectory(p: string): boolean {
  try {
    return fs.statSync(p).isDirectory();
  } catch {
    return false;
  }
}

/**
 * Where a write to `p` lands: every link resolved, through the deepest
 * existing ancestor when `p` does not exist yet, and through a dangling link's
 * target (writing to a dangling link creates its target).
 */
function landing(p: string, hops = 0): string {
  const abs = path.resolve(p);
  try {
    return fs.realpathSync.native(abs);
  } catch {
    // Missing, or a link whose target is missing.
  }
  let link: string | undefined;
  try {
    if (fs.lstatSync(abs).isSymbolicLink()) link = fs.readlinkSync(abs);
  } catch {
    // Not there at all.
  }
  if (link !== undefined) {
    if (hops > 40) throw new WriteRefused(`refused ${abs}: too many symbolic links`);
    return landing(path.resolve(path.dirname(abs), link), hops + 1);
  }
  const parent = path.dirname(abs);
  if (parent === abs) return abs;
  return path.join(landing(parent, hops), path.basename(abs));
}

/** The directory entry `p` names: its parent resolved, its own name kept. */
function entry(p: string): string {
  const abs = path.resolve(p);
  const parent = path.dirname(abs);
  if (parent === abs) return abs;
  return path.join(landing(parent), path.basename(abs));
}
