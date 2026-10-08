/**
 * Where an installed package sits on disk, by path alone, for capture's
 * resolution walk. It mirrors `DeclarationReader.installedPackage` in
 * library-claims.ts: the capture seam (pinned decision 11a) lets neither side
 * import the other, so a change to what counts as installed changes both.
 *
 * Seam: node builtins only.
 */

import * as path from 'node:path';

/**
 * The installed package directory a real path lies in, by path alone: the
 * directory after the path's last `node_modules` segment (two segments for a
 * scope), taken relative to `root`, and the name it is installed as. A source
 * file in a directory named `node_modules` above `root`, or a checkout under a
 * `node_modules` ancestor, is not installed; an install hoisted above `root`
 * (`../../node_modules/pkg`) and a pnpm store
 * (`node_modules/.pnpm/pkg@1/node_modules/pkg`) are. The caller reads the
 * package.json there: without one, nothing is installed.
 */
export function installedPackageDirectory(
  root: string,
  realPath: string
): { directory: string; name: string } | undefined {
  const segments = path.relative(root, realPath).split(path.sep);
  const last = segments.lastIndexOf('node_modules');
  if (last < 0) return undefined;
  const width = segments[last + 1]?.startsWith('@') ? 2 : 1;
  return {
    directory: path.resolve(root, ...segments.slice(0, last + 1 + width)),
    name: segments.slice(last + 1, last + 1 + width).join('/'),
  };
}
