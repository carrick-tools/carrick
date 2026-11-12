/**
 * Where the check phase looks for its vendored pnpm and tsc (carrick#833).
 *
 * The check phase is the only thing that produces a type verdict, and it
 * spawns two executables to do it. A missing lookup does not crash: every pair
 * degrades to `unverifiable`, which in the output is indistinguishable from
 * the two types agreeing. So the lookup has to hold across every shape the
 * sidecar can arrive in, and each shape is pinned here rather than assumed.
 *
 * The shapes, and why each one exists:
 *  - checkout / release tarball: `src/sidecar/node_modules/.bin`, installed by
 *    the sidecar's own `npm ci`.
 *  - npm install, hoisted: the package is `node_modules/carrick/`, the sidecar
 *    a plain directory inside it (npm strips nested `node_modules` from a
 *    published tarball), and the bins are at the install root.
 *  - npm install, nested: the same, except a version conflict elsewhere in the
 *    tree made npm put this package's own copy under it.
 */
export {};
