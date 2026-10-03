/**
 * Which tsconfig types a service that names none (carrick#1776).
 *
 * Both readers of a service, the init'd project and capture, ask this one
 * function, so they type the service under the same options.
 *
 * The service root is searched first, for the names a service keeps its own
 * config under. Above it, the search is `tsc`'s own when it is run with no
 * `-p`: the nearest `tsconfig.json` in an ancestor directory. A layout that
 * keeps one config above several services (their module resolution, `paths`,
 * `lib`) is typed under that config, as the repo's own `tsc` types it; without
 * it, every import only that config resolves read `any`.
 *
 * The walk stops at the scan root, inclusive: a config outside the scanned
 * tree is never read. With no scan root, or a service outside it, only the
 * service root is searched.
 */

import * as fs from 'node:fs';
import * as path from 'node:path';

/** The names a service root is searched for, in order. */
const SERVICE_ROOT_NAMES = ['tsconfig.json', 'tsconfig.build.json', 'tsconfig.app.json'];

/** The name an ancestor directory is searched for, as `tsc` searches. */
const ANCESTOR_NAME = 'tsconfig.json';

/** Absolute path of the config that types the service, or undefined for none. */
export function findServiceTsconfig(serviceRoot: string, scanRoot?: string): string | undefined {
  const root = path.resolve(serviceRoot);
  for (const name of SERVICE_ROOT_NAMES) {
    const candidate = path.join(root, name);
    if (fs.existsSync(candidate)) return candidate;
  }
  if (scanRoot === undefined) return undefined;
  // Containment is decided on real paths, so a symlink on either end cannot
  // hide the service inside the scan root or place it outside; the walk then
  // climbs the path as given, one directory per level between them.
  const fromBound = path.relative(realPath(scanRoot), realPath(root));
  if (fromBound === '' || fromBound.startsWith('..') || path.isAbsolute(fromBound)) return undefined;
  let dir = root;
  for (let level = fromBound.split(path.sep).length; level > 0; level--) {
    dir = path.dirname(dir);
    const candidate = path.join(dir, ANCESTOR_NAME);
    if (fs.existsSync(candidate)) return candidate;
  }
  return undefined;
}

/** The path with every symlink resolved; the path as given when it does not exist. */
export function realPath(p: string): string {
  try {
    return fs.realpathSync(p);
  } catch {
    return path.resolve(p);
  }
}
