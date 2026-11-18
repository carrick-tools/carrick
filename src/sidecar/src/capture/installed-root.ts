/**
 * Where a service's dependencies are installed: the nearest `node_modules`
 * directory at or above the service, and no further than the scan root. The
 * rule of `ancestors_within` in the pre-flight (`src/preflight.rs`): a
 * workspace whose install is hoisted to the repository root installs its
 * subfolder services, and an install above the scan root is not theirs.
 *
 * Seam: node builtins only, like the rest of this directory.
 */

import * as fs from 'node:fs';
import * as path from 'node:path';
import { realPath } from './service-config.js';

/**
 * `serviceRoot` and each parent up to and including `scanRoot`, nearest first.
 * Only `serviceRoot` when there is no scan root or the service is not inside
 * it. Containment is decided on real paths; the walk climbs the path as given.
 */
export function ancestorsWithin(serviceRoot: string, scanRoot?: string): string[] {
  const start = path.resolve(serviceRoot);
  if (scanRoot === undefined) return [start];
  const fromBound = path.relative(realPath(scanRoot), realPath(start));
  if (fromBound.startsWith('..') || path.isAbsolute(fromBound)) return [start];
  const levels = fromBound === '' ? 0 : fromBound.split(path.sep).length;
  const dirs = [start];
  for (let level = 0; level < levels; level++) dirs.push(path.dirname(dirs[dirs.length - 1]));
  return dirs;
}

/** The nearest `node_modules` directory for the service, or undefined for none. */
export function nearestNodeModules(serviceRoot: string, scanRoot?: string): string | undefined {
  for (const dir of ancestorsWithin(serviceRoot, scanRoot)) {
    const candidate = path.join(dir, 'node_modules');
    if (fs.existsSync(candidate)) return candidate;
  }
  return undefined;
}
