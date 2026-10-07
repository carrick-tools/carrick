/**
 * How the scanned repo's installed tree resolved the packages a stub pins
 * (carrick#2091).
 *
 * A stub pins only the externals its declarations import. The check
 * workspace has no lockfile, so without this record every package those pins
 * depend on resolves fresh from the registry: the check can install a version
 * the repo never ran, or fail on one the registry lists but cannot serve.
 *
 * The walk reads the installed tree the way Node's lookup does, so one reader
 * covers npm, pnpm (isolated store and hoisted), yarn's node-modules linker
 * and bun. Starting at each pin installed at the pinned version, it follows
 * `dependencies` and `optionalDependencies` breadth first and records
 * `<parent>@<version> -> <child>: <version>` for every child installed as a
 * published package of that name. Workspace members, `link:` targets and npm
 * aliases are left out: the registry cannot serve them by that name. An edge
 * whose parent version is installed twice with different children is left
 * out, since no one answer is the repo's.
 *
 * The walk only reads. Seam: node builtins and this bundle only.
 */

import * as fs from 'node:fs';
import * as path from 'node:path';
import { installedPackageDirectory } from '../installed-package-directory.js';
import { isPublishedSemver } from './lockfile.js';

/** The stub file the check reads to pin transitive edges. */
export const RESOLUTION_FILE = 'carrick-resolution.json';

/** `<parent>@<version>` -> child name -> child version. */
export type ResolutionEdges = Record<string, Record<string, string>>;

export interface InstalledResolution {
  /** Key-sorted at both levels; byte-stable for one tree. */
  edges: ResolutionEdges;
  /** Dependencies of walked packages that no edge records: the check resolves these from the registry. */
  unrecorded: number;
}

interface Installed {
  /** Realpath of the package directory. */
  dir: string;
  name: string;
  version: string;
  dependencies: string[];
}

function validName(name: string): boolean {
  return !name.split('/').some((s) => s === '' || s === '.' || s === '..');
}

/**
 * Node's lookup of `name` from `fromDir`: `<dir>/node_modules/<name>` at each
 * ancestor, skipping an ancestor that is itself a `node_modules` directory.
 * That skip is how a pnpm store package finds its sibling links.
 */
function lookup(fromDir: string, name: string): string | undefined {
  let dir = fromDir;
  for (;;) {
    if (path.basename(dir) !== 'node_modules') {
      const candidate = path.join(dir, 'node_modules', ...name.split('/'));
      if (fs.existsSync(path.join(candidate, 'package.json'))) return candidate;
    }
    const parent = path.dirname(dir);
    if (parent === dir) return undefined;
    dir = parent;
  }
}

/**
 * The package `name` installed as Node would find it from `fromDir`: a
 * directory `installedPackageDirectory` accepts, holding a package.json of
 * that name with a published version.
 */
function installedFrom(root: string, fromDir: string, name: string): Installed | undefined {
  if (!validName(name)) return undefined;
  const found = lookup(fromDir, name);
  if (found === undefined) return undefined;
  try {
    const dir = fs.realpathSync(found);
    const installed = installedPackageDirectory(root, dir);
    if (installed === undefined || installed.directory !== dir || installed.name !== name) {
      return undefined;
    }
    const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'package.json'), 'utf8')) as {
      name?: unknown;
      version?: unknown;
      dependencies?: unknown;
      optionalDependencies?: unknown;
    };
    if (manifest?.name !== name || typeof manifest.version !== 'string') return undefined;
    if (!isPublishedSemver(manifest.version)) return undefined;
    const names = new Set<string>();
    for (const field of [manifest.dependencies, manifest.optionalDependencies]) {
      if (field && typeof field === 'object') for (const dep of Object.keys(field)) names.add(dep);
    }
    return { dir, name, version: manifest.version, dependencies: [...names].sort() };
  } catch {
    // Dangling link, unreadable or corrupt manifest: nothing installed here.
    return undefined;
  }
}

function sorted<T>(record: Record<string, T>): Record<string, T> {
  return Object.fromEntries(
    Object.keys(record)
      .sort()
      .map((k) => [k, record[k]])
  );
}

/**
 * The edges the installed tree under `serviceRoot` resolved, from the stub's
 * pins outward. A pin not installed at its pinned version starts no walk.
 */
export function installedResolutionEdges(
  serviceRoot: string,
  pins: Record<string, string>
): InstalledResolution {
  let root: string;
  try {
    root = fs.realpathSync(path.resolve(serviceRoot));
  } catch {
    return { edges: {}, unrecorded: 0 };
  }

  const queue: Installed[] = [];
  const visited = new Set<string>();
  for (const name of Object.keys(pins).sort()) {
    const pkg = installedFrom(root, root, name);
    if (pkg === undefined || pkg.version !== pins[name] || visited.has(pkg.dir)) continue;
    visited.add(pkg.dir);
    queue.push(pkg);
  }

  // parent key -> child -> every version a copy of that parent resolved.
  const seen = new Map<string, Map<string, Set<string>>>();
  // Each (parent key, child) a walked package declares, recorded or not.
  const declared = new Set<string>();
  for (let i = 0; i < queue.length; i++) {
    const parent = queue[i];
    const key = `${parent.name}@${parent.version}`;
    let children = seen.get(key);
    if (children === undefined) seen.set(key, (children = new Map()));
    for (const name of parent.dependencies) {
      declared.add(`${key}>${name}`);
      const child = installedFrom(root, parent.dir, name);
      if (child === undefined) continue;
      let versions = children.get(name);
      if (versions === undefined) children.set(name, (versions = new Set()));
      versions.add(child.version);
      if (!visited.has(child.dir)) {
        visited.add(child.dir);
        queue.push(child);
      }
    }
  }

  const edges: ResolutionEdges = {};
  let recorded = 0;
  for (const [key, children] of seen) {
    const kept: Record<string, string> = {};
    for (const [name, versions] of children) {
      if (versions.size === 1) kept[name] = [...versions][0];
    }
    if (Object.keys(kept).length === 0) continue;
    edges[key] = sorted(kept);
    recorded += Object.keys(kept).length;
  }
  return { edges: sorted(edges), unrecorded: declared.size - recorded };
}
